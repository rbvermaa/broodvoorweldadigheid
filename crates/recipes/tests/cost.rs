#![forbid(unsafe_code)]
//! Costing (F16): dough cost, standard cost per piece and the batch variance.

use domain::{IngredientId, Money, RecipeId, RecipeVersionId};
use proptest::prelude::*;
use recipes::cost::{
    CostError, DoughCost, MilliCents, PieceCost, Prices, UnitPrice, Variance, VarianceInput,
    round_div,
};
use recipes::{
    DoughGrams, Formula, IngredientRef, IngredientRole, Line, Part, Pct, RecipeKind, ScaleError,
    SubRecipe,
};

fn pct(percent: u32) -> Pct {
    Pct::from_thousandths(percent * 1000)
}

fn leaf(id: i64, thousandths: u32) -> Line {
    Line {
        pct: Pct::from_thousandths(thousandths),
        part: Part::Ingredient(IngredientRef {
            id: IngredientId::new(id),
            name: format!("ingredient {id}"),
            role: IngredientRole::Other,
        }),
    }
}

fn levain(thousandths: u32) -> Line {
    Line {
        pct: Pct::from_thousandths(thousandths),
        part: Part::Sub(SubRecipe {
            version_id: RecipeVersionId::new(10),
            recipe_id: RecipeId::new(10),
            name: "Levain".to_owned(),
            version: 1,
            kind: RecipeKind::Preferment,
            formula: Formula {
                lines: vec![leaf(4, 100_000), leaf(2, 100_000)],
            },
        }),
    }
}

/// Flour 100%, water 75%, salt 2%.
fn bread() -> Formula {
    Formula {
        lines: vec![leaf(1, 100_000), leaf(2, 75_000), leaf(3, 2_000)],
    }
}

fn prices(list: &[(i64, i64)]) -> Prices {
    list.iter()
        .map(|&(id, cents)| {
            (
                IngredientId::new(id),
                UnitPrice::per_kg(Money::from_cents(cents)),
            )
        })
        .collect()
}

fn cents(c: i64) -> Money {
    Money::from_cents(c)
}

#[test]
fn a_dough_costs_its_grams_times_the_price_per_kg() {
    let dough = DoughGrams::from_formula(&bread(), 17_700).unwrap();
    let cost = DoughCost::new(&dough, &prices(&[(1, 95), (2, 0), (3, 120)]));
    // 10 kg flour at €0,95 + 7,5 kg water free + 200 g salt at €1,20.
    assert_eq!(cost.total().unwrap().get(), 950_000 + 24_000);
    assert_eq!(cost.total().unwrap().cents(), cents(974));
    assert_eq!(cost.missing().count(), 0);
    assert_eq!(cost.lines[1].cost, MilliCents::ZERO);
}

#[test]
fn a_missing_price_leaves_the_total_unknown() {
    let dough = DoughGrams::from_formula(&bread(), 1770).unwrap();
    let cost = DoughCost::new(&dough, &prices(&[(1, 95), (2, 0)]));
    assert_eq!(cost.total(), None);
    let missing: Vec<i64> = cost.missing().map(|i| i.id.get()).collect();
    assert_eq!(missing, [3]);
    assert_eq!(cost.lines[2].price, None);
}

#[test]
fn the_standard_cost_per_piece_comes_from_a_thousand_pieces() {
    let formula = Formula {
        lines: vec![
            leaf(1, 90_000),
            leaf(2, 65_000),
            leaf(3, 2_000),
            levain(20_000),
        ],
    };
    let all = prices(&[(1, 95), (2, 0), (3, 120), (4, 180)]);
    let piece = PieceCost::new(&formula, 900, &all).unwrap();
    assert_eq!(piece.dough_g_per_piece, 900);
    let grams: u64 = piece.dough.lines.iter().map(|l| l.grams).sum();
    assert_eq!(grams, 900_000);
    // Per piece 457,6 g flour, 10,2 g salt and 50,8 g rye: 43,47 + 1,22 + 9,15 cents.
    assert_eq!(piece.per_piece(), Some(cents(54)));
    let without_rye = PieceCost::new(&formula, 900, &prices(&[(1, 95), (2, 0), (3, 120)])).unwrap();
    assert_eq!(without_rye.per_piece(), None);
    assert_eq!(
        PieceCost::new(&Formula { lines: vec![] }, 900, &all),
        Err(ScaleError::Empty(None))
    );
}

fn input<'a>(
    standard: &'a Formula,
    actual: &'a Formula,
    dough_g: u64,
    good_g: u64,
    standard_prices: &'a Prices,
    actual_prices: &'a Prices,
) -> VarianceInput<'a> {
    VarianceInput {
        standard,
        actual,
        dough_g,
        good_g,
        standard_prices,
        actual_prices,
    }
}

#[test]
fn the_variance_splits_into_recipe_price_and_yield() {
    let standard = bread();
    // The baker took the water down to 70% and the salt up to 2,2%.
    let actual = Formula {
        lines: vec![leaf(1, 100_000), leaf(2, 70_000), leaf(3, 2_200)],
    };
    let old = prices(&[(1, 90), (2, 0), (3, 120)]);
    let new = prices(&[(1, 100), (2, 0), (3, 120)]);
    // 20 pieces of 900 g planned, 18 good.
    let v = Variance::new(&input(&standard, &actual, 18_000, 16_200, &old, &new)).unwrap();
    // Standard dough: 10 170 g flour, 7627 g water, 203 g salt: €9,3966 at the old prices.
    // Batch dough: 10 453 g flour, 7317 g water, 230 g salt: €9,6837 old, €10,729 new.
    assert_eq!(v.standard, cents(846));
    assert_eq!(v.yield_loss, cents(94));
    assert_eq!(v.recipe, cents(28));
    assert_eq!(v.price, cents(105));
    assert_eq!(v.actual, cents(1073));
    assert_eq!(v.total(), cents(227));
}

#[test]
fn a_variance_needs_dough_lines_and_prices() {
    let standard = bread();
    let all = prices(&[(1, 90), (2, 0), (3, 120)]);
    let partial = prices(&[(1, 90), (2, 0)]);
    assert_eq!(
        Variance::new(&input(&standard, &standard, 0, 0, &all, &all)),
        Err(CostError::NoDough)
    );
    let empty = Formula { lines: vec![] };
    assert_eq!(
        Variance::new(&input(&standard, &empty, 900, 900, &all, &all)),
        Err(CostError::Scale(ScaleError::Empty(None)))
    );
    let Err(CostError::MissingPrices(missing)) =
        Variance::new(&input(&standard, &standard, 900, 900, &all, &partial))
    else {
        panic!("a missing price must stop the variance");
    };
    assert_eq!(missing.iter().map(|i| i.id.get()).collect::<Vec<_>>(), [3]);
}

#[test]
fn more_good_pieces_than_planned_is_a_yield_gain() {
    let standard = bread();
    let all = prices(&[(1, 90), (2, 0), (3, 120)]);
    let v = Variance::new(&input(&standard, &standard, 9000, 9900, &all, &all)).unwrap();
    assert!(v.yield_loss < Money::ZERO);
    assert_eq!(v.recipe, Money::ZERO);
    assert_eq!(v.price, Money::ZERO);
    assert_eq!(v.total(), v.yield_loss);
}

fn formula_strategy() -> impl Strategy<Value = Formula> {
    (
        prop::collection::vec((1_i64..6, 0_u32..200_000), 1..6),
        prop::option::of(1_u32..50_000),
    )
        .prop_map(|(lines, sub)| {
            let mut lines: Vec<Line> = lines.into_iter().map(|(id, p)| leaf(id, p)).collect();
            if lines.iter().all(|l| l.pct.thousandths() == 0) {
                lines[0].pct = pct(100);
            }
            if let Some(p) = sub {
                lines.push(levain(p));
            }
            Formula { lines }
        })
}

fn prices_strategy() -> impl Strategy<Value = Prices> {
    prop::collection::vec(0_i64..2_000_000, 5).prop_map(|list| {
        list.into_iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    IngredientId::new(i64::try_from(i).unwrap() + 1),
                    UnitPrice::per_kg(cents(c)),
                )
            })
            .collect()
    })
}

proptest! {
    #[test]
    fn the_parts_add_up_to_the_total(
        standard in formula_strategy(),
        actual in formula_strategy(),
        dough_g in 1_u64..5_000_000,
        good in 0_u64..2_000,
        standard_prices in prices_strategy(),
        actual_prices in prices_strategy(),
    ) {
        let good_g = dough_g * good / 1000;
        let v = Variance::new(&input(
            &standard, &actual, dough_g, good_g, &standard_prices, &actual_prices,
        )).unwrap();
        let parts = v.recipe.cents() + v.price.cents() + v.yield_loss.cents();
        prop_assert_eq!(parts, v.total().cents());
        prop_assert_eq!(v.total().cents(), v.actual.cents() - v.standard.cents());

        let exact = |f: &Formula, p: &Prices| {
            DoughCost::new(&DoughGrams::from_formula(f, dough_g).unwrap(), p)
                .total()
                .unwrap()
                .get()
        };
        let s1 = exact(&standard, &standard_prices);
        let s2 = exact(&actual, &standard_prices);
        let s3 = exact(&actual, &actual_prices);
        // Each part is less than a cent (1000 millicents) from its exact value.
        let near = |part: Money, exact_millicents: i128| {
            (i128::from(part.cents()) * 1000 - exact_millicents).abs() < 1000
        };
        prop_assert!(near(v.recipe, s2 - s1));
        prop_assert!(near(v.price, s3 - s2));
        let s0_times_dough = s1 * i128::from(good_g);
        prop_assert!(
            (i128::from(v.yield_loss.cents()) * 1000 * i128::from(dough_g)
                - (s1 * i128::from(dough_g) - s0_times_dough)).abs()
                < 1000 * i128::from(dough_g)
        );
        prop_assert_eq!(v.actual.cents(), round_div(s3, 1000));
    }
}

#[test]
fn a_line_left_out_needs_no_price() {
    // The baker set salt to 0%: it weighs nothing and has no price.
    let formula = Formula {
        lines: vec![leaf(1, 100_000), leaf(2, 75_000), leaf(3, 0)],
    };
    let dough = DoughGrams::from_formula(&formula, 1750).unwrap();
    let cost = DoughCost::new(&dough, &prices(&[(1, 100), (2, 0)]));
    assert_eq!(cost.missing().count(), 0);
    assert_eq!(cost.total().unwrap().cents(), cents(100));
}

fn package(cents: i64, weight_g: u32) -> UnitPrice {
    UnitPrice::new(Money::from_cents(cents), weight_g).unwrap()
}

#[test]
fn a_price_needs_a_weight() {
    assert_eq!(UnitPrice::new(Money::from_cents(2137), 0), None);
    let sack = package(2137, 25_000);
    assert_eq!(sack.price(), Money::from_cents(2137));
    assert_eq!(sack.weight_g(), 25_000);
    assert_eq!(UnitPrice::per_kg(cents(95)), package(95, 1000));
}

#[test]
fn a_package_price_costs_grams_without_rounding_it_per_kilo() {
    // €21,37 for 25 kg is €0,8548 per kilo: 0,08548 cent per gram, 85,48 millicents.
    let sack = package(2137, 25_000);
    assert_eq!(sack.cost(25_000).get(), 2_137_000);
    assert_eq!(sack.cost(1000).get(), 85_480);
    assert_eq!(sack.cost(10_453).get(), 893_522);
    assert_eq!(sack.per_kg_cents(), cents(85));
    assert_eq!(sack.per_gram().get(), 85);
    // Rounded to €0,85 per kilo first, 10 453 g would come to 888 505 millicents.
    assert_ne!(
        sack.cost(10_453),
        UnitPrice::per_kg(sack.per_kg_cents()).cost(10_453)
    );

    // €3,99 for 750 g: 0,532 cent per gram.
    let pack = package(399, 750);
    assert_eq!(pack.cost(750).get(), 399_000);
    assert_eq!(pack.cost(1).get(), 532);
    assert_eq!(pack.cost(100).get(), 53_200);
    assert_eq!(pack.cost(7).get(), 3724);
    assert_eq!(pack.per_kg_cents(), cents(532));

    // €1,00 for 3 g: 33 333,33… millicents a gram, half up per line.
    let odd = package(100, 3);
    assert_eq!(odd.cost(1).get(), 33_333);
    assert_eq!(odd.cost(2).get(), 66_667);
    assert_eq!(odd.per_kg_cents(), cents(33_333));
    assert_eq!(package(0, 750).cost(1_000_000), MilliCents::ZERO);
}

#[test]
fn package_prices_cost_a_dough() {
    let dough = DoughGrams::from_formula(&bread(), 17_700).unwrap();
    let all: Prices = [
        (IngredientId::new(1), package(2137, 25_000)),
        (IngredientId::new(2), package(0, 1)),
        (IngredientId::new(3), package(399, 750)),
    ]
    .into_iter()
    .collect();
    let cost = DoughCost::new(&dough, &all);
    // 10 kg flour: 854 800; 200 g salt: 106 400.
    assert_eq!(cost.total().unwrap().get(), 854_800 + 106_400);
    assert_eq!(cost.total().unwrap().cents(), cents(961));
    assert_eq!(cost.lines[0].price, Some(package(2137, 25_000)));
}

proptest! {
    #[test]
    fn a_price_per_kilo_costs_as_before(grams in 0_u64..100_000_000, c in 0_i64..10_000_001) {
        // ADR 0045 before prices per package: grams × cents per kg, exactly.
        let price = UnitPrice::per_kg(cents(c));
        prop_assert_eq!(price.cost(grams).get(), i128::from(grams) * i128::from(c));
        prop_assert_eq!(price.per_kg_cents(), cents(c));
        prop_assert_eq!(price.per_gram().get(), i128::from(c));
    }

    #[test]
    fn a_package_line_is_within_half_a_millicent(
        grams in 0_u64..100_000_000,
        c in 0_i64..10_000_001,
        weight in 1_u32..1_000_001,
    ) {
        let cost = package(c, weight).cost(grams).get();
        let exact_times_weight = i128::from(grams) * i128::from(c) * 1000;
        let weight = i128::from(weight);
        // |cost − exact| ≤ ½ millicent, with ties rounded up.
        prop_assert!(2 * (cost * weight - exact_times_weight) <= weight);
        prop_assert!(2 * (exact_times_weight - cost * weight) < weight);
    }

    #[test]
    fn package_prices_keep_the_parts_adding_up(
        standard in formula_strategy(),
        actual in formula_strategy(),
        dough_g in 1_u64..5_000_000,
        good in 0_u64..2_000,
        standard_prices in package_prices_strategy(),
        actual_prices in package_prices_strategy(),
    ) {
        let good_g = dough_g * good / 1000;
        let v = Variance::new(&input(
            &standard, &actual, dough_g, good_g, &standard_prices, &actual_prices,
        )).unwrap();
        let parts = v.recipe.cents() + v.price.cents() + v.yield_loss.cents();
        prop_assert_eq!(parts, v.total().cents());
        let exact = |f: &Formula, p: &Prices| {
            DoughCost::new(&DoughGrams::from_formula(f, dough_g).unwrap(), p)
                .total()
                .unwrap()
                .get()
        };
        let s1 = exact(&standard, &standard_prices);
        let s2 = exact(&actual, &standard_prices);
        let s3 = exact(&actual, &actual_prices);
        let near = |part: Money, exact_millicents: i128| {
            (i128::from(part.cents()) * 1000 - exact_millicents).abs() < 1000
        };
        prop_assert!(near(v.recipe, s2 - s1));
        prop_assert!(near(v.price, s3 - s2));
        prop_assert_eq!(v.actual.cents(), round_div(s3, 1000));
    }
}

fn package_prices_strategy() -> impl Strategy<Value = Prices> {
    prop::collection::vec((0_i64..10_000_001, 1_u32..1_000_001), 5).prop_map(|list| {
        list.into_iter()
            .enumerate()
            .map(|(i, (c, w))| {
                (
                    IngredientId::new(i64::try_from(i).unwrap() + 1),
                    package(c, w),
                )
            })
            .collect()
    })
}
