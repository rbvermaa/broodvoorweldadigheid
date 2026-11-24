//! What to transfer to a charity, and recording the transfer (F11b).
//!
//! A charity gets the full amount paid minus refunds; nothing is taken off for BTW, fees
//! or costs. A transfer covers every earmarked allocation and settles every refund not
//! settled before, so a refund after a transfer is a correction in the next one.

use db::time::{from_epoch_days, to_epoch_days, to_micros};
use domain::{CharityId, Clock, DonationAllocationId, Money};
use jiff::civil::Date;
use orders::audit::{self, Actor, Entry};
use sqlx::{Acquire, PgConnection};

use crate::facts::{self, AllocationStatus, DonationFact};
use crate::{Error, Period};

/// One allocation's share of a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Settlement {
    pub allocation: DonationAllocationId,
    /// The allocation's amount, when it was not covered before.
    pub paid: Money,
    /// Refunds not settled before.
    pub refunded: Money,
}

/// What a transfer for `charity` covering drops up to `up_to` settles now.
pub(crate) fn settlements(
    facts: &[DonationFact],
    charity: CharityId,
    up_to: Date,
) -> Result<Vec<Settlement>, Error> {
    let mut out = Vec::new();
    for fact in facts {
        if fact.charity != Some(charity)
            || fact.status == AllocationStatus::Saved
            || fact.drop_date > up_to
        {
            continue;
        }
        let paid = fact.amount.checked_sub(fact.settled_paid)?;
        let refunded = fact.refunded.checked_sub(fact.settled_refunded)?;
        if paid != Money::ZERO || refunded != Money::ZERO {
            out.push(Settlement {
                allocation: fact.id,
                paid,
                refunded,
            });
        }
    }
    Ok(out)
}

pub(crate) fn total(settlements: &[Settlement]) -> Result<Money, Error> {
    settlements.iter().try_fold(Money::ZERO, |sum, s| {
        Ok(sum.checked_add(s.paid)?.checked_sub(s.refunded)?)
    })
}

/// The payout overview of one charity for drops dated in a period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payout {
    pub charity: CharityId,
    pub period: Period,
    /// Donations earmarked for the charity on drops in the period.
    pub received: Money,
    /// Refunds of those donations.
    pub refunds: Money,
    /// Already transferred of those donations, net of settled refunds.
    pub transferred: Money,
    /// Donations saved for a later charity (F4d) on this charity's drops in the period;
    /// not part of the amount to transfer.
    pub saved: Money,
    /// Refunds after a transfer, on drops up to the period's end, not settled yet.
    pub corrections: Money,
    /// What [`record_transfer`] for drops up to the period's end must transfer now:
    /// everything paid minus refunds, minus what earlier transfers paid.
    pub to_transfer: Money,
}

impl Payout {
    /// Received minus refunds.
    ///
    /// # Errors
    /// [`Error::Money`].
    pub fn net(&self) -> Result<Money, Error> {
        Ok(self.received.checked_sub(self.refunds)?)
    }
}

pub(crate) fn build(
    facts: &[DonationFact],
    charity: CharityId,
    period: Period,
) -> Result<Payout, Error> {
    let mut payout = Payout {
        charity,
        period,
        received: Money::ZERO,
        refunds: Money::ZERO,
        transferred: Money::ZERO,
        saved: Money::ZERO,
        corrections: Money::ZERO,
        to_transfer: Money::ZERO,
    };
    for fact in facts {
        if !period.contains(fact.drop_date) {
            continue;
        }
        if fact.status == AllocationStatus::Saved {
            if fact.drop_charity == Some(charity) {
                payout.saved = payout.saved.checked_add(fact.net()?)?;
            }
        } else if fact.charity == Some(charity) {
            payout.received = payout.received.checked_add(fact.amount)?;
            payout.refunds = payout.refunds.checked_add(fact.refunded)?;
            payout.transferred = payout.transferred.checked_add(fact.settled()?)?;
        }
    }
    let due = settlements(facts, charity, period.to())?;
    for s in &due {
        if s.paid == Money::ZERO {
            payout.corrections = payout.corrections.checked_add(s.refunded)?;
        }
    }
    payout.to_transfer = total(&due)?;
    Ok(payout)
}

/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Money`].
pub async fn payout(
    conn: &mut PgConnection,
    charity: CharityId,
    period: Period,
) -> Result<Payout, Error> {
    let facts = facts::donations(conn, None, Some(period.to())).await?;
    build(&facts, charity, period)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTransfer {
    pub charity: CharityId,
    /// The transfer covers drops up to and including this date.
    pub up_to: Date,
    pub amount: Money,
    pub transferred_on: Date,
    pub bank_reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharityTransfer {
    pub id: i64,
    pub charity: CharityId,
    pub amount: Money,
    pub transferred_on: Date,
    pub bank_reference: String,
}

/// Records a bank transfer to a charity. The amount must be exactly what is due for
/// drops up to `up_to` ([`Payout::to_transfer`]); every allocation it covers becomes
/// transferred and never changes again.
///
/// # Errors
/// [`Error::NotFound`] for an unknown charity; [`Error::Invalid`] for an empty bank
/// reference; [`Error::NothingToTransfer`] when nothing positive is due;
/// [`Error::AmountMismatch`] with the amount due; [`Error::Db`].
pub async fn record_transfer(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    actor: Actor,
    transfer: &NewTransfer,
) -> Result<CharityTransfer, Error> {
    let bank_reference = transfer.bank_reference.trim().to_owned();
    if bank_reference.is_empty() {
        return Err(Error::Invalid("bank reference"));
    }
    let mut tx = conn.begin().await?;
    sqlx::query_scalar!(
        "SELECT id FROM charity WHERE id = $1 FOR UPDATE",
        transfer.charity.get()
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    sqlx::query_scalar!(
        "SELECT id FROM donation_allocation WHERE charity_id = $1 ORDER BY id FOR UPDATE",
        transfer.charity.get()
    )
    .fetch_all(&mut *tx)
    .await?;
    let facts = facts::donations(&mut tx, None, Some(transfer.up_to)).await?;
    let due_now = settlements(&facts, transfer.charity, transfer.up_to)?;
    let due = total(&due_now)?;
    if due <= Money::ZERO {
        return Err(Error::NothingToTransfer { due });
    }
    if transfer.amount != due {
        return Err(Error::AmountMismatch { due });
    }
    let now = clock.now();
    let id = sqlx::query_scalar!(
        "INSERT INTO charity_transfer (charity_id, amount_cents, transferred_on, bank_reference,
            recorded_at, recorded_by)
        VALUES ($1, $2, date 'epoch' + $3::integer, $4,
            timestamptz 'epoch' + $5::bigint * interval '1 microsecond', $6)
        RETURNING id",
        transfer.charity.get(),
        due.cents(),
        to_epoch_days(transfer.transferred_on),
        bank_reference,
        to_micros(now),
        actor.to_db(),
    )
    .fetch_one(&mut *tx)
    .await?;
    let allocations: Vec<i64> = due_now.iter().map(|s| s.allocation.get()).collect();
    let paid: Vec<i64> = due_now.iter().map(|s| s.paid.cents()).collect();
    let refunded: Vec<i64> = due_now.iter().map(|s| s.refunded.cents()).collect();
    sqlx::query!(
        "INSERT INTO charity_transfer_allocation (transfer_id, allocation_id, paid_cents,
            refunded_cents)
        SELECT $1, * FROM unnest($2::bigint[], $3::bigint[], $4::bigint[])",
        id,
        &allocations,
        &paid,
        &refunded,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE donation_allocation SET status = 'transferred'
        WHERE id = ANY($1) AND status = 'earmarked'",
        &allocations,
    )
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        &Entry {
            entity: "charity_transfer",
            entity_id: id,
            action: "recorded",
            before: None,
            after: Some(serde_json::json!({
                "charity_id": transfer.charity.get(),
                "amount_cents": due.cents(),
                "up_to": transfer.up_to.to_string(),
                "transferred_on": transfer.transferred_on.to_string(),
                "bank_reference": bank_reference,
                "allocations": allocations,
            })),
            actor,
            at: now,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(CharityTransfer {
        id,
        charity: transfer.charity,
        amount: due,
        transferred_on: transfer.transferred_on,
        bank_reference,
    })
}

/// Every transfer to a charity, oldest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn transfers(
    conn: &mut PgConnection,
    charity: CharityId,
) -> Result<Vec<CharityTransfer>, Error> {
    sqlx::query!(
        r#"SELECT id, amount_cents, (transferred_on - date 'epoch') AS "day!", bank_reference
        FROM charity_transfer WHERE charity_id = $1 ORDER BY transferred_on, id"#,
        charity.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(CharityTransfer {
            id: row.id,
            charity,
            amount: Money::from_cents(row.amount_cents),
            transferred_on: from_epoch_days(row.day)?,
            bank_reference: row.bank_reference,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::tests::donation;
    use jiff::civil::date;
    use proptest::prelude::*;

    const ONE: CharityId = CharityId::new(1);

    fn october() -> Period {
        Period::new(date(2026, 10, 1), date(2026, 10, 31)).unwrap()
    }

    #[test]
    fn the_payout_is_paid_minus_refunds_with_saved_apart() {
        let mut refunded = donation(2, 2, Some(1), 500);
        refunded.refunded = Money::from_cents(200);
        let facts = [
            donation(1, 1, Some(1), 600),
            refunded,
            donation(3, 3, None, 700),
            donation(4, 4, Some(2), 800),
        ];
        let payout = build(&facts, ONE, october()).unwrap();
        assert_eq!(payout.received.cents(), 1100);
        assert_eq!(payout.refunds.cents(), 200);
        assert_eq!(payout.net().unwrap().cents(), 900);
        assert_eq!(payout.saved.cents(), 700);
        assert_eq!(payout.to_transfer.cents(), 900);
        assert_eq!(payout.corrections, Money::ZERO);
    }

    #[test]
    fn a_refund_after_a_transfer_is_a_correction() {
        let mut fact = donation(1, 1, Some(1), 600);
        fact.status = AllocationStatus::Transferred;
        fact.settled_paid = Money::from_cents(600);
        fact.refunded = Money::from_cents(150);
        let facts = [fact, donation(2, 2, Some(1), 400)];
        let payout = build(&facts, ONE, october()).unwrap();
        assert_eq!(payout.transferred.cents(), 600);
        assert_eq!(payout.corrections.cents(), 150);
        assert_eq!(payout.to_transfer.cents(), 250);
    }

    #[test]
    fn later_drops_are_not_due_yet() {
        let mut later = donation(2, 2, Some(1), 400);
        later.drop_date = date(2026, 11, 7);
        let facts = [donation(1, 1, Some(1), 600), later];
        assert_eq!(
            build(&facts, ONE, october()).unwrap().to_transfer.cents(),
            600
        );
    }

    fn arb_fact() -> impl Strategy<Value = DonationFact> {
        (
            1_i64..50,
            prop::option::of(1_i64..3),
            0_i64..2_000,
            0_i64..=100,
            any::<bool>(),
        )
            .prop_map(|(id, charity, amount, pct, transferred)| {
                let mut fact = donation(id, id, charity, amount);
                if transferred && charity.is_some() {
                    fact.status = AllocationStatus::Transferred;
                    fact.settled_paid = fact.amount;
                }
                fact.refunded = Money::from_cents(amount * pct / 100);
                fact
            })
    }

    proptest! {
        #[test]
        fn a_transfer_covers_exactly_the_earmarked_minus_refunds(
            facts in prop::collection::vec(arb_fact(), 0..20)
        ) {
            // Before any transfer, the amount due is every earmarked allocation's net.
            let fresh: Vec<DonationFact> = facts
                .iter()
                .map(|f| DonationFact {
                    status: if f.charity.is_some() { AllocationStatus::Earmarked } else { f.status },
                    settled_paid: Money::ZERO,
                    ..*f
                })
                .collect();
            let due = total(&settlements(&fresh, ONE, date(2026, 12, 31)).unwrap()).unwrap();
            let expected: i64 = fresh
                .iter()
                .filter(|f| f.charity == Some(ONE))
                .map(|f| f.net().unwrap().cents())
                .sum();
            prop_assert_eq!(due.cents(), expected);

            // Settling, then settling again with no new refunds, leaves nothing due.
            let settled: Vec<DonationFact> = fresh
                .iter()
                .map(|f| DonationFact {
                    settled_paid: f.amount,
                    settled_refunded: f.refunded,
                    ..*f
                })
                .collect();
            prop_assert!(settlements(&settled, ONE, date(2026, 12, 31)).unwrap().is_empty());

            // Paid out over time always adds up to paid minus refunds.
            let later = total(&settlements(&facts, ONE, date(2026, 12, 31)).unwrap()).unwrap();
            let before: i64 = facts
                .iter()
                .filter(|f| f.charity == Some(ONE))
                .map(|f| f.settled().unwrap().cents())
                .sum();
            prop_assert_eq!(before + later.cents(), expected);
        }
    }
}
