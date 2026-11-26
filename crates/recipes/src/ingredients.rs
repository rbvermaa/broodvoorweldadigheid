//! Ingredients: name, role, allergens and the name on the ingredient list (F14, F10).

use std::collections::BTreeSet;

use domain::{Allergen, IngredientId, RecipeVersionId};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::error::unique;
use crate::formula::{IngredientRole, MAX_STARTER_HYDRATION, Pct};
use crate::recipes::{formula, lock_graph, set_pcts};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngredientInput {
    pub name: String,
    /// The name on the product's ingredient list; empty means the same as `name`.
    pub label_name: String,
    pub role: IngredientRole,
    /// Water per 100 g of flour; required for a starter and ignored for other roles.
    pub hydration: Option<Pct>,
    pub supplier_name: String,
    pub allergens: BTreeSet<Allergen>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingredient {
    pub id: IngredientId,
    pub name: String,
    pub label_name: String,
    pub role: IngredientRole,
    /// Water per 100 g of flour of a starter; `None` for other roles.
    pub hydration: Option<Pct>,
    pub supplier_name: String,
    pub allergens: BTreeSet<Allergen>,
}

impl Ingredient {
    #[must_use]
    pub fn label(&self) -> &str {
        if self.label_name.is_empty() {
            &self.name
        } else {
            &self.label_name
        }
    }
}

struct Row {
    id: i64,
    name: String,
    label_name: String,
    role: String,
    starter_hydration_x1000: Option<i32>,
    supplier_name: String,
    allergens: Vec<String>,
}

fn allergens_from_db(codes: &[String]) -> Result<BTreeSet<Allergen>, Error> {
    codes
        .iter()
        .map(|code| code.parse().map_err(|e| Error::Corrupt(format!("{e}"))))
        .collect()
}

fn allergens_to_db(allergens: &BTreeSet<Allergen>) -> Vec<String> {
    allergens.iter().map(|a| a.code().to_owned()).collect()
}

impl TryFrom<Row> for Ingredient {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: IngredientId::new(row.id),
            name: row.name,
            label_name: row.label_name,
            role: row.role.parse()?,
            hydration: row
                .starter_hydration_x1000
                .map(|h| {
                    u32::try_from(h)
                        .map(Pct::from_thousandths)
                        .map_err(|_| Error::Corrupt("negative hydration".to_owned()))
                })
                .transpose()?,
            supplier_name: row.supplier_name,
            allergens: allergens_from_db(&row.allergens)?,
        })
    }
}

/// The hydration to store: a starter's, checked, and none for other roles.
fn validate(input: &IngredientInput) -> Result<Option<Pct>, Error> {
    if input.name.trim().is_empty() {
        return Err(Error::Invalid("name"));
    }
    if input.role != IngredientRole::Starter {
        return Ok(None);
    }
    match input.hydration {
        Some(hydration) if hydration <= MAX_STARTER_HYDRATION => Ok(Some(hydration)),
        _ => Err(Error::Invalid("hydration")),
    }
}

fn hydration_to_db(hydration: Option<Pct>) -> Option<i32> {
    hydration.map(|h| i32::try_from(h.thousandths()).unwrap_or(i32::MAX))
}

/// # Errors
/// [`Error::Invalid`] for an empty name or a starter without a hydration from 0 to 500%,
/// [`Error::Duplicate`] for a name in use.
pub async fn create(conn: &mut PgConnection, input: &IngredientInput) -> Result<Ingredient, Error> {
    let hydration = validate(input)?;
    sqlx::query_as!(
        Row,
        r#"INSERT INTO ingredient
            (name, label_name, role, starter_hydration_x1000, supplier_name, allergens)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, name, label_name, role, starter_hydration_x1000, supplier_name, allergens"#,
        input.name.trim(),
        input.label_name.trim(),
        input.role.code(),
        hydration_to_db(hydration),
        input.supplier_name.trim(),
        &allergens_to_db(&input.allergens),
    )
    .fetch_one(conn)
    .await
    .map_err(unique)?
    .try_into()
}

/// A frozen recipe version or a batch keeps its hydration, so the role of an ingredient
/// it uses, or a starter's hydration, cannot change. When a starter is involved, every draft that
/// uses the ingredient, directly or through sub-recipes, keeps its grams and total flour
/// ([`crate::Formula::rebased`], ADR 0037).
///
/// # Errors
/// [`Error::NotFound`], [`Error::Invalid`], [`Error::Duplicate`] or [`Error::InUse`].
pub async fn update(
    conn: &mut PgConnection,
    id: IngredientId,
    input: &IngredientInput,
) -> Result<Ingredient, Error> {
    let hydration = validate(input)?;
    let mut tx = conn.begin().await?;
    // Taken before reading, so no freeze or other edit can slip in before the update.
    lock_graph(&mut tx).await?;
    // Batch lines take only their batch lock; their foreign key waits for this row lock,
    // so a starter added to a batch meanwhile is seen by the check below.
    sqlx::query!(
        "SELECT id FROM ingredient WHERE id = $1 FOR UPDATE",
        id.get()
    )
    .fetch_optional(&mut *tx)
    .await?;
    let current = get(&mut tx, id).await?;
    let counts_differently = current.role != input.role || current.hydration != hydration;
    let mut drafts = Vec::new();
    if counts_differently {
        let frozen_use = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM recipe_line l
                JOIN recipe_version v ON v.id = l.recipe_version_id
                WHERE l.ingredient_id = $1 AND v.frozen_at IS NOT NULL)
            OR EXISTS (SELECT 1 FROM batch_line WHERE ingredient_id = $1) AS "used!""#,
            id.get(),
        )
        .fetch_one(&mut *tx)
        .await?;
        if frozen_use {
            return Err(Error::InUse);
        }
        if current.role == IngredientRole::Starter || input.role == IngredientRole::Starter {
            for version in versions_using(&mut tx, id).await? {
                let before = formula(&mut tx, version).await?;
                drafts.push((version, before));
            }
        }
    }
    let updated = sqlx::query_as!(
        Row,
        r#"UPDATE ingredient SET name = $2, label_name = $3, role = $4,
            starter_hydration_x1000 = $5, supplier_name = $6, allergens = $7
        WHERE id = $1
        RETURNING id, name, label_name, role, starter_hydration_x1000, supplier_name, allergens"#,
        id.get(),
        input.name.trim(),
        input.label_name.trim(),
        input.role.code(),
        hydration_to_db(hydration),
        input.supplier_name.trim(),
        &allergens_to_db(&input.allergens),
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(unique)?
    .ok_or(Error::NotFound)?
    .try_into()?;
    // Every new formula is read before any draft changes, so a sub-recipe rebased first
    // does not shift the figures of the drafts that use it.
    let mut rebased = Vec::new();
    for (version, before) in drafts {
        let after = formula(&mut tx, version).await?;
        rebased.extend(before.rebased(&after).map(|pcts| (version, pcts)));
    }
    for (version, pcts) in rebased {
        set_pcts(&mut tx, version, &pcts).await?;
    }
    tx.commit().await?;
    Ok(updated)
}

/// Every version that holds the ingredient, directly or through sub-recipes.
async fn versions_using(
    conn: &mut PgConnection,
    id: IngredientId,
) -> Result<Vec<RecipeVersionId>, Error> {
    Ok(sqlx::query_scalar!(
        r#"WITH RECURSIVE up (version_id) AS (
            SELECT recipe_version_id FROM recipe_line WHERE ingredient_id = $1
            UNION
            SELECT l.recipe_version_id
            FROM recipe_line l JOIN up u ON l.sub_recipe_version_id = u.version_id
        )
        SELECT u.version_id AS "version_id!" FROM up u
        JOIN recipe_version v ON v.id = u.version_id AND v.frozen_at IS NULL
        ORDER BY u.version_id"#,
        id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(RecipeVersionId::new)
    .collect())
}

/// # Errors
/// [`Error::NotFound`] when there is no such ingredient.
pub async fn get(conn: &mut PgConnection, id: IngredientId) -> Result<Ingredient, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name, label_name, role, starter_hydration_x1000, supplier_name, allergens
        FROM ingredient WHERE id = $1"#,
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// Every ingredient by name.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list(conn: &mut PgConnection) -> Result<Vec<Ingredient>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name, label_name, role, starter_hydration_x1000, supplier_name, allergens
        FROM ingredient ORDER BY lower(name), id"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Ingredient::try_from)
    .collect()
}
