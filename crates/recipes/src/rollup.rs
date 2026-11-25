//! Allergens and ingredient list computed from a recipe (F17, ADR 0046).
//!
//! The list holds every ingredient on a path of lines above 0%, sub-recipes merged in, by
//! its label name. Ingredients with the same label name are one entry. Entries go in
//! descending order of their weight in the final dough; equal weights keep the order in
//! which the recipe first uses them. The allergens are the union of those ingredients'.

use std::collections::{BTreeSet, HashMap};

use catalog::{ComputedAllergens, IngredientList, ListedIngredient, Product};
use domain::{Allergen, Clock, IngredientId, ProductId, RecipeVersionId, StaffUserId};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::formula::{Formula, Line, Part, ingredient_totals};
use crate::ingredients::{self, Ingredient};
use crate::needs::product_recipes;
use crate::recipes::{formula, lock_graph};

/// The dough weight the order by weight is decided at: large enough that the rounding of
/// whole units does not move an ingredient past another one.
const REFERENCE_GRAMS: u64 = 1_000_000_000_000;

fn used(lines: &[Line], ids: &mut BTreeSet<IngredientId>) {
    for line in lines.iter().filter(|line| line.pct.thousandths() > 0) {
        match &line.part {
            Part::Ingredient(ingredient) => {
                ids.insert(ingredient.id);
            }
            Part::Sub(sub) => used(&sub.formula.lines, ids),
        }
    }
}

struct Entry<'a> {
    name: &'a str,
    grams: u64,
    allergens: BTreeSet<Allergen>,
}

/// # Errors
/// [`Error::Unweighable`] when the formula or one of its sub-recipes has no lines;
/// [`Error::Corrupt`] when an ingredient is missing from `ingredients`.
pub fn compute(
    formula: &Formula,
    ingredients: &HashMap<IngredientId, Ingredient>,
) -> Result<ComputedAllergens, Error> {
    let scaled = formula
        .scale(REFERENCE_GRAMS)
        .map_err(|_| Error::Unweighable)?;
    let mut ids = BTreeSet::new();
    used(&formula.lines, &mut ids);
    let mut entries: Vec<Entry<'_>> = Vec::new();
    for (line, grams) in ingredient_totals(&scaled) {
        if !ids.contains(&line.id) {
            continue;
        }
        let ingredient = ingredients
            .get(&line.id)
            .ok_or_else(|| Error::Corrupt(format!("ingredient {}", line.id.get())))?;
        match entries.iter_mut().find(|e| e.name == ingredient.label()) {
            Some(entry) => {
                entry.grams += grams;
                entry.allergens.extend(ingredient.allergens.iter().copied());
            }
            None => entries.push(Entry {
                name: ingredient.label(),
                grams,
                allergens: ingredient.allergens.clone(),
            }),
        }
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.grams));
    Ok(ComputedAllergens {
        allergens: entries
            .iter()
            .flat_map(|e| e.allergens.iter().copied())
            .collect(),
        ingredients: IngredientList(
            entries
                .into_iter()
                .map(|e| ListedIngredient {
                    name: e.name.to_owned(),
                    emphasised: !e.allergens.is_empty(),
                })
                .collect(),
        ),
    })
}

/// [`compute`] with the ingredients as stored now.
///
/// # Errors
/// As [`compute`], or [`Error::Db`].
pub async fn compute_stored(
    conn: &mut PgConnection,
    formula: &Formula,
) -> Result<ComputedAllergens, Error> {
    let book: HashMap<IngredientId, Ingredient> = ingredients::list(conn)
        .await?
        .into_iter()
        .map(|i| (i.id, i))
        .collect();
    compute(formula, &book)
}

/// # Errors
/// As [`compute`], or [`Error::Nesting`] from loading the formula.
pub async fn for_version(
    conn: &mut PgConnection,
    id: RecipeVersionId,
) -> Result<ComputedAllergens, Error> {
    let formula = formula(conn, id).await?;
    compute_stored(conn, &formula).await
}

/// The values a product's recipe version in use computes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductRollUp {
    pub recipe_name: String,
    pub version_id: RecipeVersionId,
    pub version: i32,
    /// `None` when the version or one of its sub-recipes has no lines to weigh.
    pub computed: Option<ComputedAllergens>,
}

/// `None` for a product without a recipe or whose recipe has no version in use: its
/// allergens and ingredient text stay as entered by hand.
///
/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Nesting`].
pub async fn for_product(
    conn: &mut PgConnection,
    product: ProductId,
) -> Result<Option<ProductRollUp>, Error> {
    let Some(recipe) = product_recipes(conn, &[product])
        .await?
        .pop()
        .and_then(|p| p.recipe)
    else {
        return Ok(None);
    };
    let Some((version_id, version)) = recipe.version else {
        return Ok(None);
    };
    let computed = match for_version(conn, version_id).await {
        Ok(computed) => Some(computed),
        Err(Error::Unweighable) => None,
        Err(other) => return Err(other),
    };
    Ok(Some(ProductRollUp {
        recipe_name: recipe.name,
        version_id,
        version,
        computed,
    }))
}

/// Approves the values the recipe computes for the product as its allergen data, when
/// they equal `reviewed` and the product is still at `revision`. Holds the recipe lock,
/// so no recipe or ingredient change can come between computing and approving.
///
/// # Errors
/// [`Error::AllergensChanged`] when `reviewed` or `revision` is out of date;
/// [`Error::NotFound`] for an unknown product or one without a recipe to compute from.
pub async fn approve_product(
    conn: &mut PgConnection,
    product: ProductId,
    reviewed: &ComputedAllergens,
    revision: i64,
    by: StaffUserId,
    clock: &dyn Clock,
) -> Result<Product, Error> {
    let mut tx = conn.begin().await?;
    lock_graph(&mut tx).await?;
    let computed = for_product(&mut tx, product)
        .await?
        .and_then(|rolled| rolled.computed)
        .ok_or(Error::NotFound)?;
    if &computed != reviewed {
        return Err(Error::AllergensChanged);
    }
    let approved =
        catalog::products::approve_computed(&mut tx, product, &computed, revision, by, clock)
            .await
            .map_err(|error| match error {
                catalog::Error::AllergensChanged => Error::AllergensChanged,
                catalog::Error::NotFound => Error::NotFound,
                other => Error::Catalog(other),
            })?;
    tx.commit().await?;
    Ok(approved)
}
