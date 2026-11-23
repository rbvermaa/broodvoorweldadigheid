use std::path::Path;
use std::process::Command;

use jiff::Timestamp;
use serde::Deserialize;

use super::{Error, process};

pub const HOST: &str = "bakflow";

#[derive(Debug, Clone, Copy)]
pub struct Repo<'a> {
    pub location: &'a str,
    pub password_file: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub time: Timestamp,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn rustic(repo: &Repo<'_>) -> Command {
    let mut command = Command::new("rustic");
    command
        .args(["--repository", repo.location, "--password-file"])
        .arg(repo.password_file)
        .args(["--no-cache", "--no-progress", "--log-level", "warn"]);
    command
}

fn backup(repo: &Repo<'_>, tags: &[&str]) -> Command {
    let mut command = rustic(repo);
    command.args(["backup", "--init", "--host", HOST, "--tag", &tags.join(",")]);
    command
}

/// The snapshot holds one file, `filename`, with the bytes rustic reads from its stdin.
/// `--force`: with a parent snapshot rustic would keep the parent's copy without reading stdin.
pub fn backup_stdin(repo: &Repo<'_>, filename: &str, tags: &[&str]) -> Command {
    let mut command = backup(repo, tags);
    command.args(["--force", "--stdin-filename", filename, "-"]);
    command
}

/// rustic runs `program` itself, so a failing dump fails the backup.
pub fn backup_command_output(
    repo: &Repo<'_>,
    program: &[String],
    filename: &str,
    tags: &[&str],
) -> Command {
    let mut command = backup(repo, tags);
    command.args([
        "--force",
        "--stdin-command",
        &shell_join(program),
        "--stdin-filename",
        filename,
        "-",
    ]);
    command
}

pub fn backup_dir(repo: &Repo<'_>, dir: &Path, tags: &[&str]) -> Command {
    let mut command = backup(repo, tags);
    command.arg(dir);
    command
}

/// All tags must match. Never use `latest` without tags: it spans every snapshot group.
pub fn snapshots(repo: &Repo<'_>, tags: &[&str]) -> Command {
    let mut command = rustic(repo);
    command.args([
        "snapshots",
        "--json",
        "--group-by",
        "",
        "--filter-tags",
        &tags.join(","),
    ]);
    command
}

pub fn dump(repo: &Repo<'_>, snapshot_id: &str, filename: &str) -> Command {
    let mut command = rustic(repo);
    command.args(["dump", &format!("{snapshot_id}:{filename}")]);
    command
}

pub fn forget(repo: &Repo<'_>, snapshot_ids: &[String]) -> Command {
    let mut command = rustic(repo);
    command.arg("forget").args(snapshot_ids);
    command
}

/// Oldest first.
pub fn parse_snapshots(json: &[u8]) -> Result<Vec<Snapshot>, Error> {
    let mut snapshots: Vec<Snapshot> = serde_json::from_slice(json).map_err(Error::SnapshotList)?;
    snapshots.sort_by_key(|snapshot| snapshot.time);
    Ok(snapshots)
}

pub fn list_snapshots(repo: &Repo<'_>, tags: &[&str]) -> Result<Vec<Snapshot>, Error> {
    parse_snapshots(&process::run(snapshots(repo, tags))?)
}

pub fn latest_snapshot(repo: &Repo<'_>, tags: &[&str]) -> Result<Option<Snapshot>, Error> {
    Ok(list_snapshots(repo, tags)?.pop())
}

/// rustic logs files it could not read as `[WARN] ignoring error...` and still succeeds.
pub fn skipped_files(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|line| line.trim_start().starts_with("[WARN] ignoring error"))
}

/// rustic splits `--stdin-command` with POSIX shell word rules.
pub fn shell_join(words: &[String]) -> String {
    words
        .iter()
        .map(|word| format!("'{}'", word.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> Repo<'static> {
        Repo {
            location: "/backup/local",
            password_file: Path::new("/run/secrets/rustic"),
        }
    }

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    const GLOBAL: [&str; 8] = [
        "--repository",
        "/backup/local",
        "--password-file",
        "/run/secrets/rustic",
        "--no-cache",
        "--no-progress",
        "--log-level",
        "warn",
    ];

    fn with_global(rest: &[&str]) -> Vec<String> {
        GLOBAL.iter().chain(rest).map(ToString::to_string).collect()
    }

    #[test]
    fn backup_stdin_reads_from_stdin_with_tags() {
        let command = backup_stdin(&repo(), "manifest.json", &["manifest", "run-1"]);
        assert_eq!(command.get_program(), "rustic");
        assert_eq!(
            args(&command),
            with_global(&[
                "backup",
                "--init",
                "--host",
                "bakflow",
                "--tag",
                "manifest,run-1",
                "--force",
                "--stdin-filename",
                "manifest.json",
                "-",
            ])
        );
    }

    #[test]
    fn backup_command_output_quotes_the_program() {
        let program = [
            "pg_dump".to_string(),
            "--dbname=postgres://x/y?host=/s&a=b".to_string(),
        ];
        let command = backup_command_output(&repo(), &program, "db.dump", &["db", "run-1"]);
        assert_eq!(
            args(&command),
            with_global(&[
                "backup",
                "--init",
                "--host",
                "bakflow",
                "--tag",
                "db,run-1",
                "--force",
                "--stdin-command",
                "'pg_dump' '--dbname=postgres://x/y?host=/s&a=b'",
                "--stdin-filename",
                "db.dump",
                "-",
            ])
        );
    }

    #[test]
    fn backup_dir_adds_the_directory() {
        let command = backup_dir(&repo(), Path::new("/data/uploads"), &["uploads"]);
        assert_eq!(
            args(&command),
            with_global(&[
                "backup",
                "--init",
                "--host",
                "bakflow",
                "--tag",
                "uploads",
                "/data/uploads"
            ])
        );
    }

    #[test]
    fn snapshots_filters_on_all_tags() {
        assert_eq!(
            args(&snapshots(&repo(), &["manifest", "run-9"])),
            with_global(&[
                "snapshots",
                "--json",
                "--group-by",
                "",
                "--filter-tags",
                "manifest,run-9"
            ])
        );
    }

    #[test]
    fn dump_names_snapshot_and_file() {
        assert_eq!(
            args(&dump(&repo(), "8dd7c1", "db.dump")),
            with_global(&["dump", "8dd7c1:db.dump"])
        );
    }

    #[test]
    fn shell_join_escapes_single_quotes() {
        assert_eq!(
            shell_join(&["a b".to_string(), "it's".to_string()]),
            r"'a b' 'it'\''s'"
        );
    }

    #[test]
    fn skipped_files_spots_read_errors_only() {
        assert!(!skipped_files(""));
        assert!(!skipped_files("[WARN] some other warning"));
        assert!(skipped_files(
            "[WARN] ignoring error: Error: Failed to get next entry from walk iterator.\n\
             caused by: /data/uploads/sub: Permission denied (os error 13)"
        ));
        assert!(skipped_files(
            "[WARN] ignoring error obtaining xargs: Error getting xattrs for `/data/uploads/a`"
        ));
    }

    #[test]
    fn forget_names_every_snapshot() {
        assert_eq!(
            args(&forget(&repo(), &["aaa".into(), "bbb".into()])),
            with_global(&["forget", "aaa", "bbb"])
        );
    }

    #[test]
    fn parse_snapshots_sorts_oldest_first_and_reads_time() {
        let json = br#"[
            {"id": "bbb", "time": "2026-11-24T01:00:00.123456789+01:00", "tags": ["db", "run-2"]},
            {"id": "aaa", "time": "2026-11-23T01:00:00+01:00", "tags": ["db", "run-1"]}
        ]"#;
        assert_eq!(
            parse_snapshots(json).expect("valid"),
            [
                Snapshot {
                    id: "aaa".into(),
                    time: "2026-11-23T00:00:00Z".parse().expect("time"),
                    tags: vec!["db".into(), "run-1".into()],
                },
                Snapshot {
                    id: "bbb".into(),
                    time: "2026-11-24T00:00:00.123456789Z".parse().expect("time"),
                    tags: vec!["db".into(), "run-2".into()],
                },
            ]
        );
    }

    #[test]
    fn parse_snapshots_handles_empty_list_and_missing_tags() {
        assert_eq!(parse_snapshots(b"[]").expect("valid"), []);
        assert_eq!(
            parse_snapshots(br#"[{"id": "ccc", "time": "2026-11-23T01:00:00+01:00"}]"#)
                .expect("valid"),
            [Snapshot {
                id: "ccc".into(),
                time: "2026-11-23T00:00:00Z".parse().expect("time"),
                tags: vec![],
            }]
        );
    }

    #[test]
    fn parse_snapshots_rejects_garbage() {
        let error = parse_snapshots(b"not json").expect_err("invalid");
        assert!(
            error
                .to_string()
                .starts_with("could not read the rustic snapshot list: "),
            "{error}"
        );
    }
}
