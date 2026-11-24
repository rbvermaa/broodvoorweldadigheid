//! The public webshop: drop calendar, menu, cart, checkout, order status and the Mollie
//! webhook (F1a, F4, F4b, F4c, F5). See ADR 0015.

mod cart;
mod checkout;
mod drops;
mod lookups;
pub mod money;
mod order;
mod webhook;

use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use bakedays::calendar::{PublicDrop, public_drops};
use bakedays::{PublicStatus, days};
use domain::{BakeDayId, in_bakery_time};
use i18n::{Amsterdam, FluentArgs};
use orders::Orders;
use sqlx::PgConnection;
use tower_sessions::Session;

use crate::components::{Drop, DropCharity};
use crate::pages;
use crate::state::AppState;

pub use lookups::PaymentLookups;
pub use webhook::settle;

const CART_KEY: &str = "cart";
/// Checkout: a burst of 5 tries, then one every 12 seconds per client (spec, Security).
pub const CHECKOUT_PERIOD: Duration = Duration::from_secs(12);
pub const CHECKOUT_BURST: u32 = 5;

/// Pages and htmx actions behind the session and CSRF layers.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/drops", get(drops::calendar))
        .route("/drops.ics", get(drops::ical))
        .route("/drops/{id}", get(drops::menu))
        .route("/drops/{id}/mandje", post(cart::update))
        .route("/drops/{id}/afrekenen", get(checkout::form))
        .route("/bestelling/{token}", get(order::page))
}

/// The checkout POST, which the server rate limits (spec, Security).
pub fn checkout_routes() -> Router<AppState> {
    Router::new().route("/drops/{id}/bestellen", post(checkout::submit))
}

/// Mollie's webhook: no session and no CSRF token, since Mollie cannot send one. It
/// only carries a payment id, which is looked up at Mollie before anything changes.
pub fn webhook_routes() -> Router<AppState> {
    Router::new().route("/webhooks/mollie", post(webhook::mollie))
}

pub(crate) fn drop_path(id: BakeDayId) -> String {
    format!("/drops/{}", id.get())
}

/// One public drop by id, as the calendar shows it; `None` for an unknown or
/// production-only day.
pub(crate) async fn find_drop(
    conn: &mut PgConnection,
    state: &AppState,
    id: BakeDayId,
) -> Result<Option<PublicDrop>, bakedays::Error> {
    let day = match days::get(conn, id).await {
        Ok(day) => day,
        Err(bakedays::Error::NotFound) => return Ok(None),
        Err(err) => return Err(err),
    };
    let clock = state.clock.as_ref();
    Ok(
        public_drops(conn, clock, day.date, day.date, &Orders::new(clock))
            .await?
            .into_iter()
            .find(|drop| drop.bake_day_id == id),
    )
}

const fn status_id(status: PublicStatus) -> &'static str {
    match status {
        PublicStatus::Planned => "drop-status-planned",
        PublicStatus::MenuAnnounced => "drop-status-menu-announced",
        PublicStatus::Open => "drop-status-open",
        PublicStatus::SoldOut => "drop-status-sold-out",
        PublicStatus::Closed => "drop-status-closed",
        PublicStatus::Cancelled => "drop-status-cancelled",
    }
}

/// The display text of a drop. `link` makes the date link to the drop's page.
pub(crate) fn drop_view(
    state: &AppState,
    drop: &PublicDrop,
    link: bool,
) -> Result<Drop, i18n::I18nError> {
    let t = &state.i18n;
    let zone = Amsterdam::new();
    let pickups = drop
        .pickups
        .iter()
        .map(|pickup| {
            let mut args = FluentArgs::new();
            args.set("day", t.long_date(zone.date(pickup.starts_at)));
            args.set("range", zone.time_range(pickup.starts_at, pickup.ends_at)?);
            args.set("location", pickup.location_name.clone());
            args.set("address", pickup.address.clone());
            t.t("drop-pickup", Some(&args))
        })
        .collect::<Result<_, _>>()?;
    let deadline = match drop.cutoff_at {
        Some(cutoff) => {
            let mut at = FluentArgs::new();
            at.set("date", t.long_date(zone.date(cutoff)));
            at.set("time", zone.time(cutoff));
            let mut args = FluentArgs::new();
            args.set("deadline", t.t("calendar-deadline-at", Some(&at))?);
            t.t("order-deadline", Some(&args))?
        }
        None => String::new(),
    };
    let charity = match &drop.charity {
        Some(charity) => {
            let mut args = FluentArgs::new();
            args.set("charity", charity.name.clone());
            let web = charity.url.starts_with("https://") || charity.url.starts_with("http://");
            Some(DropCharity {
                text: t.t("drop-charity", Some(&args))?,
                url: if web {
                    charity.url.clone()
                } else {
                    String::new()
                },
                link: t.t("drop-charity-link", Some(&args))?,
            })
        }
        None => None,
    };
    Ok(Drop {
        date: t.long_date(drop.date),
        status: t.t(status_id(drop.status), None)?,
        open: drop.status == PublicStatus::Open,
        pickups,
        deadline,
        charity,
        link: if link {
            drop_path(drop.bake_day_id)
        } else {
            String::new()
        },
    })
}

/// Reads the session from its store, which takes a pool connection of its own. Pages call
/// it before they hold a connection, so a full pool cannot leave every request waiting
/// for a second one. The page needs the CSRF token anyway, so that is what it reads.
pub(crate) async fn load_session(session: &Session) -> Result<(), tower_sessions::session::Error> {
    crate::csrf::token(session).await.map(|_| ())
}

/// The session's cart, if it is for `day`. An unreadable cart counts as none.
pub(crate) async fn session_cart(session: &Session) -> Option<orders::Cart> {
    match session.get::<orders::Cart>(CART_KEY).await {
        Ok(cart) => cart,
        Err(error) => {
            tracing::warn!(%error, "dropping an unreadable cart");
            None
        }
    }
}

pub(crate) async fn save_cart(
    session: &Session,
    cart: &orders::Cart,
) -> Result<(), tower_sessions::session::Error> {
    if cart.is_empty() {
        session.remove::<orders::Cart>(CART_KEY).await.map(|_| ())
    } else {
        session.insert(CART_KEY, cart).await
    }
}

/// Today in the bakery's time zone.
pub(crate) fn today(state: &AppState) -> jiff::civil::Date {
    in_bakery_time(state.clock.now()).date()
}

pub(crate) fn server_error(
    state: &AppState,
    what: &str,
    error: &dyn std::fmt::Display,
) -> Response {
    tracing::error!(%error, "{what}");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}
