use crate::Money;

/// Price bounds for a pay-what-you-want product (F4b). All amounts include BTW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayWhatYouWant {
    suggested: Money,
    min: Money,
    max: Money,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidPriceRange {
    #[error("minimum price is negative")]
    NegativeMinimum,
    #[error("suggested price is outside the minimum and maximum")]
    SuggestedOutsideRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PriceRefused {
    #[error("chosen price {chosen:?} is below the minimum {min:?}")]
    BelowMinimum { chosen: Money, min: Money },
    #[error("chosen price {chosen:?} is above the maximum {max:?}")]
    AboveMaximum { chosen: Money, max: Money },
}

impl PayWhatYouWant {
    /// # Errors
    /// [`InvalidPriceRange`] unless `0 <= min <= suggested <= max`.
    pub fn new(suggested: Money, min: Money, max: Money) -> Result<Self, InvalidPriceRange> {
        if min < Money::ZERO {
            return Err(InvalidPriceRange::NegativeMinimum);
        }
        if suggested < min || suggested > max {
            return Err(InvalidPriceRange::SuggestedOutsideRange);
        }
        Ok(Self {
            suggested,
            min,
            max,
        })
    }

    #[must_use]
    pub const fn suggested(&self) -> Money {
        self.suggested
    }

    #[must_use]
    pub const fn min(&self) -> Money {
        self.min
    }

    #[must_use]
    pub const fn max(&self) -> Money {
        self.max
    }

    /// Returns the chosen price when `min <= chosen <= max`.
    ///
    /// # Errors
    /// [`PriceRefused`] when the chosen price is outside the bounds.
    pub fn check(&self, chosen: Money) -> Result<Money, PriceRefused> {
        if chosen < self.min {
            Err(PriceRefused::BelowMinimum {
                chosen,
                min: self.min,
            })
        } else if chosen > self.max {
            Err(PriceRefused::AboveMaximum {
                chosen,
                max: self.max,
            })
        } else {
            Ok(chosen)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn eur(cents: i64) -> Money {
        Money::from_cents(cents)
    }

    fn bread() -> PayWhatYouWant {
        PayWhatYouWant::new(eur(600), eur(300), eur(1500)).unwrap()
    }

    #[test]
    fn accepts_prices_within_bounds() {
        let pwyw = bread();
        assert_eq!(pwyw.check(eur(300)), Ok(eur(300)));
        assert_eq!(pwyw.check(eur(600)), Ok(eur(600)));
        assert_eq!(pwyw.check(eur(1500)), Ok(eur(1500)));
        assert_eq!(pwyw.suggested(), eur(600));
        assert_eq!(pwyw.min(), eur(300));
        assert_eq!(pwyw.max(), eur(1500));
    }

    #[test]
    fn refuses_prices_outside_bounds() {
        let pwyw = bread();
        assert_eq!(
            pwyw.check(eur(299)),
            Err(PriceRefused::BelowMinimum {
                chosen: eur(299),
                min: eur(300)
            })
        );
        assert_eq!(
            pwyw.check(eur(1501)),
            Err(PriceRefused::AboveMaximum {
                chosen: eur(1501),
                max: eur(1500)
            })
        );
    }

    #[test]
    fn invalid_ranges_are_refused() {
        assert_eq!(
            PayWhatYouWant::new(eur(600), eur(-1), eur(1500)),
            Err(InvalidPriceRange::NegativeMinimum)
        );
        assert_eq!(
            PayWhatYouWant::new(eur(200), eur(300), eur(1500)),
            Err(InvalidPriceRange::SuggestedOutsideRange)
        );
        assert_eq!(
            PayWhatYouWant::new(eur(1600), eur(300), eur(1500)),
            Err(InvalidPriceRange::SuggestedOutsideRange)
        );
        assert!(PayWhatYouWant::new(eur(0), eur(0), eur(0)).is_ok());
    }

    proptest! {
        #[test]
        fn price_outside_min_max_is_always_refused(
            a in 0_i64..100_000,
            b in 0_i64..100_000,
            c in 0_i64..100_000,
            chosen in any::<i64>(),
        ) {
            let mut bounds = [a, b, c];
            bounds.sort_unstable();
            let [min, suggested, max] = bounds;
            let pwyw = PayWhatYouWant::new(eur(suggested), eur(min), eur(max)).unwrap();
            let result = pwyw.check(eur(chosen));
            if chosen < min || chosen > max {
                prop_assert!(result.is_err());
            } else {
                prop_assert_eq!(result, Ok(eur(chosen)));
            }
        }
    }
}
