#![forbid(unsafe_code)]

pub mod time;

use std::time::Duration;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::PgPoolOptions;

pub use sqlx::PgPool;

pub static MIGRATOR: Migrator = sqlx::migrate!();

const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] MigrateError),
}

pub async fn connect(url: &str) -> Result<PgPool, Error> {
    Ok(PgPoolOptions::new()
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .connect(url)
        .await?)
}

/// Opens no connection until the pool is first used, so a server can start while
/// the database is down and report that through `ping`.
pub fn connect_lazy(url: &str, acquire_timeout: Duration) -> Result<PgPool, Error> {
    Ok(PgPoolOptions::new()
        .acquire_timeout(acquire_timeout)
        .connect_lazy(url)?)
}

pub async fn migrate(pool: &PgPool) -> Result<(), Error> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

pub async fn ping(pool: &PgPool) -> Result<(), Error> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}
