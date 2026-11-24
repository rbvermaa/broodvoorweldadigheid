use catalog::SourdoughDeadline;
use domain::{bakery_time_zone, in_bakery_time};
use jiff::civil::{Date, Time};
use jiff::{Span, Timestamp};

use crate::Error;

/// A wall-clock time on a date in the bakery's time zone. A time that does not exist
/// because the clocks jump forward moves forward by the gap; an ambiguous time when they
/// fall back takes the earlier (summer time) instant.
///
/// # Errors
/// [`Error::Invalid`] when the result is out of range.
pub fn bakery_timestamp(date: Date, time: Time) -> Result<Timestamp, Error> {
    date.to_datetime(time)
        .to_zoned(bakery_time_zone())
        .map(|zoned| zoned.timestamp())
        .map_err(|_| Error::Invalid("date"))
}

/// The default order deadline: `days_before` days before pickup, at `at` local time.
///
/// # Errors
/// [`Error::Invalid`] when the result is out of range.
pub fn default_cutoff(pickup_date: Date, days_before: i16, at: Time) -> Result<Timestamp, Error> {
    let span = Span::new()
        .try_days(days_before)
        .map_err(|_| Error::Invalid("deadline days before"))?;
    let date = pickup_date
        .checked_sub(span)
        .map_err(|_| Error::Invalid("date"))?;
    bakery_timestamp(date, at)
}

/// Moves a timestamp by whole days, keeping its local wall-clock time.
///
/// # Errors
/// [`Error::Invalid`] when the result is out of range.
pub fn shift_days(timestamp: Timestamp, days: i32) -> Result<Timestamp, Error> {
    Span::new()
        .try_days(days)
        .and_then(|span| in_bakery_time(timestamp).checked_add(span))
        .map(|zoned| zoned.timestamp())
        .map_err(|_| Error::Invalid("date"))
}

/// When baking starts on a bake date, in local time. Staff do not enter it (ADR 0027).
pub const BAKE_STARTS: Time = jiff::civil::time(5, 0, 0, 0);

/// The moment baking starts on `bake_date`: [`BAKE_STARTS`] in the bakery's time zone.
///
/// # Errors
/// [`Error::Invalid`] when the result is out of range.
pub fn bake_starts_at(bake_date: Date) -> Result<Timestamp, Error> {
    bakery_timestamp(bake_date, BAKE_STARTS)
}

/// The deadline is later than the bakery's sourdough rule allows (F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeadTimeWarning {
    pub cutoff_at: Timestamp,
    pub latest_cutoff: Timestamp,
}

/// Warns when `cutoff_at` is later than the sourdough deadline for `bake_date`.
///
/// # Errors
/// [`Error::Invalid`] when the limit is out of range.
pub fn lead_time_warning(
    cutoff_at: Option<Timestamp>,
    bake_date: Date,
    sourdough: SourdoughDeadline,
) -> Result<Option<LeadTimeWarning>, Error> {
    let Some(cutoff_at) = cutoff_at else {
        return Ok(None);
    };
    let latest_cutoff = default_cutoff(bake_date, sourdough.days_before, sourdough.at)?;
    Ok((cutoff_at > latest_cutoff).then_some(LeadTimeWarning {
        cutoff_at,
        latest_cutoff,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, time};

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    const THURSDAY_8PM: Time = time(20, 0, 0, 0);

    #[test]
    fn saturday_pickup_after_the_october_change_has_a_winter_time_deadline() {
        // Clocks fall back on Sunday 2026-10-25. Thursday 20:00 CET is 19:00 UTC.
        assert_eq!(
            default_cutoff(date(2026, 10, 31), 2, THURSDAY_8PM).unwrap(),
            ts("2026-10-29T19:00:00Z")
        );
        assert_eq!(
            default_cutoff(date(2026, 10, 24), 2, THURSDAY_8PM).unwrap(),
            ts("2026-10-22T18:00:00Z")
        );
    }

    #[test]
    fn saturday_pickup_after_the_march_change_has_a_summer_time_deadline() {
        assert_eq!(
            default_cutoff(date(2026, 3, 28), 2, THURSDAY_8PM).unwrap(),
            ts("2026-03-26T19:00:00Z")
        );
        assert_eq!(
            default_cutoff(date(2026, 4, 4), 2, THURSDAY_8PM).unwrap(),
            ts("2026-04-02T18:00:00Z")
        );
    }

    #[test]
    fn deadline_before_a_change_and_pickup_after_it() {
        // Monday pickup in winter time, deadline on Saturday in summer time.
        assert_eq!(
            default_cutoff(date(2026, 10, 26), 2, THURSDAY_8PM).unwrap(),
            ts("2026-10-24T18:00:00Z")
        );
        assert_eq!(
            default_cutoff(date(2026, 10, 26), 0, time(9, 0, 0, 0)).unwrap(),
            ts("2026-10-26T08:00:00Z")
        );
    }

    #[test]
    fn times_in_the_gap_and_the_overlap() {
        assert_eq!(
            bakery_timestamp(date(2026, 3, 29), time(2, 30, 0, 0)).unwrap(),
            ts("2026-03-29T01:30:00Z")
        );
        assert_eq!(
            bakery_timestamp(date(2026, 10, 25), time(2, 30, 0, 0)).unwrap(),
            ts("2026-10-25T00:30:00Z")
        );
    }

    #[test]
    fn out_of_range_dates_are_invalid() {
        assert!(matches!(
            default_cutoff(date(-9999, 1, 1), 1, THURSDAY_8PM),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            bakery_timestamp(date(9999, 12, 31), time(23, 30, 0, 0)),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            shift_days(ts("2026-10-24T09:00:00Z"), i32::MAX),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn shifting_by_weeks_keeps_the_local_time_across_changes() {
        // Saturday 11:00 CEST -> Saturday 11:00 CET.
        assert_eq!(
            shift_days(ts("2026-10-24T09:00:00Z"), 7).unwrap(),
            ts("2026-10-31T10:00:00Z")
        );
        assert_eq!(
            shift_days(ts("2026-03-28T10:00:00Z"), 7).unwrap(),
            ts("2026-04-04T09:00:00Z")
        );
        assert_eq!(
            shift_days(ts("2026-04-04T09:00:00Z"), -7).unwrap(),
            ts("2026-03-28T10:00:00Z")
        );
    }

    #[test]
    fn baking_starts_at_five_local_time_across_clock_changes() {
        assert_eq!(
            bake_starts_at(date(2026, 3, 28)).unwrap(),
            ts("2026-03-28T04:00:00Z")
        );
        // The clocks jump from 02:00 to 03:00 on 29 March; 05:00 is summer time.
        assert_eq!(
            bake_starts_at(date(2026, 3, 29)).unwrap(),
            ts("2026-03-29T03:00:00Z")
        );
        assert_eq!(
            bake_starts_at(date(2026, 10, 24)).unwrap(),
            ts("2026-10-24T03:00:00Z")
        );
        // The clocks fall back from 03:00 to 02:00 on 25 October; 05:00 is winter time.
        assert_eq!(
            bake_starts_at(date(2026, 10, 25)).unwrap(),
            ts("2026-10-25T04:00:00Z")
        );
    }

    const TWO_DAYS_AT_8PM: SourdoughDeadline = SourdoughDeadline {
        days_before: 2,
        at: THURSDAY_8PM,
    };

    fn warning(cutoff: &str, bake_date: Date) -> Option<LeadTimeWarning> {
        lead_time_warning(Some(ts(cutoff)), bake_date, TWO_DAYS_AT_8PM).unwrap()
    }

    fn sourdough_warning(cutoff: &str, latest: &str) -> Option<LeadTimeWarning> {
        Some(LeadTimeWarning {
            cutoff_at: ts(cutoff),
            latest_cutoff: ts(latest),
        })
    }

    #[test]
    fn deadline_after_thursday_8pm_warns_for_a_saturday_bake() {
        // Saturday 2026-10-31 is in winter time: Thursday 20:00 CET is 19:00 UTC.
        let saturday = date(2026, 10, 31);
        assert_eq!(warning("2026-10-29T19:00:00Z", saturday), None);
        assert_eq!(
            warning("2026-10-29T19:01:00Z", saturday),
            sourdough_warning("2026-10-29T19:01:00Z", "2026-10-29T19:00:00Z")
        );
        assert_eq!(
            warning("2026-10-30T19:00:00Z", saturday),
            sourdough_warning("2026-10-30T19:00:00Z", "2026-10-29T19:00:00Z")
        );
        // A week earlier is summer time: Thursday 20:00 CEST is 18:00 UTC.
        assert_eq!(
            warning("2026-10-22T18:01:00Z", date(2026, 10, 24)),
            sourdough_warning("2026-10-22T18:01:00Z", "2026-10-22T18:00:00Z")
        );
    }

    #[test]
    fn sourdough_deadline_around_the_march_change() {
        assert_eq!(warning("2026-03-26T19:00:00Z", date(2026, 3, 28)), None);
        assert_eq!(
            warning("2026-04-02T19:00:00Z", date(2026, 4, 4)),
            sourdough_warning("2026-04-02T19:00:00Z", "2026-04-02T18:00:00Z")
        );
        // Tuesday bake: the deadline falls on Sunday 29 March, the day the clocks change.
        assert_eq!(warning("2026-03-29T18:00:00Z", date(2026, 3, 31)), None);
        assert_eq!(
            warning("2026-03-29T18:01:00Z", date(2026, 3, 31)),
            sourdough_warning("2026-03-29T18:01:00Z", "2026-03-29T18:00:00Z")
        );
        let gap = SourdoughDeadline {
            days_before: 0,
            at: time(2, 30, 0, 0),
        };
        assert_eq!(
            lead_time_warning(Some(ts("2026-03-29T01:31:00Z")), date(2026, 3, 29), gap)
                .unwrap()
                .map(|w| w.latest_cutoff),
            Some(ts("2026-03-29T01:30:00Z"))
        );
        // 02:30 on 25 October happens twice; the earlier (summer-time) instant is the limit.
        assert_eq!(
            lead_time_warning(Some(ts("2026-10-25T00:31:00Z")), date(2026, 10, 25), gap)
                .unwrap()
                .map(|w| w.latest_cutoff),
            Some(ts("2026-10-25T00:30:00Z"))
        );
    }

    #[test]
    fn no_deadline_no_warning() {
        assert_eq!(
            lead_time_warning(None, date(2026, 10, 31), TWO_DAYS_AT_8PM).unwrap(),
            None
        );
    }
}
