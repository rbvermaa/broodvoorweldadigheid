use jiff::civil::{Date, Time, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use crate::I18nError;

pub const TIME_ZONE: &str = "Europe/Amsterdam";

const NBSP: char = '\u{a0}';

const WEEKDAYS: [(&str, &str); 7] = [
    ("maandag", "ma"),
    ("dinsdag", "di"),
    ("woensdag", "wo"),
    ("donderdag", "do"),
    ("vrijdag", "vr"),
    ("zaterdag", "za"),
    ("zondag", "zo"),
];

const MONTHS: [(&str, &str); 12] = [
    ("januari", "jan"),
    ("februari", "feb"),
    ("maart", "mrt"),
    ("april", "apr"),
    ("mei", "mei"),
    ("juni", "jun"),
    ("juli", "jul"),
    ("augustus", "aug"),
    ("september", "sep"),
    ("oktober", "okt"),
    ("november", "nov"),
    ("december", "dec"),
];

/// Formats cents as Dutch euros: `€ 1.234,50`, `€ -4,50`. The space is U+00A0 so the
/// euro sign never wraps onto its own line.
#[must_use]
pub fn money(cents: i64) -> String {
    let abs = cents.unsigned_abs();
    let euros = (abs / 100).to_string();
    let mut grouped = String::with_capacity(euros.len() + euros.len() / 3);
    for (i, digit) in euros.chars().enumerate() {
        if i > 0 && (euros.len() - i).is_multiple_of(3) {
            grouped.push('.');
        }
        grouped.push(digit);
    }
    let sign = if cents < 0 { "-" } else { "" };
    format!("€{NBSP}{sign}{grouped},{:02}", abs % 100)
}

fn weekday(date: Date) -> (&'static str, &'static str) {
    let index = match date.weekday() {
        Weekday::Monday => 0,
        Weekday::Tuesday => 1,
        Weekday::Wednesday => 2,
        Weekday::Thursday => 3,
        Weekday::Friday => 4,
        Weekday::Saturday => 5,
        Weekday::Sunday => 6,
    };
    WEEKDAYS[index]
}

fn month(date: Date) -> (&'static str, &'static str) {
    MONTHS[usize::from(date.month().unsigned_abs()) - 1]
}

/// `zaterdag 5 december 2026`
#[must_use]
pub fn long_date(date: Date) -> String {
    format!(
        "{} {} {} {}",
        weekday(date).0,
        date.day(),
        month(date).0,
        date.year()
    )
}

/// `za 5 dec`
#[must_use]
pub fn short_date(date: Date) -> String {
    format!("{} {} {}", weekday(date).1, date.day(), month(date).1)
}

/// `11:00`
#[must_use]
pub fn time(time: Time) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}

/// `11:00–12:00`, with an en dash.
#[must_use]
pub fn time_range(start: Time, end: Time) -> String {
    format!("{}–{}", self::time(start), self::time(end))
}

/// Formats instants in Europe/Amsterdam local time, so summer and winter time are handled.
#[derive(Clone, Debug)]
pub struct Amsterdam(TimeZone);

impl Amsterdam {
    /// Loads Europe/Amsterdam from the system time zone database (`TZDIR` in the images).
    ///
    /// # Errors
    /// Fails when the time zone database is missing or has no Europe/Amsterdam.
    pub fn load() -> Result<Self, I18nError> {
        TimeZone::get(TIME_ZONE)
            .map(Self)
            .map_err(|e| I18nError::TimeZone {
                zone: TIME_ZONE.to_owned(),
                details: e.to_string(),
            })
    }

    #[must_use]
    pub fn local(&self, at: Timestamp) -> Zoned {
        at.to_zoned(self.0.clone())
    }

    #[must_use]
    pub fn long_date(&self, at: Timestamp) -> String {
        long_date(self.local(at).date())
    }

    #[must_use]
    pub fn short_date(&self, at: Timestamp) -> String {
        short_date(self.local(at).date())
    }

    #[must_use]
    pub fn time(&self, at: Timestamp) -> String {
        time(self.local(at).time())
    }

    #[must_use]
    pub fn time_range(&self, start: Timestamp, end: Timestamp) -> String {
        time_range(self.local(start).time(), self.local(end).time())
    }
}
