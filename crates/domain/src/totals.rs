use crate::{Money, MoneyError, VatRate};

/// A sold quantity at a unit price that includes BTW. Refunds are negative lines.
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

    /// The line total split into `(net, vat)`; BTW is rounded per line (ADR 0005).
    ///
    /// # Errors
    /// [`MoneyError::Overflow`] when the line total does not fit.
    pub fn split(&self) -> Result<(Money, Money), MoneyError> {
        let gross = self.total()?;
        Ok((self.vat_rate.net_part(gross), self.vat_rate.vat_part(gross)))
    }
}

/// Gross, BTW and net for one rate. Only [`VatTotals`] makes one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateTotal {
    gross: Money,
    vat: Money,
    net: Money,
}

impl RateTotal {
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

    fn checked_add(&self, other: &Self) -> Result<Self, MoneyError> {
        Ok(Self {
            gross: self.gross.checked_add(other.gross)?,
            vat: self.vat.checked_add(other.vat)?,
            net: self.net.checked_add(other.net)?,
        })
    }
}

/// Totals per BTW rate, always computed from lines.
///
/// Each rate's net and BTW are the sums of the per-line splits (ADR 0005), so
/// totals of documents add up exactly to the totals of all their lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VatTotals {
    nine: RateTotal,
    twenty_one: RateTotal,
}

impl VatTotals {
    /// # Errors
    /// [`MoneyError::Overflow`] when a line or rate total does not fit.
    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a Line>) -> Result<Self, MoneyError> {
        let mut totals = Self::default();
        for line in lines {
            let (net, vat) = line.split()?;
            let one = RateTotal {
                gross: line.total()?,
                vat,
                net,
            };
            let sum = match line.vat_rate {
                VatRate::Nine => &mut totals.nine,
                VatRate::TwentyOne => &mut totals.twenty_one,
            };
            *sum = sum.checked_add(&one)?;
        }
        Ok(totals)
    }

    /// Field-wise sum, for adding up the totals of several documents.
    ///
    /// # Errors
    /// [`MoneyError::Overflow`] when a sum does not fit.
    pub fn checked_add(&self, other: &Self) -> Result<Self, MoneyError> {
        Ok(Self {
            nine: self.nine.checked_add(&other.nine)?,
            twenty_one: self.twenty_one.checked_add(&other.twenty_one)?,
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

    fn eur(cents: i64) -> Money {
        Money::from_cents(cents)
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
    fn line_split_rounds_the_line_total() {
        assert_eq!(line(450, 2, VatRate::Nine).split(), Ok((eur(826), eur(74))));
        assert_eq!(
            line(-450, 2, VatRate::Nine).split(),
            Ok((eur(-826), eur(-74)))
        );
        assert_eq!(
            line(i64::MAX, 2, VatRate::Nine).split(),
            Err(MoneyError::Overflow)
        );
    }

    #[test]
    fn sums_per_rate() {
        let lines = [
            line(450, 2, VatRate::Nine),
            line(395, 1, VatRate::TwentyOne),
            line(325, 1, VatRate::Nine),
        ];
        let totals = VatTotals::from_lines(&lines).unwrap();

        let nine = totals.rate(VatRate::Nine);
        assert_eq!(nine.gross(), eur(1225));
        assert_eq!(nine.vat(), eur(101));
        assert_eq!(nine.net(), eur(1124));

        let twenty_one = totals.rate(VatRate::TwentyOne);
        assert_eq!(twenty_one.gross(), eur(395));
        assert_eq!(twenty_one.vat(), eur(69));
        assert_eq!(twenty_one.net(), eur(326));

        assert_eq!(totals.gross(), Ok(eur(1620)));
    }

    #[test]
    fn btw_is_rounded_per_line() {
        // Each 7c line holds 0.578c BTW, rounded to 1c; the 21c sum would round to 2c.
        let lines = [line(7, 1, VatRate::Nine); 3];
        let nine = VatTotals::from_lines(&lines).unwrap().rate(VatRate::Nine);
        assert_eq!(nine.gross(), eur(21));
        assert_eq!(nine.vat(), eur(3));
        assert_eq!(nine.net(), eur(18));
    }

    #[test]
    fn a_refund_line_cancels_its_sale_exactly() {
        let lines = [line(7, 1, VatRate::Nine), line(-7, 1, VatRate::Nine)];
        let totals = VatTotals::from_lines(&lines).unwrap();
        assert_eq!(totals, VatTotals::default());
    }

    #[test]
    fn overflow_is_an_error() {
        assert_eq!(
            line(i64::MAX, 2, VatRate::Nine).total(),
            Err(MoneyError::Overflow)
        );
        assert_eq!(
            VatTotals::from_lines(&[line(i64::MAX, 1, VatRate::Nine), line(1, 1, VatRate::Nine)]),
            Err(MoneyError::Overflow)
        );
        let big = VatTotals::from_lines(&[line(i64::MAX, 1, VatRate::Nine)]).unwrap();
        assert_eq!(big.checked_add(&big), Err(MoneyError::Overflow));
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

    fn arb_lines() -> impl Strategy<Value = Vec<Line>> {
        prop::collection::vec(arb_line(), 0..50)
    }

    proptest! {
        #[test]
        fn totals_per_rate_equal_the_sum_of_the_lines(lines in arb_lines()) {
            let totals = VatTotals::from_lines(&lines).unwrap();
            for rate in VatRate::ALL {
                let mut gross = 0;
                let mut net = 0;
                let mut vat = 0;
                for l in lines.iter().filter(|l| l.vat_rate == rate) {
                    let (line_net, line_vat) = l.split().unwrap();
                    gross += l.total().unwrap().cents();
                    net += line_net.cents();
                    vat += line_vat.cents();
                }
                let total = totals.rate(rate);
                prop_assert_eq!(total.gross().cents(), gross);
                prop_assert_eq!(total.net().cents(), net);
                prop_assert_eq!(total.vat().cents(), vat);
                prop_assert_eq!(total.vat().cents() + total.net().cents(), gross);
            }
            let all: i64 = lines.iter().map(|l| l.unit_price.cents() * i64::from(l.qty)).sum();
            prop_assert_eq!(totals.gross(), Ok(eur(all)));
        }

        #[test]
        fn combined_totals_equal_totals_of_all_lines(a in arb_lines(), b in arb_lines()) {
            let combined = VatTotals::from_lines(&a)
                .unwrap()
                .checked_add(&VatTotals::from_lines(&b).unwrap());
            let all: Vec<Line> = a.iter().chain(&b).copied().collect();
            prop_assert_eq!(combined, VatTotals::from_lines(&all));
        }
    }
}
