#![forbid(unsafe_code)]

pub mod assets;
pub mod clock;
pub mod config;
pub mod healthcheck;
#[cfg(test)]
mod tokens;

use std::future::Future;
use std::io;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use db::PgPool;
use serde::Serialize;
use tokio::net::TcpListener;

#[derive(Serialize)]
struct Health {
    status: &'static str,
    db: &'static str,
}

pub fn router(pool: PgPool) -> Router {
    Router::new().route("/health", get(health)).with_state(pool)
}

const PING_TIMEOUT: Duration = Duration::from_secs(3);

async fn health(State(pool): State<PgPool>) -> (StatusCode, Json<Health>) {
    health_within(db::ping(&pool), PING_TIMEOUT).await
}

async fn health_within<E: std::fmt::Display>(
    ping: impl Future<Output = Result<(), E>>,
    timeout: Duration,
) -> (StatusCode, Json<Health>) {
    let result = match tokio::time::timeout(timeout, ping).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err(format!("no answer within {timeout:?}")),
    };
    match result {
        Ok(()) => (
            StatusCode::OK,
            Json(Health {
                status: "ok",
                db: "ok",
            }),
        ),
        Err(error) => {
            tracing::warn!(%error, "database ping failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Health {
                    status: "degraded",
                    db: "down",
                }),
            )
        }
    }
}

pub async fn serve(
    listener: TcpListener,
    pool: PgPool,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(listener, router(pool))
        .with_graceful_shutdown(shutdown)
        .await
}

pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "cannot listen for ctrl-c");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_ping_counts_as_down() {
        let (status, Json(body)) = health_within(
            std::future::pending::<Result<(), String>>(),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.db, "down");
    }
}
