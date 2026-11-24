//! The rows every report starts from: counted orders with their lines, refunds, fees and
//! donation allocations.

use std::collections::HashMap;

use db::time::{from_epoch_days, from_micros, to_epoch_days, to_micros};
use domain::{
    BakeDayId, CharityId, DonationAllocationId, Line, Money, OrderId, ProductId, VatRate,
};
use jiff::Timestamp;
use jiff::civil::Date;
use orders::OrderNumber;
use sqlx::PgConnection;

use crate::Error;
use crate::period::local_date;
use crate::refunds::{RefundSplit, refund_lines};

/// Orders that are sales: paid, confirmed (on account), picked up, or refunded after
/// payment. A cancelled order counts only when it was paid, with its refunds (ADR 0026);
/// pending orders never count.
pub(crate) const COUNTED: &[&str] = &["paid", "confirmed", "picked_up", "refunded"];

fn counted() -> Vec<String> {
    COUNTED.iter().map(|s| (*s).to_owned()).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FactLine {
    pub product_id: ProductId,
    pub product_name: String,
    pub qty: u32,
    pub unit_price: Money,
    pub suggested_price: Option<Money>,
    pub vat_rate: VatRate,
    pub estimated_cost: Option<Money>,
}

impl FactLine {
    pub fn line(&self) -> Line {
        Line {
            unit_price: self.unit_price,
            qty: self.qty,
            vat_rate: self.vat_rate,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrderFact {
    pub id: OrderId,
    pub number: OrderNumber,
    pub bake_day_id: BakeDayId,
    pub drop_date: Date,
    pub drop_charity: Option<CharityId>,
    /// When the sale happened: paid, or created for an order on account.
    pub sold_at: Timestamp,
    pub lines: Vec<FactLine>,
    /// Refunded on payments the order accepted. Refunds of refused payments return money
    /// that never was a sale.
    pub refunded: Money,
    /// Known fees of every payment on the order.
    pub fees: Money,
    /// A payment on the order has no fee yet.
    pub fee_missing: bool,
}

impl OrderFact {
    pub fn sold_on(&self) -> Date {
        local_date(self.sold_at)
    }

    pub fn lines(&self) -> Vec<Line> {
        self.lines.iter().map(FactLine::line).collect()
    }

    /// The order's lines plus its refunds as negative lines.
    pub fn net_lines(&self) -> Result<(Vec<Line>, RefundSplit), Error> {
        let lines = self.lines();
        let split = refund_lines(&lines, self.refunded)?;
        let mut all = lines;
        all.extend(split.lines.iter().copied());
        Ok((all, split))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection {
    /// Sold in `[start, end)`.
    SoldBetween(Timestamp, Timestamp),
    /// On drops dated `from` through `to` that have a charity.
    CharityDrops { from: Date, to: Date },
}

/// Counted orders by id, with lines, refunds and fees.
pub(crate) async fn orders(
    conn: &mut PgConnection,
    selection: Selection,
) -> Result<Vec<OrderFact>, Error> {
    let (sold_from, sold_to, drop_from, drop_to) = match selection {
        Selection::SoldBetween(start, end) => {
            (Some(to_micros(start)), Some(to_micros(end)), None, None)
        }
        Selection::CharityDrops { from, to } => (
            None,
            None,
            Some(to_epoch_days(from)),
            Some(to_epoch_days(to)),
        ),
    };
    let rows = sqlx::query!(
        r#"SELECT o.id, o.number_year, o.number_seq, o.bake_day_id,
            (bd.date - date 'epoch') AS "drop_day!", bd.charity_id,
            (extract(epoch FROM COALESCE(o.paid_at, o.created_at)) * 1000000)::bigint
                AS "sold_at_us!"
        FROM "order" o JOIN bake_day bd ON bd.id = o.bake_day_id
        WHERE (o.status = ANY($1) OR (o.status = 'cancelled' AND o.paid_at IS NOT NULL))
            AND ($2::bigint IS NULL OR COALESCE(o.paid_at, o.created_at)
                >= timestamptz 'epoch' + $2 * interval '1 microsecond')
            AND ($3::bigint IS NULL OR COALESCE(o.paid_at, o.created_at)
                < timestamptz 'epoch' + $3 * interval '1 microsecond')
            AND ($4::integer IS NULL OR (bd.charity_id IS NOT NULL
                AND bd.date BETWEEN date 'epoch' + $4 AND date 'epoch' + $5::integer))
        ORDER BY o.id"#,
        &counted(),
        sold_from,
        sold_to,
        drop_from,
        drop_to,
    )
    .fetch_all(&mut *conn)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let mut lines = order_lines(conn, &ids).await?;
    let money = payments(conn, &ids).await?;
    rows.into_iter()
        .map(|row| {
            let (refunded, fees, fee_missing) =
                money
                    .get(&row.id)
                    .copied()
                    .unwrap_or((Money::ZERO, Money::ZERO, false));
            Ok(OrderFact {
                id: OrderId::new(row.id),
                number: OrderNumber {
                    year: row.number_year,
                    seq: row.number_seq,
                },
                bake_day_id: BakeDayId::new(row.bake_day_id),
                drop_date: from_epoch_days(row.drop_day)?,
                drop_charity: row.charity_id.map(CharityId::new),
                sold_at: from_micros(row.sold_at_us)?,
                lines: lines.remove(&row.id).unwrap_or_default(),
                refunded,
                fees,
                fee_missing,
            })
        })
        .collect()
}

async fn order_lines(
    conn: &mut PgConnection,
    ids: &[i64],
) -> Result<HashMap<i64, Vec<FactLine>>, Error> {
    let rows = sqlx::query!(
        r#"SELECT ol.order_id, ol.product_id, p.name_nl, ol.qty, ol.unit_price_cents,
            ol.suggested_price_cents, ol.vat_rate, p.estimated_cost_cents
        FROM order_line ol JOIN product p ON p.id = ol.product_id
        WHERE ol.order_id = ANY($1) ORDER BY ol.order_id, ol.id"#,
        ids,
    )
    .fetch_all(conn)
    .await?;
    let mut out: HashMap<i64, Vec<FactLine>> = HashMap::new();
    for row in rows {
        out.entry(row.order_id).or_default().push(FactLine {
            product_id: ProductId::new(row.product_id),
            product_name: row.name_nl,
            qty: u32::try_from(row.qty).map_err(|_| Error::Corrupt("line quantity".into()))?,
            unit_price: Money::from_cents(row.unit_price_cents),
            suggested_price: row.suggested_price_cents.map(Money::from_cents),
            vat_rate: VatRate::try_from(row.vat_rate)?,
            estimated_cost: row.estimated_cost_cents.map(Money::from_cents),
        });
    }
    Ok(out)
}

/// Per order: refunds on accepted payments, known fees, and whether a fee is missing. A
/// payment's refund is the larger of what the provider reported and its refund rows, in
/// any status (ADR 0026).
pub(crate) async fn payments(
    conn: &mut PgConnection,
    ids: &[i64],
) -> Result<HashMap<i64, (Money, Money, bool)>, Error> {
    let rows = sqlx::query!(
        r#"SELECT order_id,
            COALESCE(SUM(GREATEST(refunded_cents, COALESCE(r.requested, 0)))
                FILTER (WHERE NOT refund_due), 0)::bigint AS "refunded!",
            COALESCE(SUM(fee_cents), 0)::bigint AS "fees!",
            COUNT(*) FILTER (WHERE fee_cents IS NULL) AS "fees_missing!"
        FROM payment
        LEFT JOIN (
            SELECT payment_id, SUM(amount_cents) AS requested FROM refund GROUP BY payment_id
        ) r ON r.payment_id = payment.id
        WHERE order_id = ANY($1) AND status = 'paid'
        GROUP BY order_id"#,
        ids,
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.order_id,
                (
                    Money::from_cents(row.refunded),
                    Money::from_cents(row.fees),
                    row.fees_missing > 0,
                ),
            )
        })
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllocationStatus {
    Earmarked,
    Saved,
    Transferred,
}

pub(crate) fn parse_status(text: &str) -> Result<AllocationStatus, Error> {
    match text {
        "earmarked" => Ok(AllocationStatus::Earmarked),
        "saved" => Ok(AllocationStatus::Saved),
        "transferred" => Ok(AllocationStatus::Transferred),
        other => Err(Error::Corrupt(format!("allocation status {other:?}"))),
    }
}

/// A donation allocation with its drop, refunds and what transfers settled of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DonationFact {
    pub id: DonationAllocationId,
    pub order_id: OrderId,
    pub bake_day_id: BakeDayId,
    pub drop_date: Date,
    /// The drop's charity; allocations only exist on drops with one (F4c).
    pub drop_charity: Option<CharityId>,
    /// `None` while saved (F4d).
    pub charity: Option<CharityId>,
    pub status: AllocationStatus,
    pub amount: Money,
    pub refunded: Money,
    pub settled_paid: Money,
    pub settled_refunded: Money,
}

impl DonationFact {
    /// What the charity gets: the full amount paid minus refunds.
    pub fn net(&self) -> Result<Money, Error> {
        Ok(self.amount.checked_sub(self.refunded)?)
    }

    /// What transfers paid out for it so far.
    pub fn settled(&self) -> Result<Money, Error> {
        Ok(self.settled_paid.checked_sub(self.settled_refunded)?)
    }
}

/// Donation allocations on drops dated `from` through `to` (all drops when `None`),
/// oldest first.
pub(crate) async fn donations(
    conn: &mut PgConnection,
    from: Option<Date>,
    to: Option<Date>,
) -> Result<Vec<DonationFact>, Error> {
    let rows = sqlx::query!(
        r#"SELECT da.id, da.order_id, o.bake_day_id, (bd.date - date 'epoch') AS "drop_day!",
            bd.charity_id AS drop_charity, da.charity_id, da.status, da.amount_cents,
            COALESCE(t.paid, 0)::bigint AS "settled_paid!",
            COALESCE(t.refunded, 0)::bigint AS "settled_refunded!"
        FROM donation_allocation da
        JOIN "order" o ON o.id = da.order_id
        JOIN bake_day bd ON bd.id = o.bake_day_id
        LEFT JOIN (
            SELECT allocation_id, SUM(paid_cents) AS paid, SUM(refunded_cents) AS refunded
            FROM charity_transfer_allocation GROUP BY allocation_id
        ) t ON t.allocation_id = da.id
        WHERE ($1::integer IS NULL OR bd.date >= date 'epoch' + $1)
            AND ($2::integer IS NULL OR bd.date <= date 'epoch' + $2)
        ORDER BY da.id"#,
        from.map(to_epoch_days),
        to.map(to_epoch_days),
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut order_ids: Vec<i64> = rows.iter().map(|r| r.order_id).collect();
    order_ids.sort_unstable();
    order_ids.dedup();
    let refunded: HashMap<OrderId, Money> = payments(conn, &order_ids)
        .await?
        .into_iter()
        .map(|(id, (refunded, _, _))| (OrderId::new(id), refunded))
        .collect();
    let mut facts = rows
        .into_iter()
        .map(|row| {
            Ok(DonationFact {
                id: DonationAllocationId::new(row.id),
                order_id: OrderId::new(row.order_id),
                bake_day_id: BakeDayId::new(row.bake_day_id),
                drop_date: from_epoch_days(row.drop_day)?,
                drop_charity: row.drop_charity.map(CharityId::new),
                charity: row.charity_id.map(CharityId::new),
                status: parse_status(&row.status)?,
                amount: Money::from_cents(row.amount_cents),
                refunded: Money::ZERO,
                settled_paid: Money::from_cents(row.settled_paid),
                settled_refunded: Money::from_cents(row.settled_refunded),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    spread_refunds(&mut facts, &refunded)?;
    Ok(facts)
}

/// Sets each allocation's refunds: an order's refunds are taken from its allocations
/// oldest first, never more than an allocation's amount. An order has one allocation
/// (ADR 0012), so this only matters for data F4d may add later.
pub(crate) fn spread_refunds(
    facts: &mut [DonationFact],
    refunded_per_order: &HashMap<OrderId, Money>,
) -> Result<(), Error> {
    let mut left = refunded_per_order.clone();
    for fact in facts.iter_mut() {
        let remaining = left.entry(fact.order_id).or_insert(Money::ZERO);
        let take = (*remaining).min(fact.amount).max(Money::ZERO);
        *remaining = remaining.checked_sub(take)?;
        fact.refunded = take;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn donation(id: i64, order: i64, charity: Option<i64>, amount: i64) -> DonationFact {
        DonationFact {
            id: DonationAllocationId::new(id),
            order_id: OrderId::new(order),
            bake_day_id: BakeDayId::new(1),
            drop_date: jiff::civil::date(2026, 10, 31),
            drop_charity: Some(CharityId::new(1)),
            charity: charity.map(CharityId::new),
            status: if charity.is_some() {
                AllocationStatus::Earmarked
            } else {
                AllocationStatus::Saved
            },
            amount: Money::from_cents(amount),
            refunded: Money::ZERO,
            settled_paid: Money::ZERO,
            settled_refunded: Money::ZERO,
        }
    }

    #[test]
    fn refunds_fill_allocations_in_order() {
        let mut facts = [
            donation(1, 10, Some(1), 300),
            donation(2, 10, Some(1), 300),
            donation(3, 11, Some(1), 500),
        ];
        let refunded = HashMap::from([(OrderId::new(10), Money::from_cents(400))]);
        spread_refunds(&mut facts, &refunded).unwrap();
        let got: Vec<i64> = facts.iter().map(|f| f.refunded.cents()).collect();
        assert_eq!(got, [300, 100, 0]);
        assert_eq!(facts[1].net().unwrap().cents(), 200);
    }

    #[test]
    fn statuses_parse() {
        assert_eq!(parse_status("saved").unwrap(), AllocationStatus::Saved);
        assert_eq!(
            parse_status("transferred").unwrap(),
            AllocationStatus::Transferred
        );
        assert_eq!(
            parse_status("earmarked").unwrap(),
            AllocationStatus::Earmarked
        );
        assert!(parse_status("gone").is_err());
    }
}
