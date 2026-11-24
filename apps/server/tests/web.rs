#![forbid(unsafe_code)]

use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, ETAG, IF_NONE_MATCH,
    REFERRER_POLICY, RETRY_AFTER, SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use db::PgPool;
use i18n::Translator;
use server::config::Config;
use server::{AppState, assets, csrf, rate_limit};
use tower::ServiceExt;
use tower_sessions::Session;

fn closed_port() -> u16 {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

/// For routes that never touch the database: the pool connects on first use only.
fn unused_pool() -> PgPool {
    let url = format!("postgres://postgres@127.0.0.1:{}/postgres", closed_port());
    db::connect_lazy(&url, Duration::from_millis(300)).expect("lazy pool")
}

fn state_with(pool: PgPool, vars: &[(&str, &str)]) -> AppState {
    let config = Config::from_lookup(|key| {
        if key == "DATABASE_URL" {
            return Some("postgres://unused/db".to_string());
        }
        vars.iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| (*value).to_string())
    })
    .expect("config");
    AppState::new(pool, Translator::dutch().expect("Dutch texts"), config)
}

fn state(pool: PgPool) -> AppState {
    state_with(pool, &[])
}

fn dutch(id: &str) -> String {
    Translator::dutch()
        .expect("Dutch texts")
        .t(id, None)
        .expect("message")
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    Reply {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).expect("utf-8 body"),
    }
}

async fn get_path(app: &Router, path: &str) -> Reply {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
    )
    .await
}

fn header(reply: &Reply, name: impl axum::http::header::AsHeaderName) -> &str {
    reply
        .headers
        .get(name)
        .map(|value| value.to_str().expect("ascii header"))
        .unwrap_or_default()
}

#[tokio::test]
async fn home_renders_dutch_text_through_the_layout() {
    let app = server::router(state(unused_pool()));
    let reply = get_path(&app, "/").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, CONTENT_TYPE), "text/html; charset=utf-8");
    assert!(
        reply
            .body
            .starts_with("<!doctype html>\n<html lang=\"nl\">")
    );
    for id in [
        "app-name",
        "home-title",
        "home-intro",
        "skip-to-content",
        "tagline",
    ] {
        assert!(
            reply.body.contains(&dutch(id)),
            "{id} missing:\n{}",
            reply.body
        );
    }
    assert!(reply.body.contains("name=\"viewport\""));
    for asset in assets::ASSETS.iter() {
        assert!(
            reply.body.contains(&asset.url()),
            "{} not linked",
            asset.name
        );
    }
    assert!(!reply.body.contains("csrf-token"), "home has no form");
}

#[tokio::test]
async fn anonymous_pages_set_no_cookie() {
    let app = server::router(state(unused_pool()));
    for path in ["/", "/static/app.css", "/nope"] {
        let reply = get_path(&app, path).await;
        assert!(reply.headers.get(SET_COOKIE).is_none(), "{path}");
    }
}

#[tokio::test]
async fn static_assets_have_type_etag_and_cache_headers() {
    let app = server::router(state(unused_pool()));
    for asset in assets::ASSETS.iter() {
        let versioned = get_path(&app, &asset.url()).await;
        assert_eq!(versioned.status, StatusCode::OK, "{}", asset.name);
        assert_eq!(header(&versioned, CONTENT_TYPE), asset.content_type);
        assert_eq!(header(&versioned, CACHE_CONTROL), assets::IMMUTABLE);
        assert_eq!(header(&versioned, ETAG), format!("\"{}\"", asset.hash));
        assert_eq!(versioned.body, asset.body);

        let bare = get_path(&app, &format!("/static/{}", asset.name)).await;
        assert_eq!(bare.status, StatusCode::OK);
        assert_eq!(header(&bare, CACHE_CONTROL), assets::REVALIDATE);

        let stale = get_path(&app, &format!("/static/{}?v=0000", asset.name)).await;
        assert_eq!(header(&stale, CACHE_CONTROL), assets::REVALIDATE);

        let cached = send(
            &app,
            Request::get(asset.url())
                .header(IF_NONE_MATCH, format!("\"{}\"", asset.hash))
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
        assert!(cached.body.is_empty());
        assert_eq!(header(&cached, CACHE_CONTROL), assets::IMMUTABLE);
    }
    let css = get_path(&app, "/static/app.css").await;
    assert_eq!(css.headers[CONTENT_TYPE], "text/css; charset=utf-8");
    let js = get_path(&app, "/static/htmx.min.js").await;
    assert_eq!(js.headers[CONTENT_TYPE], "text/javascript; charset=utf-8");
}

#[tokio::test]
async fn unknown_pages_and_assets_get_the_dutch_404_page() {
    let app = server::router(state(unused_pool()));
    for path in ["/bestaat-niet", "/static/evil.js", "/static/..%2Fapp.css"] {
        let reply = get_path(&app, path).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(header(&reply, CONTENT_TYPE), "text/html; charset=utf-8");
        assert!(
            reply.body.contains(&dutch("error-not-found-title")),
            "{path}"
        );
        assert!(
            reply.body.contains(&dutch("error-not-found-text")),
            "{path}"
        );
        assert!(reply.body.contains(&dutch("error-back-home")), "{path}");
    }
}

#[tokio::test]
async fn every_response_has_the_security_headers() {
    let app = server::router(state(unused_pool()));
    for path in ["/", "/static/app.css", "/nope"] {
        let reply = get_path(&app, path).await;
        let csp = header(&reply, CONTENT_SECURITY_POLICY);
        assert!(csp.contains("default-src 'self'"), "{path}: {csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{path}: {csp}");
        assert!(!csp.contains("unsafe-inline"), "{path}: {csp}");
        assert!(!csp.contains("unsafe-eval"), "{path}: {csp}");
        assert_eq!(header(&reply, X_CONTENT_TYPE_OPTIONS), "nosniff", "{path}");
        assert_eq!(
            header(&reply, REFERRER_POLICY),
            "strict-origin-when-cross-origin",
            "{path}"
        );
    }
}

async fn issue_token(session: Session) -> String {
    csrf::token(&session).await.expect("token")
}

async fn submitted(body: String) -> String {
    format!("ontvangen: {body}")
}

fn form_app(state: AppState) -> Router {
    let routes = Router::new()
        .route("/form", get(issue_token))
        .route("/submit", post(submitted))
        .route("/htmx", post(|| async { "ok".into_response() }));
    server::with_shell(routes, state)
}

/// A fresh session: its cookie (`name=value`) and its CSRF token.
async fn new_session(app: &Router) -> (String, String) {
    let reply = get_path(app, "/form").await;
    assert_eq!(reply.status, StatusCode::OK);
    let cookie = header(&reply, SET_COOKIE)
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string();
    (cookie, reply.body)
}

fn htmx_post(cookie: Option<&str>, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::post("/htmx");
    if let Some(cookie) = cookie {
        builder = builder.header(COOKIE, cookie);
    }
    if let Some(token) = token {
        builder = builder.header(csrf::HEADER, token);
    }
    builder.body(Body::empty()).expect("request")
}

fn form_post(cookie: &str, body: String) -> Request<Body> {
    Request::post("/submit")
        .header(COOKIE, cookie)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .expect("request")
}

fn assert_forbidden(reply: &Reply, case: &str) {
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{case}");
    assert!(
        reply.body.contains(&dutch("error-forbidden-title")),
        "{case}: {}",
        reply.body
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn csrf_accepts_the_session_token_in_header_or_form(pool: PgPool) {
    let app = form_app(state(pool));
    let (cookie, token) = new_session(&app).await;
    assert_eq!(token.len(), 43, "32 random bytes in base64url: {token}");

    let reply = send(&app, htmx_post(Some(&cookie), Some(&token))).await;
    assert_eq!(reply.status, StatusCode::OK);

    let body = format!("naam=Brood&{}={token}", csrf::FIELD);
    let reply = send(&app, form_post(&cookie, body.clone())).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.body,
        format!("ontvangen: {body}"),
        "body reaches the handler"
    );

    let again = send(
        &app,
        Request::get("/form")
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(
        again.body, token,
        "the token stays the same within a session"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn csrf_rejects_missing_wrong_and_foreign_tokens(pool: PgPool) {
    let app = form_app(state(pool));
    let (cookie, token) = new_session(&app).await;
    let (other_cookie, other_token) = new_session(&app).await;
    assert_ne!(cookie, other_cookie);
    assert_ne!(token, other_token);

    let cases = [
        ("no session, no token", htmx_post(None, None)),
        ("no session, a real token", htmx_post(None, Some(&token))),
        ("session, no token", htmx_post(Some(&cookie), None)),
        (
            "session, wrong token",
            htmx_post(Some(&cookie), Some("bogus")),
        ),
        ("session, empty token", htmx_post(Some(&cookie), Some(""))),
        (
            "token from another session",
            htmx_post(Some(&cookie), Some(&other_token)),
        ),
        (
            "form without the field",
            form_post(&cookie, "naam=Brood".to_string()),
        ),
        (
            "form with a wrong field",
            form_post(&cookie, format!("{}=bogus", csrf::FIELD)),
        ),
        (
            "form with another session's token",
            form_post(&cookie, format!("{}={other_token}", csrf::FIELD)),
        ),
        (
            "token as a prefix",
            htmx_post(Some(&cookie), Some(&token[..42])),
        ),
    ];
    for (case, request) in cases {
        let reply = send(&app, request).await;
        assert_forbidden(&reply, case);
    }

    let multipart = Request::post("/submit")
        .header(COOKIE, &cookie)
        .header(CONTENT_TYPE, "multipart/form-data; boundary=x")
        .body(Body::from(format!(
            "--x\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{token}\r\n--x--\r\n",
            csrf::FIELD
        )))
        .expect("request");
    assert_forbidden(&send(&app, multipart).await, "multipart needs the header");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn session_cookie_is_http_only_lax_and_secure(pool: PgPool) {
    let app = form_app(state(pool.clone()));
    let reply = get_path(&app, "/form").await;
    let cookie = header(&reply, SET_COOKIE);
    assert!(cookie.starts_with("bvw_session="), "{cookie}");
    for flag in ["HttpOnly", "SameSite=Lax", "Secure", "Path=/"] {
        assert!(
            cookie.split("; ").any(|part| part == flag),
            "{flag}: {cookie}"
        );
    }

    let local = form_app(state_with(pool, &[("COOKIE_SECURE", "false")]));
    let reply = get_path(&local, "/form").await;
    let cookie = header(&reply, SET_COOKIE);
    assert!(!cookie.split("; ").any(|part| part == "Secure"), "{cookie}");
    assert!(
        cookie.split("; ").any(|part| part == "HttpOnly"),
        "{cookie}"
    );
}

fn from_peer(path: &str, peer: &str) -> Request<Body> {
    let mut request = Request::get(path).body(Body::empty()).expect("request");
    request.extensions_mut().insert(ConnectInfo(
        format!("{peer}:5000").parse::<SocketAddr>().expect("addr"),
    ));
    request
}

#[tokio::test]
async fn over_the_rate_limit_gives_the_dutch_429_page() {
    let state = state(unused_pool());
    let limited = rate_limit::apply(
        Router::new().route("/probe", get(|| async { "ok" })),
        &state,
        Duration::from_secs(3600),
        2,
    );
    let app = server::with_shell(limited, state);

    for _ in 0..2 {
        let reply = send(&app, from_peer("/probe", "192.0.2.1")).await;
        assert_eq!(reply.status, StatusCode::OK);
    }
    let reply = send(&app, from_peer("/probe", "192.0.2.1")).await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header(&reply, CONTENT_TYPE), "text/html; charset=utf-8");
    assert!(
        reply.body.contains(&dutch("error-too-many-title")),
        "{}",
        reply.body
    );
    assert!(reply.body.contains(&dutch("error-too-many-text")));
    let wait: u64 = header(&reply, RETRY_AFTER)
        .parse()
        .expect("Retry-After seconds");
    assert!(wait > 0);
    assert!(header(&reply, CONTENT_SECURITY_POLICY).contains("default-src 'self'"));

    let other = send(&app, from_peer("/probe", "192.0.2.2")).await;
    assert_eq!(other.status, StatusCode::OK, "limits are per client");
}

#[tokio::test]
async fn rate_limit_trusts_forwarded_for_only_from_the_proxy() {
    let state = state_with(unused_pool(), &[("TRUSTED_PROXY", "10.89.0.10")]);
    let limited = rate_limit::apply(
        Router::new().route("/probe", get(|| async { "ok" })),
        &state,
        Duration::from_secs(3600),
        1,
    );
    let app = server::with_shell(limited, state);
    let via_proxy = |client: &str| {
        let mut request = from_peer("/probe", "10.89.0.10");
        request
            .headers_mut()
            .insert("x-forwarded-for", client.parse().expect("header"));
        request
    };

    assert_eq!(
        send(&app, via_proxy("198.51.100.1")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, via_proxy("198.51.100.2")).await.status,
        StatusCode::OK,
        "another client behind the proxy has its own quota"
    );
    assert_eq!(
        send(&app, via_proxy("198.51.100.1")).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        send(&app, via_proxy("6.6.6.6, 198.51.100.1")).await.status,
        StatusCode::TOO_MANY_REQUESTS,
        "a spoofed first entry does not reset the quota"
    );
}
