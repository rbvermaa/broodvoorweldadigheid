#![forbid(unsafe_code)]

mod support;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use auth::{NewStaff, Permission, Role, StaffUser};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::header::{CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::get;
use db::PgPool;
use i18n::Translator;
use server::config::Config;
use server::staff::{Forbidden, Staff};
use server::{AppState, csrf, staff};
use support::{signed_in, signed_in_at};
use testkit::FakeClock;
use tokio::sync::{Notify, oneshot};
use tower::ServiceExt;

const LOCAL_PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5000);

const DEV_LOGIN: &[(&str, &str)] = &[("BIND_ADDR", "127.0.0.1:8080"), ("DEV_STAFF_LOGIN", "true")];

fn state(pool: PgPool, vars: &[(&str, &str)]) -> AppState {
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

fn app(pool: PgPool, vars: &[(&str, &str)]) -> Router {
    server::router(state(pool, vars))
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

impl Reply {
    fn header(&self, name: impl axum::http::header::AsHeaderName) -> &str {
        self.headers
            .get(name)
            .map(|value| value.to_str().expect("ascii"))
            .unwrap_or_default()
    }

    /// The `name=value` part of the Set-Cookie header, if any.
    fn cookie(&self) -> Option<String> {
        self.headers.get(SET_COOKIE).map(|value| {
            value
                .to_str()
                .expect("ascii")
                .split(';')
                .next()
                .expect("pair")
                .to_string()
        })
    }
}

async fn send(app: &Router, mut request: Request<Body>) -> Reply {
    if request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_none()
    {
        request.extensions_mut().insert(ConnectInfo(LOCAL_PEER));
    }
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    Reply {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).expect("utf-8"),
    }
}

async fn get_with(app: &Router, path: &str, cookie: Option<&str>) -> Reply {
    let mut builder = Request::get(path);
    if let Some(cookie) = cookie {
        builder = builder.header(COOKIE, cookie);
    }
    send(app, builder.body(Body::empty()).expect("request")).await
}

async fn post_form(app: &Router, path: &str, cookie: Option<&str>, body: &str) -> Reply {
    let mut builder = Request::post(path).header(CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        builder = builder.header(COOKIE, cookie);
    }
    send(
        app,
        builder.body(Body::from(body.to_string())).expect("request"),
    )
    .await
}

async fn add(pool: &PgPool, sub: &str, name: &str, role: Role) -> StaffUser {
    auth::staff::add(
        pool,
        &NewStaff {
            oidc_sub: sub.to_string(),
            name: name.to_string(),
            email: None,
            role,
        },
    )
    .await
    .expect("add staff")
}

async fn deactivate(pool: &PgPool, staff: &StaffUser) {
    sqlx::query("UPDATE staff_user SET active = false WHERE id = $1")
        .bind(staff.id.get())
        .execute(pool)
        .await
        .expect("deactivate");
}

fn assert_page(reply: &Reply, status: StatusCode, title: &str, text: &str) {
    assert_eq!(reply.status, status, "{}", reply.body);
    assert_eq!(reply.header(CONTENT_TYPE), "text/html; charset=utf-8");
    assert!(
        reply.body.contains(&dutch(title)),
        "{title}: {}",
        reply.body
    );
    assert!(reply.body.contains(&dutch(text)), "{text}: {}", reply.body);
}

/// The CSRF token in the page's meta tag.
fn token_in(page: &str) -> String {
    page.split("name=\"csrf-token\" content=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("csrf meta tag")
        .to_string()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_staff_page_without_a_session_redirects_to_login(pool: PgPool) {
    let reply = get_with(&app(pool, &[]), "/staff", None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(LOCATION), staff::LOGIN_PATH);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_signed_in_staff_member_sees_the_staff_page(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bakker Bas", Role::Baker).await;
    let (cookie, _) = signed_in(&pool, &baker).await;
    let page = get_with(&app(pool, &[]), "/staff", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("Bakker Bas"), "{}", page.body);
    assert!(page.body.contains(&dutch("staff-role-baker")));
    assert!(page.body.contains(&dutch("staff-logout")));
    assert!(page.body.contains(&format!("name=\"{}\"", csrf::FIELD)));
}

async fn manage_staff(
    State(state): State<AppState>,
    staff: Staff,
) -> Result<&'static str, Forbidden> {
    staff.require(&state, Permission::ManageStaff)?;
    Ok("beheer")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_role_without_the_permission_gets_the_dutch_403(pool: PgPool) {
    let counter = add(&pool, "uuid-counter", "Toonbank Tineke", Role::Counter).await;
    let owner = add(&pool, "uuid-owner", "Eigenaar", Role::Owner).await;
    let routes = Router::new().route("/staff/medewerkers", get(manage_staff));
    let app = server::with_shell(routes, state(pool.clone(), &[]));

    let (cookie, _) = signed_in(&pool, &counter).await;
    let reply = get_with(&app, "/staff/medewerkers", Some(&cookie)).await;
    assert_page(
        &reply,
        StatusCode::FORBIDDEN,
        "staff-forbidden-title",
        "staff-forbidden-text",
    );

    let (cookie, _) = signed_in(&pool, &owner).await;
    let reply = get_with(&app, "/staff/medewerkers", Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, "beheer");

    let reply = get_with(&app, "/staff/medewerkers", None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(LOCATION), staff::LOGIN_PATH);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_deactivated_staff_member_loses_the_session(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let (cookie, _) = signed_in(&pool, &baker).await;
    let app = app(pool.clone(), &[]);
    assert_eq!(
        get_with(&app, "/staff", Some(&cookie)).await.status,
        StatusCode::OK
    );
    deactivate(&pool, &baker).await;
    let reply = get_with(&app, "/staff", Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(LOCATION), staff::LOGIN_PATH);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn logout_needs_the_csrf_token_and_ends_the_session(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let (cookie, token) = signed_in(&pool, &baker).await;
    let app = app(pool, &[]);

    let without = post_form(&app, "/staff/logout", Some(&cookie), "").await;
    assert_eq!(without.status, StatusCode::FORBIDDEN);
    let wrong = post_form(
        &app,
        "/staff/logout",
        Some(&cookie),
        &format!("{}=wrong", csrf::FIELD),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert_eq!(
        get_with(&app, "/staff", Some(&cookie)).await.status,
        StatusCode::OK,
        "still signed in"
    );

    let with = post_form(
        &app,
        "/staff/logout",
        Some(&cookie),
        &format!("{}={token}", csrf::FIELD),
    )
    .await;
    assert_eq!(with.status, StatusCode::SEE_OTHER);
    assert_eq!(with.header(LOCATION), "/");
    let after = get_with(&app, "/staff", Some(&cookie)).await;
    assert_eq!(after.status, StatusCode::SEE_OTHER);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_dev_login_there_is_no_way_in(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let app = app(pool.clone(), &[]);
    let reply = get_with(&app, staff::LOGIN_PATH, None).await;
    assert_page(
        &reply,
        StatusCode::SERVICE_UNAVAILABLE,
        "staff-unavailable-title",
        "staff-unavailable-text",
    );

    let other = add(&pool, "uuid-other", "Ander", Role::Counter).await;
    let (cookie, token) = signed_in(&pool, &other).await;
    let reply = post_form(
        &app,
        staff::DEV_LOGIN_PATH,
        Some(&cookie),
        &format!("{}={token}&staff_id={}", csrf::FIELD, baker.id.get()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    assert!(reply.cookie().is_none_or(|after| after == cookie));
}

/// Opens the dev login page; the session cookie and the form's CSRF token.
async fn dev_login_page(app: &Router) -> (String, String) {
    let reply = get_with(app, staff::LOGIN_PATH, None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains(&dutch("staff-dev-login-title")));
    assert!(reply.body.contains("name=\"staff_id\""));
    let cookie = reply.cookie().expect("the page starts a session");
    (cookie, token_in(&reply.body))
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn dev_login_signs_in_with_a_new_session_id(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bakker Bas", Role::Baker).await;
    let app = app(pool, DEV_LOGIN);
    let (before, token) = dev_login_page(&app).await;

    let reply = post_form(
        &app,
        staff::DEV_LOGIN_PATH,
        Some(&before),
        &format!("{}={token}&staff_id={}", csrf::FIELD, baker.id.get()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(reply.header(LOCATION), "/staff");
    let after = reply.cookie().expect("new cookie");
    assert_ne!(after, before, "the session id changes on sign-in");

    let page = get_with(&app, "/staff", Some(&after)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("Bakker Bas"));
    assert_ne!(token_in(&page.body), token, "the old CSRF token is gone");

    let old = get_with(&app, "/staff", Some(&before)).await;
    assert_eq!(
        old.status,
        StatusCode::SEE_OTHER,
        "the old session id is gone"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn dev_login_needs_the_csrf_token(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let app = app(pool, DEV_LOGIN);
    let (cookie, _) = dev_login_page(&app).await;
    let reply = post_form(
        &app,
        staff::DEV_LOGIN_PATH,
        Some(&cookie),
        &format!("staff_id={}", baker.id.get()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(
        get_with(&app, "/staff", Some(&cookie)).await.status,
        StatusCode::SEE_OTHER
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn dev_login_refuses_unknown_and_inactive_staff(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    deactivate(&pool, &baker).await;
    let app = app(pool, DEV_LOGIN);
    for staff_id in [
        baker.id.get().to_string(),
        "999999".to_string(),
        "bas".to_string(),
    ] {
        let (cookie, token) = dev_login_page(&app).await;
        let reply = post_form(
            &app,
            staff::DEV_LOGIN_PATH,
            Some(&cookie),
            &format!("{}={token}&staff_id={staff_id}", csrf::FIELD),
        )
        .await;
        assert_page(
            &reply,
            StatusCode::FORBIDDEN,
            "staff-not-allowed-title",
            "staff-not-allowed-text",
        );
        assert_eq!(
            get_with(&app, "/staff", Some(&cookie)).await.status,
            StatusCode::SEE_OTHER,
            "{staff_id}"
        );
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn dev_login_refuses_requests_from_elsewhere_or_through_a_proxy(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let app = app(pool, DEV_LOGIN);
    let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 5000);

    let mut request = Request::get(staff::LOGIN_PATH)
        .body(Body::empty())
        .expect("request");
    request.extensions_mut().insert(ConnectInfo(remote));
    assert_eq!(
        send(&app, request).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    for header in ["x-forwarded-for", "forwarded", "x-real-ip"] {
        let request = Request::get(staff::LOGIN_PATH)
            .header(header, "192.0.2.10")
            .body(Body::empty())
            .expect("request");
        assert_eq!(
            send(&app, request).await.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{header}"
        );
    }

    let (cookie, token) = dev_login_page(&app).await;
    let body = format!("{}={token}&staff_id={}", csrf::FIELD, baker.id.get());
    let mut request = Request::post(staff::DEV_LOGIN_PATH)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(COOKIE, &cookie)
        .body(Body::from(body.clone()))
        .expect("request");
    request.extensions_mut().insert(ConnectInfo(remote));
    assert_eq!(send(&app, request).await.status, StatusCode::NOT_FOUND);
    let request = Request::post(staff::DEV_LOGIN_PATH)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(COOKIE, &cookie)
        .header("x-forwarded-for", "192.0.2.10")
        .body(Body::from(body))
        .expect("request");
    assert_eq!(send(&app, request).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        get_with(&app, "/staff", Some(&cookie)).await.status,
        StatusCode::SEE_OTHER
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn htmx_requests_without_a_login_get_hx_redirect(pool: PgPool) {
    let request = Request::get("/staff")
        .header("hx-request", "true")
        .body(Body::empty())
        .expect("request");
    let reply = send(&app(pool, &[]), request).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(reply.header("hx-redirect"), staff::LOGIN_PATH);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_role_change_applies_on_the_next_request(pool: PgPool) {
    let counter = add(&pool, "uuid-counter", "Tineke", Role::Counter).await;
    add(&pool, "uuid-owner", "Eigenaar", Role::Owner).await;
    let routes = Router::new().route("/staff/medewerkers", get(manage_staff));
    let app = server::with_shell(routes, state(pool.clone(), &[]));
    let (cookie, _) = signed_in(&pool, &counter).await;
    assert_eq!(
        get_with(&app, "/staff/medewerkers", Some(&cookie))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    auth::staff::set_role(&pool, counter.id, Role::Owner)
        .await
        .expect("promote");
    assert_eq!(
        get_with(&app, "/staff/medewerkers", Some(&cookie))
            .await
            .status,
        StatusCode::OK
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_idle_login_ends_after_twelve_hours(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let start: jiff::Timestamp = "2026-11-24T09:00:00Z".parse().expect("timestamp");
    let clock = Arc::new(FakeClock::new(start));
    let app = server::router(state(pool.clone(), &[]).with_clock(clock.clone()));
    let (cookie, _) = signed_in_at(&pool, &baker, start).await;

    clock.advance(jiff::SignedDuration::from_hours(11));
    assert_eq!(
        get_with(&app, "/staff", Some(&cookie)).await.status,
        StatusCode::OK
    );
    clock.advance(jiff::SignedDuration::from_hours(12));
    let reply = get_with(&app, "/staff", Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(LOCATION), staff::LOGIN_PATH);
}

/// Holds a staff request open until the test lets it go.
#[derive(Clone)]
struct Gate {
    entered: Arc<std::sync::Mutex<Option<oneshot::Sender<()>>>>,
    release: Arc<Notify>,
}

async fn held(axum::Extension(gate): axum::Extension<Gate>, _staff: Staff) -> &'static str {
    let entered = gate
        .entered
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(entered) = entered {
        let _ = entered.send(());
    }
    gate.release.notified().await;
    "klaar"
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_request_in_flight_cannot_undo_logout(pool: PgPool) {
    let baker = add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let (cookie, token) = signed_in(&pool, &baker).await;
    let (entered_tx, entered_rx) = oneshot::channel();
    let gate = Gate {
        entered: Arc::new(std::sync::Mutex::new(Some(entered_tx))),
        release: Arc::new(Notify::new()),
    };
    let state = state(pool, &[]);
    let routes = Router::new()
        .route("/staff", get(staff::home))
        .route("/staff/logout", axum::routing::post(staff::logout))
        .route("/staff/held", get(held))
        .layer(axum::Extension(gate.clone()));
    let app = server::with_shell(routes, state);

    let in_flight = tokio::spawn({
        let app = app.clone();
        let cookie = cookie.clone();
        async move { get_with(&app, "/staff/held", Some(&cookie)).await }
    });
    entered_rx
        .await
        .expect("the held request is in its handler");

    let logout = post_form(
        &app,
        "/staff/logout",
        Some(&cookie),
        &format!("{}={token}", csrf::FIELD),
    )
    .await;
    assert_eq!(logout.status, StatusCode::SEE_OTHER);

    gate.release.notify_one();
    let held = in_flight.await.expect("held request");
    assert_eq!(held.status, StatusCode::OK, "{}", held.body);

    let after = get_with(&app, "/staff", Some(&cookie)).await;
    assert_eq!(after.status, StatusCode::SEE_OTHER, "logout stays done");
}
