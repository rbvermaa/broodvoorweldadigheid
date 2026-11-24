//! Customer login by email magic link and the customer's order overview (spec, Security;
//! ADR 0028). The base for F4d, F11a and F12.

use std::time::Duration;

use askama::Template;
use auth::customer::{self, LINK_VALID, LinkRequest};
use axum::extract::{Path, State};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use i18n::FluentArgs;
use notify::{Composer, LoginLink};
use orders::OrderStatus;
use serde::Deserialize;
use tower_sessions::Session;

use crate::components::{Cell, Table};
use crate::csrf;
use crate::customer_session;
use crate::pages::{self, Layout};
use crate::state::AppState;

pub const LOGIN_PATH: &str = "/inloggen";
pub const ORDERS_PATH: &str = "/mijn-bestellingen";
/// Asking for a link and using one: a burst of 5, then one every 12 seconds per client
/// (spec, Security).
pub const LOGIN_PERIOD: Duration = Duration::from_secs(12);
pub const LOGIN_BURST: u32 = 5;

/// The pages, behind the session and CSRF layers.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(LOGIN_PATH, get(form))
        .route("/inloggen/{token}", get(link_page))
        .route(ORDERS_PATH, get(my_orders))
        .route("/uitloggen", post(logout))
}

/// Asking for a link and using one, which the server rate limits together outside the
/// session and CSRF layers (ADR 0007).
pub fn login_routes() -> Router<AppState> {
    Router::new()
        .route(LOGIN_PATH, post(request_link))
        .route("/inloggen/{token}", post(use_link))
}

#[derive(Template)]
#[template(path = "account/login.html")]
struct LoginPage<'a> {
    layout: Layout<'a>,
    email: String,
    error: String,
}

#[derive(Template)]
#[template(path = "account/message.html")]
struct Message<'a> {
    layout: Layout<'a>,
    title: &'static str,
    text: &'static str,
    kind: &'static str,
    login_link: bool,
}

#[derive(Template)]
#[template(path = "account/link.html")]
struct LinkPage<'a> {
    layout: Layout<'a>,
    action: String,
}

#[derive(Template)]
#[template(path = "account/orders.html")]
struct OrdersPage<'a> {
    layout: Layout<'a>,
    signed_in_as: String,
    orders: Table,
}

#[derive(Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    email: String,
}

const CHECK_MAIL: (&str, &str) = ("account-check-title", "account-check-text");
const LINK_FAILED: (&str, &str) = ("account-link-failed-title", "account-link-failed-text");

fn message(
    state: &AppState,
    status: StatusCode,
    (title, text): (&'static str, &'static str),
    kind: &'static str,
    login_link: bool,
) -> Response {
    pages::render(
        state,
        status,
        &Message {
            layout: Layout::new(state),
            title,
            text,
            kind,
            login_link,
        },
    )
}

/// For pages whose URL holds a secret or that show a customer's own data.
fn no_store(mut page: Response) -> Response {
    page.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    page
}

fn link_failed(state: &AppState) -> Response {
    message(state, StatusCode::NOT_FOUND, LINK_FAILED, "warning", true)
}

fn server_error(state: &AppState, error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, "customer login");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}

fn status_id(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::PendingPayment => "account-status-pending_payment",
        OrderStatus::Paid => "account-status-paid",
        OrderStatus::Confirmed => "account-status-confirmed",
        OrderStatus::PickedUp => "account-status-picked_up",
        OrderStatus::Cancelled => "account-status-cancelled",
        OrderStatus::Refunded => "account-status-refunded",
    }
}

async fn login_form(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    email: String,
    error: String,
) -> Response {
    let token = match csrf::token(session).await {
        Ok(token) => token,
        Err(error) => return server_error(state, &error),
    };
    pages::render(
        state,
        status,
        &LoginPage {
            layout: Layout::new(state).with_csrf_token(token),
            email,
            error,
        },
    )
}

async fn form(State(state): State<AppState>, session: Session) -> Response {
    login_form(
        &state,
        &session,
        StatusCode::OK,
        String::new(),
        String::new(),
    )
    .await
}

/// Every well-formed address gets the same page, whether a customer has it, has asked too
/// often, or the email could not be composed: nobody learns who orders here.
async fn request_link(
    State(state): State<AppState>,
    session: Session,
    Form(input): Form<LoginForm>,
) -> Response {
    let email = input.email.trim();
    if !looks_like_email(email) {
        let error = match state.i18n.t("account-email-error", None) {
            Ok(text) => text,
            Err(error) => return server_error(&state, &error),
        };
        return login_form(
            &state,
            &session,
            StatusCode::UNPROCESSABLE_ENTITY,
            input.email,
            error,
        )
        .await;
    }
    if let Err(error) = queue_link(&state, email).await {
        tracing::error!(%error, "cannot send a login link");
    }
    message(&state, StatusCode::OK, CHECK_MAIL, "info", false)
}

/// The same loose check checkout makes, so every customer it stored can ask for a link.
fn looks_like_email(email: &str) -> bool {
    email.len() <= 254
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.contains('@')
        })
}

#[derive(Debug, thiserror::Error)]
enum QueueError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Customer(#[from] customer::Error),
    #[error(transparent)]
    Compose(#[from] notify::ComposeError),
    #[error(transparent)]
    Outbox(#[from] notify::OutboxError),
}

/// Issues the link and queues its email in one transaction, so a link that cannot be
/// mailed is never stored.
async fn queue_link(state: &AppState, email: &str) -> Result<(), QueueError> {
    let mut tx = state.pool.begin().await?;
    let link = match customer::request_link(&mut tx, email, state.clock.now()).await? {
        LinkRequest::Issued(link) => link,
        LinkRequest::Unknown | LinkRequest::Throttled => return Ok(()),
    };
    let company = notify::Company::load(&mut tx).await?;
    let composer = Composer::new(&state.i18n, &company);
    let mail = composer.login_link(&LoginLink {
        to: link.email,
        login_url: format!(
            "{}{LOGIN_PATH}/{}",
            state.config.public_url.trim_end_matches('/'),
            link.token.as_str()
        ),
        valid_minutes: LINK_VALID.as_mins(),
    })?;
    notify::enqueue_expiring(&mut tx, state.clock.as_ref(), &mail, link.expires_at).await?;
    tx.commit().await?;
    Ok(())
}

/// The page behind the link in the email. Signing in takes a press on its button, so a
/// mail scanner that opens the link does not use it up.
async fn link_page(
    State(state): State<AppState>,
    session: Session,
    Path(token): Path<String>,
) -> Response {
    let csrf = match csrf::token(&session).await {
        Ok(csrf) => csrf,
        Err(error) => return server_error(&state, &error),
    };
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => return server_error(&state, &error),
    };
    match customer::link_is_valid(&mut conn, &token, state.clock.now()).await {
        Ok(true) => no_store(pages::render(
            &state,
            StatusCode::OK,
            &LinkPage {
                layout: Layout::new(&state).with_csrf_token(csrf),
                action: format!("{LOGIN_PATH}/{token}"),
            },
        )),
        Ok(false) => link_failed(&state),
        Err(error) => server_error(&state, &error),
    }
}

async fn use_link(
    State(state): State<AppState>,
    session: Session,
    Path(token): Path<String>,
) -> Response {
    let redeemed = {
        let mut conn = match state.pool.acquire().await {
            Ok(conn) => conn,
            Err(error) => return server_error(&state, &error),
        };
        customer::redeem_link(&mut conn, &token, state.clock.now()).await
    };
    match redeemed {
        Ok(Some(id)) => match customer_session::sign_in(&state, &session, id).await {
            Ok(()) => Redirect::to(ORDERS_PATH).into_response(),
            Err(error) => server_error(&state, &error),
        },
        Ok(None) => link_failed(&state),
        Err(error) => server_error(&state, &error),
    }
}

async fn my_orders(State(state): State<AppState>, session: Session) -> Response {
    let csrf = match csrf::token(&session).await {
        Ok(csrf) => csrf,
        Err(error) => return server_error(&state, &error),
    };
    let me = match customer_session::current(&state, &session).await {
        Ok(Some(me)) => me,
        Ok(None) => return Redirect::to(LOGIN_PATH).into_response(),
        Err(error) => return server_error(&state, &error),
    };
    let list = {
        let mut conn = match state.pool.acquire().await {
            Ok(conn) => conn,
            Err(error) => return server_error(&state, &error),
        };
        orders::order::for_customer(&mut conn, me.id).await
    };
    let list = match list {
        Ok(list) => list,
        Err(error) => return server_error(&state, &error),
    };
    let page = orders_page(&state, &me.email, &list);
    match page {
        Ok((signed_in_as, orders)) => no_store(pages::render(
            &state,
            StatusCode::OK,
            &OrdersPage {
                layout: Layout::new(&state).with_csrf_token(csrf),
                signed_in_as,
                orders,
            },
        )),
        Err(error) => server_error(&state, &error),
    }
}

fn orders_page(
    state: &AppState,
    email: &str,
    list: &[orders::order::CustomerOrder],
) -> Result<(String, Table), i18n::I18nError> {
    let t = |id: &str| state.i18n.t(id, None);
    let mut args = FluentArgs::new();
    args.set("email", email.to_owned());
    let signed_in_as = state.i18n.t("account-orders-signed-in-as", Some(&args))?;
    let rows = list
        .iter()
        .map(|order| {
            Ok(vec![
                Cell::link(
                    order.number.to_string(),
                    format!("/bestelling/{}", order.status_token),
                ),
                Cell::text(state.i18n.long_date(order.date)),
                Cell::text(t(status_id(order.status))?),
                Cell::number(i18n::money(order.total.cents())),
            ])
        })
        .collect::<Result<_, i18n::I18nError>>()?;
    let table = Table {
        id: "mijn-bestellingen".to_owned(),
        caption: t("account-orders-caption")?,
        head: vec![
            Cell::text(t("account-orders-number")?),
            Cell::text(t("account-orders-date")?),
            Cell::text(t("account-orders-status")?),
            Cell::number(t("account-orders-total")?),
        ],
        rows,
    };
    Ok((signed_in_as, table))
}

async fn logout(State(state): State<AppState>, session: Session) -> Response {
    if let Err(error) = customer_session::sign_out(&state, &session).await {
        return server_error(&state, &error);
    }
    Redirect::to("/").into_response()
}

#[cfg(test)]
mod tests {
    use i18n::Translator;

    use super::*;

    #[test]
    fn every_text_exists() {
        let i18n = Translator::dutch().expect("Dutch texts");
        for (title, text) in [CHECK_MAIL, LINK_FAILED] {
            i18n.t(title, None).expect(title);
            i18n.t(text, None).expect(text);
        }
        for status in OrderStatus::ALL {
            let id = status_id(*status);
            i18n.t(id, None).expect(id);
        }
    }
}
