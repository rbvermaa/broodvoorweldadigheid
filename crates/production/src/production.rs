use std::collections::BTreeMap;

use bakedays::{BakeDay, Purpose, days, extras, range};
use domain::{BakeDayId, CapacityPoolId, ProductId};
use i18n::Translator;
use sqlx::PgConnection;

use crate::{Error, paper, pdf};

/// Extra pieces on top of orders, by purpose (F7a).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Extras {
    pub freezer: u64,
    pub sumup: u64,
    pub samples: u64,
    pub other: u64,
}

impl Extras {
    #[must_use]
    pub const fn get(&self, purpose: Purpose) -> u64 {
        match purpose {
            Purpose::Freezer => self.freezer,
            Purpose::SumUp => self.sumup,
            Purpose::Samples => self.samples,
            Purpose::Other => self.other,
        }
    }

    #[must_use]
    pub const fn total(&self) -> u64 {
        self.freezer + self.sumup + self.samples + self.other
    }

    pub(crate) fn add(&mut self, purpose: Purpose, qty: u64) {
        let slot = match purpose {
            Purpose::Freezer => &mut self.freezer,
            Purpose::SumUp => &mut self.sumup,
            Purpose::Samples => &mut self.samples,
            Purpose::Other => &mut self.other,
        };
        *slot += qty;
    }
}

/// What to bake of one product.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductTotal {
    pub product_id: ProductId,
    pub name: String,
    /// Pieces in paid, confirmed and picked-up orders.
    pub ordered: u64,
    pub extras: Extras,
}

impl ProductTotal {
    #[must_use]
    pub const fn to_bake(&self) -> u64 {
        self.ordered + self.extras.total()
    }
}

/// The units a capacity pool's products take on the production list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolTotal {
    pub pool_id: CapacityPoolId,
    pub name: String,
    pub max_units: u32,
    pub units: u64,
}

/// Totals per product for one bake day: orders plus extra production (F7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionList {
    pub day: BakeDay,
    /// Products with something to bake, by name.
    pub products: Vec<ProductTotal>,
    /// Every pool of the day, by name.
    pub pools: Vec<PoolTotal>,
}

/// The production list of a bake day. Orders waiting for payment are left out, also
/// within their hold: the list shows what the bakery is sure to bake. All reads see one
/// snapshot of the database, so call it outside a transaction.
///
/// # Errors
/// [`Error::BakeDays`] with `NotFound` for an unknown day, or a database error.
pub async fn production_list(
    conn: &mut PgConnection,
    day: BakeDayId,
) -> Result<ProductionList, Error> {
    let mut tx = crate::snapshot(conn).await?;
    let day = days::get(&mut tx, day).await?;
    let mut counts: BTreeMap<ProductId, (u64, Extras)> = BTreeMap::new();
    for order in orders::order::list_firm(&mut tx, day.id).await? {
        for line in &order.lines {
            counts.entry(line.product_id).or_default().0 += u64::from(line.qty);
        }
    }
    for extra in extras::list(&mut tx, day.id).await? {
        counts
            .entry(extra.product_id)
            .or_default()
            .1
            .add(extra.purpose, u64::from(extra.qty));
    }
    let mut products = Vec::with_capacity(counts.len());
    for (&product_id, &(ordered, extras)) in &counts {
        let product = catalog::products::get(&mut tx, product_id).await?;
        products.push(ProductTotal {
            product_id,
            name: product.name_nl,
            ordered,
            extras,
        });
    }
    products.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.product_id.cmp(&b.product_id))
    });

    let mut units: BTreeMap<CapacityPoolId, u64> = BTreeMap::new();
    for on_range in range::list_products(&mut tx, day.id).await? {
        let (Some(pool), Some(product)) = (
            on_range.capacity_pool_id,
            products
                .iter()
                .find(|p| p.product_id == on_range.product_id),
        ) else {
            continue;
        };
        *units.entry(pool).or_default() += product.to_bake() * u64::from(on_range.units_per_item);
    }
    let pools = range::list_pools(&mut tx, day.id)
        .await?
        .into_iter()
        .map(|pool| PoolTotal {
            units: units.get(&pool.id).copied().unwrap_or(0),
            pool_id: pool.id,
            name: pool.name,
            max_units: pool.max_units,
        })
        .collect();
    tx.commit().await?;
    Ok(ProductionList {
        day,
        products,
        pools,
    })
}

/// The production list as an A4 PDF.
///
/// # Errors
/// [`Error::I18n`] or [`Error::Pdf`].
pub fn production_pdf(list: &ProductionList, t: &Translator) -> Result<Vec<u8>, Error> {
    let data = paper::production(list, t)?;
    pdf::render(
        &pdf::PRODUCTION,
        &data,
        format!("production-{}", list.day.date),
    )
}
