use bakedays::range;
use db::time::to_micros;
use domain::{BakeDayId, CharityId, Clock, DonationAllocationId, Money, OrderId, PickupSlotId};
use jiff::Timestamp;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::checkout::{lock_day, lock_slot};
use crate::order::{self, CancelReason, Channel, Order, OrderStatus};
use crate::source::Orders;

/// A completed payment, as the payment provider reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentReceived {
    /// The provider's payment id, e.g. Mollie's `tr_...`.
    pub reference: String,
    pub amount: Money,
}

/// Money earmarked for a charity by one paid order (F4c, F11b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DonationAllocation {
    pub id: DonationAllocationId,
    pub charity_id: CharityId,
    pub amount: Money,
}

/// Why money that came in must go back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefundReason {
    /// The hold expired and the pieces are gone.
    SoldOut,
    /// The hold expired and the deadline has passed (web orders only).
    PastDeadline,
    /// The bake day is cancelled or done.
    DayFinal,
    /// The order was cancelled for another reason than an expired hold.
    OrderCancelled,
    /// The order is already paid with another payment.
    PaidTwice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaidOutcome {
    /// The order is paid. `allocation` is `None` on a drop without charity.
    Paid {
        order: Order,
        allocation: Option<DonationAllocation>,
    },
    /// The same payment was reported before; nothing changed.
    AlreadyPaid(Order),
    /// The order does not count; the money must be refunded. The order stays (or becomes)
    /// cancelled and keeps the payment reference ([`Order::refund_due`]).
    RefundDue { order: Order, reason: RefundReason },
}

const ACTOR: Actor = Actor::System("payment");

fn decide(
    order: &Order,
    day: &crate::checkout::DayFacts,
    now: Timestamp,
) -> Result<Option<RefundReason>, NeedsCapacity> {
    let revivable = match order.status {
        OrderStatus::PendingPayment => true,
        OrderStatus::Cancelled => {
            order.cancel_reason == Some(CancelReason::HoldExpired) && order.payment_ref.is_none()
        }
        _ => false,
    };
    if !revivable {
        return Ok(Some(RefundReason::OrderCancelled));
    }
    if day.status.is_final() {
        return Ok(Some(RefundReason::DayFinal));
    }
    let within_hold = order.status == OrderStatus::PendingPayment
        && order.hold_expires_at.is_some_and(|hold| now < hold);
    if within_hold {
        return Ok(None);
    }
    if order.channel == Channel::Web && day.cutoff_at.is_none_or(|cutoff| now >= cutoff) {
        return Ok(Some(RefundReason::PastDeadline));
    }
    Err(NeedsCapacity)
}

/// The hold expired before the deadline: the order counts only if it still fits.
struct NeedsCapacity;

/// Records a completed payment (F5): pending payment to paid, with a donation allocation
/// of the full amount for the drop's charity, or none on a drop without charity (F4c).
///
/// A payment within the hold always counts. A payment after the hold counts if the
/// deadline has not passed (web orders) and the pieces and the pickup slot still fit;
/// otherwise the order is cancelled and the money is due back (ADR 0012). Locks in the
/// order of ADR 0008: bake day, products and pools, slot, then the order row.
///
/// # Errors
/// [`Error::NotFound`], or [`Error::AmountMismatch`] when the amount differs from the
/// order total (nothing changes then).
pub async fn mark_paid(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: OrderId,
    payment: &PaymentReceived,
) -> Result<PaidOutcome, Error> {
    let now = clock.now();
    let mut tx = conn.begin().await?;
    let placed = sqlx::query!(
        r#"SELECT bake_day_id, pickup_slot_id FROM "order" WHERE id = $1"#,
        id.get(),
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    let day_id = BakeDayId::new(placed.bake_day_id);
    let day = lock_day(&mut tx, day_id).await?;
    let products: Vec<_> = order::get(&mut tx, id)
        .await?
        .lines
        .iter()
        .map(|l| l.product_id)
        .collect();
    range::lock_capacity(&mut tx, day_id, &products).await?;
    sqlx::query_scalar!(
        r#"SELECT id FROM "order" WHERE id = $1 FOR UPDATE"#,
        id.get()
    )
    .fetch_one(&mut *tx)
    .await?;
    let before = order::get(&mut tx, id).await?;

    if before.payment_ref.as_deref() == Some(payment.reference.as_str()) {
        tx.commit().await?;
        return Ok(if before.refund_due() {
            PaidOutcome::RefundDue {
                reason: RefundReason::OrderCancelled,
                order: before,
            }
        } else {
            PaidOutcome::AlreadyPaid(before)
        });
    }
    if before.paid_at.is_some() || before.refund_due() {
        record(&mut tx, &before, &before, "second_payment", payment, now).await?;
        tx.commit().await?;
        let reason = if before.paid_at.is_some() {
            RefundReason::PaidTwice
        } else {
            RefundReason::OrderCancelled
        };
        return Ok(PaidOutcome::RefundDue {
            order: before,
            reason,
        });
    }
    if payment.amount != before.total {
        return Err(Error::AmountMismatch {
            expected: before.total,
            paid: payment.amount,
        });
    }

    let refusal = match decide(&before, &day, now) {
        Ok(refusal) => refusal,
        Err(NeedsCapacity) => {
            let request: Vec<_> = before.lines.iter().map(|l| (l.product_id, l.qty)).collect();
            match range::check_order(&mut tx, day_id, &Orders::new(clock), &request).await {
                Ok(()) => {
                    match lock_slot(
                        &mut tx,
                        day_id,
                        PickupSlotId::new(placed.pickup_slot_id),
                        now,
                        Some(id),
                    )
                    .await
                    {
                        Ok(()) => None,
                        Err(Error::SlotFull) => Some(RefundReason::SoldOut),
                        Err(err) => return Err(err),
                    }
                }
                Err(bakedays::Error::CapacityExceeded { .. }) => Some(RefundReason::SoldOut),
                Err(err) => return Err(err.into()),
            }
        }
    };

    let outcome = if refusal == Some(RefundReason::OrderCancelled)
        && before.status != OrderStatus::Cancelled
    {
        record(&mut tx, &before, &before, "second_payment", payment, now).await?;
        PaidOutcome::RefundDue {
            order: before,
            reason: RefundReason::OrderCancelled,
        }
    } else if let Some(reason) = refusal {
        let cancel_reason = before.cancel_reason.unwrap_or(match reason {
            RefundReason::DayFinal => CancelReason::DayCancelled,
            _ => CancelReason::HoldExpired,
        });
        sqlx::query!(
            r#"UPDATE "order" SET status = 'cancelled', cancel_reason = $2, payment_ref = $3
            WHERE id = $1"#,
            id.get(),
            cancel_reason.as_str(),
            payment.reference,
        )
        .execute(&mut *tx)
        .await?;
        let after = order::get(&mut tx, id).await?;
        record(&mut tx, &before, &after, "refund_due", payment, now).await?;
        PaidOutcome::RefundDue {
            order: after,
            reason,
        }
    } else {
        sqlx::query!(
            r#"UPDATE "order" SET status = 'paid', cancel_reason = NULL, payment_ref = $2,
                paid_at = timestamptz 'epoch' + $3::bigint * interval '1 microsecond'
            WHERE id = $1"#,
            id.get(),
            payment.reference,
            to_micros(now),
        )
        .execute(&mut *tx)
        .await?;
        let allocation = match day.charity_id.map(CharityId::new) {
            Some(charity_id) => Some(allocate(&mut tx, id, charity_id, payment.amount, now).await?),
            None => None,
        };
        let after = order::get(&mut tx, id).await?;
        record(&mut tx, &before, &after, "paid", payment, now).await?;
        PaidOutcome::Paid {
            order: after,
            allocation,
        }
    };
    tx.commit().await?;
    Ok(outcome)
}

async fn allocate(
    conn: &mut PgConnection,
    order: OrderId,
    charity_id: CharityId,
    amount: Money,
    now: Timestamp,
) -> Result<DonationAllocation, Error> {
    let id = sqlx::query_scalar!(
        "INSERT INTO donation_allocation (order_id, charity_id, amount_cents, status, created_at)
        VALUES ($1, $2, $3, 'earmarked', timestamptz 'epoch' + $4::bigint * interval '1 microsecond')
        RETURNING id",
        order.get(),
        charity_id.get(),
        amount.cents(),
        to_micros(now),
    )
    .fetch_one(&mut *conn)
    .await?;
    let allocation = DonationAllocation {
        id: DonationAllocationId::new(id),
        charity_id,
        amount,
    };
    audit::record(
        conn,
        &Entry {
            entity: "donation_allocation",
            entity_id: id,
            action: "earmarked",
            before: None,
            after: Some(serde_json::json!({
                "order_id": order.get(),
                "charity_id": charity_id.get(),
                "amount_cents": amount.cents(),
                "status": "earmarked",
            })),
            actor: ACTOR,
            at: now,
        },
    )
    .await?;
    Ok(allocation)
}

async fn record(
    conn: &mut PgConnection,
    before: &Order,
    after: &Order,
    action: &str,
    payment: &PaymentReceived,
    now: Timestamp,
) -> Result<(), Error> {
    let mut after = order::snapshot(after);
    after["payment"] = serde_json::json!({
        "reference": payment.reference,
        "amount_cents": payment.amount.cents(),
    });
    audit::record(
        conn,
        &Entry {
            entity: "order",
            entity_id: before.id.get(),
            action,
            before: Some(order::snapshot(before)),
            after: Some(after),
            actor: ACTOR,
            at: now,
        },
    )
    .await
}

/// The donation allocations of an order, oldest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn allocations(
    conn: &mut PgConnection,
    order: OrderId,
) -> Result<Vec<DonationAllocation>, Error> {
    sqlx::query!(
        r#"SELECT id, charity_id AS "charity_id!", amount_cents FROM donation_allocation
        WHERE order_id = $1 AND charity_id IS NOT NULL ORDER BY id"#,
        order.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(DonationAllocation {
            id: DonationAllocationId::new(row.id),
            charity_id: CharityId::new(row.charity_id),
            amount: Money::from_cents(row.amount_cents),
        })
    })
    .collect()
}
