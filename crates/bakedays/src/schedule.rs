use domain::{ProductId, bakery_time_zone, in_bakery_time};
use jiff::civil::{Date, Time};
use jiff::{SignedDuration, Span, Timestamp};

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

/// The deadline leaves less than the longest sourdough lead time before baking starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeadTimeWarning {
    pub product_id: ProductId,
    pub lead_time_hours: i32,
    pub cutoff_at: Timestamp,
    pub latest_cutoff: Timestamp,
}

/// Warns when `cutoff_at` is later than `bake_starts_at` minus the longest lead time.
#[must_use]
pub fn lead_time_warning(
    cutoff_at: Option<Timestamp>,
    bake_starts_at: Timestamp,
    lead_times: &[(ProductId, i32)],
) -> Option<LeadTimeWarning> {
    let cutoff_at = cutoff_at?;
    let &(product_id, lead_time_hours) = lead_times
        .iter()
        .reduce(|longest, next| if next.1 > longest.1 { next } else { longest })?;
    let latest_cutoff = bake_starts_at
        .checked_sub(SignedDuration::from_hours(i64::from(lead_time_hours)))
        .ok()?;
    (cutoff_at > latest_cutoff).then_some(LeadTimeWarning {
        product_id,
        lead_time_hours,
        cutoff_at,
        latest_cutoff,
    })
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

    const BREAD: ProductId = ProductId::new(1);
    const ROLLS: ProductId = ProductId::new(2);

    #[test]
    fn warns_when_the_deadline_is_later_than_the_longest_lead_time_allows() {
        let bake = ts("2026-10-31T03:00:00Z");
        let warning = lead_time_warning(
            Some(ts("2026-10-29T19:00:00Z")),
            bake,
            &[(ROLLS, 12), (BREAD, 36)],
        )
        .unwrap();
        assert_eq!(
            warning,
            LeadTimeWarning {
                product_id: BREAD,
                lead_time_hours: 36,
                cutoff_at: ts("2026-10-29T19:00:00Z"),
                latest_cutoff: ts("2026-10-29T15:00:00Z"),
            }
        );
    }

    #[test]
    fn no_warning_when_there_is_enough_time() {
        let bake = ts("2026-10-31T03:00:00Z");
        let at_limit = Some(ts("2026-10-29T15:00:00Z"));
        assert_eq!(lead_time_warning(at_limit, bake, &[(BREAD, 36)]), None);
        assert_eq!(
            lead_time_warning(Some(ts("2026-10-30T15:00:00Z")), bake, &[(ROLLS, 12)]),
            None
        );
        assert_eq!(lead_time_warning(at_limit, bake, &[]), None);
        assert_eq!(lead_time_warning(None, bake, &[(BREAD, 36)]), None);
    }
}
