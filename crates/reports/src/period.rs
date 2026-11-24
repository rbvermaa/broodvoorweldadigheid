use jiff::Timestamp;
use jiff::civil::Date;

use crate::Error;

/// Calendar days `from` through `to`, both included, in the bakery's time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    from: Date,
    to: Date,
}

impl Period {
    /// # Errors
    /// [`Error::Invalid`] when `to` is before `from`.
    pub fn new(from: Date, to: Date) -> Result<Self, Error> {
        if to < from {
            return Err(Error::Invalid("period"));
        }
        Ok(Self { from, to })
    }

    #[must_use]
    pub const fn from(&self) -> Date {
        self.from
    }

    #[must_use]
    pub const fn to(&self) -> Date {
        self.to
    }

    #[must_use]
    pub fn contains(&self, date: Date) -> bool {
        self.from <= date && date <= self.to
    }

    /// The instants `[start, end)` of the period: local midnight at the start of `from`
    /// up to local midnight after `to`. Summer and winter time make days 23 or 25 hours.
    ///
    /// # Errors
    /// [`Error::Invalid`] at the edges of jiff's date range.
    pub fn instants(&self) -> Result<(Timestamp, Timestamp), Error> {
        let tz = domain::bakery_time_zone();
        let start = self
            .from
            .to_zoned(tz.clone())
            .map_err(|_| Error::Invalid("period"))?;
        let end = self
            .to
            .tomorrow()
            .and_then(|d| d.to_zoned(tz))
            .map_err(|_| Error::Invalid("period"))?;
        Ok((start.timestamp(), end.timestamp()))
    }
}

/// How a report groups days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Granularity {
    Day,
    Month,
    Quarter,
}

impl Granularity {
    /// The first day of the day, month or quarter that holds `date`.
    #[must_use]
    pub fn start(self, date: Date) -> Date {
        match self {
            Self::Day => date,
            Self::Month => date.first_of_month(),
            Self::Quarter => {
                let month = (date.month() - 1) / 3 * 3 + 1;
                Date::new(date.year(), month, 1).unwrap_or(date)
            }
        }
    }

    /// The last day of the bucket that starts on `start`.
    #[must_use]
    pub fn end(self, start: Date) -> Date {
        match self {
            Self::Day => start,
            Self::Month => start.last_of_month(),
            Self::Quarter => {
                let last_month = (start.month() - 1) / 3 * 3 + 3;
                Date::new(start.year(), last_month, 1).map_or(start, |d| d.last_of_month())
            }
        }
    }

    /// The bucket holding `date`, cut to `period`.
    #[must_use]
    pub fn bucket(self, date: Date, period: &Period) -> Period {
        let start = self.start(date);
        Period {
            from: start.max(period.from),
            to: self.end(start).min(period.to),
        }
    }
}

/// The local calendar date of an instant.
pub(crate) fn local_date(at: Timestamp) -> Date {
    domain::in_bakery_time(at).date()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn a_period_never_ends_before_it_starts() {
        assert!(Period::new(date(2026, 10, 2), date(2026, 10, 1)).is_err());
        let one_day = Period::new(date(2026, 10, 1), date(2026, 10, 1)).unwrap();
        assert!(one_day.contains(date(2026, 10, 1)));
        assert!(!one_day.contains(date(2026, 10, 2)));
    }

    #[test]
    fn instants_follow_summer_and_winter_time() {
        let october = Period::new(date(2026, 10, 1), date(2026, 10, 31)).unwrap();
        assert_eq!(
            october.instants().unwrap(),
            (ts("2026-09-30T22:00:00Z"), ts("2026-10-31T23:00:00Z"))
        );
        let change = Period::new(date(2026, 10, 25), date(2026, 10, 25)).unwrap();
        let (start, end) = change.instants().unwrap();
        assert_eq!(end.duration_since(start).as_hours(), 25);
        let spring = Period::new(date(2026, 3, 29), date(2026, 3, 29)).unwrap();
        let (start, end) = spring.instants().unwrap();
        assert_eq!(end.duration_since(start).as_hours(), 23);
        assert!(
            Period::new(Date::MAX, Date::MAX)
                .unwrap()
                .instants()
                .is_err()
        );
    }

    #[test]
    fn local_dates_cross_midnight_in_amsterdam() {
        assert_eq!(local_date(ts("2026-10-24T22:30:00Z")), date(2026, 10, 25));
        assert_eq!(local_date(ts("2026-10-25T23:30:00Z")), date(2026, 10, 26));
        assert_eq!(local_date(ts("2026-10-25T22:30:00Z")), date(2026, 10, 25));
    }

    #[test]
    fn buckets() {
        let year = Period::new(date(2026, 1, 1), date(2026, 12, 31)).unwrap();
        let day = date(2026, 8, 17);
        assert_eq!(
            Granularity::Day.bucket(day, &year),
            Period::new(day, day).unwrap()
        );
        assert_eq!(
            Granularity::Month.bucket(day, &year),
            Period::new(date(2026, 8, 1), date(2026, 8, 31)).unwrap()
        );
        assert_eq!(
            Granularity::Quarter.bucket(day, &year),
            Period::new(date(2026, 7, 1), date(2026, 9, 30)).unwrap()
        );
        assert_eq!(
            Granularity::Quarter.bucket(date(2026, 12, 31), &year),
            Period::new(date(2026, 10, 1), date(2026, 12, 31)).unwrap()
        );
        assert_eq!(
            Granularity::Quarter.bucket(date(2026, 2, 28), &year),
            Period::new(date(2026, 1, 1), date(2026, 3, 31)).unwrap()
        );
        let part = Period::new(date(2026, 8, 10), date(2026, 8, 20)).unwrap();
        assert_eq!(Granularity::Quarter.bucket(day, &part), part);
    }
}
