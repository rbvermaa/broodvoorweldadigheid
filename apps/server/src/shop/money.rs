use domain::Money;

/// Most a customer can type as a price per piece; the product's own maximum is checked
/// later. Keeps the arithmetic far from overflow.
const MAX_CENTS: i64 = 10_000_000;

/// A euro amount as a customer types it: `4`, `4,5`, `4,50`, `4.50` or `€ 4,50`. At most
/// two decimals and no thousands separators.
pub fn parse_euros(text: &str) -> Option<Money> {
    let text = text.trim();
    let text = text.strip_prefix('€').unwrap_or(text).trim();
    let (whole, fraction) = match text.split_once([',', '.']) {
        Some((whole, fraction)) => (whole, fraction),
        None => (text, ""),
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty()
        || whole.len() > 6
        || fraction.len() > 2
        || !digits(whole)
        || !digits(fraction)
    {
        return None;
    }
    let whole: i64 = whole.parse().ok()?;
    let fraction: i64 = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction.parse().ok()?,
    };
    let cents = whole * 100 + fraction;
    (cents <= MAX_CENTS).then(|| Money::from_cents(cents))
}

/// `6,50`: the value of a price field, without the euro sign.
pub fn field_value(money: Money) -> String {
    let cents = money.cents();
    format!("{},{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_ways_people_write_euros() {
        for (text, cents) in [
            ("4", 400),
            ("4,5", 450),
            ("4,50", 450),
            ("4.50", 450),
            (" € 12,05 ", 1205),
            ("€4", 400),
            ("0,00", 0),
            ("100000", 10_000_000),
        ] {
            assert_eq!(
                parse_euros(text),
                Some(Money::from_cents(cents)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn refuses_anything_else() {
        for text in [
            "",
            "€",
            ",50",
            "4,505",
            "4,5,0",
            "1.234,50",
            "-4",
            "4 50",
            "vier",
            "4,-",
            "1000000",
            "100000,01",
            "+4",
        ] {
            assert_eq!(parse_euros(text), None, "{text:?}");
        }
    }

    #[test]
    fn field_values_parse_back() {
        for cents in [0, 5, 450, 1_234_567] {
            let money = Money::from_cents(cents);
            assert_eq!(parse_euros(&field_value(money)), Some(money));
        }
        assert_eq!(field_value(Money::from_cents(650)), "6,50");
    }
}
