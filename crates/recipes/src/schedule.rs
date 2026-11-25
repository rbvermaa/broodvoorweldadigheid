//! Clock times for a recipe's steps from a start time (F14, ADR 0049). Pure: no I/O.
//!
//! Durations are elapsed minutes, so on a day the clocks change a step still takes as
//! long as it says, and the clock time jumps with the change.

use jiff::civil::Time;
use jiff::{RoundMode, SignedDuration, Unit, Zoned, ZonedRound};

/// When each step starts and when the last one ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// One start per step: the start time plus the durations of the steps before it.
    pub starts: Vec<Zoned>,
    pub end: Zoned,
    /// Steps without a duration; they add no time.
    pub untimed: usize,
}

/// The schedule for steps with these durations in minutes, or `None` when the times fall
/// outside the range jiff supports.
#[must_use]
pub fn schedule(start: &Zoned, durations: &[Option<u32>]) -> Option<Schedule> {
    let mut starts = Vec::with_capacity(durations.len());
    let mut at = start.clone();
    for minutes in durations {
        starts.push(at.clone());
        if let Some(minutes) = minutes {
            at = at
                .checked_add(SignedDuration::from_mins(i64::from(*minutes)))
                .ok()?;
        }
    }
    Some(Schedule {
        starts,
        end: at,
        untimed: durations.iter().filter(|d| d.is_none()).count(),
    })
}

/// `now` rounded to the nearest five minutes, the start a page shows when none is asked.
#[must_use]
pub fn default_start(now: &Zoned) -> Zoned {
    let round = ZonedRound::new()
        .smallest(Unit::Minute)
        .increment(5)
        .mode(RoundMode::HalfExpand);
    now.round(round).unwrap_or_else(|_| now.clone())
}

/// `time` on the date of `day`, in its time zone. A time the clocks skip on that day
/// moves forward by the gap.
#[must_use]
pub fn start_on(day: &Zoned, time: Time) -> Zoned {
    day.date()
        .to_datetime(time)
        .to_zoned(day.time_zone().clone())
        .unwrap_or_else(|_| day.clone())
}

/// Whole calendar days from the date of `start` to the date of `at`.
#[must_use]
pub fn days_after(start: &Zoned, at: &Zoned) -> i32 {
    start
        .date()
        .until(at.date())
        .map_or(0, |span| span.get_days())
}
