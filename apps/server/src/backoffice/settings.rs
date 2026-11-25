//! Company settings for the owner: the switch for the public donation page (F11d) and the
//! address printed on labels (F18, F18a).

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use catalog::company::{self, MAX_ADDRESS_CHARS};
use serde::Deserialize;
use tower_sessions::Session;

use super::{Flash, Outcome, Rejection, fail, invalid_status, layout, require, saved, t_with};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(PATH, get(page).post(save))
        .route(ADDRESS_PATH, post(save_address))
}

pub const PATH: &str = "/staff/settings";
const ADDRESS_PATH: &str = "/staff/settings/address";

#[derive(Template)]
#[template(path = "staff/settings.html")]
struct SettingsPage<'a> {
    layout: Layout<'a>,
    saved: bool,
    public_donations: bool,
    address_action: &'static str,
    address: String,
    address_error: String,
}

async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    saved: bool,
    address: Option<(String, String)>,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let public_donations = company::public_donations(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    let (address, address_error) = match address {
        Some(posted) => posted,
        None => (
            company::contact(&mut conn)
                .await
                .map_err(|e| fail(state, &e))?
                .address
                .unwrap_or_default(),
            String::new(),
        ),
    };
    drop(conn);
    Ok(pages::render(
        state,
        status,
        &SettingsPage {
            layout: layout(state, session).await?,
            saved,
            public_donations,
            address_action: ADDRESS_PATH,
            address,
            address_error,
        },
    ))
}

pub async fn page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Settings)?;
        render(&state, &session, StatusCode::OK, flash.saved(), None).await
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
        company::set_public_donations(&mut conn, on)
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

#[derive(Debug, Default, Deserialize)]
pub struct AddressForm {
    #[serde(default)]
    address: String,
}

pub async fn save_address(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Form(form): Form<AddressForm>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Settings)?;
        let address = form.address.trim().to_owned();
        if address.chars().count() > MAX_ADDRESS_CHARS {
            let error = t_with(
                &state,
                "settings-address-too-long",
                &[("max", MAX_ADDRESS_CHARS.to_string())],
            );
            return render(
                &state,
                &session,
                invalid_status(&headers),
                false,
                Some((address, error)),
            )
            .await;
        }
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        company::set_address(&mut conn, &address)
            .await
            .map_err(|e| fail(&state, &e))?;
        tracing::info!(staff = staff.0.id.get(), "company address saved");
        Ok(saved(&headers, PATH))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
