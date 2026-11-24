use db::time::to_micros;
use domain::{Clock, OrderId};
use jiff::Timestamp;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::order::{self, CancelReason, OrderStatus};

/// Cancels orders still waiting for payment after their hold, so their pieces go back to
/// the shop. The capacity is already free once the hold passes ([`crate::Orders`]); this
/// makes it visible in the order list and the audit log. A payment that still
/// arrives is refunded ([`crate::mark_paid`], ADR 0012).
///
/// Rows another session holds (a payment being recorded) are skipped and picked up by the
/// next run, so it is safe to run from every server.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn release_expired_holds(
    conn: &mut PgConnection,
    clock: &dyn Clock,
) -> Result<Vec<OrderId>, Error> {
    let now = clock.now();
    let mut tx = conn.begin().await?;
    let expired = sqlx::query_scalar!(
        r#"SELECT id FROM "order"
        WHERE status = 'pending_payment'
            AND hold_expires_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
        ORDER BY id FOR UPDATE SKIP LOCKED"#,
        to_micros(now),
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut released = Vec::with_capacity(expired.len());
    for id in expired.into_iter().map(OrderId::new) {
        let before = order::get(&mut tx, id).await?;
        sqlx::query!(
            r#"UPDATE "order" SET status = 'cancelled', cancel_reason = $2 WHERE id = $1"#,
            id.get(),
            CancelReason::HoldExpired.as_str(),
        )
        .execute(&mut *tx)
        .await?;
        let after = order::get(&mut tx, id).await?;
        audit::record(
            &mut tx,
            &Entry {
                entity: "order",
                entity_id: id.get(),
                action: "hold_expired",
                before: Some(order::snapshot(&before)),
                after: Some(order::snapshot(&after)),
                actor: Actor::System("hold-expiry"),
                at: now,
            },
        )
        .await?;
        released.push(id);
    }
    tx.commit().await?;
    Ok(released)
}

/// Cancels an order still waiting for payment with `reason`, so its hold gives its pieces
/// and slot place back at once. Nothing happens, and `false` comes back, when the order
/// is no longer pending or a payment for it is paid or authorized: that payment decides
/// ([`crate::mark_paid`]). A payment that still completes later is refunded. No email.
///
/// Takes only the order row lock, like [`release_expired_holds`].
///
/// # Errors
/// [`Error::NotFound`], [`Error::Db`] or [`Error::Corrupt`].
pub async fn cancel_pending(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: OrderId,
    reason: CancelReason,
) -> Result<bool, Error> {
    let mut tx = conn.begin().await?;
    let cancelled = cancel_locked(&mut tx, id, reason, None, clock.now()).await?;
    tx.commit().await?;
    Ok(cancelled)
}

/// [`cancel_pending`] inside the caller's transaction, which must already hold the locks
/// that come before the order row (ADR 0008, ADR 0012). `actor` defaults to the order's
/// customer.
pub(crate) async fn cancel_locked(
    conn: &mut PgConnection,
    id: OrderId,
    reason: CancelReason,
    actor: Option<Actor>,
    now: Timestamp,
) -> Result<bool, Error> {
    let status = sqlx::query_scalar!(
        r#"SELECT status FROM "order" WHERE id = $1 FOR UPDATE"#,
        id.get()
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::NotFound)?;
    let settling = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM payment
            WHERE order_id = $1 AND status IN ('paid', 'authorized')) AS "settling!""#,
        id.get()
    )
    .fetch_one(&mut *conn)
    .await?;
    if status != OrderStatus::PendingPayment.as_str() || settling {
        return Ok(false);
    }
    let before = order::get(conn, id).await?;
    sqlx::query!(
        r#"UPDATE "order" SET status = 'cancelled', cancel_reason = $2 WHERE id = $1"#,
        id.get(),
        reason.as_str(),
    )
    .execute(&mut *conn)
    .await?;
    let after = order::get(conn, id).await?;
    audit::record(
        conn,
        &Entry {
            entity: "order",
            entity_id: id.get(),
            action: "cancelled",
            before: Some(order::snapshot(&before)),
            after: Some(order::snapshot(&after)),
            actor: actor.unwrap_or(Actor::Customer(before.customer_id)),
            at: now,
        },
    )
    .await?;
    Ok(true)
}
