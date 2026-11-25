//! Baker's percentages, sub-recipes, scaling and hydration (F14). Pure: no I/O.
//!
//! Every line's percentage is relative to the total flour of the recipe it is in, flour
//! inside its sub-recipes included. A sub-recipe line at p% weighs p% of that flour, and
//! its own lines split that weight in their own percentages.

use std::fmt;
use std::str::FromStr;

use domain::{IngredientId, RecipeId, RecipeVersionId};

/// A percentage in integer thousandths of a percent: 100% is `Pct::from_thousandths(100_000)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pct(u32);

impl Pct {
    pub const HUNDRED: Self = Self(100_000);

    #[must_use]
    pub const fn from_thousandths(thousandths: u32) -> Self {
        Self(thousandths)
    }

    #[must_use]
    pub const fn thousandths(self) -> u32 {
        self.0
    }
}

/// How an ingredient counts for hydration and total flour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IngredientRole {
    Flour,
    Water,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown code: {0}")]
pub struct UnknownCode(pub String);

impl IngredientRole {
    pub const ALL: [Self; 3] = [Self::Flour, Self::Water, Self::Other];

    /// Stored in the database; never change.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Flour => "flour",
            Self::Water => "water",
            Self::Other => "other",
        }
    }
}

impl FromStr for IngredientRole {
    type Err = UnknownCode;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|role| role.code() == code)
            .ok_or_else(|| UnknownCode(code.to_owned()))
    }
}

/// What a recipe is for. Flour inside a [`RecipeKind::Preferment`] counts as prefermented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecipeKind {
    Dough,
    Preferment,
    Soaker,
    Filling,
    Topping,
}

impl RecipeKind {
    pub const ALL: [Self; 5] = [
        Self::Dough,
        Self::Preferment,
        Self::Soaker,
        Self::Filling,
        Self::Topping,
    ];

    /// Stored in the database; never change.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Dough => "dough",
            Self::Preferment => "preferment",
            Self::Soaker => "soaker",
            Self::Filling => "filling",
            Self::Topping => "topping",
        }
    }

    /// Whether a sub-recipe of this kind shows on an ingredient list as a compound
    /// ingredient unless its recipe says otherwise (ADR 0046): fillings and toppings do;
    /// a levain, soaker or dough merges into the list.
    #[must_use]
    pub const fn compound_by_default(self) -> bool {
        matches!(self, Self::Filling | Self::Topping)
    }
}

impl FromStr for RecipeKind {
    type Err = UnknownCode;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.code() == code)
            .ok_or_else(|| UnknownCode(code.to_owned()))
    }
}

impl fmt::Display for RecipeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formula {
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub pct: Pct,
    pub part: Part,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Ingredient(IngredientRef),
    Sub(SubRecipe),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngredientRef {
    pub id: IngredientId,
    pub name: String,
    pub role: IngredientRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubRecipe {
    pub version_id: RecipeVersionId,
    pub recipe_id: RecipeId,
    pub name: String,
    pub version: i32,
    pub kind: RecipeKind,
    pub formula: Formula,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScaleError {
    /// The recipe, or the sub-recipe with this version, has no lines to split a weight over.
    #[error("recipe has no lines")]
    Empty(Option<RecipeVersionId>),
}

/// One line with its weight in whole grams; a sub-recipe line has its own lines below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scaled<'a> {
    pub line: &'a Line,
    pub grams: u64,
    pub lines: Vec<Scaled<'a>>,
}

/// Splits `total` over `weights` in proportion, in whole units, so the parts add up to
/// `total` exactly (largest remainder). Ties go to the earlier weight. `None` when every
/// weight is zero.
fn apportion(total: u64, weights: &[u32]) -> Option<Vec<u64>> {
    let sum: u128 = weights.iter().map(|&w| u128::from(w)).sum();
    if sum == 0 {
        return None;
    }
    let mut parts = Vec::with_capacity(weights.len());
    let mut remainders = Vec::with_capacity(weights.len());
    for (index, &weight) in weights.iter().enumerate() {
        let exact = u128::from(total) * u128::from(weight);
        parts.push(u64::try_from(exact / sum).unwrap_or(u64::MAX));
        remainders.push((exact % sum, index));
    }
    let left = total - parts.iter().sum::<u64>();
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for &(_, index) in remainders
        .iter()
        .take(usize::try_from(left).unwrap_or(usize::MAX))
    {
        parts[index] += 1;
    }
    Some(parts)
}

fn scale_lines(
    lines: &[Line],
    grams: u64,
    owner: Option<RecipeVersionId>,
) -> Result<Vec<Scaled<'_>>, ScaleError> {
    let weights: Vec<u32> = lines.iter().map(|line| line.pct.thousandths()).collect();
    let parts = apportion(grams, &weights).ok_or(ScaleError::Empty(owner))?;
    lines
        .iter()
        .zip(parts)
        .map(|(line, grams)| {
            let lines = match &line.part {
                Part::Ingredient(_) => Vec::new(),
                Part::Sub(sub) => scale_lines(&sub.formula.lines, grams, Some(sub.version_id))?,
            };
            Ok(Scaled { line, grams, lines })
        })
        .collect()
}

impl Formula {
    /// The grams of every line for a dough of `grams` in total. Each level splits its
    /// whole grams with the largest-remainder rule, so all ingredients add up to `grams`
    /// exactly and an ingredient at nesting depth d is less than d + 1 grams off its exact
    /// share (ADR 0037).
    ///
    /// # Errors
    /// [`ScaleError::Empty`] when the recipe or one of its sub-recipes has no lines.
    pub fn scale(&self, grams: u64) -> Result<Vec<Scaled<'_>>, ScaleError> {
        scale_lines(&self.lines, grams, None)
    }

    /// Total flour, water and prefermented flour, and the derived percentages.
    ///
    /// # Errors
    /// [`ScaleError::Empty`] as for [`Formula::scale`].
    pub fn analysis(&self) -> Result<Analysis, ScaleError> {
        let scaled = self.scale(REFERENCE_GRAMS)?;
        let sums = leaf_grams(&scaled);
        let line_sum = self.line_sum();
        Ok(Analysis {
            hydration: percent_of(sums.water, sums.flour),
            prefermented_flour: percent_of(sums.prefermented_flour, sums.flour),
            total_flour: rounded(
                u128::from(sums.flour) * u128::from(line_sum),
                u128::from(REFERENCE_GRAMS),
            )
            .unwrap_or_default(),
        })
    }

    /// The percentages of the recipe's own lines added up, in thousandths.
    #[must_use]
    pub fn line_sum(&self) -> u64 {
        self.lines
            .iter()
            .map(|line| u64::from(line.pct.thousandths()))
            .sum()
    }
}

/// The percentage that makes a line weigh `grams` in a dough of `dough_grams` whose
/// lines add up to `line_sum` thousandths, rounded half up to 0.001%. `None` without a
/// dough weight or lines.
#[must_use]
pub fn pct_for_grams(grams: u64, dough_grams: u64, line_sum: u64) -> Option<Pct> {
    if line_sum == 0 {
        return None;
    }
    rounded(
        u128::from(grams) * u128::from(line_sum),
        u128::from(dough_grams),
    )
}

/// The dough weight once the lines' sum goes from `old_sum` to `new_sum` and the grams
/// per percent stay the same, rounded half up. Unchanged without an old sum.
#[must_use]
pub fn dough_for_sum(dough_grams: u64, old_sum: u64, new_sum: u64) -> u64 {
    if old_sum == 0 {
        return dough_grams;
    }
    let numerator = u128::from(dough_grams) * u128::from(new_sum);
    let denominator = u128::from(old_sum);
    u64::try_from((numerator * 2 + denominator) / (denominator * 2)).unwrap_or(u64::MAX)
}

/// The dough weight [`Formula::analysis`] scales to: large enough that whole-unit
/// rounding is far below the 0.001% the results show.
const REFERENCE_GRAMS: u64 = 1_000_000_000_000;

/// Flour and water over all leaves of a scaled formula, sub-recipes included.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeafGrams {
    pub flour: u64,
    pub water: u64,
    /// Flour inside recipes of kind [`RecipeKind::Preferment`].
    pub prefermented_flour: u64,
}

#[must_use]
pub fn leaf_grams(scaled: &[Scaled<'_>]) -> LeafGrams {
    let mut sums = LeafGrams::default();
    add_up(scaled, false, &mut sums);
    sums
}

fn add_up(scaled: &[Scaled<'_>], in_preferment: bool, sums: &mut LeafGrams) {
    for line in scaled {
        match &line.line.part {
            Part::Ingredient(ingredient) => match ingredient.role {
                IngredientRole::Flour => {
                    sums.flour += line.grams;
                    if in_preferment {
                        sums.prefermented_flour += line.grams;
                    }
                }
                IngredientRole::Water => sums.water += line.grams,
                IngredientRole::Other => {}
            },
            Part::Sub(sub) => add_up(
                &line.lines,
                in_preferment || sub.kind == RecipeKind::Preferment,
                sums,
            ),
        }
    }
}

/// `part / whole` as a percentage, rounded half up to 0.001%. `None` without a whole.
fn percent_of(part: u64, whole: u64) -> Option<Pct> {
    rounded(
        u128::from(part) * u128::from(Pct::HUNDRED.0),
        u128::from(whole),
    )
}

/// `numerator / denominator` in thousandths of a percent, rounded half up.
fn rounded(numerator: u128, denominator: u128) -> Option<Pct> {
    if denominator == 0 {
        return None;
    }
    let value = (numerator * 2 + denominator) / (denominator * 2);
    Some(Pct(u32::try_from(value).unwrap_or(u32::MAX)))
}

/// Derived from the leaf ingredients, water and flour inside sub-recipes included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Analysis {
    /// Water ÷ flour; `None` without flour.
    pub hydration: Option<Pct>,
    /// Flour inside preferments ÷ all flour; `None` without flour.
    pub prefermented_flour: Option<Pct>,
    /// All flour in baker's percent of the recipe's own lines; 100% when the percentages
    /// are consistent.
    pub total_flour: Pct,
}

/// Grams per ingredient over all levels, in order of first appearance.
#[must_use]
pub fn ingredient_totals<'a>(scaled: &[Scaled<'a>]) -> Vec<(&'a IngredientRef, u64)> {
    let mut totals: Vec<(&IngredientRef, u64)> = Vec::new();
    collect_totals(scaled, &mut totals);
    totals
}

fn collect_totals<'a>(scaled: &[Scaled<'a>], totals: &mut Vec<(&'a IngredientRef, u64)>) {
    for line in scaled {
        match &line.line.part {
            Part::Ingredient(ingredient) => {
                match totals
                    .iter_mut()
                    .find(|(known, _)| known.id == ingredient.id)
                {
                    Some((_, grams)) => *grams += line.grams,
                    None => totals.push((ingredient, line.grams)),
                }
            }
            Part::Sub(_) => collect_totals(&line.lines, totals),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apportion_gives_ties_to_the_earlier_weight() {
        assert_eq!(apportion(10, &[1, 1, 1]), Some(vec![4, 3, 3]));
        assert_eq!(apportion(0, &[5, 5]), Some(vec![0, 0]));
        assert_eq!(apportion(7, &[0, 0]), None);
        assert_eq!(apportion(7, &[]), None);
    }

    #[test]
    fn ratio_rounds_half_up() {
        assert_eq!(percent_of(1, 3), Some(Pct(33_333)));
        assert_eq!(percent_of(2, 3), Some(Pct(66_667)));
        assert_eq!(percent_of(1, 0), None);
    }

    #[test]
    fn codes_round_trip() {
        for role in IngredientRole::ALL {
            assert_eq!(role.code().parse::<IngredientRole>(), Ok(role));
        }
        for kind in RecipeKind::ALL {
            assert_eq!(kind.code().parse::<RecipeKind>(), Ok(kind));
            assert_eq!(kind.to_string(), kind.code());
        }
        assert_eq!(
            "meel".parse::<IngredientRole>(),
            Err(UnknownCode("meel".to_owned()))
        );
        assert!("deeg".parse::<RecipeKind>().is_err());
        assert_eq!(Pct::default().thousandths(), 0);
    }
}
