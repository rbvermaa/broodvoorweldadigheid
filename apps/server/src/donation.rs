//! Choosing a charity for a saved donation (F4d): through the link in the order
//! confirmation, or from "Mijn bestellingen" when signed in.

use std::collections::HashMap;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use domain::{CharityId, DonationAllocationId};
use i18n::{Amsterdam, FluentArgs};
use orders::donations::{self, Chooser, Saved};
use sqlx::PgConnection;
use tower_sessions::Session;

use crate::account::{LOGIN_PATH, ORDERS_PATH, message, no_store, server_error};
use crate::components::SelectOption;
use crate::csrf;
use crate::customer_session;
use crate::pages::{self, Layout};
use crate::state::AppState;

pub const CHOOSE_PATH: &str = "/donatie/kies";

const LINK_FAILED: (&str, &str) = ("donation-link-failed-title", "donation-link-failed-text");
const CHOSEN: (&str, &str) = ("donation-chosen-title", "donation-chosen-text");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/donatie/kies/{token}", get(link_page).post(choose_by_link))
        .route("/mijn-bestellingen/donatie/{id}", post(choose_signed_in))
}

/// One saved donation with its form.
pub struct ChooseForm {
    pub action: String,
    /// The select's name and id, unique on the page.
    pub field: String,
    pub text: String,
    pub options: Vec<SelectOption>,
    pub error: String,
}

#[derive(Template)]
#[template(path = "account/donation.html")]
struct ChoosePage<'a> {
    layout: Layout<'a>,
    form: ChooseForm,
}

/// The select on the link page; on "Mijn bestellingen" each donation has its own.
const FIELD: &str = "goed-doel";

fn signed_in_field(allocation: DonationAllocationId) -> String {
    format!("{FIELD}-{}", allocation.get())
}

fn link_failed(state: &AppState) -> Response {
    message(state, StatusCode::NOT_FOUND, LINK_FAILED, "warning", false)
}

/// The text and charity options for one saved donation: for the link page when `link`
/// is its token, else for "Mijn bestellingen".
pub async fn form(
    state: &AppState,
    conn: &mut PgConnection,
    saved: &Saved,
    link: Option<&str>,
    error: String,
) -> Result<ChooseForm, String> {
    let charities = catalog::charities::list_active(conn)
        .await
        .map_err(|err| err.to_string())?;
    let mut args = FluentArgs::new();
    args.set("amount", i18n::money(saved.amount.cents()));
    args.set("number", saved.number.to_string());
    args.set("drop", state.i18n.long_date(saved.drop_date));
    args.set(
        "date",
        state.i18n.long_date(Amsterdam::new().date(saved.choose_by)),
    );
    let text = state
        .i18n
        .t("donation-choose-text", Some(&args))
        .map_err(|err| err.to_string())?;
    let (action, field) = match link {
        Some(token) => (format!("{CHOOSE_PATH}/{token}"), FIELD.to_owned()),
        None => (
            format!("{ORDERS_PATH}/donatie/{}", saved.allocation.get()),
            signed_in_field(saved.allocation),
        ),
    };
    Ok(ChooseForm {
        action,
        field,
        text,
        options: charities
            .into_iter()
            .map(|charity| SelectOption {
                value: charity.id.get().to_string(),
                label: charity.name,
                selected: false,
            })
            .collect(),
        error,
    })
}

fn parse(form: &HashMap<String, String>, field: &str) -> Option<CharityId> {
    form.get(field)?.trim().parse().ok().map(CharityId::new)
}

async fn link_page(
    State(state): State<AppState>,
    session: Session,
    Path(token): Path<String>,
) -> Response {
    show(&state, &session, &token, String::new(), StatusCode::OK).await
}

async fn show(
    state: &AppState,
    session: &Session,
    token: &str,
    error: String,
    status: StatusCode,
) -> Response {
    let csrf = match csrf::token(session).await {
        Ok(csrf) => csrf,
        Err(error) => return server_error(state, &error),
    };
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => return server_error(state, &error),
    };
    let saved = match donations::saved_by_link(&mut conn, token, state.clock.now()).await {
        Ok(Some(saved)) => saved,
        Ok(None) => return no_store(link_failed(state)),
        Err(error) => return server_error(state, &error),
    };
    match form(state, &mut conn, &saved, Some(token), error).await {
        Ok(form) => no_store(pages::render(
            state,
            status,
            &ChoosePage {
                layout: Layout::new(state).with_csrf_token(csrf),
                form,
            },
        )),
        Err(error) => server_error(state, &error),
    }
}

fn error_text(state: &AppState) -> String {
    state
        .i18n
        .t("donation-choose-error", None)
        .unwrap_or_default()
}

async fn choose_by_link(
    State(state): State<AppState>,
    session: Session,
    Path(token): Path<String>,
    Form(choice): Form<HashMap<String, String>>,
) -> Response {
    let Some(charity) = parse(&choice, FIELD) else {
        return show(
            &state,
            &session,
            &token,
            error_text(&state),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    let chosen = {
        let mut conn = match state.pool.acquire().await {
            Ok(conn) => conn,
            Err(error) => return server_error(&state, &error),
        };
        donations::choose(&mut conn, Chooser::Link(&token), charity, state.clock.now()).await
    };
    match chosen {
        Ok(Some(_)) => no_store(message(&state, StatusCode::OK, CHOSEN, "success", false)),
        Ok(None) => no_store(link_failed(&state)),
        Err(orders::Error::Invalid(_)) => {
            show(
                &state,
                &session,
                &token,
                error_text(&state),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(error) => server_error(&state, &error),
    }
}

async fn choose_signed_in(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
    Form(choice): Form<HashMap<String, String>>,
) -> Response {
    let me = match customer_session::current(&state, &session).await {
        Ok(Some(me)) => me,
        Ok(None) => return Redirect::to(LOGIN_PATH).into_response(),
        Err(error) => return server_error(&state, &error),
    };
    let Some(charity) = parse(&choice, &signed_in_field(DonationAllocationId::new(id))) else {
        return Redirect::to(ORDERS_PATH).into_response();
    };
    let chosen = {
        let mut conn = match state.pool.acquire().await {
            Ok(conn) => conn,
            Err(error) => return server_error(&state, &error),
        };
        donations::choose(
            &mut conn,
            Chooser::Customer(me.id, DonationAllocationId::new(id)),
            charity,
            state.clock.now(),
        )
        .await
    };
    match chosen {
        Ok(_) | Err(orders::Error::Invalid(_)) => Redirect::to(ORDERS_PATH).into_response(),
        Err(error) => server_error(&state, &error),
    }
}
