#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use cli::backup::{self, BackupRun, Error, Repo, RestoreTest, rustic};
use jiff::{SignedDuration, Timestamp};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

fn admin_url() -> String {
    std::env::var("DATABASE_URL").expect(
        "DATABASE_URL must point at a Postgres superuser connection; `nix flake check` starts one",
    )
}

struct Fixture {
    dir: PathBuf,
    scratch_database: String,
    uploads: PathBuf,
    source_url: String,
    password_file: PathBuf,
}

impl Fixture {
    async fn new(name: &str, seed: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("cli-backup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("uploads")).expect("create fixture dir");
        std::fs::write(dir.join("uploads/photo.txt"), "not really a photo").expect("write upload");
        let password_file = dir.join("password");
        std::fs::write(&password_file, "test-only-password\n").expect("write password");

        let database = format!("source_{name}");
        let mut admin = PgConnection::connect(&admin_url())
            .await
            .expect("connect admin");
        admin
            .execute(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {database} WITH (FORCE)"
            )))
            .await
            .expect("drop source db");
        admin
            .execute(AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .await
            .expect("create source db");
        let source_url = backup::with_database(&admin_url(), &database).expect("source url");
        let mut source = PgConnection::connect(&source_url)
            .await
            .expect("connect source");
        source
            .execute(AssertSqlSafe(seed.to_string()))
            .await
            .expect("seed source db");
        Fixture {
            scratch_database: format!("scratch_{name}"),
            uploads: dir.join("uploads"),
            dir,
            source_url,
            password_file,
        }
    }

    fn repo(&self, name: &str) -> String {
        self.dir.join(name).display().to_string()
    }

    fn backup<'a>(&'a self, repos: &'a [String], run_id: &'a str) -> BackupRun<'a> {
        BackupRun {
            repos,
            password_file: &self.password_file,
            database_url: &self.source_url,
            uploads: Some(&self.uploads),
            run_id,
        }
    }

    fn restore_test<'a>(&'a self, repo: &'a str, admin_url: &'a str) -> RestoreTest<'a> {
        RestoreTest {
            repo,
            password_file: &self.password_file,
            admin_url,
            scratch_database: &self.scratch_database,
            max_age: SignedDuration::from_hours(26),
            now: Timestamp::now(),
        }
    }

    fn rustic_repo<'a>(&'a self, location: &'a str) -> Repo<'a> {
        Repo {
            location,
            password_file: &self.password_file,
        }
    }
}

const SEED: &str = "
    CREATE TABLE loaves (id integer PRIMARY KEY, name text NOT NULL);
    INSERT INTO loaves VALUES (1, 'sourdough'), (2, 'spelt'), (3, 'rye');
    CREATE SCHEMA bakery;
    CREATE TABLE bakery.\"Sentinel\" (value text NOT NULL);
    INSERT INTO bakery.\"Sentinel\" VALUES ('restore-sentinel');
";

async fn scratch_exists(name: &str) -> bool {
    let mut admin = PgConnection::connect(&admin_url())
        .await
        .expect("connect admin");
    sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
        .bind(name)
        .fetch_one(&mut admin)
        .await
        .expect("query pg_database")
}

#[tokio::test]
async fn backup_then_restore_test_passes_for_every_repo() {
    let fixture = Fixture::new("round_trip", SEED).await;
    let repos = [fixture.repo("local"), fixture.repo("offsite")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup");

    let admin = admin_url();
    for repo in &repos {
        let verified = backup::restore_test(&fixture.restore_test(repo, &admin))
            .await
            .expect("restore test");
        assert_eq!(verified.tables, 2);
        assert_eq!(verified.run, "1");

        let uploads = rustic::latest_snapshot(&fixture.rustic_repo(repo), &["uploads", "run-1"])
            .expect("list uploads snapshots");
        assert!(uploads.is_some(), "uploads snapshot missing in {repo}");
    }
    assert!(!scratch_exists("scratch_round_trip").await);
}

#[tokio::test]
async fn manifest_fingerprint_ignores_row_order_but_not_contents() {
    let first = Fixture::new(
        "order_a",
        "CREATE TABLE loaves (id integer, name text);
         INSERT INTO loaves VALUES (1, 'sourdough'), (2, 'spelt'), (3, 'rye');",
    )
    .await;
    let second = Fixture::new(
        "order_b",
        "CREATE TABLE loaves (id integer, name text);
         INSERT INTO loaves VALUES (3, 'rye'), (1, 'sourdough'), (2, 'spelt');",
    )
    .await;
    let read = |url: String| async move {
        let mut conn = PgConnection::connect(&url).await.expect("connect");
        backup::postgres::read_manifest(&mut conn)
            .await
            .expect("manifest")
    };
    let a = read(first.source_url.clone()).await;
    let b = read(second.source_url.clone()).await;
    assert_eq!(a, b);

    let mut conn = PgConnection::connect(&second.source_url)
        .await
        .expect("connect");
    conn.execute("UPDATE loaves SET name = 'Rye' WHERE id = 3")
        .await
        .expect("update");
    let changed = read(second.source_url.clone()).await;
    assert_eq!(changed.tables["public.loaves"].rows, 3);
    assert_ne!(
        changed.tables["public.loaves"].fingerprint,
        a.tables["public.loaves"].fingerprint
    );
}

#[tokio::test]
async fn restore_test_ignores_database_level_output_settings() {
    let seed = "
        CREATE TABLE proofing (id integer PRIMARY KEY, rest interval, starter bytea);
        INSERT INTO proofing VALUES (1, '14 hours 30 minutes', '\\x01ff');
    ";
    let fixture = Fixture::new("output_settings", seed).await;
    let mut admin = PgConnection::connect(&admin_url())
        .await
        .expect("connect admin");
    admin
        .execute(
            "ALTER DATABASE source_output_settings SET IntervalStyle = 'iso_8601';
             ALTER DATABASE source_output_settings SET bytea_output = 'escape';",
        )
        .await
        .expect("set database defaults");
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup");

    let admin = admin_url();
    backup::restore_test(&fixture.restore_test(&repos[0], &admin))
        .await
        .expect("restore test");
}

#[tokio::test]
async fn restore_test_ignores_a_database_level_search_path() {
    let seed = "
        CREATE TABLE ovens (id integer PRIMARY KEY);
        CREATE TABLE refs (target regclass NOT NULL);
        INSERT INTO refs VALUES ('public.ovens');
    ";
    let fixture = Fixture::new("search_path", seed).await;
    let mut admin = PgConnection::connect(&admin_url())
        .await
        .expect("connect admin");
    admin
        .execute("ALTER DATABASE source_search_path SET search_path = pg_catalog")
        .await
        .expect("set database search_path");
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup");

    let admin = admin_url();
    backup::restore_test(&fixture.restore_test(&repos[0], &admin))
        .await
        .expect("restore test");
}

fn with_password(url: &str, password: &str) -> String {
    let with = url.replacen("://postgres@", &format!("://postgres:{password}@"), 1);
    assert_ne!(with, url, "DATABASE_URL must use the postgres user: {url}");
    with
}

#[tokio::test]
async fn backup_keeps_the_database_password_out_of_snapshots() {
    let password = "test-only-db-secret";
    let fixture = Fixture::new("password", SEED).await;
    let repos = [fixture.repo("local")];
    let source_url = with_password(&fixture.source_url, password);
    backup::run(&BackupRun {
        database_url: &source_url,
        ..fixture.backup(&repos, "1")
    })
    .await
    .expect("backup");

    let admin = with_password(&admin_url(), password);
    backup::restore_test(&fixture.restore_test(&repos[0], &admin))
        .await
        .expect("restore test");

    let snapshots = Command::new("rustic")
        .args(["--repository", &repos[0], "--password-file"])
        .arg(&fixture.password_file)
        .args(["--no-cache", "--no-progress", "snapshots", "--json"])
        .output()
        .expect("rustic snapshots");
    assert!(snapshots.status.success());
    let json = String::from_utf8_lossy(&snapshots.stdout);
    assert!(json.contains("pg_dump"), "{json}");
    assert!(!json.contains(password), "{json}");
}

#[tokio::test]
async fn restore_test_uses_the_latest_backup_run() {
    let fixture = Fixture::new("latest_run", SEED).await;
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("first backup");
    let mut source = PgConnection::connect(&fixture.source_url)
        .await
        .expect("connect");
    source
        .execute("INSERT INTO loaves VALUES (4, 'wholewheat')")
        .await
        .expect("insert");
    backup::run(&fixture.backup(&repos, "2"))
        .await
        .expect("second backup");

    let admin = admin_url();
    backup::restore_test(&fixture.restore_test(&repos[0], &admin))
        .await
        .expect("restore test");
}

#[tokio::test]
async fn restore_test_fails_when_the_manifest_disagrees_with_the_dump() {
    let fixture = Fixture::new("forged_manifest", SEED).await;
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "7"))
        .await
        .expect("backup");

    let forged = r#"{"tables":{"bakery.Sentinel":{"rows":1,"fingerprint":"0"},"public.loaves":{"rows":4,"fingerprint":"0"}}}"#;
    let status = rustic::backup_stdin(
        &fixture.rustic_repo(&repos[0]),
        backup::MANIFEST_FILE,
        &[backup::MANIFEST_TAG, "run-7"],
    )
    .stdin(std::process::Stdio::piped())
    .spawn()
    .and_then(|mut child| {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(forged.as_bytes())?;
        child.wait()
    })
    .expect("store forged manifest");
    assert!(status.success());

    let admin = admin_url();
    let error = backup::restore_test(&fixture.restore_test(&repos[0], &admin))
        .await
        .expect_err("forged manifest must fail");
    let message = error.to_string();
    assert!(matches!(error, Error::Mismatch { .. }), "{message}");
    assert!(
        message.contains(&format!("restored from {} does not match", repos[0])),
        "{message}"
    );
    assert!(
        message.contains("public.loaves: expected 4 rows, restored 3"),
        "{message}"
    );
    assert!(
        message.contains("bakery.Sentinel: contents differ"),
        "{message}"
    );
    assert!(!scratch_exists("scratch_forged_manifest").await);
}

#[tokio::test]
async fn restore_test_fails_when_pack_files_are_missing() {
    let fixture = Fixture::new("missing_packs", SEED).await;
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup");
    std::fs::remove_dir_all(Path::new(&repos[0]).join("data")).expect("delete packs");

    let admin = admin_url();
    let error = backup::restore_test(&fixture.restore_test(&repos[0], &admin))
        .await
        .expect_err("missing packs must fail");
    assert!(matches!(error, Error::Failed { .. }), "{error}");
    let message = error.to_string();
    assert!(message.starts_with("rustic failed"), "{message}");
    assert!(
        message.contains(&format!("Failed to open the file `{}/data/", repos[0])),
        "{message}"
    );
    assert!(!scratch_exists("scratch_missing_packs").await);
}

#[tokio::test]
async fn restore_test_fails_when_the_repository_has_no_backup() {
    let fixture = Fixture::new("no_backup", SEED).await;
    let repo = fixture.repo("empty");
    let status = Command::new("rustic")
        .args(["init", "--no-progress", "-r", &repo, "-p"])
        .arg(&fixture.password_file)
        .status()
        .expect("rustic init");
    assert!(status.success());

    let admin = admin_url();
    let error = backup::restore_test(&fixture.restore_test(&repo, &admin))
        .await
        .expect_err("empty repo must fail");
    assert_eq!(
        error.to_string(),
        format!("no snapshot tagged db in {repo}")
    );
}

#[tokio::test]
async fn restore_test_fails_when_the_database_had_no_tables() {
    let fixture = Fixture::new("no_tables", "SELECT 1").await;
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup");

    let admin = admin_url();
    let error = backup::restore_test(&fixture.restore_test(&repos[0], &admin))
        .await
        .expect_err("empty manifest must fail");
    assert!(matches!(error, Error::EmptyManifest), "{error}");
}

#[tokio::test]
async fn backup_fails_when_the_database_is_unreachable() {
    let fixture = Fixture::new("unreachable", SEED).await;
    let repos = [fixture.repo("local")];
    let missing = backup::with_database(&admin_url(), "no_such_database").expect("url");
    let run = BackupRun {
        database_url: &missing,
        ..fixture.backup(&repos, "1")
    };
    let error = backup::run(&run).await.expect_err("must fail");
    assert!(matches!(error, Error::Database(_)), "{error}");
}

#[test]
fn binary_reports_restore_test_failure_with_non_zero_exit() {
    let dir = std::env::temp_dir().join(format!("cli-backup-binary-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create dir");
    let password_file = dir.join("password");
    std::fs::write(&password_file, "test-only-password\n").expect("write password");

    let output = Command::new(env!("CARGO_BIN_EXE_cli"))
        .args(["backup", "restore-test", "--repo"])
        .arg(dir.join("does-not-exist"))
        .arg("--password-file")
        .arg(&password_file)
        .args(["--database-url", &admin_url()])
        .output()
        .expect("run cli");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let missing = dir.join("does-not-exist");
    assert!(
        stderr.starts_with(&format!(
            "error: restore test failed for {}: rustic failed",
            missing.display()
        )),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("error: restore test failed for 1 of 1 repositories\n"),
        "{stderr}"
    );
}

#[tokio::test]
async fn restore_test_fails_when_migrations_do_not_apply_to_the_backup() {
    let seed = "
        CREATE TABLE loaves (id integer PRIMARY KEY);
        INSERT INTO loaves VALUES (1);
        CREATE TABLE schema_info (note text);
    ";
    let fixture = Fixture::new("bad_migration", seed).await;
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup");

    let admin = admin_url();
    let error = backup::restore_test(&fixture.restore_test(&repos[0], &admin))
        .await
        .expect_err("conflicting schema must fail the migrations");
    assert!(matches!(error, Error::Migrate(_)), "{error}");
    assert!(!scratch_exists("scratch_bad_migration").await);
}

fn store(repo: &Repo<'_>, filename: &str, tags: &[&str], contents: &[u8]) {
    use std::io::Write;
    let mut child = rustic::backup_stdin(repo, filename, tags)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("start rustic backup");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(contents)
        .expect("write snapshot contents");
    assert!(child.wait().expect("rustic backup").success());
}

fn snapshot_tags(repo: &str, password_file: &Path) -> Vec<Vec<String>> {
    let output = Command::new("rustic")
        .args(["--repository", repo, "--password-file"])
        .arg(password_file)
        .args(["--no-cache", "--no-progress", "snapshots", "--json"])
        .args(["--group-by", ""])
        .output()
        .expect("rustic snapshots");
    assert!(output.status.success());
    rustic::parse_snapshots(&output.stdout)
        .expect("snapshot list")
        .into_iter()
        .map(|snapshot| snapshot.tags)
        .collect()
}

#[tokio::test]
async fn failed_dump_forgets_the_partial_db_snapshot() {
    let fixture = Fixture::new("failed_dump", SEED).await;
    let repo = fixture.repo("local");
    let fake_bin = fixture.dir.join("fake-bin");
    std::fs::create_dir_all(&fake_bin).expect("create fake bin");
    let fake_pg_dump = fake_bin.join("pg_dump");
    std::fs::write(
        &fake_pg_dump,
        "#!/bin/sh\necho 'fake pg_dump fails' >&2\nexit 3\n",
    )
    .expect("write fake pg_dump");
    std::fs::set_permissions(
        &fake_pg_dump,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("make fake pg_dump executable");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").expect("PATH")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cli"))
        .args(["backup", "run", "--repo", &repo, "--password-file"])
        .arg(&fixture.password_file)
        .args(["--database-url", &fixture.source_url])
        .env("PATH", path)
        .output()
        .expect("run cli");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fake pg_dump fails"), "{stderr}");

    let tags = snapshot_tags(&repo, &fixture.password_file);
    assert!(
        tags.iter().all(|tags| !tags.iter().any(|tag| tag == "db")),
        "{tags:?}"
    );
}

#[tokio::test]
async fn restore_test_fails_when_the_newest_run_has_no_manifest() {
    let fixture = Fixture::new("orphan_dump", SEED).await;
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup");
    let repo = fixture.rustic_repo(&repos[0]);
    store(&repo, backup::DB_FILE, &["db", "run-2"], b"");
    let orphan = rustic::latest_snapshot(&repo, &["db", "run-2"])
        .expect("list")
        .expect("orphan snapshot");

    let admin = admin_url();
    let error = backup::restore_test(&fixture.restore_test(&repos[0], &admin))
        .await
        .expect_err("a failed last backup must not pass");
    assert_eq!(
        error.to_string(),
        format!(
            "the newest backup in {} is incomplete; the last complete one is run 1:\
             \n  db snapshot {} of run 2 has no manifest — backup failed?",
            repos[0], orphan.id
        )
    );
    assert!(!scratch_exists("scratch_orphan_dump").await);
}

#[tokio::test]
async fn backup_continues_with_the_next_repo_when_one_fails() {
    let fixture = Fixture::new("next_repo", SEED).await;
    let broken = fixture.repo("broken");
    std::fs::write(&broken, "not a repository").expect("block the first repo");
    let good = fixture.repo("good");
    let repos = [broken.clone(), good.clone()];

    let error = backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect_err("the broken repo must fail the run");
    let message = error.to_string();
    assert!(matches!(error, Error::BackupsFailed { .. }), "{message}");
    assert!(
        message.starts_with("backup failed for 1 of 2 repositories:\n  "),
        "{message}"
    );
    assert!(message.contains(&format!("\n  {broken}: ")), "{message}");
    assert!(!message.contains(&format!("{good}:")), "{message}");

    let tags = snapshot_tags(&good, &fixture.password_file);
    for kind in ["db", "manifest", "uploads"] {
        assert!(
            tags.iter()
                .any(|tags| tags.iter().any(|tag| tag == kind)
                    && tags.iter().any(|tag| tag == "run-1")),
            "{kind} snapshot missing in {good}: {tags:?}"
        );
    }
    let admin = admin_url();
    backup::restore_test(&fixture.restore_test(&good, &admin))
        .await
        .expect("the good repo still verifies");
}

#[tokio::test]
async fn restore_test_fails_when_no_db_snapshot_has_a_manifest() {
    let fixture = Fixture::new("only_orphans", SEED).await;
    let location = fixture.repo("local");
    let repo = fixture.rustic_repo(&location);
    store(&repo, backup::DB_FILE, &["db", "run-5"], b"");
    let orphan = rustic::latest_snapshot(&repo, &["db"])
        .expect("list")
        .expect("orphan snapshot");

    let admin = admin_url();
    let error = backup::restore_test(&fixture.restore_test(&location, &admin))
        .await
        .expect_err("nothing to verify");
    assert_eq!(
        error.to_string(),
        format!(
            "no db snapshot in {location} has a manifest:\
             \n  db snapshot {} of run 5 has no manifest — backup failed?",
            orphan.id
        )
    );
}

#[tokio::test]
async fn restore_test_fails_when_the_newest_backup_is_too_old() {
    let fixture = Fixture::new("too_old", SEED).await;
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup");

    let admin = admin_url();
    let later = Timestamp::now() + SignedDuration::from_hours(27);
    let error = backup::restore_test(&RestoreTest {
        now: later,
        ..fixture.restore_test(&repos[0], &admin)
    })
    .await
    .expect_err("27h old backup");
    assert!(matches!(error, Error::TooOld { .. }), "{error}");
    let message = error.to_string();
    assert!(message.contains("of run 1, is 2"), "{message}");
    assert!(message.ends_with("old; the limit is 26h"), "{message}");
    assert!(!scratch_exists("scratch_too_old").await);
}

#[tokio::test]
async fn restore_test_drops_only_stale_scratch_databases() {
    let fixture = Fixture::new("stale_scratch", SEED).await;
    let repos = [fixture.repo("local")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup");

    let now = Timestamp::now();
    let name_at = |at: Timestamp| backup::scratch_database(&at.as_nanosecond().to_string());
    let stale = name_at(now - SignedDuration::from_hours(2));
    let young = name_at(now - SignedDuration::from_mins(10));
    let other = "restore_test_keep_me".to_string();
    let mut admin = PgConnection::connect(&admin_url())
        .await
        .expect("connect admin");
    for name in [&stale, &young, &other] {
        admin
            .execute(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
            )))
            .await
            .expect("drop leftover");
        admin
            .execute(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .await
            .expect("create leftover");
    }

    let admin_url = admin_url();
    backup::restore_test(&RestoreTest {
        now,
        ..fixture.restore_test(&repos[0], &admin_url)
    })
    .await
    .expect("restore test");

    assert!(!scratch_exists(&stale).await);
    assert!(scratch_exists(&young).await);
    assert!(scratch_exists(&other).await);
    for name in [&young, &other] {
        admin
            .execute(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
            )))
            .await
            .expect("clean up");
    }
}

#[tokio::test]
async fn backup_keeps_its_snapshot_open_past_idle_in_transaction_timeout() {
    let fixture = Fixture::new("idle_timeout", SEED).await;
    let mut admin = PgConnection::connect(&admin_url())
        .await
        .expect("connect admin");
    admin
        .execute(
            "ALTER DATABASE source_idle_timeout SET idle_in_transaction_session_timeout = '100ms'",
        )
        .await
        .expect("set timeout");
    let repos = [fixture.repo("local"), fixture.repo("offsite")];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("backup outlives the idle timeout");
}

#[tokio::test]
async fn binary_checks_every_repo_before_failing() {
    let fixture = Fixture::new("every_repo", SEED).await;
    let good = fixture.repo("good");
    backup::run(&fixture.backup(std::slice::from_ref(&good), "1"))
        .await
        .expect("backup");
    let missing = fixture.repo("missing");

    let output = Command::new(env!("CARGO_BIN_EXE_cli"))
        .args([
            "backup",
            "restore-test",
            "--repo",
            &missing,
            "--repo",
            &good,
        ])
        .arg("--password-file")
        .arg(&fixture.password_file)
        .args(["--database-url", &admin_url()])
        .output()
        .expect("run cli");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains(&format!("restore test passed for {good}: run 1 (")),
        "{stdout}"
    );
    assert!(
        stderr.contains(&format!("error: restore test failed for {missing}: ")),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("error: restore test failed for 1 of 2 repositories\n"),
        "{stderr}"
    );
}

fn run_tags(repo: &str, password_file: &Path, run_tag: &str) -> Vec<Vec<String>> {
    snapshot_tags(repo, password_file)
        .into_iter()
        .filter(|tags| tags.iter().any(|tag| tag == run_tag))
        .collect()
}

#[tokio::test]
async fn missing_uploads_forget_the_run_and_restore_test_uses_the_previous_run() {
    let fixture = Fixture::new("missing_uploads", SEED).await;
    let repo = fixture.repo("local");
    let repos = [repo.clone()];
    backup::run(&fixture.backup(&repos, "1"))
        .await
        .expect("first backup");

    let missing = fixture.dir.join("no-such-uploads");
    let error = backup::run(&BackupRun {
        uploads: Some(&missing),
        ..fixture.backup(&repos, "2")
    })
    .await
    .expect_err("missing uploads must fail the run");
    let message = error.to_string();
    assert!(matches!(error, Error::BackupsFailed { .. }), "{message}");
    assert!(
        message.starts_with(&format!(
            "backup failed for 1 of 1 repositories:\n  {repo}: uploads: rustic failed"
        )),
        "{message}"
    );
    assert_eq!(message.lines().count(), 2, "{message}");
    assert!(!message.contains('\u{1b}'), "{message}");
    assert_eq!(
        run_tags(&repo, &fixture.password_file, "run-2"),
        Vec::<Vec<String>>::new()
    );

    let admin = admin_url();
    let verified = backup::restore_test(&fixture.restore_test(&repo, &admin))
        .await
        .expect("restore test falls back to the complete run");
    assert_eq!(verified.run, "1");
}

#[tokio::test]
async fn unreadable_uploads_fail_the_backup_and_leave_no_snapshots() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("unreadable_uploads", SEED).await;
    let repo = fixture.repo("local");
    let repos = [repo.clone()];
    let locked = fixture.uploads.join("locked");
    std::fs::create_dir_all(&locked).expect("create locked dir");
    std::fs::write(locked.join("label.txt"), "test label").expect("write upload");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock dir");

    let result = backup::run(&fixture.backup(&repos, "1")).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock dir");
    let error = result.expect_err("unreadable uploads must fail the run");
    let message = error.to_string();
    assert!(
        message.starts_with(&format!(
            "backup failed for 1 of 1 repositories:\n  {repo}: uploads: rustic could not read every file: "
        )),
        "{message}"
    );
    assert!(message.contains("Permission denied"), "{message}");
    assert_eq!(
        snapshot_tags(&repo, &fixture.password_file),
        Vec::<Vec<String>>::new()
    );

    let admin = admin_url();
    let error = backup::restore_test(&fixture.restore_test(&repo, &admin))
        .await
        .expect_err("no complete run");
    assert_eq!(
        error.to_string(),
        format!("no snapshot tagged db in {repo}")
    );
}

#[tokio::test]
async fn restore_test_fails_when_the_uploads_snapshot_of_the_run_is_missing() {
    let fixture = Fixture::new("lost_uploads", SEED).await;
    let repo = fixture.repo("local");
    backup::run(&fixture.backup(std::slice::from_ref(&repo), "1"))
        .await
        .expect("backup");
    let rustic_repo = fixture.rustic_repo(&repo);
    let uploads = rustic::latest_snapshot(&rustic_repo, &["uploads", "run-1"])
        .expect("list")
        .expect("uploads snapshot");
    let output = rustic::forget(&rustic_repo, &[uploads.id])
        .output()
        .expect("rustic forget");
    assert!(output.status.success());

    let admin = admin_url();
    let error = backup::restore_test(&fixture.restore_test(&repo, &admin))
        .await
        .expect_err("uploads are part of the run");
    assert!(matches!(error, Error::MissingUploads { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        format!("run 1 in {repo} backed up uploads, but no uploads snapshot of that run is left")
    );
    assert!(!scratch_exists("scratch_lost_uploads").await);
}

#[tokio::test]
async fn restore_test_needs_no_uploads_snapshot_when_none_were_backed_up() {
    let fixture = Fixture::new("no_uploads", SEED).await;
    let repo = fixture.repo("local");
    let repos = [repo.clone()];
    backup::run(&BackupRun {
        uploads: None,
        ..fixture.backup(&repos, "1")
    })
    .await
    .expect("backup");

    let admin = admin_url();
    let verified = backup::restore_test(&fixture.restore_test(&repo, &admin))
        .await
        .expect("restore test");
    assert_eq!(verified.run, "1");
}
