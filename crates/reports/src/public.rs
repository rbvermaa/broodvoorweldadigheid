//! The public donation page (F11d): all-time totals per drop, per charity and overall,
//! from the same numbers as the owner's donation report (F11c), with every amount that
//! could reveal a single customer's donation left out. See ADR 0040.

use std::collections::{BTreeMap, BTreeSet};

use domain::{BakeDayId, CharityId, Money, OrderId};
use jiff::civil::Date;
use sqlx::PgConnection;

use crate::donations::{per_charity, per_drop};
use crate::facts::{self, AllocationStatus, DonationFact};
use crate::{Error, Granularity, Period};

/// An amount is published only when it adds up the donations of at least this many
/// orders. Fully refunded orders do not count.
pub const MIN_ORDERS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Amount {
    Shown(Money),
    /// Too few orders behind it, or left out so that the other hidden amounts cannot be
    /// worked out from the total.
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicDrop {
    pub bake_day_id: BakeDayId,
    pub date: Date,
    pub charity: CharityId,
    pub amount: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicCharity {
    pub charity: CharityId,
    pub amount: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicDonations {
    pub total: Amount,
    /// Newest first.
    pub drops: Vec<PublicDrop>,
    pub charities: Vec<PublicCharity>,
    /// Saved donations no charity has been chosen for yet; `None` when there are none.
    pub unassigned: Option<Amount>,
}

/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Money`].
pub async fn donations(conn: &mut PgConnection) -> Result<PublicDonations, Error> {
    let facts = facts::donations(conn, None, None).await?;
    summarize(&facts)
}

fn is_saved(fact: &DonationFact) -> bool {
    fact.status == AllocationStatus::Saved || fact.charity.is_none()
}

/// Orders that still give something, per key.
fn givers<K: Ord>(
    facts: &[DonationFact],
    key: impl Fn(&DonationFact) -> Option<K>,
) -> Result<BTreeMap<K, BTreeSet<OrderId>>, Error> {
    let mut out: BTreeMap<K, BTreeSet<OrderId>> = BTreeMap::new();
    for fact in facts {
        if let Some(k) = key(fact) {
            let entry = out.entry(k).or_default();
            if fact.net()? > Money::ZERO {
                entry.insert(fact.order_id);
            }
        }
    }
    Ok(out)
}

/// Which amounts of one breakdown may be shown. Below [`MIN_ORDERS`] an amount is hidden;
/// then, while the hidden amounts together hold fewer than [`MIN_ORDERS`] orders, the
/// shown amount with the fewest orders is hidden too, so the total minus the shown
/// amounts never gives away a small group.
fn suppress(orders: &[usize]) -> Vec<bool> {
    let mut shown: Vec<bool> = orders.iter().map(|&n| n >= MIN_ORDERS).collect();
    loop {
        let hidden: usize = orders
            .iter()
            .zip(&shown)
            .filter(|(_, s)| !**s)
            .map(|(n, _)| *n)
            .sum();
        if hidden == 0 || hidden >= MIN_ORDERS {
            return shown;
        }
        let Some(next) = (0..orders.len())
            .filter(|&i| shown[i])
            .min_by_key(|&i| (orders[i], i))
        else {
            return shown;
        };
        shown[next] = false;
    }
}

fn amount(shown: bool, money: Money) -> Amount {
    if shown {
        Amount::Shown(money)
    } else {
        Amount::Hidden
    }
}

pub(crate) fn summarize(facts: &[DonationFact]) -> Result<PublicDonations, Error> {
    let ever = Period::new(Date::MIN, Date::MAX)?;
    let drops = per_drop(facts, ever)?;
    let charities = per_charity(facts, ever, Granularity::Quarter)?;

    let on_charity_drops = |f: &DonationFact| f.drop_charity.map(|_| ());
    let all = givers(facts, on_charity_drops)?;
    let per_drop_givers = givers(facts, |f| f.drop_charity.map(|_| f.bake_day_id))?;
    let per_charity_givers = givers(facts, |f| (!is_saved(f)).then_some(f.charity).flatten())?;
    let saved_givers = givers(facts, |f| is_saved(f).then_some(()))?;
    let count = |set: Option<&BTreeSet<OrderId>>| set.map_or(0, BTreeSet::len);

    let mut total = Money::ZERO;
    let mut saved = Money::ZERO;
    for d in &drops {
        total = total.checked_add(d.net()?)?;
        saved = saved.checked_add(d.saved)?;
    }

    let drop_counts: Vec<usize> = drops
        .iter()
        .map(|d| count(per_drop_givers.get(&d.bake_day_id)))
        .collect();
    let drop_shown = suppress(&drop_counts);
    let mut public_drops = drops
        .iter()
        .zip(drop_shown)
        .map(|(d, shown)| {
            Ok(PublicDrop {
                bake_day_id: d.bake_day_id,
                date: d.date,
                charity: d.charity,
                amount: amount(shown, d.net()?),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    public_drops.reverse();

    let saved_orders = count(saved_givers.get(&()));
    let mut charity_counts: Vec<usize> = charities
        .iter()
        .map(|c| count(per_charity_givers.get(&c.charity)))
        .collect();
    if saved_orders > 0 {
        charity_counts.push(saved_orders);
    }
    let charity_shown = suppress(&charity_counts);
    let public_charities = charities
        .iter()
        .zip(&charity_shown)
        .map(|(c, &shown)| PublicCharity {
            charity: c.charity,
            amount: amount(shown, c.all_time),
        })
        .collect();
    let unassigned = (saved_orders > 0).then(|| amount(charity_shown[charities.len()], saved));

    Ok(PublicDonations {
        total: amount(count(all.get(&())) >= MIN_ORDERS, total),
        drops: public_drops,
        charities: public_charities,
        unassigned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::tests::donation;
    use jiff::civil::date;
    use proptest::prelude::*;

    fn on_drop(id: i64, drop: i64, charity: Option<i64>, cents: i64) -> DonationFact {
        let mut fact = donation(id, id, charity, cents);
        fact.bake_day_id = BakeDayId::new(drop);
        fact.drop_charity = Some(CharityId::new(drop));
        fact.drop_date = date(2026, 1, 1) + jiff::Span::new().days(drop * 7);
        fact
    }

    fn shown(cents: i64) -> Amount {
        Amount::Shown(Money::from_cents(cents))
    }

    #[test]
    fn small_breakdowns_hide_the_smallest_shown_amount_too() {
        assert_eq!(suppress(&[5, 7, 1]), [false, true, false]);
        assert_eq!(suppress(&[5, 7, 6]), [true, true, true]);
        assert_eq!(suppress(&[3, 3]), [false, false]);
        assert_eq!(
            suppress(&[9, 0]),
            [true, false],
            "an empty cell gives nothing away"
        );
        assert_eq!(suppress(&[1]), [false]);
        assert_eq!(suppress(&[]), Vec::<bool>::new());
    }

    #[test]
    fn amounts_are_the_owner_reports_and_small_ones_are_hidden() {
        let mut facts: Vec<DonationFact> = (1..=6).map(|i| on_drop(i, 1, Some(1), 500)).collect();
        facts[0].refunded = Money::from_cents(200);
        facts.push(on_drop(7, 2, Some(2), 900));
        let public = summarize(&facts).unwrap();
        assert_eq!(public.total, shown(3700));
        assert_eq!(
            public.drops.iter().map(|d| d.amount).collect::<Vec<_>>(),
            [Amount::Hidden, Amount::Hidden],
            "the 1-order drop is hidden, and so is the other: total minus it would give it away"
        );
        assert_eq!(
            public
                .charities
                .iter()
                .map(|c| c.amount)
                .collect::<Vec<_>>(),
            [Amount::Hidden, Amount::Hidden]
        );
        assert_eq!(public.unassigned, None);

        facts.extend((8..=12).map(|i| on_drop(i, 2, Some(2), 100)));
        let public = summarize(&facts).unwrap();
        let ever = Period::new(Date::MIN, Date::MAX).unwrap();
        let owner = per_drop(&facts, ever).unwrap();
        assert_eq!(
            public.drops[0].bake_day_id,
            BakeDayId::new(2),
            "newest first"
        );
        assert_eq!(
            public.drops[0].amount,
            shown(owner[1].net().unwrap().cents())
        );
        assert_eq!(public.drops[1].amount, shown(2800));
        assert_eq!(public.charities[0].amount, shown(2800));
        assert_eq!(public.charities[1].amount, shown(1400));
        assert_eq!(public.total, shown(4200));
    }

    #[test]
    fn refunded_orders_do_not_count_and_saved_money_is_unassigned() {
        let mut facts: Vec<DonationFact> = (1..=5).map(|i| on_drop(i, 1, Some(1), 500)).collect();
        for fact in &mut facts[1..] {
            fact.refunded = fact.amount;
        }
        let public = summarize(&facts).unwrap();
        assert_eq!(
            public.total,
            Amount::Hidden,
            "one order still gives something"
        );
        assert_eq!(public.drops[0].amount, Amount::Hidden);

        let mut facts: Vec<DonationFact> = (1..=6).map(|i| on_drop(i, 1, Some(1), 500)).collect();
        facts.extend((7..=11).map(|i| on_drop(i, 1, None, 300)));
        let public = summarize(&facts).unwrap();
        assert_eq!(public.unassigned, Some(shown(1500)));
        assert_eq!(public.charities[0].amount, shown(3000));

        facts.truncate(7);
        let public = summarize(&facts).unwrap();
        assert_eq!(public.unassigned, Some(Amount::Hidden));
        assert_eq!(
            public.charities[0].amount,
            Amount::Hidden,
            "else the total minus the charity would give the one saved donation away"
        );
        assert_eq!(public.total, shown(3300));
    }

    fn arb_fact() -> impl Strategy<Value = DonationFact> {
        (
            1_i64..60,
            1_i64..4,
            prop::option::of(1_i64..4),
            1_i64..2_000,
            prop::bool::weighted(0.2),
        )
            .prop_map(|(id, drop, charity, cents, refunded)| {
                let mut fact = on_drop(id, drop, charity, cents);
                if refunded {
                    fact.refunded = fact.amount;
                }
                fact
            })
    }

    proptest! {
        #[test]
        fn shown_amounts_match_the_owner_report_and_hide_small_groups(
            facts in prop::collection::vec(arb_fact(), 0..60)
        ) {
            let mut seen = BTreeSet::new();
            let facts: Vec<DonationFact> =
                facts.into_iter().filter(|f| seen.insert(f.id)).collect();
            let public = summarize(&facts).unwrap();
            let ever = Period::new(Date::MIN, Date::MAX).unwrap();
            let owner_drops = per_drop(&facts, ever).unwrap();
            let owner_charities = per_charity(&facts, ever, Granularity::Month).unwrap();
            let total: i64 = owner_drops.iter().map(|d| d.net().unwrap().cents()).sum();
            if let Amount::Shown(t) = public.total {
                prop_assert_eq!(t.cents(), total);
            }
            for d in &public.drops {
                let owner = owner_drops.iter().find(|o| o.bake_day_id == d.bake_day_id).unwrap();
                if let Amount::Shown(m) = d.amount {
                    prop_assert_eq!(m, owner.net().unwrap());
                }
            }
            for c in &public.charities {
                let owner = owner_charities.iter().find(|o| o.charity == c.charity).unwrap();
                if let Amount::Shown(m) = c.amount {
                    prop_assert_eq!(m, owner.all_time);
                }
            }
            let drop_orders = givers(&facts, |f| Some(f.bake_day_id)).unwrap();
            let hidden: usize = public
                .drops
                .iter()
                .filter(|d| d.amount == Amount::Hidden)
                .map(|d| drop_orders[&d.bake_day_id].len())
                .sum();
            if public.total == Amount::Hidden {
                prop_assert!(public.drops.iter().all(|d| d.amount == Amount::Hidden));
                prop_assert!(public.charities.iter().all(|c| c.amount == Amount::Hidden));
            } else {
                prop_assert!(hidden == 0 || hidden >= MIN_ORDERS);
            }
            for d in &public.drops {
                if matches!(d.amount, Amount::Shown(_)) {
                    prop_assert!(drop_orders[&d.bake_day_id].len() >= MIN_ORDERS);
                }
            }
        }
    }
}
