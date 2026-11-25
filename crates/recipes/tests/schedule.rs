//! Step times from a start time (F14, ADR 0049), in Europe/Amsterdam.
#![forbid(unsafe_code)]

use jiff::civil::time;
use jiff::{SignedDuration, Zoned};
use proptest::prelude::*;
use recipes::schedule::{days_after, default_start, schedule, start_on};

fn at(s: &str) -> Zoned {
    format!("{s}[Europe/Amsterdam]").parse().unwrap()
}

fn clock(z: &Zoned) -> String {
    z.strftime("%Y-%m-%d %H:%M %:z").to_string()
}

#[test]
fn each_step_starts_after_the_durations_before_it() {
    let start = at("2026-12-04T09:00");
    let plan = schedule(&start, &[Some(30), None, Some(240), Some(45)]).unwrap();
    let starts: Vec<String> = plan.starts.iter().map(clock).collect();
    assert_eq!(
        starts,
        [
            "2026-12-04 09:00 +01:00",
            "2026-12-04 09:30 +01:00",
            "2026-12-04 09:30 +01:00",
            "2026-12-04 13:30 +01:00",
        ]
    );
    assert_eq!(clock(&plan.end), "2026-12-04 14:15 +01:00");
    assert_eq!(plan.untimed, 1);
}

#[test]
fn no_steps_end_at_the_start() {
    let start = at("2026-12-04T09:00");
    let plan = schedule(&start, &[]).unwrap();
    assert!(plan.starts.is_empty());
    assert_eq!(plan.end, start);
    assert_eq!(plan.untimed, 0);
}

#[test]
fn a_cold_retard_crosses_midnight_into_the_next_day() {
    let start = at("2026-12-04T20:00");
    let plan = schedule(&start, &[Some(60), Some(14 * 60), Some(45)]).unwrap();
    assert_eq!(clock(&plan.starts[1]), "2026-12-04 21:00 +01:00");
    assert_eq!(clock(&plan.starts[2]), "2026-12-05 11:00 +01:00");
    assert_eq!(days_after(&start, &plan.starts[1]), 0);
    assert_eq!(days_after(&start, &plan.starts[2]), 1);
    let long = schedule(&start, &[Some(3 * 24 * 60)]).unwrap();
    assert_eq!(days_after(&start, &long.end), 3);
}

#[test]
fn durations_are_elapsed_time_when_summer_time_starts() {
    // 29 March 2026: 02:00 CET becomes 03:00 CEST.
    let start = at("2026-03-29T01:30");
    let plan = schedule(&start, &[Some(60), Some(60)]).unwrap();
    assert_eq!(clock(&plan.starts[1]), "2026-03-29 03:30 +02:00");
    assert_eq!(clock(&plan.end), "2026-03-29 04:30 +02:00");
}

#[test]
fn durations_are_elapsed_time_when_summer_time_ends() {
    // 25 October 2026: 03:00 CEST becomes 02:00 CET, so 02:30 happens twice.
    let start = at("2026-10-25T01:30");
    let plan = schedule(&start, &[Some(60), Some(60), Some(60)]).unwrap();
    let starts: Vec<String> = plan.starts.iter().map(clock).collect();
    assert_eq!(
        starts,
        [
            "2026-10-25 01:30 +02:00",
            "2026-10-25 02:30 +02:00",
            "2026-10-25 02:30 +01:00",
        ]
    );
    assert_eq!(clock(&plan.end), "2026-10-25 03:30 +01:00");
}

#[test]
fn a_chosen_time_lands_on_the_day_and_skips_the_gap() {
    let day = at("2026-12-04T15:12");
    assert_eq!(
        clock(&start_on(&day, time(6, 30, 0, 0))),
        "2026-12-04 06:30 +01:00"
    );
    let spring = at("2026-03-29T10:00");
    assert_eq!(
        clock(&start_on(&spring, time(2, 30, 0, 0))),
        "2026-03-29 03:30 +02:00"
    );
}

#[test]
fn the_default_start_is_now_to_the_nearest_five_minutes() {
    let cases = [
        ("2026-12-04T09:02:29", "2026-12-04 09:00 +01:00"),
        ("2026-12-04T09:02:30", "2026-12-04 09:05 +01:00"),
        ("2026-12-04T23:58", "2026-12-05 00:00 +01:00"),
        ("2026-12-04T09:05", "2026-12-04 09:05 +01:00"),
    ];
    for (now, expected) in cases {
        assert_eq!(clock(&default_start(&at(now))), expected, "{now}");
    }
}

#[test]
fn times_past_the_supported_range_give_no_schedule() {
    let end_of_time = at("9999-12-29T12:00");
    assert!(schedule(&end_of_time, &[Some(10_080)]).is_none());
}

fn durations() -> impl Strategy<Value = Vec<Option<u32>>> {
    prop::collection::vec(prop::option::of(1u32..=10_080), 0..40)
}

proptest! {
    #[test]
    fn the_end_is_the_start_plus_every_duration(
        minutes in 0i64..(366 * 24 * 60),
        steps in durations(),
    ) {
        let start = at("2026-01-01T00:00")
            .checked_add(SignedDuration::from_mins(minutes))
            .unwrap();
        let plan = schedule(&start, &steps).unwrap();
        let total: i64 = steps.iter().flatten().map(|m| i64::from(*m)).sum();
        prop_assert_eq!(plan.end.timestamp().duration_since(start.timestamp()), SignedDuration::from_mins(total));
        prop_assert_eq!(plan.starts.len(), steps.len());
        prop_assert_eq!(plan.untimed, steps.iter().filter(|d| d.is_none()).count());
        let mut previous = start.clone();
        for (index, step_start) in plan.starts.iter().enumerate() {
            prop_assert!(*step_start >= previous);
            let next = plan.starts.get(index + 1).unwrap_or(&plan.end);
            let length = next.timestamp().duration_since(step_start.timestamp());
            let expected = steps[index].map_or(0, i64::from);
            prop_assert_eq!(length, SignedDuration::from_mins(expected));
            previous = step_start.clone();
        }
        prop_assert!(days_after(&start, &plan.end) >= 0);
    }
}
