use jiff::{SignedDuration, Timestamp};

use super::Error;
use super::rustic::Snapshot;

pub const RUN_TAG_PREFIX: &str = "run-";
/// Followed by the run id: nanoseconds since the Unix epoch when the restore test started.
pub const SCRATCH_PREFIX: &str = "restore_test_";
pub const SCRATCH_MAX_AGE: SignedDuration = SignedDuration::from_hours(1);
pub const CLOCK_SKEW: SignedDuration = SignedDuration::from_mins(5);

pub fn run_id(now: Timestamp) -> Result<String, Error> {
    if now < Timestamp::UNIX_EPOCH {
        return Err(Error::ClockBeforeEpoch(now));
    }
    Ok(now.as_nanosecond().to_string())
}

pub fn scratch_database(run_id: &str) -> String {
    format!("{SCRATCH_PREFIX}{run_id}")
}

/// Only names this program generates count: anything else is never touched.
pub fn is_stale_scratch(name: &str, now: Timestamp) -> bool {
    let Some(digits) = name.strip_prefix(SCRATCH_PREFIX) else {
        return false;
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(created) = digits
        .parse::<i128>()
        .map_err(|_| ())
        .and_then(|nanos| Timestamp::from_nanosecond(nanos).map_err(|_| ()))
    else {
        return false;
    };
    now.duration_since(created) > SCRATCH_MAX_AGE
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub dump: Snapshot,
    pub manifest: Snapshot,
    pub run: String,
}

fn run_of(snapshot: &Snapshot) -> Option<&str> {
    snapshot
        .tags
        .iter()
        .find_map(|tag| tag.strip_prefix(RUN_TAG_PREFIX))
}

/// The newest db snapshot, which must have a manifest from the same run. With several
/// manifests for one run, the newest wins.
pub fn choose(repo: &str, dumps: &[Snapshot], manifests: &[Snapshot]) -> Result<Choice, Error> {
    let mut skipped = Vec::new();
    for dump in dumps.iter().rev() {
        let Some(run) = run_of(dump) else {
            skipped.push(format!("db snapshot {} has no run tag", dump.id));
            continue;
        };
        let manifest = manifests
            .iter()
            .rev()
            .find(|manifest| run_of(manifest) == Some(run));
        match manifest {
            Some(_) if !skipped.is_empty() => {
                return Err(Error::NewestIncomplete {
                    repo: repo.to_string(),
                    complete_run: run.to_string(),
                    skipped: skipped.join("\n  "),
                });
            }
            Some(manifest) => {
                return Ok(Choice {
                    dump: dump.clone(),
                    manifest: manifest.clone(),
                    run: run.to_string(),
                });
            }
            None => skipped.push(format!(
                "db snapshot {} of run {run} has no manifest — backup failed?",
                dump.id
            )),
        }
    }
    if skipped.is_empty() {
        Err(Error::NoSnapshot {
            tags: super::DB_TAG.to_string(),
            repo: repo.to_string(),
        })
    } else {
        Err(Error::NoUsableSnapshot {
            repo: repo.to_string(),
            skipped: skipped.join("\n  "),
        })
    }
}

pub fn check_age(
    choice: &Choice,
    now: Timestamp,
    max_age: SignedDuration,
) -> Result<SignedDuration, Error> {
    let age = SignedDuration::from_secs(now.duration_since(choice.dump.time).as_secs());
    if -age > CLOCK_SKEW {
        return Err(Error::FromTheFuture {
            snapshot: choice.dump.id.clone(),
            run: choice.run.clone(),
            ahead: -age,
            limit: CLOCK_SKEW,
        });
    }
    if age > max_age {
        return Err(Error::TooOld {
            snapshot: choice.dump.id.clone(),
            run: choice.run.clone(),
            age,
            max_age,
        });
    }
    Ok(age)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: &str, time: &str, tags: &[&str]) -> Snapshot {
        Snapshot {
            id: id.into(),
            time: time.parse().expect("time"),
            tags: tags.iter().map(ToString::to_string).collect(),
        }
    }

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    #[test]
    fn run_id_is_nanoseconds_since_the_epoch() {
        assert_eq!(
            run_id(ts("2026-11-23T02:00:00.000000123Z")).expect("after 1970"),
            "1795399200000000123"
        );
    }

    #[test]
    fn run_id_rejects_a_clock_before_1970() {
        let error = run_id(ts("1969-12-31T23:59:59Z")).expect_err("before 1970");
        assert_eq!(
            error.to_string(),
            "the system clock says 1969-12-31T23:59:59Z, before 1970; fix the clock"
        );
    }

    #[test]
    fn stale_scratch_needs_our_prefix_digits_and_an_hour() {
        let now = ts("2026-11-23T03:00:00Z");
        let nanos = |at: &str| scratch_database(&ts(at).as_nanosecond().to_string());
        assert!(is_stale_scratch(&nanos("2026-11-23T01:59:59Z"), now));
        assert!(!is_stale_scratch(&nanos("2026-11-23T02:00:00Z"), now));
        assert!(!is_stale_scratch(&nanos("2026-11-23T02:50:00Z"), now));
        for other in [
            "restore_test_",
            "restore_test_abc",
            "restore_test_12x",
            "restore_test_-5",
            "restore_testx1",
            "bakflow",
            "scratch_1",
            "restore_test_99999999999999999999999999999999999999999",
        ] {
            assert!(!is_stale_scratch(other, now), "{other}");
        }
    }

    #[test]
    fn choose_takes_the_newest_run_with_a_manifest() {
        let dumps = [
            snapshot("d1", "2026-11-21T02:00:00Z", &["db", "run-1"]),
            snapshot("d2", "2026-11-22T02:00:00Z", &["db", "run-2"]),
        ];
        let manifests = [
            snapshot("m1", "2026-11-21T02:00:01Z", &["manifest", "run-1"]),
            snapshot("m2", "2026-11-22T02:00:01Z", &["manifest", "run-2"]),
            snapshot("m2b", "2026-11-22T03:00:00Z", &["manifest", "run-2"]),
        ];
        let choice = choose("/repo", &dumps, &manifests).expect("choice");
        assert_eq!(choice.dump.id, "d2");
        assert_eq!(choice.manifest.id, "m2b");
        assert_eq!(choice.run, "2");
    }

    #[test]
    fn choose_fails_when_a_newer_run_is_incomplete() {
        let dumps = [
            snapshot("d1", "2026-11-21T02:00:00Z", &["db", "run-1"]),
            snapshot("d2", "2026-11-22T02:00:00Z", &["db"]),
            snapshot("d3", "2026-11-23T02:00:00Z", &["db", "run-3"]),
        ];
        let manifests = [snapshot(
            "m1",
            "2026-11-21T02:00:01Z",
            &["manifest", "run-1"],
        )];
        let error = choose("/repo", &dumps, &manifests).expect_err("newer run incomplete");
        assert_eq!(
            error.to_string(),
            "the newest backup in /repo is incomplete; the last complete one is run 1:\
             \n  db snapshot d3 of run 3 has no manifest — backup failed?\
             \n  db snapshot d2 has no run tag"
        );
    }

    #[test]
    fn choose_fails_without_a_usable_snapshot() {
        let error = choose("/repo", &[], &[]).expect_err("empty");
        assert_eq!(error.to_string(), "no snapshot tagged db in /repo");

        let dumps = [snapshot("d9", "2026-11-23T02:00:00Z", &["db", "run-9"])];
        let error = choose("/repo", &dumps, &[]).expect_err("no manifest");
        assert_eq!(
            error.to_string(),
            "no db snapshot in /repo has a manifest:\
             \n  db snapshot d9 of run 9 has no manifest — backup failed?"
        );
    }

    #[test]
    fn check_age_accepts_up_to_the_limit_and_rejects_older() {
        let dumps = [snapshot("d1", "2026-11-22T02:00:00Z", &["db", "run-1"])];
        let manifests = [snapshot(
            "m1",
            "2026-11-22T02:00:01Z",
            &["manifest", "run-1"],
        )];
        let choice = choose("/repo", &dumps, &manifests).expect("choice");
        let max_age: SignedDuration = "26h".parse().expect("duration");

        assert_eq!(
            check_age(&choice, ts("2026-11-23T04:00:00Z"), max_age).expect("fresh"),
            max_age
        );
        let error = check_age(&choice, ts("2026-11-23T04:00:01Z"), max_age).expect_err("old");
        assert_eq!(
            error.to_string(),
            "the newest usable backup, db snapshot d1 of run 1, is 26h 1s old; the limit is 26h"
        );
    }

    #[test]
    fn check_age_tolerates_small_clock_skew_but_rejects_future_backups() {
        let dumps = [snapshot("d1", "2026-11-23T02:05:00Z", &["db", "run-1"])];
        let manifests = [snapshot(
            "m1",
            "2026-11-23T02:05:01Z",
            &["manifest", "run-1"],
        )];
        let choice = choose("/repo", &dumps, &manifests).expect("choice");
        let max_age: SignedDuration = "26h".parse().expect("duration");

        assert_eq!(
            check_age(&choice, ts("2026-11-23T02:00:00Z"), max_age).expect("within skew"),
            SignedDuration::from_mins(-5)
        );
        let error = check_age(&choice, ts("2026-11-23T01:59:59Z"), max_age).expect_err("future");
        assert_eq!(
            error.to_string(),
            "db snapshot d1 of run 1 is dated 5m 1s in the future; the limit is 5m. \
             Check the clocks of this machine and the backup host"
        );
    }
}
