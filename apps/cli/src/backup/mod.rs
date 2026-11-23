pub mod manifest;
pub mod postgres;
mod process;
mod report;
pub mod rustic;
pub mod select;

use std::path::Path;

use jiff::{SignedDuration, Timestamp};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

pub use manifest::{Manifest, Mismatch, Mismatches};
pub use postgres::with_database;
pub use rustic::Repo;
pub use select::{RUN_TAG_PREFIX, run_id, scratch_database};

pub const DB_TAG: &str = "db";
pub const DB_FILE: &str = "db.dump";
pub const MANIFEST_TAG: &str = "manifest";
pub const MANIFEST_FILE: &str = "manifest.json";
pub const UPLOADS_TAG: &str = "uploads";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not start {program}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("{program} failed ({status}): {stderr}")]
    Failed {
        program: String,
        status: std::process::ExitStatus,
        stderr: String,
    },
    #[error("{producer}\n{consumer}")]
    Pipe {
        producer: Box<Error>,
        consumer: Box<Error>,
    },
    #[error("{program} could not read every file: {stderr}")]
    Unreadable { program: String, stderr: String },
    #[error(
        "{source}\nalso could not forget the partial snapshots of this run in {repo}: {cleanup}"
    )]
    Cleanup {
        repo: String,
        source: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("the database URL must look like postgres://user@host/database")]
    DatabaseUrl,
    #[error("the database URL must not name a database in its query (dbname=, database=)")]
    DatabaseInQuery,
    #[error("the system clock says {0}, before 1970; fix the clock")]
    ClockBeforeEpoch(Timestamp),
    #[error("could not read the rustic snapshot list: {0}")]
    SnapshotList(serde_json::Error),
    #[error("manifest of {snapshot} is not valid: {source}")]
    ManifestJson {
        snapshot: String,
        source: serde_json::Error,
    },
    #[error("no snapshot tagged {tags} in {repo}")]
    NoSnapshot { tags: String, repo: String },
    #[error("no db snapshot in {repo} has a manifest:\n  {skipped}")]
    NoUsableSnapshot { repo: String, skipped: String },
    #[error(
        "the newest backup in {repo} is incomplete; the last complete one is run {complete_run}:\n  {skipped}"
    )]
    NewestIncomplete {
        repo: String,
        complete_run: String,
        skipped: String,
    },
    #[error(
        "the newest usable backup, db snapshot {snapshot} of run {run}, is {age:#} old; \
         the limit is {max_age:#}"
    )]
    TooOld {
        snapshot: String,
        run: String,
        age: SignedDuration,
        max_age: SignedDuration,
    },
    #[error(
        "db snapshot {snapshot} of run {run} is dated {ahead:#} in the future; the limit is {limit:#}. \
         Check the clocks of this machine and the backup host"
    )]
    FromTheFuture {
        snapshot: String,
        run: String,
        ahead: SignedDuration,
        limit: SignedDuration,
    },
    #[error("run {run} in {repo} backed up uploads, but no uploads snapshot of that run is left")]
    MissingUploads { repo: String, run: String },
    #[error("the manifest lists no tables, so there is nothing to verify")]
    EmptyManifest,
    #[error("the database restored from {repo} does not match the manifest:{mismatches}")]
    Mismatch {
        repo: String,
        mismatches: Mismatches,
    },
    #[error("migrations failed on the restored database: {0}")]
    Migrate(db::Error),
    #[error("backup failed for {failed} of {total} repositories:\n  {details}")]
    BackupsFailed {
        failed: usize,
        total: usize,
        details: String,
    },
    #[error("restore test failed for {failed} of {total} repositories")]
    RestoreTestsFailed { failed: usize, total: usize },
}

#[derive(Debug, Clone, Copy)]
pub struct BackupRun<'a> {
    pub repos: &'a [String],
    pub password_file: &'a Path,
    pub database_url: &'a str,
    pub uploads: Option<&'a Path>,
    pub run_id: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct RestoreTest<'a> {
    pub repo: &'a str,
    pub password_file: &'a Path,
    pub admin_url: &'a str,
    pub scratch_database: &'a str,
    pub max_age: SignedDuration,
    pub now: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub tables: usize,
    pub run: String,
    pub age: SignedDuration,
}

/// Every repository gets the same database state: the manifest is read in the transaction
/// whose snapshot pg_dump reuses, and the transaction stays open until the last dump ends.
pub async fn run(backup: &BackupRun<'_>) -> Result<(), Error> {
    let mut conn = PgConnection::connect(backup.database_url).await?;
    let mut tx = conn.begin().await?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .await?;
    tx.execute("SET LOCAL idle_in_transaction_session_timeout = 0")
        .await?;
    let snapshot: String = sqlx::query_scalar("SELECT pg_export_snapshot()")
        .fetch_one(&mut *tx)
        .await?;
    let mut manifest = postgres::read_manifest(&mut tx).await?;
    manifest.uploads = backup.uploads.is_some();
    let manifest_json =
        serde_json::to_vec_pretty(&manifest).map_err(|source| Error::ManifestJson {
            snapshot: snapshot.clone(),
            source,
        })?;
    let run_tag = format!("{RUN_TAG_PREFIX}{}", backup.run_id);
    let source = postgres::Libpq::parse(backup.database_url)?;
    let dump = postgres::pg_dump(&source.url, &snapshot);

    let mut failures = Vec::new();
    for location in backup.repos {
        let repo = Repo {
            location,
            password_file: backup.password_file,
        };
        if let Err(error) = store_run(
            &repo,
            &source,
            &dump,
            &manifest_json,
            &run_tag,
            backup.uploads,
        ) {
            failures.push(format!(
                "{location}: {}: {}",
                error.step,
                report::summary(&error.error)
            ));
        }
    }
    tx.rollback().await?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::BackupsFailed {
            failed: failures.len(),
            total: backup.repos.len(),
            details: failures.join("\n  "),
        })
    }
}

struct StepFailed {
    step: &'static str,
    error: Error,
}

/// All or nothing: on any failure every snapshot of this run in `repo` is forgotten. The
/// manifest goes last, so a run without one never counts as complete.
fn store_run(
    repo: &Repo<'_>,
    source: &postgres::Libpq,
    dump: &[String],
    manifest_json: &[u8],
    run_tag: &str,
    uploads: Option<&Path>,
) -> Result<(), StepFailed> {
    let stored = (|| {
        let mut dump_command =
            rustic::backup_command_output(repo, dump, DB_FILE, &[DB_TAG, run_tag]);
        source.apply(&mut dump_command);
        process::run(dump_command).map_err(|error| ("db", error))?;
        if let Some(uploads) = uploads {
            backup_uploads(repo, uploads, run_tag).map_err(|error| ("uploads", error))?;
        }
        process::run_with_input(
            rustic::backup_stdin(repo, MANIFEST_FILE, &[MANIFEST_TAG, run_tag]),
            manifest_json,
        )
        .map(drop)
        .map_err(|error| ("manifest", error))
    })();
    stored.map_err(|(step, error)| StepFailed {
        step,
        error: forget_partial_run(repo, run_tag, error),
    })
}

/// rustic skips files it cannot read and still exits 0.
fn backup_uploads(repo: &Repo<'_>, uploads: &Path, run_tag: &str) -> Result<(), Error> {
    let stderr = process::run_stderr(rustic::backup_dir(repo, uploads, &[UPLOADS_TAG, run_tag]))?;
    if rustic::skipped_files(&stderr) {
        return Err(Error::Unreadable {
            program: "rustic".to_string(),
            stderr,
        });
    }
    Ok(())
}

/// A failed dump still leaves a snapshot behind (rustic stores what it read, maybe 0 bytes).
fn forget_partial_run(repo: &Repo<'_>, run_tag: &str, error: Error) -> Error {
    let forgotten = rustic::list_snapshots(repo, &[run_tag]).and_then(|partial| {
        if partial.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = partial.into_iter().map(|snapshot| snapshot.id).collect();
        process::run(rustic::forget(repo, &ids)).map(drop)
    });
    match forgotten {
        Ok(()) => error,
        Err(cleanup) => Error::Cleanup {
            repo: repo.location.to_string(),
            source: Box::new(error),
            cleanup: Box::new(cleanup),
        },
    }
}

/// Drops scratch databases left behind by restore tests that were killed.
pub async fn drop_stale_scratch_databases(
    admin: &mut PgConnection,
    now: Timestamp,
) -> Result<Vec<String>, Error> {
    let names: Vec<String> = sqlx::query_scalar("SELECT datname::text FROM pg_database")
        .fetch_all(&mut *admin)
        .await?;
    let stale: Vec<String> = names
        .into_iter()
        .filter(|name| select::is_stale_scratch(name, now))
        .collect();
    for name in &stale {
        admin
            .execute(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                postgres::quote_ident(name)
            )))
            .await?;
    }
    Ok(stale)
}

pub async fn restore_test(test: &RestoreTest<'_>) -> Result<Verified, Error> {
    let scratch_url = with_database(test.admin_url, test.scratch_database)?;
    let repo = Repo {
        location: test.repo,
        password_file: test.password_file,
    };
    let choice = select::choose(
        test.repo,
        &rustic::list_snapshots(&repo, &[DB_TAG])?,
        &rustic::list_snapshots(&repo, &[MANIFEST_TAG])?,
    )?;
    let age = select::check_age(&choice, test.now, test.max_age)?;
    let manifest_json = process::run(rustic::dump(&repo, &choice.manifest.id, MANIFEST_FILE))?;
    let expected: Manifest =
        serde_json::from_slice(&manifest_json).map_err(|source| Error::ManifestJson {
            snapshot: choice.manifest.id.clone(),
            source,
        })?;
    if expected.tables.is_empty() {
        return Err(Error::EmptyManifest);
    }
    if expected.uploads {
        let run_tag = format!("{RUN_TAG_PREFIX}{}", choice.run);
        if rustic::list_snapshots(&repo, &[UPLOADS_TAG, &run_tag])?.is_empty() {
            return Err(Error::MissingUploads {
                repo: test.repo.to_string(),
                run: choice.run,
            });
        }
    }

    let scratch = postgres::quote_ident(test.scratch_database);
    let mut admin = PgConnection::connect(test.admin_url).await?;
    drop_stale_scratch_databases(&mut admin, test.now).await?;
    admin
        .execute(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {scratch} WITH (FORCE)"
        )))
        .await?;
    admin
        .execute(AssertSqlSafe(format!("CREATE DATABASE {scratch}")))
        .await?;
    let verified = restore_and_compare(&repo, &choice.dump.id, &scratch_url, &expected).await;
    let dropped = admin
        .execute(AssertSqlSafe(format!(
            "DROP DATABASE {scratch} WITH (FORCE)"
        )))
        .await;
    let tables = verified?;
    dropped?;
    Ok(Verified {
        tables,
        run: choice.run,
        age,
    })
}

async fn restore_and_compare(
    repo: &Repo<'_>,
    snapshot_id: &str,
    scratch_url: &str,
    expected: &Manifest,
) -> Result<usize, Error> {
    process::pipe(
        rustic::dump(repo, snapshot_id, DB_FILE),
        postgres::pg_restore(&postgres::Libpq::parse(scratch_url)?),
    )?;
    let mut conn = PgConnection::connect(scratch_url).await?;
    let restored = postgres::read_manifest(&mut conn).await?;
    conn.close().await?;
    let mismatches = manifest::compare(expected, &restored);
    if mismatches.is_empty() {
        migrate_and_smoke_test(scratch_url).await?;
        Ok(expected.tables.len())
    } else {
        Err(Error::Mismatch {
            repo: repo.location.to_string(),
            mismatches: Mismatches(mismatches),
        })
    }
}

async fn migrate_and_smoke_test(scratch_url: &str) -> Result<(), Error> {
    let pool = db::connect(scratch_url).await.map_err(Error::Migrate)?;
    let result = async {
        db::migrate(&pool).await?;
        db::ping(&pool).await
    }
    .await;
    pool.close().await;
    result.map_err(Error::Migrate)
}
