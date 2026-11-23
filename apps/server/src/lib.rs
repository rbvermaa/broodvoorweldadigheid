#![forbid(unsafe_code)]

pub mod config;
pub mod healthcheck;

use std::future::Future;
use std::io;

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

async fn health(State(pool): State<PgPool>) -> (StatusCode, Json<Health>) {
    match db::ping(&pool).await {
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
