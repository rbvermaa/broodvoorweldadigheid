use std::collections::BTreeMap;

use domain::{BakeDayId, CapacityPoolId, ProductId};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::capacity::{Capacity, PoolLimit, ProductLimit, Remaining, Used};
use crate::extras::ProductionExtraId;
use crate::orders::OrderSource;
use crate::status::BakeDayStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityPool {
    pub id: CapacityPoolId,
    pub bake_day_id: BakeDayId,
    pub name: String,
    pub max_units: u32,
}

/// A product on a bake day's range (F1, F3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BakeDayProduct {
    pub product_id: ProductId,
    pub max_qty: Option<u32>,
    pub capacity_pool_id: Option<CapacityPoolId>,
    pub units_per_item: u32,
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
/// days. Every write to a day's range, pools or extras starts here (ADR 0008).
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
/// ADR 0008: their `bake_day_product` rows by product id, then their `capacity_pool` rows
/// by pool id, all `FOR UPDATE`. Anything that takes capacity (extra production now,
/// checkout later) holds these locks while it reads usage, checks and writes, so two
/// writers never both take the last piece and never deadlock on each other.
///
/// # Errors
/// [`Error::Db`].
pub async fn lock_capacity(
    conn: &mut PgConnection,
    day: BakeDayId,
    products: &[ProductId],
) -> Result<(), Error> {
    let ids: Vec<i64> = products.iter().map(|p| p.get()).collect();
    let mut pools: Vec<i64> = sqlx::query_scalar!(
        "SELECT capacity_pool_id FROM bake_day_product
        WHERE bake_day_id = $1 AND product_id = ANY($2)
        ORDER BY product_id FOR UPDATE",
        day.get(),
        &ids,
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .flatten()
    .collect();
    pools.sort_unstable();
    pools.dedup();
    // One pool at a time, in id order, so the lock order never depends on a query plan.
    for pool in pools {
        sqlx::query_scalar!(
            "SELECT id FROM capacity_pool WHERE id = $1 FOR UPDATE",
            pool
        )
        .fetch_one(&mut *conn)
        .await?;
    }
    Ok(())
}

/// # Errors
/// [`Error::NotFound`], [`Error::Final`], or [`Error::Invalid`] for an empty or
/// duplicate name.
pub async fn create_pool(
    conn: &mut PgConnection,
    day: BakeDayId,
    name: &str,
    max_units: u32,
) -> Result<CapacityPool, Error> {
    if name.trim().is_empty() {
        return Err(Error::Invalid("pool name"));
    }
    let max = to_db(max_units, "pool size")?;
    let mut tx = conn.begin().await?;
    lock_editable_day(&mut tx, day).await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO capacity_pool (bake_day_id, name, max_units) VALUES ($1, $2, $3)
        ON CONFLICT (bake_day_id, name) DO NOTHING RETURNING id",
        day.get(),
        name.trim(),
        max,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::Invalid("duplicate pool name"))?;
    tx.commit().await?;
    Ok(CapacityPool {
        id: CapacityPoolId::new(id),
        bake_day_id: day,
        name: name.trim().to_owned(),
        max_units,
    })
}

/// Changes a pool's size. Making it smaller than what is already taken is allowed; the
/// remaining capacity is then zero.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Final`] or [`Error::Invalid`].
pub async fn resize_pool(
    conn: &mut PgConnection,
    id: CapacityPoolId,
    max_units: u32,
) -> Result<(), Error> {
    let max = to_db(max_units, "pool size")?;
    let mut tx = conn.begin().await?;
    let day = sqlx::query_scalar!(
        "SELECT bake_day_id FROM capacity_pool WHERE id = $1",
        id.get()
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    lock_editable_day(&mut tx, BakeDayId::new(day)).await?;
    sqlx::query!(
        "UPDATE capacity_pool SET max_units = $2 WHERE id = $1",
        id.get(),
        max,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// A bake day's pools, by name.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_pools(
    conn: &mut PgConnection,
    day: BakeDayId,
) -> Result<Vec<CapacityPool>, Error> {
    sqlx::query!(
        "SELECT id, bake_day_id, name, max_units FROM capacity_pool
        WHERE bake_day_id = $1 ORDER BY name, id",
        day.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(CapacityPool {
            id: CapacityPoolId::new(row.id),
            bake_day_id: BakeDayId::new(row.bake_day_id),
            name: row.name,
            max_units: from_db(row.max_units)?,
        })
    })
    .collect()
}

/// Locks the day and checks `input`; the limits as stored.
async fn checked_limits(
    conn: &mut PgConnection,
    day: BakeDayId,
    input: &BakeDayProduct,
) -> Result<(Option<i32>, i32), Error> {
    if input.units_per_item == 0 {
        return Err(Error::Invalid("units per item"));
    }
    let max_qty = input.max_qty.map(|q| to_db(q, "maximum")).transpose()?;
    let units = to_db(input.units_per_item, "units per item")?;
    lock_editable_day(conn, day).await?;
    let product_exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM product WHERE id = $1) AS "exists!""#,
        input.product_id.get(),
    )
    .fetch_one(&mut *conn)
    .await?;
    if !product_exists {
        return Err(Error::Invalid("product"));
    }
    if let Some(pool) = input.capacity_pool_id {
        let same_day = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM capacity_pool WHERE id = $1 AND bake_day_id = $2) AS "exists!""#,
            pool.get(),
            day.get(),
        )
        .fetch_one(&mut *conn)
        .await?;
        if !same_day {
            return Err(Error::Invalid("capacity pool"));
        }
    }
    Ok((max_qty, units))
}

/// Adds a product to the day's range, or replaces its limits.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Final`], or [`Error::Invalid`] for an unknown product,
/// a pool of another day or `units_per_item == 0`.
pub async fn set_product(
    conn: &mut PgConnection,
    day: BakeDayId,
    input: &BakeDayProduct,
) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    let (max_qty, units) = checked_limits(&mut tx, day, input).await?;
    sqlx::query!(
        "INSERT INTO bake_day_product (bake_day_id, product_id, max_qty, capacity_pool_id, units_per_item)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (bake_day_id, product_id) DO UPDATE
        SET max_qty = EXCLUDED.max_qty, capacity_pool_id = EXCLUDED.capacity_pool_id,
            units_per_item = EXCLUDED.units_per_item",
        day.get(),
        input.product_id.get(),
        max_qty,
        input.capacity_pool_id.map(CapacityPoolId::get),
        units,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Changes the maximum of a product already on the day's range. Its capacity pool and
/// units per item stay as they are.
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
    let input = BakeDayProduct {
        product_id: product,
        max_qty,
        capacity_pool_id: None,
        units_per_item: 1,
    };
    let mut tx = conn.begin().await?;
    let (max_qty, _) = checked_limits(&mut tx, day, &input).await?;
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
        "SELECT product_id, max_qty, capacity_pool_id, units_per_item FROM bake_day_product
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
            capacity_pool_id: row.capacity_pool_id.map(CapacityPoolId::new),
            units_per_item: from_db(row.units_per_item)?,
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
            pool: p.capacity_pool_id,
            units_per_item: p.units_per_item,
        })
        .collect();
    let pools = list_pools(conn, day).await?;
    let extras = extras_by_product(conn, day, excluding_extra).await?;
    let mut used: BTreeMap<ProductId, Used> = ordered
        .iter()
        .map(|(&product, &ordered)| (product, Used { ordered, extras: 0 }))
        .collect();
    for (product, qty) in extras {
        used.entry(product).or_default().extras = qty;
    }
    let capacity = Capacity::new(
        limits,
        pools.into_iter().map(|p| PoolLimit {
            id: p.id,
            max_units: p.max_units,
        }),
        used,
    );
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
