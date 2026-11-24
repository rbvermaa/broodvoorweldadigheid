/// An amount in euro cents. Negative amounts are allowed for refunds and corrections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Money(i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("amount out of range")]
    Overflow,
}

impl Money {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn from_cents(cents: i64) -> Self {
        Self(cents)
    }

    #[must_use]
    pub const fn cents(self) -> i64 {
        self.0
    }

    /// # Errors
    /// [`MoneyError::Overflow`] when the sum does not fit.
    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    /// # Errors
    /// [`MoneyError::Overflow`] when the difference does not fit.
    pub fn checked_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    /// # Errors
    /// [`MoneyError::Overflow`] when the product does not fit.
    pub fn checked_mul(self, qty: u32) -> Result<Self, MoneyError> {
        self.0
            .checked_mul(i64::from(qty))
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_round_trip() {
        assert_eq!(Money::from_cents(450).cents(), 450);
        assert_eq!(Money::ZERO.cents(), 0);
        assert_eq!(Money::default(), Money::ZERO);
    }

    #[test]
    fn add_and_sub() {
        let a = Money::from_cents(450);
        let b = Money::from_cents(125);
        assert_eq!(a.checked_add(b), Ok(Money::from_cents(575)));
        assert_eq!(b.checked_sub(a), Ok(Money::from_cents(-325)));
    }

    #[test]
    fn mul_by_quantity() {
        assert_eq!(
            Money::from_cents(450).checked_mul(3),
            Ok(Money::from_cents(1350))
        );
        assert_eq!(Money::from_cents(450).checked_mul(0), Ok(Money::ZERO));
    }

    #[test]
    fn overflow_is_an_error() {
        let max = Money::from_cents(i64::MAX);
        let min = Money::from_cents(i64::MIN);
        assert_eq!(
            max.checked_add(Money::from_cents(1)),
            Err(MoneyError::Overflow)
        );
        assert_eq!(
            min.checked_sub(Money::from_cents(1)),
            Err(MoneyError::Overflow)
        );
        assert_eq!(max.checked_mul(2), Err(MoneyError::Overflow));
    }

    #[test]
    fn ordering_follows_cents() {
        assert!(Money::from_cents(-1) < Money::ZERO);
        assert!(Money::from_cents(100) > Money::from_cents(99));
    }
}
