use std::time::Duration;

use db::PgPool;
use tower_sessions::cookie::SameSite;
use tower_sessions::cookie::time::Duration as CookieDuration;
use tower_sessions::session_store::ExpiredDeletion;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::PostgresStore;

use crate::config::Config;

pub const COOKIE_NAME: &str = "bvw_session";
/// With `always_save` every request through the layer moves the expiry forward, so this is
/// an idle timeout, not a lifetime.
const IDLE_DAYS: i64 = 14;
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The table comes from crates/db/migrations, so `PostgresStore::migrate` is never run.
pub fn store(pool: PgPool) -> PostgresStore {
    PostgresStore::new(pool)
        .with_schema_name("public")
        .and_then(|store| store.with_table_name("http_session"))
        .expect("schema and table names are valid identifiers")
}

pub fn layer(pool: PgPool, config: &Config) -> SessionManagerLayer<PostgresStore> {
    SessionManagerLayer::new(store(pool))
        .with_name(COOKIE_NAME)
        .with_http_only(true)
        .with_same_site(SameSite::Lax)
        .with_secure(config.cookie_secure)
        .with_path("/")
        .with_expiry(Expiry::OnInactivity(CookieDuration::days(IDLE_DAYS)))
        .with_always_save(true)
}

/// Keeps going when the database is down; the next tick tries again.
pub async fn delete_expired_forever(store: PostgresStore) {
    let mut tick = tokio::time::interval(CLEANUP_INTERVAL);
    loop {
        tick.tick().await;
        if let Err(error) = store.delete_expired().await {
            tracing::warn!(%error, "cannot delete expired sessions");
        }
    }
}
