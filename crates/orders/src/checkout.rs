use bakedays::{BakeDayStatus, range};
use db::time::{from_micros, to_micros};
use domain::{BakeDayId, Clock, OrderId, PickupSlotId, StaffUserId, in_bakery_time};
use jiff::{SignedDuration, Timestamp};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::cart::{self, Cart, PricedLine};
use crate::customer::{self, CustomerInput};
use crate::order::{self, Channel, Order, OrderStatus, PaymentMode, vat_to_db};
use crate::source::Orders;

/// How long an unpaid order holds its capacity (spec, "Data model").
pub const HOLD: SignedDuration = SignedDuration::from_mins(15);

/// Who places the order: a guest in the web shop, or staff (F6). Only staff can order
/// after the deadline, and still only within capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderedBy {
    Guest,
    Staff(StaffUserId),
}

impl OrderedBy {
    const fn channel(self) -> Channel {
        match self {
            Self::Guest => Channel::Web,
            Self::Staff(_) => Channel::Manual,
        }
    }
}

pub(crate) struct DayFacts {
    pub status: BakeDayStatus,
    pub online_ordering: bool,
    pub cutoff_at: Option<Timestamp>,
    pub charity_id: Option<i64>,
}

/// Step 1 of ADR 0008: the bake day row `FOR SHARE`, so its status, deadline and charity
/// cannot change until the transaction ends.
pub(crate) async fn lock_day(conn: &mut PgConnection, day: BakeDayId) -> Result<DayFacts, Error> {
    let row = sqlx::query!(
        r#"SELECT status, online_ordering, charity_id,
            (extract(epoch FROM cutoff_at) * 1000000)::bigint AS cutoff_at_us
        FROM bake_day WHERE id = $1 FOR SHARE"#,
        day.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(DayFacts {
        status: row
            .status
            .parse()
            .map_err(|e: bakedays::status::UnknownStatus| Error::Corrupt(e.to_string()))?,
        online_ordering: row.online_ordering,
        cutoff_at: row.cutoff_at_us.map(from_micros).transpose()?,
        charity_id: row.charity_id,
    })
}

/// Whether an order placed at `now` may be taken (F1, F3; ADR 0011: the stored status can
/// lag the deadline by a tick, so the deadline is checked here).
fn ensure_orderable(day: &DayFacts, channel: Channel, now: Timestamp) -> Result<(), Error> {
    if !day.online_ordering {
        return Err(Error::ProductionOnly);
    }
    if day.status.is_final() {
        return Err(Error::DayFinal);
    }
    if channel == Channel::Web {
        if day.status != BakeDayStatus::Open {
            return Err(Error::NotOpen);
        }
        if day.cutoff_at.is_none_or(|cutoff| now >= cutoff) {
            return Err(Error::PastDeadline);
        }
    }
    Ok(())
}

/// Locks the pickup slot after the capacity locks and checks its order limit. `except`
/// leaves out an order that is being paid late.
pub(crate) async fn lock_slot(
    conn: &mut PgConnection,
    day: BakeDayId,
    slot: PickupSlotId,
    now: Timestamp,
    except: Option<OrderId>,
) -> Result<(), Error> {
    let max_orders = sqlx::query_scalar!(
        "SELECT max_orders FROM pickup_slot WHERE id = $1 AND bake_day_id = $2 FOR UPDATE",
        slot.get(),
        day.get(),
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::SlotNotOnDay)?;
    let Some(max_orders) = max_orders else {
        return Ok(());
    };
    let taken = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM "order"
        WHERE pickup_slot_id = $1 AND id IS DISTINCT FROM $3
            AND (status IN ('paid', 'confirmed', 'picked_up')
                OR (status = 'pending_payment'
                    AND hold_expires_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'))"#,
        slot.get(),
        to_micros(now),
        except.map(OrderId::get),
    )
    .fetch_one(conn)
    .await?;
    if taken >= i64::from(max_orders) {
        return Err(Error::SlotFull);
    }
    Ok(())
}

async fn next_number(conn: &mut PgConnection, now: Timestamp) -> Result<(i16, i32), Error> {
    let year = in_bakery_time(now).year();
    let seq = sqlx::query_scalar!(
        "INSERT INTO order_number (year, last) VALUES ($1, 1)
        ON CONFLICT (year) DO UPDATE SET last = order_number.last + 1
        RETURNING last",
        year,
    )
    .fetch_one(conn)
    .await?;
    Ok((year, seq))
}

async fn insert_lines(
    conn: &mut PgConnection,
    order: i64,
    lines: &[PricedLine],
) -> Result<(), Error> {
    for line in lines {
        sqlx::query!(
            "INSERT INTO order_line
                (order_id, product_id, qty, unit_price_cents, suggested_price_cents, vat_rate)
            VALUES ($1, $2, $3, $4, $5, $6)",
            order,
            line.product_id.get(),
            i32::try_from(line.qty).map_err(|_| Error::Invalid("quantity"))?,
            line.unit_price.cents(),
            line.suggested_price.map(domain::Money::cents),
            vat_to_db(line.vat_rate),
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Turns a cart into an order waiting for payment, holding its capacity for [`HOLD`]
/// (F3, F4, F4b). One transaction, locking in the order of ADR 0008 and ADR 0012: the
/// bake day `FOR SHARE`, the products and pools ([`range::lock_capacity`]), the pickup
/// slot, the customer, the year's order number. Orders are read after those locks.
///
/// Lines copy the price and BTW rate of this moment; a pay-what-you-want price is checked
/// here, never trusted from the browser.
///
/// # Errors
/// [`Error::NotFound`] for an unknown bake day; [`Error::ProductionOnly`],
/// [`Error::DayFinal`], [`Error::NotOpen`] or [`Error::PastDeadline`] when the day takes
/// no orders from this channel; [`Error::SlotNotOnDay`], [`Error::SlotFull`];
/// [`Error::Cart`] for a product that is not for sale or a refused price;
/// [`Error::CapacityExceeded`]; [`Error::Invalid`] for customer details.
pub async fn checkout(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    cart: &Cart,
    customer: &CustomerInput,
    slot: PickupSlotId,
    ordered_by: OrderedBy,
) -> Result<Order, Error> {
    let now = clock.now();
    let channel = ordered_by.channel();
    let day = cart.bake_day();
    if cart.is_empty() {
        return Err(cart::CartError::Empty.into());
    }
    let mut tx = conn.begin().await?;
    let facts = lock_day(&mut tx, day).await?;
    ensure_orderable(&facts, channel, now)?;
    let products = cart.products();
    range::lock_capacity(&mut tx, day, &products).await?;
    let mut catalog = Vec::with_capacity(products.len());
    for &id in &products {
        match catalog::products::get(&mut tx, id).await {
            Ok(product) => catalog.push(product),
            Err(catalog::Error::NotFound) => {}
            Err(err) => return Err(err.into()),
        }
    }
    let on_range = range::list_products(&mut tx, day).await?;
    let lines = cart.price(&catalog, &on_range)?;
    range::check_order(
        &mut tx,
        day,
        &Orders::new(clock),
        &lines
            .iter()
            .map(|l| (l.product_id, l.qty))
            .collect::<Vec<_>>(),
    )
    .await?;
    lock_slot(&mut tx, day, slot, now, None).await?;
    let customer_id = customer::upsert(&mut tx, customer, now).await?;
    let total = cart::totals(&lines)?.gross()?;
    let (year, seq) = next_number(&mut tx, now).await?;
    let hold_expires_at = now.checked_add(HOLD).map_err(|_| Error::Invalid("time"))?;
    let id = sqlx::query_scalar!(
        r#"INSERT INTO "order" (number_year, number_seq, customer_id, bake_day_id, pickup_slot_id,
            channel, payment_mode, status, total_cents, hold_expires_at, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
            timestamptz 'epoch' + $10::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $11::bigint * interval '1 microsecond')
        RETURNING id"#,
        year,
        seq,
        customer_id.get(),
        day.get(),
        slot.get(),
        channel.as_str(),
        PaymentMode::Online.as_str(),
        OrderStatus::PendingPayment.as_str(),
        total.cents(),
        to_micros(hold_expires_at),
        to_micros(now),
    )
    .fetch_one(&mut *tx)
    .await?;
    insert_lines(&mut tx, id, &lines).await?;
    let order = order::get(&mut tx, OrderId::new(id)).await?;
    let actor = match ordered_by {
        OrderedBy::Guest => Actor::Customer(customer_id),
        OrderedBy::Staff(staff) => Actor::Staff(staff),
    };
    audit::record(
        &mut tx,
        &Entry {
            entity: "order",
            entity_id: id,
            action: "created",
            before: None,
            after: Some(order::snapshot(&order)),
            actor,
            at: now,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(status: BakeDayStatus, online: bool, cutoff: Option<&str>) -> DayFacts {
        DayFacts {
            status,
            online_ordering: online,
            cutoff_at: cutoff.map(|c| c.parse().unwrap()),
            charity_id: None,
        }
    }

    #[test]
    fn web_orders_need_an_open_day_before_the_deadline() {
        let now: Timestamp = "2026-10-29T18:59:59Z".parse().unwrap();
        let cutoff = Some("2026-10-29T19:00:00Z");
        let open = facts(BakeDayStatus::Open, true, cutoff);
        assert!(ensure_orderable(&open, Channel::Web, now).is_ok());
        let at_cutoff = "2026-10-29T19:00:00Z".parse().unwrap();
        assert!(matches!(
            ensure_orderable(&open, Channel::Web, at_cutoff),
            Err(Error::PastDeadline)
        ));
        assert!(ensure_orderable(&open, Channel::Manual, at_cutoff).is_ok());
        assert!(matches!(
            ensure_orderable(
                &facts(BakeDayStatus::Draft, true, cutoff),
                Channel::Web,
                now
            ),
            Err(Error::NotOpen)
        ));
        assert!(matches!(
            ensure_orderable(&facts(BakeDayStatus::Open, true, None), Channel::Web, now),
            Err(Error::PastDeadline)
        ));
        let closed = facts(BakeDayStatus::Closed, true, cutoff);
        assert!(matches!(
            ensure_orderable(&closed, Channel::Web, now),
            Err(Error::NotOpen)
        ));
        assert!(ensure_orderable(&closed, Channel::Manual, at_cutoff).is_ok());
        for status in [BakeDayStatus::Cancelled, BakeDayStatus::Done] {
            assert!(matches!(
                ensure_orderable(&facts(status, true, cutoff), Channel::Manual, now),
                Err(Error::DayFinal)
            ));
        }
        assert!(matches!(
            ensure_orderable(
                &facts(BakeDayStatus::Closed, false, None),
                Channel::Manual,
                now
            ),
            Err(Error::ProductionOnly)
        ));
    }
}
