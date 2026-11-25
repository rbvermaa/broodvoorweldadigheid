//! Allergens and ingredient list computed from a recipe (F17, ADR 0046).
//!
//! The list holds every ingredient on a path of lines above 0%, sub-recipes merged in, by
//! its label name. Ingredients whose label names differ only in case are one entry, under
//! the spelling the recipe uses first. Entries go in descending order of their weight in
//! the final dough; equal weights keep the order in which the recipe first uses them.
//! The allergens are the union of those ingredients'.

use std::collections::{BTreeSet, HashMap};

use catalog::{ComputedAllergens, IngredientList, ListedIngredient, Product};
use domain::{Allergen, Clock, IngredientId, ProductId, RecipeVersionId, StaffUserId};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::formula::{Formula, Line, Part, SubRecipe, ingredient_totals};
use crate::ingredients::{self, Ingredient};
use crate::needs::product_recipes;
use crate::recipes::{formula, lock_graph};

/// The dough weight for the rounded fallback in [`weights`].
const REFERENCE_GRAMS: u64 = 1_000_000_000_000;

/// The formula without lines at 0% and without what is below them.
fn pruned(lines: &[Line]) -> Result<Vec<Line>, Error> {
    let kept: Vec<Line> = lines
        .iter()
        .filter(|line| line.pct.thousandths() > 0)
        .map(|line| {
            Ok(Line {
                pct: line.pct,
                part: match &line.part {
                    Part::Ingredient(ingredient) => Part::Ingredient(ingredient.clone()),
                    Part::Sub(sub) => Part::Sub(SubRecipe {
                        formula: Formula {
                            lines: pruned(&sub.formula.lines)?,
                        },
                        ..sub.clone()
                    }),
                },
            })
        })
        .collect::<Result<_, Error>>()?;
    if kept.is_empty() {
        return Err(Error::Unweighable);
    }
    Ok(kept)
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Each leaf as the fraction `numerator / denominator` of the dough.
fn fractions(
    lines: &[Line],
    numerator: u128,
    denominator: u128,
    out: &mut Vec<(IngredientId, u128, u128)>,
) -> Option<()> {
    let sum: u128 = lines.iter().map(|l| u128::from(l.pct.thousandths())).sum();
    let denominator = denominator.checked_mul(sum)?;
    for line in lines {
        let numerator = numerator.checked_mul(u128::from(line.pct.thousandths()))?;
        match &line.part {
            Part::Ingredient(ingredient) => out.push((ingredient.id, numerator, denominator)),
            Part::Sub(sub) => fractions(&sub.formula.lines, numerator, denominator, out)?,
        }
    }
    Some(())
}

/// Exact weights on one common denominator, so equal weights compare equal; `None` when
/// they do not fit in 128 bits.
fn exact_weights(lines: &[Line]) -> Option<Vec<(IngredientId, u128)>> {
    let mut leaves = Vec::new();
    fractions(lines, 1, 1, &mut leaves)?;
    let mut common: u128 = 1;
    for &(_, _, denominator) in &leaves {
        common = (common / gcd(common, denominator)).checked_mul(denominator)?;
    }
    leaves
        .into_iter()
        .map(|(id, numerator, denominator)| {
            Some((id, numerator.checked_mul(common / denominator)?))
        })
        .collect()
}

/// The weight of every ingredient in the dough, in order of first use. Exact when it fits,
/// else whole units of a dough of [`REFERENCE_GRAMS`].
fn weights(lines: &[Line]) -> Result<Vec<(IngredientId, u128)>, Error> {
    let leaves = match exact_weights(lines) {
        Some(leaves) => leaves,
        None => {
            let formula = Formula {
                lines: lines.to_vec(),
            };
            let scaled = formula
                .scale(REFERENCE_GRAMS)
                .map_err(|_| Error::Unweighable)?;
            ingredient_totals(&scaled)
                .into_iter()
                .map(|(ingredient, grams)| (ingredient.id, u128::from(grams)))
                .collect()
        }
    };
    let mut totals: Vec<(IngredientId, u128)> = Vec::new();
    for (id, weight) in leaves {
        match totals.iter_mut().find(|(known, _)| *known == id) {
            Some((_, total)) => *total += weight,
            None => totals.push((id, weight)),
        }
    }
    Ok(totals)
}

struct Entry<'a> {
    name: &'a str,
    weight: u128,
    allergens: BTreeSet<Allergen>,
}

/// # Errors
/// [`Error::Unweighable`] when the formula or one of its sub-recipes has no lines above
/// 0%; [`Error::Corrupt`] when an ingredient is missing from `ingredients`.
pub fn compute(
    formula: &Formula,
    ingredients: &HashMap<IngredientId, Ingredient>,
) -> Result<ComputedAllergens, Error> {
    let lines = pruned(&formula.lines)?;
    let mut entries: Vec<Entry<'_>> = Vec::new();
    for (id, weight) in weights(&lines)? {
        let ingredient = ingredients
            .get(&id)
            .ok_or_else(|| Error::Corrupt(format!("ingredient {}", id.get())))?;
        let key = ingredient.label().to_lowercase();
        match entries.iter_mut().find(|e| e.name.to_lowercase() == key) {
            Some(entry) => {
                entry.weight += weight;
                entry.allergens.extend(ingredient.allergens.iter().copied());
            }
            None => entries.push(Entry {
                name: ingredient.label(),
                weight,
                allergens: ingredient.allergens.clone(),
            }),
        }
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.weight));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::{IngredientRef, IngredientRole, Pct, RecipeKind};
    use domain::RecipeId;

    fn leaf(id: i64, thousandths: u32) -> Line {
        Line {
            pct: Pct::from_thousandths(thousandths),
            part: Part::Ingredient(IngredientRef {
                id: IngredientId::new(id),
                name: String::new(),
                role: IngredientRole::Other,
            }),
        }
    }

    fn sub(lines: Vec<Line>) -> Line {
        Line {
            pct: Pct::from_thousandths(1000),
            part: Part::Sub(SubRecipe {
                version_id: RecipeVersionId::new(1),
                recipe_id: RecipeId::new(1),
                name: String::new(),
                version: 1,
                kind: RecipeKind::Filling,
                formula: Formula { lines },
            }),
        }
    }

    /// Three branches three levels deep whose sums are different primes: their common
    /// denominator does not fit in 128 bits.
    fn too_fine() -> Vec<Line> {
        let branch = |id: i64, [a, b, c]: [u32; 3]| {
            sub(vec![
                leaf(id, a),
                sub(vec![leaf(id, b), sub(vec![leaf(id, c), leaf(9, 1)])]),
            ])
        };
        vec![
            branch(1, [999_982, 999_978, 999_960]),
            branch(2, [999_952, 999_930, 999_906]),
            branch(3, [999_870, 999_852, 999_828]),
            leaf(4, 1000),
        ]
    }

    #[test]
    fn weights_fall_back_to_whole_units_when_exact_ones_do_not_fit() {
        let lines = too_fine();
        assert_eq!(exact_weights(&lines), None);
        let fallback = weights(&lines).unwrap();
        let ids: Vec<i64> = fallback.iter().map(|(id, _)| id.get()).collect();
        assert_eq!(ids, [1, 9, 2, 3, 4]);
        let total: u128 = fallback.iter().map(|&(_, w)| w).sum();
        assert_eq!(total, u128::from(REFERENCE_GRAMS));
    }

    #[test]
    fn exact_weights_share_one_denominator() {
        let lines = vec![leaf(1, 50_000), sub(vec![leaf(1, 1000), leaf(2, 2000)])];
        let exact = exact_weights(&lines).unwrap();
        // 50/51 flour, then 1/51 × 1/3 flour and 1/51 × 2/3 water.
        let w: Vec<u128> = exact.iter().map(|&(_, w)| w).collect();
        assert_eq!((w[0], w[2]), (150 * w[1], 2 * w[1]));
        assert_eq!(gcd(12, 18), 6);
    }
}
