use db::time::{from_epoch_days, from_micros, to_micros};
use domain::OrderId;
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::PgConnection;

use crate::Error;
use crate::order::{self, Order};

/// The ophaalmoment of an order and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickupDetails {
    pub starts: Timestamp,
    pub ends: Timestamp,
    pub location: String,
    pub address: String,
}

/// What the customer emails of F9 say about an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderDetails {
    pub order: Order,
    pub email: String,
    pub drop_date: Date,
    pub pickup: PickupDetails,
    /// The product name of each of `order.lines`, in the same order.
    pub products: Vec<String>,
    /// The charity the order's money is earmarked for; `None` for a normal sale.
    pub charity: Option<String>,
    pub status_token: String,
}

/// An order with everything its emails show.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Db`] or [`Error::Corrupt`].
pub async fn details(conn: &mut PgConnection, id: OrderId) -> Result<OrderDetails, Error> {
    let order = order::get(conn, id).await?;
    let row = sqlx::query!(
        r#"SELECT c.email, d.date - date 'epoch' AS "date_days!",
            (extract(epoch FROM s.starts_at) * 1000000)::bigint AS "starts_us!",
            (extract(epoch FROM s.ends_at) * 1000000)::bigint AS "ends_us!",
            l.name AS location, l.address, o.status_token::text AS "status_token!"
        FROM "order" o
        JOIN customer c ON c.id = o.customer_id
        JOIN bake_day d ON d.id = o.bake_day_id
        JOIN pickup_slot s ON s.id = o.pickup_slot_id
        JOIN pickup_location l ON l.id = s.location_id
        WHERE o.id = $1"#,
        id.get(),
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::NotFound)?;
    let names = sqlx::query!(
        "SELECT ol.id, p.name_nl FROM order_line ol JOIN product p ON p.id = ol.product_id
        WHERE ol.order_id = $1",
        id.get(),
    )
    .fetch_all(&mut *conn)
    .await?;
    let products = order
        .lines
        .iter()
        .map(|line| {
            names
                .iter()
                .find(|name| name.id == line.id.get())
                .map(|name| name.name_nl.clone())
                .ok_or_else(|| Error::Corrupt(format!("order line {}", line.id.get())))
        })
        .collect::<Result<_, _>>()?;
    let charity = sqlx::query_scalar!(
        "SELECT c.name FROM donation_allocation a JOIN charity c ON c.id = a.charity_id
        WHERE a.order_id = $1 ORDER BY a.id LIMIT 1",
        id.get(),
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(OrderDetails {
        order,
        email: row.email,
        drop_date: from_epoch_days(row.date_days)?,
        pickup: PickupDetails {
            starts: from_micros(row.starts_us)?,
            ends: from_micros(row.ends_us)?,
            location: row.location,
            address: row.address,
        },
        products,
        charity,
        status_token: row.status_token,
    })
}

/// Paid or confirmed orders on a bake day that is not cancelled, whose pickup starts after
/// `after` and no later than `until`, with that start, by pickup start.
///
/// # Errors
/// [`Error::Db`].
pub async fn awaiting_pickup(
    conn: &mut PgConnection,
    after: Timestamp,
    until: Timestamp,
) -> Result<Vec<(OrderId, Timestamp)>, Error> {
    let rows = sqlx::query!(
        r#"SELECT o.id, (extract(epoch FROM s.starts_at) * 1000000)::bigint AS "starts_us!"
        FROM "order" o
        JOIN pickup_slot s ON s.id = o.pickup_slot_id
        JOIN bake_day d ON d.id = o.bake_day_id
        WHERE o.status IN ('paid', 'confirmed') AND d.status <> 'cancelled'
            AND s.starts_at > timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
            AND s.starts_at <= timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
        ORDER BY s.starts_at, o.id"#,
        to_micros(after),
        to_micros(until),
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|row| Ok((OrderId::new(row.id), from_micros(row.starts_us)?)))
        .collect()
}

/// Locks the order's bake day `FOR SHARE`, then the order row, as ADR 0012 orders them,
/// and returns its pickup start if the order still awaits pickup: paid or confirmed, on a
/// bake day that is not cancelled. Call it in the transaction that acts on the answer, so
/// a cancel or a slot change cannot slip in between.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn lock_awaiting_pickup(
    conn: &mut PgConnection,
    id: OrderId,
) -> Result<Option<Timestamp>, Error> {
    let day = sqlx::query_scalar!(
        r#"SELECT d.status FROM bake_day d JOIN "order" o ON o.bake_day_id = d.id
        WHERE o.id = $1 FOR SHARE OF d"#,
        id.get(),
    )
    .fetch_optional(&mut *conn)
    .await?;
    if day.is_none_or(|status| status == "cancelled") {
        return Ok(None);
    }
    let row = sqlx::query!(
        r#"SELECT o.status, (extract(epoch FROM s.starts_at) * 1000000)::bigint AS "starts_us!"
        FROM "order" o JOIN pickup_slot s ON s.id = o.pickup_slot_id
        WHERE o.id = $1 FOR SHARE OF o"#,
        id.get(),
    )
    .fetch_optional(conn)
    .await?;
    match row {
        Some(row) if row.status == "paid" || row.status == "confirmed" => {
            Ok(Some(from_micros(row.starts_us)?))
        }
        _ => Ok(None),
    }
}
