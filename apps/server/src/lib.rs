#![forbid(unsafe_code)]

pub mod assets;
pub mod config;
pub mod csrf;
pub mod healthcheck;
pub mod pages;
pub mod rate_limit;
pub mod security;
pub mod session;
pub mod state;
#[cfg(test)]
mod tokens;

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router, middleware};
use serde::Serialize;
use tokio::net::TcpListener;

pub use crate::state::AppState;

#[derive(Serialize)]
struct Health {
    status: &'static str,
    db: &'static str,
}

pub fn router(state: AppState) -> Router {
    let routes = Router::new()
        .route("/", get(pages::home))
        .route("/health", get(health));
    with_shell(routes, state)
}

/// Wraps `routes` with what every page needs: static assets, the Dutch 404 page, CSRF
/// checks, sessions and security headers.
pub fn with_shell(routes: Router<AppState>, state: AppState) -> Router {
    let sessions = session::layer(state.pool.clone(), &state.config);
    routes
        .route("/static/{name}", get(assets::serve))
        .fallback(pages::not_found)
        .layer(middleware::from_fn_with_state(state.clone(), csrf::verify))
        .layer(sessions)
        .layer(middleware::map_response(security::headers))
        .with_state(state)
}

const PING_TIMEOUT: Duration = Duration::from_secs(3);

async fn health(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    health_within(db::ping(&state.pool), PING_TIMEOUT).await
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
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let cleanup = tokio::spawn(session::delete_expired_forever(session::store(
        state.pool.clone(),
    )));
    let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await;
    cleanup.abort();
    served
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
