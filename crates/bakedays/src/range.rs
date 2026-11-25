use std::collections::BTreeMap;

use domain::{BakeDayId, ProductId};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::capacity::{Capacity, ProductLimit, Remaining, Used};
use crate::extras::ProductionExtraId;
use crate::orders::OrderSource;
use crate::status::BakeDayStatus;

/// A product on a bake day's range (F1, F3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BakeDayProduct {
    pub product_id: ProductId,
    pub max_qty: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Availability {
    pub product_id: ProductId,
    pub remaining: Remaining,
}

fn to_db(value: u32, what: &'static str) -> Result<i32, Error> {
    i32::try_from(value).map_err(|_| Error::Invalid(what))
}

fn from_db(value: i32) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| Error::Corrupt(format!("negative quantity {value}")))
}

/// Locks the bake day against status changes (`FOR SHARE`) and refuses done or cancelled
/// days. Every write to a day's range or extras starts here (ADR 0008).
pub(crate) async fn lock_editable_day(
    conn: &mut PgConnection,
    day: BakeDayId,
) -> Result<(), Error> {
    let status = sqlx::query_scalar!(
        "SELECT status FROM bake_day WHERE id = $1 FOR SHARE",
        day.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?;
    let status: BakeDayStatus = status
        .parse()
        .map_err(|e: crate::status::UnknownStatus| Error::Corrupt(e.to_string()))?;
    if status.is_final() {
        return Err(Error::Final);
    }
    Ok(())
}

/// Takes the capacity locks for some products on a bake day, in the fixed order of
/// ADR 0008: their `bake_day_product` rows by product id, `FOR UPDATE`. Anything that
/// takes capacity (extra production, checkout) holds these locks while it reads usage,
/// checks and writes, so two writers never both take the last piece and never deadlock
/// on each other.
///
/// # Errors
/// [`Error::Db`].
pub async fn lock_capacity(
    conn: &mut PgConnection,
    day: BakeDayId,
    products: &[ProductId],
) -> Result<(), Error> {
    let ids: Vec<i64> = products.iter().map(|p| p.get()).collect();
    sqlx::query!(
        "SELECT product_id FROM bake_day_product
        WHERE bake_day_id = $1 AND product_id = ANY($2)
        ORDER BY product_id FOR UPDATE",
        day.get(),
        &ids,
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(())
}

/// Locks the day and checks `product` and `max_qty`; the maximum as stored.
async fn checked_limit(
    conn: &mut PgConnection,
    day: BakeDayId,
    product: ProductId,
    max_qty: Option<u32>,
) -> Result<Option<i32>, Error> {
    let max_qty = max_qty.map(|q| to_db(q, "maximum")).transpose()?;
    lock_editable_day(conn, day).await?;
    let product_exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM product WHERE id = $1) AS "exists!""#,
        product.get(),
    )
    .fetch_one(&mut *conn)
    .await?;
    if !product_exists {
        return Err(Error::Invalid("product"));
    }
    Ok(max_qty)
}

/// Adds a product to the day's range, or replaces its maximum.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Final`], or [`Error::Invalid`] for an unknown product
/// or a maximum that does not fit.
pub async fn set_product(
    conn: &mut PgConnection,
    day: BakeDayId,
    input: &BakeDayProduct,
) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    let max_qty = checked_limit(&mut tx, day, input.product_id, input.max_qty).await?;
    sqlx::query!(
        "INSERT INTO bake_day_product (bake_day_id, product_id, max_qty) VALUES ($1, $2, $3)
        ON CONFLICT (bake_day_id, product_id) DO UPDATE SET max_qty = EXCLUDED.max_qty",
        day.get(),
        input.product_id.get(),
        max_qty,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Changes the maximum of a product already on the day's range.
///
/// # Errors
/// [`Error::NotFound`] when the day or the product on it does not exist, [`Error::Final`],
/// or [`Error::Invalid`] as for [`set_product`].
pub async fn update_product(
    conn: &mut PgConnection,
    day: BakeDayId,
    product: ProductId,
    max_qty: Option<u32>,
) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    let max_qty = checked_limit(&mut tx, day, product, max_qty).await?;
    let updated = sqlx::query!(
        "UPDATE bake_day_product SET max_qty = $3 WHERE bake_day_id = $1 AND product_id = $2",
        day.get(),
        product.get(),
        max_qty,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(Error::NotFound);
    }
    tx.commit().await?;
    Ok(())
}

/// # Errors
/// [`Error::NotFound`] when the product is not on the day, or [`Error::Final`].
pub async fn remove_product(
    conn: &mut PgConnection,
    day: BakeDayId,
    product: ProductId,
) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    lock_editable_day(&mut tx, day).await?;
    let removed = sqlx::query!(
        "DELETE FROM bake_day_product WHERE bake_day_id = $1 AND product_id = $2",
        day.get(),
        product.get(),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if removed == 0 {
        return Err(Error::NotFound);
    }
    tx.commit().await?;
    Ok(())
}

/// The day's range, by product id.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_products(
    conn: &mut PgConnection,
    day: BakeDayId,
) -> Result<Vec<BakeDayProduct>, Error> {
    sqlx::query!(
        "SELECT product_id, max_qty FROM bake_day_product
        WHERE bake_day_id = $1 ORDER BY product_id",
        day.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(BakeDayProduct {
            product_id: ProductId::new(row.product_id),
            max_qty: row.max_qty.map(from_db).transpose()?,
        })
    })
    .collect()
}

pub(crate) async fn extras_by_product(
    conn: &mut PgConnection,
    day: BakeDayId,
    excluding_extra: Option<ProductionExtraId>,
) -> Result<BTreeMap<ProductId, u32>, Error> {
    sqlx::query!(
        r#"SELECT product_id, sum(qty)::bigint AS "qty!" FROM production_extra
        WHERE bake_day_id = $1 AND id IS DISTINCT FROM $2
        GROUP BY product_id"#,
        day.get(),
        excluding_extra.map(ProductionExtraId::get),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        let qty =
            u32::try_from(row.qty).map_err(|_| Error::Corrupt(format!("extras {}", row.qty)))?;
        Ok((ProductId::new(row.product_id), qty))
    })
    .collect()
}

pub(crate) async fn load_capacity(
    conn: &mut PgConnection,
    day: BakeDayId,
    orders: &impl OrderSource,
    excluding_extra: Option<ProductionExtraId>,
) -> Result<(Capacity, Vec<BakeDayProduct>), Error> {
    let ordered = orders.ordered(conn, day).await?;
    let products = list_products(conn, day).await?;
    let limits: Vec<ProductLimit> = products
        .iter()
        .map(|p| ProductLimit {
            product_id: p.product_id,
            max_qty: p.max_qty,
        })
        .collect();
    let extras = extras_by_product(conn, day, excluding_extra).await?;
    let mut used: BTreeMap<ProductId, Used> = ordered
        .iter()
        .map(|(&product, &ordered)| (product, Used { ordered, extras: 0 }))
        .collect();
    for (product, qty) in extras {
        used.entry(product).or_default().extras = qty;
    }
    let capacity = Capacity::new(limits, used);
    Ok((capacity, products))
}

/// Remaining pieces per product on the day's range (F3), after orders and extra production.
/// A display read: it takes no locks, so a writer must not decide on it; writers lock with
/// [`lock_capacity`] and check inside their own transaction (ADR 0008).
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn availability(
    conn: &mut PgConnection,
    day: BakeDayId,
    orders: &impl OrderSource,
) -> Result<Vec<Availability>, Error> {
    let (capacity, products) = load_capacity(conn, day, orders, None).await?;
    Ok(products
        .into_iter()
        .map(|p| Availability {
            product_id: p.product_id,
            remaining: capacity.remaining(p.product_id),
        })
        .collect())
}

/// Checks that an order of `request` pieces still fits the day's capacity, counting its
/// lines together. Call it only after [`lock_capacity`] for the same products, in the
/// same transaction; `orders` is read here, after the locks (ADR 0008).
///
/// # Errors
/// [`Error::CapacityExceeded`] for the first product that does not fit, or
/// [`Error::Db`] / [`Error::Corrupt`].
pub async fn check_order(
    conn: &mut PgConnection,
    day: BakeDayId,
    orders: &impl OrderSource,
    request: &[(ProductId, u32)],
) -> Result<(), Error> {
    let (capacity, _) = load_capacity(conn, day, orders, None).await?;
    capacity.check_all(request)?;
    Ok(())
}

/// Sold out: the range is not empty and nothing is left of any product (F1a).
#[must_use]
pub fn sold_out(availability: &[Availability]) -> bool {
    !availability.is_empty()
        && availability
            .iter()
            .all(|a| a.remaining == Remaining::Pieces(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn left(remaining: Remaining) -> Availability {
        Availability {
            product_id: ProductId::new(1),
            remaining,
        }
    }

    #[test]
    fn sold_out_needs_a_range_with_nothing_left() {
        assert!(!sold_out(&[]));
        assert!(sold_out(&[
            left(Remaining::Pieces(0)),
            left(Remaining::Pieces(0))
        ]));
        assert!(!sold_out(&[
            left(Remaining::Pieces(0)),
            left(Remaining::Pieces(1))
        ]));
        assert!(!sold_out(&[left(Remaining::Unlimited)]));
    }
}
