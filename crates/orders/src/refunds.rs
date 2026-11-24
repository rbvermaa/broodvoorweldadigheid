//! Refunds through the payment provider (F5, F6) and the sick-day cancel of a whole bake
//! day (F2). Cancelling records the refunds as rows in one transaction; [`issue`] sends
//! them to the provider afterwards. See ADR 0028.

use db::time::{from_micros, to_micros};
use domain::{
    BakeDayId, Clock, IdempotencyKey, Money, OrderId, PaymentId, PaymentProvider,
    ProviderPaymentId, RefundId, RefundRequest, RefundStatus, StaffUserId,
};
use jiff::Timestamp;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::checkout::lock_day;
use crate::order::{self, CancelReason, Order, OrderNumber, OrderStatus};

/// Where a refund stands. Reports subtract a refund in every state (ADR 0028).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefundState {
    /// Recorded; not yet accepted by the provider.
    Requested,
    /// Accepted by the provider.
    Done,
    /// Refused, or no answer; staff can retry.
    Failed,
}

impl RefundState {
    const ALL: [Self; 3] = [Self::Requested, Self::Done, Self::Failed];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Result<Self, Error> {
        Self::ALL
            .into_iter()
            .find(|s| s.as_str() == text)
            .ok_or_else(|| Error::Corrupt(format!("refund status {text:?}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refund {
    pub id: RefundId,
    pub payment_id: PaymentId,
    pub provider_payment_id: String,
    pub order_id: OrderId,
    pub order_number: OrderNumber,
    pub amount: Money,
    pub state: RefundState,
    pub provider_refund_id: Option<String>,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub requested_by: String,
    pub requested_at: Timestamp,
}

struct RefundRow {
    id: i64,
    payment_id: i64,
    provider_payment_id: String,
    order_id: i64,
    number_year: i16,
    number_seq: i32,
    amount_cents: i64,
    status: String,
    provider_refund_id: Option<String>,
    attempts: i32,
    last_error: Option<String>,
    requested_by: String,
    requested_at_us: i64,
}

fn refund_from_row(row: RefundRow) -> Result<Refund, Error> {
    Ok(Refund {
        id: RefundId::new(row.id),
        payment_id: PaymentId::new(row.payment_id),
        provider_payment_id: row.provider_payment_id,
        order_id: OrderId::new(row.order_id),
        order_number: OrderNumber {
            year: row.number_year,
            seq: row.number_seq,
        },
        amount: Money::from_cents(row.amount_cents),
        state: RefundState::parse(&row.status)?,
        provider_refund_id: row.provider_refund_id,
        attempts: row.attempts,
        last_error: row.last_error,
        requested_by: row.requested_by,
        requested_at: from_micros(row.requested_at_us)?,
    })
}

/// The refunds of an order, oldest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn of_order(conn: &mut PgConnection, order: OrderId) -> Result<Vec<Refund>, Error> {
    sqlx::query_as!(
        RefundRow,
        r#"SELECT r.id, r.payment_id, p.provider_payment_id, p.order_id, o.number_year,
            o.number_seq, r.amount_cents, r.status, r.provider_refund_id, r.attempts,
            r.last_error, r.requested_by,
            (extract(epoch FROM r.requested_at) * 1000000)::bigint AS "requested_at_us!"
        FROM refund r
        JOIN payment p ON p.id = r.payment_id
        JOIN "order" o ON o.id = p.order_id
        WHERE p.order_id = $1 ORDER BY r.id"#,
        order.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(refund_from_row)
    .collect()
}

/// The refunds of a bake day's orders that are not done yet, oldest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn open_of_day(conn: &mut PgConnection, day: BakeDayId) -> Result<Vec<Refund>, Error> {
    sqlx::query_as!(
        RefundRow,
        r#"SELECT r.id, r.payment_id, p.provider_payment_id, p.order_id, o.number_year,
            o.number_seq, r.amount_cents, r.status, r.provider_refund_id, r.attempts,
            r.last_error, r.requested_by,
            (extract(epoch FROM r.requested_at) * 1000000)::bigint AS "requested_at_us!"
        FROM refund r
        JOIN payment p ON p.id = r.payment_id
        JOIN "order" o ON o.id = p.order_id
        WHERE o.bake_day_id = $1 AND r.status <> 'done' ORDER BY r.id"#,
        day.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(refund_from_row)
    .collect()
}

async fn get(conn: &mut PgConnection, id: RefundId, lock: bool) -> Result<Refund, Error> {
    if lock {
        sqlx::query_scalar!("SELECT id FROM refund WHERE id = $1 FOR UPDATE", id.get())
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(Error::NotFound)?;
    }
    sqlx::query_as!(
        RefundRow,
        r#"SELECT r.id, r.payment_id, p.provider_payment_id, p.order_id, o.number_year,
            o.number_seq, r.amount_cents, r.status, r.provider_refund_id, r.attempts,
            r.last_error, r.requested_by,
            (extract(epoch FROM r.requested_at) * 1000000)::bigint AS "requested_at_us!"
        FROM refund r
        JOIN payment p ON p.id = r.payment_id
        JOIN "order" o ON o.id = p.order_id
        WHERE r.id = $1"#,
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)
    .and_then(refund_from_row)
}

/// Adds a `requested` refund for what is left of every payment the order accepted.
async fn request_refunds(
    conn: &mut PgConnection,
    order: OrderId,
    actor: Actor,
    now: Timestamp,
) -> Result<Vec<RefundId>, Error> {
    let ids = sqlx::query_scalar!(
        r#"INSERT INTO refund (payment_id, amount_cents, refunded_before_cents, status,
            requested_by, requested_at, updated_at)
        SELECT p.id, p.amount_cents - payment_refunded_cents(p.id), payment_refunded_cents(p.id),
            'requested', $2, timestamptz 'epoch' + $3::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $3::bigint * interval '1 microsecond'
        FROM payment p
        WHERE p.order_id = $1 AND p.status = 'paid' AND NOT p.refund_due
            AND p.amount_cents > payment_refunded_cents(p.id)
        ORDER BY p.id
        RETURNING id"#,
        order.get(),
        actor.to_db(),
        to_micros(now),
    )
    .fetch_all(conn)
    .await?;
    Ok(ids.into_iter().map(RefundId::new).collect())
}

/// The order's refunds as audit data.
async fn refunds_json(conn: &mut PgConnection, order: OrderId) -> Result<serde_json::Value, Error> {
    let refunds = of_order(conn, order).await?;
    Ok(refunds
        .iter()
        .map(|r| serde_json::json!({ "id": r.id.get(), "amount_cents": r.amount.cents() }))
        .collect())
}

/// Cancels `order` (already locked) with `reason` and requests its refunds, with one audit
/// entry. Returns the new refunds.
async fn cancel_locked_with_refunds(
    conn: &mut PgConnection,
    before: &Order,
    reason: CancelReason,
    actor: Actor,
    now: Timestamp,
) -> Result<Vec<RefundId>, Error> {
    sqlx::query!(
        r#"UPDATE "order" SET status = 'cancelled', cancel_reason = $2 WHERE id = $1"#,
        before.id.get(),
        reason.as_str(),
    )
    .execute(&mut *conn)
    .await?;
    let refunds = request_refunds(conn, before.id, actor, now).await?;
    let after = order::get(conn, before.id).await?;
    let mut snapshot = order::snapshot(&after);
    snapshot["refunds"] = refunds_json(conn, before.id).await?;
    audit::record(
        conn,
        &Entry {
            entity: "order",
            entity_id: before.id.get(),
            action: "cancelled",
            before: Some(order::snapshot(before)),
            after: Some(snapshot),
            actor,
            at: now,
        },
    )
    .await?;
    Ok(refunds)
}

/// A cancelled order with the refunds the cancel requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cancelled {
    pub order: Order,
    pub refunds: Vec<Refund>,
}

impl Cancelled {
    /// What goes back to the customer; `None` when nothing was paid.
    #[must_use]
    pub fn refunded(&self) -> Option<Money> {
        let cents: i64 = self.refunds.iter().map(|r| r.amount.cents()).sum();
        (cents > 0).then(|| Money::from_cents(cents))
    }
}

/// Cancels a paid or confirmed order for staff (F6) and records a `requested` refund for
/// every payment it accepted; an on-account order gets none. Send the refunds with
/// [`issue`] after the commit. Runs in a savepoint when `conn` is a transaction, so the
/// caller can queue the cancellation email in the same one. A second call finds the order
/// cancelled and changes nothing.
///
/// # Errors
/// [`Error::NotFound`]; [`Error::DayFinal`] when the bake day is done or cancelled;
/// [`Error::WrongStatus`] when the order is not paid or confirmed; [`Error::Invoiced`].
pub async fn cancel_paid(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: OrderId,
    staff: StaffUserId,
) -> Result<Cancelled, Error> {
    let mut tx = conn.begin().await?;
    let day = sqlx::query_scalar!(r#"SELECT bake_day_id FROM "order" WHERE id = $1"#, id.get())
        .fetch_optional(&mut *tx)
        .await?
        .map(BakeDayId::new)
        .ok_or(Error::NotFound)?;
    let facts = lock_day(&mut tx, day).await?;
    let before = lock_order(&mut tx, id).await?;
    if !matches!(before.status, OrderStatus::Paid | OrderStatus::Confirmed) {
        return Err(Error::WrongStatus(before.status));
    }
    if facts.status.is_final() {
        return Err(Error::DayFinal);
    }
    if invoiced(&mut tx, id).await? {
        return Err(Error::Invoiced);
    }
    let now = clock.now();
    cancel_locked_with_refunds(
        &mut tx,
        &before,
        CancelReason::Staff,
        Actor::Staff(staff),
        now,
    )
    .await?;
    let order = order::get(&mut tx, id).await?;
    let refunds = of_order(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Cancelled { order, refunds })
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

async fn invoiced(conn: &mut PgConnection, id: OrderId) -> Result<bool, Error> {
    Ok(sqlx::query_scalar!(
        r#"SELECT manual_invoice_id IS NOT NULL AS "invoiced!" FROM "order" WHERE id = $1"#,
        id.get()
    )
    .fetch_one(conn)
    .await?)
}

/// Sends one refund to the provider, holding its row `FOR UPDATE`, so two calls never send
/// it twice. A done refund is returned as it is. Before every call the payment is read
/// from the provider: when it already shows the refund, the row is marked done without a
/// new call, and when it cannot be read nothing is sent and the attempt fails. The idempotency key is `refund-<id>-<attempt>`. Each outcome writes an audit entry
/// on the order. `description` is what the customer sees on the refund.
///
/// # Errors
/// [`Error::NotFound`] or [`Error::Db`]; a provider error is recorded on the refund, not
/// returned.
pub async fn issue(
    conn: &mut PgConnection,
    provider: &dyn PaymentProvider,
    clock: &dyn Clock,
    id: RefundId,
    description: &str,
) -> Result<Refund, Error> {
    let mut tx = conn.begin().await?;
    // The row stays locked during the provider call; a server-wide idle timeout must not
    // end the session after the provider accepted the refund.
    sqlx::query!("SET LOCAL idle_in_transaction_session_timeout = 0")
        .execute(&mut *tx)
        .await?;
    let refund = get(&mut tx, id, true).await?;
    if refund.state == RefundState::Done {
        tx.commit().await?;
        return Ok(refund);
    }
    let attempt = refund.attempts + 1;
    let payment = ProviderPaymentId::new(refund.provider_payment_id.clone())
        .map_err(|e| Error::Corrupt(e.to_string()))?;
    let outcome = match already_refunded(&mut tx, provider, &refund, &payment).await? {
        Ok(true) => Ok(None),
        Ok(false) => send(provider, &refund, &payment, attempt, description).await,
        Err(error) => Err(error),
    };
    let (state, provider_id, error) = settle(&refund, outcome);
    let now = clock.now();
    sqlx::query!(
        "UPDATE refund SET status = $2, provider_refund_id = $3, attempts = $4, last_error = $5,
            updated_at = timestamptz 'epoch' + $6::bigint * interval '1 microsecond'
        WHERE id = $1",
        id.get(),
        state.as_str(),
        provider_id,
        attempt,
        error,
        to_micros(now),
    )
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        &Entry {
            entity: "order",
            entity_id: refund.order_id.get(),
            action: match state {
                RefundState::Done => "refund_done",
                _ => "refund_failed",
            },
            before: None,
            after: Some(serde_json::json!({
                "refund_id": id.get(),
                "amount_cents": refund.amount.cents(),
                "provider_refund_id": provider_id,
                "attempt": attempt,
                "error": error,
            })),
            actor: Actor::System("refund"),
            at: now,
        },
    )
    .await?;
    let refund = get(&mut tx, id, false).await?;
    tx.commit().await?;
    Ok(refund)
}

/// Whether the provider already refunded this refund: it reports at least what was
/// refunded before this refund plus this refund.
async fn already_refunded(
    conn: &mut PgConnection,
    provider: &dyn PaymentProvider,
    refund: &Refund,
    payment: &ProviderPaymentId,
) -> Result<Result<bool, String>, Error> {
    let snapshot = match provider.get_payment(payment).await {
        Ok(snapshot) => snapshot,
        Err(error) => return Ok(Err(error.to_string())),
    };
    let before = sqlx::query_scalar!(
        "SELECT refunded_before_cents FROM refund WHERE id = $1",
        refund.id.get(),
    )
    .fetch_one(conn)
    .await?;
    Ok(Ok(
        snapshot.amount_refunded.cents() >= before + refund.amount.cents()
    ))
}

type Settled = (RefundState, Option<String>, Option<String>);

/// The refund's new status, provider id and error after an attempt: `Ok(None)` when the
/// provider already showed the refund, `Ok(Some(..))` with what the provider answered.
fn settle(refund: &Refund, outcome: Result<Option<(String, RefundStatus)>, String>) -> Settled {
    match outcome {
        Ok(None) => (RefundState::Done, refund.provider_refund_id.clone(), None),
        Ok(Some((provider_id, RefundStatus::Failed | RefundStatus::Canceled))) => (
            RefundState::Failed,
            Some(provider_id),
            Some("the provider failed the refund".to_owned()),
        ),
        Ok(Some((provider_id, _))) => (RefundState::Done, Some(provider_id), None),
        Err(error) => (
            RefundState::Failed,
            refund.provider_refund_id.clone(),
            Some(error),
        ),
    }
}

async fn send(
    provider: &dyn PaymentProvider,
    refund: &Refund,
    payment: &ProviderPaymentId,
    attempt: i32,
    description: &str,
) -> Result<Option<(String, RefundStatus)>, String> {
    let key = IdempotencyKey::new(format!("refund-{}-{attempt}", refund.id.get()))
        .map_err(|e| e.to_string())?;
    provider
        .refund(&RefundRequest {
            payment: payment.clone(),
            amount: refund.amount,
            description: description.to_owned(),
            idempotency_key: key,
        })
        .await
        .map(|created| Some((created.id.as_str().to_owned(), created.status)))
        .map_err(|e| e.to_string())
}

/// What a sick-day cancel did. Lists the whole day, also what an earlier run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayCancelled {
    pub message: String,
    /// Every order cancelled because of the day, with what goes back to its customer.
    pub orders: Vec<(OrderId, Option<Money>)>,
    /// Invoiced on-account orders, left as they are; their customers get the email too.
    pub invoiced: Vec<(OrderId, OrderNumber)>,
    /// Refunds of the day's orders that still have to be sent.
    pub open_refunds: Vec<Refund>,
}

/// Cancels a bake day for a sick day (F2): the day gets `message`, every order that is
/// pending, paid or confirmed is cancelled with reason `day_cancelled`, and every payment
/// those orders accepted gets a `requested` refund. Send the refunds with [`issue`] after
/// the commit. Picked-up orders and invoiced on-account orders stay as they are.
///
/// Safe to run again on a cancelled day: the first message stays, nothing is cancelled or
/// refunded twice, and the result lists the refunds still open. Runs in a savepoint when
/// `conn` is a transaction, so the caller can queue the emails in the same one.
///
/// Locks the bake day `FOR UPDATE`, then the orders by id.
///
/// # Errors
/// [`Error::NotFound`]; [`Error::Invalid`] for an empty message on a day not cancelled
/// yet; [`Error::DayFinal`] when the day is done.
pub async fn cancel_day(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    day: BakeDayId,
    message: &str,
    staff: StaffUserId,
) -> Result<DayCancelled, Error> {
    let mut tx = conn.begin().await?;
    let row = sqlx::query!(
        "SELECT status, cancel_message FROM bake_day WHERE id = $1 FOR UPDATE",
        day.get()
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    let message = match (row.status.as_str(), row.cancel_message) {
        ("cancelled", Some(kept)) => kept,
        ("done", _) => return Err(Error::DayFinal),
        _ => {
            if message.trim().is_empty() {
                return Err(Error::Invalid("cancel message"));
            }
            bakedays::days::cancel(&mut tx, clock, day, message)
                .await?
                .cancel_message
                .unwrap_or_default()
        }
    };
    let ids = sqlx::query_scalar!(
        r#"SELECT id FROM "order" WHERE bake_day_id = $1
            AND status IN ('pending_payment', 'paid', 'confirmed')
        ORDER BY id FOR UPDATE"#,
        day.get(),
    )
    .fetch_all(&mut *tx)
    .await?;
    let now = clock.now();
    let actor = Actor::Staff(staff);
    let mut invoiced_orders = Vec::new();
    for id in ids.into_iter().map(OrderId::new) {
        let before = order::get(&mut tx, id).await?;
        if invoiced(&mut tx, id).await? {
            invoiced_orders.push((id, before.number));
            continue;
        }
        cancel_locked_with_refunds(&mut tx, &before, CancelReason::DayCancelled, actor, now)
            .await?;
    }
    let orders = sqlx::query!(
        r#"SELECT o.id, (SELECT sum(r.amount_cents) FROM refund r JOIN payment p
                ON p.id = r.payment_id WHERE p.order_id = o.id)::bigint AS refunded
        FROM "order" o
        WHERE o.bake_day_id = $1 AND o.status = 'cancelled' AND o.cancel_reason = 'day_cancelled'
        ORDER BY o.id"#,
        day.get(),
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|row| {
        (
            OrderId::new(row.id),
            row.refunded.filter(|c| *c > 0).map(Money::from_cents),
        )
    })
    .collect();
    let open_refunds = open_of_day(&mut tx, day).await?;
    tx.commit().await?;
    Ok(DayCancelled {
        message,
        orders,
        invoiced: invoiced_orders,
        open_refunds,
    })
}

/// What a sick-day cancel of a bake day does, for the confirmation text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayPreview {
    /// Orders that would be cancelled.
    pub orders: u32,
    /// Those of them that get a refund.
    pub refunds: u32,
    /// What goes back in total.
    pub refund_total: Money,
    /// Invoiced on-account orders, which stay as they are.
    pub invoiced: Vec<OrderNumber>,
}

/// What [`cancel_day`] would do to `day` now.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn day_preview(conn: &mut PgConnection, day: BakeDayId) -> Result<DayPreview, Error> {
    let rows = sqlx::query!(
        r#"SELECT o.number_year, o.number_seq, o.manual_invoice_id IS NOT NULL AS "invoiced!",
            COALESCE((SELECT sum(p.amount_cents - payment_refunded_cents(p.id))
                FROM payment p
                WHERE p.order_id = o.id AND p.status = 'paid' AND NOT p.refund_due), 0)::bigint
                AS "refund!"
        FROM "order" o
        WHERE o.bake_day_id = $1 AND o.status IN ('pending_payment', 'paid', 'confirmed')
        ORDER BY o.id"#,
        day.get(),
    )
    .fetch_all(conn)
    .await?;
    let mut preview = DayPreview {
        orders: 0,
        refunds: 0,
        refund_total: Money::ZERO,
        invoiced: Vec::new(),
    };
    for row in rows {
        if row.invoiced {
            preview.invoiced.push(OrderNumber {
                year: row.number_year,
                seq: row.number_seq,
            });
            continue;
        }
        preview.orders += 1;
        if row.refund > 0 {
            preview.refunds += 1;
            preview.refund_total = preview
                .refund_total
                .checked_add(Money::from_cents(row.refund))?;
        }
    }
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refund(provider_refund_id: Option<&str>) -> Refund {
        Refund {
            id: RefundId::new(1),
            payment_id: PaymentId::new(1),
            provider_payment_id: "tr_1".to_owned(),
            order_id: OrderId::new(1),
            order_number: OrderNumber { year: 2026, seq: 1 },
            amount: Money::from_cents(450),
            state: RefundState::Failed,
            provider_refund_id: provider_refund_id.map(str::to_owned),
            attempts: 1,
            last_error: None,
            requested_by: "staff:1".to_owned(),
            requested_at: Timestamp::UNIX_EPOCH,
        }
    }

    #[test]
    fn states_round_trip_through_text() {
        for state in RefundState::ALL {
            assert_eq!(RefundState::parse(state.as_str()).unwrap(), state);
        }
        assert!(matches!(RefundState::parse("gone"), Err(Error::Corrupt(_))));
    }

    #[test]
    fn what_the_provider_answers_settles_the_refund() {
        let earlier = refund(Some("re_1"));
        assert_eq!(
            settle(&earlier, Ok(None)),
            (RefundState::Done, Some("re_1".to_owned()), None)
        );
        for status in [
            RefundStatus::Queued,
            RefundStatus::Pending,
            RefundStatus::Processing,
            RefundStatus::Refunded,
        ] {
            assert_eq!(
                settle(&refund(None), Ok(Some(("re_2".to_owned(), status)))),
                (RefundState::Done, Some("re_2".to_owned()), None)
            );
        }
        for status in [RefundStatus::Failed, RefundStatus::Canceled] {
            let (state, id, error) = settle(&refund(None), Ok(Some(("re_3".to_owned(), status))));
            assert_eq!((state, id.as_deref()), (RefundState::Failed, Some("re_3")));
            assert!(error.is_some());
        }
        assert_eq!(
            settle(&earlier, Err("down".to_owned())),
            (
                RefundState::Failed,
                Some("re_1".to_owned()),
                Some("down".to_owned())
            )
        );
    }
}
