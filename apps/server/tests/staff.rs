#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use auth::{NewStaff, Permission, Role};
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
use server::staff_login::StaffLogin;
use server::{AppState, csrf, staff};
use testkit::{FakeClock, FakeOidc, FakeUser, Sabotage};
use tower::ServiceExt;

const REDIRECT: &str = "https://shop.test/staff/oidc/callback";

fn now() -> jiff::Timestamp {
    "2026-11-24T09:00:00Z".parse().expect("timestamp")
}

struct SecretFile(PathBuf);

impl SecretFile {
    fn new(contents: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "server-staff-secret-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::write(&path, contents).expect("write secret");
        Self(path)
    }
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

struct Harness {
    app: Router,
    fake: FakeOidc,
    _secret: SecretFile,
}

fn config(vars: &[(&str, &str)]) -> Config {
    Config::from_lookup(|key| {
        if key == "DATABASE_URL" {
            return Some("postgres://unused/db".to_string());
        }
        vars.iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| (*value).to_string())
    })
    .expect("config")
}

fn state_with(
    pool: PgPool,
    vars: &[(&str, &str)],
    secret: &SecretFile,
    fake: &FakeOidc,
) -> AppState {
    let secret_path = secret.0.to_string_lossy().into_owned();
    let mut all = vec![
        ("OIDC_ISSUER", testkit::oidc::ISSUER),
        ("OIDC_CLIENT_ID", testkit::oidc::CLIENT_ID),
        ("OIDC_REDIRECT_URL", REDIRECT),
        ("OIDC_CLIENT_SECRET_FILE", secret_path.as_str()),
    ];
    all.extend_from_slice(vars);
    let config = config(&all);
    let oidc = config.oidc.clone().expect("oidc config");
    AppState::new(pool, Translator::dutch().expect("Dutch texts"), config)
        .with_clock(Arc::new(FakeClock::new(now())))
        .with_staff_login(StaffLogin::new(oidc, Arc::new(fake.clone())))
}

fn harness(pool: PgPool, vars: &[(&str, &str)]) -> Harness {
    let fake = FakeOidc::new(now());
    let secret = SecretFile::new(testkit::oidc::CLIENT_SECRET);
    let app = server::router(state_with(pool, vars, &secret, &fake));
    Harness {
        app,
        fake,
        _secret: secret,
    }
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
    request.extensions_mut().insert(ConnectInfo(
        "192.0.2.10:5000".parse::<SocketAddr>().expect("addr"),
    ));
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

/// Starts a login: the session cookie and the provider URL the browser is sent to.
async fn begin_login(app: &Router) -> (String, String) {
    let reply = get_with(app, staff::LOGIN_PATH, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let cookie = reply.cookie().expect("login starts a session");
    (cookie, reply.header(LOCATION).to_string())
}

/// Logs `user` in; the session cookie after login.
async fn log_in(harness: &Harness, user: FakeUser) -> String {
    let (cookie, url) = begin_login(&harness.app).await;
    let callback = harness.fake.authorize(&url, user);
    let reply = get_with(&harness.app, &callback.path_and_query(), Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(reply.header(LOCATION), "/staff");
    reply.cookie().expect("login sets a new cookie")
}

async fn add(pool: &PgPool, sub: &str, name: &str, role: Role) -> auth::StaffUser {
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

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_staff_page_without_a_session_redirects_to_login(pool: PgPool) {
    let harness = harness(pool, &[]);
    let reply = get_with(&harness.app, "/staff", None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(LOCATION), staff::LOGIN_PATH);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn login_redirects_to_the_provider_with_pkce(pool: PgPool) {
    let harness = harness(pool, &[]);
    let (_, url) = begin_login(&harness.app).await;
    assert!(
        url.starts_with(&format!("{}/authorize?", testkit::oidc::BASE)),
        "{url}"
    );
    for param in [
        "code_challenge_method=S256",
        "code_challenge=",
        "state=",
        "nonce=",
        "response_type=code",
    ] {
        assert!(url.contains(param), "{param}: {url}");
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_staff_member_logs_in_with_a_new_session_id(pool: PgPool) {
    add(&pool, "uuid-baker", "Bakker Bas", Role::Baker).await;
    let harness = harness(pool, &[]);
    let (before, url) = begin_login(&harness.app).await;
    let callback = harness
        .fake
        .authorize(&url, FakeUser::new("uuid-baker", "Bas"));
    let reply = get_with(&harness.app, &callback.path_and_query(), Some(&before)).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let after = reply.cookie().expect("new cookie");
    assert_ne!(after, before, "the session id changes on login");

    let page = get_with(&harness.app, "/staff", Some(&after)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("Bas"), "{}", page.body);
    assert!(page.body.contains(&dutch("staff-role-baker")));
    assert!(page.body.contains(&dutch("staff-logout")));
    assert!(page.body.contains(&format!("name=\"{}\"", csrf::FIELD)));

    let old = get_with(&harness.app, "/staff", Some(&before)).await;
    assert_eq!(
        old.status,
        StatusCode::SEE_OTHER,
        "the old session id is gone"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_callback_with_a_wrong_state_is_refused(pool: PgPool) {
    add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let harness = harness(pool, &[]);
    let (cookie, url) = begin_login(&harness.app).await;
    let mut callback = harness
        .fake
        .authorize(&url, FakeUser::new("uuid-baker", "Bas"));
    callback.state = "forged".to_string();
    let reply = get_with(&harness.app, &callback.path_and_query(), Some(&cookie)).await;
    assert_page(
        &reply,
        StatusCode::BAD_REQUEST,
        "staff-login-failed-title",
        "staff-login-failed-text",
    );
    assert!(
        !harness.fake.requests().iter().any(|r| r.contains("/token")),
        "no code exchange"
    );

    let retry = get_with(&harness.app, &callback.path_and_query(), Some(&cookie)).await;
    assert_eq!(
        retry.status,
        StatusCode::BAD_REQUEST,
        "a pending login is used once"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_callback_without_a_login_or_with_an_error_is_refused(pool: PgPool) {
    let harness = harness(pool, &[]);
    let reply = get_with(&harness.app, "/staff/oidc/callback?code=x&state=y", None).await;
    assert_page(
        &reply,
        StatusCode::BAD_REQUEST,
        "staff-login-failed-title",
        "staff-login-failed-text",
    );

    let (cookie, _) = begin_login(&harness.app).await;
    let reply = get_with(
        &harness.app,
        "/staff/oidc/callback?error=access_denied",
        Some(&cookie),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);

    let (cookie, _) = begin_login(&harness.app).await;
    let reply = get_with(&harness.app, "/staff/oidc/callback?state=y", Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_forged_id_token_is_refused(pool: PgPool) {
    add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let harness = harness(pool, &[]);
    let (cookie, url) = begin_login(&harness.app).await;
    let callback = harness
        .fake
        .authorize(&url, FakeUser::new("uuid-baker", "Bas"));
    harness.fake.sabotage(Sabotage::ForeignSignature);
    let reply = get_with(&harness.app, &callback.path_and_query(), Some(&cookie)).await;
    assert_page(
        &reply,
        StatusCode::FORBIDDEN,
        "staff-login-failed-title",
        "staff-login-failed-text",
    );
    assert!(reply.cookie().is_none_or(|after| after == cookie));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn strangers_are_refused_and_not_added(pool: PgPool) {
    let harness = harness(pool.clone(), &[]);
    let (cookie, url) = begin_login(&harness.app).await;
    let callback = harness
        .fake
        .authorize(&url, FakeUser::new("stranger", "Onbekend"));
    let reply = get_with(&harness.app, &callback.path_and_query(), Some(&cookie)).await;
    assert_page(
        &reply,
        StatusCode::FORBIDDEN,
        "staff-not-allowed-title",
        "staff-not-allowed-text",
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM staff_user")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 0, "no self-signup");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_bootstrap_owner_from_the_environment_becomes_owner(pool: PgPool) {
    let harness = harness(
        pool,
        &[("BVW_BOOTSTRAP_OWNER_EMAIL", "eigenaar@example.org")],
    );
    let cookie = log_in(
        &harness,
        FakeUser::new("uuid-owner", "Eigenaar").with_email("eigenaar@example.org", true),
    )
    .await;
    let page = get_with(&harness.app, "/staff", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains(&dutch("staff-role-owner")),
        "{}",
        page.body
    );
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
    add(&pool, "uuid-counter", "Toonbank Tineke", Role::Counter).await;
    add(&pool, "uuid-owner", "Eigenaar", Role::Owner).await;
    let fake = FakeOidc::new(now());
    let secret = SecretFile::new(testkit::oidc::CLIENT_SECRET);
    let state = state_with(pool, &[], &secret, &fake);
    let routes = Router::new()
        .route(staff::LOGIN_PATH, get(staff::login))
        .route("/staff/oidc/callback", get(staff::callback))
        .route("/staff/medewerkers", get(manage_staff));
    let harness = Harness {
        app: server::with_shell(routes, state),
        fake,
        _secret: secret,
    };

    let counter = log_in(&harness, FakeUser::new("uuid-counter", "Tineke")).await;
    let reply = get_with(&harness.app, "/staff/medewerkers", Some(&counter)).await;
    assert_page(
        &reply,
        StatusCode::FORBIDDEN,
        "staff-forbidden-title",
        "staff-forbidden-text",
    );

    let owner = log_in(&harness, FakeUser::new("uuid-owner", "Eigenaar")).await;
    let reply = get_with(&harness.app, "/staff/medewerkers", Some(&owner)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, "beheer");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_deactivated_staff_member_loses_the_session(pool: PgPool) {
    let added = add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let harness = harness(pool.clone(), &[]);
    let cookie = log_in(&harness, FakeUser::new("uuid-baker", "Bas")).await;
    sqlx::query("UPDATE staff_user SET active = false WHERE id = $1")
        .bind(added.id.get())
        .execute(&pool)
        .await
        .expect("deactivate");
    let reply = get_with(&harness.app, "/staff", Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(LOCATION), staff::LOGIN_PATH);
}

/// The CSRF token on the staff page.
fn token_in(page: &str) -> String {
    page.split("name=\"csrf-token\" content=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("csrf meta tag")
        .to_string()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn logout_needs_the_csrf_token_and_ends_the_session(pool: PgPool) {
    add(&pool, "uuid-baker", "Bas", Role::Baker).await;
    let harness = harness(pool, &[]);
    let cookie = log_in(&harness, FakeUser::new("uuid-baker", "Bas")).await;
    let page = get_with(&harness.app, "/staff", Some(&cookie)).await;
    let token = token_in(&page.body);

    let without = send(
        &harness.app,
        Request::post("/staff/logout")
            .header(COOKIE, &cookie)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(without.status, StatusCode::FORBIDDEN);
    assert_eq!(
        get_with(&harness.app, "/staff", Some(&cookie)).await.status,
        StatusCode::OK,
        "still logged in"
    );

    let with = send(
        &harness.app,
        Request::post("/staff/logout")
            .header(COOKIE, &cookie)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!("{}={token}", csrf::FIELD)))
            .expect("request"),
    )
    .await;
    assert_eq!(with.status, StatusCode::SEE_OTHER);
    assert_eq!(with.header(LOCATION), "/");
    let after = get_with(&harness.app, "/staff", Some(&cookie)).await;
    assert_eq!(after.status, StatusCode::SEE_OTHER);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn login_answers_503_until_it_is_set_up(pool: PgPool) {
    let unconfigured = server::router(AppState::new(
        pool.clone(),
        Translator::dutch().expect("Dutch texts"),
        config(&[]),
    ));
    let reply = get_with(&unconfigured, staff::LOGIN_PATH, None).await;
    assert_page(
        &reply,
        StatusCode::SERVICE_UNAVAILABLE,
        "staff-unavailable-title",
        "staff-unavailable-text",
    );

    let fake = FakeOidc::new(now());
    let empty_secret = SecretFile::new("\n");
    let app = server::router(state_with(pool, &[], &empty_secret, &fake));
    let reply = get_with(&app, staff::LOGIN_PATH, None).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(fake.requests().is_empty(), "no discovery without a secret");

    std::fs::write(&empty_secret.0, testkit::oidc::CLIENT_SECRET).expect("write secret");
    let reply = get_with(&app, staff::LOGIN_PATH, None).await;
    assert_eq!(
        reply.status,
        StatusCode::SEE_OTHER,
        "picked up without a restart"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn login_is_rate_limited(pool: PgPool) {
    let harness = harness(pool, &[]);
    let mut statuses = Vec::new();
    for _ in 0..11 {
        statuses.push(get_with(&harness.app, staff::LOGIN_PATH, None).await.status);
    }
    assert!(
        statuses[..10].iter().all(|s| *s == StatusCode::SEE_OTHER),
        "{statuses:?}"
    );
    assert_eq!(statuses[10], StatusCode::TOO_MANY_REQUESTS);
}
