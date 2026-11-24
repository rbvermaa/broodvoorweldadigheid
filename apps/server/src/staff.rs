use askama::Template;
use auth::staff as repo;
use auth::{Permission, Role, StaffUser};
use axum::extract::{Form, FromRequestParts, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
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

impl FromRequestParts<AppState> for Staff {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let server_error = || pages::error(state, StatusCode::INTERNAL_SERVER_ERROR);
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_| server_error())?;
        let id = staff_session::staff_id(&session).await.map_err(|error| {
            tracing::error!(%error, "cannot load the session");
            server_error()
        })?;
        let Some(id) = id else {
            return Err(Redirect::to(LOGIN_PATH).into_response());
        };
        match repo::find(&state.pool, id).await {
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

#[derive(Template)]
#[template(path = "staff/dev_login.html")]
struct DevLoginPage<'a> {
    layout: Layout<'a>,
    action: &'static str,
}

/// Without a sign-in method configured there is no way in yet.
pub async fn login(State(state): State<AppState>, session: Session) -> Response {
    if !state.config.dev_staff_login {
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
/// `DEV_STAFF_LOGIN=true`, which the config allows on a loopback address only.
pub async fn dev_login(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<DevLogin>,
) -> Response {
    if !state.config.dev_staff_login {
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
    if let Err(error) = staff_session::sign_in(&session, &user).await {
        return server_error(&state, &error);
    }
    tracing::warn!(staff = user.id.get(), role = %user.role, "staff signed in through the dev login");
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
