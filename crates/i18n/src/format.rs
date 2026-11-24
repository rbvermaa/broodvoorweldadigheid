use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use crate::I18nError;

const NBSP: char = '\u{a0}';

/// Formats `scaled / 10^fraction_digits` with `.` for thousands and `,` for decimals:
/// `decimal(123_456, 2)` is `1.234,56`.
#[must_use]
pub fn decimal(scaled: i64, fraction_digits: usize) -> String {
    let digits = format!(
        "{:0>width$}",
        scaled.unsigned_abs(),
        width = fraction_digits + 1
    );
    let (whole, fraction) = digits.split_at(digits.len() - fraction_digits);
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 2);
    if scaled < 0 {
        out.push('-');
    }
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(digit);
    }
    if !fraction.is_empty() {
        out.push(',');
        out.push_str(fraction);
    }
    out
}

/// `€ 1.234,50` and `€ -4,50`; the space is U+00A0.
#[must_use]
pub fn money(cents: i64) -> String {
    format!("€{NBSP}{}", decimal(cents, 2))
}

/// `05-12-2026`
#[must_use]
pub fn numeric_date(date: Date) -> String {
    format!("{:02}-{:02}-{:04}", date.day(), date.month(), date.year())
}

/// `11:00`
#[must_use]
pub fn time(time: Time) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}

/// `11:00–12:00`, with an en dash.
///
/// # Errors
/// Fails when `end` is before `start`.
pub fn time_range(start: Time, end: Time) -> Result<String, I18nError> {
    if end < start {
        return Err(I18nError::TimeRange {
            start: start.to_string(),
            end: end.to_string(),
        });
    }
    Ok(format!("{}–{}", self::time(start), self::time(end)))
}

/// Converts instants to Europe/Amsterdam local time.
#[derive(Clone, Debug)]
pub struct Amsterdam(TimeZone);

impl Default for Amsterdam {
    fn default() -> Self {
        Self(domain::bakery_time_zone())
    }
}

impl Amsterdam {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn local(&self, at: Timestamp) -> Zoned {
        at.to_zoned(self.0.clone())
    }

    #[must_use]
    pub fn date(&self, at: Timestamp) -> Date {
        self.local(at).date()
    }

    #[must_use]
    pub fn time(&self, at: Timestamp) -> String {
        time(self.local(at).time())
    }

    /// A window on one local day, such as an ophaalmoment.
    ///
    /// # Errors
    /// Fails when `end` is before `start`, in UTC or on the local clock, or when the
    /// two fall on different local days.
    pub fn time_range(&self, start: Timestamp, end: Timestamp) -> Result<String, I18nError> {
        let (local_start, local_end) = (self.local(start), self.local(end));
        if end < start || local_start.date() != local_end.date() {
            return Err(I18nError::TimeRange {
                start: local_start.to_string(),
                end: local_end.to_string(),
            });
        }
        time_range(local_start.time(), local_end.time())
    }
}
