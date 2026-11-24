use bakedays::{BakeDayStatus, range};
use db::time::{from_micros, to_micros};
use domain::{BakeDayId, Clock, OrderId, PickupSlotId, StaffUserId, in_bakery_time};
use jiff::{SignedDuration, Timestamp};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::cart::{self, Cart, PricedLine};
use crate::customer::{self, CustomerInput};
use crate::holds;
use crate::order::{self, CancelReason, Channel, Order, OrderStatus, PaymentMode, vat_to_db};
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

/// Locks the pickup slot after the capacity locks and returns its order limit.
pub(crate) async fn lock_slot(
    conn: &mut PgConnection,
    day: BakeDayId,
    slot: PickupSlotId,
) -> Result<Option<i32>, Error> {
    sqlx::query_scalar!(
        "SELECT max_orders FROM pickup_slot WHERE id = $1 AND bake_day_id = $2 FOR UPDATE",
        slot.get(),
        day.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::SlotNotOnDay)
}

/// Checks the slot's order limit, after [`lock_slot`]. `except` leaves out an order that
/// is being paid late.
pub(crate) async fn ensure_slot_room(
    conn: &mut PgConnection,
    slot: PickupSlotId,
    max_orders: Option<i32>,
    now: Timestamp,
    except: Option<OrderId>,
) -> Result<(), Error> {
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
/// [`Error::CapacityExceeded`]; [`Error::FreeOrder`] when the total is zero, since
/// Mollie cannot take a payment of €0; [`Error::Invalid`] for customer details.
pub async fn checkout(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    cart: &Cart,
    customer: &CustomerInput,
    slot: PickupSlotId,
    ordered_by: OrderedBy,
) -> Result<Order, Error> {
    place(
        conn,
        clock,
        cart,
        customer,
        slot,
        ordered_by,
        Mode::Online(None),
    )
    .await
}

/// [`checkout`] that first cancels `replaces`, a pending order of the same bake day, as
/// replaced by the customer (F5, ADR 0015). Both happen in one transaction: when the new
/// order is refused for any reason, the old one stays as it was. The old order's
/// products and slot are locked with the new ones, in the usual order, before its row.
///
/// # Errors
/// Those of [`checkout`], and [`Error::NotReplaceable`] when `replaces` is no longer
/// pending, has a payment that is paid or authorized, or belongs to another bake day.
pub async fn checkout_replacing(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    cart: &Cart,
    customer: &CustomerInput,
    slot: PickupSlotId,
    ordered_by: OrderedBy,
    replaces: OrderId,
) -> Result<Order, Error> {
    place(
        conn,
        clock,
        cart,
        customer,
        slot,
        ordered_by,
        Mode::Online(Some(replaces)),
    )
    .await
}

/// A staff order on account for a business customer (F11a): confirmed straight away,
/// without payment or hold. Pay-what-you-want lines take the suggested price (F4b).
/// `customer` must carry the email of a business account that may order on account; that
/// account is read `FOR SHARE` after the customer lock.
///
/// # Errors
/// Those of [`checkout`], and [`Error::NotOnAccount`] for a customer without a business
/// account that may order on account.
pub async fn checkout_on_account(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    cart: &Cart,
    customer: &CustomerInput,
    slot: PickupSlotId,
    staff: StaffUserId,
) -> Result<Order, Error> {
    let mut suggested = Cart::new(cart.bake_day());
    for (product, line) in cart.lines() {
        suggested.set(product, line.qty, None)?;
    }
    place(
        conn,
        clock,
        &suggested,
        customer,
        slot,
        OrderedBy::Staff(staff),
        Mode::OnAccount,
    )
    .await
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    /// Waits for payment within its hold; replaces the given pending order, if any.
    Online(Option<OrderId>),
    OnAccount,
}

async fn place(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    cart: &Cart,
    customer: &CustomerInput,
    slot: PickupSlotId,
    ordered_by: OrderedBy,
    mode: Mode,
) -> Result<Order, Error> {
    let now = clock.now();
    let channel = ordered_by.channel();
    let day = cart.bake_day();
    if cart.is_empty() {
        return Err(cart::CartError::Empty.into());
    }
    let details = customer::validate(customer)?;
    let mut tx = conn.begin().await?;
    let facts = lock_day(&mut tx, day).await?;
    ensure_orderable(&facts, channel, now)?;
    let products = cart.products();
    let replaces = match mode {
        Mode::Online(replaces) => replaces,
        Mode::OnAccount => None,
    };
    let replaced = match replaces {
        Some(id) => Some(order::get(&mut tx, id).await?),
        None => None,
    };
    let mut locked = products.clone();
    if let Some(old) = &replaced {
        if old.bake_day_id != day {
            return Err(Error::NotReplaceable);
        }
        locked.extend(old.lines.iter().map(|line| line.product_id));
        locked.sort_unstable();
        locked.dedup();
    }
    range::lock_capacity(&mut tx, day, &locked).await?;
    if let Some(old) = &replaced {
        let mut slots = vec![old.pickup_slot_id, slot];
        slots.sort_unstable();
        slots.dedup();
        for s in slots {
            lock_slot(&mut tx, day, s).await?;
        }
        let actor = match ordered_by {
            OrderedBy::Guest => None,
            OrderedBy::Staff(staff) => Some(Actor::Staff(staff)),
        };
        let cancelled = holds::cancel_locked(
            &mut tx,
            old.id,
            CancelReason::ReplacedByCustomer,
            actor,
            now,
        )
        .await?;
        if !cancelled {
            return Err(Error::NotReplaceable);
        }
    }
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
    let total = cart::totals(&lines)?.gross()?;
    if total <= domain::Money::ZERO {
        return Err(Error::FreeOrder);
    }
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
    let max_orders = lock_slot(&mut tx, day, slot).await?;
    ensure_slot_room(&mut tx, slot, max_orders, clock.now(), None).await?;
    let customer_id = customer::find_or_create(&mut tx, &details, now).await?;
    let (payment_mode, status, hold_expires_at) = match mode {
        Mode::Online(_) => (
            PaymentMode::Online,
            OrderStatus::PendingPayment,
            Some(now.checked_add(HOLD).map_err(|_| Error::Invalid("time"))?),
        ),
        Mode::OnAccount => {
            let allowed = sqlx::query_scalar!(
                "SELECT on_account_allowed FROM business_account WHERE customer_id = $1
                FOR SHARE",
                customer_id.get(),
            )
            .fetch_optional(&mut *tx)
            .await?;
            if allowed != Some(true) {
                return Err(Error::NotOnAccount);
            }
            (PaymentMode::OnAccount, OrderStatus::Confirmed, None)
        }
    };
    let (year, seq) = next_number(&mut tx, now).await?;
    let id = sqlx::query_scalar!(
        r#"INSERT INTO "order" (number_year, number_seq, customer_id, customer_name,
            customer_phone, marketing_opt_in, bake_day_id, pickup_slot_id, channel,
            payment_mode, status, total_cents, hold_expires_at, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
            timestamptz 'epoch' + $13::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $14::bigint * interval '1 microsecond')
        RETURNING id"#,
        year,
        seq,
        customer_id.get(),
        details.name,
        details.phone,
        customer.marketing_opt_in,
        day.get(),
        slot.get(),
        channel.as_str(),
        payment_mode.as_str(),
        status.as_str(),
        total.cents(),
        hold_expires_at.map(to_micros),
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

/// Whether checking out `cart` for `customer` and `slot` now would place `order` again:
/// the same lines at the same prices, email, name, phone, pickup slot and marketing wish.
/// Details or a cart that checkout would refuse count as different. A display read
/// without locks; checkout decides under its own.
///
/// # Errors
/// [`Error::Db`], [`Error::Catalog`] or [`Error::BakeDays`].
pub async fn same_as_order(
    conn: &mut PgConnection,
    order: &Order,
    cart: &Cart,
    customer: &CustomerInput,
    slot: PickupSlotId,
) -> Result<bool, Error> {
    let Ok(details) = customer::validate(customer) else {
        return Ok(false);
    };
    if order.bake_day_id != cart.bake_day()
        || order.pickup_slot_id != slot
        || order.customer_name != details.name
        || order.customer_phone != details.phone
        || order.marketing_opt_in != customer.marketing_opt_in
    {
        return Ok(false);
    }
    let email = sqlx::query_scalar!(
        "SELECT email FROM customer WHERE id = $1",
        order.customer_id.get()
    )
    .fetch_one(&mut *conn)
    .await?;
    if email != details.email {
        return Ok(false);
    }
    let mut catalog = Vec::new();
    for id in cart.products() {
        match catalog::products::get(&mut *conn, id).await {
            Ok(product) => catalog.push(product),
            Err(catalog::Error::NotFound) => {}
            Err(err) => return Err(err.into()),
        }
    }
    let on_range = range::list_products(&mut *conn, cart.bake_day()).await?;
    let Ok(lines) = cart.price(&catalog, &on_range) else {
        return Ok(false);
    };
    Ok(lines.len() == order.lines.len()
        && lines.iter().all(|line| {
            order.lines.iter().any(|placed| {
                placed.product_id == line.product_id
                    && placed.qty == line.qty
                    && placed.unit_price == line.unit_price
            })
        }))
}

/// Orders that take a place in the slot at `now`: paid, confirmed, picked up, or pending
/// within their hold. A display read without locks, for the pickup picker; checkout checks
/// again under the slot lock.
///
/// # Errors
/// [`Error::Db`].
pub async fn slot_orders(
    conn: &mut PgConnection,
    slot: PickupSlotId,
    now: Timestamp,
) -> Result<i64, Error> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM "order"
        WHERE pickup_slot_id = $1
            AND (status IN ('paid', 'confirmed', 'picked_up')
                OR (status = 'pending_payment'
                    AND hold_expires_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'))"#,
        slot.get(),
        to_micros(now),
    )
    .fetch_one(conn)
    .await?)
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
