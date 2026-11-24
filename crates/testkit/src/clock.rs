use std::sync::{Mutex, PoisonError};

use domain::Clock;
use jiff::{SignedDuration, Timestamp};

/// A clock that only moves when a test sets or advances it.
#[derive(Debug)]
pub struct FakeClock {
    now: Mutex<Timestamp>,
}

impl FakeClock {
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    pub fn set(&self, now: Timestamp) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = now;
    }

    /// # Panics
    /// When the result is outside the range jiff supports.
    pub fn advance(&self, by: SignedDuration) {
        let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        *now = now
            .checked_add(by)
            .expect("fake clock advanced out of range");
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn returns_the_time_it_was_given() {
        let clock = FakeClock::new(ts("2026-03-28T10:00:00Z"));
        assert_eq!(clock.now(), ts("2026-03-28T10:00:00Z"));
        assert_eq!(clock.now(), ts("2026-03-28T10:00:00Z"));
    }

    #[test]
    fn set_and_advance() {
        let clock = FakeClock::new(ts("2026-03-28T10:00:00Z"));
        clock.set(ts("2026-03-29T00:30:00Z"));
        assert_eq!(clock.now(), ts("2026-03-29T00:30:00Z"));
        clock.advance(SignedDuration::from_mins(45));
        assert_eq!(clock.now(), ts("2026-03-29T01:15:00Z"));
    }

    #[test]
    fn advancing_over_spring_forward_skips_an_hour_of_local_time() {
        let clock = FakeClock::new(ts("2026-03-29T00:30:00Z"));
        assert_eq!(clock.now_in_bakery().hour(), 1);
        clock.advance(SignedDuration::from_hours(1));
        assert_eq!(clock.now_in_bakery().hour(), 3);
    }

    #[test]
    fn advancing_over_fall_back_repeats_an_hour_of_local_time() {
        let clock = FakeClock::new(ts("2026-10-25T00:30:00Z"));
        let first = clock.now_in_bakery();
        clock.advance(SignedDuration::from_hours(1));
        let second = clock.now_in_bakery();
        assert_eq!(first.datetime(), second.datetime());
        assert_ne!(first.offset(), second.offset());
    }

    #[test]
    fn shared_between_threads() {
        let clock = Arc::new(FakeClock::new(ts("2026-01-01T00:00:00Z")));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let clock = Arc::clone(&clock);
                std::thread::spawn(move || clock.advance(SignedDuration::from_secs(1)))
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(clock.now(), ts("2026-01-01T00:00:04Z"));
    }

    #[test]
    fn usable_as_a_dyn_clock() {
        let clock: Box<dyn Clock> = Box::new(FakeClock::new(ts("2026-06-01T12:00:00Z")));
        assert_eq!(clock.now(), ts("2026-06-01T12:00:00Z"));
    }
}
