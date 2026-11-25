//! F9a over HTTP: sign up, confirm, unsubscribe (with the RFC 8058 one-click POST).
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{Request, StatusCode};
use db::PgPool;
use i18n::Translator;
use jiff::Timestamp;
use server::AppState;
use server::config::Config;
use tower::ServiceExt;

const ORIGIN: &str = "https://broodvoorweldadigheid.nl";

fn dutch(id: &str) -> String {
    Translator::dutch().unwrap().t(id, None).unwrap()
}

async fn app(pool: &PgPool) -> Router {
    sqlx::query(
        "UPDATE company SET kvk = '12345678',
            address = 'Majoor van Swietenlaan 15, 8382 CE Frederiksoord'",
    )
    .execute(pool)
    .await
    .unwrap();
    let config = Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://unused/db".to_owned()),
        "PUBLIC_URL" => Some(ORIGIN.to_owned()),
        _ => None,
    })
    .unwrap();
    let clock = testkit::FakeClock::new("2026-11-24T10:00:00Z".parse::<Timestamp>().unwrap());
    server::router(
        AppState::new(pool.clone(), Translator::dutch().unwrap(), config)
            .with_clock(Arc::new(clock)),
    )
}

struct Reply {
    status: StatusCode,
    cookie: Option<String>,
    csp: Option<String>,
    body: String,
}

async fn send(app: &Router, mut request: Request<Body>) -> Reply {
    request
        .extensions_mut()
        .insert(ConnectInfo("192.0.2.7:4000".parse::<SocketAddr>().unwrap()));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let csp = response
        .headers()
        .get(CONTENT_SECURITY_POLICY)
        .map(|v| v.to_str().unwrap().to_owned());
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    Reply {
        status,
        cookie,
        csp,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn get(app: &Router, path: &str) -> Reply {
    send(app, Request::get(path).body(Body::empty()).unwrap()).await
}

async fn post(app: &Router, path: &str, cookie: Option<&str>, body: &str) -> Reply {
    let mut request = Request::post(path).header(CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    send(app, request.body(Body::from(body.to_owned())).unwrap()).await
}

/// A session cookie and its CSRF token, from the sign-up page.
async fn session(app: &Router) -> (String, String) {
    let page = get(app, "/meldingen").await;
    assert_eq!(page.status, StatusCode::OK);
    let marker = "name=\"_csrf\" value=\"";
    let start = page.body.find(marker).expect("csrf field") + marker.len();
    let end = start + page.body[start..].find('"').unwrap();
    (
        page.cookie.expect("session"),
        page.body[start..end].to_owned(),
    )
}

async fn sign_up(app: &Router, email: &str) -> Reply {
    let (cookie, token) = session(app).await;
    let body = format!("_csrf={token}&email={}", email.replace('@', "%40"));
    post(app, "/meldingen", Some(&cookie), &body).await
}

fn link_token(text: &str, path: &str) -> String {
    let start = text.find(path).expect("link") + path.len();
    text[start..start + 43].to_owned()
}

async fn confirmations(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT text_body FROM email_outbox WHERE kind = 'subscription_confirmation' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn active(pool: &PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM subscriber WHERE confirmed_at IS NOT NULL AND unsubscribed_at IS NULL",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sign_up_page_asks_for_consent_and_carries_a_csrf_token(pool: PgPool) {
    let app = app(&pool).await;
    let page = get(&app, "/meldingen").await;
    assert_eq!(page.status, StatusCode::OK);
    for id in [
        "alerts-title",
        "alerts-consent",
        "alerts-email",
        "alerts-submit",
    ] {
        assert!(page.body.contains(&dutch(id)), "{id}");
    }
    assert!(page.body.contains("name=\"_csrf\""));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn signing_up_needs_the_csrf_token(pool: PgPool) {
    let app = app(&pool).await;
    let reply = post(&app, "/meldingen", None, "email=iemand%40example.nl").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(confirmations(&pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn every_address_gets_the_same_answer(pool: PgPool) {
    let app = app(&pool).await;
    let new = sign_up(&app, "iemand@example.nl").await;
    assert_eq!(new.status, StatusCode::OK);
    assert!(new.body.contains(&dutch("alerts-check-text")));
    assert_eq!(confirmations(&pool).await.len(), 1);

    let waiting = sign_up(&app, "IEMAND@example.nl").await;
    let token = link_token(&confirmations(&pool).await[0], "/meldingen/bevestig/");
    get(&app, &format!("/meldingen/bevestig/{token}")).await;
    let confirmed = sign_up(&app, "iemand@example.nl").await;
    for reply in [&waiting, &confirmed] {
        assert_eq!(reply.status, new.status);
        assert_eq!(reply.body, new.body);
    }
    assert_eq!(confirmations(&pool).await.len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_bad_address_shows_the_form_again(pool: PgPool) {
    let app = app(&pool).await;
    let reply = sign_up(&app, "geen-adres").await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains(&dutch("alerts-email-error")));
    assert!(reply.body.contains("value=\"geen-adres\""));
    assert!(confirmations(&pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_confirmation_link_confirms_once_valid(pool: PgPool) {
    let app = app(&pool).await;
    sign_up(&app, "iemand@example.nl").await;
    let mail = &confirmations(&pool).await[0];
    assert!(mail.contains(&format!("{ORIGIN}/meldingen/bevestig/")));
    let token = link_token(mail, "/meldingen/bevestig/");

    let reply = get(&app, &format!("/meldingen/bevestig/{token}")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains(&dutch("alerts-confirmed-title")));
    assert_eq!(active(&pool).await, 1);

    let reply = get(&app, &format!("/meldingen/bevestig/{}", "x".repeat(43))).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert!(reply.body.contains(&dutch("alerts-confirm-failed-title")));
}

async fn unsubscribe_token(pool: &PgPool) -> String {
    let id: i64 = sqlx::query_scalar("SELECT id FROM subscriber")
        .fetch_one(pool)
        .await
        .unwrap();
    let clock = testkit::FakeClock::new("2026-11-24T10:00:00Z".parse::<Timestamp>().unwrap());
    let mut conn = pool.acquire().await.unwrap();
    let url = notify::subscribers::unsubscribe_link(
        &mut conn,
        &clock,
        &notify::subscribers::Links::new(ORIGIN),
        id,
    )
    .await
    .unwrap();
    url.rsplit('/').next().unwrap().to_owned()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn unsubscribe_page_shows_a_button_that_posts_without_a_session(pool: PgPool) {
    let app = app(&pool).await;
    sign_up(&app, "iemand@example.nl").await;
    let token = link_token(&confirmations(&pool).await[0], "/meldingen/bevestig/");
    get(&app, &format!("/meldingen/bevestig/{token}")).await;
    let unsubscribe = unsubscribe_token(&pool).await;
    let path = format!("/meldingen/afmelden/{unsubscribe}");

    let page = get(&app, &path).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(&format!("action=\"{path}\"")));
    assert!(page.body.contains(&dutch("alerts-unsubscribe-submit")));
    assert_eq!(active(&pool).await, 1, "opening the page changes nothing");

    let reply = post(&app, &path, None, "List-Unsubscribe=One-Click").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains(&dutch("alerts-unsubscribed-title")));
    assert!(reply.cookie.is_none(), "no session for mail clients");
    assert_eq!(reply.csp.as_deref(), Some(server::security::CSP));
    assert_eq!(active(&pool).await, 0);

    let again = post(&app, &path, None, "List-Unsubscribe=One-Click").await;
    assert_eq!(again.status, StatusCode::OK);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn unknown_unsubscribe_links_are_not_found(pool: PgPool) {
    let app = app(&pool).await;
    let path = format!("/meldingen/afmelden/{}", "x".repeat(43));
    assert_eq!(get(&app, &path).await.status, StatusCode::NOT_FOUND);
    let reply = post(&app, &path, None, "List-Unsubscribe=One-Click").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert!(reply.body.contains(&dutch("alerts-unknown-link-title")));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sign_ups_are_rate_limited(pool: PgPool) {
    let app = app(&pool).await;
    let (cookie, token) = session(&app).await;
    let mut statuses = Vec::new();
    for n in 0..6 {
        let body = format!("_csrf={token}&email=lid{n}%40example.nl");
        statuses.push(post(&app, "/meldingen", Some(&cookie), &body).await.status);
    }
    assert_eq!(statuses[..5], [StatusCode::OK; 5]);
    assert_eq!(statuses[5], StatusCode::TOO_MANY_REQUESTS);
}
