//! Ingredients: name, role, allergens and the name on the ingredient list (F14, F10).

use std::collections::BTreeSet;

use domain::{Allergen, IngredientId};
use sqlx::PgConnection;

use crate::Error;
use crate::error::unique;
use crate::formula::IngredientRole;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngredientInput {
    pub name: String,
    /// The name on the product's ingredient list; empty means the same as `name`.
    pub label_name: String,
    pub role: IngredientRole,
    pub supplier_name: String,
    pub allergens: BTreeSet<Allergen>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingredient {
    pub id: IngredientId,
    pub name: String,
    pub label_name: String,
    pub role: IngredientRole,
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
            supplier_name: row.supplier_name,
            allergens: allergens_from_db(&row.allergens)?,
        })
    }
}

fn validate(input: &IngredientInput) -> Result<(), Error> {
    if input.name.trim().is_empty() {
        return Err(Error::Invalid("name"));
    }
    Ok(())
}

/// # Errors
/// [`Error::Invalid`] for an empty name, [`Error::Duplicate`] for a name in use.
pub async fn create(conn: &mut PgConnection, input: &IngredientInput) -> Result<Ingredient, Error> {
    validate(input)?;
    sqlx::query_as!(
        Row,
        r#"INSERT INTO ingredient (name, label_name, role, supplier_name, allergens)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, name, label_name, role, supplier_name, allergens"#,
        input.name.trim(),
        input.label_name.trim(),
        input.role.code(),
        input.supplier_name.trim(),
        &allergens_to_db(&input.allergens),
    )
    .fetch_one(conn)
    .await
    .map_err(unique)?
    .try_into()
}

/// # Errors
/// [`Error::NotFound`], [`Error::Invalid`] or [`Error::Duplicate`].
pub async fn update(
    conn: &mut PgConnection,
    id: IngredientId,
    input: &IngredientInput,
) -> Result<Ingredient, Error> {
    validate(input)?;
    sqlx::query_as!(
        Row,
        r#"UPDATE ingredient SET name = $2, label_name = $3, role = $4, supplier_name = $5,
            allergens = $6
        WHERE id = $1
        RETURNING id, name, label_name, role, supplier_name, allergens"#,
        id.get(),
        input.name.trim(),
        input.label_name.trim(),
        input.role.code(),
        input.supplier_name.trim(),
        &allergens_to_db(&input.allergens),
    )
    .fetch_optional(conn)
    .await
    .map_err(unique)?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// # Errors
/// [`Error::NotFound`] when there is no such ingredient.
pub async fn get(conn: &mut PgConnection, id: IngredientId) -> Result<Ingredient, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name, label_name, role, supplier_name, allergens
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
        r#"SELECT id, name, label_name, role, supplier_name, allergens
        FROM ingredient ORDER BY lower(name), id"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Ingredient::try_from)
    .collect()
}
