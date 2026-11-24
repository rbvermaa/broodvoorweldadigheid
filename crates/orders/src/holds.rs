use db::time::to_micros;
use domain::{Clock, OrderId};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::order::{self, CancelReason};

/// Cancels orders still waiting for payment after their hold, so their pieces go back to
/// the shop. The capacity is already free once the hold passes ([`crate::Orders`]); this
/// makes it visible in the order list and the audit log. A payment that still arrives
/// goes through [`crate::mark_paid`] (ADR 0012).
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
