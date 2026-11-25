//! Recipes and their versions (F14). A version is a draft until it freezes; a frozen
//! version never changes (database trigger), and editing it creates the next version.

use std::collections::HashMap;

use domain::{Clock, IngredientId, ProductId, RecipeId, RecipeVersionId};
use jiff::Timestamp;
use sqlx::{Acquire, PgConnection};

use crate::error::unique;
use crate::formula::{Formula, IngredientRef, Line, Part, Pct, RecipeKind, SubRecipe};
use crate::{Error, MAX_NESTING};

/// The highest lead time a version may record, in hours.
pub const MAX_LEAD_TIME_HOURS: i32 = 168;
/// The highest percentage of one line: 1000%.
pub const MAX_LINE_PCT: Pct = Pct::from_thousandths(1_000_000);
pub const MAX_LINES: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeInput {
    pub name: String,
    pub kind: RecipeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub id: RecipeId,
    pub name: String,
    pub kind: RecipeKind,
    /// The version production uses.
    pub current_version_id: Option<RecipeVersionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeSummary {
    pub recipe: Recipe,
    pub current_version: Option<i32>,
    /// Names of the products made from this recipe.
    pub products: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub id: RecipeVersionId,
    pub recipe_id: RecipeId,
    pub version: i32,
    pub frozen_at: Option<Timestamp>,
    /// Information for the baker only; it drives no deadline (ADR 0027).
    pub lead_time_hours: Option<i32>,
    pub notes: String,
}

impl Version {
    #[must_use]
    pub const fn is_frozen(&self) -> bool {
        self.frozen_at.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinePart {
    Ingredient(IngredientId),
    Sub(RecipeVersionId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineInput {
    pub pct: Pct,
    pub part: LinePart,
}

/// A stored line, as the edit form shows it.
pub type LineRow = LineInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftInput {
    pub lines: Vec<LineInput>,
    pub lead_time_hours: Option<i32>,
    pub notes: String,
}

/// A version of another recipe that a line can use as sub-recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubChoice {
    pub version_id: RecipeVersionId,
    pub recipe_name: String,
    pub version: i32,
    pub kind: RecipeKind,
    pub frozen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedProduct {
    pub id: ProductId,
    pub name: String,
}

struct RecipeRow {
    id: i64,
    name: String,
    kind: String,
    current_version_id: Option<i64>,
}

impl TryFrom<RecipeRow> for Recipe {
    type Error = Error;

    fn try_from(row: RecipeRow) -> Result<Self, Error> {
        Ok(Self {
            id: RecipeId::new(row.id),
            name: row.name,
            kind: row.kind.parse()?,
            current_version_id: row.current_version_id.map(RecipeVersionId::new),
        })
    }
}

struct VersionRow {
    id: i64,
    recipe_id: i64,
    version: i32,
    frozen_at_us: Option<i64>,
    lead_time_hours: Option<i32>,
    notes: String,
}

impl TryFrom<VersionRow> for Version {
    type Error = Error;

    fn try_from(row: VersionRow) -> Result<Self, Error> {
        Ok(Self {
            id: RecipeVersionId::new(row.id),
            recipe_id: RecipeId::new(row.recipe_id),
            version: row.version,
            frozen_at: row.frozen_at_us.map(db::time::from_micros).transpose()?,
            lead_time_hours: row.lead_time_hours,
            notes: row.notes,
        })
    }
}

fn validate_recipe(input: &RecipeInput) -> Result<(), Error> {
    if input.name.trim().is_empty() {
        return Err(Error::Invalid("name"));
    }
    Ok(())
}

fn validate_draft(input: &DraftInput) -> Result<(), Error> {
    if input
        .lead_time_hours
        .is_some_and(|hours| !(0..=MAX_LEAD_TIME_HOURS).contains(&hours))
    {
        return Err(Error::Invalid("lead_time_hours"));
    }
    if input.lines.len() > MAX_LINES {
        return Err(Error::Invalid("lines"));
    }
    if input
        .lines
        .iter()
        .any(|line| line.pct.thousandths() == 0 || line.pct > MAX_LINE_PCT)
    {
        return Err(Error::Invalid("pct"));
    }
    Ok(())
}

/// A new recipe with an empty draft version 1, which is its current version.
///
/// # Errors
/// [`Error::Invalid`] for an empty name, [`Error::Duplicate`] for a name in use.
pub async fn create(
    conn: &mut PgConnection,
    input: &RecipeInput,
) -> Result<(Recipe, Version), Error> {
    validate_recipe(input)?;
    let mut tx = conn.begin().await?;
    let recipe_id = sqlx::query_scalar!(
        "INSERT INTO recipe (name, kind) VALUES ($1, $2) RETURNING id",
        input.name.trim(),
        input.kind.code(),
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(unique)?;
    let version_id = sqlx::query_scalar!(
        "INSERT INTO recipe_version (recipe_id, version) VALUES ($1, 1) RETURNING id",
        recipe_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE recipe SET current_version_id = $2 WHERE id = $1",
        recipe_id,
        version_id,
    )
    .execute(&mut *tx)
    .await?;
    let recipe = get(&mut tx, RecipeId::new(recipe_id)).await?;
    let version = version(&mut tx, recipe.id, RecipeVersionId::new(version_id)).await?;
    tx.commit().await?;
    Ok((recipe, version))
}

/// Renames a recipe or changes its kind; versions stay as they are.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Invalid`] or [`Error::Duplicate`].
pub async fn update(
    conn: &mut PgConnection,
    id: RecipeId,
    input: &RecipeInput,
) -> Result<Recipe, Error> {
    validate_recipe(input)?;
    sqlx::query_as!(
        RecipeRow,
        "UPDATE recipe SET name = $2, kind = $3 WHERE id = $1
        RETURNING id, name, kind, current_version_id",
        id.get(),
        input.name.trim(),
        input.kind.code(),
    )
    .fetch_optional(conn)
    .await
    .map_err(unique)?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// # Errors
/// [`Error::NotFound`] when there is no such recipe.
pub async fn get(conn: &mut PgConnection, id: RecipeId) -> Result<Recipe, Error> {
    sqlx::query_as!(
        RecipeRow,
        "SELECT id, name, kind, current_version_id FROM recipe WHERE id = $1",
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// Every recipe by name, with its current version number and the products made from it.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list(conn: &mut PgConnection) -> Result<Vec<RecipeSummary>, Error> {
    let rows = sqlx::query!(
        r#"SELECT r.id, r.name, r.kind, r.current_version_id, v.version AS "current_version?",
            coalesce(array_agg(p.name_nl ORDER BY p.name_nl) FILTER (WHERE p.id IS NOT NULL),
                '{}') AS "products!"
        FROM recipe r
        LEFT JOIN recipe_version v ON v.id = r.current_version_id
        LEFT JOIN product p ON p.recipe_id = r.id
        GROUP BY r.id, v.version
        ORDER BY lower(r.name), r.id"#,
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(RecipeSummary {
                recipe: RecipeRow {
                    id: row.id,
                    name: row.name,
                    kind: row.kind,
                    current_version_id: row.current_version_id,
                }
                .try_into()?,
                current_version: row.current_version,
                products: row.products,
            })
        })
        .collect()
}

/// The versions of a recipe, newest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn versions(conn: &mut PgConnection, recipe_id: RecipeId) -> Result<Vec<Version>, Error> {
    sqlx::query_as!(
        VersionRow,
        r#"SELECT id, recipe_id, version,
            (extract(epoch FROM frozen_at) * 1000000)::bigint AS frozen_at_us,
            lead_time_hours, notes
        FROM recipe_version WHERE recipe_id = $1 ORDER BY version DESC"#,
        recipe_id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Version::try_from)
    .collect()
}

/// # Errors
/// [`Error::NotFound`] when the version does not exist or belongs to another recipe.
pub async fn version(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
    id: RecipeVersionId,
) -> Result<Version, Error> {
    sqlx::query_as!(
        VersionRow,
        r#"SELECT id, recipe_id, version,
            (extract(epoch FROM frozen_at) * 1000000)::bigint AS frozen_at_us,
            lead_time_hours, notes
        FROM recipe_version WHERE id = $1 AND recipe_id = $2"#,
        id.get(),
        recipe_id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// The lines of one version in order, without what they refer to.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn lines(conn: &mut PgConnection, id: RecipeVersionId) -> Result<Vec<LineRow>, Error> {
    sqlx::query!(
        "SELECT ingredient_id, sub_recipe_version_id, bakers_pct_x1000 FROM recipe_line
        WHERE recipe_version_id = $1 ORDER BY position",
        id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        let part = match (row.ingredient_id, row.sub_recipe_version_id) {
            (Some(id), None) => LinePart::Ingredient(IngredientId::new(id)),
            (None, Some(id)) => LinePart::Sub(RecipeVersionId::new(id)),
            _ => return Err(Error::Corrupt("recipe line part".to_owned())),
        };
        Ok(LineRow {
            pct: pct_from_db(row.bakers_pct_x1000)?,
            part,
        })
    })
    .collect()
}

fn pct_from_db(thousandths: i32) -> Result<Pct, Error> {
    u32::try_from(thousandths)
        .map(Pct::from_thousandths)
        .map_err(|_| Error::Corrupt("negative percentage".to_owned()))
}

struct FormulaRow {
    recipe_version_id: i64,
    bakers_pct_x1000: i32,
    ingredient_id: Option<i64>,
    ingredient_name: Option<String>,
    ingredient_role: Option<String>,
    sub_recipe_version_id: Option<i64>,
    sub_recipe_id: Option<i64>,
    sub_name: Option<String>,
    sub_version: Option<i32>,
    sub_kind: Option<String>,
}

/// The full formula of a version: every line with its ingredient or sub-recipe, down
/// through all sub-recipes.
///
/// # Errors
/// [`Error::Nesting`] when sub-recipes nest deeper than [`MAX_NESTING`] or loop.
pub async fn formula(conn: &mut PgConnection, id: RecipeVersionId) -> Result<Formula, Error> {
    let rows = sqlx::query_as!(
        FormulaRow,
        r#"WITH RECURSIVE reach (version_id, depth) AS (
            SELECT $1::bigint, 0
            UNION ALL
            SELECT l.sub_recipe_version_id, r.depth + 1
            FROM recipe_line l JOIN reach r ON l.recipe_version_id = r.version_id
            WHERE l.sub_recipe_version_id IS NOT NULL AND r.depth < $2
        )
        SELECT l.recipe_version_id, l.bakers_pct_x1000,
            l.ingredient_id, i.name AS "ingredient_name?", i.role AS "ingredient_role?",
            l.sub_recipe_version_id, sv.recipe_id AS "sub_recipe_id?", sr.name AS "sub_name?",
            sv.version AS "sub_version?", sr.kind AS "sub_kind?"
        FROM recipe_line l
        LEFT JOIN ingredient i ON i.id = l.ingredient_id
        LEFT JOIN recipe_version sv ON sv.id = l.sub_recipe_version_id
        LEFT JOIN recipe sr ON sr.id = sv.recipe_id
        WHERE l.recipe_version_id IN (SELECT DISTINCT version_id FROM reach)
        ORDER BY l.recipe_version_id, l.position"#,
        id.get(),
        i32::try_from(MAX_NESTING).unwrap_or(i32::MAX),
    )
    .fetch_all(conn)
    .await?;
    let mut by_version: HashMap<i64, Vec<&FormulaRow>> = HashMap::new();
    for row in &rows {
        by_version
            .entry(row.recipe_version_id)
            .or_default()
            .push(row);
    }
    build(&by_version, id.get(), 0)
}

fn build(
    rows: &HashMap<i64, Vec<&FormulaRow>>,
    version_id: i64,
    depth: usize,
) -> Result<Formula, Error> {
    let lines = rows
        .get(&version_id)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|row| {
            let part = match row {
                FormulaRow {
                    ingredient_id: Some(id),
                    ingredient_name: Some(name),
                    ingredient_role: Some(role),
                    ..
                } => Part::Ingredient(IngredientRef {
                    id: IngredientId::new(*id),
                    name: name.clone(),
                    role: role.parse()?,
                }),
                FormulaRow {
                    sub_recipe_version_id: Some(sub_id),
                    sub_recipe_id: Some(recipe_id),
                    sub_name: Some(name),
                    sub_version: Some(version),
                    sub_kind: Some(kind),
                    ..
                } => {
                    if depth >= MAX_NESTING {
                        return Err(Error::Nesting);
                    }
                    Part::Sub(SubRecipe {
                        version_id: RecipeVersionId::new(*sub_id),
                        recipe_id: RecipeId::new(*recipe_id),
                        name: name.clone(),
                        version: *version,
                        kind: kind.parse()?,
                        formula: build(rows, *sub_id, depth + 1)?,
                    })
                }
                _ => return Err(Error::Corrupt("recipe line part".to_owned())),
            };
            Ok(Line {
                pct: pct_from_db(row.bakers_pct_x1000)?,
                part,
            })
        })
        .collect::<Result<_, Error>>()?;
    Ok(Formula { lines })
}

async fn lock_recipe(conn: &mut PgConnection, id: RecipeId) -> Result<(), Error> {
    sqlx::query_scalar!("SELECT id FROM recipe WHERE id = $1 FOR UPDATE", id.get())
        .fetch_optional(conn)
        .await?
        .ok_or(Error::NotFound)?;
    Ok(())
}

async fn draft_of(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
) -> Result<Option<RecipeVersionId>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM recipe_version WHERE recipe_id = $1 AND frozen_at IS NULL",
        recipe_id.get(),
    )
    .fetch_optional(conn)
    .await?
    .map(RecipeVersionId::new))
}

async fn insert_next(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
    lead_time_hours: Option<i32>,
    notes: &str,
) -> Result<RecipeVersionId, Error> {
    let id = sqlx::query_scalar!(
        "INSERT INTO recipe_version (recipe_id, version, lead_time_hours, notes)
        SELECT $1, coalesce(max(version), 0) + 1, $2, $3 FROM recipe_version WHERE recipe_id = $1
        RETURNING id",
        recipe_id.get(),
        lead_time_hours,
        notes,
    )
    .fetch_one(conn)
    .await?;
    Ok(RecipeVersionId::new(id))
}

/// Saves the lines, lead time and notes of a draft. Saving a frozen version creates the
/// next version with this content instead, and returns that one.
///
/// # Errors
/// [`Error::NotFound`] for an unknown version; [`Error::Invalid`] for a bad lead time,
/// percentage or line count, or a line naming an unknown ingredient or version;
/// [`Error::DraftExists`] when the version is frozen and the recipe already has a draft;
/// [`Error::Nesting`] when a line uses the recipe itself or nests too deep.
pub async fn save_draft(
    conn: &mut PgConnection,
    id: RecipeVersionId,
    input: &DraftInput,
) -> Result<Version, Error> {
    validate_draft(input)?;
    let mut tx = conn.begin().await?;
    let row = sqlx::query!(
        r#"SELECT recipe_id, frozen_at IS NOT NULL AS "frozen!" FROM recipe_version
        WHERE id = $1 FOR UPDATE"#,
        id.get(),
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    let recipe_id = RecipeId::new(row.recipe_id);
    let target = if row.frozen {
        lock_recipe(&mut tx, recipe_id).await?;
        if let Some(draft) = draft_of(&mut tx, recipe_id).await? {
            return Err(Error::DraftExists(draft));
        }
        insert_next(&mut tx, recipe_id, input.lead_time_hours, &input.notes).await?
    } else {
        sqlx::query!(
            "UPDATE recipe_version SET lead_time_hours = $2, notes = $3 WHERE id = $1",
            id.get(),
            input.lead_time_hours,
            input.notes,
        )
        .execute(&mut *tx)
        .await?;
        id
    };
    replace_lines(&mut tx, recipe_id, target, &input.lines).await?;
    formula(&mut tx, target).await?;
    for user in users(&mut tx, target).await? {
        formula(&mut tx, user).await?;
    }
    let saved = version(&mut tx, recipe_id, target).await?;
    tx.commit().await?;
    Ok(saved)
}

/// Every version that uses `id` as sub-recipe, directly or further up.
async fn users(
    conn: &mut PgConnection,
    id: RecipeVersionId,
) -> Result<Vec<RecipeVersionId>, Error> {
    Ok(sqlx::query_scalar!(
        r#"WITH RECURSIVE up (version_id) AS (
            SELECT $1::bigint
            UNION
            SELECT l.recipe_version_id
            FROM recipe_line l JOIN up u ON l.sub_recipe_version_id = u.version_id
        )
        SELECT version_id AS "version_id!" FROM up WHERE version_id <> $1"#,
        id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(RecipeVersionId::new)
    .collect())
}

async fn replace_lines(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
    id: RecipeVersionId,
    lines: &[LineInput],
) -> Result<(), Error> {
    let mut positions = Vec::with_capacity(lines.len());
    let mut ingredients = Vec::with_capacity(lines.len());
    let mut subs = Vec::with_capacity(lines.len());
    let mut pcts = Vec::with_capacity(lines.len());
    for (position, line) in lines.iter().enumerate() {
        positions.push(i32::try_from(position).unwrap_or(i32::MAX));
        let (ingredient, sub) = match line.part {
            LinePart::Ingredient(ingredient) => (Some(ingredient.get()), None),
            LinePart::Sub(sub) => (None, Some(sub.get())),
        };
        ingredients.push(ingredient);
        subs.push(sub);
        pcts.push(i32::try_from(line.pct.thousandths()).unwrap_or(i32::MAX));
    }
    let own = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM recipe_version
            WHERE id = ANY($1::bigint[]) AND recipe_id = $2) AS "own!""#,
        &subs.iter().flatten().copied().collect::<Vec<i64>>(),
        recipe_id.get(),
    )
    .fetch_one(&mut *conn)
    .await?;
    if own {
        return Err(Error::Nesting);
    }
    sqlx::query!(
        "DELETE FROM recipe_line WHERE recipe_version_id = $1",
        id.get()
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO recipe_line
            (recipe_version_id, position, ingredient_id, sub_recipe_version_id, bakers_pct_x1000)
        SELECT $1, t.position, t.ingredient_id, t.sub_id, t.pct
        FROM unnest($2::int[], $3::bigint[], $4::bigint[], $5::int[])
            AS t (position, ingredient_id, sub_id, pct)",
        id.get(),
        &positions,
        &ingredients as &[Option<i64>],
        &subs as &[Option<i64>],
        &pcts,
    )
    .execute(&mut *conn)
    .await
    .map_err(|err| match &err {
        sqlx::Error::Database(db) if db.is_foreign_key_violation() => Error::Invalid("line"),
        _ => Error::Db(err),
    })?;
    Ok(())
}

/// The recipe's draft, made when there is none: the next version number with the lines,
/// lead time and notes of the newest version.
///
/// # Errors
/// [`Error::NotFound`] when there is no such recipe.
pub async fn new_version(conn: &mut PgConnection, recipe_id: RecipeId) -> Result<Version, Error> {
    let mut tx = conn.begin().await?;
    lock_recipe(&mut tx, recipe_id).await?;
    let id = match draft_of(&mut tx, recipe_id).await? {
        Some(draft) => draft,
        None => {
            let newest = sqlx::query!(
                "SELECT id, lead_time_hours, notes FROM recipe_version
                WHERE recipe_id = $1 ORDER BY version DESC LIMIT 1",
                recipe_id.get(),
            )
            .fetch_one(&mut *tx)
            .await?;
            let id = insert_next(&mut tx, recipe_id, newest.lead_time_hours, &newest.notes).await?;
            sqlx::query!(
                "INSERT INTO recipe_line
                    (recipe_version_id, position, ingredient_id, sub_recipe_version_id,
                    bakers_pct_x1000)
                SELECT $1, position, ingredient_id, sub_recipe_version_id, bakers_pct_x1000
                FROM recipe_line WHERE recipe_version_id = $2",
                id.get(),
                newest.id,
            )
            .execute(&mut *tx)
            .await?;
            id
        }
    };
    let draft = version(&mut tx, recipe_id, id).await?;
    tx.commit().await?;
    Ok(draft)
}

/// Freezes a version and every sub-recipe version it uses, so its formula never changes.
/// Freezing a frozen version keeps its first time. Staff call it by hand; production
/// calls it when the first batch uses the version (F14a).
///
/// # Errors
/// [`Error::NotFound`] when the version does not belong to the recipe.
pub async fn freeze(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
    id: RecipeVersionId,
    clock: &dyn Clock,
) -> Result<Version, Error> {
    let mut tx = conn.begin().await?;
    version(&mut tx, recipe_id, id).await?;
    sqlx::query!(
        "WITH RECURSIVE reach (version_id) AS (
            SELECT $1::bigint
            UNION
            SELECT l.sub_recipe_version_id
            FROM recipe_line l JOIN reach r ON l.recipe_version_id = r.version_id
            WHERE l.sub_recipe_version_id IS NOT NULL
        )
        UPDATE recipe_version
        SET frozen_at = timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
        WHERE id IN (SELECT version_id FROM reach) AND frozen_at IS NULL",
        id.get(),
        db::time::to_micros(clock.now()),
    )
    .execute(&mut *tx)
    .await?;
    let frozen = version(&mut tx, recipe_id, id).await?;
    tx.commit().await?;
    Ok(frozen)
}

/// Makes `id` the version production uses.
///
/// # Errors
/// [`Error::NotFound`] when the version does not belong to the recipe.
pub async fn set_current(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
    id: RecipeVersionId,
) -> Result<Recipe, Error> {
    version(conn, recipe_id, id).await?;
    sqlx::query_as!(
        RecipeRow,
        "UPDATE recipe SET current_version_id = $2 WHERE id = $1
        RETURNING id, name, kind, current_version_id",
        recipe_id.get(),
        id.get(),
    )
    .fetch_one(conn)
    .await?
    .try_into()
}

/// Versions of other recipes that a line of `recipe_id` can use, by recipe name and newest
/// version first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn sub_choices(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
) -> Result<Vec<SubChoice>, Error> {
    sqlx::query!(
        r#"SELECT v.id, r.name, v.version, r.kind, v.frozen_at IS NOT NULL AS "frozen!"
        FROM recipe_version v JOIN recipe r ON r.id = v.recipe_id
        WHERE r.id <> $1
        ORDER BY lower(r.name), r.id, v.version DESC"#,
        recipe_id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(SubChoice {
            version_id: RecipeVersionId::new(row.id),
            recipe_name: row.name,
            version: row.version,
            kind: row.kind.parse()?,
            frozen: row.frozen,
        })
    })
    .collect()
}

/// The products made from a recipe, by name.
///
/// # Errors
/// [`Error::Db`].
pub async fn products(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
) -> Result<Vec<LinkedProduct>, Error> {
    Ok(sqlx::query!(
        "SELECT id, name_nl FROM product WHERE recipe_id = $1 ORDER BY name_nl, id",
        recipe_id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| LinkedProduct {
        id: ProductId::new(row.id),
        name: row.name_nl,
    })
    .collect())
}

/// Makes the product use this recipe, replacing any recipe it had.
///
/// # Errors
/// [`Error::NotFound`] when the recipe or the product does not exist.
pub async fn link_product(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
    product_id: ProductId,
) -> Result<(), Error> {
    get(conn, recipe_id).await?;
    let done = sqlx::query!(
        "UPDATE product SET recipe_id = $1 WHERE id = $2",
        recipe_id.get(),
        product_id.get(),
    )
    .execute(conn)
    .await?;
    if done.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

/// Takes the recipe off the product; nothing happens when it uses another recipe.
///
/// # Errors
/// [`Error::Db`].
pub async fn unlink_product(
    conn: &mut PgConnection,
    recipe_id: RecipeId,
    product_id: ProductId,
) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE product SET recipe_id = NULL WHERE id = $1 AND recipe_id = $2",
        product_id.get(),
        recipe_id.get(),
    )
    .execute(conn)
    .await?;
    Ok(())
}
