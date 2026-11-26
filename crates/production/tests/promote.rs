//! Promoting a batch (F14b): the new version weighs out the batch's grams.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use domain::{IngredientId, RecipeId, RecipeVersionId};
use production::batches::{promoted_formula, version_lines};
use proptest::prelude::*;
use recipes::{
    Formula, IngredientRef, IngredientRole, Line, LinePart, Part, Pct, RecipeKind, SubRecipe,
    ingredient_totals,
};

fn leaf(min: u32) -> impl Strategy<Value = Line> {
    (min..=200_000u32, 0usize..3, 1i64..6).prop_map(|(thousandths, role, id)| Line {
        pct: Pct::from_thousandths(thousandths),
        part: Part::Ingredient(IngredientRef {
            id: IngredientId::new(id),
            name: format!("Ingrediënt {id}"),
            role: IngredientRole::ALL[role],
            hydration: None,
        }),
    })
}

fn sub(min: u32) -> impl Strategy<Value = Line> {
    let inner = leaf(1).prop_recursive(2, 20, 4, |inner| {
        (1u32..=200_000, prop::collection::vec(inner, 1..4)).prop_map(|(thousandths, lines)| Line {
            pct: Pct::from_thousandths(thousandths),
            part: Part::Sub(SubRecipe {
                version_id: RecipeVersionId::new(98),
                recipe_id: RecipeId::new(98),
                name: "Binnen".to_owned(),
                version: 1,
                kind: RecipeKind::Preferment,
                formula: Formula { lines },
            }),
        })
    });
    (min..=200_000u32, prop::collection::vec(inner, 1..4)).prop_map(|(thousandths, lines)| Line {
        pct: Pct::from_thousandths(thousandths),
        part: Part::Sub(SubRecipe {
            version_id: RecipeVersionId::new(99),
            recipe_id: RecipeId::new(99),
            name: "Desem".to_owned(),
            version: 1,
            kind: RecipeKind::Preferment,
            formula: Formula { lines },
        }),
    })
}

/// A batch formula: lines the baker may have set to 0%, at least one above 0%.
fn batch_formula() -> impl Strategy<Value = Formula> {
    let line = prop_oneof![3 => leaf(0), 1 => sub(0)];
    (
        prop_oneof![leaf(1), sub(1)],
        prop::collection::vec(line, 0..8),
        any::<prop::sample::Index>(),
    )
        .prop_map(|(kept, mut lines, at)| {
            let at = at.index(lines.len() + 1);
            lines.insert(at, kept);
            Formula { lines }
        })
}

fn grams(formula: &Formula, dough_g: u64) -> BTreeMap<IngredientId, u64> {
    let scaled = formula.scale(dough_g).unwrap();
    let mut totals = BTreeMap::new();
    for (ingredient, grams) in ingredient_totals(&scaled) {
        *totals.entry(ingredient.id).or_default() += grams;
    }
    totals.retain(|_, grams| *grams > 0);
    totals
}

proptest! {
    /// ADR 0037 allows d + 1 grams per ingredient; a promotion copies the percentages, so
    /// the new version must give exactly the batch's grams.
    #[test]
    fn a_promoted_version_scales_back_to_the_batch_grams(
        formula in batch_formula(),
        dough_g in 0u64..=500_000,
    ) {
        let promoted = promoted_formula(&formula);
        prop_assert_eq!(grams(&promoted, dough_g), grams(&formula, dough_g));
    }

    #[test]
    fn a_promoted_version_keeps_every_line_above_zero_in_order(formula in batch_formula()) {
        let lines = version_lines(&promoted_formula(&formula));
        let kept: Vec<&Line> = formula.lines.iter().filter(|l| l.pct.thousandths() > 0).collect();
        prop_assert_eq!(lines.len(), kept.len());
        for (stored, line) in lines.iter().zip(kept) {
            prop_assert_eq!(stored.pct, line.pct);
            let same_part = match (&stored.part, &line.part) {
                (LinePart::Ingredient(id), Part::Ingredient(i)) => *id == i.id,
                (LinePart::Sub(id), Part::Sub(s)) => *id == s.version_id,
                _ => false,
            };
            prop_assert!(same_part);
        }
    }
}
