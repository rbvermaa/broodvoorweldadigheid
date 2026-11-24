//! SQLx has no jiff support, so queries move timestamps as microseconds since the
//! Unix epoch, times of day as seconds since midnight and dates as days since 1970-01-01:
//!
//! - read: `(extract(epoch FROM col) * 1000000)::bigint`, `extract(epoch FROM col)::integer`,
//!   `col - date 'epoch'`
//! - write: `timestamptz 'epoch' + $1 * interval '1 microsecond'`, `make_interval(secs => $1)::time`,
//!   `date 'epoch' + $1::integer`

use jiff::Span;
use jiff::Timestamp;
use jiff::civil::{Date, Time, date};

const EPOCH_DATE: Date = date(1970, 1, 1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0} out of range")]
pub struct OutOfRange(pub &'static str);

#[must_use]
pub fn to_micros(timestamp: Timestamp) -> i64 {
    timestamp.as_microsecond()
}

/// # Errors
/// [`OutOfRange`] when jiff cannot represent the value.
pub fn from_micros(micros: i64) -> Result<Timestamp, OutOfRange> {
    Timestamp::from_microsecond(micros).map_err(|_| OutOfRange("timestamp"))
}

#[must_use]
pub fn to_seconds_of_day(time: Time) -> i32 {
    i32::from(time.hour()) * 3600 + i32::from(time.minute()) * 60 + i32::from(time.second())
}

/// # Errors
/// [`OutOfRange`] unless `0 <= seconds < 86400`.
pub fn from_seconds_of_day(seconds: i32) -> Result<Time, OutOfRange> {
    if !(0..86_400).contains(&seconds) {
        return Err(OutOfRange("time of day"));
    }
    let hour = i8::try_from(seconds / 3600).map_err(|_| OutOfRange("time of day"))?;
    let minute = i8::try_from(seconds / 60 % 60).map_err(|_| OutOfRange("time of day"))?;
    let second = i8::try_from(seconds % 60).map_err(|_| OutOfRange("time of day"))?;
    Time::new(hour, minute, second, 0).map_err(|_| OutOfRange("time of day"))
}

/// # Errors
/// Never for a date jiff can represent; the error keeps the signature honest.
pub fn to_epoch_days(day: Date) -> Result<i32, OutOfRange> {
    EPOCH_DATE
        .until(day)
        .map_err(|_| OutOfRange("date"))
        .map(|span| span.get_days())
}

/// # Errors
/// [`OutOfRange`] when jiff cannot represent the date.
pub fn from_epoch_days(days: i32) -> Result<Date, OutOfRange> {
    Span::new()
        .try_days(days)
        .and_then(|span| EPOCH_DATE.checked_add(span))
        .map_err(|_| OutOfRange("date"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_through_micros() {
        let ts: Timestamp = "2026-03-29T01:00:00.123456Z".parse().unwrap();
        assert_eq!(from_micros(to_micros(ts)), Ok(ts));
        assert_eq!(to_micros(Timestamp::UNIX_EPOCH), 0);
        assert_eq!(from_micros(i64::MAX), Err(OutOfRange("timestamp")));
    }

    #[test]
    fn dates_round_trip_through_epoch_days() {
        assert_eq!(to_epoch_days(date(1970, 1, 1)), Ok(0));
        assert_eq!(to_epoch_days(date(2026, 10, 31)), Ok(20_757));
        assert_eq!(to_epoch_days(date(1969, 12, 31)), Ok(-1));
        assert_eq!(from_epoch_days(20_757), Ok(date(2026, 10, 31)));
        assert_eq!(from_epoch_days(i32::MAX), Err(OutOfRange("date")));
    }

    #[test]
    fn times_round_trip_through_seconds() {
        let time = Time::new(20, 15, 30, 0).unwrap();
        assert_eq!(to_seconds_of_day(time), 72_930);
        assert_eq!(from_seconds_of_day(72_930), Ok(time));
        assert_eq!(from_seconds_of_day(0), Ok(Time::midnight()));
        assert_eq!(from_seconds_of_day(86_400), Err(OutOfRange("time of day")));
        assert_eq!(from_seconds_of_day(-1), Err(OutOfRange("time of day")));
        assert_eq!(OutOfRange("x").to_string(), "x out of range");
    }
}
