use bakedays::range;
use db::time::{from_micros, to_micros};
use domain::{
    BakeDayId, CharityId, Clock, DonationAllocationId, Money, OrderId, PaymentId, PickupSlotId,
};
use jiff::Timestamp;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::checkout::{DayFacts, lock_day, lock_slot};
use crate::order::{self, CancelReason, Order, OrderStatus};

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

/// Why money that came in must go back (ADR 0012).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefundReason {
    /// The payment completed after the order's hold (spec, "Data model").
    HoldExpired,
    /// The bake day is cancelled or done.
    DayFinal,
    /// The order was cancelled before the payment came in.
    OrderCancelled,
    /// The order is confirmed, picked up or refunded without this payment.
    OrderConfirmed,
    /// The order is already paid with another payment.
    PaidTwice,
    /// The amount differs from the order total.
    AmountMismatch,
}

impl RefundReason {
    const ALL: [Self; 6] = [
        Self::HoldExpired,
        Self::DayFinal,
        Self::OrderCancelled,
        Self::OrderConfirmed,
        Self::PaidTwice,
        Self::AmountMismatch,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HoldExpired => "hold_expired",
            Self::DayFinal => "day_final",
            Self::OrderCancelled => "order_cancelled",
            Self::OrderConfirmed => "order_confirmed",
            Self::PaidTwice => "paid_twice",
            Self::AmountMismatch => "amount_mismatch",
        }
    }

    fn parse(text: &str) -> Result<Self, Error> {
        Self::ALL
            .into_iter()
            .find(|r| r.as_str() == text)
            .ok_or_else(|| Error::Corrupt(format!("refund reason {text:?}")))
    }
}

/// A payment as recorded. `refund_reason` is set when the money must go back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payment {
    pub id: PaymentId,
    pub order_id: OrderId,
    pub provider_payment_id: String,
    pub amount: Money,
    pub refunded: Money,
    pub refund_reason: Option<RefundReason>,
    pub received_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaidOutcome {
    /// The order is paid. `allocation` is `None` on a drop without charity.
    Paid {
        order: Order,
        allocation: Option<DonationAllocation>,
    },
    /// This payment was reported before and accepted; nothing changed.
    AlreadyPaid(Order),
    /// The payment is recorded but the order cannot take it; the money must go back
    /// ([`refunds_due`]). A repeated report returns the same reason and changes nothing.
    RefundDue {
        order: Order,
        payment: Payment,
        reason: RefundReason,
    },
}

const ACTOR: Actor = Actor::System("payment");

/// Whether `order` can take a payment of `amount` at `now`; `None` means it can.
fn refusal(order: &Order, day: &DayFacts, amount: Money, now: Timestamp) -> Option<RefundReason> {
    if order.paid_at.is_some() {
        return Some(RefundReason::PaidTwice);
    }
    match order.status {
        OrderStatus::PendingPayment => {}
        OrderStatus::Cancelled => {
            return Some(match order.cancel_reason {
                Some(CancelReason::HoldExpired) => RefundReason::HoldExpired,
                Some(CancelReason::DayCancelled) => RefundReason::DayFinal,
                _ => RefundReason::OrderCancelled,
            });
        }
        _ => return Some(RefundReason::OrderConfirmed),
    }
    if amount != order.total {
        Some(RefundReason::AmountMismatch)
    } else if day.status.is_final() {
        Some(RefundReason::DayFinal)
    } else if order.hold_expires_at.is_none_or(|hold| now >= hold) {
        Some(RefundReason::HoldExpired)
    } else {
        None
    }
}

struct PaymentRow {
    id: i64,
    order_id: i64,
    provider_payment_id: String,
    amount_cents: i64,
    refunded_cents: i64,
    refund_reason: Option<String>,
    received_at_us: i64,
}

fn payment_from_row(row: PaymentRow) -> Result<Payment, Error> {
    Ok(Payment {
        id: PaymentId::new(row.id),
        order_id: OrderId::new(row.order_id),
        provider_payment_id: row.provider_payment_id,
        amount: Money::from_cents(row.amount_cents),
        refunded: Money::from_cents(row.refunded_cents),
        refund_reason: row
            .refund_reason
            .as_deref()
            .map(RefundReason::parse)
            .transpose()?,
        received_at: from_micros(row.received_at_us)?,
    })
}

async fn find_payment(conn: &mut PgConnection, reference: &str) -> Result<Option<Payment>, Error> {
    sqlx::query_as!(
        PaymentRow,
        r#"SELECT id, order_id, provider_payment_id, amount_cents, refunded_cents, refund_reason,
            (extract(epoch FROM received_at) * 1000000)::bigint AS "received_at_us!"
        FROM payment WHERE provider_payment_id = $1"#,
        reference,
    )
    .fetch_optional(conn)
    .await?
    .map(payment_from_row)
    .transpose()
}

/// Records a completed payment (F5) and, if the order can take it, moves the order from
/// pending payment to paid with a donation allocation of the full amount for the drop's
/// charity, or none on a drop without charity (F4c).
///
/// Every payment is stored, also one the order cannot take: then the payment is marked
/// refund due with its [`RefundReason`] and a pending order is cancelled. Only a payment
/// that completes within the order's hold counts; a later one is always refunded
/// (ADR 0012). Reporting the same payment again changes nothing.
///
/// Locks in the order of ADR 0008 and ADR 0012: bake day, products and pools, slot, then
/// the order row. The clock is read after the locks.
///
/// # Errors
/// [`Error::NotFound`], or [`Error::PaymentOfOtherOrder`] when the reference is already
/// recorded for another order.
pub async fn mark_paid(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: OrderId,
    payment: &PaymentReceived,
) -> Result<PaidOutcome, Error> {
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
    lock_slot(&mut tx, day_id, PickupSlotId::new(placed.pickup_slot_id)).await?;
    sqlx::query_scalar!(
        r#"SELECT id FROM "order" WHERE id = $1 FOR UPDATE"#,
        id.get()
    )
    .fetch_one(&mut *tx)
    .await?;
    let before = order::get(&mut tx, id).await?;

    if let Some(known) = find_payment(&mut tx, &payment.reference).await? {
        if known.order_id != id {
            return Err(Error::PaymentOfOtherOrder);
        }
        tx.commit().await?;
        return Ok(match known.refund_reason {
            None => PaidOutcome::AlreadyPaid(before),
            Some(reason) => PaidOutcome::RefundDue {
                order: before,
                payment: known,
                reason,
            },
        });
    }

    let now = clock.now();
    let reason = refusal(&before, &day, payment.amount, now);
    sqlx::query!(
        "INSERT INTO payment (order_id, provider_payment_id, amount_cents, status, refund_due,
            refund_reason, received_at)
        VALUES ($1, $2, $3, 'paid', $4, $5,
            timestamptz 'epoch' + $6::bigint * interval '1 microsecond')",
        id.get(),
        payment.reference,
        payment.amount.cents(),
        reason.is_some(),
        reason.map(RefundReason::as_str),
        to_micros(now),
    )
    .execute(&mut *tx)
    .await?;
    let recorded = find_payment(&mut tx, &payment.reference)
        .await?
        .ok_or(Error::NotFound)?;

    let outcome = if let Some(reason) = reason {
        let cancel = match reason {
            RefundReason::HoldExpired => Some(CancelReason::HoldExpired),
            RefundReason::DayFinal => Some(CancelReason::DayCancelled),
            _ => None,
        };
        if let (Some(cancel), OrderStatus::PendingPayment) = (cancel, before.status) {
            sqlx::query!(
                r#"UPDATE "order" SET status = 'cancelled', cancel_reason = $2 WHERE id = $1"#,
                id.get(),
                cancel.as_str(),
            )
            .execute(&mut *tx)
            .await?;
        }
        let after = order::get(&mut tx, id).await?;
        record(&mut tx, &before, &after, "payment_refused", &recorded, now).await?;
        PaidOutcome::RefundDue {
            order: after,
            payment: recorded,
            reason,
        }
    } else {
        sqlx::query!(
            r#"UPDATE "order" SET status = 'paid',
                paid_at = timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
            WHERE id = $1"#,
            id.get(),
            to_micros(now),
        )
        .execute(&mut *tx)
        .await?;
        let allocation = match day.charity_id.map(CharityId::new) {
            Some(charity_id) => Some(allocate(&mut tx, id, charity_id, payment.amount, now).await?),
            None => None,
        };
        let after = order::get(&mut tx, id).await?;
        record(&mut tx, &before, &after, "paid", &recorded, now).await?;
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
    payment: &Payment,
    now: Timestamp,
) -> Result<(), Error> {
    let mut after = order::snapshot(after);
    after["payment"] = serde_json::json!({
        "id": payment.id.get(),
        "reference": payment.provider_payment_id,
        "amount_cents": payment.amount.cents(),
        "refund_reason": payment.refund_reason.map(RefundReason::as_str),
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

/// Payments whose money must go back and is not fully refunded yet, oldest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn refunds_due(conn: &mut PgConnection) -> Result<Vec<Payment>, Error> {
    sqlx::query_as!(
        PaymentRow,
        r#"SELECT id, order_id, provider_payment_id, amount_cents, refunded_cents, refund_reason,
            (extract(epoch FROM received_at) * 1000000)::bigint AS "received_at_us!"
        FROM payment WHERE refund_due AND refunded_cents < amount_cents
        ORDER BY received_at, id"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(payment_from_row)
    .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use bakedays::BakeDayStatus;
    use domain::{CustomerId, PickupSlotId as Slot};

    use crate::order::{Channel, OrderNumber, PaymentMode};

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn order(status: OrderStatus, cancel_reason: Option<CancelReason>, paid: bool) -> Order {
        Order {
            id: OrderId::new(1),
            number: OrderNumber { year: 2026, seq: 1 },
            customer_id: CustomerId::new(1),
            customer_name: "Klant".to_owned(),
            customer_phone: None,
            marketing_opt_in: false,
            bake_day_id: BakeDayId::new(1),
            pickup_slot_id: Slot::new(1),
            channel: Channel::Web,
            payment_mode: PaymentMode::Online,
            status,
            cancel_reason,
            total: Money::from_cents(450),
            hold_expires_at: Some(ts("2026-10-28T12:15:00Z")),
            paid_at: paid.then(|| ts("2026-10-28T12:05:00Z")),
            created_at: ts("2026-10-28T12:00:00Z"),
            lines: Vec::new(),
        }
    }

    fn day(status: BakeDayStatus) -> DayFacts {
        DayFacts {
            status,
            online_ordering: true,
            cutoff_at: Some(ts("2026-10-29T19:00:00Z")),
            charity_id: None,
        }
    }

    #[test]
    fn only_a_pending_order_within_its_hold_takes_the_payment() {
        let open = day(BakeDayStatus::Open);
        let pending = order(OrderStatus::PendingPayment, None, false);
        let ok = Money::from_cents(450);
        let within = ts("2026-10-28T12:14:59Z");
        let expiry = ts("2026-10-28T12:15:00Z");
        assert_eq!(refusal(&pending, &open, ok, within), None);
        assert_eq!(
            refusal(&pending, &open, ok, expiry),
            Some(RefundReason::HoldExpired)
        );
        assert_eq!(
            refusal(&pending, &open, Money::from_cents(449), within),
            Some(RefundReason::AmountMismatch)
        );
        assert_eq!(
            refusal(&pending, &day(BakeDayStatus::Cancelled), ok, within),
            Some(RefundReason::DayFinal)
        );
        let cases = [
            (OrderStatus::Paid, None, true, RefundReason::PaidTwice),
            (OrderStatus::Refunded, None, true, RefundReason::PaidTwice),
            (
                OrderStatus::Confirmed,
                None,
                false,
                RefundReason::OrderConfirmed,
            ),
            (
                OrderStatus::PickedUp,
                None,
                false,
                RefundReason::OrderConfirmed,
            ),
            (
                OrderStatus::Cancelled,
                Some(CancelReason::HoldExpired),
                false,
                RefundReason::HoldExpired,
            ),
            (
                OrderStatus::Cancelled,
                Some(CancelReason::DayCancelled),
                false,
                RefundReason::DayFinal,
            ),
            (
                OrderStatus::Cancelled,
                Some(CancelReason::Staff),
                false,
                RefundReason::OrderCancelled,
            ),
        ];
        for (status, cancel, paid, reason) in cases {
            assert_eq!(
                refusal(&order(status, cancel, paid), &open, ok, within),
                Some(reason),
                "{status:?}"
            );
        }
    }

    #[test]
    fn refund_reasons_round_trip_through_text() {
        for reason in RefundReason::ALL {
            assert_eq!(RefundReason::parse(reason.as_str()).unwrap(), reason);
        }
        assert!(matches!(
            RefundReason::parse("whim"),
            Err(Error::Corrupt(_))
        ));
    }
}
