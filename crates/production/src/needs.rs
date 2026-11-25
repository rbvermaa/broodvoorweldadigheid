//! Ingredient needs per bake day as a shopping list (F15). See ADR 0041.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};

use bakedays::BakeDay;
use domain::{BakeDayId, ProductId, RecipeVersionId};
use i18n::{FluentArgs, Translator};
use recipes::{DoughGrams, Formula, Need, ProductRecipe};
use sqlx::PgConnection;

use crate::{Error, ProductTotal, paper, pdf};

/// What a bake day needs of each ingredient, with the doughs it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngredientNeeds {
    pub day: BakeDay,
    /// By name; each [`Need::parts`] index points into `doughs`.
    pub ingredients: Vec<Need>,
    pub doughs: Vec<Dough>,
    /// Products with something to bake that add nothing to `ingredients`, by name.
    pub missing: Vec<Missing>,
}

/// One dough: a recipe version made in one weight for one or more products.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dough {
    pub recipe: String,
    pub version: i32,
    pub products: Vec<DoughProduct>,
    pub grams: DoughGrams,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoughProduct {
    pub product_id: ProductId,
    pub name: String,
    pub pieces: u64,
    pub dough_g_per_piece: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub product_id: ProductId,
    pub name: String,
    pub pieces: u64,
    pub reason: MissingReason,
}

/// Why a product's pieces are not in the ingredient totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingReason {
    NoRecipe,
    /// The recipe has no version in use.
    NoVersion,
    /// The version in use, or one of its sub-recipes, has no lines.
    EmptyRecipe,
    /// The product's dough weight per piece is 0 g.
    NoDoughWeight,
}

impl MissingReason {
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::NoRecipe => "needs-reason-no-recipe",
            Self::NoVersion => "needs-reason-no-version",
            Self::EmptyRecipe => "needs-reason-empty",
            Self::NoDoughWeight => "needs-reason-no-dough-weight",
        }
    }
}

pub(crate) struct Plan {
    pub doughs: Vec<Dough>,
    pub missing: Vec<Missing>,
}

/// Groups the products by the recipe version they use and scales each version once, to
/// the sum of pieces × dough weight of its products.
pub(crate) fn plan(
    products: &[ProductTotal],
    recipes: &[ProductRecipe],
    formulas: &HashMap<RecipeVersionId, Formula>,
) -> Plan {
    let recipes: HashMap<ProductId, &ProductRecipe> =
        recipes.iter().map(|r| (r.product_id, r)).collect();
    let mut groups: BTreeMap<RecipeVersionId, (&str, i32, Vec<DoughProduct>)> = BTreeMap::new();
    let mut missing = Vec::new();
    for product in products.iter().filter(|p| p.to_bake() > 0) {
        let lacking = |reason| Missing {
            product_id: product.product_id,
            name: product.name.clone(),
            pieces: product.to_bake(),
            reason,
        };
        let Some((info, recipe)) = recipes
            .get(&product.product_id)
            .and_then(|info| Some((info, info.recipe.as_ref()?)))
        else {
            missing.push(lacking(MissingReason::NoRecipe));
            continue;
        };
        let Some((version_id, version)) = recipe.version else {
            missing.push(lacking(MissingReason::NoVersion));
            continue;
        };
        if info.dough_g_per_piece == 0 {
            missing.push(lacking(MissingReason::NoDoughWeight));
            continue;
        }
        groups
            .entry(version_id)
            .or_insert_with(|| (recipe.name.as_str(), version, Vec::new()))
            .2
            .push(DoughProduct {
                product_id: product.product_id,
                name: product.name.clone(),
                pieces: product.to_bake(),
                dough_g_per_piece: info.dough_g_per_piece,
            });
    }
    let mut doughs = Vec::with_capacity(groups.len());
    for (version_id, (recipe, version, products)) in groups {
        let target = products
            .iter()
            .map(|p| p.pieces * u64::from(p.dough_g_per_piece))
            .sum();
        match formulas
            .get(&version_id)
            .and_then(|formula| DoughGrams::from_formula(formula, target).ok())
        {
            Some(grams) => doughs.push(Dough {
                recipe: recipe.to_owned(),
                version,
                products,
                grams,
            }),
            None => missing.extend(products.into_iter().map(|p| Missing {
                product_id: p.product_id,
                name: p.name,
                pieces: p.pieces,
                reason: MissingReason::EmptyRecipe,
            })),
        }
    }
    doughs.sort_by(|a, b| {
        a.recipe
            .to_lowercase()
            .cmp(&b.recipe.to_lowercase())
            .then(a.version.cmp(&b.version))
    });
    missing.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.product_id.cmp(&b.product_id))
    });
    Plan { doughs, missing }
}

/// The doughs a day's needs are summed from: today the recipe version each product uses,
/// scaled to its pieces. This is the seam for batches (F14a): after cutoff it is to
/// return one [`Dough`] per batch, with [`DoughGrams`] from the batch lines (ADR 0041).
async fn doughs(conn: &mut PgConnection, products: &[ProductTotal]) -> Result<Plan, Error> {
    let ids: Vec<ProductId> = products.iter().map(|p| p.product_id).collect();
    let recipes = recipes::needs::product_recipes(&mut *conn, &ids).await?;
    let mut formulas = HashMap::new();
    for recipe in &recipes {
        let Some((version_id, _)) = recipe.recipe.as_ref().and_then(|r| r.version) else {
            continue;
        };
        if let Entry::Vacant(slot) = formulas.entry(version_id) {
            slot.insert(recipes::recipes::formula(&mut *conn, version_id).await?);
        }
    }
    Ok(plan(products, &recipes, &formulas))
}

/// The ingredient needs of a bake day: orders (paid, confirmed, picked up) plus extra
/// production, as on the production list. All reads see one snapshot of the database,
/// so call it outside a transaction.
///
/// # Errors
/// [`Error::BakeDays`] with `NotFound` for an unknown day, [`Error::Recipes`], or a
/// database error.
pub async fn ingredient_needs(
    conn: &mut PgConnection,
    day: BakeDayId,
) -> Result<IngredientNeeds, Error> {
    let mut tx = crate::snapshot(conn).await?;
    let list = crate::production::totals(&mut tx, day).await?;
    let Plan { doughs, missing } = doughs(&mut tx, &list.products).await?;
    tx.commit().await?;
    let ingredients = recipes::shopping_list(doughs.iter().map(|d| &d.grams));
    Ok(IngredientNeeds {
        day: list.day,
        ingredients,
        doughs,
        missing,
    })
}

/// `850 g` below a kilogram, `12,345 kg` from there: always exact to the gram.
///
/// # Errors
/// [`Error::I18n`].
pub fn weight(t: &Translator, grams: u64) -> Result<String, Error> {
    let scaled = i64::try_from(grams).unwrap_or(i64::MAX);
    let (id, amount) = if grams < 1000 {
        ("needs-grams", i18n::decimal(scaled, 0))
    } else {
        ("needs-kilograms", i18n::decimal(scaled, 3))
    };
    let mut args = FluentArgs::new();
    args.set("amount", amount);
    Ok(t.t(id, Some(&args))?)
}

/// The ingredient needs as an A4 PDF.
///
/// # Errors
/// [`Error::I18n`] or [`Error::Pdf`].
pub fn ingredient_needs_pdf(needs: &IngredientNeeds, t: &Translator) -> Result<Vec<u8>, Error> {
    let data = paper::needs(needs, t)?;
    pdf::render(
        &pdf::NEEDS,
        &data,
        format!("ingredients-{}", needs.day.date),
    )
}

/// The shopping list as CSV for Dutch Excel: semicolons, whole grams, UTF-8 with a byte
/// order mark (ADR 0017). Products left out follow the ingredients, one row each.
///
/// # Errors
/// [`Error::I18n`] or [`Error::Csv`].
pub fn ingredient_needs_csv(needs: &IngredientNeeds, t: &Translator) -> Result<String, Error> {
    let csv_error = |e: csv::Error| Error::Csv(e.to_string());
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b';')
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    writer
        .write_record([
            t.t("needs-ingredient", None)?,
            t.t("needs-csv-grams", None)?,
        ])
        .map_err(csv_error)?;
    for need in &needs.ingredients {
        writer
            .write_record([need.ingredient.name.clone(), need.grams.to_string()])
            .map_err(csv_error)?;
    }
    for missing in &needs.missing {
        let mut args = FluentArgs::new();
        args.set("product", missing.name.as_str());
        args.set("pieces", missing.pieces.to_string());
        args.set("reason", t.t(missing.reason.message_id(), None)?);
        writer
            .write_record([t.t("needs-csv-missing", Some(&args))?, String::new()])
            .map_err(csv_error)?;
    }
    let bytes = writer.into_inner().map_err(|e| Error::Csv(e.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|e| Error::Csv(e.to_string()))?;
    Ok(format!("\u{feff}{text}"))
}

#[cfg(test)]
mod tests {
    use domain::{IngredientId, RecipeId};
    use proptest::prelude::*;
    use recipes::{IngredientRef, IngredientRole, Line, Part, Pct, RecipeInUse};

    use super::*;
    use crate::Extras;

    fn formula(lines: &[(i64, u32)]) -> Formula {
        Formula {
            lines: lines
                .iter()
                .map(|&(id, thousandths)| Line {
                    pct: Pct::from_thousandths(thousandths),
                    part: Part::Ingredient(IngredientRef {
                        id: IngredientId::new(id),
                        name: format!("Ingrediënt {id}"),
                        role: IngredientRole::Other,
                    }),
                })
                .collect(),
        }
    }

    /// A product: pieces ordered and extra, its dough weight, and which of three
    /// versions it uses (3 means no version, 4 no recipe). Version 2 has no lines.
    fn any_product() -> impl Strategy<Value = (u32, u32, u32, usize)> {
        (0u32..500, 0u32..50, 0u32..2_000, 0usize..5)
    }

    proptest! {
        #[test]
        fn every_piece_is_either_in_the_grams_or_listed_as_missing(
            products in prop::collection::vec(any_product(), 0..12),
            lines in prop::collection::vec((1i64..6, 1u32..200_000), 1..6),
        ) {
            let formulas: HashMap<RecipeVersionId, Formula> = [
                (RecipeVersionId::new(0), formula(&lines)),
                (RecipeVersionId::new(1), formula(&[(1, 100_000), (2, 70_000)])),
                (RecipeVersionId::new(2), formula(&[])),
            ]
            .into_iter()
            .collect();
            let totals: Vec<ProductTotal> = products
                .iter()
                .enumerate()
                .map(|(i, &(ordered, extra, _, _))| ProductTotal {
                    product_id: ProductId::new(i64::try_from(i).unwrap()),
                    name: format!("Product {i}"),
                    ordered: u64::from(ordered),
                    extras: Extras { freezer: u64::from(extra), ..Extras::default() },
                })
                .collect();
            let recipes: Vec<ProductRecipe> = products
                .iter()
                .enumerate()
                .map(|(i, &(_, _, dough, which))| ProductRecipe {
                    product_id: ProductId::new(i64::try_from(i).unwrap()),
                    dough_g_per_piece: dough,
                    recipe: (which < 4).then(|| RecipeInUse {
                        recipe_id: RecipeId::new(i64::try_from(which).unwrap()),
                        name: format!("Recept {which}"),
                        version: (which < 3).then(|| {
                            (RecipeVersionId::new(i64::try_from(which).unwrap()), 1)
                        }),
                    }),
                })
                .collect();

            let plan = plan(&totals, &recipes, &formulas);
            let list = recipes::shopping_list(plan.doughs.iter().map(|d| &d.grams));

            let mut expected = 0;
            for (total, recipe) in totals.iter().zip(&recipes) {
                let pieces = total.to_bake();
                let in_doughs = plan
                    .doughs
                    .iter()
                    .flat_map(|d| &d.products)
                    .filter(|p| p.product_id == total.product_id)
                    .count();
                let listed = plan.missing.iter().filter(|m| m.product_id == total.product_id).count();
                if pieces == 0 {
                    prop_assert_eq!(in_doughs + listed, 0);
                    continue;
                }
                prop_assert_eq!(in_doughs + listed, 1);
                if in_doughs == 1 {
                    expected += pieces * u64::from(recipe.dough_g_per_piece);
                }
            }
            prop_assert_eq!(list.iter().map(|n| n.grams).sum::<u64>(), expected);
            for dough in &plan.doughs {
                let target: u64 = dough
                    .products
                    .iter()
                    .map(|p| p.pieces * u64::from(p.dough_g_per_piece))
                    .sum();
                prop_assert_eq!(dough.grams.total(), target);
            }
            for need in &list {
                prop_assert_eq!(need.parts.iter().map(|(_, g)| g).sum::<u64>(), need.grams);
            }
        }
    }

    #[test]
    fn weights_switch_to_kilograms_at_a_kilogram() {
        let t = Translator::dutch().unwrap();
        assert_eq!(weight(&t, 0).unwrap(), "0 g");
        assert_eq!(weight(&t, 999).unwrap(), "999 g");
        assert_eq!(weight(&t, 1000).unwrap(), "1,000 kg");
        assert_eq!(weight(&t, 1_234_567).unwrap(), "1.234,567 kg");
    }
}
