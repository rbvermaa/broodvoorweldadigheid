#![forbid(unsafe_code)]

pub mod account;
pub mod alerts;
pub mod assets;
pub mod background;
pub mod backoffice;
pub mod clock;
pub mod components;
pub mod config;
pub mod csrf;
pub mod customer_session;
pub mod donation;
pub mod gallery;
pub mod healthcheck;
#[cfg(test)]
mod icon_set;
pub mod icons;
pub mod pages;
pub mod payment_provider;
pub mod privacy;
pub mod rate_limit;
pub mod security;
pub mod session;
pub mod shop;
pub mod site;
pub mod staff;
pub mod staff_session;
pub mod state;
#[cfg(test)]
mod tokens;

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
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
    let site = site::routes(&state, state.clock.clone());
    let mut routes = Router::new()
        .route("/", get(pages::home))
        .merge(shop::routes())
        .merge(site)
        .merge(alerts::routes(&state))
        .merge(account::routes())
        .merge(donation::routes())
        .merge(privacy::routes())
        .route("/staff", get(staff::home))
        .route(staff::LOGIN_PATH, get(staff::login))
        .route("/staff/logout", post(staff::logout))
        .merge(backoffice::routes());
    if state.config.dev_staff_login {
        routes = routes.route(staff::DEV_LOGIN_PATH, post(staff::dev_login));
    }
    if state.config.gallery {
        routes = routes.route("/gallery", get(gallery::page).post(gallery::save));
    }
    let checkout = rate_limit::apply(
        sessioned(shop::checkout_routes(), &state),
        &state,
        shop::CHECKOUT_PERIOD,
        shop::CHECKOUT_BURST,
    );
    let login = rate_limit::apply(
        sessioned(account::login_routes(), &state),
        &state,
        account::LOGIN_PERIOD,
        account::LOGIN_BURST,
    );
    shell(
        routes,
        Some(checkout.merge(login)),
        shop::webhook_routes(),
        state.clone(),
    )
    .merge(alerts::one_click_routes(state))
}

/// CSRF checks inside a session layer.
fn sessioned(routes: Router<AppState>, state: &AppState) -> Router<AppState> {
    routes
        .layer(middleware::from_fn(pages::header_login))
        .layer(middleware::from_fn_with_state(state.clone(), csrf::verify))
        .layer(session::layer(state.pool.clone(), &state.config))
}

/// Wraps `routes` with what every page needs: the Dutch 404 and 405 pages, CSRF checks,
/// sessions and security headers, and adds `/health` and the static assets. Those two
/// stay outside the session layer, which saves the session on every request it sees.
pub fn with_shell(routes: Router<AppState>, state: AppState) -> Router {
    shell(routes, None, Router::new(), state)
}

/// `limited` comes with its own session, CSRF and rate-limit layers, the rate limit
/// outermost (ADR 0007). `open` gets neither session nor CSRF check.
fn shell(
    routes: Router<AppState>,
    limited: Option<Router<AppState>>,
    open: Router<AppState>,
    state: AppState,
) -> Router {
    let mut sessioned = sessioned(
        routes
            .fallback(pages::not_found)
            .method_not_allowed_fallback(pages::method_not_allowed),
        &state,
    );
    if let Some(limited) = limited {
        sessioned = sessioned.merge(limited);
    }
    Router::new()
        .route("/health", get(health))
        .route("/static/{*name}", get(assets::serve))
        .merge(open)
        .merge(sessioned)
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
