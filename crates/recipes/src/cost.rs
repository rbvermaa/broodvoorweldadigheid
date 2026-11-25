//! Costing (F16, ADR 0045): what a dough costs at given ingredient prices, the standard
//! cost per piece, and a batch's variance split into recipe, price and yield. Pure: no I/O.
//!
//! Costs are exact integers in thousandths of a cent ([`MilliCents`]): grams × cents per
//! kilogram. Only the figures shown are rounded, half up to whole cents.

use std::collections::HashMap;

use domain::{IngredientId, Money};

use crate::formula::{Formula, IngredientRef, ScaleError};
use crate::needs::DoughGrams;

/// Ingredient prices valid on one date, in whole cents per kilogram.
pub type Prices = HashMap<IngredientId, Money>;

/// The standard cost per piece is worked out on this many pieces (ADR 0045).
pub const STANDARD_PIECES: u32 = 1000;

/// An exact cost in thousandths of a cent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MilliCents(i128);

impl MilliCents {
    pub const ZERO: Self = Self(0);

    /// `grams` at `per_kg` cents per kilogram.
    #[must_use]
    pub fn of(grams: u64, per_kg: Money) -> Self {
        Self(i128::from(grams) * i128::from(per_kg.cents()))
    }

    #[must_use]
    pub const fn get(self) -> i128 {
        self.0
    }

    /// Rounded half up to whole cents.
    #[must_use]
    pub fn cents(self) -> Money {
        Money::from_cents(round_div(self.0, 1000))
    }
}

/// `numerator / denominator` rounded half up (towards positive), clamped to `i64`.
/// `denominator` must be positive.
#[must_use]
pub fn round_div(numerator: i128, denominator: i128) -> i64 {
    let quotient = (2 * numerator + denominator).div_euclid(2 * denominator);
    i64::try_from(quotient.clamp(i128::from(i64::MIN), i128::from(i64::MAX))).unwrap_or_default()
}

/// One ingredient of a dough with its grams and what they cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostLine {
    pub ingredient: IngredientRef,
    pub grams: u64,
    /// `None` when the ingredient has no price on the date.
    pub per_kg: Option<Money>,
    /// Zero without a price.
    pub cost: MilliCents,
}

/// A dough's ingredients priced, each ingredient once, in the order of [`DoughGrams`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoughCost {
    pub lines: Vec<CostLine>,
}

impl DoughCost {
    #[must_use]
    pub fn new(dough: &DoughGrams, prices: &Prices) -> Self {
        Self {
            lines: dough
                .ingredients
                .iter()
                .map(|(ingredient, grams)| {
                    let per_kg = prices.get(&ingredient.id).copied();
                    CostLine {
                        ingredient: ingredient.clone(),
                        grams: *grams,
                        per_kg,
                        cost: per_kg.map_or(MilliCents::ZERO, |p| MilliCents::of(*grams, p)),
                    }
                })
                .collect(),
        }
    }

    /// Ingredients without a price on the date.
    pub fn missing(&self) -> impl Iterator<Item = &IngredientRef> {
        self.lines
            .iter()
            .filter(|line| line.per_kg.is_none())
            .map(|line| &line.ingredient)
    }

    /// The whole dough; `None` while an ingredient has no price, never a cost too low.
    #[must_use]
    pub fn total(&self) -> Option<MilliCents> {
        self.lines.iter().try_fold(MilliCents::ZERO, |sum, line| {
            line.per_kg.map(|_| MilliCents(sum.0 + line.cost.0))
        })
    }
}

/// The standard cost of a product: its recipe version at given prices, for
/// [`STANDARD_PIECES`] pieces of `dough_g_per_piece` grams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PieceCost {
    pub dough_g_per_piece: u32,
    /// The dough of [`STANDARD_PIECES`] pieces.
    pub dough: DoughCost,
}

impl PieceCost {
    /// # Errors
    /// [`ScaleError::Empty`] when the recipe or a sub-recipe has no lines.
    pub fn new(
        formula: &Formula,
        dough_g_per_piece: u32,
        prices: &Prices,
    ) -> Result<Self, ScaleError> {
        let grams = u64::from(dough_g_per_piece) * u64::from(STANDARD_PIECES);
        Ok(Self {
            dough_g_per_piece,
            dough: DoughCost::new(&DoughGrams::from_formula(formula, grams)?, prices),
        })
    }

    /// One piece, rounded half up to cents; `None` while an ingredient has no price.
    #[must_use]
    pub fn per_piece(&self) -> Option<Money> {
        self.dough
            .total()
            .map(|total| Money::from_cents(round_div(total.0, 1000 * i128::from(STANDARD_PIECES))))
    }
}

/// What can stop a cost from being worked out.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CostError {
    #[error(transparent)]
    Scale(#[from] ScaleError),
    /// These ingredients have no price on a date the cost needs.
    #[error("ingredients without a price")]
    MissingPrices(Vec<IngredientRef>),
    /// The batch has no planned dough to compare with.
    #[error("no planned dough")]
    NoDough,
}

/// What [`Variance::new`] compares.
#[derive(Debug, Clone, Copy)]
pub struct VarianceInput<'a> {
    /// The recipe version the batch was made from.
    pub standard: &'a Formula,
    /// The batch's own lines.
    pub actual: &'a Formula,
    /// Planned dough: planned pieces × dough per piece.
    pub dough_g: u64,
    /// Good pieces (baked minus waste) × dough per piece.
    pub good_g: u64,
    pub standard_prices: &'a Prices,
    /// Prices valid on the bake date.
    pub actual_prices: &'a Prices,
}

/// Actual minus standard cost of a batch's good pieces, split into three parts that add up
/// to it exactly (ADR 0045). With the exact costs
///
/// - `s0` = the version at standard prices for the planned dough × good ÷ planned dough,
/// - `s1` = the version at standard prices for the planned dough,
/// - `s2` = the batch lines at standard prices for the planned dough,
/// - `s3` = the batch lines at the bake date's prices for the planned dough (actual cost),
///
/// each rounded half up to cents as `r0..r3`: yield = r1 − r0, recipe = r2 − r1,
/// price = r3 − r2, total = r3 − r0. Every part is less than a cent from its exact value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variance {
    /// What the good pieces should have cost: `r0`.
    pub standard: Money,
    /// What the batch cost: `r3`.
    pub actual: Money,
    pub recipe: Money,
    pub price: Money,
    /// Positive when fewer good pieces came out than planned.
    pub yield_loss: Money,
}

impl Variance {
    /// # Errors
    /// [`CostError::Scale`] for a formula without lines, [`CostError::NoDough`] when no
    /// dough was planned, [`CostError::MissingPrices`] when an ingredient has no price at
    /// either date.
    pub fn new(input: &VarianceInput<'_>) -> Result<Self, CostError> {
        if input.dough_g == 0 {
            return Err(CostError::NoDough);
        }
        let standard = DoughGrams::from_formula(input.standard, input.dough_g)?;
        let actual = DoughGrams::from_formula(input.actual, input.dough_g)?;
        let costs = [
            DoughCost::new(&standard, input.standard_prices),
            DoughCost::new(&actual, input.standard_prices),
            DoughCost::new(&actual, input.actual_prices),
        ];
        let mut missing: Vec<IngredientRef> = Vec::new();
        for ingredient in costs.iter().flat_map(DoughCost::missing) {
            if !missing.iter().any(|known| known.id == ingredient.id) {
                missing.push(ingredient.clone());
            }
        }
        let [Some(s1), Some(s2), Some(s3)] = costs.each_ref().map(DoughCost::total) else {
            return Err(CostError::MissingPrices(missing));
        };
        let r0 = round_div(
            s1.0 * i128::from(input.good_g),
            1000 * i128::from(input.dough_g),
        );
        let [r1, r2, r3] = [s1, s2, s3].map(|s| s.cents().cents());
        Ok(Self {
            standard: Money::from_cents(r0),
            actual: Money::from_cents(r3),
            recipe: Money::from_cents(r2 - r1),
            price: Money::from_cents(r3 - r2),
            yield_loss: Money::from_cents(r1 - r0),
        })
    }

    /// Actual minus standard; the three parts add up to it.
    #[must_use]
    pub fn total(&self) -> Money {
        Money::from_cents(self.actual.cents() - self.standard.cents())
    }
}

/// The cost of one good piece of `dough_g_per_piece` grams when a batch's good pieces
/// weigh `good_g` in dough and the batch cost `total`; `None` without good pieces.
#[must_use]
pub fn per_good_piece(total: MilliCents, good_g: u64, dough_g_per_piece: u32) -> Option<Money> {
    (good_g > 0).then(|| {
        Money::from_cents(round_div(
            total.0 * i128::from(dough_g_per_piece),
            1000 * i128::from(good_g),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_is_half_up() {
        assert_eq!(round_div(5, 10), 1);
        assert_eq!(round_div(4, 10), 0);
        assert_eq!(round_div(-5, 10), 0);
        assert_eq!(round_div(-6, 10), -1);
        assert_eq!(round_div(i128::MAX / 4, 1), i64::MAX);
        assert_eq!(
            MilliCents::of(1500, Money::from_cents(81)).cents().cents(),
            122
        );
        assert_eq!(MilliCents::of(1, Money::from_cents(499)).get(), 499);
    }

    #[test]
    fn no_good_pieces_have_no_cost_per_piece() {
        assert_eq!(per_good_piece(MilliCents(1_000_000), 0, 900), None);
        assert_eq!(
            per_good_piece(MilliCents(1_000_000), 9000, 900),
            Some(Money::from_cents(100))
        );
    }

    #[test]
    fn errors_read_well_in_the_log() {
        assert_eq!(CostError::NoDough.to_string(), "no planned dough");
        assert_eq!(
            CostError::MissingPrices(Vec::new()).to_string(),
            "ingredients without a price"
        );
        assert_eq!(
            CostError::from(ScaleError::Empty(None)).to_string(),
            "recipe has no lines"
        );
    }
}
