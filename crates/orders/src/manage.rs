//! Back-office order management (F6): the order list of a bake day, moving the
//! ophaalmoment, marking picked up and cancelling. Every change writes an audit entry
//! with the staff member as actor, in the same transaction.

use db::time::from_micros;
use domain::{BakeDayId, Clock, Money, OrderId, PickupLocationId, PickupSlotId, StaffUserId};
use jiff::Timestamp;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::checkout::{ensure_slot_room, lock_day, lock_slot};
use crate::holds;
use crate::order::{self, CancelReason, Order, OrderStatus};
use crate::payment::{Payment, PaymentRow, payment_from_row};

/// Which orders of a bake day to list; `None` means any.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrderFilter {
    pub status: Option<OrderStatus>,
    pub location: Option<PickupLocationId>,
}

/// One row of the order list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayOrder {
    pub id: OrderId,
    pub number: order::OrderNumber,
    pub customer_name: String,
    /// The customer was erased (F12); `customer_name` is then a placeholder.
    pub customer_erased: bool,
    pub email: String,
    pub status: OrderStatus,
    pub cancel_reason: Option<CancelReason>,
    pub total: Money,
    pub pieces: u32,
    pub pickup_slot_id: PickupSlotId,
    pub pickup_starts: Timestamp,
    pub pickup_ends: Timestamp,
    pub location: String,
}

/// The orders of a bake day that match `filter`, by ophaalmoment, then order number.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn day_orders(
    conn: &mut PgConnection,
    day: BakeDayId,
    filter: &OrderFilter,
) -> Result<Vec<DayOrder>, Error> {
    let rows = sqlx::query!(
        r#"SELECT o.id, o.number_year, o.number_seq, o.customer_name,
            c.erased_at IS NOT NULL AS "customer_erased!", c.email, o.status,
            o.cancel_reason, o.total_cents, o.pickup_slot_id,
            (SELECT coalesce(sum(qty), 0) FROM order_line WHERE order_id = o.id)::bigint
                AS "pieces!",
            (extract(epoch FROM s.starts_at) * 1000000)::bigint AS "starts_us!",
            (extract(epoch FROM s.ends_at) * 1000000)::bigint AS "ends_us!",
            l.name AS location
        FROM "order" o
        JOIN customer c ON c.id = o.customer_id
        JOIN pickup_slot s ON s.id = o.pickup_slot_id
        JOIN pickup_location l ON l.id = s.location_id
        WHERE o.bake_day_id = $1
            AND ($2::text IS NULL OR o.status = $2)
            AND ($3::bigint IS NULL OR s.location_id = $3)
        ORDER BY s.starts_at, o.number_year, o.number_seq"#,
        day.get(),
        filter.status.map(OrderStatus::as_str),
        filter.location.map(PickupLocationId::get),
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(DayOrder {
                id: OrderId::new(row.id),
                number: order::OrderNumber {
                    year: row.number_year,
                    seq: row.number_seq,
                },
                customer_name: row.customer_name,
                customer_erased: row.customer_erased,
                email: row.email,
                status: OrderStatus::parse(&row.status)?,
                cancel_reason: row
                    .cancel_reason
                    .as_deref()
                    .map(CancelReason::parse)
                    .transpose()?,
                total: Money::from_cents(row.total_cents),
                pieces: u32::try_from(row.pieces)
                    .map_err(|_| Error::Corrupt(format!("pieces {}", row.pieces)))?,
                pickup_slot_id: PickupSlotId::new(row.pickup_slot_id),
                pickup_starts: from_micros(row.starts_us)?,
                pickup_ends: from_micros(row.ends_us)?,
                location: row.location,
            })
        })
        .collect()
}

/// Every payment recorded for an order, oldest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn payments(conn: &mut PgConnection, id: OrderId) -> Result<Vec<Payment>, Error> {
    sqlx::query_as!(
        PaymentRow,
        r#"SELECT id, order_id, provider_payment_id, amount_cents, refunded_cents, refund_reason,
            (extract(epoch FROM received_at) * 1000000)::bigint AS "received_at_us!"
        FROM payment WHERE order_id = $1 AND status = 'paid' ORDER BY received_at, id"#,
        id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(payment_from_row)
    .collect()
}

/// Whether the order is linked to a manual invoice; then only picking up may change it
/// (F11a).
///
/// # Errors
/// [`Error::NotFound`] or [`Error::Db`].
pub async fn invoiced(conn: &mut PgConnection, id: OrderId) -> Result<bool, Error> {
    sqlx::query_scalar!(
        r#"SELECT manual_invoice_id IS NOT NULL AS "invoiced!" FROM "order" WHERE id = $1"#,
        id.get()
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)
}

/// The order's bake day, locked `FOR SHARE` before the order row (ADR 0012).
async fn day_of(conn: &mut PgConnection, id: OrderId) -> Result<BakeDayId, Error> {
    sqlx::query_scalar!(r#"SELECT bake_day_id FROM "order" WHERE id = $1"#, id.get())
        .fetch_optional(conn)
        .await?
        .map(BakeDayId::new)
        .ok_or(Error::NotFound)
}

async fn lock_order(conn: &mut PgConnection, id: OrderId) -> Result<Order, Error> {
    sqlx::query_scalar!(
        r#"SELECT id FROM "order" WHERE id = $1 FOR UPDATE"#,
        id.get()
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::NotFound)?;
    order::get(conn, id).await
}

const fn awaits_pickup(status: OrderStatus) -> bool {
    matches!(status, OrderStatus::Paid | OrderStatus::Confirmed)
}

/// Moves a paid or confirmed order to another pickup slot of its bake day, if that slot
/// has room. Staff may do this after the deadline too: the slot changes no quantities.
/// Moving to the slot the order already has changes nothing and writes no entry.
///
/// Locks the bake day, both slots by id, then the order (ADR 0012).
///
/// # Errors
/// [`Error::NotFound`]; [`Error::DayFinal`] when the bake day is done or cancelled;
/// [`Error::WrongStatus`] when the order is not paid or confirmed;
/// [`Error::Invoiced`]; [`Error::SlotNotOnDay`]; [`Error::SlotFull`].
pub async fn move_pickup(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: OrderId,
    slot: PickupSlotId,
    staff: StaffUserId,
) -> Result<Order, Error> {
    let mut tx = conn.begin().await?;
    let day = day_of(&mut tx, id).await?;
    let facts = lock_day(&mut tx, day).await?;
    let current = order::get(&mut tx, id).await?;
    let mut slots = vec![current.pickup_slot_id, slot];
    slots.sort_unstable();
    slots.dedup();
    let mut max_orders = None;
    for s in slots {
        let max = lock_slot(&mut tx, day, s).await?;
        if s == slot {
            max_orders = max;
        }
    }
    let before = lock_order(&mut tx, id).await?;
    if facts.status.is_final() {
        return Err(Error::DayFinal);
    }
    if !awaits_pickup(before.status) {
        return Err(Error::WrongStatus(before.status));
    }
    if invoiced(&mut tx, id).await? {
        return Err(Error::Invoiced);
    }
    if before.pickup_slot_id == slot {
        tx.commit().await?;
        return Ok(before);
    }
    let now = clock.now();
    ensure_slot_room(&mut tx, slot, max_orders, now, Some(id)).await?;
    sqlx::query!(
        r#"UPDATE "order" SET pickup_slot_id = $2 WHERE id = $1"#,
        id.get(),
        slot.get(),
    )
    .execute(&mut *tx)
    .await?;
    let after = order::get(&mut tx, id).await?;
    audit::record(
        &mut tx,
        &Entry {
            entity: "order",
            entity_id: id.get(),
            action: "pickup_moved",
            before: Some(serde_json::json!({ "pickup_slot_id": before.pickup_slot_id.get() })),
            after: Some(serde_json::json!({ "pickup_slot_id": after.pickup_slot_id.get() })),
            actor: Actor::Staff(staff),
            at: now,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// Marks a paid or confirmed order picked up (F6, F8). Not on a cancelled bake day.
///
/// # Errors
/// [`Error::NotFound`]; [`Error::DayFinal`] when the bake day is cancelled;
/// [`Error::WrongStatus`] when the order is not paid or confirmed.
pub async fn mark_picked_up(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: OrderId,
    staff: StaffUserId,
) -> Result<Order, Error> {
    let mut tx = conn.begin().await?;
    let day = day_of(&mut tx, id).await?;
    let facts = lock_day(&mut tx, day).await?;
    let before = lock_order(&mut tx, id).await?;
    if facts.status == bakedays::BakeDayStatus::Cancelled {
        return Err(Error::DayFinal);
    }
    if !awaits_pickup(before.status) {
        return Err(Error::WrongStatus(before.status));
    }
    sqlx::query!(
        r#"UPDATE "order" SET status = 'picked_up' WHERE id = $1"#,
        id.get()
    )
    .execute(&mut *tx)
    .await?;
    let after = order::get(&mut tx, id).await?;
    audit::record(
        &mut tx,
        &Entry {
            entity: "order",
            entity_id: id.get(),
            action: "picked_up",
            before: Some(order::snapshot(&before)),
            after: Some(order::snapshot(&after)),
            actor: Actor::Staff(staff),
            at: clock.now(),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// Cancels an order that waits for payment, for staff (F6), like
/// [`crate::cancel_pending`] with the staff member as actor. Its pieces and slot place go
/// back at once. A paid order is refused: cancelling it needs a refund, and the donation
/// rules need that refund settled first (ADR 0025). Runs in a savepoint when `conn` is a
/// transaction, so the caller can queue the cancellation email in the same one.
///
/// # Errors
/// [`Error::NotFound`]; [`Error::WrongStatus`] for an order that is not pending, or
/// pending with a payment that is completing.
pub async fn cancel_by_staff(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: OrderId,
    staff: StaffUserId,
) -> Result<Order, Error> {
    let mut tx = conn.begin().await?;
    let day = day_of(&mut tx, id).await?;
    lock_day(&mut tx, day).await?;
    let before = lock_order(&mut tx, id).await?;
    if before.status != OrderStatus::PendingPayment
        || !holds::cancel_locked(
            &mut tx,
            id,
            CancelReason::Staff,
            Some(Actor::Staff(staff)),
            clock.now(),
        )
        .await?
    {
        return Err(Error::WrongStatus(before.status));
    }
    let order = order::get(&mut tx, id).await?;
    tx.commit().await?;
    Ok(order)
}
