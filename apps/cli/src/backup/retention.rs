use std::collections::BTreeMap;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use super::rustic::Snapshot;
use super::select::run_of;
use super::{Error, MANIFEST_TAG};

/// How many hourly, daily and weekly runs to keep, counted like restic's `--keep-*`: the
/// newest run of each hour, day or ISO week in the bakery's time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub hourly: usize,
    pub daily: usize,
    pub weekly: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    /// The time of the run's first snapshot.
    pub time: Timestamp,
    pub complete: bool,
    /// Tags other than the run tag, sorted, e.g. `db`, `kanidm`, `manifest`.
    pub parts: Vec<String>,
    pub snapshots: Vec<String>,
}

/// Snapshots without a run tag are not ours and are left out. Newest first.
pub fn runs(snapshots: &[Snapshot]) -> Vec<Run> {
    let mut by_run: BTreeMap<&str, Run> = BTreeMap::new();
    for snapshot in snapshots {
        let Some(id) = run_of(snapshot) else {
            continue;
        };
        let run = by_run.entry(id).or_insert_with(|| Run {
            id: id.to_string(),
            time: snapshot.time,
            complete: false,
            parts: Vec::new(),
            snapshots: Vec::new(),
        });
        run.time = run.time.min(snapshot.time);
        run.snapshots.push(snapshot.id.clone());
        for tag in &snapshot.tags {
            if run_of_tag(tag).is_none() && !run.parts.contains(tag) {
                run.parts.push(tag.clone());
            }
            if tag == MANIFEST_TAG {
                run.complete = true;
            }
        }
    }
    let mut runs: Vec<Run> = by_run.into_values().collect();
    for run in &mut runs {
        run.parts.sort();
        run.snapshots.sort();
    }
    runs.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| b.id.cmp(&a.id)));
    runs
}

fn run_of_tag(tag: &str) -> Option<&str> {
    tag.strip_prefix(super::RUN_TAG_PREFIX)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Run ids, newest first.
    pub keep: Vec<String>,
    pub forget_runs: Vec<String>,
    /// Snapshot ids of every forgotten run, sorted.
    pub forget: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bucket {
    Hour(Date, i8),
    Day(Date),
    Week(i16, i8),
}

/// Complete runs are thinned out by the policy. Incomplete runs older than the newest
/// complete run are forgotten; newer ones may still be running and stay.
pub fn plan(snapshots: &[Snapshot], policy: Policy, time_zone: &TimeZone) -> Result<Plan, Error> {
    if policy.hourly == 0 && policy.daily == 0 && policy.weekly == 0 {
        return Err(Error::KeepsNothing);
    }
    let all = runs(snapshots);
    let newest_complete = all.iter().find(|run| run.complete).map(|run| run.time);
    let mut left = [policy.hourly, policy.daily, policy.weekly];
    let mut last: [Option<Bucket>; 3] = [None; 3];
    let mut result = Plan::default();
    for run in &all {
        let keep = if run.complete {
            let local = run.time.to_zoned(time_zone.clone());
            let week = local.date().iso_week_date();
            let buckets = [
                Bucket::Hour(local.date(), local.hour()),
                Bucket::Day(local.date()),
                Bucket::Week(week.year(), week.week()),
            ];
            let mut keep = false;
            for (i, bucket) in buckets.into_iter().enumerate() {
                if left[i] > 0 && last[i] != Some(bucket) {
                    keep = true;
                    last[i] = Some(bucket);
                    left[i] -= 1;
                }
            }
            keep
        } else {
            newest_complete.is_none_or(|newest| run.time > newest)
        };
        if keep {
            result.keep.push(run.id.clone());
        } else {
            result.forget_runs.push(run.id.clone());
            result.forget.extend(run.snapshots.iter().cloned());
        }
    }
    result.forget.sort();
    Ok(result)
}

/// One line of `backup list`: the run id to pass to `backup restore --run`, its local time
/// and what it holds.
pub fn describe(run: &Run, time_zone: &TimeZone) -> String {
    let local = run.time.to_zoned(time_zone.clone());
    format!(
        "{:<20}  {}  {:<10}  {}",
        run.id,
        local.strftime("%Y-%m-%d %H:%M:%S %Z"),
        if run.complete {
            "complete"
        } else {
            "incomplete"
        },
        run.parts.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_shows_local_time_and_parts() {
        let mut snapshots = complete("1795640400000000000", ts("2026-11-25T19:00:00Z")).to_vec();
        snapshots.push(snapshot(
            "d2",
            ts("2026-07-01T10:00:00Z"),
            &["db", "run-1782900000000000000"],
        ));
        let lines: Vec<String> = runs(&snapshots)
            .iter()
            .map(|run| describe(run, &amsterdam()))
            .collect();
        assert_eq!(
            lines,
            [
                "1795640400000000000   2026-11-25 20:00:00 CET  complete    db, manifest",
                "1782900000000000000   2026-07-01 12:00:00 CEST  incomplete  db",
            ]
        );
    }

    const LOCAL: Policy = Policy {
        hourly: 24,
        daily: 14,
        weekly: 8,
    };

    fn amsterdam() -> TimeZone {
        jiff::tz::get!("Europe/Amsterdam")
    }

    fn snapshot(id: &str, time: Timestamp, tags: &[&str]) -> Snapshot {
        Snapshot {
            id: id.into(),
            time,
            tags: tags.iter().map(ToString::to_string).collect(),
        }
    }

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    /// A complete run: db and manifest snapshots, one second apart.
    fn complete(id: &str, time: Timestamp) -> [Snapshot; 2] {
        let tag = format!("run-{id}");
        [
            snapshot(&format!("d{id}"), time, &["db", &tag]),
            snapshot(
                &format!("m{id}"),
                time + jiff::SignedDuration::from_secs(1),
                &["manifest", &tag],
            ),
        ]
    }

    fn hourly_runs(start: Timestamp, count: i64) -> Vec<Snapshot> {
        (0..count)
            .flat_map(|hour| {
                complete(
                    &hour.to_string(),
                    start + jiff::SignedDuration::from_hours(hour),
                )
            })
            .collect()
    }

    #[test]
    fn runs_group_snapshots_by_run_tag_newest_first() {
        let mut snapshots = complete("1", ts("2026-11-25T10:00:00Z")).to_vec();
        snapshots.push(snapshot(
            "k1",
            ts("2026-11-25T10:00:00.5Z"),
            &["kanidm", "run-1"],
        ));
        snapshots.push(snapshot("d2", ts("2026-11-25T11:00:00Z"), &["db", "run-2"]));
        snapshots.push(snapshot("x", ts("2026-11-25T12:00:00Z"), &["db"]));
        assert_eq!(
            runs(&snapshots),
            [
                Run {
                    id: "2".into(),
                    time: ts("2026-11-25T11:00:00Z"),
                    complete: false,
                    parts: vec!["db".into()],
                    snapshots: vec!["d2".into()],
                },
                Run {
                    id: "1".into(),
                    time: ts("2026-11-25T10:00:00Z"),
                    complete: true,
                    parts: vec!["db".into(), "kanidm".into(), "manifest".into()],
                    snapshots: vec!["d1".into(), "k1".into(), "m1".into()],
                },
            ]
        );
    }

    #[test]
    fn a_week_of_hourly_runs_keeps_24_hours_then_one_per_day() {
        // 2026-11-19 00:30 to 2026-11-25 23:30 Amsterdam time (UTC+1), 168 runs.
        let snapshots = hourly_runs(ts("2026-11-18T23:30:00Z"), 168);
        let plan = plan(&snapshots, LOCAL, &amsterdam()).expect("plan");
        let expected: Vec<String> = (144..168)
            .rev()
            .chain([143, 119, 95, 71, 47, 23])
            .map(|hour: i64| hour.to_string())
            .collect();
        assert_eq!(plan.keep, expected);
        assert_eq!(plan.forget_runs.len(), 168 - 30);
        assert_eq!(plan.forget.len(), 2 * (168 - 30));
        assert!(plan.forget.contains(&"d0".to_string()));
        assert!(plan.forget.contains(&"m0".to_string()));
        assert!(!plan.forget.contains(&"d23".to_string()));
    }

    #[test]
    fn days_and_weeks_follow_amsterdam_time_not_utc() {
        // 23:30 UTC on 30 November is 00:30 on 1 December in Amsterdam: a new day.
        let mut snapshots = complete("a", ts("2026-11-30T22:30:00Z")).to_vec();
        snapshots.extend(complete("b", ts("2026-11-30T23:30:00Z")));
        let policy = Policy {
            hourly: 0,
            daily: 2,
            weekly: 0,
        };
        let plan = plan(&snapshots, policy, &amsterdam()).expect("plan");
        assert_eq!(plan.keep, ["b", "a"]);
        assert!(plan.forget.is_empty());
    }

    #[test]
    fn weekly_keeps_the_newest_run_of_each_iso_week() {
        // ISO weeks start on Monday: 15 Nov is in week 46, 16-22 Nov in week 47, 23 Nov on in week 48.
        let mut snapshots = Vec::new();
        for (id, at) in [
            ("1", "2026-11-15T12:00:00Z"),
            ("2", "2026-11-16T12:00:00Z"),
            ("3", "2026-11-22T12:00:00Z"),
            ("4", "2026-11-23T12:00:00Z"),
            ("5", "2026-11-24T12:00:00Z"),
        ] {
            snapshots.extend(complete(id, ts(at)));
        }
        let policy = Policy {
            hourly: 0,
            daily: 0,
            weekly: 3,
        };
        let plan = plan(&snapshots, policy, &amsterdam()).expect("plan");
        assert_eq!(plan.keep, ["5", "3", "1"]);
        assert_eq!(plan.forget_runs, ["4", "2"]);
    }

    #[test]
    fn the_repeated_hour_at_the_end_of_summer_time_counts_once() {
        // 2026-10-25 02:30 CEST and 02:30 CET are both 02:xx on the wall clock.
        let mut snapshots = complete("cest", ts("2026-10-25T00:30:00Z")).to_vec();
        snapshots.extend(complete("cet", ts("2026-10-25T01:30:00Z")));
        let policy = Policy {
            hourly: 5,
            daily: 0,
            weekly: 0,
        };
        let plan = plan(&snapshots, policy, &amsterdam()).expect("plan");
        assert_eq!(plan.keep, ["cet"]);
        assert_eq!(plan.forget_runs, ["cest"]);
    }

    #[test]
    fn incomplete_runs_stay_only_while_newer_than_the_newest_complete_run() {
        let mut snapshots = vec![snapshot(
            "old",
            ts("2026-11-25T08:00:00Z"),
            &["db", "run-0"],
        )];
        snapshots.extend(complete("1", ts("2026-11-25T09:00:00Z")));
        snapshots.push(snapshot(
            "busy",
            ts("2026-11-25T10:00:00Z"),
            &["db", "run-2"],
        ));
        snapshots.push(snapshot("foreign", ts("2026-11-25T07:00:00Z"), &["db"]));
        let plan = plan(&snapshots, LOCAL, &amsterdam()).expect("plan");
        assert_eq!(plan.keep, ["2", "1"]);
        assert_eq!(plan.forget_runs, ["0"]);
        assert_eq!(plan.forget, ["old"]);
    }

    #[test]
    fn without_a_complete_run_nothing_is_forgotten() {
        let snapshots = [snapshot("d1", ts("2026-11-25T08:00:00Z"), &["db", "run-1"])];
        let plan = plan(&snapshots, LOCAL, &amsterdam()).expect("plan");
        assert_eq!(plan.keep, ["1"]);
        assert!(plan.forget.is_empty());
        assert_eq!(
            super::plan(&[], LOCAL, &amsterdam()).expect("empty"),
            Plan::default()
        );
    }

    #[test]
    fn a_policy_that_keeps_nothing_is_refused() {
        let snapshots = hourly_runs(ts("2026-11-25T08:00:00Z"), 2);
        let none = Policy {
            hourly: 0,
            daily: 0,
            weekly: 0,
        };
        let error = plan(&snapshots, none, &amsterdam()).expect_err("keeps nothing");
        assert_eq!(
            error.to_string(),
            "the retention policy keeps no backups; set at least one --keep-* above 0"
        );
    }
}
