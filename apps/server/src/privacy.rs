//! Privacy tools (F12, ADR 0031): a signed-in customer downloads their data, sets their
//! drop-alert consent and deletes their account from "Mijn bestellingen"; the owner does
//! the export and delete on request from the back office. Nothing here logs an address
//! or a name.

use axum::extract::State;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use domain::CustomerId;
use notify::Composer;
use orders::audit::Actor;
use orders::privacy::{self, DropAlertsRecord, Erase, VoteRecord};
use serde::Deserialize;
use tower_sessions::Session;

use crate::account::{LOGIN_PATH, ORDERS_PATH, message, no_store, server_error};
use crate::customer_session;
use crate::state::AppState;

pub const EXPORT_PATH: &str = "/mijn-bestellingen/gegevens";
pub const CONSENT_PATH: &str = "/mijn-bestellingen/meldingen";
pub const ERASE_PATH: &str = "/mijn-bestellingen/verwijderen";
pub const ERASED_PATH: &str = "/mijn-bestellingen/verwijderd";

const ERASED: (&str, &str) = ("privacy-erased-title", "privacy-erased-text");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(EXPORT_PATH, get(download))
        .route(CONSENT_PATH, post(consent))
        .route(ERASE_PATH, post(erase_own))
        .route(ERASED_PATH, get(erased_page))
}

#[derive(Debug, thiserror::Error)]
pub enum PrivacyError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Orders(#[from] orders::Error),
    #[error(transparent)]
    Subscribers(#[from] notify::subscribers::SubscriberError),
    #[error(transparent)]
    Outbox(#[from] notify::OutboxError),
    #[error(transparent)]
    Polls(#[from] notify::polls::PollError),
}

/// What [`erase`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Erased {
    Done,
    /// An order still waits for payment or pickup.
    OpenOrders,
    Business,
    /// Unknown or already erased.
    NotFound,
}

/// The customer's data as JSON, with the drop-alert subscription and the bread votes of
/// their address. Drop
/// alerts are the only marketing mail, so the subscription is the consent: an unsubscribe
/// link or a sign-up on /meldingen does not touch `customer.marketing_consent`.
///
/// # Errors
/// [`PrivacyError`]; an unknown or erased customer is `Orders(NotFound)`.
pub async fn export_json(state: &AppState, customer: CustomerId) -> Result<String, PrivacyError> {
    let mut conn = state.pool.acquire().await?;
    let mut data = privacy::export(&mut conn, customer).await?;
    let subscription = notify::subscribers::of_address(&mut conn, &data.customer.email).await?;
    data.customer.marketing_consent = subscription.as_ref().is_some_and(|s| s.is_active());
    data.drop_alerts = subscription.map(|s| DropAlertsRecord {
        signed_up_at: s.created_at.to_string(),
        confirmed_at: s.confirmed_at.map(|t| t.to_string()),
        unsubscribed_at: s.unsubscribed_at.map(|t| t.to_string()),
    });
    data.votes = notify::polls::of_address(&mut conn, &data.customer.email)
        .await?
        .into_iter()
        .map(|v| VoteRecord {
            poll: v.poll,
            choice: v.choice,
            wish: v.wish,
            voted_at: v.voted_at.to_string(),
            confirmed_at: v.confirmed_at.map(|t| t.to_string()),
        })
        .collect();
    Ok(data.to_json()?)
}

/// A JSON file to save, never cached.
pub fn json_download(json: String, filename: &str) -> Response {
    let disposition = HeaderValue::try_from(format!("attachment; filename=\"{filename}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    no_store(
        (
            StatusCode::OK,
            [
                (
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/json; charset=utf-8"),
                ),
                (CONTENT_DISPOSITION, disposition),
            ],
            json,
        )
            .into_response(),
    )
}

/// Erases the customer, takes their address off the drop-alert list and out of the bread
/// votes, deletes the emails to or from it still in the outbox, and queues one last email
/// to say it is done, all in one transaction. When that email cannot be composed (company
/// details missing), the account is erased anyway.
///
/// # Errors
/// [`PrivacyError`] when the database fails.
pub async fn erase(
    state: &AppState,
    customer: CustomerId,
    actor: Actor,
) -> Result<Erased, PrivacyError> {
    let mut tx = state.pool.begin().await?;
    let email = match privacy::erase(&mut tx, customer, actor, state.clock.now()).await {
        Ok(Erase::Erased { email }) => email,
        Ok(Erase::OpenOrders) => return Ok(Erased::OpenOrders),
        Ok(Erase::Business) => return Ok(Erased::Business),
        Err(orders::Error::NotFound) => return Ok(Erased::NotFound),
        Err(error) => return Err(error.into()),
    };
    notify::subscribers::forget(&mut tx, &email).await?;
    notify::polls::forget(&mut tx, &email).await?;
    notify::outbox::forget_address(&mut tx, &email).await?;
    let company = notify::Company::load(&mut tx).await?;
    match Composer::new(&state.i18n, &company).account_deleted(&email) {
        Ok(mail) => {
            notify::enqueue(&mut tx, state.clock.as_ref(), &mail, None).await?;
        }
        Err(error) => {
            tracing::warn!(%error, "account erased without a confirmation email");
        }
    }
    tx.commit().await?;
    Ok(Erased::Done)
}

/// A 303 to `path`, or for htmx an `HX-Redirect` there.
pub fn go(headers: &HeaderMap, path: &'static str) -> Response {
    if headers.contains_key("hx-request") {
        (
            StatusCode::OK,
            [(
                HeaderName::from_static("hx-redirect"),
                HeaderValue::from_static(path),
            )],
        )
            .into_response()
    } else {
        Redirect::to(path).into_response()
    }
}

async fn download(State(state): State<AppState>, session: Session) -> Response {
    let me = match customer_session::current(&state, &session).await {
        Ok(Some(me)) => me,
        Ok(None) => return Redirect::to(LOGIN_PATH).into_response(),
        Err(error) => return server_error(&state, &error),
    };
    match export_json(&state, me.id).await {
        Ok(json) => json_download(json, "mijn-gegevens.json"),
        Err(error) => server_error(&state, &error),
    }
}

#[derive(Deserialize)]
pub struct ConsentForm {
    #[serde(default)]
    meldingen: Option<String>,
}

/// Records the consent and signs the address up for drop alerts, or takes it off. The
/// customer proved the address by signing in, so no confirmation email is needed.
async fn set_consent(
    state: &AppState,
    customer: CustomerId,
    consent: bool,
) -> Result<(), PrivacyError> {
    let mut tx = state.pool.begin().await?;
    let email = privacy::set_marketing_consent(&mut tx, customer, consent).await?;
    if consent {
        notify::subscribers::subscribe_verified(&mut tx, state.clock.as_ref(), &email).await?;
    } else {
        notify::subscribers::withdraw(&mut tx, state.clock.as_ref(), &email).await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn consent(
    State(state): State<AppState>,
    session: Session,
    Form(input): Form<ConsentForm>,
) -> Response {
    let me = match customer_session::current(&state, &session).await {
        Ok(Some(me)) => me,
        Ok(None) => return Redirect::to(LOGIN_PATH).into_response(),
        Err(error) => return server_error(&state, &error),
    };
    match set_consent(&state, me.id, input.meldingen.is_some()).await {
        Ok(()) => Redirect::to(&format!("{ORDERS_PATH}?opgeslagen=1")).into_response(),
        Err(error) => server_error(&state, &error),
    }
}

async fn erase_own(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
) -> Response {
    let me = match customer_session::current(&state, &session).await {
        Ok(Some(me)) => me,
        Ok(None) => return go(&headers, LOGIN_PATH),
        Err(error) => return server_error(&state, &error),
    };
    match erase(&state, me.id, Actor::Customer(me.id)).await {
        Ok(Erased::Done | Erased::NotFound) => {
            if let Err(error) = customer_session::sign_out(&state, &session).await {
                return server_error(&state, &error);
            }
            go(&headers, ERASED_PATH)
        }
        Ok(Erased::OpenOrders) => go(&headers, "/mijn-bestellingen?verwijderen=open"),
        Ok(Erased::Business) => go(&headers, "/mijn-bestellingen?verwijderen=zakelijk"),
        Err(error) => server_error(&state, &error),
    }
}

async fn erased_page(State(state): State<AppState>) -> Response {
    message(&state, StatusCode::OK, ERASED, "success", false)
}

#[cfg(test)]
mod tests {
    use i18n::Translator;

    use super::*;

    #[test]
    fn every_text_exists() {
        let i18n = Translator::dutch().expect("Dutch texts");
        i18n.t(ERASED.0, None).expect(ERASED.0);
        i18n.t(ERASED.1, None).expect(ERASED.1);
    }
}
