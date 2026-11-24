#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

const LOCK_FILE: &str = "migrations.lock";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn version(file: &str) -> Option<i64> {
    file.split_once('_')?.0.parse().ok()
}

/// Problems with the lock: changed, missing, unlisted or duplicate migrations.
/// `lock` has one `<sha384>  <file>` line per migration, as `sha384sum` prints it.
fn problems(
    lock: &str,
    files: &BTreeSet<String>,
    checksums: &BTreeMap<i64, String>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut listed = BTreeSet::new();
    for line in lock.lines().filter(|l| !l.trim().is_empty()) {
        let Some((hash, file)) = line.split_once("  ") else {
            problems.push(format!("malformed line in {LOCK_FILE}: {line:?}"));
            continue;
        };
        if !listed.insert(file.to_owned()) {
            problems.push(format!("{file} is listed twice in {LOCK_FILE}"));
        }
        if !files.contains(file) {
            problems.push(format!(
                "{file} is in {LOCK_FILE} but missing; migrations are never removed"
            ));
            continue;
        }
        if version(file)
            .and_then(|v| checksums.get(&v))
            .map(String::as_str)
            != Some(hash)
        {
            problems.push(format!(
                "{file} changed after it was locked; migrations only add (CLAUDE.md rule 7), \
                 so put the change in a new migration"
            ));
        }
    }
    for file in files.difference(&listed) {
        problems.push(format!(
            "{file} is not in {LOCK_FILE}; run `make migrations-lock` to append it"
        ));
    }
    problems
}

#[test]
fn every_migration_is_locked_and_unchanged() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let lock = fs::read_to_string(dir.join(LOCK_FILE)).unwrap_or_default();
    let files: BTreeSet<String> = fs::read_dir(&dir)
        .expect("read migrations")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".sql"))
        .collect();
    let checksums: BTreeMap<i64, String> = db::MIGRATOR
        .iter()
        .map(|m| (m.version, hex(&m.checksum)))
        .collect();
    let problems = problems(&lock, &files, &checksums);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_check_reports_each_kind_of_problem() {
    let files: BTreeSet<String> = ["1_a.sql", "2_b.sql", "3_c.sql"].map(str::to_owned).into();
    let checksums = BTreeMap::from([
        (1, "aa".to_owned()),
        (2, "bb".to_owned()),
        (3, "cc".to_owned()),
    ]);
    assert!(
        problems(
            "aa  1_a.sql\nbb  2_b.sql\ncc  3_c.sql\n",
            &files,
            &checksums
        )
        .is_empty()
    );

    let found = problems(
        "aa  1_a.sql\nxx  2_b.sql\naa  1_a.sql\ndd  4_d.sql\nnonsense\n",
        &files,
        &checksums,
    );
    let expected = [
        "2_b.sql changed after it was locked",
        "1_a.sql is listed twice",
        "4_d.sql is in migrations.lock but missing",
        "malformed line",
        "3_c.sql is not in migrations.lock; run `make migrations-lock`",
    ];
    assert_eq!(found.len(), expected.len(), "{found:#?}");
    for (problem, start) in found.iter().zip(expected) {
        assert!(problem.starts_with(start), "{problem}");
    }
}
