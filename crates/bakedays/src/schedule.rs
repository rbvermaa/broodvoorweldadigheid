use catalog::SourdoughDeadline;
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

/// The rule that sets the latest deadline a drop's sourdough allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadTimeRule {
    /// The bakery's rule: a number of days before the bake date, at a local time.
    Sourdough(SourdoughDeadline),
    /// A product's own lead time before baking starts.
    Product {
        product_id: ProductId,
        lead_time_hours: i32,
    },
}

/// The deadline is later than the sourdough allows (F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeadTimeWarning {
    pub rule: LeadTimeRule,
    pub cutoff_at: Timestamp,
    pub latest_cutoff: Timestamp,
}

/// Warns when `cutoff_at` is later than the earliest of: the sourdough deadline for
/// `bake_date`, and `bake_starts_at` minus each product's own lead time.
///
/// # Errors
/// [`Error::Invalid`] when a limit is out of range.
pub fn lead_time_warning(
    cutoff_at: Option<Timestamp>,
    bake_date: Date,
    bake_starts_at: Timestamp,
    sourdough: SourdoughDeadline,
    product_lead_times: &[(ProductId, i32)],
) -> Result<Option<LeadTimeWarning>, Error> {
    let Some(cutoff_at) = cutoff_at else {
        return Ok(None);
    };
    let mut rule = LeadTimeRule::Sourdough(sourdough);
    let mut latest_cutoff = default_cutoff(bake_date, sourdough.days_before, sourdough.at)?;
    for &(product_id, lead_time_hours) in product_lead_times {
        let limit = bake_starts_at
            .checked_sub(SignedDuration::from_hours(i64::from(lead_time_hours)))
            .map_err(|_| Error::Invalid("lead time"))?;
        if limit < latest_cutoff {
            rule = LeadTimeRule::Product {
                product_id,
                lead_time_hours,
            };
            latest_cutoff = limit;
        }
    }
    Ok((cutoff_at > latest_cutoff).then_some(LeadTimeWarning {
        rule,
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

    const BREAD: ProductId = ProductId::new(1);
    const ROLLS: ProductId = ProductId::new(2);
    const TWO_DAYS_AT_8PM: SourdoughDeadline = SourdoughDeadline {
        days_before: 2,
        at: THURSDAY_8PM,
    };

    fn warning(
        cutoff: &str,
        bake_date: Date,
        products: &[(ProductId, i32)],
    ) -> Option<LeadTimeWarning> {
        let bake = bakery_timestamp(bake_date, time(4, 0, 0, 0)).unwrap();
        lead_time_warning(Some(ts(cutoff)), bake_date, bake, TWO_DAYS_AT_8PM, products).unwrap()
    }

    fn sourdough_warning(cutoff: &str, latest: &str) -> Option<LeadTimeWarning> {
        Some(LeadTimeWarning {
            rule: LeadTimeRule::Sourdough(TWO_DAYS_AT_8PM),
            cutoff_at: ts(cutoff),
            latest_cutoff: ts(latest),
        })
    }

    #[test]
    fn deadline_after_thursday_8pm_warns_for_a_saturday_bake() {
        // Saturday 2026-10-31 is in winter time: Thursday 20:00 CET is 19:00 UTC.
        let saturday = date(2026, 10, 31);
        assert_eq!(warning("2026-10-29T19:00:00Z", saturday, &[]), None);
        assert_eq!(
            warning("2026-10-29T19:01:00Z", saturday, &[]),
            sourdough_warning("2026-10-29T19:01:00Z", "2026-10-29T19:00:00Z")
        );
        assert_eq!(
            warning("2026-10-30T19:00:00Z", saturday, &[]),
            sourdough_warning("2026-10-30T19:00:00Z", "2026-10-29T19:00:00Z")
        );
        // A week earlier is summer time: Thursday 20:00 CEST is 18:00 UTC.
        assert_eq!(
            warning("2026-10-22T18:01:00Z", date(2026, 10, 24), &[]),
            sourdough_warning("2026-10-22T18:01:00Z", "2026-10-22T18:00:00Z")
        );
    }

    #[test]
    fn sourdough_deadline_around_the_march_change() {
        assert_eq!(
            warning("2026-03-26T19:00:00Z", date(2026, 3, 28), &[]),
            None
        );
        assert_eq!(
            warning("2026-04-02T19:00:00Z", date(2026, 4, 4), &[]),
            sourdough_warning("2026-04-02T19:00:00Z", "2026-04-02T18:00:00Z")
        );
        // Tuesday bake: the deadline falls on Sunday 29 March, the day the clocks change.
        assert_eq!(
            warning("2026-03-29T18:00:00Z", date(2026, 3, 31), &[]),
            None
        );
        let gap = SourdoughDeadline {
            days_before: 0,
            at: time(2, 30, 0, 0),
        };
        let sunday = date(2026, 3, 29);
        let bake = ts("2026-03-29T03:00:00Z");
        assert_eq!(
            lead_time_warning(Some(ts("2026-03-29T01:31:00Z")), sunday, bake, gap, &[])
                .unwrap()
                .map(|w| w.latest_cutoff),
            Some(ts("2026-03-29T01:30:00Z"))
        );
    }

    #[test]
    fn a_product_lead_time_can_be_stricter() {
        let saturday = date(2026, 10, 31);
        assert_eq!(
            warning("2026-10-29T19:00:00Z", saturday, &[(ROLLS, 12)]),
            None
        );
        // 60 h before Saturday 04:00 CET is Wednesday 16:00 CET.
        assert_eq!(
            warning(
                "2026-10-29T19:00:00Z",
                saturday,
                &[(ROLLS, 12), (BREAD, 60)]
            ),
            Some(LeadTimeWarning {
                rule: LeadTimeRule::Product {
                    product_id: BREAD,
                    lead_time_hours: 60,
                },
                cutoff_at: ts("2026-10-29T19:00:00Z"),
                latest_cutoff: ts("2026-10-28T15:00:00Z"),
            })
        );
        // 32 h before Saturday 04:00 CET is exactly Thursday 20:00 CET.
        assert_eq!(
            warning("2026-10-29T19:01:00Z", saturday, &[(BREAD, 32)]),
            sourdough_warning("2026-10-29T19:01:00Z", "2026-10-29T19:00:00Z")
        );
    }

    #[test]
    fn no_deadline_no_warning() {
        let saturday = date(2026, 10, 31);
        let bake = ts("2026-10-31T03:00:00Z");
        assert_eq!(
            lead_time_warning(None, saturday, bake, TWO_DAYS_AT_8PM, &[(BREAD, 168)]).unwrap(),
            None
        );
    }
}
