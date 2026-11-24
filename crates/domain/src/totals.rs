use crate::{Money, MoneyError, VatRate};

/// A sold quantity at a unit price that includes BTW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub unit_price: Money,
    pub qty: u32,
    pub vat_rate: VatRate,
}

impl Line {
    /// # Errors
    /// [`MoneyError::Overflow`] when the line total does not fit.
    pub fn total(&self) -> Result<Money, MoneyError> {
        self.unit_price.checked_mul(self.qty)
    }
}

/// Gross, BTW and net for one rate. Only [`VatTotals::from_lines`] makes one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateTotal {
    gross: Money,
    vat: Money,
    net: Money,
}

impl RateTotal {
    fn new(rate: VatRate, gross: Money) -> Self {
        Self {
            gross,
            vat: rate.vat_part(gross),
            net: rate.net_part(gross),
        }
    }

    #[must_use]
    pub const fn gross(&self) -> Money {
        self.gross
    }

    #[must_use]
    pub const fn vat(&self) -> Money {
        self.vat
    }

    #[must_use]
    pub const fn net(&self) -> Money {
        self.net
    }
}

/// Totals per BTW rate, always computed from lines.
///
/// BTW is split once per rate on the summed gross amount, not per line, so the
/// BTW per rate matches what a single invoice or report line shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VatTotals {
    nine: RateTotal,
    twenty_one: RateTotal,
}

impl VatTotals {
    /// # Errors
    /// [`MoneyError::Overflow`] when a line or rate total does not fit.
    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a Line>) -> Result<Self, MoneyError> {
        let mut nine = Money::ZERO;
        let mut twenty_one = Money::ZERO;
        for line in lines {
            let sum = match line.vat_rate {
                VatRate::Nine => &mut nine,
                VatRate::TwentyOne => &mut twenty_one,
            };
            *sum = sum.checked_add(line.total()?)?;
        }
        Ok(Self {
            nine: RateTotal::new(VatRate::Nine, nine),
            twenty_one: RateTotal::new(VatRate::TwentyOne, twenty_one),
        })
    }

    #[must_use]
    pub const fn rate(&self, rate: VatRate) -> RateTotal {
        match rate {
            VatRate::Nine => self.nine,
            VatRate::TwentyOne => self.twenty_one,
        }
    }

    /// # Errors
    /// [`MoneyError::Overflow`] when the sum over all rates does not fit.
    pub fn gross(&self) -> Result<Money, MoneyError> {
        self.nine.gross.checked_add(self.twenty_one.gross)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn line(cents: i64, qty: u32, vat_rate: VatRate) -> Line {
        Line {
            unit_price: Money::from_cents(cents),
            qty,
            vat_rate,
        }
    }

    #[test]
    fn empty_totals_are_zero() {
        let totals = VatTotals::from_lines(&[]).unwrap();
        assert_eq!(totals, VatTotals::default());
        for rate in VatRate::ALL {
            assert_eq!(totals.rate(rate), RateTotal::default());
        }
        assert_eq!(totals.gross(), Ok(Money::ZERO));
    }

    #[test]
    fn sums_per_rate_and_splits_btw_on_the_rate_total() {
        let lines = [
            line(450, 2, VatRate::Nine),
            line(395, 1, VatRate::TwentyOne),
            line(325, 1, VatRate::Nine),
        ];
        let totals = VatTotals::from_lines(&lines).unwrap();

        let nine = totals.rate(VatRate::Nine);
        assert_eq!(nine.gross(), Money::from_cents(1225));
        assert_eq!(nine.vat(), Money::from_cents(101));
        assert_eq!(nine.net(), Money::from_cents(1124));

        let twenty_one = totals.rate(VatRate::TwentyOne);
        assert_eq!(twenty_one.gross(), Money::from_cents(395));
        assert_eq!(twenty_one.vat(), Money::from_cents(69));
        assert_eq!(twenty_one.net(), Money::from_cents(326));

        assert_eq!(totals.gross(), Ok(Money::from_cents(1620)));
    }

    #[test]
    fn line_total_overflow_is_an_error() {
        assert_eq!(
            line(i64::MAX, 2, VatRate::Nine).total(),
            Err(MoneyError::Overflow)
        );
        assert_eq!(
            VatTotals::from_lines(&[line(i64::MAX, 1, VatRate::Nine), line(1, 1, VatRate::Nine)]),
            Err(MoneyError::Overflow)
        );
    }

    #[test]
    fn grand_total_overflow_is_an_error() {
        let totals = VatTotals::from_lines(&[
            line(i64::MAX, 1, VatRate::Nine),
            line(1, 1, VatRate::TwentyOne),
        ])
        .unwrap();
        assert_eq!(totals.gross(), Err(MoneyError::Overflow));
    }

    fn arb_line() -> impl Strategy<Value = Line> {
        (
            -100_000_i64..1_000_000,
            0_u32..1_000,
            prop_oneof![Just(VatRate::Nine), Just(VatRate::TwentyOne)],
        )
            .prop_map(|(cents, qty, rate)| line(cents, qty, rate))
    }

    proptest! {
        #[test]
        fn totals_per_rate_equal_the_sum_of_the_lines(lines in prop::collection::vec(arb_line(), 0..50)) {
            let totals = VatTotals::from_lines(&lines).unwrap();
            for rate in VatRate::ALL {
                let expected: i64 = lines
                    .iter()
                    .filter(|l| l.vat_rate == rate)
                    .map(|l| l.unit_price.cents() * i64::from(l.qty))
                    .sum();
                let total = totals.rate(rate);
                prop_assert_eq!(total.gross().cents(), expected);
                prop_assert_eq!(total.vat(), rate.vat_part(total.gross()));
                prop_assert_eq!(total.vat().cents() + total.net().cents(), expected);
            }
            let all: i64 = lines.iter().map(|l| l.unit_price.cents() * i64::from(l.qty)).sum();
            prop_assert_eq!(totals.gross(), Ok(Money::from_cents(all)));
        }
    }
}
