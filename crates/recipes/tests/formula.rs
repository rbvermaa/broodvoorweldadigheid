#![forbid(unsafe_code)]

use domain::{IngredientId, RecipeId, RecipeVersionId};
use proptest::prelude::*;
use recipes::{
    Base, FlourWater, Formula, IngredientRef, IngredientRole, Line, Part, Pct, RecipeKind,
    ScaleError, Scaled, SubRecipe, ingredient_totals, leaf_grams,
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
            hydration: None,
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
    (1u32..=200_000, 0usize..4, 1i64..6, 0u32..=500_000).prop_map(
        |(thousandths, role, id, hydration)| {
            let role = IngredientRole::ALL[role];
            Line {
                pct: Pct::from_thousandths(thousandths),
                part: Part::Ingredient(IngredientRef {
                    id: IngredientId::new(id),
                    name: format!("Ingrediënt {id}"),
                    role,
                    hydration: (role == IngredientRole::Starter)
                        .then_some(Pct::from_thousandths(hydration)),
                }),
            }
        },
    )
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
                    hydration: None,
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

const STARTER: i64 = 5;

fn starter_ref(hydration: Option<u32>) -> IngredientRef {
    IngredientRef {
        id: IngredientId::new(STARTER),
        name: "Desem".to_owned(),
        role: IngredientRole::Starter,
        hydration: hydration.map(Pct::from_thousandths),
    }
}

fn starter(percent: u32, hydration: u32) -> Line {
    Line {
        pct: pct(percent),
        part: Part::Ingredient(starter_ref(Some(hydration * 1000))),
    }
}

#[test]
fn a_starter_splits_into_flour_and_water_by_its_hydration() {
    let split = |hydration, grams| starter_ref(hydration).flour_and_water(grams);
    assert_eq!(
        split(Some(100_000), 180),
        FlourWater {
            flour: 90,
            water: 90
        }
    );
    assert_eq!(
        split(Some(50_000), 150),
        FlourWater {
            flour: 100,
            water: 50
        }
    );
    // 100 g at 200%: 33.33 g flour and 66.67 g water.
    assert_eq!(
        split(Some(200_000), 100),
        FlourWater {
            flour: 33,
            water: 67
        }
    );
    assert_eq!(
        split(Some(0), 100),
        FlourWater {
            flour: 100,
            water: 0
        }
    );
    assert_eq!(split(None, 100), FlourWater::default());
}

#[test]
fn starter_flour_and_water_count_in_total_flour_and_hydration() {
    // Landbrood with 20% of a 100% starter that is not yet counted in the flour: 110% flour.
    let dough = Formula {
        lines: vec![flour(100), water(65), salt(2), starter(20, 100)],
    };
    let analysis = dough.analysis().unwrap();
    assert_eq!(analysis.total_flour, pct(110));
    assert_eq!(analysis.hydration, Some(Pct::from_thousandths(68_182)));
    assert_eq!(
        analysis.prefermented_flour,
        Some(Pct::from_thousandths(9_091))
    );

    // Written against all flour, a 1:1 starter at 20% is the levain at 20%.
    let dough = Formula {
        lines: vec![flour(90), water(65), salt(2), starter(20, 100)],
    };
    let levain_dough = Formula {
        lines: vec![flour(90), water(65), salt(2), levain(20)],
    };
    assert_eq!(dough.analysis(), levain_dough.analysis());
    let analysis = dough.analysis().unwrap();
    assert_eq!(analysis.total_flour, pct(100));
    assert_eq!(analysis.hydration, Some(pct(75)));
    let scaled = dough.scale(1770).unwrap();
    assert_eq!(grams(&scaled), vec![900, 650, 20, 200]);
    let leaves = leaf_grams(&scaled);
    assert_eq!(
        (leaves.flour, leaves.water, leaves.starter),
        (
            1000,
            750,
            FlourWater {
                flour: 100,
                water: 100
            }
        )
    );
    // Priced and listed as itself: the starter stays one ingredient.
    assert_eq!(
        ingredient_totals(&scaled)
            .into_iter()
            .map(|(i, g)| (i.id.get(), g))
            .collect::<Vec<_>>(),
        vec![(FLOUR, 900), (WATER, 650), (SALT, 20), (STARTER, 200)]
    );
}

#[test]
fn a_starter_without_hydration_counts_as_neither() {
    let dough = Formula {
        lines: vec![
            flour(100),
            water(70),
            Line {
                pct: pct(20),
                part: Part::Ingredient(starter_ref(None)),
            },
        ],
    };
    let analysis = dough.analysis().unwrap();
    assert_eq!(analysis.total_flour, pct(100));
    assert_eq!(analysis.hydration, Some(pct(70)));
}

proptest! {
    #[test]
    fn a_starter_split_adds_up_and_is_within_a_gram(
        grams in 0u64..=10_000_000,
        hydration in 0u32..=500_000,
    ) {
        let split = starter_ref(Some(hydration)).flour_and_water(grams);
        prop_assert_eq!(split.flour + split.water, grams);
        let exact_numerator = u128::from(grams) * 100_000;
        let denominator = 100_000 + u128::from(hydration);
        let flour = u128::from(split.flour) * denominator;
        prop_assert!(flour.abs_diff(exact_numerator) < denominator);
    }

    #[test]
    fn a_starter_counts_like_a_preferment_of_flour_and_water(
        own_flour in 1u32..=100,
        own_water in 0u32..=100,
        starter_pct in 1u32..=100,
        hydration in 0u32..=500_000,
    ) {
        let with_starter = Formula {
            lines: vec![
                flour(own_flour),
                water(own_water),
                Line {
                    pct: pct(starter_pct),
                    part: Part::Ingredient(starter_ref(Some(hydration))),
                },
            ],
        };
        let with_levain = Formula {
            lines: vec![
                flour(own_flour),
                water(own_water),
                sub(
                    10,
                    RecipeKind::Preferment,
                    starter_pct,
                    vec![
                        flour(100),
                        Line {
                            pct: Pct::from_thousandths(hydration),
                            part: Part::Ingredient(IngredientRef {
                                id: IngredientId::new(WATER),
                                name: "Water".to_owned(),
                                role: IngredientRole::Water,
                                hydration: None,
                            }),
                        },
                    ],
                ),
            ],
        };
        prop_assert_eq!(with_starter.analysis(), with_levain.analysis());
    }


    #[test]
    fn rebasing_keeps_total_flour_within_rounding(
        own_water in 0u32..=100,
        starter_pct in 1u32..=200,
        before_hydration in 0u32..=500,
        after_hydration in 0u32..=500,
    ) {
        prop_assume!(before_hydration != after_hydration);
        let lines = |hydration: u32| vec![water(own_water), salt(2), starter(starter_pct, hydration)];
        let before = Formula { lines: lines(before_hydration) };
        let after = Formula { lines: lines(after_hydration) };
        let pcts = before.rebased(&after).unwrap();
        let rebased = with_pcts(&after, &pcts);
        let old = before.analysis().unwrap().total_flour.thousandths();
        let new = rebased.analysis().unwrap().total_flour.thousandths();
        prop_assert!(old.abs_diff(new) <= 2, "{old} vs {new}");
    }
}

fn with_pcts(formula: &Formula, pcts: &[Pct]) -> Formula {
    Formula {
        lines: formula
            .lines
            .iter()
            .zip(pcts)
            .map(|(line, pct)| Line {
                pct: *pct,
                part: line.part.clone(),
            })
            .collect(),
    }
}

#[test]
fn a_starter_does_not_rebase_a_recipe_with_its_own_flour() {
    // The base is the dough's own flour, which a starter's role or hydration leaves alone.
    let other = Formula {
        lines: vec![
            flour(100),
            water(70),
            salt(2),
            ingredient(STARTER, "Desem", IngredientRole::Other, 20),
        ],
    };
    let as_starter = Formula {
        lines: vec![flour(100), water(70), salt(2), starter(20, 100)],
    };
    assert_eq!(other.rebased(&as_starter), None);
    let stiffer = Formula {
        lines: vec![flour(100), water(70), salt(2), starter(20, 50)],
    };
    assert_eq!(as_starter.rebased(&stiffer), None);
}

#[test]
fn rebasing_keeps_the_grams_and_the_total_flour_without_own_flour() {
    // A dough made from a starter only: its base is all its flour (ADR 0053).
    let before = Formula {
        lines: vec![starter(200, 100), water(50), salt(2)],
    };
    let after = Formula {
        lines: vec![starter(200, 50), water(50), salt(2)],
    };
    assert_eq!(before.analysis().unwrap().base, Base::AllFlour);
    let pcts = before.rebased(&after).unwrap();
    let thousandths: Vec<u32> = pcts.iter().map(|p| p.thousandths()).collect();
    assert_eq!(thousandths, vec![150_000, 37_500, 1_500]);
    let rebased = with_pcts(&after, &pcts);
    let analysis = rebased.analysis().unwrap();
    assert_eq!(analysis.base_flour(), Some(pct(100)));
    assert_eq!(
        grams(&rebased.scale(2520).unwrap()),
        grams(&before.scale(2520).unwrap())
    );

    assert_eq!(after.rebased(&after), None);
    let no_flour = Formula {
        lines: vec![water(100)],
    };
    assert_eq!(no_flour.rebased(&no_flour), None);
}

#[test]
fn rebasing_never_drops_a_line_to_zero() {
    let lines = |hydration| {
        vec![
            Line {
                pct: Pct::from_thousandths(1),
                part: water(1).part,
            },
            starter(500, hydration),
        ]
    };
    let before = Formula { lines: lines(500) };
    let after = Formula { lines: lines(0) };
    let thousandths: Vec<u32> = before
        .rebased(&after)
        .unwrap()
        .iter()
        .map(|p| p.thousandths())
        .collect();
    assert_eq!(thousandths, vec![1, 83_333]);
}

fn named_flour(id: i64, name: &str, percent: u32) -> Line {
    ingredient(id, name, IngredientRole::Flour, percent)
}

/// The owner's Landbrood: own flours 70/15/15, water 70%, salt 2% and 20% of a starter
/// at 50% hydration.
fn landbrood() -> Formula {
    Formula {
        lines: vec![
            named_flour(FLOUR, "Tarwebloem", 70),
            named_flour(6, "Speltmeel", 15),
            named_flour(7, "Roggemeel", 15),
            water(70),
            salt(2),
            starter(20, 50),
        ],
    }
}

#[test]
fn percentages_are_relative_to_the_main_dough_flour() {
    let dough = landbrood();
    let analysis = dough.analysis().unwrap();
    assert_eq!(analysis.base, Base::MainFlour);
    assert_eq!(analysis.main_flour, pct(100));
    assert_eq!(analysis.base_flour(), Some(pct(100)));
    // The starter's 20% holds 13.333% flour and 6.667% water.
    assert_eq!(analysis.total_flour, Pct::from_thousandths(113_333));
    assert_eq!(analysis.hydration, Some(Pct::from_thousandths(67_647)));
    assert_eq!(
        analysis.prefermented_flour,
        Some(Pct::from_thousandths(11_765))
    );
    assert_eq!(dough.main_flour(), 100_000);
    let scaled = dough.scale(1920).unwrap();
    assert_eq!(grams(&scaled), vec![700, 150, 150, 700, 20, 200]);
    let leaves = leaf_grams(&scaled);
    assert_eq!((leaves.flour, leaves.water), (1133, 767));
}

#[test]
fn main_flour_off_shows_in_the_base() {
    let dough = Formula {
        lines: vec![flour(90), water(65), levain(20)],
    };
    let analysis = dough.analysis().unwrap();
    assert_eq!(analysis.base_flour(), Some(pct(90)));
    assert_eq!(analysis.total_flour, pct(100));
}

#[test]
fn without_own_flour_the_base_is_all_flour_and_without_flour_the_weight() {
    let from_levain = Formula {
        lines: vec![levain(200), water(50), salt(2)],
    };
    let analysis = from_levain.analysis().unwrap();
    assert_eq!(analysis.base, Base::AllFlour);
    assert_eq!(analysis.main_flour, Pct::default());
    assert_eq!(analysis.base_flour(), Some(pct(100)));

    let filling = Formula {
        lines: vec![salt(60), water(40)],
    };
    let analysis = filling.analysis().unwrap();
    assert_eq!(analysis.base, Base::Weight);
    assert_eq!(analysis.base_flour(), None);
}

#[test]
fn percentages_show_with_one_decimal_rounded_half_up() {
    let tenths = |t| Pct::from_thousandths(t).tenths();
    assert_eq!(tenths(113_333), 1133);
    assert_eq!(tenths(79_050), 791);
    assert_eq!(tenths(2_249), 22);
    assert_eq!(tenths(2_250), 23);
    assert_eq!(tenths(100_000), 1000);
    assert_eq!(tenths(49), 0);
    assert_eq!(tenths(50), 1);
}

/// What the main-dough migration (ADR 0053) does to a version's own lines: each
/// percentage times total flour ÷ main flour, rounded half up to 0.001%.
fn migrated(formula: &Formula) -> Option<Formula> {
    let main = u128::from(formula.main_flour());
    let total = u128::from(formula.analysis().ok()?.total_flour.thousandths());
    if main == 0 || main == total {
        return None;
    }
    let pcts: Vec<Pct> = formula
        .lines
        .iter()
        .map(|line| {
            let value = (u128::from(line.pct.thousandths()) * total * 2 + main) / (main * 2);
            Pct::from_thousandths(u32::try_from(value).unwrap())
        })
        .collect();
    Some(with_pcts(formula, &pcts))
}

/// Every leaf's grams with its depth, in order.
fn leaves(scaled: &[Scaled<'_>], depth: u64, out: &mut Vec<(u64, u64)>) {
    for line in scaled {
        match &line.line.part {
            Part::Ingredient(_) => out.push((line.grams, depth)),
            Part::Sub(_) => leaves(&line.lines, depth + 1, out),
        }
    }
}

proptest! {
    #[test]
    fn one_decimal_is_within_half_a_tenth(thousandths in 0u32..=1_000_000) {
        let tenths = Pct::from_thousandths(thousandths).tenths();
        let diff = i64::from(tenths) * 100 - i64::from(thousandths);
        prop_assert!((-49..=50).contains(&diff), "{thousandths} -> {tenths}");
    }

    #[test]
    fn migrating_to_the_main_dough_base_keeps_the_grams(
        formula in formula(),
        target in 0u64..=200_000,
    ) {
        let Some(after) = migrated(&formula) else {
            return Ok(());
        };
        prop_assume!(after.lines.iter().all(|l| l.pct.thousandths() <= 1_000_000));
        let analysis = after.analysis().unwrap();
        let old_total = formula.analysis().unwrap().total_flour.thousandths();
        // The new main flour is the old total flour, give or take half a thousandth a line.
        let lines = u32::try_from(after.lines.len()).unwrap();
        prop_assert!(analysis.main_flour.thousandths().abs_diff(old_total) <= lines);
        let (mut old, mut new) = (Vec::new(), Vec::new());
        leaves(&formula.scale(target).unwrap(), 0, &mut old);
        leaves(&after.scale(target).unwrap(), 0, &mut new);
        // Each leaf's exact share moves by less than target · (n + 1) / (2 · S'), from
        // rounding the n percentages; whole grams add less than depth + 1 on each side.
        let sum = u128::from(after.line_sum());
        let n = u128::from(lines);
        for ((before, depth), (got, _)) in old.iter().zip(&new) {
            let diff = u128::from(before.abs_diff(*got));
            let slack = 2 * (u128::from(*depth) + 1);
            prop_assert!(
                2 * sum * diff.saturating_sub(slack) <= u128::from(target) * (n + 1),
                "{before} g became {got} g at {target} g"
            );
        }
    }
}
