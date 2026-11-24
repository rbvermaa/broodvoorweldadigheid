//! The back office under `/staff`: catalogue, pickup locations, charities, bake days,
//! extra production, orders, the production and pickup lists, the reports, and customer
//! data requests (F12). Every handler takes [`Staff`] and checks a permission (ADR 0010).
//! Forms post normally and redirect with `?opgeslagen=1`; htmx only adds inline checks and
//! partial updates on top (ADR 0019).

pub mod bake_days;
pub mod charities;
pub mod customers;
pub mod extras;
pub mod form;
pub mod lists;
pub mod locations;
pub mod orders;
pub mod payouts;
pub mod products;
pub mod reports;
pub mod sick_day;

use std::fmt::Display;

use auth::Permission;
use axum::Router;
use axum::http::header::HeaderName;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use i18n::FluentArgs;
use serde::Deserialize;
use tower_sessions::Session;

use crate::csrf;
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

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

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            customers::PATH,
            get(customers::page).post(customers::search),
        )
        .route("/staff/customers/{id}/export", get(customers::export))
        .route("/staff/customers/{id}/erase", post(customers::erase))
        .route("/staff/products", get(products::list))
        .route(
            "/staff/products/new",
            get(products::new_form).post(products::create),
        )
        .route("/staff/products/check", post(products::check))
        .route(
            "/staff/products/{id}",
            get(products::edit_form).post(products::update),
        )
        .route(
            "/staff/products/{id}/approve",
            get(products::approve_form).post(products::approve),
        )
        .route("/staff/charities", get(charities::list))
        .route(
            "/staff/charities/new",
            get(charities::new_form).post(charities::create),
        )
        .route("/staff/charities/check", post(charities::check))
        .route(
            "/staff/charities/{id}",
            get(charities::edit_form).post(charities::update),
        )
        .route("/staff/charities/{id}/archive", post(charities::archive))
        .route("/staff/locations", get(locations::list))
        .route(
            "/staff/locations/new",
            get(locations::new_form).post(locations::create),
        )
        .route(
            "/staff/locations/{id}",
            get(locations::edit_form).post(locations::update),
        )
        .route(
            "/staff/locations/{id}/windows/new",
            get(locations::new_window_form).post(locations::create_window),
        )
        .route(
            "/staff/locations/windows/check",
            post(locations::check_window),
        )
        .route(
            "/staff/locations/{id}/windows/{window}",
            get(locations::edit_window_form).post(locations::update_window),
        )
        .route("/staff/bake-days", get(bake_days::list))
        .route(
            "/staff/bake-days/new",
            get(bake_days::new_form).post(bake_days::create),
        )
        .route("/staff/bake-days/{id}", get(bake_days::show))
        .route("/staff/bake-days/{id}/schedule", post(bake_days::schedule))
        .route("/staff/bake-days/{id}/charity", post(bake_days::charity))
        .route("/staff/bake-days/{id}/range", post(bake_days::set_product))
        .route(
            "/staff/bake-days/{id}/range/{product}",
            post(bake_days::update_product),
        )
        .route(
            "/staff/bake-days/{id}/range/{product}/remove",
            post(bake_days::remove_product),
        )
        .route("/staff/bake-days/{id}/copy", post(bake_days::copy))
        .route("/staff/bake-days/{id}/repeat", post(bake_days::repeat))
        .route("/staff/bake-days/{id}/slots", post(bake_days::add_slot))
        .route(
            "/staff/bake-days/{id}/slots/{slot}",
            post(bake_days::update_slot),
        )
        .route(
            "/staff/bake-days/{id}/slots/{slot}/remove",
            post(bake_days::remove_slot),
        )
        .route("/staff/bake-days/{id}/open", post(bake_days::open))
        .route("/staff/bake-days/{id}/close", post(bake_days::close))
        .route(
            "/staff/bake-days/{id}/extras",
            get(extras::page).post(extras::add),
        )
        .route("/staff/bake-days/{id}/extras/{extra}", post(extras::update))
        .route(
            "/staff/bake-days/{id}/extras/{extra}/remove",
            post(extras::remove),
        )
        .route(
            "/staff/bake-days/{id}/production",
            get(lists::production_page),
        )
        .route(
            "/staff/bake-days/{id}/production/pdf",
            get(lists::production_pdf),
        )
        .route("/staff/bake-days/{id}/pickup-list", get(lists::pickup_page))
        .route(
            "/staff/bake-days/{id}/pickup-list/pdf",
            get(lists::pickup_pdf),
        )
        .route(
            "/staff/bake-days/{id}/pickup-list/{order}/picked-up",
            post(lists::picked_up),
        )
        .route(
            "/staff/bake-days/{id}/pickup-list/{order}/undo",
            post(lists::undo),
        )
        .route("/staff/reports", get(reports::index))
        .route("/staff/reports/sales", get(reports::sales_page))
        .route("/staff/reports/sales.csv", get(reports::sales_csv))
        .route(
            "/staff/reports/pay-what-you-want.csv",
            get(reports::pay_what_you_want_csv),
        )
        .route("/staff/reports/donations", get(reports::donations_page))
        .route(
            "/staff/reports/donations/drops.csv",
            get(reports::donation_drops_csv),
        )
        .route(
            "/staff/reports/donations/charities.csv",
            get(reports::donation_charities_csv),
        )
        .route(
            "/staff/reports/contribution",
            get(reports::contribution_page),
        )
        .route("/staff/reports/contribution/costs", post(reports::add_cost))
        .route("/staff/reports/payouts", get(payouts::list))
        .route("/staff/reports/payouts/{id}", get(payouts::detail))
        .route(
            "/staff/reports/payouts/{id}/transfers",
            post(payouts::record),
        )
        .route("/staff/orders", get(orders::days_list))
        .route("/staff/bake-days/{id}/orders", get(orders::list))
        .route("/staff/orders/{id}", get(orders::show))
        .route("/staff/orders/{id}/pickup", post(orders::move_pickup))
        .route("/staff/orders/{id}/picked-up", post(orders::picked_up))
        .route("/staff/orders/{id}/cancel", post(orders::cancel))
        .route(
            "/staff/orders/{id}/refunds/{refund}/retry",
            post(orders::retry_refund),
        )
        .route(
            "/staff/bake-days/{id}/cancel",
            get(sick_day::page).post(sick_day::cancel),
        )
}

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
