//! The back office under `/staff`. Every handler takes [`Staff`] and checks a permission
//! (ADR 0010). Forms post normally and redirect with `?opgeslagen=1`; htmx only adds inline
//! checks and partial updates on top (ADR 0019).

pub mod bake_days;
pub mod batch_labels;
pub mod batches;
pub mod business;
pub mod charities;
pub mod costing;
pub mod customers;
pub mod extras;
pub mod form;
pub mod ingredients;
pub mod kitchen;
pub mod lists;
pub mod locations;
pub mod nav;
pub mod orders;
pub mod payouts;
pub mod polls;
pub mod product_labels;
pub mod products;
pub mod recipes;
pub mod reports;
pub mod settings;
pub mod sick_day;

use std::fmt::Display;

use auth::Permission;
use axum::Router;
use axum::http::header::HeaderName;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use i18n::FluentArgs;
use serde::Deserialize;
use tower_sessions::Session;

use crate::csrf;
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

// One line per feature, sorted (ADR 0044).
const FEATURES: &[fn() -> Router<AppState>] = &[
    bake_days::routes,
    batch_labels::routes,
    batches::routes,
    business::routes,
    charities::routes,
    costing::routes,
    customers::routes,
    extras::routes,
    ingredients::routes,
    kitchen::routes,
    lists::routes,
    locations::routes,
    orders::routes,
    payouts::routes,
    polls::routes,
    product_labels::routes,
    products::routes,
    recipes::routes,
    reports::routes,
    settings::routes,
    sick_day::routes,
];

pub fn routes() -> Router<AppState> {
    FEATURES
        .iter()
        .fold(Router::new(), |router, feature| router.merge(feature()))
}

/// A response that ends the handler early: a refusal, a 404 or a 500 page. Boxed, so
/// results stay small.
pub struct Rejection(Box<Response>);

impl From<Response> for Rejection {
    fn from(response: Response) -> Self {
        Self(Box::new(response))
    }
}

impl Rejection {
    pub fn into_response(self) -> Response {
        *self.0
    }
}

pub type Step<T> = Result<T, Rejection>;
pub type Outcome = Step<Response>;

/// `?opgeslagen=1` after a save, so the page shows the save confirmation.
#[derive(Debug, Default, Deserialize)]
pub struct Flash {
    opgeslagen: Option<String>,
}

impl Flash {
    pub fn saved(&self) -> bool {
        self.opgeslagen.is_some()
    }
}

pub fn require(state: &AppState, staff: &Staff, permission: Permission) -> Step<()> {
    staff
        .require(state, permission)
        .map_err(|forbidden| forbidden.into_response().into())
}

pub fn fail(state: &AppState, error: &dyn Display) -> Response {
    tracing::error!(%error, "back office");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}

pub fn not_found(state: &AppState) -> Response {
    pages::error(state, StatusCode::NOT_FOUND)
}

/// The layout with the session's CSRF token, for pages with forms.
pub async fn layout<'a>(state: &'a AppState, session: &Session) -> Step<Layout<'a>> {
    let token = csrf::token(session)
        .await
        .map_err(|error| fail(state, &error))?;
    Ok(Layout::new(state).with_csrf_token(token))
}

pub fn is_htmx(headers: &HeaderMap) -> bool {
    headers.contains_key("hx-request")
}

/// After a save: a 303 to `path` with `opgeslagen=1` added to its query, or for htmx an `HX-Redirect` there.
pub fn saved(headers: &HeaderMap, path: &str) -> Response {
    let separator = if path.contains('?') { '&' } else { '?' };
    let target = format!("{path}{separator}opgeslagen=1");
    if is_htmx(headers) {
        match HeaderValue::try_from(target) {
            Ok(value) => (
                StatusCode::OK,
                [(HeaderName::from_static("hx-redirect"), value)],
            )
                .into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    } else {
        Redirect::to(&target).into_response()
    }
}

/// A form with mistakes goes back with 422; htmx gets 200, because it only swaps 2xx.
pub fn invalid_status(headers: &HeaderMap) -> StatusCode {
    if is_htmx(headers) {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    }
}

/// A Dutch text; a missing id is logged and shown as the id, so a page still renders.
pub fn t(state: &AppState, id: &str) -> String {
    t_with(state, id, &[])
}

pub fn t_with(state: &AppState, id: &str, args: &[(&str, String)]) -> String {
    let mut fluent = FluentArgs::new();
    for (name, value) in args {
        fluent.set(*name, value.clone());
    }
    let args = (!args.is_empty()).then_some(&fluent);
    state.i18n.t(id, args).unwrap_or_else(|error| {
        tracing::error!(%error, "missing text");
        id.to_owned()
    })
}

pub fn t_count(state: &AppState, id: &str, n: u32) -> String {
    let mut fluent = FluentArgs::new();
    fluent.set("n", i64::from(n));
    state.i18n.t(id, Some(&fluent)).unwrap_or_else(|error| {
        tracing::error!(%error, "missing text");
        id.to_owned()
    })
}

/// Parses a numeric path id; anything else is a 404.
pub fn path_id(state: &AppState, raw: &str) -> Step<i64> {
    raw.parse().map_err(|_| not_found(state).into())
}
