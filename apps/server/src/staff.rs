use askama::Template;
use auth::oidc::{OidcError, PendingLogin};
use auth::staff::{self as repo, Error as StaffError};
use auth::{Permission, Role, StaffUser};
use axum::extract::{FromRequestParts, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use domain::StaffUserId;
use serde::Deserialize;
use tower_sessions::Session;

use crate::csrf;
use crate::pages::{self, Layout};
use crate::state::AppState;

pub const LOGIN_PATH: &str = "/staff/login";
const PENDING_KEY: &str = "staff_login_pending";
const STAFF_KEY: &str = "staff_id";

/// A logged-in, active staff member. Without one the request is sent to the login.
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

impl FromRequestParts<AppState> for Staff {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let server_error = || pages::error(state, StatusCode::INTERNAL_SERVER_ERROR);
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_| server_error())?;
        let id = session.get::<i64>(STAFF_KEY).await.map_err(|error| {
            tracing::error!(%error, "cannot load the session");
            server_error()
        })?;
        let Some(id) = id else {
            return Err(Redirect::to(LOGIN_PATH).into_response());
        };
        match repo::find(&state.pool, StaffUserId::new(id)).await {
            Ok(Some(user)) if user.active => Ok(Self(user)),
            Ok(_) => {
                if let Err(error) = session.flush().await {
                    tracing::error!(%error, "cannot end the session");
                }
                Err(Redirect::to(LOGIN_PATH).into_response())
            }
            Err(error) => {
                tracing::error!(%error, "cannot load the staff user");
                Err(server_error())
            }
        }
    }
}

fn unavailable(state: &AppState) -> Response {
    pages::error_with(
        state,
        StatusCode::SERVICE_UNAVAILABLE,
        "staff-unavailable-title",
        "staff-unavailable-text",
    )
}

fn login_failed(state: &AppState, status: StatusCode, reason: &str) -> Response {
    tracing::warn!(reason, "staff login failed");
    pages::error_with(
        state,
        status,
        "staff-login-failed-title",
        "staff-login-failed-text",
    )
}

fn server_error(state: &AppState, error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, "staff login");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}

/// Sends the browser to Kanidm with a fresh state, nonce and PKCE challenge.
pub async fn login(State(state): State<AppState>, session: Session) -> Response {
    let Some(login) = state.staff_login.as_deref() else {
        tracing::warn!("staff login is not configured (OIDC_ISSUER)");
        return unavailable(&state);
    };
    let provider = match login.provider(state.clock.now()).await {
        Ok(provider) => provider,
        Err(error) => {
            tracing::error!(error = %auth::oidc::error_chain(&error), "staff login unavailable");
            return unavailable(&state);
        }
    };
    let (url, pending) = provider.begin();
    if let Err(error) = session.insert(PENDING_KEY, pending).await {
        return server_error(&state, &error);
    }
    Redirect::to(&url).into_response()
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn callback(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let pending = match session.remove::<PendingLogin>(PENDING_KEY).await {
        Ok(Some(pending)) => pending,
        Ok(None) => return login_failed(&state, StatusCode::BAD_REQUEST, "no login pending"),
        Err(error) => return server_error(&state, &error),
    };
    if let Some(error) = query.error.as_deref() {
        return login_failed(&state, StatusCode::BAD_REQUEST, error);
    }
    let (Some(code), Some(returned_state)) = (query.code, query.state) else {
        return login_failed(&state, StatusCode::BAD_REQUEST, "code or state missing");
    };
    let Some(login) = state.staff_login.as_deref() else {
        return unavailable(&state);
    };
    let now = state.clock.now();
    let provider = match login.provider(now).await {
        Ok(provider) => provider,
        Err(error) => {
            tracing::error!(error = %auth::oidc::error_chain(&error), "staff login unavailable");
            return unavailable(&state);
        }
    };
    let identity = match provider
        .complete(login.http(), pending, &returned_state, &code, now)
        .await
    {
        Ok(identity) => identity,
        Err(OidcError::StateMismatch) => {
            return login_failed(&state, StatusCode::BAD_REQUEST, "state mismatch");
        }
        Err(error) => {
            let reason = auth::oidc::error_chain(&error);
            return login_failed(&state, StatusCode::FORBIDDEN, &reason);
        }
    };
    let user = match repo::sign_in(&state.pool, &identity, &state.config.bootstrap_owner).await {
        Ok(user) => user,
        Err(StaffError::Unknown | StaffError::Inactive(_)) => {
            tracing::warn!(sub = %identity.sub, "refused staff login");
            return pages::error_with(
                &state,
                StatusCode::FORBIDDEN,
                "staff-not-allowed-title",
                "staff-not-allowed-text",
            );
        }
        Err(error) => return server_error(&state, &error),
    };
    session.clear().await;
    if let Err(error) = session.cycle_id().await {
        return server_error(&state, &error);
    }
    if let Err(error) = session.insert(STAFF_KEY, user.id.get()).await {
        return server_error(&state, &error);
    }
    tracing::info!(staff = user.id.get(), role = %user.role, "staff logged in");
    Redirect::to("/staff").into_response()
}

pub async fn logout(State(state): State<AppState>, session: Session) -> Response {
    if let Err(error) = session.flush().await {
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
    csrf_token: &'a str,
    csrf_field: &'static str,
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
            layout: Layout::new(&state).with_csrf_token(token.clone()),
            name: &staff.0.name,
            role: role_message(staff.0.role),
            csrf_token: &token,
            csrf_field: csrf::FIELD,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_staff_message_exists_in_fluent() {
        let translator = i18n::Translator::dutch().expect("Dutch texts");
        let known = translator.message_ids();
        let mut ids: Vec<&str> = Role::ALL.into_iter().map(role_message).collect();
        ids.extend([
            "staff-forbidden-title",
            "staff-forbidden-text",
            "staff-unavailable-title",
            "staff-unavailable-text",
            "staff-login-failed-title",
            "staff-login-failed-text",
            "staff-not-allowed-title",
            "staff-not-allowed-text",
        ]);
        for id in ids {
            assert!(known.iter().any(|known| known == id), "missing {id}");
        }
    }
}
