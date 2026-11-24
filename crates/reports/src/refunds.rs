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
/// - A partial refund is split over the order's lines in proportion to their gross,
///   largest remainder first, one negative line per original line. No line gets back
///   more than it cost, so no line ends with negative BTW. When the order has both
///   rates the split is marked `estimated` (open question on mixed-rate refunds).
///
/// # Errors
/// [`Error::Money`] when an amount does not fit.
pub fn refund_lines(lines: &[Line], refunded: Money) -> Result<RefundSplit, Error> {
    if refunded <= Money::ZERO || lines.is_empty() {
        return Ok(RefundSplit::default());
    }
    let totals = lines
        .iter()
        .map(Line::total)
        .collect::<Result<Vec<_>, _>>()?;
    let gross = totals
        .iter()
        .try_fold(Money::ZERO, |sum, t| sum.checked_add(*t))?;
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
    let total = i128::from(gross.cents());
    let wanted = i128::from(refunded.cents());
    let mut shares: Vec<(i128, i128)> = totals
        .iter()
        .map(|t| {
            let exact = wanted * i128::from(t.cents().max(0));
            (exact / total, exact % total)
        })
        .collect();
    let mut left = wanted - shares.iter().map(|s| s.0).sum::<i128>();
    let mut order: Vec<usize> = (0..shares.len()).collect();
    order.sort_by(|&a, &b| shares[b].1.cmp(&shares[a].1).then(a.cmp(&b)));
    for i in order {
        if left == 0 {
            break;
        }
        shares[i].0 += 1;
        left -= 1;
    }
    let mut rates: Vec<VatRate> = Vec::new();
    let mut out = Vec::new();
    for (line, (cents, _)) in lines.iter().zip(shares) {
        if cents == 0 {
            continue;
        }
        if !rates.contains(&line.vat_rate) {
            rates.push(line.vat_rate);
        }
        let cents = i64::try_from(cents).map_err(|_| domain::MoneyError::Overflow)?;
        out.push(Line {
            unit_price: Money::from_cents(-cents),
            qty: 1,
            vat_rate: line.vat_rate,
        });
    }
    let estimated = lines.iter().any(|l| l.vat_rate != lines[0].vat_rate);
    Ok(RefundSplit {
        lines: out,
        estimated,
    })
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

    #[test]
    fn a_partial_refund_never_takes_back_more_btw_than_was_charged() {
        let lines = [line(500, 1, VatRate::Nine); 3];
        let split = refund_lines(&lines, eur(1499)).unwrap();
        let mut all = lines.to_vec();
        all.extend(split.lines);
        let nine = VatTotals::from_lines(&all).unwrap().rate(VatRate::Nine);
        assert_eq!(nine.gross(), eur(1));
        assert_eq!(nine.vat(), Money::ZERO);
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
            let mut all = lines.clone();
            all.extend(split.lines);
            let totals = VatTotals::from_lines(&all).unwrap();
            for rate in VatRate::ALL {
                prop_assert!(totals.rate(rate).gross() >= Money::ZERO);
                prop_assert!(totals.rate(rate).vat() >= Money::ZERO);
            }
        }
    }
}
