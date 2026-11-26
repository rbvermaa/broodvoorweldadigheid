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

use crate::components::{Drop, DropCharity, DropOrdering, DropPickup, DropStatus};
use crate::pages;
use crate::state::AppState;

pub(crate) use drops::for_home;
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

/// The status a customer can act on; planned drops show when ordering opens instead.
const fn status_id(status: PublicStatus) -> Option<&'static str> {
    match status {
        PublicStatus::Planned | PublicStatus::MenuAnnounced => None,
        PublicStatus::Open => Some("drop-status-open"),
        PublicStatus::SoldOut => Some("drop-status-sold-out"),
        PublicStatus::Closed => Some("drop-status-closed"),
        PublicStatus::Cancelled => Some("drop-status-cancelled"),
    }
}

/// `za 12 dec`, with the year when it is not `today`'s.
fn short_date(t: &i18n::Translator, date: jiff::civil::Date, today: jiff::civil::Date) -> String {
    if date.year() == today.year() {
        t.short_date(date)
    } else {
        t.short_date_with_year(date)
    }
}

fn datetime(zone: &Amsterdam, at: jiff::Timestamp) -> String {
    zone.local(at).strftime("%Y-%m-%dT%H:%M%:z").to_string()
}

/// The display text of a drop. `link` makes the date link to the drop's page.
pub(crate) fn drop_view(
    state: &AppState,
    drop: &PublicDrop,
    link: bool,
) -> Result<Drop, i18n::I18nError> {
    let t = &state.i18n;
    let zone = Amsterdam::new();
    let now = state.clock.now();
    let today = zone.date(now);
    let pickups = drop
        .pickups
        .iter()
        .map(|pickup| {
            let day = zone.date(pickup.starts_at);
            let range = zone.time_range(pickup.starts_at, pickup.ends_at)?;
            Ok(DropPickup {
                when: if day == drop.date {
                    range
                } else {
                    format!("{} {range}", short_date(t, day, today))
                },
                datetime: datetime(&zone, pickup.starts_at),
                location: pickup.location_name.clone(),
                address: pickup.address.clone(),
            })
        })
        .collect::<Result<_, i18n::I18nError>>()?;
    let ordering = match (drop.status, drop.opens_at, drop.cutoff_at) {
        (PublicStatus::Planned | PublicStatus::MenuAnnounced, Some(opens), _) if opens > now => {
            Some(("drop-label-order-from", "drop-order-from", opens))
        }
        (PublicStatus::Open, _, Some(cutoff)) => {
            Some(("drop-label-order-until", "drop-order-until", cutoff))
        }
        (PublicStatus::Planned | PublicStatus::MenuAnnounced, _, Some(cutoff)) => {
            Some(("drop-label-deadline", "drop-deadline", cutoff))
        }
        _ => None,
    };
    let ordering = match ordering {
        Some((label, text, at)) => {
            let when = format!("{} {}", short_date(t, zone.date(at), today), zone.time(at));
            let mut args = FluentArgs::new();
            args.set("when", when.clone());
            Some(DropOrdering {
                label: t.t(label, None)?,
                text: t.t(text, Some(&args))?,
                when,
                datetime: datetime(&zone, at),
            })
        }
        None => None,
    };
    let status = match status_id(drop.status) {
        Some(id) => Some(DropStatus {
            text: t.t(id, None)?,
            open: drop.status == PublicStatus::Open,
        }),
        None => None,
    };
    let charity = match &drop.charity {
        Some(charity) => {
            let web = charity.url.starts_with("https://") || charity.url.starts_with("http://");
            let mut args = FluentArgs::new();
            args.set("charity", charity.name.clone());
            Some(DropCharity {
                name: charity.name.clone(),
                list_text: t.t("drop-for-charity", Some(&args))?,
                description: charity.description.clone(),
                url: if web {
                    charity.url.clone()
                } else {
                    String::new()
                },
            })
        }
        None => None,
    };
    let mut heading = FluentArgs::new();
    heading.set("date", t.long_date(drop.date));
    Ok(Drop {
        date: short_date(t, drop.date, today),
        heading: t.t("drop-heading", Some(&heading))?,
        datetime: drop.date.to_string(),
        status,
        pickups,
        ordering,
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
