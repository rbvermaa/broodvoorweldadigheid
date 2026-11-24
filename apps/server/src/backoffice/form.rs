//! Reading back-office forms: urlencoded pairs, Dutch amounts, dates and times entered in
//! the bakery's time zone, and the per-field error messages the templates show.

use std::collections::BTreeMap;

use domain::in_bakery_time;
use jiff::Timestamp;
use jiff::civil::{Date, DateTime, Time};
use serde::Deserialize;

/// The posted fields in order. Checkboxes that are off are simply absent.
#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct Fields(Vec<(String, String)>);

impl Fields {
    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    /// The first value posted under `name`, trimmed; empty when absent.
    pub fn text(&self, name: &str) -> &str {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map_or("", |(_, value)| value.trim())
    }

    /// Like [`Self::text`], but keeps the value as typed (multi-line text).
    pub fn raw(&self, name: &str) -> &str {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map_or("", |(_, value)| value.as_str())
    }

    /// Every value posted under `name`, such as repeated hidden fields.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.0
            .iter()
            .filter(move |(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn checked(&self, name: &str) -> bool {
        self.0.iter().any(|(key, _)| key == name)
    }
}

/// Error messages by field name, already translated.
#[derive(Debug, Default)]
pub struct Errors(BTreeMap<String, String>);

impl Errors {
    pub fn add(&mut self, field: &str, message: String) {
        self.0.entry(field.to_owned()).or_insert(message);
    }

    /// The message for `field`, or empty; the field macros show nothing for empty.
    pub fn get(&self, field: &str) -> &str {
        self.0.get(field).map_or("", String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Cents from a Dutch amount such as `6,50`, `6`, `1.234,50` or `6.50`, with or without
/// `€`. Refuses negative amounts and more than two decimals.
pub fn parse_money(input: &str) -> Option<i64> {
    let cleaned: String = input
        .trim()
        .trim_start_matches('€')
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let (whole, fraction) = match cleaned.rsplit_once(',') {
        Some((whole, fraction)) => (thousands(whole)?, fraction.to_owned()),
        None => match cleaned.rsplit_once('.') {
            Some((whole, fraction)) if fraction.len() <= 2 && !whole.contains('.') => {
                (whole.to_owned(), fraction.to_owned())
            }
            _ => (thousands(&cleaned)?, String::new()),
        },
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(&whole) || fraction.len() > 2 || !(fraction.is_empty() || digits(&fraction)) {
        return None;
    }
    let euros: i64 = whole.parse().ok()?;
    let cents: i64 = format!("{fraction:0<2}").parse().ok()?;
    euros.checked_mul(100)?.checked_add(cents)
}

/// `1.234.567` without its dots; `None` unless every group after a dot has 3 digits.
fn thousands(whole: &str) -> Option<String> {
    let mut groups = whole.split('.');
    let first = groups.next()?;
    let mut out = first.to_owned();
    for group in groups {
        if group.len() != 3 || first.is_empty() {
            return None;
        }
        out.push_str(group);
    }
    Some(out)
}

/// The value a money field shows: `6,50`.
pub fn money_value(cents: i64) -> String {
    i18n::decimal(cents, 2)
}

/// A whole number of at least zero.
pub fn parse_count(input: &str) -> Option<u32> {
    input.trim().parse().ok()
}

/// `HH:MM` from a time input.
pub fn parse_time(input: &str) -> Option<Time> {
    Time::strptime("%H:%M", input.trim()).ok()
}

pub fn time_value(time: Time) -> String {
    i18n::time(time)
}

/// `YYYY-MM-DD` from a date input.
pub fn parse_date(input: &str) -> Option<Date> {
    input.trim().parse().ok()
}

/// A `datetime-local` value (`2026-12-05T04:00`), read as bakery time. A time that the
/// clocks skip moves forward; an ambiguous one takes the earlier instant.
pub fn parse_local(input: &str) -> Option<Timestamp> {
    let local = DateTime::strptime("%Y-%m-%dT%H:%M", input.trim()).ok()?;
    bakedays::schedule::bakery_timestamp(local.date(), local.time()).ok()
}

/// The `datetime-local` value for `at` in bakery time.
pub fn local_value(at: Timestamp) -> String {
    in_bakery_time(at).strftime("%Y-%m-%dT%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dutch_amounts_become_cents() {
        for (input, cents) in [
            ("6,50", 650),
            ("6,5", 650),
            ("6", 600),
            ("€ 6,50", 650),
            ("1.234,50", 123_450),
            ("6.50", 650),
            ("1.234", 123_400),
            ("0,05", 5),
        ] {
            assert_eq!(parse_money(input), Some(cents), "{input}");
        }
        for input in [
            "", "-1", "6,505", "zes", "6,5x", ",50", "1,2,3", "1.2.3", "1.234.5", "6.5000",
            "6.5.0", "12.34,50", ".500",
        ] {
            assert_eq!(parse_money(input), None, "{input}");
        }
        assert_eq!(parse_money(&money_value(123_456)), Some(123_456));
    }

    #[test]
    fn local_times_round_trip_across_the_clock_change() {
        let winter = parse_local("2026-10-31T04:00").expect("winter");
        assert_eq!(winter, "2026-10-31T03:00:00Z".parse().expect("ts"));
        assert_eq!(local_value(winter), "2026-10-31T04:00");
        let summer = parse_local("2026-10-24T04:00").expect("summer");
        assert_eq!(summer, "2026-10-24T02:00:00Z".parse().expect("ts"));
        assert_eq!(parse_local("2026-10-24"), None);
    }

    #[test]
    fn fields_read_text_and_checkboxes() {
        let fields = Fields::from_pairs(&[("name", "  Desem "), ("milk", "ja"), ("note", " a\n")]);
        assert_eq!(fields.text("name"), "Desem");
        assert_eq!(fields.raw("note"), " a\n");
        assert_eq!(fields.text("missing"), "");
        assert!(fields.checked("milk"));
        assert!(!fields.checked("eggs"));
        assert_eq!(parse_time("11:00"), Some(jiff::civil::time(11, 0, 0, 0)));
        assert_eq!(parse_time("25:00"), None);
        assert_eq!(
            parse_date("2026-12-05"),
            Some(jiff::civil::date(2026, 12, 5))
        );
        assert_eq!(parse_count("3"), Some(3));
        assert_eq!(parse_count("-3"), None);
    }

    #[test]
    fn errors_keep_the_first_message_per_field() {
        let mut errors = Errors::default();
        assert!(errors.is_empty());
        errors.add("name", "eerste".to_owned());
        errors.add("name", "tweede".to_owned());
        assert_eq!(errors.get("name"), "eerste");
        assert_eq!(errors.get("price"), "");
    }
}
