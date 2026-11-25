//! Ingredient needs (F15): grams per ingredient summed over the doughs of a bake day.
//!
//! A [`DoughGrams`] is one dough broken down to its ingredients in whole grams. A recipe
//! version scaled to a weight gives one ([`DoughGrams::from_formula`]); batches (F14a)
//! give one per batch from their own lines. [`shopping_list`] adds them up without any
//! further rounding, so every total is exactly the sum of its parts (ADR 0041).

use domain::{ProductId, RecipeId, RecipeVersionId};
use sqlx::PgConnection;

use crate::Error;
use crate::formula::{Formula, IngredientRef, ScaleError, ingredient_totals};

/// One dough's ingredients in whole grams, each ingredient once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoughGrams {
    pub ingredients: Vec<(IngredientRef, u64)>,
}

impl DoughGrams {
    /// The recipe scaled to `grams` with the rounding rule of ADR 0037, sub-recipes
    /// resolved down to their ingredients. The ingredients add up to `grams` exactly.
    ///
    /// # Errors
    /// [`ScaleError::Empty`] when the recipe or one of its sub-recipes has no lines.
    pub fn from_formula(formula: &Formula, grams: u64) -> Result<Self, ScaleError> {
        let scaled = formula.scale(grams)?;
        Ok(Self {
            ingredients: ingredient_totals(&scaled)
                .into_iter()
                .map(|(ingredient, grams)| (ingredient.clone(), grams))
                .collect(),
        })
    }

    #[must_use]
    pub fn total(&self) -> u64 {
        self.ingredients.iter().map(|(_, grams)| grams).sum()
    }
}

/// One line of the shopping list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub ingredient: IngredientRef,
    pub grams: u64,
    /// The grams each dough brings, as (index of the dough, grams); they add up to `grams`.
    pub parts: Vec<(usize, u64)>,
}

/// Grams per ingredient over all doughs, by name (ignoring case), then id.
pub fn shopping_list<'a>(doughs: impl IntoIterator<Item = &'a DoughGrams>) -> Vec<Need> {
    let mut list: Vec<Need> = Vec::new();
    for (index, dough) in doughs.into_iter().enumerate() {
        for (ingredient, grams) in &dough.ingredients {
            match list
                .iter_mut()
                .find(|need| need.ingredient.id == ingredient.id)
            {
                Some(need) => {
                    need.grams += grams;
                    need.parts.push((index, *grams));
                }
                None => list.push(Need {
                    ingredient: ingredient.clone(),
                    grams: *grams,
                    parts: vec![(index, *grams)],
                }),
            }
        }
    }
    list.sort_by(|a, b| {
        a.ingredient
            .name
            .to_lowercase()
            .cmp(&b.ingredient.name.to_lowercase())
            .then(a.ingredient.id.cmp(&b.ingredient.id))
    });
    list
}

/// The recipe a product is made from and the version production uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductRecipe {
    pub product_id: ProductId,
    pub dough_g_per_piece: u32,
    pub recipe: Option<RecipeInUse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeInUse {
    pub recipe_id: RecipeId,
    pub name: String,
    /// The version in use and its number; `None` when the recipe has none.
    pub version: Option<(RecipeVersionId, i32)>,
}

/// The recipe of each product in `products` that exists, by product id.
///
/// # Errors
/// [`Error::Db`], or [`Error::Corrupt`] for a negative dough weight.
pub async fn product_recipes(
    conn: &mut PgConnection,
    products: &[ProductId],
) -> Result<Vec<ProductRecipe>, Error> {
    let ids: Vec<i64> = products.iter().map(|id| id.get()).collect();
    sqlx::query!(
        r#"SELECT p.id, p.dough_g_per_piece, r.id AS "recipe_id?", r.name AS "recipe_name?",
            v.id AS "version_id?", v.version AS "version?"
        FROM product p
        LEFT JOIN recipe r ON r.id = p.recipe_id
        LEFT JOIN recipe_version v ON v.id = r.current_version_id
        WHERE p.id = ANY($1)
        ORDER BY p.id"#,
        &ids,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        let dough_g_per_piece = u32::try_from(row.dough_g_per_piece)
            .map_err(|_| Error::Corrupt("negative dough weight".to_owned()))?;
        let recipe = match (row.recipe_id, row.recipe_name) {
            (Some(id), Some(name)) => Some(RecipeInUse {
                recipe_id: RecipeId::new(id),
                name,
                version: row
                    .version_id
                    .zip(row.version)
                    .map(|(id, version)| (RecipeVersionId::new(id), version)),
            }),
            _ => None,
        };
        Ok(ProductRecipe {
            product_id: ProductId::new(row.id),
            dough_g_per_piece,
            recipe,
        })
    })
    .collect()
}
