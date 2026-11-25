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

/// Only drops whose ordering has closed (`closed` or `done`) count, so a total never moves
/// with each new order while a visitor watches.
///
/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Money`].
pub async fn donations(conn: &mut PgConnection) -> Result<PublicDonations, Error> {
    let closed: BTreeSet<BakeDayId> = sqlx::query_scalar!(
        "SELECT id FROM bake_day WHERE status IN ('closed', 'done') AND charity_id IS NOT NULL"
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(BakeDayId::new)
    .collect();
    let facts: Vec<DonationFact> = facts::donations(conn, None, None)
        .await?
        .into_iter()
        .filter(|f| closed.contains(&f.bake_day_id))
        .collect();
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

/// A total and its parts, as a visitor can tie them together on the page.
struct Sum {
    head: usize,
    parts: Vec<usize>,
}

impl Sum {
    fn members(&self) -> impl Iterator<Item = usize> + '_ {
        std::iter::once(self.head).chain(self.parts.iter().copied())
    }
}

/// What a visitor can work out: starting from the shown amounts and the empty ones (a
/// cancelled drop is public, so its zero is known), any sum with one unknown member gives
/// it away. Returns the sum that gives away an amount of 1 to `MIN_ORDERS - 1` orders,
/// if any.
fn reveals(orders: &[usize], sums: &[Sum], shown: &[bool]) -> Option<usize> {
    let mut known: Vec<bool> = shown
        .iter()
        .zip(orders)
        .map(|(&s, &n)| s || n == 0)
        .collect();
    loop {
        let mut progress = false;
        for (s, sum) in sums.iter().enumerate() {
            let mut unknown = sum.members().filter(|&i| !known[i]);
            if let (Some(i), None) = (unknown.next(), unknown.next()) {
                if (1..MIN_ORDERS).contains(&orders[i]) {
                    return Some(s);
                }
                known[i] = true;
                progress = true;
            }
        }
        if !progress {
            return None;
        }
    }
}

/// Which amounts may be shown. Below [`MIN_ORDERS`] an amount is hidden. Then, while a
/// visitor could still work out a small amount by adding and subtracting shown ones
/// ([`reveals`]), the shown amount with the fewest orders in the sum that gives it away
/// is hidden too.
fn suppress(orders: &[usize], sums: &[Sum]) -> Vec<bool> {
    let mut shown: Vec<bool> = orders.iter().map(|&n| n >= MIN_ORDERS).collect();
    while let Some(s) = reveals(orders, sums, &shown) {
        let smallest = |cells: &mut dyn Iterator<Item = usize>| {
            cells.filter(|&i| shown[i]).min_by_key(|&i| (orders[i], i))
        };
        let next = smallest(&mut sums[s].members()).or_else(|| smallest(&mut (0..orders.len())));
        let Some(next) = next else {
            return shown;
        };
        shown[next] = false;
    }
    shown
}

/// The sums on the page, for cells laid out as: the total, one per drop, one per charity,
/// then "Nog te verdelen" when `cells` has room for it. A charity is taken as the sum of
/// its drops; a charity without drops of its own forms no sum.
fn page_sums(drops: &[CharityId], charities: &[CharityId], cells: usize) -> Vec<Sum> {
    let first_drop = 1;
    let first_charity = first_drop + drops.len();
    let mut sums: Vec<Sum> = charities
        .iter()
        .enumerate()
        .map(|(c, charity)| Sum {
            head: first_charity + c,
            parts: drops
                .iter()
                .enumerate()
                .filter(|(_, d)| *d == charity)
                .map(|(i, _)| first_drop + i)
                .collect(),
        })
        .filter(|sum| !sum.parts.is_empty())
        .collect();
    sums.push(Sum {
        head: 0,
        parts: (first_drop..first_charity).collect(),
    });
    sums.push(Sum {
        head: 0,
        parts: (first_charity..cells).collect(),
    });
    sums
}

fn amount(shown: bool, money: Money) -> Amount {
    if shown {
        Amount::Shown(money)
    } else {
        Amount::Hidden
    }
}

pub(crate) fn summarize(facts: &[DonationFact]) -> Result<PublicDonations, Error> {
    let facts: Vec<DonationFact> = facts
        .iter()
        .filter(|f| f.drop_charity.is_some())
        .copied()
        .collect();
    let ever = Period::new(Date::MIN, Date::MAX)?;
    let drops = per_drop(&facts, ever)?;
    let charities = per_charity(&facts, ever, Granularity::Quarter)?;

    let all = givers(&facts, |_| Some(()))?;
    let per_drop_givers = givers(&facts, |f| Some(f.bake_day_id))?;
    let per_charity_givers = givers(&facts, |f| (!is_saved(f)).then_some(f.charity).flatten())?;
    let saved_givers = givers(&facts, |f| is_saved(f).then_some(()))?;
    let count = |set: Option<&BTreeSet<OrderId>>| set.map_or(0, BTreeSet::len);

    let mut total = Money::ZERO;
    let mut saved = Money::ZERO;
    for d in &drops {
        total = total.checked_add(d.net()?)?;
        saved = saved.checked_add(d.saved)?;
    }
    let saved_orders = count(saved_givers.get(&()));

    let first_drop = 1;
    let first_charity = first_drop + drops.len();
    let saved_cell = first_charity + charities.len();
    let mut orders = vec![count(all.get(&()))];
    orders.extend(
        drops
            .iter()
            .map(|d| count(per_drop_givers.get(&d.bake_day_id))),
    );
    orders.extend(
        charities
            .iter()
            .map(|c| count(per_charity_givers.get(&c.charity))),
    );
    if saved_orders > 0 {
        orders.push(saved_orders);
    }

    let sums = page_sums(
        &drops.iter().map(|d| d.charity).collect::<Vec<_>>(),
        &charities.iter().map(|c| c.charity).collect::<Vec<_>>(),
        orders.len(),
    );
    let shown = suppress(&orders, &sums);

    let mut public_drops = drops
        .iter()
        .enumerate()
        .map(|(i, d)| {
            Ok(PublicDrop {
                bake_day_id: d.bake_day_id,
                date: d.date,
                charity: d.charity,
                amount: amount(shown[first_drop + i], d.net()?),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    public_drops.reverse();
    let public_charities = charities
        .iter()
        .enumerate()
        .map(|(i, c)| PublicCharity {
            charity: c.charity,
            amount: amount(shown[first_charity + i], c.all_time),
        })
        .collect();
    let unassigned = (saved_orders > 0).then(|| amount(shown[saved_cell], saved));

    Ok(PublicDonations {
        total: amount(shown[0], total),
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
        fact.drop_charity = Some(CharityId::new(1 + (drop - 1) % 2));
        fact.drop_date = date(2026, 1, 1) + jiff::Span::new().days(drop * 7);
        fact
    }

    fn shown(cents: i64) -> Amount {
        Amount::Shown(Money::from_cents(cents))
    }

    fn one_list(parts: &[usize]) -> Vec<bool> {
        let mut orders = vec![parts.iter().sum()];
        orders.extend_from_slice(parts);
        let sum = Sum {
            head: 0,
            parts: (1..orders.len()).collect(),
        };
        suppress(&orders, &[sum])[1..].to_vec()
    }

    #[test]
    fn small_breakdowns_hide_the_smallest_shown_amount_too() {
        assert_eq!(one_list(&[5, 7, 1]), [false, true, false]);
        assert_eq!(one_list(&[5, 7, 6]), [true, true, true]);
        assert_eq!(one_list(&[3, 3]), [false, false]);
        assert_eq!(
            one_list(&[9, 0]),
            [true, false],
            "an empty cell gives nothing away"
        );
        assert_eq!(one_list(&[1]), [false]);
        assert_eq!(one_list(&[]), Vec::<bool>::new());
    }

    #[test]
    fn amounts_hidden_for_cover_cannot_be_worked_out_either() {
        let mut facts = Vec::new();
        let mut id = 0;
        for (drop, charity, orders) in [(1, 1, 10), (2, 2, 6), (3, 3, 1), (4, 2, 7)] {
            for _ in 0..orders {
                id += 1;
                let mut fact = on_drop(id, drop, Some(charity), 100);
                fact.drop_charity = Some(CharityId::new(charity));
                facts.push(fact);
            }
        }
        let public = summarize(&facts).unwrap();
        let drops: Vec<(i64, Amount)> = public
            .drops
            .iter()
            .map(|d| (d.bake_day_id.get(), d.amount))
            .collect();
        let charities: Vec<Amount> = public.charities.iter().map(|c| c.amount).collect();
        let mut orders = vec![24];
        orders.extend([10, 6, 1, 7]);
        orders.extend([10, 13, 1]);
        let mut visible = vec![public.total != Amount::Hidden];
        visible.extend(drops.iter().rev().map(|(_, a)| *a != Amount::Hidden));
        visible.extend(charities.iter().map(|a| *a != Amount::Hidden));
        let ids = |n: i64| CharityId::new(n);
        let sums = page_sums(
            &[ids(1), ids(2), ids(3), ids(2)],
            &[ids(1), ids(2), ids(3)],
            orders.len(),
        );
        assert_eq!(visible.len(), orders.len());
        assert_eq!(
            reveals(&orders, &sums, &visible),
            None,
            "{drops:?} {charities:?}"
        );
        assert_eq!(drops[1], (3, Amount::Hidden));
        assert_eq!(public.total, shown(2400));
    }

    #[test]
    fn a_fully_refunded_drop_is_no_cover() {
        let mut facts: Vec<DonationFact> = (1..=5).map(|i| on_drop(i, 1, Some(1), 500)).collect();
        facts.push(on_drop(6, 3, Some(1), 700));
        let mut cancelled = on_drop(7, 5, Some(1), 900);
        cancelled.refunded = cancelled.amount;
        facts.push(cancelled);
        let public = summarize(&facts).unwrap();
        let shown_amounts: Vec<Amount> = std::iter::once(public.total)
            .chain(public.drops.iter().map(|d| d.amount))
            .chain(public.charities.iter().map(|c| c.amount))
            .filter(|a| *a != Amount::Hidden)
            .collect();
        assert!(
            !(shown_amounts.contains(&shown(3200)) && shown_amounts.contains(&shown(2500))),
            "€32 and €25 together give the €7 away: {public:?}"
        );
    }

    #[test]
    fn a_charity_total_cannot_give_away_its_small_drop() {
        let mut facts: Vec<DonationFact> = (1..=10).map(|i| on_drop(i, 1, Some(1), 500)).collect();
        facts.push(on_drop(11, 3, Some(1), 700));
        facts.extend((12..=17).map(|i| on_drop(i, 2, Some(2), 400)));
        let public = summarize(&facts).unwrap();
        let drops: Vec<(i64, Amount)> = public
            .drops
            .iter()
            .map(|d| (d.bake_day_id.get(), d.amount))
            .collect();
        assert_eq!(
            drops,
            [(3, Amount::Hidden), (2, shown(2400)), (1, Amount::Hidden)],
            "charity 1 minus drop 1 would give drop 3 away"
        );
        assert_eq!(public.charities[0].amount, shown(5700));
        assert_eq!(public.total, shown(8100));
    }

    #[test]
    fn amounts_are_the_owner_reports_and_small_ones_are_hidden() {
        let mut facts: Vec<DonationFact> = (1..=6).map(|i| on_drop(i, 1, Some(1), 500)).collect();
        facts[0].refunded = Money::from_cents(200);
        facts.push(on_drop(7, 2, Some(2), 900));
        let public = summarize(&facts).unwrap();
        assert_eq!(
            public.total,
            Amount::Hidden,
            "with both drops hidden, the total minus charity 1 would give drop 2 away"
        );
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
            [shown(2800), Amount::Hidden]
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
            "else the drop minus the charity would give the one saved donation away"
        );
        assert_eq!(public.total, Amount::Hidden, "the total is the drop");
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
        fn no_small_amount_can_be_worked_out(
            drops in prop::collection::vec((0_usize..4, 0_usize..12, 0_usize..3), 0..8),
        ) {
            let charity_ids: Vec<CharityId> = (1..=4).map(CharityId::new).collect();
            let drop_charities: Vec<CharityId> =
                drops.iter().map(|&(c, _, _)| charity_ids[c]).collect();
            let charities: Vec<CharityId> = charity_ids
                .iter()
                .copied()
                .filter(|c| drop_charities.contains(c))
                .collect();
            let saved: usize = drops.iter().map(|&(_, _, m)| m).sum();
            let mut orders = vec![drops.iter().map(|&(_, n, m)| n + m).sum::<usize>()];
            orders.extend(drops.iter().map(|&(_, n, m)| n + m));
            orders.extend(charities.iter().map(|c| {
                drops
                    .iter()
                    .filter(|&&(d, _, _)| charity_ids[d] == *c)
                    .map(|&(_, n, _)| n)
                    .sum::<usize>()
            }));
            if saved > 0 {
                orders.push(saved);
            }
            let sums = page_sums(&drop_charities, &charities, orders.len());
            let shown = suppress(&orders, &sums);
            prop_assert!(
                reveals(&orders, &sums, &shown).is_none() || !shown.contains(&true),
                "a small amount can be worked out from what is shown"
            );
            for (i, &s) in shown.iter().enumerate() {
                prop_assert!(!s || orders[i] >= MIN_ORDERS);
            }
        }

        #[test]
        fn shown_amounts_match_the_owner_report(
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
            for d in &public.drops {
                if matches!(d.amount, Amount::Shown(_)) {
                    prop_assert!(drop_orders[&d.bake_day_id].len() >= MIN_ORDERS);
                }
            }
        }
    }
}
