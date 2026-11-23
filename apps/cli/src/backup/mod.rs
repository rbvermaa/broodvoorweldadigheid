pub mod manifest;
pub mod postgres;
mod process;
pub mod rustic;

use std::path::Path;

use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

pub use manifest::{Manifest, Mismatch, Mismatches};
pub use postgres::with_database;
pub use rustic::Repo;

pub const DB_TAG: &str = "db";
pub const DB_FILE: &str = "db.dump";
pub const MANIFEST_TAG: &str = "manifest";
pub const MANIFEST_FILE: &str = "manifest.json";
pub const UPLOADS_TAG: &str = "uploads";
const RUN_TAG_PREFIX: &str = "run-";

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
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("the database URL must look like postgres://user@host/database")]
    DatabaseUrl,
    #[error("the database URL must not name a database in its query (dbname=, database=)")]
    DatabaseInQuery,
    #[error("could not read the rustic snapshot list: {0}")]
    SnapshotList(serde_json::Error),
    #[error("manifest of {snapshot} is not valid: {source}")]
    ManifestJson {
        snapshot: String,
        source: serde_json::Error,
    },
    #[error("no snapshot tagged {tags} in {repo}")]
    NoSnapshot { tags: String, repo: String },
    #[error("snapshot {0} has no run tag")]
    NoRunTag(String),
    #[error("the manifest lists no tables, so there is nothing to verify")]
    EmptyManifest,
    #[error("the restored database does not match the manifest:{0}")]
    Mismatch(Mismatches),
    #[error("migrations failed on the restored database: {0}")]
    Migrate(db::Error),
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
}

/// Every repository gets the same database state: the manifest is read in the transaction
/// whose snapshot pg_dump reuses, and the transaction stays open until the last dump ends.
pub async fn run(backup: &BackupRun<'_>) -> Result<(), Error> {
    let mut conn = PgConnection::connect(backup.database_url).await?;
    let mut tx = conn.begin().await?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .await?;
    let snapshot: String = sqlx::query_scalar("SELECT pg_export_snapshot()")
        .fetch_one(&mut *tx)
        .await?;
    let manifest = postgres::read_manifest(&mut tx).await?;
    let manifest_json =
        serde_json::to_vec_pretty(&manifest).map_err(|source| Error::ManifestJson {
            snapshot: snapshot.clone(),
            source,
        })?;
    let run_tag = format!("{RUN_TAG_PREFIX}{}", backup.run_id);
    let dump = postgres::pg_dump(backup.database_url, &snapshot);

    for location in backup.repos {
        let repo = Repo {
            location,
            password_file: backup.password_file,
        };
        process::run(rustic::backup_command_output(
            &repo,
            &dump,
            DB_FILE,
            &[DB_TAG, &run_tag],
        ))?;
        process::run_with_input(
            rustic::backup_stdin(&repo, MANIFEST_FILE, &[MANIFEST_TAG, &run_tag]),
            &manifest_json,
        )?;
        if let Some(uploads) = backup.uploads {
            process::run(rustic::backup_dir(&repo, uploads, &[UPLOADS_TAG, &run_tag]))?;
        }
    }
    tx.rollback().await?;
    Ok(())
}

fn require_latest(repo: &Repo<'_>, tags: &[&str]) -> Result<rustic::Snapshot, Error> {
    rustic::latest_snapshot(repo, tags)?.ok_or_else(|| Error::NoSnapshot {
        tags: tags.join(","),
        repo: repo.location.to_string(),
    })
}

/// Returns the number of tables verified.
pub async fn restore_test(test: &RestoreTest<'_>) -> Result<usize, Error> {
    let scratch_url = with_database(test.admin_url, test.scratch_database)?;
    let repo = Repo {
        location: test.repo,
        password_file: test.password_file,
    };
    let dump = require_latest(&repo, &[DB_TAG])?;
    let run_tag = dump
        .tags
        .iter()
        .find(|tag| tag.starts_with(RUN_TAG_PREFIX))
        .ok_or_else(|| Error::NoRunTag(dump.id.clone()))?;
    let manifest_snapshot = require_latest(&repo, &[MANIFEST_TAG, run_tag])?;
    let manifest_json = process::run(rustic::dump(&repo, &manifest_snapshot.id, MANIFEST_FILE))?;
    let expected: Manifest =
        serde_json::from_slice(&manifest_json).map_err(|source| Error::ManifestJson {
            snapshot: manifest_snapshot.id.clone(),
            source,
        })?;
    if expected.tables.is_empty() {
        return Err(Error::EmptyManifest);
    }

    let scratch = postgres::quote_ident(test.scratch_database);
    let mut admin = PgConnection::connect(test.admin_url).await?;
    admin
        .execute(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {scratch} WITH (FORCE)"
        )))
        .await?;
    admin
        .execute(AssertSqlSafe(format!("CREATE DATABASE {scratch}")))
        .await?;
    let verified = restore_and_compare(&repo, &dump.id, &scratch_url, &expected).await;
    let dropped = admin
        .execute(AssertSqlSafe(format!(
            "DROP DATABASE {scratch} WITH (FORCE)"
        )))
        .await;
    let verified = verified?;
    dropped?;
    Ok(verified)
}

async fn restore_and_compare(
    repo: &Repo<'_>,
    snapshot_id: &str,
    scratch_url: &str,
    expected: &Manifest,
) -> Result<usize, Error> {
    process::pipe(
        rustic::dump(repo, snapshot_id, DB_FILE),
        postgres::pg_restore(scratch_url),
    )?;
    let mut conn = PgConnection::connect(scratch_url).await?;
    let restored = postgres::read_manifest(&mut conn).await?;
    conn.close().await?;
    let mismatches = manifest::compare(expected, &restored);
    if mismatches.is_empty() {
        migrate_and_smoke_test(scratch_url).await?;
        Ok(expected.tables.len())
    } else {
        Err(Error::Mismatch(Mismatches(mismatches)))
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
