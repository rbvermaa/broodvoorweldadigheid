use domain::Money;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    #[error("currency {0:?} is not EUR")]
    Currency(String),
    #[error("amount {0:?} is not like 12.34")]
    Malformed(String),
    #[error("amount {0:?} is out of range")]
    OutOfRange(String),
}

/// Mollie's amount value: an optional `-`, whole euros without leading zeros, a dot
/// and exactly two cent digits.
#[must_use]
pub fn format_value(money: Money) -> String {
    let cents = money.cents();
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// # Errors
/// [`AmountError`] for anything [`format_value`] would not produce, such as `1.2`,
/// `1,00`, `01.00`, `+1.00` or `-0.00`.
pub fn parse_value(value: &str) -> Result<Money, AmountError> {
    let malformed = || AmountError::Malformed(value.to_string());
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    let (euros, cents) = unsigned.split_once('.').ok_or_else(malformed)?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(euros) || cents.len() != 2 || !digits(cents) {
        return Err(malformed());
    }
    if euros.len() > 1 && euros.starts_with('0') {
        return Err(malformed());
    }
    let out_of_range = || AmountError::OutOfRange(value.to_string());
    let magnitude = euros
        .parse::<u64>()
        .ok()
        .and_then(|e| e.checked_mul(100))
        .and_then(|c| c.checked_add(cents.parse::<u64>().ok()?))
        .ok_or_else(out_of_range)?;
    if negative && magnitude == 0 {
        return Err(malformed());
    }
    let cents = if negative {
        0i64.checked_sub_unsigned(magnitude)
    } else {
        i64::try_from(magnitude).ok()
    };
    cents.map(Money::from_cents).ok_or_else(out_of_range)
}

/// # Errors
/// [`AmountError::Currency`] unless the currency is EUR, or a malformed value.
pub fn parse_amount(currency: &str, value: &str) -> Result<Money, AmountError> {
    if currency != "EUR" {
        return Err(AmountError::Currency(currency.to_string()));
    }
    parse_value(value)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn formats_cents() {
        let cases = [
            (0, "0.00"),
            (5, "0.05"),
            (1234, "12.34"),
            (100, "1.00"),
            (-29, "-0.29"),
            (-100_000, "-1000.00"),
            (i64::MAX, "92233720368547758.07"),
            (i64::MIN, "-92233720368547758.08"),
        ];
        for (cents, text) in cases {
            assert_eq!(format_value(Money::from_cents(cents)), text);
            assert_eq!(parse_value(text), Ok(Money::from_cents(cents)), "{text}");
        }
    }

    #[test]
    fn refuses_malformed_values() {
        for bad in [
            "", "1", "1.", ".50", "1.2", "1.234", "1,00", "01.00", "00.00", "+1.00", "-0.00",
            "--1.00", " 1.00", "1.00 ", "1.0a", "1e2.00", "١.٠٠", "-.50", "1.-1",
        ] {
            assert_eq!(
                parse_value(bad),
                Err(AmountError::Malformed(bad.into())),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn refuses_out_of_range_values() {
        for bad in [
            "92233720368547758.08",
            "-92233720368547758.09",
            "999999999999999999999.00",
        ] {
            assert_eq!(
                parse_value(bad),
                Err(AmountError::OutOfRange(bad.into())),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn only_euros() {
        assert_eq!(parse_amount("EUR", "4.50"), Ok(Money::from_cents(450)));
        assert_eq!(
            parse_amount("USD", "4.50"),
            Err(AmountError::Currency("USD".into()))
        );
    }

    proptest! {
        #[test]
        fn round_trips(cents in any::<i64>()) {
            let money = Money::from_cents(cents);
            prop_assert_eq!(parse_value(&format_value(money)), Ok(money));
        }

        #[test]
        fn anything_accepted_formats_back_the_same(text in "-?[0-9]{1,4}[.,]?[0-9]{0,3}") {
            if let Ok(money) = parse_value(&text) {
                prop_assert_eq!(format_value(money), text);
            }
        }
    }
}
