//! The costs the bakery covers so a charity gets the full amount: BTW on the donations,
//! Mollie fees, ingredients and other drop costs, per drop and per charity (F11e).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use db::time::{to_epoch_days, to_micros};
use domain::{BakeDayId, CharityId, Clock, Money, OrderId, VatRate, VatTotals};
use jiff::civil::Date;
use sqlx::PgConnection;

use crate::facts::{self, DonationFact, OrderFact, Selection};
use crate::{Error, Period};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropCostKind {
    Packaging,
    Energy,
    Other,
}

impl DropCostKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Packaging => "packaging",
            Self::Energy => "energy",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDropCost {
    pub bake_day_id: BakeDayId,
    pub kind: DropCostKind,
    pub amount: Money,
    pub note: Option<String>,
}

/// Records a cost of a drop, such as packaging or energy.
///
/// # Errors
/// [`Error::Invalid`] for an amount that is not positive; [`Error::NotFound`] for an
/// unknown bake day; [`Error::Db`].
pub async fn add_drop_cost(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    cost: &NewDropCost,
) -> Result<i64, Error> {
    if cost.amount <= Money::ZERO {
        return Err(Error::Invalid("amount"));
    }
    let note = cost
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    sqlx::query_scalar!(
        "INSERT INTO drop_cost (bake_day_id, kind, amount_cents, note, created_at)
        SELECT id, $2, $3, $4, timestamptz 'epoch' + $5::bigint * interval '1 microsecond'
        FROM bake_day WHERE id = $1
        RETURNING id",
        cost.bake_day_id.get(),
        cost.kind.as_str(),
        cost.amount.cents(),
        note,
        to_micros(clock.now()),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)
}

/// Costs the bakery covers for some donations. Unknown fees and missing cost estimates
/// are counted, never taken as zero silently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contribution {
    /// What the charities get: paid minus refunds.
    pub received: Money,
    /// BTW owed on the donation orders, from their lines net of refunds.
    pub btw: Money,
    /// Known Mollie fees.
    pub fees: Money,
    /// Orders with a payment whose fee is not known yet.
    pub unknown_fees: u32,
    /// Estimated ingredient cost of the products with an estimate.
    pub ingredients: Money,
    /// Products sold without a cost estimate.
    pub missing_estimates: BTreeSet<String>,
    /// Packaging, energy and other costs entered for the drops.
    pub drop_costs: Money,
}

impl Contribution {
    /// BTW, fees, ingredients and drop costs together.
    ///
    /// # Errors
    /// [`Error::Money`].
    pub fn covered(&self) -> Result<Money, Error> {
        Ok(self
            .btw
            .checked_add(self.fees)?
            .checked_add(self.ingredients)?
            .checked_add(self.drop_costs)?)
    }

    /// False when fees or estimates are missing, so the total is too low.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.unknown_fees == 0 && self.missing_estimates.is_empty()
    }

    /// Covered per euro received, in cents, rounded half-up: 38 means "for every €1 the
    /// charity received, you covered €0,38". `None` when nothing was received.
    ///
    /// # Errors
    /// [`Error::Money`].
    pub fn per_euro_cents(&self) -> Result<Option<i64>, Error> {
        self.ratio(100)
    }

    /// Covered per euro received in basis points (1/10 000), rounded half-up.
    ///
    /// # Errors
    /// [`Error::Money`].
    pub fn basis_points(&self) -> Result<Option<i64>, Error> {
        self.ratio(10_000)
    }

    fn ratio(&self, scale: i128) -> Result<Option<i64>, Error> {
        let received = i128::from(self.received.cents());
        if received <= 0 {
            return Ok(None);
        }
        let covered = i128::from(self.covered()?.cents()) * scale;
        let rounded = (2 * covered.abs() + received) / (2 * received) * covered.signum();
        Ok(Some(
            i64::try_from(rounded).map_err(|_| domain::MoneyError::Overflow)?,
        ))
    }

    fn add(&mut self, other: &Self) -> Result<(), Error> {
        self.received = self.received.checked_add(other.received)?;
        self.btw = self.btw.checked_add(other.btw)?;
        self.fees = self.fees.checked_add(other.fees)?;
        self.unknown_fees += other.unknown_fees;
        self.ingredients = self.ingredients.checked_add(other.ingredients)?;
        self.missing_estimates
            .extend(other.missing_estimates.iter().cloned());
        self.drop_costs = self.drop_costs.checked_add(other.drop_costs)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropContribution {
    pub bake_day_id: BakeDayId,
    pub date: Date,
    pub charity: CharityId,
    pub contribution: Contribution,
}

/// `charity` is `None` for donations still saved (F4d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharityContribution {
    pub charity: Option<CharityId>,
    pub contribution: Contribution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionReport {
    pub drops: Vec<DropContribution>,
    pub charities: Vec<CharityContribution>,
    pub total: Contribution,
}

/// One donation order's share; drop costs are added per drop.
fn order_contribution(
    order: &OrderFact,
    donations: &[&DonationFact],
) -> Result<Contribution, Error> {
    let mut c = Contribution::default();
    for d in donations {
        c.received = c.received.checked_add(d.net()?)?;
    }
    let (lines, _) = order.net_lines()?;
    let totals = VatTotals::from_lines(&lines)?;
    for rate in VatRate::ALL {
        c.btw = c.btw.checked_add(totals.rate(rate).vat())?;
    }
    match order.fees {
        Some(fees) => c.fees = fees,
        None => c.unknown_fees = 1,
    }
    for line in &order.lines {
        match line.estimated_cost {
            Some(cost) => c.ingredients = c.ingredients.checked_add(cost.checked_mul(line.qty)?)?,
            None => {
                c.missing_estimates.insert(line.product_name.clone());
            }
        }
    }
    Ok(c)
}

pub(crate) fn build(
    orders: &[OrderFact],
    donations: &[DonationFact],
    drop_costs: &HashMap<BakeDayId, Money>,
    period: Period,
) -> Result<ContributionReport, Error> {
    let mut by_order: HashMap<OrderId, Vec<&DonationFact>> = HashMap::new();
    for d in donations {
        by_order.entry(d.order_id).or_default().push(d);
    }
    let mut drops: BTreeMap<(Date, BakeDayId), DropContribution> = BTreeMap::new();
    let mut charities: BTreeMap<Option<CharityId>, Contribution> = BTreeMap::new();
    for order in orders {
        let (Some(drop_charity), Some(own)) = (order.drop_charity, by_order.get(&order.id)) else {
            continue;
        };
        if !period.contains(order.drop_date) {
            continue;
        }
        let c = order_contribution(order, own)?;
        let drop = drops
            .entry((order.drop_date, order.bake_day_id))
            .or_insert_with(|| DropContribution {
                bake_day_id: order.bake_day_id,
                date: order.drop_date,
                charity: drop_charity,
                contribution: Contribution::default(),
            });
        drop.contribution.add(&c)?;
        let group = own.first().and_then(|d| d.charity);
        charities.entry(group).or_default().add(&c)?;
    }
    for drop in drops.values_mut() {
        if let Some(cost) = drop_costs.get(&drop.bake_day_id) {
            drop.contribution.drop_costs = *cost;
            let costs = Contribution {
                drop_costs: *cost,
                ..Contribution::default()
            };
            charities
                .entry(Some(drop.charity))
                .or_default()
                .add(&costs)?;
        }
    }
    let mut total = Contribution::default();
    for c in charities.values() {
        total.add(c)?;
    }
    Ok(ContributionReport {
        drops: drops.into_values().collect(),
        charities: charities
            .into_iter()
            .map(|(charity, contribution)| CharityContribution {
                charity,
                contribution,
            })
            .collect(),
        total,
    })
}

/// The bakery's contribution for charity drops dated in `period`. A drop's other costs
/// count for the drop's charity; an order's BTW, fees and ingredients for the charity
/// its donation went to. Ingredient cost uses the product's current estimate.
///
/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Money`].
pub async fn report(conn: &mut PgConnection, period: Period) -> Result<ContributionReport, Error> {
    let orders = facts::orders(
        conn,
        Selection::CharityDrops {
            from: period.from(),
            to: period.to(),
        },
    )
    .await?;
    let donations = facts::donations(conn, Some(period.from()), Some(period.to())).await?;
    let costs = sqlx::query!(
        r#"SELECT dc.bake_day_id, SUM(dc.amount_cents)::bigint AS "amount!"
        FROM drop_cost dc JOIN bake_day bd ON bd.id = dc.bake_day_id
        WHERE bd.date BETWEEN date 'epoch' + $1::integer AND date 'epoch' + $2::integer
        GROUP BY dc.bake_day_id"#,
        to_epoch_days(period.from()),
        to_epoch_days(period.to()),
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|row| {
        (
            BakeDayId::new(row.bake_day_id),
            Money::from_cents(row.amount),
        )
    })
    .collect();
    build(&orders, &donations, &costs, period)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::FactLine;
    use crate::facts::tests::donation;
    use domain::ProductId;
    use jiff::civil::date;
    use orders::OrderNumber;
    use proptest::prelude::*;

    fn year() -> Period {
        Period::new(date(2026, 1, 1), date(2026, 12, 31)).unwrap()
    }

    fn order(id: i64, drop: i64, price: i64, cost: Option<i64>, fees: Option<i64>) -> OrderFact {
        OrderFact {
            id: OrderId::new(id),
            number: OrderNumber {
                year: 2026,
                seq: i32::try_from(id).unwrap(),
            },
            bake_day_id: BakeDayId::new(drop),
            drop_date: date(2026, 10, 31),
            drop_charity: Some(CharityId::new(drop)),
            sold_at: "2026-10-28T12:00:00Z".parse().unwrap(),
            lines: vec![FactLine {
                product_id: ProductId::new(1),
                product_name: "Desem".into(),
                qty: 2,
                unit_price: Money::from_cents(price),
                suggested_price: None,
                vat_rate: VatRate::Nine,
                estimated_cost: cost.map(Money::from_cents),
            }],
            refunded: Money::ZERO,
            fees: fees.map(Money::from_cents),
        }
    }

    fn on_drop(mut fact: DonationFact, drop: i64) -> DonationFact {
        fact.bake_day_id = BakeDayId::new(drop);
        fact.drop_charity = Some(CharityId::new(drop));
        fact
    }

    #[test]
    fn a_drop_adds_btw_fees_ingredients_and_costs() {
        let orders = [
            order(1, 1, 545, Some(120), Some(32)),
            order(2, 1, 545, Some(120), Some(32)),
        ];
        let donations = [donation(1, 1, Some(1), 1090), donation(2, 2, Some(1), 1090)];
        let costs = HashMap::from([(BakeDayId::new(1), Money::from_cents(250))]);
        let report = build(&orders, &donations, &costs, year()).unwrap();
        let c = &report.drops[0].contribution;
        assert_eq!(c.received.cents(), 2180);
        assert_eq!(c.btw.cents(), 180);
        assert_eq!(c.fees.cents(), 64);
        assert_eq!(c.ingredients.cents(), 480);
        assert_eq!(c.drop_costs.cents(), 250);
        assert_eq!(c.covered().unwrap().cents(), 974);
        // 974 / 2180 = 0.4468
        assert_eq!(c.per_euro_cents().unwrap(), Some(45));
        assert_eq!(c.basis_points().unwrap(), Some(4468));
        assert!(c.complete());
        assert_eq!(report.total, *c);
    }

    #[test]
    fn unknown_fees_and_estimates_are_reported() {
        let orders = [order(1, 1, 545, None, None)];
        let donations = [donation(1, 1, Some(1), 1090)];
        let report = build(&orders, &donations, &HashMap::new(), year()).unwrap();
        let c = &report.total;
        assert_eq!(c.unknown_fees, 1);
        assert_eq!(c.missing_estimates.iter().collect::<Vec<_>>(), ["Desem"]);
        assert!(!c.complete());
        assert_eq!(c.fees, Money::ZERO);
    }

    #[test]
    fn nothing_received_has_no_ratio() {
        let c = Contribution {
            btw: Money::from_cents(10),
            ..Contribution::default()
        };
        assert_eq!(c.per_euro_cents().unwrap(), None);
    }

    #[test]
    fn orders_without_donation_are_skipped() {
        let mut normal = order(1, 1, 545, Some(1), Some(1));
        normal.drop_charity = None;
        let report = build(
            &[normal, order(2, 1, 545, None, None)],
            &[],
            &HashMap::new(),
            year(),
        )
        .unwrap();
        assert!(report.drops.is_empty());
        assert_eq!(report.total, Contribution::default());
    }

    proptest! {
        #[test]
        fn drops_and_charities_cover_the_same_costs(
            spec in prop::collection::vec(
                (1_i64..4, prop::option::of(1_i64..4), 1_i64..3_000, prop::option::of(0_i64..500),
                 prop::option::of(0_i64..100)),
                0..15,
            ),
            costs in prop::collection::vec(0_i64..1_000, 3),
        ) {
            let mut orders = Vec::new();
            let mut donations = Vec::new();
            for (i, (drop, charity, price, cost, fee)) in spec.into_iter().enumerate() {
                let id = i64::try_from(i).unwrap() + 1;
                orders.push(order(id, drop, price, cost, fee));
                donations.push(on_drop(donation(id, id, charity, price * 2), drop));
            }
            let costs: HashMap<BakeDayId, Money> = costs
                .into_iter()
                .enumerate()
                .map(|(i, c)| (BakeDayId::new(i64::try_from(i).unwrap() + 1), Money::from_cents(c)))
                .collect();
            let report = build(&orders, &donations, &costs, year()).unwrap();
            let mut drops = Contribution::default();
            for d in &report.drops {
                drops.add(&d.contribution).unwrap();
            }
            prop_assert_eq!(drops.covered().unwrap(), report.total.covered().unwrap());
            prop_assert_eq!(drops.received, report.total.received);
            prop_assert_eq!(drops.unknown_fees, report.total.unknown_fees);
        }
    }
}
