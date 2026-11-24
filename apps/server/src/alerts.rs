//! Drop alerts (F9a, ADR 0021): sign-up with double opt-in, the confirmation link and
//! the unsubscribe link, which also takes the RFC 8058 one-click POST.

use std::time::Duration;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Form, Router, middleware};
use notify::Composer;
use notify::subscribers::{self, Links, SubscriberError};
use serde::Deserialize;
use tower_sessions::Session;

use crate::pages::{self, Layout};
use crate::state::AppState;
use crate::{csrf, rate_limit, security};

/// Sign-ups: a burst of 5, then one every 12 seconds per client (spec, Security).
pub const SUBSCRIBE_PERIOD: Duration = Duration::from_secs(12);
pub const SUBSCRIBE_BURST: u32 = 5;

/// The pages, behind the session and CSRF layers; the sign-up POST is rate limited.
pub fn routes(state: &AppState) -> Router<AppState> {
    let subscribe = rate_limit::apply(
        Router::new().route("/meldingen", post(subscribe)),
        state,
        SUBSCRIBE_PERIOD,
        SUBSCRIBE_BURST,
    );
    Router::new()
        .route("/meldingen", get(form))
        .route("/meldingen/bevestig/{token}", get(confirm))
        .route("/meldingen/afmelden/{token}", get(unsubscribe_page))
        .merge(subscribe)
}

/// The unsubscribe POST, without session or CSRF check: mail clients send the RFC 8058
/// one-click POST with neither. The token in the path is the only authority it needs.
pub fn one_click_routes(state: AppState) -> Router {
    Router::new()
        .route("/meldingen/afmelden/{token}", post(unsubscribe))
        .layer(middleware::map_response(security::headers))
        .with_state(state)
}

#[derive(Template)]
#[template(path = "alerts/subscribe.html")]
struct SubscribePage<'a> {
    layout: Layout<'a>,
    email: String,
    error: String,
}

#[derive(Template)]
#[template(path = "alerts/message.html")]
struct Message<'a> {
    layout: Layout<'a>,
    title: &'static str,
    text: &'static str,
    kind: &'static str,
    subscribe_link: bool,
}

#[derive(Template)]
#[template(path = "alerts/unsubscribe.html")]
struct UnsubscribePage<'a> {
    layout: Layout<'a>,
    action: String,
}

#[derive(Deserialize)]
pub struct SubscribeForm {
    #[serde(default)]
    email: String,
}

const CHECK_MAIL: (&str, &str) = ("alerts-check-title", "alerts-check-text");
const CONFIRMED: (&str, &str) = ("alerts-confirmed-title", "alerts-confirmed-text");
const CONFIRM_FAILED: (&str, &str) = ("alerts-confirm-failed-title", "alerts-confirm-failed-text");
const UNSUBSCRIBED: (&str, &str) = ("alerts-unsubscribed-title", "alerts-unsubscribed-text");
const UNKNOWN_LINK: (&str, &str) = ("alerts-unknown-link-title", "alerts-unknown-link-text");

fn message(
    state: &AppState,
    status: StatusCode,
    (title, text): (&'static str, &'static str),
    kind: &'static str,
    subscribe_link: bool,
) -> Response {
    pages::render(
        state,
        status,
        &Message {
            layout: Layout::new(state),
            title,
            text,
            kind,
            subscribe_link,
        },
    )
}

fn server_error(state: &AppState, error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, "drop alerts");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}

async fn subscribe_form(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    email: String,
    error: String,
) -> Response {
    let token = match csrf::token(session).await {
        Ok(token) => token,
        Err(error) => return server_error(state, &error),
    };
    pages::render(
        state,
        status,
        &SubscribePage {
            layout: Layout::new(state).with_csrf_token(token),
            email,
            error,
        },
    )
}

async fn form(State(state): State<AppState>, session: Session) -> Response {
    subscribe_form(
        &state,
        &session,
        StatusCode::OK,
        String::new(),
        String::new(),
    )
    .await
}

/// Every valid address gets the same answer, whether it was new, waiting for
/// confirmation or already on the list.
async fn subscribe(
    State(state): State<AppState>,
    session: Session,
    Form(input): Form<SubscribeForm>,
) -> Response {
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => return server_error(&state, &error),
    };
    let company = match notify::Company::load(&mut conn).await {
        Ok(company) => company,
        Err(error) => return server_error(&state, &error),
    };
    let composer = Composer::new(&state.i18n, &company);
    let links = Links::new(&state.config.public_url);
    let result = subscribers::subscribe(
        &mut conn,
        state.clock.as_ref(),
        &composer,
        &links,
        &input.email,
    )
    .await;
    match result {
        Ok(_) => message(&state, StatusCode::OK, CHECK_MAIL, "info", false),
        Err(SubscriberError::Address) => {
            let error = match state.i18n.t("alerts-email-error", None) {
                Ok(text) => text,
                Err(error) => return server_error(&state, &error),
            };
            subscribe_form(
                &state,
                &session,
                StatusCode::UNPROCESSABLE_ENTITY,
                input.email,
                error,
            )
            .await
        }
        Err(error) => server_error(&state, &error),
    }
}

async fn confirm(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => return server_error(&state, &error),
    };
    match subscribers::confirm(&mut conn, state.clock.as_ref(), &token).await {
        Ok(true) => message(&state, StatusCode::OK, CONFIRMED, "success", false),
        Ok(false) => message(
            &state,
            StatusCode::NOT_FOUND,
            CONFIRM_FAILED,
            "warning",
            true,
        ),
        Err(error) => server_error(&state, &error),
    }
}

async fn unsubscribe_page(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => return server_error(&state, &error),
    };
    match subscribers::unsubscribe_link_known(&mut conn, &token).await {
        Ok(true) => pages::render(
            &state,
            StatusCode::OK,
            &UnsubscribePage {
                layout: Layout::new(&state),
                action: format!("/meldingen/afmelden/{token}"),
            },
        ),
        Ok(false) => message(
            &state,
            StatusCode::NOT_FOUND,
            UNKNOWN_LINK,
            "warning",
            false,
        ),
        Err(error) => server_error(&state, &error),
    }
}

/// The body is not read: the RFC 8058 `List-Unsubscribe=One-Click` and the page's own
/// button do the same thing.
async fn unsubscribe(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => return server_error(&state, &error),
    };
    match subscribers::unsubscribe(&mut conn, state.clock.as_ref(), &token).await {
        Ok(true) => message(&state, StatusCode::OK, UNSUBSCRIBED, "success", true),
        Ok(false) => message(
            &state,
            StatusCode::NOT_FOUND,
            UNKNOWN_LINK,
            "warning",
            false,
        ),
        Err(error) => server_error(&state, &error),
    }
}

#[cfg(test)]
mod tests {
    use i18n::Translator;

    use super::*;

    #[test]
    fn every_message_page_text_exists() {
        let i18n = Translator::dutch().expect("Dutch texts");
        for (title, text) in [
            CHECK_MAIL,
            CONFIRMED,
            CONFIRM_FAILED,
            UNSUBSCRIBED,
            UNKNOWN_LINK,
        ] {
            i18n.t(title, None).expect(title);
            i18n.t(text, None).expect(text);
        }
    }
}
