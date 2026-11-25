//! Company settings for the owner: the switch for the public donation page (F11d).

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;
use tower_sessions::Session;

use super::{Flash, Rejection, fail, layout, require, saved};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route(PATH, get(page).post(save))
}

pub const PATH: &str = "/staff/settings";

#[derive(Template)]
#[template(path = "staff/settings.html")]
struct SettingsPage<'a> {
    layout: Layout<'a>,
    saved: bool,
    public_donations: bool,
}

pub async fn page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Settings)?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let public_donations = catalog::company::public_donations(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        drop(conn);
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &SettingsPage {
                layout: layout(&state, &session).await?,
                saved: flash.saved(),
                public_donations,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// An unticked checkbox sends nothing.
#[derive(Debug, Default, Deserialize)]
pub struct SettingsForm {
    public_donations: Option<String>,
}

pub async fn save(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Settings)?;
        let on = form.public_donations.is_some();
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        catalog::company::set_public_donations(&mut conn, on)
            .await
            .map_err(|e| fail(&state, &e))?;
        state.public_donations.remember(on);
        tracing::info!(
            staff = staff.0.id.get(),
            on,
            "public donation page switched"
        );
        Ok(saved(&headers, PATH))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
