#![forbid(unsafe_code)]

use domain::{IngredientId, RecipeId, RecipeVersionId};
use proptest::prelude::*;
use recipes::{
    Formula, IngredientRef, IngredientRole, Line, Part, Pct, RecipeKind, ScaleError, Scaled,
    SubRecipe, ingredient_totals,
};

fn pct(percent: u32) -> Pct {
    Pct::from_thousandths(percent * 1000)
}

fn ingredient(id: i64, name: &str, role: IngredientRole, percent: u32) -> Line {
    Line {
        pct: pct(percent),
        part: Part::Ingredient(IngredientRef {
            id: IngredientId::new(id),
            name: name.to_owned(),
            role,
        }),
    }
}

const FLOUR: i64 = 1;
const WATER: i64 = 2;
const SALT: i64 = 3;
const SEEDS: i64 = 4;

fn flour(percent: u32) -> Line {
    ingredient(FLOUR, "Tarwebloem", IngredientRole::Flour, percent)
}

fn water(percent: u32) -> Line {
    ingredient(WATER, "Water", IngredientRole::Water, percent)
}

fn salt(percent: u32) -> Line {
    ingredient(SALT, "Zout", IngredientRole::Other, percent)
}

fn sub(version: i64, kind: RecipeKind, percent: u32, lines: Vec<Line>) -> Line {
    Line {
        pct: pct(percent),
        part: Part::Sub(SubRecipe {
            version_id: RecipeVersionId::new(version),
            recipe_id: RecipeId::new(version),
            name: format!("Recept {version}"),
            version: 1,
            kind,
            formula: Formula { lines },
        }),
    }
}

/// A 1:1 levain: flour 100%, water 100%.
fn levain(percent: u32) -> Line {
    sub(
        10,
        RecipeKind::Preferment,
        percent,
        vec![flour(100), water(100)],
    )
}

fn grams(scaled: &[Scaled<'_>]) -> Vec<u64> {
    scaled.iter().map(|line| line.grams).collect()
}

#[test]
fn straight_dough_scales_and_has_its_hydration() {
    let dough = Formula {
        lines: vec![flour(100), water(75), salt(2)],
    };
    let scaled = dough.scale(1770).unwrap();
    assert_eq!(grams(&scaled), vec![1000, 750, 20]);

    let analysis = dough.analysis().unwrap();
    assert_eq!(analysis.hydration, Some(pct(75)));
    assert_eq!(analysis.total_flour, pct(100));
    assert_eq!(analysis.prefermented_flour, Some(pct(0)));
}

#[test]
fn levain_flour_and_water_count_for_the_whole_dough() {
    // Levain at 20% of total flour holds 10% flour and 10% water, so the dough's own
    // 90% flour and 65% water make 100% flour and 75% hydration.
    let dough = Formula {
        lines: vec![flour(90), water(65), salt(2), levain(20)],
    };
    let scaled = dough.scale(1770).unwrap();
    assert_eq!(grams(&scaled), vec![900, 650, 20, 200]);
    assert_eq!(grams(&scaled[3].lines), vec![100, 100]);

    let analysis = dough.analysis().unwrap();
    assert_eq!(analysis.hydration, Some(pct(75)));
    assert_eq!(analysis.total_flour, pct(100));
    assert_eq!(analysis.prefermented_flour, Some(pct(10)));

    let totals: Vec<(i64, u64)> = ingredient_totals(&scaled)
        .into_iter()
        .map(|(ingredient, grams)| (ingredient.id.get(), grams))
        .collect();
    assert_eq!(totals, vec![(FLOUR, 1000), (WATER, 750), (SALT, 20)]);
}

#[test]
fn soaker_water_counts_but_its_seeds_are_not_flour_or_preferment() {
    let soaker = sub(
        11,
        RecipeKind::Soaker,
        30,
        vec![
            ingredient(SEEDS, "Lijnzaad", IngredientRole::Other, 100),
            water(100),
        ],
    );
    let dough = Formula {
        lines: vec![flour(100), water(70), salt(2), soaker],
    };
    let analysis = dough.analysis().unwrap();
    assert_eq!(analysis.hydration, Some(pct(85)));
    assert_eq!(analysis.total_flour, pct(100));
    assert_eq!(analysis.prefermented_flour, Some(pct(0)));
    assert_eq!(grams(&dough.scale(2020).unwrap()), vec![1000, 700, 20, 300]);
}

#[test]
fn a_preferment_inside_a_preferment_counts_once() {
    // A stiff starter (flour 100%, water 50%) at 30% of the levain's flour, inside a
    // levain at 20% of the dough's flour.
    let starter = sub(12, RecipeKind::Preferment, 30, vec![flour(100), water(50)]);
    let levain = sub(
        13,
        RecipeKind::Preferment,
        20,
        vec![flour(80), water(90), starter],
    );
    let dough = Formula {
        lines: vec![flour(90), water(66), levain],
    };
    let analysis = dough.analysis().unwrap();
    // Levain: 200% of its own flour weighs 20% of the dough's flour, so 10% is flour, of
    // which 2% through the starter, and 10% water.
    assert_eq!(analysis.total_flour, pct(100));
    assert_eq!(analysis.prefermented_flour, Some(pct(10)));
    assert_eq!(analysis.hydration, Some(pct(76)));
}

#[test]
fn inconsistent_percentages_show_in_total_flour() {
    let dough = Formula {
        lines: vec![flour(100), water(65), levain(20)],
    };
    assert_eq!(dough.analysis().unwrap().total_flour, pct(110));
}

#[test]
fn hydration_rounds_to_a_thousandth_of_a_percent() {
    let dough = Formula {
        lines: vec![flour(300), water(200)],
    };
    let analysis = dough.analysis().unwrap();
    assert_eq!(analysis.hydration, Some(Pct::from_thousandths(66_667)));
    assert_eq!(analysis.total_flour, pct(300));
}

#[test]
fn no_flour_means_no_hydration() {
    let filling = Formula {
        lines: vec![salt(10), water(5)],
    };
    let analysis = filling.analysis().unwrap();
    assert_eq!(analysis.hydration, None);
    assert_eq!(analysis.prefermented_flour, None);
    assert_eq!(analysis.total_flour, pct(0));
}

#[test]
fn empty_recipes_cannot_be_scaled() {
    let empty = Formula { lines: vec![] };
    assert_eq!(empty.scale(1000), Err(ScaleError::Empty(None)));
    assert_eq!(empty.analysis(), Err(ScaleError::Empty(None)));
    let dough = Formula {
        lines: vec![flour(100), sub(14, RecipeKind::Preferment, 20, vec![])],
    };
    assert_eq!(
        dough.scale(1000),
        Err(ScaleError::Empty(Some(RecipeVersionId::new(14))))
    );
}

#[test]
fn rounding_spreads_the_leftover_grams() {
    let dough = Formula {
        lines: vec![flour(100), water(100), salt(100)],
    };
    assert_eq!(grams(&dough.scale(1000).unwrap()), vec![334, 333, 333]);
    assert_eq!(grams(&dough.scale(0).unwrap()), vec![0, 0, 0]);
}

fn leaf() -> impl Strategy<Value = Line> {
    (1u32..=200_000, 0usize..3, 1i64..6).prop_map(|(thousandths, role, id)| Line {
        pct: Pct::from_thousandths(thousandths),
        part: Part::Ingredient(IngredientRef {
            id: IngredientId::new(id),
            name: format!("Ingrediënt {id}"),
            role: IngredientRole::ALL[role],
        }),
    })
}

fn formula() -> impl Strategy<Value = Formula> {
    let line = leaf().prop_recursive(3, 40, 5, |inner| {
        (
            1u32..=200_000,
            0usize..5,
            prop::collection::vec(inner, 1..5),
        )
            .prop_map(|(thousandths, kind, lines)| Line {
                pct: Pct::from_thousandths(thousandths),
                part: Part::Sub(SubRecipe {
                    version_id: RecipeVersionId::new(99),
                    recipe_id: RecipeId::new(99),
                    name: "Sub".to_owned(),
                    version: 1,
                    kind: RecipeKind::ALL[kind],
                    formula: Formula { lines },
                }),
            })
    });
    prop::collection::vec(line, 1..6).prop_map(|lines| Formula { lines })
}

/// Checks every line: children add up to their parent, and each leaf is within
/// depth + 1 grams of `target` × the product of p / S along its path, which is
/// `numerator / denominator`.
fn check(scaled: &[Scaled<'_>], target: u64, numerator: u128, denominator: u128, depth: u128) {
    let sum: u128 = scaled
        .iter()
        .map(|line| u128::from(line.line.pct.thousandths()))
        .sum();
    for line in scaled {
        let numerator = numerator * u128::from(line.line.pct.thousandths());
        let denominator = denominator * sum;
        match &line.line.part {
            Part::Ingredient(_) => {
                let got = u128::from(line.grams) * denominator;
                let exact = u128::from(target) * numerator;
                assert!(
                    got.abs_diff(exact) < (depth + 1) * denominator,
                    "{} g is too far from {target} × {numerator} / {denominator}",
                    line.grams
                );
            }
            Part::Sub(_) => {
                let children: u64 = line.lines.iter().map(|child| child.grams).sum();
                assert_eq!(children, line.grams);
                check(&line.lines, target, numerator, denominator, depth + 1);
            }
        }
    }
}

proptest! {
    #[test]
    fn scaled_grams_add_up_to_the_target(formula in formula(), target in 0u64..=200_000) {
        let scaled = formula.scale(target).unwrap();
        let total: u64 = scaled.iter().map(|line| line.grams).sum();
        prop_assert_eq!(total, target);
        let leaves: u64 = ingredient_totals(&scaled).iter().map(|(_, grams)| grams).sum();
        prop_assert_eq!(leaves, target);
    }

    #[test]
    fn sub_recipe_percentages_resolve_to_their_exact_share(
        formula in formula(),
        target in 0u64..=200_000,
    ) {
        let scaled = formula.scale(target).unwrap();
        check(&scaled, target, 1, 1, 0);
    }

    #[test]
    fn hydration_is_the_same_when_the_dough_is_used_as_a_sub_recipe(
        formula in formula(),
        thousandths in 1u32..=500_000,
        kind in 0usize..5,
    ) {
        let wrapped = Formula {
            lines: vec![Line {
                pct: Pct::from_thousandths(thousandths),
                part: Part::Sub(SubRecipe {
                    version_id: RecipeVersionId::new(98),
                    recipe_id: RecipeId::new(98),
                    name: "Buiten".to_owned(),
                    version: 1,
                    kind: RecipeKind::ALL[kind],
                    formula: formula.clone(),
                }),
            }],
        };
        let inner = formula.analysis().unwrap();
        let outer = wrapped.analysis().unwrap();
        let close = |a: Option<Pct>, b: Option<Pct>| match (a, b) {
            (Some(a), Some(b)) => a.thousandths().abs_diff(b.thousandths()) <= 1,
            (a, b) => a == b,
        };
        prop_assert!(close(inner.hydration, outer.hydration), "{inner:?} {outer:?}");
        if RecipeKind::ALL[kind] != RecipeKind::Preferment {
            prop_assert!(
                close(inner.prefermented_flour, outer.prefermented_flour),
                "{inner:?} {outer:?}"
            );
        }
    }
}

#[test]
fn grams_become_a_percentage_at_the_current_scale() {
    let dough = Formula {
        lines: vec![flour(100), water(75), salt(2)],
    };
    assert_eq!(dough.line_sum(), 177_000);
    assert_eq!(
        recipes::pct_for_grams(22, 1770, dough.line_sum()),
        Some(Pct::from_thousandths(2200))
    );
    assert_eq!(
        recipes::pct_for_grams(1, 3, 1),
        Some(Pct::from_thousandths(0))
    );
    assert_eq!(
        recipes::pct_for_grams(2, 3, 1),
        Some(Pct::from_thousandths(1))
    );
    assert_eq!(recipes::pct_for_grams(5, 0, 177_000), None);
    assert_eq!(recipes::pct_for_grams(5, 1770, 0), None);
    assert_eq!(recipes::dough_for_sum(1770, 177_000, 177_200), 1772);
    assert_eq!(recipes::dough_for_sum(1770, 0, 177_200), 1770);

    let scaled = dough.scale(1770).unwrap();
    let leaves = recipes::leaf_grams(&scaled);
    assert_eq!(
        (leaves.flour, leaves.water, leaves.prefermented_flour),
        (1000, 750, 0)
    );
    let with_levain = Formula {
        lines: vec![flour(90), water(65), levain(20)],
    };
    let leaves = recipes::leaf_grams(&with_levain.scale(1750).unwrap());
    assert_eq!(
        (leaves.flour, leaves.water, leaves.prefermented_flour),
        (1000, 750, 100)
    );
}

proptest! {
    /// Typing grams on a line stores a percentage; scaled back at the dough weight that
    /// keeps the other lines' grams, the line weighs what was typed, within half the
    /// grams of 0.001% (the percentage's rounding) plus 1.5 g (whole grams, ADR 0037).
    #[test]
    fn grams_to_percentage_to_grams_stays_within_the_rounding_bound(
        pcts in prop::collection::vec(1u32..=300_000, 1..12),
        index in 0usize..12,
        dough in 1u64..=1_000_000,
        typed in 0u64..=200_000,
    ) {
        let index = index % pcts.len();
        let lines: Vec<Line> = pcts
            .iter()
            .enumerate()
            .map(|(i, &p)| Line {
                pct: Pct::from_thousandths(p),
                part: Part::Ingredient(IngredientRef {
                    id: IngredientId::new(i64::try_from(i).unwrap()),
                    name: format!("Ingrediënt {i}"),
                    role: IngredientRole::Other,
                }),
            })
            .collect();
        let before = Formula { lines };
        let sum = before.line_sum();
        let pct = recipes::pct_for_grams(typed, dough, sum).unwrap();
        prop_assume!(pct.thousandths() > 0);
        let mut after = before.clone();
        after.lines[index].pct = pct;
        let new_sum = after.line_sum();
        let new_dough = recipes::dough_for_sum(dough, sum, new_sum);
        let scaled = after.scale(new_dough).unwrap();
        let got = scaled[index].grams;
        // |got - typed| <= dough / (2 sum) + 1.5, in integers.
        let lhs = 2 * u128::from(sum) * u128::from(got.abs_diff(typed));
        let rhs = u128::from(dough) + 3 * u128::from(sum);
        prop_assert!(lhs <= rhs, "typed {typed} g, got {got} g at {new_dough} g");
    }
}
