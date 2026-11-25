//! Batches (F14a, ADR 0042): batches started by hand, the baker's adjustments, the lock at mixing and the pieces out of the oven.
//!
//! A batch keeps its own copy of the recipe version's lines; the version freezes when the
//! first batch uses it. Grams are never stored: they follow from the lines and the dough
//! weight (planned pieces × dough per piece) with the recipes rounding rule (ADR 0037).

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use bakedays::days;
use db::time::{from_epoch_days, from_micros, to_epoch_days, to_micros};
use domain::{
    BakeDayId, BatchId, BatchLineId, Clock, IngredientId, ProductId, RecipeId, RecipeVersionId,
};
use jiff::Timestamp;
use jiff::civil::Date;
use orders::audit::{self, Actor, Entry};
use recipes::{Formula, IngredientRef, Line, Part, Pct, RecipeKind, SubRecipe};
use serde_json::json;
use sqlx::{Acquire, PgConnection};

use crate::Error;

/// The most pieces of one product in one batch.
pub const MAX_PIECES: u32 = 100_000;
/// The most dough one piece may have; the limit of `batch_output.dough_g_per_piece`.
pub const MAX_DOUGH_G_PER_PIECE: u32 = 1_000_000;
/// Longest reason the baker can give for an adjustment.
pub const MAX_REASON_CHARS: usize = 200;
/// Serialises numbering batches; unique among the advisory lock keys the crates use.
const NUMBERING_LOCK: i64 = 0x6261_7463_6865_7331;
const AUDIT_ENTITY: &str = "batch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Planned,
    Adjusted,
    /// Locked: lines and planned pieces never change again.
    Mixed,
    Baked,
}

impl Status {
    /// Stored in the database; never change.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Adjusted => "adjusted",
            Self::Mixed => "mixed",
            Self::Baked => "baked",
        }
    }

    fn from_code(code: &str) -> Result<Self, Error> {
        match code {
            "planned" => Ok(Self::Planned),
            "adjusted" => Ok(Self::Adjusted),
            "mixed" => Ok(Self::Mixed),
            "baked" => Ok(Self::Baked),
            other => Err(Error::Corrupt(format!("batch status {other}"))),
        }
    }

    #[must_use]
    pub const fn is_locked(self) -> bool {
        matches!(self, Self::Mixed | Self::Baked)
    }
}

/// The short code on labels: the bake date as YYMMDD, a dash and the batch number of
/// that date, e.g. `261205-2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchCode {
    pub date: Date,
    pub mix_no: u32,
}

impl fmt::Display for BatchCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02}{:02}{:02}-{}",
            self.date.year().rem_euclid(100),
            self.date.month(),
            self.date.day(),
            self.mix_no
        )
    }
}

/// What a batch line uses: an ingredient, or a frozen sub-recipe version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineRef {
    Ingredient(IngredientRef),
    Sub {
        version_id: RecipeVersionId,
        recipe_id: RecipeId,
        name: String,
        version: i32,
        kind: RecipeKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchLine {
    pub id: BatchLineId,
    pub part: LineRef,
    /// The recipe version's percentage; `None` for a line the baker added.
    pub base_pct: Option<Pct>,
    /// What goes in; zero leaves the line out.
    pub actual_pct: Pct,
    pub reason: Option<String>,
}

impl BatchLine {
    #[must_use]
    pub fn is_changed(&self) -> bool {
        self.base_pct != Some(self.actual_pct)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub product_id: ProductId,
    pub name: String,
    pub dough_g_per_piece: u32,
    pub planned_pieces: u32,
    /// Recorded after baking, with the pieces wasted among them.
    pub actual_pieces: Option<u32>,
    pub waste_pieces: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub id: BatchId,
    pub bake_day_id: BakeDayId,
    pub recipe_version_id: RecipeVersionId,
    pub recipe_id: RecipeId,
    pub recipe_name: String,
    pub version: i32,
    pub code: BatchCode,
    pub status: Status,
    pub notes: String,
    pub mixed_at: Option<Timestamp>,
    pub baked_at: Option<Timestamp>,
    pub lines: Vec<BatchLine>,
    /// By product name.
    pub outputs: Vec<Output>,
}

impl Batch {
    /// Planned pieces × dough per piece, over all products.
    #[must_use]
    pub fn dough_g(&self) -> u64 {
        self.outputs
            .iter()
            .map(|o| u64::from(o.planned_pieces) * u64::from(o.dough_g_per_piece))
            .sum()
    }
}

/// A batch's dough weight and its own formula, sub-recipes resolved, for ingredient needs
/// (F15): `formula.scale(dough_g)` gives the grams of every line. One per batch, it maps
/// onto one F15 dough: recipe and version, the outputs as its products and
/// `recipes::DoughGrams::from_formula(&formula, dough_g)` as its grams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchFormula {
    pub batch_id: BatchId,
    pub code: BatchCode,
    pub recipe_version_id: RecipeVersionId,
    pub recipe_name: String,
    pub version: i32,
    /// Planned pieces and dough per piece per product, by product name.
    pub outputs: Vec<Output>,
    pub dough_g: u64,
    pub formula: Formula,
}

/// The baker's changes to a batch before mixing, with one optional reason for all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Adjustment {
    pub lines: Vec<(BatchLineId, Pct)>,
    pub pieces: Vec<(ProductId, u32)>,
    pub reason: Option<String>,
}

/// Pieces out of the oven for one product, wasted pieces among them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actual {
    pub product_id: ProductId,
    pub pieces: u32,
    pub waste: u32,
}

/// Products a batch can be started for by hand on a day: active, or with pieces still to
/// bake that day, and with a recipe that has a version in use and dough per piece. By name.
///
/// # Errors
/// [`Error::Db`], [`Error::Orders`] or [`Error::BakeDays`].
pub async fn startable_products(
    conn: &mut PgConnection,
    day: BakeDayId,
) -> Result<Vec<(ProductId, String)>, Error> {
    let baking: Vec<i64> = to_bake(conn, day)
        .await?
        .into_keys()
        .map(|id| id.get())
        .collect();
    Ok(sqlx::query!(
        "SELECT p.id, p.name_nl FROM product p JOIN recipe r ON r.id = p.recipe_id
        WHERE (p.active OR p.id = ANY($1)) AND p.dough_g_per_piece > 0
            AND r.current_version_id IS NOT NULL
        ORDER BY lower(p.name_nl), p.id",
        &baking,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| (ProductId::new(row.id), row.name_nl))
    .collect())
}

struct ProductRecipe {
    name: String,
    dough_g_per_piece: i32,
    recipe_id: Option<i64>,
    version_id: Option<i64>,
    has_lines: bool,
}

async fn product_recipes(
    conn: &mut PgConnection,
    ids: &[i64],
) -> Result<HashMap<i64, ProductRecipe>, Error> {
    Ok(sqlx::query!(
        r#"SELECT p.id AS "id!", p.name_nl AS "name_nl!", p.dough_g_per_piece AS "dough_g_per_piece!",
            p.recipe_id,
            r.current_version_id AS "version_id?",
            EXISTS (SELECT 1 FROM recipe_line l WHERE l.recipe_version_id = r.current_version_id)
                AS "has_lines!"
        FROM product p LEFT JOIN recipe r ON r.id = p.recipe_id
        WHERE p.id = ANY($1)"#,
        ids,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        (
            row.id,
            ProductRecipe {
                name: row.name_nl,
                dough_g_per_piece: row.dough_g_per_piece,
                recipe_id: row.recipe_id,
                version_id: row.version_id,
                has_lines: row.has_lines,
            },
        )
    })
    .collect())
}

/// Pieces to bake per product: firm orders plus extra production (F7).
async fn to_bake(
    conn: &mut PgConnection,
    day: BakeDayId,
) -> Result<BTreeMap<ProductId, u32>, Error> {
    let mut pieces: BTreeMap<ProductId, u32> = BTreeMap::new();
    for order in orders::order::list_firm(conn, day).await? {
        for line in &order.lines {
            let entry = pieces.entry(line.product_id).or_default();
            *entry = entry.saturating_add(line.qty);
        }
    }
    for extra in bakedays::extras::list(conn, day).await? {
        let entry = pieces.entry(extra.product_id).or_default();
        *entry = entry.saturating_add(extra.qty);
    }
    Ok(pieces)
}

/// Pieces already planned in this day's batches per product, whatever recipe version the
/// batch uses, so a new version in use does not plan the same pieces again.
async fn planned(conn: &mut PgConnection, day: BakeDayId) -> Result<HashMap<i64, u32>, Error> {
    sqlx::query!(
        r#"SELECT o.product_id, sum(o.planned_pieces)::bigint AS "pieces!"
        FROM batch b JOIN batch_output o ON o.batch_id = b.id
        WHERE b.bake_day_id = $1
        GROUP BY o.product_id"#,
        day.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        let pieces = u32::try_from(row.pieces).map_err(|_| Error::Corrupt("pieces".to_owned()))?;
        Ok((row.product_id, pieces))
    })
    .collect()
}

/// Pieces still to bake on a day that are in none of its batches, per product with its
/// name, by name: what still needs a batch started by hand.
///
/// # Errors
/// [`Error::Db`], [`Error::Orders`] or [`Error::BakeDays`].
pub async fn unbatched(
    conn: &mut PgConnection,
    day: BakeDayId,
) -> Result<Vec<(ProductId, String, u32)>, Error> {
    let pieces = to_bake(conn, day).await?;
    let already = planned(conn, day).await?;
    let ids: Vec<i64> = pieces.keys().map(|id| id.get()).collect();
    let products = product_recipes(conn, &ids).await?;
    let mut left: Vec<(ProductId, String, u32)> = pieces
        .into_iter()
        .filter_map(|(id, count)| {
            let left = count.saturating_sub(already.get(&id.get()).copied().unwrap_or(0));
            let name = products.get(&id.get())?.name.clone();
            (left > 0).then_some((id, name, left))
        })
        .collect();
    left.sort_by(|a, b| {
        a.1.to_lowercase()
            .cmp(&b.1.to_lowercase())
            .then(a.0.cmp(&b.0))
    });
    Ok(left)
}

/// Refuses a batch whose lines are all at 0%: there would be nothing to weigh.
async fn require_lines(conn: &mut PgConnection, id: BatchId) -> Result<(), Error> {
    let any = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM batch_line WHERE batch_id = $1 AND actual_pct_x1000 > 0)
            AS "any!""#,
        id.get(),
    )
    .fetch_one(conn)
    .await?;
    if any {
        Ok(())
    } else {
        Err(Error::Invalid("empty"))
    }
}

/// Holds the bake day row, so it cannot be cancelled or finished while batches are made.
async fn lock_day(conn: &mut PgConnection, day: BakeDayId) -> Result<(), Error> {
    sqlx::query_scalar!("SELECT id FROM bake_day WHERE id = $1 FOR SHARE", day.get())
        .fetch_optional(conn)
        .await?;
    Ok(())
}

async fn lock_numbering(conn: &mut PgConnection) -> Result<(), Error> {
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", NUMBERING_LOCK)
        .execute(conn)
        .await?;
    Ok(())
}

struct NewBatch<'a> {
    day: &'a bakedays::BakeDay,
    recipe_id: RecipeId,
    version_id: RecipeVersionId,
    /// Product, dough grams per piece, pieces.
    outputs: &'a [(ProductId, u32, u32)],
}

/// Inserts a batch with a copy of the version's lines, after freezing the version.
async fn insert_batch(
    conn: &mut PgConnection,
    new: &NewBatch<'_>,
    actor: Actor,
    clock: &dyn Clock,
) -> Result<BatchId, Error> {
    let NewBatch {
        day,
        recipe_id,
        version_id,
        outputs,
    } = *new;
    recipes::recipes::freeze(conn, recipe_id, version_id, clock).await?;
    let now = clock.now();
    let id = sqlx::query_scalar!(
        "INSERT INTO batch (bake_day_id, recipe_version_id, code_date, mix_no, created_at)
        SELECT $1, $2, date 'epoch' + $3::integer,
            COALESCE(max(mix_no), 0) + 1,
            timestamptz 'epoch' + $4::bigint * interval '1 microsecond'
        FROM batch WHERE code_date = date 'epoch' + $3::integer
        RETURNING id",
        day.id.get(),
        version_id.get(),
        to_epoch_days(day.date),
        to_micros(now),
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO batch_line (batch_id, position, ingredient_id, sub_recipe_version_id,
            base_pct_x1000, actual_pct_x1000)
        SELECT $1, position, ingredient_id, sub_recipe_version_id,
            bakers_pct_x1000, bakers_pct_x1000
        FROM recipe_line WHERE recipe_version_id = $2",
        id,
        version_id.get(),
    )
    .execute(&mut *conn)
    .await?;
    for &(product_id, grams, pieces) in outputs {
        sqlx::query!(
            "INSERT INTO batch_output (batch_id, product_id, dough_g_per_piece, planned_pieces)
            VALUES ($1, $2, $3, $4)",
            id,
            product_id.get(),
            i32::try_from(grams).map_err(|_| Error::Invalid("dough"))?,
            i32::try_from(pieces).map_err(|_| Error::Invalid("pieces"))?,
        )
        .execute(&mut *conn)
        .await?;
    }
    audit::record(
        conn,
        &Entry {
            entity: AUDIT_ENTITY,
            entity_id: id,
            action: "create",
            before: None,
            after: Some(json!({
                "recipe_version_id": version_id.get(),
                "outputs": outputs
                    .iter()
                    .map(|(p, g, n)| json!({ "product_id": p.get(), "dough_g_per_piece": g, "pieces": n }))
                    .collect::<Vec<_>>(),
            })),
            actor,
            at: now,
        },
    )
    .await?;
    Ok(BatchId::new(id))
}

/// Starts a batch by hand: `pieces` of one product, made from the version in use of its
/// recipe. Any quantity.
///
/// # Errors
/// [`Error::Invalid`] with `pieces` outside 1 to [`MAX_PIECES`], `recipe` when the
/// product has no recipe with a version in use that has lines, or `dough` without a dough
/// weight per piece; [`Error::DayFinal`] for a cancelled or finished day.
pub async fn start_manual(
    conn: &mut PgConnection,
    day_id: BakeDayId,
    product_id: ProductId,
    pieces: u32,
    actor: Actor,
    clock: &dyn Clock,
) -> Result<BatchId, Error> {
    if pieces == 0 || pieces > MAX_PIECES {
        return Err(Error::Invalid("pieces"));
    }
    let mut tx = conn.begin().await?;
    lock_day(&mut tx, day_id).await?;
    let day = days::get(&mut tx, day_id).await?;
    if day.status.is_final() {
        return Err(Error::DayFinal);
    }
    let products = product_recipes(&mut tx, &[product_id.get()]).await?;
    let product = products.get(&product_id.get()).ok_or(Error::NotFound)?;
    let (Some(recipe_id), Some(version_id), true) =
        (product.recipe_id, product.version_id, product.has_lines)
    else {
        return Err(Error::Invalid("recipe"));
    };
    let grams = u32::try_from(product.dough_g_per_piece)
        .ok()
        .filter(|g| (1..=MAX_DOUGH_G_PER_PIECE).contains(g))
        .ok_or(Error::Invalid("dough"))?;
    lock_numbering(&mut tx).await?;
    let new = NewBatch {
        day: &day,
        recipe_id: RecipeId::new(recipe_id),
        version_id: RecipeVersionId::new(version_id),
        outputs: &[(product_id, grams, pieces)],
    };
    let id = insert_batch(&mut tx, &new, actor, clock).await?;
    tx.commit().await?;
    Ok(id)
}

struct BatchRow {
    id: i64,
    bake_day_id: i64,
    recipe_version_id: i64,
    recipe_id: i64,
    recipe_name: String,
    version: i32,
    code_days: i32,
    mix_no: i32,
    status: String,
    notes: String,
    mixed_at_us: Option<i64>,
    baked_at_us: Option<i64>,
}

struct LineRow {
    id: i64,
    batch_id: i64,
    ingredient_id: Option<i64>,
    ingredient_name: Option<String>,
    ingredient_role: Option<String>,
    sub_recipe_version_id: Option<i64>,
    sub_recipe_id: Option<i64>,
    sub_name: Option<String>,
    sub_version: Option<i32>,
    sub_kind: Option<String>,
    base_pct_x1000: Option<i32>,
    actual_pct_x1000: i32,
    reason: Option<String>,
}

fn pct(thousandths: i32) -> Result<Pct, Error> {
    u32::try_from(thousandths)
        .map(Pct::from_thousandths)
        .map_err(|_| Error::Corrupt("negative percentage".to_owned()))
}

fn count(value: i32) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| Error::Corrupt("negative count".to_owned()))
}

impl TryFrom<LineRow> for BatchLine {
    type Error = Error;

    fn try_from(row: LineRow) -> Result<Self, Error> {
        let part = match row {
            LineRow {
                ingredient_id: Some(id),
                ingredient_name: Some(ref name),
                ingredient_role: Some(ref role),
                ..
            } => LineRef::Ingredient(IngredientRef {
                id: IngredientId::new(id),
                name: name.clone(),
                role: role.parse().map_err(|e| Error::Corrupt(format!("{e}")))?,
            }),
            LineRow {
                sub_recipe_version_id: Some(version_id),
                sub_recipe_id: Some(recipe_id),
                sub_name: Some(ref name),
                sub_version: Some(version),
                sub_kind: Some(ref kind),
                ..
            } => LineRef::Sub {
                version_id: RecipeVersionId::new(version_id),
                recipe_id: RecipeId::new(recipe_id),
                name: name.clone(),
                version,
                kind: kind.parse().map_err(|e| Error::Corrupt(format!("{e}")))?,
            },
            _ => return Err(Error::Corrupt("batch line part".to_owned())),
        };
        Ok(Self {
            id: BatchLineId::new(row.id),
            part,
            base_pct: row.base_pct_x1000.map(pct).transpose()?,
            actual_pct: pct(row.actual_pct_x1000)?,
            reason: row.reason,
        })
    }
}

async fn load(conn: &mut PgConnection, rows: Vec<BatchRow>) -> Result<Vec<Batch>, Error> {
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let mut lines: HashMap<i64, Vec<BatchLine>> = HashMap::new();
    for row in sqlx::query_as!(
        LineRow,
        r#"SELECT l.id, l.batch_id,
            l.ingredient_id, i.name AS "ingredient_name?", i.role AS "ingredient_role?",
            l.sub_recipe_version_id, sv.recipe_id AS "sub_recipe_id?", sr.name AS "sub_name?",
            sv.version AS "sub_version?", sr.kind AS "sub_kind?",
            l.base_pct_x1000, l.actual_pct_x1000, l.reason
        FROM batch_line l
        LEFT JOIN ingredient i ON i.id = l.ingredient_id
        LEFT JOIN recipe_version sv ON sv.id = l.sub_recipe_version_id
        LEFT JOIN recipe sr ON sr.id = sv.recipe_id
        WHERE l.batch_id = ANY($1)
        ORDER BY l.batch_id, l.position"#,
        &ids,
    )
    .fetch_all(&mut *conn)
    .await?
    {
        let batch_id = row.batch_id;
        lines.entry(batch_id).or_default().push(row.try_into()?);
    }
    let mut outputs: HashMap<i64, Vec<Output>> = HashMap::new();
    for row in sqlx::query!(
        "SELECT o.batch_id, o.product_id, p.name_nl, o.dough_g_per_piece, o.planned_pieces,
            o.actual_pieces, o.waste_pieces
        FROM batch_output o JOIN product p ON p.id = o.product_id
        WHERE o.batch_id = ANY($1)
        ORDER BY o.batch_id, lower(p.name_nl), p.id",
        &ids,
    )
    .fetch_all(&mut *conn)
    .await?
    {
        outputs.entry(row.batch_id).or_default().push(Output {
            product_id: ProductId::new(row.product_id),
            name: row.name_nl,
            dough_g_per_piece: count(row.dough_g_per_piece)?,
            planned_pieces: count(row.planned_pieces)?,
            actual_pieces: row.actual_pieces.map(count).transpose()?,
            waste_pieces: row.waste_pieces.map(count).transpose()?,
        });
    }
    rows.into_iter()
        .map(|row| {
            Ok(Batch {
                id: BatchId::new(row.id),
                bake_day_id: BakeDayId::new(row.bake_day_id),
                recipe_version_id: RecipeVersionId::new(row.recipe_version_id),
                recipe_id: RecipeId::new(row.recipe_id),
                recipe_name: row.recipe_name,
                version: row.version,
                code: BatchCode {
                    date: from_epoch_days(row.code_days)?,
                    mix_no: count(row.mix_no)?,
                },
                status: Status::from_code(&row.status)?,
                notes: row.notes,
                mixed_at: row.mixed_at_us.map(from_micros).transpose()?,
                baked_at: row.baked_at_us.map(from_micros).transpose()?,
                lines: lines.remove(&row.id).unwrap_or_default(),
                outputs: outputs.remove(&row.id).unwrap_or_default(),
            })
        })
        .collect()
}

/// A day's batches by code.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list(conn: &mut PgConnection, day: BakeDayId) -> Result<Vec<Batch>, Error> {
    let rows = sqlx::query_as!(
        BatchRow,
        r#"SELECT b.id, b.bake_day_id, b.recipe_version_id, v.recipe_id, r.name AS recipe_name,
            v.version, (b.code_date - date 'epoch') AS "code_days!", b.mix_no,
            b.status, b.notes,
            (extract(epoch FROM b.mixed_at) * 1000000)::bigint AS mixed_at_us,
            (extract(epoch FROM b.baked_at) * 1000000)::bigint AS baked_at_us
        FROM batch b
        JOIN recipe_version v ON v.id = b.recipe_version_id
        JOIN recipe r ON r.id = v.recipe_id
        WHERE b.bake_day_id = $1
        ORDER BY b.code_date, b.mix_no"#,
        day.get(),
    )
    .fetch_all(&mut *conn)
    .await?;
    load(conn, rows).await
}

/// # Errors
/// [`Error::NotFound`] when the batch does not exist.
pub async fn get(conn: &mut PgConnection, id: BatchId) -> Result<Batch, Error> {
    let rows = sqlx::query_as!(
        BatchRow,
        r#"SELECT b.id, b.bake_day_id, b.recipe_version_id, v.recipe_id, r.name AS recipe_name,
            v.version, (b.code_date - date 'epoch') AS "code_days!", b.mix_no,
            b.status, b.notes,
            (extract(epoch FROM b.mixed_at) * 1000000)::bigint AS mixed_at_us,
            (extract(epoch FROM b.baked_at) * 1000000)::bigint AS baked_at_us
        FROM batch b
        JOIN recipe_version v ON v.id = b.recipe_version_id
        JOIN recipe r ON r.id = v.recipe_id
        WHERE b.id = $1"#,
        id.get(),
    )
    .fetch_all(&mut *conn)
    .await?;
    load(conn, rows).await?.pop().ok_or(Error::NotFound)
}

/// The batch's formula with its actual percentages, sub-recipes resolved from their
/// frozen versions.
///
/// # Errors
/// [`Error::Recipes`] when a sub-recipe cannot be read.
pub async fn formula(conn: &mut PgConnection, batch: &Batch) -> Result<Formula, Error> {
    let mut subs: HashMap<RecipeVersionId, Formula> = HashMap::new();
    let mut lines = Vec::with_capacity(batch.lines.len());
    for line in &batch.lines {
        let part = match &line.part {
            LineRef::Ingredient(ingredient) => Part::Ingredient(ingredient.clone()),
            LineRef::Sub {
                version_id,
                recipe_id,
                name,
                version,
                kind,
            } => {
                let formula = match subs.get(version_id) {
                    Some(formula) => formula.clone(),
                    None => {
                        let formula = recipes::recipes::formula(conn, *version_id).await?;
                        subs.insert(*version_id, formula.clone());
                        formula
                    }
                };
                Part::Sub(SubRecipe {
                    version_id: *version_id,
                    recipe_id: *recipe_id,
                    name: name.clone(),
                    version: *version,
                    kind: *kind,
                    formula,
                })
            }
        };
        lines.push(Line {
            pct: line.actual_pct,
            part,
        });
    }
    Ok(Formula { lines })
}

/// Every batch of a bake day with its dough weight and formula, by code. Ingredient needs
/// after the deadline (F15) add these up.
///
/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Recipes`].
pub async fn formulas_for_day(
    conn: &mut PgConnection,
    day: BakeDayId,
) -> Result<Vec<BatchFormula>, Error> {
    let batches = list(conn, day).await?;
    let mut formulas = Vec::with_capacity(batches.len());
    for batch in batches {
        formulas.push(BatchFormula {
            batch_id: batch.id,
            code: batch.code,
            recipe_version_id: batch.recipe_version_id,
            dough_g: batch.dough_g(),
            formula: formula(conn, &batch).await?,
            recipe_name: batch.recipe_name,
            version: batch.version,
            outputs: batch.outputs,
        });
    }
    Ok(formulas)
}

/// Locks the batch row and returns its status.
async fn lock(conn: &mut PgConnection, id: BatchId) -> Result<Status, Error> {
    let status = sqlx::query_scalar!(
        "SELECT status FROM batch WHERE id = $1 FOR UPDATE",
        id.get()
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?;
    Status::from_code(&status)
}

fn clean_reason(reason: Option<&str>) -> Result<Option<String>, Error> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    if reason.is_some_and(|r| r.chars().count() > MAX_REASON_CHARS) {
        return Err(Error::Invalid("reason"));
    }
    Ok(reason.map(str::to_owned))
}

/// Changes percentages and planned pieces of a batch that is not mixed. Only values that
/// differ are written; each change goes into the audit log with the reason, and the batch
/// becomes adjusted. Nothing changed is not an error.
///
/// # Errors
/// [`Error::Locked`] once mixed; [`Error::Invalid`] with `pct`, `pieces`, `line`,
/// `product` or `reason` for values out of range or not in this batch, `empty` when every
/// line would be at 0%.
pub async fn adjust(
    conn: &mut PgConnection,
    id: BatchId,
    adjustment: &Adjustment,
    actor: Actor,
    clock: &dyn Clock,
) -> Result<(), Error> {
    let reason = clean_reason(adjustment.reason.as_deref())?;
    let mut tx = conn.begin().await?;
    if lock(&mut tx, id).await?.is_locked() {
        return Err(Error::Locked);
    }
    let batch = get(&mut tx, id).await?;
    let mut changed = false;
    let now = clock.now();
    for &(line_id, new_pct) in &adjustment.lines {
        if new_pct > recipes::recipes::MAX_LINE_PCT {
            return Err(Error::Invalid("pct"));
        }
        let line = batch
            .lines
            .iter()
            .find(|l| l.id == line_id)
            .ok_or(Error::Invalid("line"))?;
        if line.actual_pct == new_pct {
            continue;
        }
        sqlx::query!(
            "UPDATE batch_line SET actual_pct_x1000 = $2, reason = $3 WHERE id = $1",
            line_id.get(),
            i32::try_from(new_pct.thousandths()).map_err(|_| Error::Invalid("pct"))?,
            reason.as_deref(),
        )
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut tx,
            &Entry {
                entity: AUDIT_ENTITY,
                entity_id: id.get(),
                action: "line",
                before: Some(json!({ "line_id": line_id.get(), "pct_x1000": line.actual_pct.thousandths() })),
                after: Some(json!({ "line_id": line_id.get(), "pct_x1000": new_pct.thousandths(), "reason": reason })),
                actor,
                at: now,
            },
        )
        .await?;
        changed = true;
    }
    for &(product_id, pieces) in &adjustment.pieces {
        if pieces > MAX_PIECES {
            return Err(Error::Invalid("pieces"));
        }
        let output = batch
            .outputs
            .iter()
            .find(|o| o.product_id == product_id)
            .ok_or(Error::Invalid("product"))?;
        if output.planned_pieces == pieces {
            continue;
        }
        sqlx::query!(
            "UPDATE batch_output SET planned_pieces = $3 WHERE batch_id = $1 AND product_id = $2",
            id.get(),
            product_id.get(),
            i32::try_from(pieces).map_err(|_| Error::Invalid("pieces"))?,
        )
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut tx,
            &Entry {
                entity: AUDIT_ENTITY,
                entity_id: id.get(),
                action: "pieces",
                before: Some(
                    json!({ "product_id": product_id.get(), "pieces": output.planned_pieces }),
                ),
                after: Some(
                    json!({ "product_id": product_id.get(), "pieces": pieces, "reason": reason }),
                ),
                actor,
                at: now,
            },
        )
        .await?;
        changed = true;
    }
    if changed {
        require_lines(&mut tx, id).await?;
        sqlx::query!(
            "UPDATE batch SET status = 'adjusted' WHERE id = $1",
            id.get()
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Adds an ingredient the recipe version does not have, for example a different flour.
///
/// # Errors
/// [`Error::Locked`] once mixed; [`Error::Invalid`] with `pct` for 0 or above the line
/// maximum, `reason` for a long reason, or `lines` when the batch is full;
/// [`Error::Recipes`] with `NotFound` for an unknown ingredient.
pub async fn add_line(
    conn: &mut PgConnection,
    id: BatchId,
    ingredient: IngredientId,
    new_pct: Pct,
    reason: Option<&str>,
    actor: Actor,
    clock: &dyn Clock,
) -> Result<(), Error> {
    if new_pct.thousandths() == 0 || new_pct > recipes::recipes::MAX_LINE_PCT {
        return Err(Error::Invalid("pct"));
    }
    let reason = clean_reason(reason)?;
    let mut tx = conn.begin().await?;
    if lock(&mut tx, id).await?.is_locked() {
        return Err(Error::Locked);
    }
    recipes::ingredients::get(&mut tx, ingredient).await?;
    let lines = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM batch_line WHERE batch_id = $1"#,
        id.get()
    )
    .fetch_one(&mut *tx)
    .await?;
    if usize::try_from(lines).unwrap_or(usize::MAX) >= recipes::recipes::MAX_LINES {
        return Err(Error::Invalid("lines"));
    }
    let thousandths = i32::try_from(new_pct.thousandths()).map_err(|_| Error::Invalid("pct"))?;
    let line_id = sqlx::query_scalar!(
        "INSERT INTO batch_line (batch_id, position, ingredient_id, actual_pct_x1000, reason)
        SELECT $1, COALESCE(max(position), -1) + 1, $2, $3, $4 FROM batch_line WHERE batch_id = $1
        RETURNING id",
        id.get(),
        ingredient.get(),
        thousandths,
        reason.as_deref(),
    )
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        &Entry {
            entity: AUDIT_ENTITY,
            entity_id: id.get(),
            action: "add_line",
            before: None,
            after: Some(json!({
                "line_id": line_id,
                "ingredient_id": ingredient.get(),
                "pct_x1000": new_pct.thousandths(),
                "reason": reason,
            })),
            actor,
            at: clock.now(),
        },
    )
    .await?;
    sqlx::query!(
        "UPDATE batch SET status = 'adjusted' WHERE id = $1",
        id.get()
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Marks the batch mixed; from now on its lines and planned pieces are locked.
///
/// # Errors
/// [`Error::Locked`] when it is already mixed; [`Error::Invalid`] with `empty` when every
/// line is at 0%.
pub async fn mark_mixed(
    conn: &mut PgConnection,
    id: BatchId,
    actor: Actor,
    clock: &dyn Clock,
) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    let status = lock(&mut tx, id).await?;
    if status.is_locked() {
        return Err(Error::Locked);
    }
    require_lines(&mut tx, id).await?;
    let now = clock.now();
    sqlx::query!(
        "UPDATE batch SET status = 'mixed',
            mixed_at = timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
        WHERE id = $1",
        id.get(),
        to_micros(now),
    )
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        &Entry {
            entity: AUDIT_ENTITY,
            entity_id: id.get(),
            action: "mixed",
            before: Some(json!({ "status": status.code() })),
            after: Some(json!({ "status": Status::Mixed.code() })),
            actor,
            at: now,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Records the pieces out of the oven, and the wasted pieces among them, for every product
/// of a mixed batch; the batch becomes baked. Recording again corrects the numbers.
///
/// # Errors
/// [`Error::NotMixed`] before mixing; [`Error::Invalid`] with `product` for a product not
/// in the batch or missing, `pieces` above [`MAX_PIECES`], `waste` above the pieces.
pub async fn record_actuals(
    conn: &mut PgConnection,
    id: BatchId,
    actuals: &[Actual],
    actor: Actor,
    clock: &dyn Clock,
) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    let status = lock(&mut tx, id).await?;
    if !status.is_locked() {
        return Err(Error::NotMixed);
    }
    let batch = get(&mut tx, id).await?;
    if actuals.len() != batch.outputs.len()
        || batch
            .outputs
            .iter()
            .any(|o| !actuals.iter().any(|a| a.product_id == o.product_id))
    {
        return Err(Error::Invalid("product"));
    }
    for actual in actuals {
        if actual.pieces > MAX_PIECES {
            return Err(Error::Invalid("pieces"));
        }
        if actual.waste > actual.pieces {
            return Err(Error::Invalid("waste"));
        }
    }
    let now = clock.now();
    for actual in actuals {
        sqlx::query!(
            "UPDATE batch_output SET actual_pieces = $3, waste_pieces = $4
            WHERE batch_id = $1 AND product_id = $2",
            id.get(),
            actual.product_id.get(),
            i32::try_from(actual.pieces).map_err(|_| Error::Invalid("pieces"))?,
            i32::try_from(actual.waste).map_err(|_| Error::Invalid("waste"))?,
        )
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query!(
        "UPDATE batch SET status = 'baked',
            baked_at = COALESCE(baked_at, timestamptz 'epoch' + $2::bigint * interval '1 microsecond')
        WHERE id = $1",
        id.get(),
        to_micros(now),
    )
    .execute(&mut *tx)
    .await?;
    let before: Vec<_> = batch
        .outputs
        .iter()
        .map(|o| json!({ "product_id": o.product_id.get(), "pieces": o.actual_pieces, "waste": o.waste_pieces }))
        .collect();
    let after: Vec<_> = actuals
        .iter()
        .map(|a| json!({ "product_id": a.product_id.get(), "pieces": a.pieces, "waste": a.waste }))
        .collect();
    audit::record(
        &mut tx,
        &Entry {
            entity: AUDIT_ENTITY,
            entity_id: id.get(),
            action: "actuals",
            before: Some(json!(before)),
            after: Some(json!(after)),
            actor,
            at: now,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_is_the_date_and_the_number() {
        let code = BatchCode {
            date: jiff::civil::date(2026, 12, 5),
            mix_no: 2,
        };
        assert_eq!(code.to_string(), "261205-2");
        let code = BatchCode {
            date: jiff::civil::date(2030, 1, 9),
            mix_no: 14,
        };
        assert_eq!(code.to_string(), "300109-14");
    }

    #[test]
    fn codes_round_trip() {
        for status in [
            Status::Planned,
            Status::Adjusted,
            Status::Mixed,
            Status::Baked,
        ] {
            assert_eq!(Status::from_code(status.code()).ok(), Some(status));
        }
        assert!(Status::from_code("x").is_err());
    }

    #[test]
    fn reasons_are_trimmed_and_short() {
        assert_eq!(clean_reason(Some("  ")).ok(), Some(None));
        assert_eq!(
            clean_reason(Some(" natte bloem ")).ok(),
            Some(Some("natte bloem".to_owned()))
        );
        assert!(clean_reason(Some(&"x".repeat(MAX_REASON_CHARS + 1))).is_err());
    }
}
