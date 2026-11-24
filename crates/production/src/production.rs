use std::collections::BTreeMap;

use bakedays::{BakeDay, Purpose, days, extras};
use domain::{BakeDayId, ProductId};
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

/// Totals per product for one bake day: orders plus extra production (F7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionList {
    pub day: BakeDay,
    /// Products with something to bake, by name.
    pub products: Vec<ProductTotal>,
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

    tx.commit().await?;
    Ok(ProductionList { day, products })
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
