#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{Request, StatusCode};
use catalog::faq::{self, FaqInput};
use db::PgPool;
use i18n::Translator;
use jiff::SignedDuration;
use server::AppState;
use server::config::Config;
use server::site::{self, MESSAGE_MAX, NAME_MAX, SEND_BURST};
use testkit::{FakeClock, FakeEmailSender};
use tower::ServiceExt;

const BAKERY: &str = "bakker@example.org";

fn dutch(id: &str) -> String {
    Translator::dutch()
        .expect("Dutch texts")
        .t(id, None)
        .expect("message")
}

fn state(pool: PgPool, contact_email: Option<&str>) -> AppState {
    let config = Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://unused/db".to_owned()),
        "BAKERY_CONTACT_EMAIL" => contact_email.map(str::to_owned),
        _ => None,
    })
    .expect("config");
    AppState::new(pool, Translator::dutch().expect("Dutch texts"), config)
}

struct Site {
    app: Router,
    clock: Arc<FakeClock>,
    pool: PgPool,
}

fn site_with(pool: PgPool, contact_email: Option<&str>) -> Site {
    let clock = Arc::new(FakeClock::new("2026-12-01T09:00:00Z".parse().unwrap()));
    let state = state(pool.clone(), contact_email);
    let routes = site::routes(&state, clock.clone());
    Site {
        app: server::with_shell(routes, state),
        clock,
        pool,
    }
}

fn site(pool: PgPool) -> Site {
    site_with(pool, Some(BAKERY))
}

struct Reply {
    status: StatusCode,
    cookie: Option<String>,
    body: String,
}

async fn send(app: &Router, mut request: Request<Body>) -> Reply {
    request.extensions_mut().insert(ConnectInfo(
        "192.0.2.7:5000".parse::<SocketAddr>().expect("addr"),
    ));
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let cookie = response.headers().get(SET_COOKIE).map(|value| {
        value
            .to_str()
            .expect("ascii")
            .split(';')
            .next()
            .expect("pair")
            .to_owned()
    });
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    Reply {
        status,
        cookie,
        body: String::from_utf8(bytes.to_vec()).expect("utf-8"),
    }
}

async fn get(app: &Router, path: &str, cookie: Option<&str>) -> Reply {
    let mut request = Request::get(path);
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    send(app, request.body(Body::empty()).expect("request")).await
}

/// A browser that opened the contact form: its session cookie and CSRF token.
struct Visitor {
    cookie: String,
    token: String,
}

async fn open_form(site: &Site) -> Visitor {
    let reply = get(&site.app, "/contact", None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let token = reply
        .body
        .split("name=\"_csrf\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("csrf field")
        .to_owned();
    Visitor {
        cookie: reply.cookie.expect("session cookie"),
        token,
    }
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{name}={}", encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

async fn post(site: &Site, cookie: &str, fields: &[(&str, &str)]) -> Reply {
    let request = Request::post("/contact")
        .header(COOKIE, cookie)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body(fields)))
        .expect("request");
    send(&site.app, request).await
}

async fn submit(site: &Site, visitor: &Visitor, fields: &[(&str, &str)]) -> Reply {
    let mut all = vec![("_csrf", visitor.token.as_str())];
    all.extend_from_slice(fields);
    post(site, &visitor.cookie, &all).await
}

const GOOD: [(&str, &str); 3] = [
    ("name", "Sanne de Vries"),
    ("email", "sanne@example.org"),
    ("message", "Kan ik ook een halfje bestellen?"),
];

fn with(field: &'static str, value: &'static str) -> Vec<(&'static str, &'static str)> {
    GOOD.iter()
        .map(|&(name, old)| (name, if name == field { value } else { old }))
        .collect()
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct Queued {
    kind: String,
    to_address: String,
    reply_to: Option<String>,
}

async fn queued(pool: &PgPool) -> Vec<Queued> {
    sqlx::query_as("SELECT kind, to_address, reply_to FROM email_outbox ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("outbox")
}

fn wait(site: &Site) {
    site.clock.advance(SignedDuration::from_secs(5));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn faq_shows_the_seeded_questions(pool: PgPool) {
    let site = site(pool);
    let reply = get(&site.app, "/faq", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains(&dutch("faq-title")));
    assert!(
        reply.body.contains("Waar haal ik mijn brood op?"),
        "{}",
        reply.body
    );
    assert!(reply.body.contains("<strong>De Proef</strong>"));
    assert!(reply.body.contains("href=\"/contact\""));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn faq_shows_published_entries_in_order_with_safe_markdown(pool: PgPool) {
    sqlx::query("DELETE FROM faq_entry")
        .execute(&pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let clock = FakeClock::new("2026-12-01T09:00:00Z".parse().unwrap());
    let entry = |question: &str, answer: &str| FaqInput {
        question: question.to_owned(),
        answer_md: answer.to_owned(),
    };
    let first = faq::create(
        &mut conn,
        &clock,
        &entry("Eerste <vraag>", "**Vet** en *schuin*"),
    )
    .await
    .unwrap();
    let hidden = faq::create(&mut conn, &clock, &entry("Verborgen vraag", "Nog niet af"))
        .await
        .unwrap();
    let last = faq::create(
        &mut conn,
        &clock,
        &entry(
            "Laatste vraag",
            "<script>alert(1)</script>\n\n- [klik](javascript:alert(1))\n- [site](https://example.org/a?b=1&c=2)",
        ),
    )
    .await
    .unwrap();
    faq::set_published(&mut conn, &clock, first.id, true)
        .await
        .unwrap();
    faq::set_published(&mut conn, &clock, last.id, true)
        .await
        .unwrap();
    faq::reorder(&mut conn, &clock, &[last.id, hidden.id, first.id])
        .await
        .unwrap();

    let reply = get(&site(pool).app, "/faq", None).await;
    let html = reply.body;
    let last_at = html.find("Laatste vraag").expect("last");
    let first_at = html.find("Eerste &#60;vraag&#62;").expect("first, escaped");
    assert!(last_at < first_at, "{html}");
    assert!(!html.contains("Verborgen vraag"));
    assert!(
        html.contains("<strong>Vet</strong> en <em>schuin</em>"),
        "{html}"
    );
    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&#60;script&#62;"), "{html}");
    assert!(!html.contains("javascript:"), "{html}");
    assert!(html.contains("<li>klik)</li>"), "{html}");
    assert!(
        html.contains("<li><a href=\"https://example.org/a?b=1&#38;c=2\">site</a></li>"),
        "{html}"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn faq_without_entries_says_so(pool: PgPool) {
    sqlx::query("DELETE FROM faq_entry")
        .execute(&pool)
        .await
        .unwrap();
    let reply = get(&site(pool).app, "/faq", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains(&dutch("faq-empty-title")));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn contact_page_has_the_form_and_a_hidden_trap(pool: PgPool) {
    let site = site(pool);
    let reply = get(&site.app, "/contact", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    for id in [
        "contact-title",
        "contact-name",
        "contact-email",
        "contact-message",
        "contact-send",
    ] {
        assert!(reply.body.contains(&dutch(id)), "{id}");
    }
    assert!(
        reply
            .body
            .contains("<form class=\"contact-form\" method=\"post\" action=\"/contact\">")
    );
    assert!(
        reply
            .body
            .contains("<div class=\"field-trap\" aria-hidden=\"true\">\n<label for=\"website\">"),
        "{}",
        reply.body
    );
    assert!(
        reply
            .body
            .contains("name=\"website\" value=\"\" tabindex=\"-1\" autocomplete=\"off\"")
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_message_queues_one_email_to_the_bakery_with_reply_to(pool: PgPool) {
    let site = site(pool);
    let visitor = open_form(&site).await;
    wait(&site);
    let reply = submit(&site, &visitor, &GOOD).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains(&dutch("contact-sent-text")));
    assert_eq!(
        queued(&site.pool).await,
        [Queued {
            kind: "contact_message".to_owned(),
            to_address: BAKERY.to_owned(),
            reply_to: Some("sanne@example.org".to_owned()),
        }]
    );

    let sender = FakeEmailSender::new();
    let mut conn = site.pool.acquire().await.unwrap();
    notify::send_pending(&mut conn, &sender, site.clock.as_ref(), 10)
        .await
        .unwrap();
    let sent = sender.sent();
    assert_eq!(sent.len(), 1, "no auto-reply to the sender");
    assert!(
        sent[0]
            .email
            .text
            .contains("Kan ik ook een halfje bestellen?")
    );

    wait(&site);
    let again = submit(&site, &visitor, &GOOD).await;
    assert_eq!(
        again.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a resend needs the form again"
    );
    assert_eq!(queued(&site.pool).await.len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_filled_in_trap_looks_sent_but_sends_nothing(pool: PgPool) {
    let site = site(pool);
    let visitor = open_form(&site).await;
    wait(&site);
    let mut fields = GOOD.to_vec();
    fields.push(("website", "https://spam.example"));
    let reply = submit(&site, &visitor, &fields).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains(&dutch("contact-sent-text")));
    assert!(queued(&site.pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_submit_right_after_opening_the_form_is_refused(pool: PgPool) {
    let site = site(pool);
    let visitor = open_form(&site).await;
    site.clock.advance(SignedDuration::from_secs(2));
    let reply = submit(&site, &visitor, &GOOD).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains(&dutch("contact-error-too-fast")),
        "{}",
        reply.body
    );
    assert!(
        reply.body.contains("value=\"Sanne de Vries\""),
        "values are kept"
    );
    assert!(queued(&site.pool).await.is_empty());

    site.clock.advance(SignedDuration::from_secs(1));
    let reply = submit(&site, &visitor, &GOOD).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(queued(&site.pool).await.len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sending_needs_the_csrf_token(pool: PgPool) {
    let site = site(pool);
    let visitor = open_form(&site).await;
    wait(&site);
    let missing = post(&site, &visitor.cookie, &GOOD).await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let mut fields = vec![("_csrf", "verkeerd")];
    fields.extend_from_slice(&GOOD);
    let wrong = post(&site, &visitor.cookie, &fields).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert!(queued(&site.pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sending_is_rate_limited_per_client(pool: PgPool) {
    let site = site(pool);
    let visitor = open_form(&site).await;
    let blank = with("name", "");
    for _ in 0..SEND_BURST {
        let reply = submit(&site, &visitor, &blank).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let reply = submit(&site, &visitor, &blank).await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(reply.body.contains(&dutch("error-too-many-title")));
    assert_eq!(
        get(&site.app, "/contact", Some(&visitor.cookie))
            .await
            .status,
        StatusCode::OK
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn header_injection_and_bad_fields_are_refused_in_dutch(pool: PgPool) {
    let long_name: &'static str = "a".repeat(NAME_MAX + 1).leak();
    let long_message: &'static str = "a".repeat(MESSAGE_MAX + 1).leak();
    for (fields, error) in [
        (
            with("name", "Sanne\r\nBcc: spam@example.org"),
            "contact-error-name",
        ),
        (with("name", "Sanne\nX"), "contact-error-name"),
        (with("name", "  "), "contact-error-name"),
        (with("name", long_name), "contact-error-name-long"),
        (
            with("email", "sanne@example.org\r\nBcc: spam@example.org"),
            "contact-error-email",
        ),
        (
            with("email", "sanne@example.org\nBcc: spam@example.org"),
            "contact-error-email",
        ),
        (with("email", "sanne"), "contact-error-email"),
        (
            with("email", "Sanne <sanne@example.org>"),
            "contact-error-email",
        ),
        (with("message", " "), "contact-error-message"),
        (with("message", "hoi\u{0}"), "contact-error-message"),
        (with("message", long_message), "contact-error-message-long"),
    ] {
        let site = site(pool.clone());
        let visitor = open_form(&site).await;
        wait(&site);
        let reply = submit(&site, &visitor, &fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
        assert!(reply.body.contains(&dutch(error)), "{error}");
        assert!(reply.body.contains("aria-invalid=\"true\""), "{error}");
    }
    assert!(queued(&pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_a_bakery_address_nothing_is_queued(pool: PgPool) {
    let site = site_with(pool, None);
    let visitor = open_form(&site).await;
    wait(&site);
    let reply = submit(&site, &visitor, &GOOD).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(queued(&site.pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_main_router_serves_faq_and_contact(pool: PgPool) {
    let app = server::router(state(pool, Some(BAKERY)));
    for path in ["/faq", "/contact"] {
        assert_eq!(get(&app, path, None).await.status, StatusCode::OK, "{path}");
    }
}
