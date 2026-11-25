//! Allergens and ingredient list computed from a recipe (F17, ADR 0046).
//!
//! The list holds every ingredient on a path of lines above 0%, by its label name.
//! Sub-recipes merge into the list, except those that show as a compound ingredient
//! (`recipes::compound_names`): such a sub-recipe is one entry under its compound name,
//! its own ingredients in brackets. A compound inside a compound merges into the outer
//! one, so brackets nest one level. Ingredients whose label names differ only in case
//! are one entry, under the spelling the recipe uses first; so are compounds with such
//! names. Entries go in descending order of their weight in the final dough, a compound
//! by the sum of its ingredients and its ingredients by their own weight; equal weights
//! keep the order in which the recipe first uses them. The allergens are the union of
//! every ingredient's.

use std::collections::{BTreeSet, HashMap};

use catalog::{ComputedAllergens, IngredientList, ListedIngredient, Product};
use domain::{Allergen, Clock, IngredientId, ProductId, RecipeId, RecipeVersionId, StaffUserId};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::formula::{Formula, Line, Part, Scaled, SubRecipe};
use crate::ingredients::{self, Ingredient};
use crate::needs::product_recipes;
use crate::recipes::{compound_names, formula, lock_graph};

/// The dough weight for the rounded fallback in [`weights`].
const REFERENCE_GRAMS: u64 = 1_000_000_000_000;

/// The compound ingredient name of each recipe that shows as one.
pub type CompoundNames = HashMap<RecipeId, String>;

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

/// One ingredient line of the dough with its weight, and the outermost compound
/// sub-recipe it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Leaf {
    id: IngredientId,
    weight: u128,
    compound: Option<RecipeId>,
}

/// The compound a sub-recipe's lines belong to, given the one around it.
fn inner(outer: Option<RecipeId>, sub: &SubRecipe, compounds: &CompoundNames) -> Option<RecipeId> {
    outer.or_else(|| {
        compounds
            .contains_key(&sub.recipe_id)
            .then_some(sub.recipe_id)
    })
}

struct Fraction {
    id: IngredientId,
    numerator: u128,
    denominator: u128,
    compound: Option<RecipeId>,
}

/// Each leaf as the fraction `numerator / denominator` of the dough.
fn fractions(
    lines: &[Line],
    numerator: u128,
    denominator: u128,
    compound: Option<RecipeId>,
    compounds: &CompoundNames,
    out: &mut Vec<Fraction>,
) -> Option<()> {
    let sum: u128 = lines.iter().map(|l| u128::from(l.pct.thousandths())).sum();
    let denominator = denominator.checked_mul(sum)?;
    for line in lines {
        let numerator = numerator.checked_mul(u128::from(line.pct.thousandths()))?;
        match &line.part {
            Part::Ingredient(ingredient) => out.push(Fraction {
                id: ingredient.id,
                numerator,
                denominator,
                compound,
            }),
            Part::Sub(sub) => fractions(
                &sub.formula.lines,
                numerator,
                denominator,
                inner(compound, sub, compounds),
                compounds,
                out,
            )?,
        }
    }
    Some(())
}

/// Exact weights on one common denominator, so equal weights compare equal; `None` when
/// they do not fit in 128 bits.
fn exact_weights(lines: &[Line], compounds: &CompoundNames) -> Option<Vec<Leaf>> {
    let mut leaves = Vec::new();
    fractions(lines, 1, 1, None, compounds, &mut leaves)?;
    let mut common: u128 = 1;
    for leaf in &leaves {
        common = (common / gcd(common, leaf.denominator)).checked_mul(leaf.denominator)?;
    }
    leaves
        .into_iter()
        .map(|leaf| {
            Some(Leaf {
                id: leaf.id,
                weight: leaf.numerator.checked_mul(common / leaf.denominator)?,
                compound: leaf.compound,
            })
        })
        .collect()
}

fn scaled_leaves(
    scaled: &[Scaled<'_>],
    compound: Option<RecipeId>,
    compounds: &CompoundNames,
    out: &mut Vec<Leaf>,
) {
    for line in scaled {
        match &line.line.part {
            Part::Ingredient(ingredient) => out.push(Leaf {
                id: ingredient.id,
                weight: u128::from(line.grams),
                compound,
            }),
            Part::Sub(sub) => {
                scaled_leaves(&line.lines, inner(compound, sub, compounds), compounds, out);
            }
        }
    }
}

/// The weight of every ingredient line in the dough, in the order the recipe uses them.
/// Exact when it fits, else whole units of a dough of [`REFERENCE_GRAMS`].
fn weights(lines: &[Line], compounds: &CompoundNames) -> Result<Vec<Leaf>, Error> {
    if let Some(leaves) = exact_weights(lines, compounds) {
        return Ok(leaves);
    }
    let formula = Formula {
        lines: lines.to_vec(),
    };
    let scaled = formula
        .scale(REFERENCE_GRAMS)
        .map_err(|_| Error::Unweighable)?;
    let mut leaves = Vec::new();
    scaled_leaves(&scaled, None, compounds, &mut leaves);
    Ok(leaves)
}

struct Entry<'a> {
    name: &'a str,
    weight: u128,
    allergens: BTreeSet<Allergen>,
    /// `Some` for a compound ingredient.
    parts: Option<Vec<Entry<'a>>>,
}

impl Entry<'_> {
    fn listed(self) -> ListedIngredient {
        match self.parts {
            Some(mut parts) => {
                parts.sort_by_key(|e| std::cmp::Reverse(e.weight));
                ListedIngredient::compound(
                    self.name,
                    parts.into_iter().map(Entry::listed).collect(),
                )
            }
            None => ListedIngredient::plain(self.name, !self.allergens.is_empty()),
        }
    }
}

fn add<'a>(
    entries: &mut Vec<Entry<'a>>,
    name: &'a str,
    compound: bool,
    weight: u128,
    allergens: &BTreeSet<Allergen>,
) -> usize {
    let key = name.to_lowercase();
    let index = match entries
        .iter()
        .position(|e| e.parts.is_some() == compound && e.name.to_lowercase() == key)
    {
        Some(index) => index,
        None => {
            entries.push(Entry {
                name,
                weight: 0,
                allergens: BTreeSet::new(),
                parts: compound.then(Vec::new),
            });
            entries.len() - 1
        }
    };
    let entry = &mut entries[index];
    entry.weight += weight;
    entry.allergens.extend(allergens.iter().copied());
    index
}

/// # Errors
/// [`Error::Unweighable`] when the formula or one of its sub-recipes has no lines above
/// 0%; [`Error::Corrupt`] when an ingredient is missing from `ingredients`.
pub fn compute(
    formula: &Formula,
    ingredients: &HashMap<IngredientId, Ingredient>,
    compounds: &CompoundNames,
) -> Result<ComputedAllergens, Error> {
    let lines = pruned(&formula.lines)?;
    let mut entries: Vec<Entry<'_>> = Vec::new();
    for leaf in weights(&lines, compounds)? {
        let ingredient = ingredients
            .get(&leaf.id)
            .ok_or_else(|| Error::Corrupt(format!("ingredient {}", leaf.id.get())))?;
        let compound = leaf.compound.and_then(|id| compounds.get(&id));
        let into = match compound {
            Some(name) => {
                let index = add(&mut entries, name, true, leaf.weight, &ingredient.allergens);
                match entries[index].parts.as_mut() {
                    Some(parts) => parts,
                    None => return Err(Error::Corrupt("compound entry".to_owned())),
                }
            }
            None => &mut entries,
        };
        add(
            into,
            ingredient.label(),
            false,
            leaf.weight,
            &ingredient.allergens,
        );
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.weight));
    Ok(ComputedAllergens {
        allergens: entries
            .iter()
            .flat_map(|e| e.allergens.iter().copied())
            .collect(),
        ingredients: IngredientList(entries.into_iter().map(Entry::listed).collect()),
    })
}

/// [`compute`] with the ingredients and recipe settings as stored now.
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
    let compounds = compound_names(conn).await?;
    compute(formula, &book, &compounds)
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
        let none = CompoundNames::new();
        assert_eq!(exact_weights(&lines, &none), None);
        let fallback = weights(&lines, &none).unwrap();
        let mut ids: Vec<i64> = Vec::new();
        for leaf in &fallback {
            if !ids.contains(&leaf.id.get()) {
                ids.push(leaf.id.get());
            }
        }
        assert_eq!(ids, [1, 9, 2, 3, 4]);
        let total: u128 = fallback.iter().map(|leaf| leaf.weight).sum();
        assert_eq!(total, u128::from(REFERENCE_GRAMS));
        assert!(fallback.iter().all(|leaf| leaf.compound.is_none()));

        let filling = CompoundNames::from([(RecipeId::new(1), "vulling".to_owned())]);
        let compound = weights(&lines, &filling).unwrap();
        assert!(
            compound
                .iter()
                .all(|leaf| (leaf.id.get() == 4) == leaf.compound.is_none())
        );
        let weights = |leaves: &[Leaf]| leaves.iter().map(|l| l.weight).collect::<Vec<_>>();
        assert_eq!(weights(&compound), weights(&fallback));
    }

    #[test]
    fn exact_weights_share_one_denominator() {
        let lines = vec![leaf(1, 50_000), sub(vec![leaf(1, 1000), leaf(2, 2000)])];
        let exact = exact_weights(&lines, &CompoundNames::new()).unwrap();
        // 50/51 flour, then 1/51 × 1/3 flour and 1/51 × 2/3 water.
        let w: Vec<u128> = exact.iter().map(|leaf| leaf.weight).collect();
        assert_eq!((w[0], w[2]), (150 * w[1], 2 * w[1]));
        assert_eq!(gcd(12, 18), 6);
    }
}
