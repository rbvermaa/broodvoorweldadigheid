use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// Compiled in from jiff's bundled tz database, so the result does not depend on
/// the zoneinfo files of the host, the build sandbox or the container image.
static BAKERY_TIME_ZONE: TimeZone = jiff::tz::get!("Europe/Amsterdam");

/// The bakery's time zone, [`crate::TIME_ZONE`].
#[must_use]
pub fn bakery_time_zone() -> TimeZone {
    BAKERY_TIME_ZONE.clone()
}

#[must_use]
pub fn in_bakery_time(timestamp: Timestamp) -> Zoned {
    timestamp.to_zoned(bakery_time_zone())
}

/// The source of "now". Domain code takes a clock instead of reading the system
/// time, so tests can control it.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;

    fn now_in_bakery(&self) -> Zoned {
        in_bakery_time(self.now())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TIME_ZONE;

    struct Fixed(Timestamp);

    impl Clock for Fixed {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    fn local(utc: &str) -> String {
        Fixed(utc.parse().unwrap()).now_in_bakery().to_string()
    }

    #[test]
    fn bakery_time_zone_is_amsterdam() {
        assert_eq!(bakery_time_zone().iana_name(), Some(TIME_ZONE));
    }

    #[test]
    fn winter_and_summer_offsets() {
        assert_eq!(
            local("2026-01-15T10:00:00Z"),
            "2026-01-15T11:00:00+01:00[Europe/Amsterdam]"
        );
        assert_eq!(
            local("2026-07-15T10:00:00Z"),
            "2026-07-15T12:00:00+02:00[Europe/Amsterdam]"
        );
    }

    #[test]
    fn spring_forward_on_last_sunday_of_march() {
        assert_eq!(
            local("2026-03-29T00:59:59Z"),
            "2026-03-29T01:59:59+01:00[Europe/Amsterdam]"
        );
        assert_eq!(
            local("2026-03-29T01:00:00Z"),
            "2026-03-29T03:00:00+02:00[Europe/Amsterdam]"
        );
    }

    #[test]
    fn fall_back_on_last_sunday_of_october() {
        assert_eq!(
            local("2026-10-25T00:30:00Z"),
            "2026-10-25T02:30:00+02:00[Europe/Amsterdam]"
        );
        assert_eq!(
            local("2026-10-25T01:30:00Z"),
            "2026-10-25T02:30:00+01:00[Europe/Amsterdam]"
        );
    }

    #[test]
    fn bakery_day_has_23_and_25_hours_on_change_days() {
        let tz = bakery_time_zone();
        let hours = |date: &str| {
            let day: jiff::civil::Date = date.parse().unwrap();
            let start = day.to_zoned(tz.clone()).unwrap();
            let end = day.tomorrow().unwrap().to_zoned(tz.clone()).unwrap();
            end.timestamp().duration_since(start.timestamp()).as_hours()
        };
        assert_eq!(hours("2026-03-29"), 23);
        assert_eq!(hours("2026-10-25"), 25);
        assert_eq!(hours("2026-10-24"), 24);
    }

    #[test]
    fn system_clock_reads_the_current_time() {
        let before = Timestamp::now();
        let now = SystemClock.now();
        let after = Timestamp::now();
        assert!(before <= now && now <= after);
        assert_eq!(
            SystemClock.now_in_bakery().time_zone().iana_name(),
            Some(TIME_ZONE)
        );
    }
}
