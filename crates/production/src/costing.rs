//! Actual cost per batch and its variance against the standard (F16, ADR 0045). A batch is
//! priced at the ingredient prices valid on its bake date, so old batches keep their old
//! prices.

use std::collections::HashMap;

use bakedays::days;
use db::time::{from_epoch_days, to_epoch_days};
use domain::{BakeDayId, BatchId, Money, ProductId};
use jiff::civil::Date;
use recipes::cost::{
    CostError, DoughCost, MilliCents, Variance, VarianceInput, per_good_piece, round_div,
};
use recipes::{DoughGrams, ScaleError, prices};
use sqlx::PgConnection;

use crate::Error;
use crate::batches::{self, Batch, BatchCode, Output};

/// A batch priced: what its dough cost and, once baked, the variance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchCost {
    pub batch: Batch,
    pub bake_date: Date,
    /// The date whose prices are the standard.
    pub standard_date: Date,
    /// The batch's lines for its planned dough at the bake date's prices; an error when the
    /// batch cannot be weighed.
    pub dough: Result<DoughCost, ScaleError>,
    /// Good pieces (baked minus waste) × dough per piece; `None` before the actuals.
    pub good_g: Option<u64>,
    /// Per product, by name: one good piece; `None` before the actuals, without good
    /// pieces or while an ingredient has no price.
    pub per_piece: Vec<(ProductId, Option<Money>)>,
    /// Once the actuals are in.
    pub variance: Option<Result<Variance, CostError>>,
}

impl BatchCost {
    /// What the batch cost; `None` while an ingredient has no price.
    #[must_use]
    pub fn actual(&self) -> Option<Money> {
        self.dough
            .as_ref()
            .ok()
            .and_then(DoughCost::total)
            .map(MilliCents::cents)
    }
}

/// Good pieces × dough per piece, once every product has its actuals.
fn good_g(outputs: &[Output]) -> Option<u64> {
    outputs.iter().try_fold(0, |sum, o| {
        let good = o.actual_pieces?.saturating_sub(o.waste_pieces?);
        Some(sum + u64::from(good) * u64::from(o.dough_g_per_piece))
    })
}

/// # Errors
/// [`Error::NotFound`] for an unknown batch; [`Error::Db`], [`Error::Corrupt`] or
/// [`Error::Recipes`].
pub async fn batch_cost(
    conn: &mut PgConnection,
    id: BatchId,
    standard_date: Date,
) -> Result<BatchCost, Error> {
    let batch = batches::get(conn, id).await?;
    let bake_date = days::get(conn, batch.bake_day_id).await?.date;
    let actual = batches::formula(conn, &batch).await?;
    let standard = recipes::recipes::formula(conn, batch.recipe_version_id).await?;
    let actual_prices = prices::prices_on(conn, bake_date).await?;
    let standard_prices = prices::prices_on(conn, standard_date).await?;
    let dough_g = batch.dough_g();
    let dough = DoughGrams::from_formula(&actual, dough_g)
        .map(|grams| DoughCost::new(&grams, &actual_prices));
    let good_g = good_g(&batch.outputs);
    let total = dough.as_ref().ok().and_then(DoughCost::total);
    let per_piece = batch
        .outputs
        .iter()
        .map(|o| {
            let cost = total
                .zip(good_g)
                .and_then(|(total, good)| per_good_piece(total, good, o.dough_g_per_piece));
            (o.product_id, cost)
        })
        .collect();
    let variance = good_g.map(|good_g| {
        Variance::new(&VarianceInput {
            standard: &standard,
            actual: &actual,
            dough_g,
            good_g,
            standard_prices: &standard_prices,
            actual_prices: &actual_prices,
        })
    });
    Ok(BatchCost {
        batch,
        bake_date,
        standard_date,
        dough,
        good_g,
        per_piece,
        variance,
    })
}

/// Where a product's ingredient cost per piece comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// The day's baked batches at the bake date's prices.
    Batches,
    /// The recipe version in use at the prices valid on the date.
    Standard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitCost {
    /// `None` when a baked batch of the product has an ingredient without a price.
    pub cost: Option<Money>,
    pub basis: Basis,
}

/// Good pieces of one output; zero before the actuals.
fn good_pieces(output: &Output) -> u32 {
    output
        .actual_pieces
        .zip(output.waste_pieces)
        .map_or(0, |(pieces, waste)| pieces.saturating_sub(waste))
}

/// The ingredient cost of one piece of each product on a bake day. With baked batches: all
/// their cost over all their good pieces, wasted batches included, and unknown when one of
/// them cannot be priced. A batch with several products shares its cost by the dough of
/// their good pieces, or of their planned pieces when none came out good. Without good
/// pieces from batches: the standard cost at the prices valid on the bake date. Products
/// with neither are left out.
///
/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Recipes`].
pub async fn unit_costs(
    conn: &mut PgConnection,
    day: BakeDayId,
    products: &[ProductId],
) -> Result<HashMap<ProductId, UnitCost>, Error> {
    let date = days::get(conn, day).await?.date;
    let prices = prices::prices_on(conn, date).await?;
    // Per product: millicents and good pieces of its baked batches; `None` once one of
    // them cannot be priced.
    let mut from_batches: HashMap<ProductId, Option<(i128, u64)>> = HashMap::new();
    for batch in batches::formulas_for_day(conn, day).await? {
        let Some(good_g) = good_g(&batch.outputs) else {
            continue;
        };
        let weight = |o: &Output| {
            let pieces = if good_g > 0 {
                good_pieces(o)
            } else {
                o.planned_pieces
            };
            i128::from(pieces) * i128::from(o.dough_g_per_piece)
        };
        let weights: i128 = batch.outputs.iter().map(weight).sum();
        if weights == 0 {
            continue;
        }
        let total = DoughGrams::from_formula(&batch.formula, batch.dough_g)
            .ok()
            .and_then(|grams| DoughCost::new(&grams, &prices).total());
        for output in &batch.outputs {
            let entry = from_batches
                .entry(output.product_id)
                .or_insert(Some((0, 0)));
            *entry = entry.zip(total).map(|((sum, pieces), total)| {
                (
                    sum + i128::from(round_div(total.get() * weight(output), weights)),
                    pieces + u64::from(good_pieces(output)),
                )
            });
        }
    }
    let mut costs = HashMap::new();
    for standard in prices::standard_costs(conn, products, date).await? {
        let id = standard.product.product_id;
        let cost = match from_batches.get(&id) {
            Some(None) => Some(UnitCost {
                cost: None,
                basis: Basis::Batches,
            }),
            Some(Some((sum, pieces))) if *pieces > 0 => Some(UnitCost {
                cost: Some(Money::from_cents(round_div(
                    *sum,
                    1000 * i128::from(*pieces),
                ))),
                basis: Basis::Batches,
            }),
            _ => standard.per_piece().map(|cost| UnitCost {
                cost: Some(cost),
                basis: Basis::Standard,
            }),
        };
        if let Some(cost) = cost {
            costs.insert(id, cost);
        }
    }
    Ok(costs)
}

/// A baked batch for the costing overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BakedBatch {
    pub id: BatchId,
    pub code: BatchCode,
    pub bake_date: Date,
    pub recipe_name: String,
    pub version: i32,
}

/// Baked batches on bake days from `from` on, newest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn baked_since(conn: &mut PgConnection, from: Date) -> Result<Vec<BakedBatch>, Error> {
    sqlx::query!(
        r#"SELECT b.id, (b.code_date - date 'epoch') AS "code_days!", b.mix_no,
            (d.date - date 'epoch') AS "bake_days!", r.name, v.version
        FROM batch b
        JOIN bake_day d ON d.id = b.bake_day_id
        JOIN recipe_version v ON v.id = b.recipe_version_id
        JOIN recipe r ON r.id = v.recipe_id
        WHERE b.status = 'baked' AND d.date >= date 'epoch' + $1::integer
        ORDER BY d.date DESC, b.code_date DESC, b.mix_no DESC"#,
        to_epoch_days(from),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(BakedBatch {
            id: BatchId::new(row.id),
            code: BatchCode {
                date: from_epoch_days(row.code_days)?,
                mix_no: u32::try_from(row.mix_no)
                    .map_err(|_| Error::Corrupt("negative mix number".to_owned()))?,
            },
            bake_date: from_epoch_days(row.bake_days)?,
            recipe_name: row.name,
            version: row.version,
        })
    })
    .collect()
}
