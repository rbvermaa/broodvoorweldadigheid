//! Donation reports per drop and per charity, with chart series and CSV (F11c).
//!
//! Everything is computed from the donation allocations, net of refunds, so the drops'
//! totals and the charities' totals are the same money. Drops without a charity have no
//! allocations and do not appear.

use std::collections::BTreeMap;

use domain::{BakeDayId, CharityId, Money, OrderId};
use i18n::Translator;
use jiff::civil::Date;
use sqlx::PgConnection;

use crate::csv_out::{self, Csv};
use crate::facts::{self, AllocationStatus, DonationFact};
use crate::{Error, Granularity, Period};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropDonations {
    pub bake_day_id: BakeDayId,
    pub date: Date,
    pub charity: CharityId,
    pub orders: u32,
    /// Paid by customers.
    pub received: Money,
    pub refunds: Money,
    /// Net, for the drop's own charity.
    pub to_drop_charity: Money,
    /// Net, moved by customers to another charity (F4d).
    pub to_other_charities: Money,
    /// Net, saved for a later charity (F4d).
    pub saved: Money,
}

impl DropDonations {
    /// Received minus refunds; the sum of the three shares.
    ///
    /// # Errors
    /// [`Error::Money`].
    pub fn net(&self) -> Result<Money, Error> {
        Ok(self.received.checked_sub(self.refunds)?)
    }
}

pub(crate) fn per_drop(
    facts: &[DonationFact],
    period: Period,
) -> Result<Vec<DropDonations>, Error> {
    let mut drops: BTreeMap<(Date, BakeDayId), (DropDonations, Vec<OrderId>)> = BTreeMap::new();
    for fact in facts {
        let Some(drop_charity) = fact.drop_charity else {
            continue;
        };
        if !period.contains(fact.drop_date) {
            continue;
        }
        let (drop, orders) = drops
            .entry((fact.drop_date, fact.bake_day_id))
            .or_insert_with(|| {
                (
                    DropDonations {
                        bake_day_id: fact.bake_day_id,
                        date: fact.drop_date,
                        charity: drop_charity,
                        orders: 0,
                        received: Money::ZERO,
                        refunds: Money::ZERO,
                        to_drop_charity: Money::ZERO,
                        to_other_charities: Money::ZERO,
                        saved: Money::ZERO,
                    },
                    Vec::new(),
                )
            });
        if !orders.contains(&fact.order_id) {
            orders.push(fact.order_id);
        }
        drop.received = drop.received.checked_add(fact.amount)?;
        drop.refunds = drop.refunds.checked_add(fact.refunded)?;
        let share = match (fact.status, fact.charity) {
            (AllocationStatus::Saved, _) | (_, None) => &mut drop.saved,
            (_, Some(c)) if c == drop_charity => &mut drop.to_drop_charity,
            _ => &mut drop.to_other_charities,
        };
        *share = share.checked_add(fact.net()?)?;
    }
    drops
        .into_values()
        .map(|(mut drop, orders)| {
            drop.orders =
                u32::try_from(orders.len()).map_err(|_| Error::Corrupt("orders".into()))?;
            Ok(drop)
        })
        .collect()
}

/// Per charity drop dated in `period`: total, number of orders, and the split between
/// the drop's charity, other chosen charities and saved donations.
///
/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Money`].
pub async fn drops(conn: &mut PgConnection, period: Period) -> Result<Vec<DropDonations>, Error> {
    let facts = facts::donations(conn, Some(period.from()), Some(period.to())).await?;
    per_drop(&facts, period)
}

/// One point of a chart series: a day, month or quarter and its net donations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub period: Period,
    pub amount: Money,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharityDonations {
    pub charity: CharityId,
    /// Net donations on drops dated in the period.
    pub in_period: Money,
    /// Net donations ever.
    pub all_time: Money,
    /// Paid out by transfers so far, net of settled refunds.
    pub transferred: Money,
    /// All-time net minus transferred.
    pub to_transfer: Money,
    /// Net donations per bucket in the period, for a chart; empty buckets are left out.
    pub series: Vec<Point>,
}

pub(crate) fn per_charity(
    facts: &[DonationFact],
    period: Period,
    granularity: Granularity,
) -> Result<Vec<CharityDonations>, Error> {
    let mut charities: BTreeMap<CharityId, (CharityDonations, BTreeMap<Date, Point>)> =
        BTreeMap::new();
    for fact in facts {
        let Some(charity) = fact.charity else {
            continue;
        };
        if fact.status == AllocationStatus::Saved {
            continue;
        }
        let (sum, series) = charities.entry(charity).or_insert_with(|| {
            (
                CharityDonations {
                    charity,
                    in_period: Money::ZERO,
                    all_time: Money::ZERO,
                    transferred: Money::ZERO,
                    to_transfer: Money::ZERO,
                    series: Vec::new(),
                },
                BTreeMap::new(),
            )
        });
        let net = fact.net()?;
        sum.all_time = sum.all_time.checked_add(net)?;
        sum.transferred = sum.transferred.checked_add(fact.settled()?)?;
        if period.contains(fact.drop_date) {
            sum.in_period = sum.in_period.checked_add(net)?;
            let bucket = granularity.bucket(fact.drop_date, &period);
            let point = series.entry(bucket.from()).or_insert(Point {
                period: bucket,
                amount: Money::ZERO,
            });
            point.amount = point.amount.checked_add(net)?;
        }
    }
    charities
        .into_values()
        .map(|(mut sum, series)| {
            sum.to_transfer = sum.all_time.checked_sub(sum.transferred)?;
            sum.series = series.into_values().collect();
            Ok(sum)
        })
        .collect()
}

/// Per charity: net donations on drops in `period` (with a series per `granularity`)
/// and all-time, transferred against still to transfer. Saved donations count for a
/// charity once they are moved to it.
///
/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Money`].
pub async fn charities(
    conn: &mut PgConnection,
    period: Period,
    granularity: Granularity,
) -> Result<Vec<CharityDonations>, Error> {
    let facts = facts::donations(conn, None, None).await?;
    per_charity(&facts, period, granularity)
}

/// One row per drop. `names` gives a charity's name.
///
/// # Errors
/// [`Error::I18n`] for a missing column name, [`Error::Csv`], [`Error::Money`].
pub fn drops_csv(
    drops: &[DropDonations],
    names: &dyn Fn(CharityId) -> String,
    t: &Translator,
) -> Result<String, Error> {
    let mut out = Csv::new(
        t,
        &[
            "report-col-drop-date",
            "report-col-charity",
            "report-col-orders",
            "report-col-received",
            "report-col-refunds",
            "report-col-donated",
            "report-col-to-drop-charity",
            "report-col-to-other-charities",
            "report-col-saved",
        ],
    )?;
    for drop in drops {
        out.row(&[
            csv_out::date(drop.date),
            names(drop.charity),
            drop.orders.to_string(),
            csv_out::amount(drop.received),
            csv_out::amount(drop.refunds),
            csv_out::amount(drop.net()?),
            csv_out::amount(drop.to_drop_charity),
            csv_out::amount(drop.to_other_charities),
            csv_out::amount(drop.saved),
        ])?;
    }
    out.finish()
}

/// One row per charity.
///
/// # Errors
/// [`Error::I18n`] for a missing column name, [`Error::Csv`].
pub fn charities_csv(
    charities: &[CharityDonations],
    names: &dyn Fn(CharityId) -> String,
    t: &Translator,
) -> Result<String, Error> {
    let mut out = Csv::new(
        t,
        &[
            "report-col-charity",
            "report-col-in-period",
            "report-col-all-time",
            "report-col-transferred",
            "report-col-to-transfer",
        ],
    )?;
    for c in charities {
        out.row(&[
            names(c.charity),
            csv_out::amount(c.in_period),
            csv_out::amount(c.all_time),
            csv_out::amount(c.transferred),
            csv_out::amount(c.to_transfer),
        ])?;
    }
    out.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::tests::donation;
    use jiff::civil::date;
    use proptest::prelude::*;

    fn year() -> Period {
        Period::new(date(2026, 1, 1), date(2026, 12, 31)).unwrap()
    }

    #[test]
    fn a_drop_splits_its_donations() {
        let mut refunded = donation(2, 2, Some(1), 500);
        refunded.refunded = Money::from_cents(500);
        let facts = [
            donation(1, 1, Some(1), 600),
            refunded,
            donation(3, 3, Some(2), 700),
            donation(4, 4, None, 800),
        ];
        let drops = per_drop(&facts, year()).unwrap();
        assert_eq!(drops.len(), 1);
        let drop = &drops[0];
        assert_eq!(drop.orders, 4);
        assert_eq!(drop.received.cents(), 2600);
        assert_eq!(drop.refunds.cents(), 500);
        assert_eq!(drop.net().unwrap().cents(), 2100);
        assert_eq!(drop.to_drop_charity.cents(), 600);
        assert_eq!(drop.to_other_charities.cents(), 700);
        assert_eq!(drop.saved.cents(), 800);

        let names = |c: CharityId| format!("Doel {}", c.get());
        let t = Translator::dutch().unwrap();
        let text = drops_csv(&drops, &names, &t).unwrap();
        assert!(text.ends_with("31-10-2026;Doel 1;4;26,00;5,00;21,00;6,00;7,00;8,00\r\n"));
    }

    #[test]
    fn a_charity_has_period_all_time_and_series() {
        let mut earlier = donation(1, 1, Some(1), 600);
        earlier.drop_date = date(2025, 12, 20);
        earlier.status = AllocationStatus::Transferred;
        earlier.settled_paid = Money::from_cents(600);
        let mut november = donation(3, 3, Some(1), 200);
        november.drop_date = date(2026, 11, 7);
        let facts = [
            earlier,
            donation(2, 2, Some(1), 400),
            november,
            donation(4, 4, None, 50),
        ];
        let charities = per_charity(&facts, year(), Granularity::Month).unwrap();
        assert_eq!(charities.len(), 1);
        let c = &charities[0];
        assert_eq!(c.in_period.cents(), 600);
        assert_eq!(c.all_time.cents(), 1200);
        assert_eq!(c.transferred.cents(), 600);
        assert_eq!(c.to_transfer.cents(), 600);
        let points: Vec<(Date, i64)> = c
            .series
            .iter()
            .map(|p| (p.period.from(), p.amount.cents()))
            .collect();
        assert_eq!(points, [(date(2026, 10, 1), 400), (date(2026, 11, 1), 200)]);

        let names = |c: CharityId| format!("Doel {}", c.get());
        let t = Translator::dutch().unwrap();
        let text = charities_csv(&charities, &names, &t).unwrap();
        assert!(text.ends_with("Doel 1;6,00;12,00;6,00;6,00\r\n"));
    }

    fn arb_fact() -> impl Strategy<Value = DonationFact> {
        (
            1_i64..40,
            1_i64..4,
            prop::option::of(1_i64..4),
            1_i64..4,
            0_i64..2_000,
            0_i64..=100,
            0_i64..400,
        )
            .prop_map(|(id, order, charity, drop, amount, pct, day)| {
                let mut fact = donation(id, order * 10 + drop, charity, amount);
                fact.bake_day_id = BakeDayId::new(drop);
                fact.drop_charity = Some(CharityId::new(drop));
                fact.drop_date = date(2026, 1, 1) + jiff::Span::new().days(day);
                fact.refunded = Money::from_cents(amount * pct / 100);
                fact
            })
    }

    proptest! {
        #[test]
        fn drop_totals_add_up_to_charity_totals(
            facts in prop::collection::vec(arb_fact(), 0..30)
        ) {
            let drops = per_drop(&facts, year()).unwrap();
            let charities = per_charity(&facts, year(), Granularity::Quarter).unwrap();
            let charity_sum: i64 = charities.iter().map(|c| c.in_period.cents()).sum();
            let drop_given: i64 = drops
                .iter()
                .map(|d| d.to_drop_charity.cents() + d.to_other_charities.cents())
                .sum();
            prop_assert_eq!(drop_given, charity_sum);
            for d in &drops {
                prop_assert_eq!(
                    d.net().unwrap().cents(),
                    d.to_drop_charity.cents() + d.to_other_charities.cents() + d.saved.cents()
                );
            }
            for c in &charities {
                let series: i64 = c.series.iter().map(|p| p.amount.cents()).sum();
                prop_assert_eq!(series, c.in_period.cents());
            }
        }
    }
}
