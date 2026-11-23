#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use cli::backup::{self, BackupRun, Error, Repo, RestoreTest, rustic};
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
        assert_eq!(verified, 2);

        let uploads = rustic::latest_snapshot(&fixture.rustic_repo(repo), &["uploads", "run-1"])
            .expect("list uploads snapshots");
        assert!(uploads.is_some(), "uploads snapshot missing in {repo}");
    }
    assert!(!scratch_exists("scratch_round_trip").await);
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

    let forged = r#"{"tables":{"bakery.Sentinel":{"rows":1,"md5":"0"},"public.loaves":{"rows":4,"md5":"0"}}}"#;
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
    assert!(matches!(error, Error::Mismatch(_)), "{message}");
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
    assert!(stderr.starts_with("error: rustic failed"), "{stderr}");
}
