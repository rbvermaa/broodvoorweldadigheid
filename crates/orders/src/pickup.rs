use domain::{Clock, OrderId, StaffUserId};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::order::{self, Order, OrderStatus};

const PICKED_UP: &str = "picked_up";
const UNDONE: &str = "picked_up_undone";

/// Ticks an order off at pickup (F8). Only paid or confirmed orders can be picked up; an
/// order already picked up is returned as it is, without an audit entry.
///
/// # Errors
/// [`Error::NotFound`], [`Error::NotPickable`] for any other status, [`Error::Db`] or
/// [`Error::Corrupt`].
pub async fn mark_picked_up(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: OrderId,
    staff: StaffUserId,
) -> Result<Order, Error> {
    let mut tx = conn.begin().await?;
    let before = lock(&mut tx, id).await?;
    match before.status {
        OrderStatus::PickedUp => return Ok(before),
        OrderStatus::Paid | OrderStatus::Confirmed => {}
        other => return Err(Error::NotPickable(other)),
    }
    let after = set_status(
        &mut tx,
        clock,
        &before,
        OrderStatus::PickedUp,
        PICKED_UP,
        staff,
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// Takes back a pickup tick: the order returns to the status it had before. An order
/// that is paid or confirmed is returned as it is, without an audit entry.
///
/// # Errors
/// [`Error::NotFound`], [`Error::NotPickable`] for any other status, [`Error::Db`] or
/// [`Error::Corrupt`] when the audit log has no pickup of the order.
pub async fn undo_picked_up(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: OrderId,
    staff: StaffUserId,
) -> Result<Order, Error> {
    let mut tx = conn.begin().await?;
    let before = lock(&mut tx, id).await?;
    match before.status {
        OrderStatus::PickedUp => {}
        OrderStatus::Paid | OrderStatus::Confirmed => return Ok(before),
        other => return Err(Error::NotPickable(other)),
    }
    let previous = status_before_pickup(&mut tx, id).await?;
    let after = set_status(&mut tx, clock, &before, previous, UNDONE, staff).await?;
    tx.commit().await?;
    Ok(after)
}

async fn lock(conn: &mut PgConnection, id: OrderId) -> Result<Order, Error> {
    sqlx::query_scalar!(
        r#"SELECT id FROM "order" WHERE id = $1 FOR UPDATE"#,
        id.get()
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::NotFound)?;
    order::get(conn, id).await
}

/// The status the latest pickup tick started from, as the audit log recorded it.
async fn status_before_pickup(conn: &mut PgConnection, id: OrderId) -> Result<OrderStatus, Error> {
    let before = sqlx::query_scalar!(
        "SELECT before ->> 'status' FROM audit_log
        WHERE entity = 'order' AND entity_id = $1 AND action = $2
        ORDER BY id DESC LIMIT 1",
        id.get(),
        PICKED_UP,
    )
    .fetch_optional(conn)
    .await?
    .flatten()
    .ok_or_else(|| Error::Corrupt(format!("order {} has no pickup entry", id.get())))?;
    match OrderStatus::parse(&before)? {
        status @ (OrderStatus::Paid | OrderStatus::Confirmed) => Ok(status),
        other => Err(Error::Corrupt(format!("picked up from {other}"))),
    }
}

async fn set_status(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    before: &Order,
    status: OrderStatus,
    action: &str,
    staff: StaffUserId,
) -> Result<Order, Error> {
    sqlx::query!(
        r#"UPDATE "order" SET status = $2 WHERE id = $1"#,
        before.id.get(),
        status.as_str(),
    )
    .execute(&mut *conn)
    .await?;
    let after = order::get(conn, before.id).await?;
    audit::record(
        conn,
        &Entry {
            entity: "order",
            entity_id: before.id.get(),
            action,
            before: Some(order::snapshot(before)),
            after: Some(order::snapshot(&after)),
            actor: Actor::Staff(staff),
            at: clock.now(),
        },
    )
    .await?;
    Ok(after)
}
