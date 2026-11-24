use crate::Money;

/// Dutch BTW rate. Prices are entered including BTW.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VatRate {
    Nine,
    TwentyOne,
}

impl VatRate {
    pub const ALL: [Self; 2] = [Self::Nine, Self::TwentyOne];

    #[must_use]
    pub const fn percent(self) -> i64 {
        match self {
            Self::Nine => 9,
            Self::TwentyOne => 21,
        }
    }

    /// The BTW contained in an amount that includes BTW:
    /// `gross * rate / (100 + rate)`, rounded half-up to whole cents.
    ///
    /// Negative amounts round the same way on their absolute value, so a refund
    /// carries exactly the BTW of the sale it reverses. A tie can never occur:
    /// `100 + rate` is odd (109, 121), so the exact result is never `n + 0.5`.
    #[must_use]
    pub fn vat_part(self, gross: Money) -> Money {
        let rate = i128::from(self.percent());
        let divisor = 100 + rate;
        let numerator = i128::from(gross.cents()).abs() * rate;
        let rounded = (2 * numerator + divisor) / (2 * divisor);
        let signed = if gross.cents() < 0 { -rounded } else { rounded };
        // |vat| <= |gross|, so it fits back into an i64.
        Money::from_cents(i64::try_from(signed).unwrap_or_default())
    }

    /// The amount excluding BTW: `gross - vat_part(gross)`.
    #[must_use]
    pub fn net_part(self, gross: Money) -> Money {
        Money::from_cents(gross.cents() - self.vat_part(gross).cents())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn vat(rate: VatRate, gross: i64) -> i64 {
        rate.vat_part(Money::from_cents(gross)).cents()
    }

    #[test]
    fn percentages() {
        assert_eq!(VatRate::Nine.percent(), 9);
        assert_eq!(VatRate::TwentyOne.percent(), 21);
        assert_eq!(VatRate::ALL, [VatRate::Nine, VatRate::TwentyOne]);
    }

    #[test]
    fn exact_amounts() {
        assert_eq!(vat(VatRate::Nine, 0), 0);
        assert_eq!(vat(VatRate::Nine, 109), 9);
        assert_eq!(vat(VatRate::Nine, 545), 45);
        assert_eq!(vat(VatRate::TwentyOne, 121), 21);
        assert_eq!(vat(VatRate::TwentyOne, 605), 105);
    }

    #[test]
    fn rounds_to_nearest_cent() {
        // 6 * 9 / 109 = 0.495..., 7 * 9 / 109 = 0.577...
        assert_eq!(vat(VatRate::Nine, 6), 0);
        assert_eq!(vat(VatRate::Nine, 7), 1);
        // 2 * 21 / 121 = 0.347..., 3 * 21 / 121 = 0.520...
        assert_eq!(vat(VatRate::TwentyOne, 2), 0);
        assert_eq!(vat(VatRate::TwentyOne, 3), 1);
        // 450 * 9 / 109 = 37.155...; 395 * 21 / 121 = 68.553...
        assert_eq!(vat(VatRate::Nine, 450), 37);
        assert_eq!(vat(VatRate::TwentyOne, 395), 69);
    }

    #[test]
    fn negative_amounts_mirror_positive_ones() {
        assert_eq!(vat(VatRate::Nine, -7), -1);
        assert_eq!(vat(VatRate::Nine, -6), 0);
        assert_eq!(vat(VatRate::TwentyOne, -395), -69);
    }

    #[test]
    fn extreme_amounts_do_not_overflow() {
        assert_eq!(vat(VatRate::TwentyOne, i64::MAX), 1_600_750_518_792_977_619);
        assert_eq!(
            vat(VatRate::TwentyOne, i64::MIN),
            -1_600_750_518_792_977_620
        );
        let gross = Money::from_cents(i64::MIN);
        assert_eq!(
            VatRate::Nine.net_part(gross).cents() + VatRate::Nine.vat_part(gross).cents(),
            i64::MIN
        );
    }

    #[test]
    fn net_part_is_gross_minus_vat() {
        assert_eq!(
            VatRate::Nine.net_part(Money::from_cents(450)),
            Money::from_cents(413)
        );
        assert_eq!(
            VatRate::TwentyOne.net_part(Money::from_cents(121)),
            Money::from_cents(100)
        );
    }

    fn rate() -> impl Strategy<Value = VatRate> {
        prop_oneof![Just(VatRate::Nine), Just(VatRate::TwentyOne)]
    }

    proptest! {
        #[test]
        fn vat_plus_net_is_gross(rate in rate(), gross in any::<i64>()) {
            let gross = Money::from_cents(gross);
            let sum = rate.vat_part(gross).checked_add(rate.net_part(gross));
            prop_assert_eq!(sum, Ok(gross));
        }

        #[test]
        fn vat_is_within_half_a_cent(rate in rate(), gross in any::<i64>()) {
            let p = i128::from(rate.percent());
            let exact_times_d = i128::from(gross) * p;
            let rounded_times_d = i128::from(rate.vat_part(Money::from_cents(gross)).cents()) * (100 + p);
            prop_assert!(2 * (exact_times_d - rounded_times_d).abs() < 100 + p);
        }
    }
}
