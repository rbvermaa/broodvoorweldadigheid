use std::fmt;
use std::str::FromStr;

use domain::{BakeDayId, ProductId};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::orders::OrderSource;
use crate::range::{load_capacity, lock_capacity, lock_editable_day};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductionExtraId(i64);

impl ProductionExtraId {
    #[must_use]
    pub const fn new(id: i64) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

/// Why pieces are baked on top of orders (F7a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purpose {
    Freezer,
    SumUp,
    Samples,
    Other,
}

impl Purpose {
    pub const ALL: [Self; 4] = [Self::Freezer, Self::SumUp, Self::Samples, Self::Other];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Freezer => "freezer",
            Self::SumUp => "sumup",
            Self::Samples => "samples",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for Purpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown extra production purpose: {0}")]
pub struct UnknownPurpose(pub String);

impl FromStr for Purpose {
    type Err = UnknownPurpose;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| UnknownPurpose(s.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraInput {
    pub qty: u32,
    pub purpose: Purpose,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionExtra {
    pub id: ProductionExtraId,
    pub bake_day_id: BakeDayId,
    pub product_id: ProductId,
    pub qty: u32,
    pub purpose: Purpose,
    pub note: Option<String>,
}

struct Row {
    id: i64,
    bake_day_id: i64,
    product_id: i64,
    qty: i32,
    purpose: String,
    note: Option<String>,
}

impl TryFrom<Row> for ProductionExtra {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: ProductionExtraId::new(row.id),
            bake_day_id: BakeDayId::new(row.bake_day_id),
            product_id: ProductId::new(row.product_id),
            qty: u32::try_from(row.qty).map_err(|_| Error::Corrupt(format!("qty {}", row.qty)))?,
            purpose: row
                .purpose
                .parse()
                .map_err(|e: UnknownPurpose| Error::Corrupt(e.to_string()))?,
            note: row.note,
        })
    }
}

fn validate(input: &ExtraInput) -> Result<(i32, Option<&str>), Error> {
    let qty = i32::try_from(input.qty)
        .ok()
        .filter(|&q| q > 0)
        .ok_or(Error::Invalid("quantity"))?;
    let note = input
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    Ok((qty, note))
}

/// Adds extra pieces of a product to a bake day. Extras count against capacity like orders
/// (F3, F7a). Ordered pieces come from `orders`, read after the capacity locks.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Final`] once the day is done or cancelled,
/// [`Error::Invalid`], or [`Error::CapacityExceeded`].
pub async fn add(
    conn: &mut PgConnection,
    day: BakeDayId,
    product: ProductId,
    input: &ExtraInput,
    orders: &impl OrderSource,
) -> Result<ProductionExtra, Error> {
    let (qty, note) = validate(input)?;
    let mut tx = conn.begin().await?;
    lock_editable_day(&mut tx, day).await?;
    lock_capacity(&mut tx, day, &[product]).await?;
    load_capacity(&mut tx, day, orders, None)
        .await?
        .check(product, input.qty)?;
    let row = sqlx::query_as!(
        Row,
        "INSERT INTO production_extra (bake_day_id, product_id, qty, purpose, note)
        SELECT $1, id, $3, $4, $5 FROM product WHERE id = $2
        RETURNING id, bake_day_id, product_id, qty, purpose, note",
        day.get(),
        product.get(),
        qty,
        input.purpose.as_str(),
        note,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::Invalid("product"))?;
    tx.commit().await?;
    row.try_into()
}

async fn locate(
    conn: &mut PgConnection,
    id: ProductionExtraId,
) -> Result<(BakeDayId, ProductId, u32), Error> {
    let row = sqlx::query!(
        "SELECT bake_day_id, product_id, qty FROM production_extra WHERE id = $1",
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?;
    let qty = u32::try_from(row.qty).map_err(|_| Error::Corrupt(format!("qty {}", row.qty)))?;
    Ok((
        BakeDayId::new(row.bake_day_id),
        ProductId::new(row.product_id),
        qty,
    ))
}

/// Changes the quantity, purpose or note. Lowering the quantity is always allowed;
/// raising it must fit the remaining capacity.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Final`], [`Error::Invalid`] or
/// [`Error::CapacityExceeded`].
pub async fn update(
    conn: &mut PgConnection,
    id: ProductionExtraId,
    input: &ExtraInput,
    orders: &impl OrderSource,
) -> Result<ProductionExtra, Error> {
    let (qty, note) = validate(input)?;
    let mut tx = conn.begin().await?;
    let (day, product, _) = locate(&mut tx, id).await?;
    lock_editable_day(&mut tx, day).await?;
    lock_capacity(&mut tx, day, &[product]).await?;
    let (_, _, current) = locate(&mut tx, id).await?;
    if input.qty > current {
        load_capacity(&mut tx, day, orders, Some(id))
            .await?
            .check(product, input.qty)?;
    }
    let row = sqlx::query_as!(
        Row,
        "UPDATE production_extra SET qty = $2, purpose = $3, note = $4 WHERE id = $1
        RETURNING id, bake_day_id, product_id, qty, purpose, note",
        id.get(),
        qty,
        input.purpose.as_str(),
        note,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    row.try_into()
}

/// # Errors
/// [`Error::NotFound`] or [`Error::Final`].
pub async fn remove(conn: &mut PgConnection, id: ProductionExtraId) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    let (day, _, _) = locate(&mut tx, id).await?;
    lock_editable_day(&mut tx, day).await?;
    sqlx::query!("DELETE FROM production_extra WHERE id = $1", id.get())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A bake day's extra production, by product and id.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list(conn: &mut PgConnection, day: BakeDayId) -> Result<Vec<ProductionExtra>, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, bake_day_id, product_id, qty, purpose, note FROM production_extra
        WHERE bake_day_id = $1 ORDER BY product_id, id",
        day.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(ProductionExtra::try_from)
    .collect()
}
