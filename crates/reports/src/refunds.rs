use domain::{Line, Money, VatRate};

use crate::Error;

/// Refunds of an order turned into negative lines, so they carry BTW (ADR 0005, ADR 0017).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RefundSplit {
    pub lines: Vec<Line>,
    /// A partial refund of an order with both rates, split in proportion to each rate's
    /// gross; see the open question on mixed-rate partial refunds.
    pub estimated: bool,
}

/// Negative lines for `refunded` cents taken back on an order with `lines`.
///
/// - A refund of the whole order (or more) reverses every line exactly.
/// - A partial refund of a single-rate order is one line at that rate.
/// - A partial refund of a mixed-rate order is split over the rates in proportion to
///   their gross, largest remainder first, and marked `estimated`.
///
/// # Errors
/// [`Error::Money`] when an amount does not fit.
pub fn refund_lines(lines: &[Line], refunded: Money) -> Result<RefundSplit, Error> {
    if refunded <= Money::ZERO || lines.is_empty() {
        return Ok(RefundSplit::default());
    }
    let mut per_rate: Vec<(VatRate, i64)> = Vec::new();
    let mut gross = Money::ZERO;
    for line in lines {
        let total = line.total()?;
        gross = gross.checked_add(total)?;
        match per_rate.iter_mut().find(|(rate, _)| *rate == line.vat_rate) {
            Some((_, sum)) => *sum = Money::from_cents(*sum).checked_add(total)?.cents(),
            None => per_rate.push((line.vat_rate, total.cents())),
        }
    }
    if refunded >= gross {
        let lines = lines
            .iter()
            .map(|line| {
                Ok(Line {
                    unit_price: line.unit_price.checked_neg()?,
                    ..*line
                })
            })
            .collect::<Result<_, Error>>()?;
        return Ok(RefundSplit {
            lines,
            estimated: false,
        });
    }
    per_rate.retain(|(_, sum)| *sum > 0);
    per_rate.sort_by_key(|(rate, _)| *rate);
    let estimated = per_rate.len() > 1;
    let total = i128::from(gross.cents());
    let wanted = i128::from(refunded.cents());
    let mut shares: Vec<(VatRate, i128, i128)> = per_rate
        .iter()
        .map(|&(rate, sum)| {
            let exact = wanted * i128::from(sum);
            (rate, exact / total, exact % total)
        })
        .collect();
    let mut left = wanted - shares.iter().map(|s| s.1).sum::<i128>();
    let mut order: Vec<usize> = (0..shares.len()).collect();
    order.sort_by(|&a, &b| shares[b].2.cmp(&shares[a].2).then(a.cmp(&b)));
    for i in order {
        if left == 0 {
            break;
        }
        shares[i].1 += 1;
        left -= 1;
    }
    let lines = shares
        .into_iter()
        .filter(|s| s.1 > 0)
        .map(|(vat_rate, cents, _)| {
            let cents = i64::try_from(cents).map_err(|_| domain::MoneyError::Overflow)?;
            Ok(Line {
                unit_price: Money::from_cents(-cents),
                qty: 1,
                vat_rate,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(RefundSplit { lines, estimated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::VatTotals;
    use proptest::prelude::*;

    fn line(cents: i64, qty: u32, vat_rate: VatRate) -> Line {
        Line {
            unit_price: Money::from_cents(cents),
            qty,
            vat_rate,
        }
    }

    fn eur(cents: i64) -> Money {
        Money::from_cents(cents)
    }

    #[test]
    fn nothing_refunded_gives_no_lines() {
        let lines = [line(450, 1, VatRate::Nine)];
        assert_eq!(
            refund_lines(&lines, Money::ZERO).unwrap(),
            RefundSplit::default()
        );
        assert_eq!(refund_lines(&[], eur(100)).unwrap(), RefundSplit::default());
    }

    #[test]
    fn a_full_refund_reverses_every_line() {
        let lines = [line(7, 3, VatRate::Nine), line(395, 1, VatRate::TwentyOne)];
        let split = refund_lines(&lines, eur(416)).unwrap();
        assert!(!split.estimated);
        let mut all = lines.to_vec();
        all.extend(split.lines);
        assert_eq!(VatTotals::from_lines(&all).unwrap(), VatTotals::default());
    }

    #[test]
    fn a_partial_single_rate_refund_is_one_line() {
        let lines = [line(450, 2, VatRate::Nine)];
        let split = refund_lines(&lines, eur(450)).unwrap();
        assert_eq!(split.lines, vec![line(-450, 1, VatRate::Nine)]);
        assert!(!split.estimated);
    }

    #[test]
    fn a_partial_mixed_refund_is_split_by_gross_and_estimated() {
        // 900 at 9% and 300 at 21%: 100 back is 75 and 25.
        let lines = [
            line(450, 2, VatRate::Nine),
            line(300, 1, VatRate::TwentyOne),
        ];
        let split = refund_lines(&lines, eur(100)).unwrap();
        assert_eq!(
            split.lines,
            vec![
                line(-75, 1, VatRate::Nine),
                line(-25, 1, VatRate::TwentyOne)
            ]
        );
        assert!(split.estimated);
        // 1 cent cannot be split; it goes to the larger remainder (9%: 0.75).
        let split = refund_lines(&lines, eur(1)).unwrap();
        assert_eq!(split.lines, vec![line(-1, 1, VatRate::Nine)]);
        assert!(split.estimated);
    }

    fn lines_strategy() -> impl Strategy<Value = Vec<Line>> {
        prop::collection::vec(
            (
                0_i64..5_000,
                1_u32..10,
                prop_oneof![Just(VatRate::Nine), Just(VatRate::TwentyOne)],
            )
                .prop_map(|(c, q, r)| line(c, q, r)),
            1..6,
        )
    }

    proptest! {
        #[test]
        fn refund_lines_sum_to_the_refund(lines in lines_strategy(), pct in 0_i64..=120) {
            let gross: i64 = lines.iter().map(|l| l.total().unwrap().cents()).sum();
            let refunded = gross * pct / 100;
            let split = refund_lines(&lines, eur(refunded)).unwrap();
            let back: i64 = split.lines.iter().map(|l| l.total().unwrap().cents()).sum();
            prop_assert_eq!(back, -refunded.min(gross));
            for l in &split.lines {
                prop_assert!(l.unit_price <= Money::ZERO);
            }
        }
    }
}
