use std::net::SocketAddr;

use askama::Template;
use auth::staff as repo;
use auth::{Permission, Role, StaffUser};
use axum::Extension;
use axum::extract::{ConnectInfo, Form, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use domain::StaffUserId;
use serde::Deserialize;
use tower_sessions::Session;

use crate::csrf;
use crate::pages::{self, Layout};
use crate::staff_session;
use crate::state::AppState;

pub const LOGIN_PATH: &str = "/staff/login";
pub const DEV_LOGIN_PATH: &str = "/staff/dev-login";

/// Headers a reverse proxy adds; a request carrying one did not come straight from the
/// developer's own machine.
const PROXY_HEADERS: [&str; 3] = ["forwarded", "x-forwarded-for", "x-real-ip"];

/// A signed-in, active staff member. Without one the request is sent to the login.
pub struct Staff(pub StaffUser);

impl Staff {
    /// # Errors
    /// `Forbidden`, which renders the Dutch 403 page, when the role lacks `permission`.
    pub fn require(&self, state: &AppState, permission: Permission) -> Result<(), Forbidden> {
        if self.0.role.can(permission) {
            Ok(())
        } else {
            tracing::warn!(
                staff = self.0.id.get(),
                ?permission,
                "role lacks permission"
            );
            Err(Forbidden(state.clone()))
        }
    }
}

pub struct Forbidden(AppState);

impl IntoResponse for Forbidden {
    fn into_response(self) -> Response {
        pages::error_with(
            &self.0,
            StatusCode::FORBIDDEN,
            "staff-forbidden-title",
            "staff-forbidden-text",
        )
    }
}

/// HTMX swaps a redirect's target into the page, so it gets `HX-Redirect` instead.
fn to_login(headers: &HeaderMap) -> Response {
    if headers.contains_key("hx-request") {
        let mut response = StatusCode::NO_CONTENT.into_response();
        response
            .headers_mut()
            .insert("hx-redirect", HeaderValue::from_static(LOGIN_PATH));
        response
    } else {
        Redirect::to(LOGIN_PATH).into_response()
    }
}

impl FromRequestParts<AppState> for Staff {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let server_error = || pages::error(state, StatusCode::INTERNAL_SERVER_ERROR);
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_| server_error())?;
        match staff_session::current(state, &session).await {
            Ok(Some(staff)) => Ok(Self(staff)),
            Ok(None) => Err(to_login(&parts.headers)),
            Err(error) => {
                tracing::error!(%error, "cannot load the staff login");
                Err(server_error())
            }
        }
    }
}

fn server_error(state: &AppState, error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, "staff session");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}

fn not_allowed(state: &AppState) -> Response {
    pages::error_with(
        state,
        StatusCode::FORBIDDEN,
        "staff-not-allowed-title",
        "staff-not-allowed-text",
    )
}

/// The dev login only answers a browser on this machine that talks to the server directly.
fn dev_login_allowed(
    state: &AppState,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: &HeaderMap,
) -> bool {
    if !state.config.dev_staff_login {
        return false;
    }
    let local =
        peer.is_some_and(|Extension(ConnectInfo(peer))| peer.ip().to_canonical().is_loopback());
    let proxied = PROXY_HEADERS.iter().any(|name| headers.contains_key(*name));
    if !local || proxied {
        tracing::warn!(
            ?peer,
            proxied,
            "dev login refused for a request from elsewhere"
        );
    }
    local && !proxied
}

#[derive(Template)]
#[template(path = "staff/dev_login.html")]
struct DevLoginPage<'a> {
    layout: Layout<'a>,
    action: &'static str,
}

pub async fn login(
    State(state): State<AppState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    session: Session,
) -> Response {
    if !dev_login_allowed(&state, peer, &headers) {
        return pages::error_with(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "staff-unavailable-title",
            "staff-unavailable-text",
        );
    }
    let token = match csrf::token(&session).await {
        Ok(token) => token,
        Err(error) => return server_error(&state, &error),
    };
    pages::render(
        &state,
        StatusCode::OK,
        &DevLoginPage {
            layout: Layout::new(&state).with_csrf_token(token),
            action: DEV_LOGIN_PATH,
        },
    )
}

#[derive(Debug, Deserialize)]
pub struct DevLogin {
    staff_id: String,
}

/// Signs in as any active staff member, no questions asked. Only routed with
/// `DEV_STAFF_LOGIN=true`, and only answered for a direct request from this machine.
pub async fn dev_login(
    State(state): State<AppState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    session: Session,
    Form(form): Form<DevLogin>,
) -> Response {
    if !dev_login_allowed(&state, peer, &headers) {
        return pages::error(&state, StatusCode::NOT_FOUND);
    }
    let Ok(id) = form.staff_id.trim().parse::<i64>() else {
        return not_allowed(&state);
    };
    let user = match repo::find(&state.pool, StaffUserId::new(id)).await {
        Ok(Some(user)) if user.active => user,
        Ok(_) => return not_allowed(&state),
        Err(error) => return server_error(&state, &error),
    };
    if let Err(error) = staff_session::sign_in(&state, &session, &user).await {
        return server_error(&state, &error);
    }
    tracing::warn!(staff = user.id.get(), role = %user.role, "staff signed in through the dev login");
    Redirect::to("/staff").into_response()
}

pub async fn logout(State(state): State<AppState>, session: Session) -> Response {
    if let Err(error) = staff_session::sign_out(&state, &session).await {
        return server_error(&state, &error);
    }
    Redirect::to("/").into_response()
}

#[derive(Template)]
#[template(path = "staff/home.html")]
struct Home<'a> {
    layout: Layout<'a>,
    name: &'a str,
    role: &'static str,
}

fn role_message(role: Role) -> &'static str {
    match role {
        Role::Owner => "staff-role-owner",
        Role::Baker => "staff-role-baker",
        Role::Counter => "staff-role-counter",
    }
}

pub async fn home(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let token = match csrf::token(&session).await {
        Ok(token) => token,
        Err(error) => return server_error(&state, &error),
    };
    pages::render(
        &state,
        StatusCode::OK,
        &Home {
            layout: Layout::new(&state).with_csrf_token(token),
            name: &staff.0.name,
            role: role_message(staff.0.role),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_has_a_fluent_name() {
        let translator = i18n::Translator::dutch().expect("Dutch texts");
        let known = translator.message_ids();
        for id in Role::ALL.into_iter().map(role_message) {
            assert!(known.iter().any(|known| known == id), "missing {id}");
        }
    }
}
