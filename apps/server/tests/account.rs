//! Customer magic links over HTTP (spec, Security; ADR 0029): asking for a link, signing
//! in with it, the order overview and signing out.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use auth::customer::LINK_VALID;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{Request, StatusCode};
use bakedays::{NewBakeDay, Schedule, days};
use db::PgPool;
use domain::{PickupLocationId, ProductId};
use i18n::Translator;
use jiff::{SignedDuration, Timestamp};
use orders::{Cart, CustomerInput, OrderedBy};
use server::AppState;
use server::config::Config;
use testkit::FakeClock;
use tower::ServiceExt;

const ORIGIN: &str = "https://broodvoorweldadigheid.nl";
const KNOWN: &str = "klant@example.nl";

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn dutch(id: &str) -> String {
    Translator::dutch().unwrap().t(id, None).unwrap()
}

struct App {
    router: Router,
    clock: Arc<FakeClock>,
}

async fn app(pool: &PgPool) -> App {
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
    let clock = Arc::new(FakeClock::new(ts("2026-10-28T12:00:00Z")));
    let router = server::router(
        AppState::new(pool.clone(), Translator::dutch().unwrap(), config).with_clock(clock.clone()),
    );
    App { router, clock }
}

async fn customer(pool: &PgPool, email: &str) {
    sqlx::query("INSERT INTO customer (email, name, created_at) VALUES ($1, 'Sanne', now())")
        .bind(email)
        .execute(pool)
        .await
        .unwrap();
}

struct Reply {
    status: StatusCode,
    cookie: Option<String>,
    location: Option<String>,
    body: String,
}

async fn send(app: &App, ip: &str, mut request: Request<Body>) -> Reply {
    request.extensions_mut().insert(ConnectInfo(
        format!("{ip}:4000").parse::<SocketAddr>().unwrap(),
    ));
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let header = |name| {
        response
            .headers()
            .get(name)
            .map(|v: &axum::http::HeaderValue| v.to_str().unwrap().to_owned())
    };
    let cookie = header(SET_COOKIE).map(|v| v.split(';').next().unwrap().to_owned());
    let location = header(LOCATION);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    Reply {
        status,
        cookie,
        location,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn get(app: &App, path: &str, cookie: Option<&str>) -> Reply {
    let mut request = Request::get(path);
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    send(app, "192.0.2.7", request.body(Body::empty()).unwrap()).await
}

async fn post_from(app: &App, ip: &str, path: &str, cookie: Option<&str>, body: &str) -> Reply {
    let mut request = Request::post(path).header(CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    send(app, ip, request.body(Body::from(body.to_owned())).unwrap()).await
}

async fn post(app: &App, path: &str, cookie: Option<&str>, body: &str) -> Reply {
    post_from(app, "192.0.2.7", path, cookie, body).await
}

fn csrf_of(body: &str) -> String {
    let marker = "name=\"_csrf\" value=\"";
    let start = body.find(marker).expect("csrf field") + marker.len();
    let end = start + body[start..].find('"').unwrap();
    body[start..end].to_owned()
}

/// A session cookie and its CSRF token, from `path`.
async fn session_at(app: &App, path: &str) -> (String, String) {
    let page = get(app, path, None).await;
    assert_eq!(page.status, StatusCode::OK, "{path}");
    (page.cookie.expect("session"), csrf_of(&page.body))
}

async fn ask_from(app: &App, ip: &str, email: &str) -> Reply {
    let (cookie, token) = session_at(app, "/inloggen").await;
    let body = format!("_csrf={token}&email={}", email.replace('@', "%40"));
    post_from(app, ip, "/inloggen", Some(&cookie), &body).await
}

async fn ask(app: &App, email: &str) -> Reply {
    ask_from(app, "192.0.2.7", email).await
}

async fn login_mails(pool: &PgPool) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT to_address, text_body FROM email_outbox WHERE kind = 'login_link' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn last_link(pool: &PgPool) -> String {
    let (_, text) = login_mails(pool).await.pop().expect("a login mail");
    let start = text.find(&format!("{ORIGIN}/inloggen/")).expect("link") + ORIGIN.len();
    text[start..start + "/inloggen/".len() + 43].to_owned()
}

/// Opens the link's page and presses its button; the reply to the POST.
async fn follow(app: &App, link: &str) -> (Reply, String) {
    let (cookie, token) = session_at(app, link).await;
    let reply = post(app, link, Some(&cookie), &format!("_csrf={token}")).await;
    (reply, cookie)
}

async fn signed_in(app: &App, pool: &PgPool) -> String {
    ask(app, KNOWN).await;
    let (reply, _) = follow(app, &last_link(pool).await).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    reply.cookie.expect("new session")
}

async fn logins(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM customer_login")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_login_page_asks_for_an_email_with_a_csrf_token(pool: PgPool) {
    let app = app(&pool).await;
    let page = get(&app, "/inloggen", None).await;
    assert_eq!(page.status, StatusCode::OK);
    for id in [
        "account-login-title",
        "account-email",
        "account-login-submit",
    ] {
        assert!(page.body.contains(&dutch(id)), "{id}");
    }
    assert!(page.body.contains("name=\"_csrf\""));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn asking_for_a_link_needs_the_csrf_token(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let reply = post(&app, "/inloggen", None, "email=klant%40example.nl").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(login_mails(&pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn every_address_gets_the_same_answer_and_only_a_customer_gets_mail(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let known = ask(&app, "Klant@Example.nl").await;
    assert_eq!(known.status, StatusCode::OK);
    assert!(known.body.contains(&dutch("account-check-title")));
    let unknown = ask(&app, "onbekend@example.nl").await;
    assert_eq!(unknown.status, known.status);
    assert_eq!(unknown.body, known.body);

    let mails = login_mails(&pool).await;
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].0, KNOWN);
    assert!(mails[0].1.contains(&format!("{ORIGIN}/inloggen/")));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_customer_gets_a_limited_number_of_links_without_noticing(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let first = ask_from(&app, "192.0.2.1", KNOWN).await;
    for n in 2..=5 {
        let reply = ask_from(&app, &format!("192.0.2.{n}"), KNOWN).await;
        assert_eq!(reply.status, first.status);
        assert_eq!(reply.body, first.body);
    }
    assert_eq!(
        login_mails(&pool).await.len(),
        usize::try_from(auth::customer::LINKS_PER_WINDOW).unwrap()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn asking_for_links_is_rate_limited_per_client(pool: PgPool) {
    let app = app(&pool).await;
    let (cookie, token) = session_at(&app, "/inloggen").await;
    let body = format!("_csrf={token}&email=iemand%40example.nl");
    for _ in 0..server::account::LOGIN_BURST {
        let reply = post(&app, "/inloggen", Some(&cookie), &body).await;
        assert_eq!(reply.status, StatusCode::OK);
    }
    let limited = post(&app, "/inloggen", Some(&cookie), &body).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    let other = post_from(&app, "192.0.2.99", "/inloggen", Some(&cookie), &body).await;
    assert_eq!(other.status, StatusCode::OK);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_bad_address_shows_the_form_again(pool: PgPool) {
    let app = app(&pool).await;
    let reply = ask(&app, "geen-adres").await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains(&dutch("account-email-error")));
    assert!(reply.body.contains("value=\"geen-adres\""));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_link_signs_in_once_on_a_new_session(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    ask(&app, KNOWN).await;
    let link = last_link(&pool).await;

    let page = get(&app, &link, None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(&dutch("account-link-submit")));
    assert_eq!(logins(&pool).await, 0, "opening the link does not sign in");

    let (reply, before) = follow(&app, &link).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.location.as_deref(), Some("/mijn-bestellingen"));
    let after = reply.cookie.expect("a new session id");
    assert_ne!(after, before);
    assert_eq!(logins(&pool).await, 1);

    let mine = get(&app, "/mijn-bestellingen", Some(&after)).await;
    assert_eq!(mine.status, StatusCode::OK);
    assert!(mine.body.contains(KNOWN));
    let old = get(&app, "/mijn-bestellingen", Some(&before)).await;
    assert_eq!(
        old.status,
        StatusCode::SEE_OTHER,
        "the old session id is not signed in"
    );

    let (again, _) = follow_used(&app, &link).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert!(again.body.contains(&dutch("account-link-failed-title")));
    assert_eq!(logins(&pool).await, 1);
}

/// A used or expired link shows the failure page instead of a button; POST anyway.
async fn follow_used(app: &App, link: &str) -> (Reply, Reply) {
    let page = get(app, link, None).await;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
    let (cookie, token) = session_at(app, "/inloggen").await;
    let reply = post(app, link, Some(&cookie), &format!("_csrf={token}")).await;
    (reply, page)
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn signing_in_with_the_link_needs_the_csrf_token(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    ask(&app, KNOWN).await;
    let link = last_link(&pool).await;
    let (cookie, _) = session_at(&app, &link).await;
    let reply = post(&app, &link, Some(&cookie), "").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(logins(&pool).await, 0);
    let (reply, _) = follow(&app, &link).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "the link still works");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_expired_link_signs_nobody_in(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    ask(&app, KNOWN).await;
    let link = last_link(&pool).await;
    app.clock.advance(LINK_VALID);
    let (reply, page) = follow_used(&app, &link).await;
    assert!(page.body.contains(&dutch("account-link-failed-title")));
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(logins(&pool).await, 0);

    let page = get(&app, &format!("/inloggen/{}", "x".repeat(43)), None).await;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn my_orders_needs_a_signed_in_customer(pool: PgPool) {
    let app = app(&pool).await;
    let reply = get(&app, "/mijn-bestellingen", None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.location.as_deref(), Some("/inloggen"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn signing_out_ends_the_login(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let cookie = signed_in(&app, &pool).await;
    let page = get(&app, "/mijn-bestellingen", Some(&cookie)).await;
    let token = csrf_of(&page.body);

    let refused = post(&app, "/uitloggen", Some(&cookie), "").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(logins(&pool).await, 1);

    let reply = post(&app, "/uitloggen", Some(&cookie), &format!("_csrf={token}")).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.location.as_deref(), Some("/"));
    assert_eq!(logins(&pool).await, 0);
    let after = get(&app, "/mijn-bestellingen", Some(&cookie)).await;
    assert_eq!(after.status, StatusCode::SEE_OTHER);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_idle_login_ends(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let cookie = signed_in(&app, &pool).await;
    app.clock
        .advance(auth::customer::IDLE_TIMEOUT + SignedDuration::from_secs(1));
    let reply = get(&app, "/mijn-bestellingen", Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
}

async fn order_for(pool: &PgPool, clock: &FakeClock, email: &str) -> orders::Order {
    let mut conn = pool.acquire().await.unwrap();
    let day = days::create(
        &mut conn,
        &NewBakeDay {
            date: "2026-10-31".parse().unwrap(),
            location_id: PickupLocationId::new(1),
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .unwrap();
    days::update_schedule(
        &mut conn,
        day.id,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(ts("2026-10-01T00:00:00Z")),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    days::open(&mut conn, clock, day.id).await.unwrap();
    let slot = days::list_slots(&mut conn, day.id).await.unwrap()[0].id;
    let bread: i64 = sqlx::query_scalar(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role)
            VALUES ('test-baker', 'Testbakker', 'baker') RETURNING id)
         INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online, allergens_approved_by, allergens_approved_at)
         VALUES ('Desem', 450, 9, 800, 900, true, (SELECT id FROM baker),
            '2026-10-01T10:00:00Z')
         RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let bread = ProductId::new(bread);
    bakedays::range::set_product(
        &mut conn,
        day.id,
        &bakedays::BakeDayProduct {
            product_id: bread,
            max_qty: None,
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await
    .unwrap();
    let mut cart = Cart::new(day.id);
    cart.set(bread, 2, None).unwrap();
    orders::checkout(
        &mut conn,
        clock,
        &cart,
        &CustomerInput {
            email: email.to_owned(),
            name: "Sanne".to_owned(),
            phone: None,
            marketing_opt_in: false,
        },
        slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn my_orders_lists_the_customers_orders_with_links(pool: PgPool) {
    let app = app(&pool).await;
    let order = order_for(&pool, &app.clock, KNOWN).await;
    let token = orders::order::status_token(&mut pool.acquire().await.unwrap(), order.id)
        .await
        .unwrap();
    let cookie = signed_in(&app, &pool).await;
    let page = get(&app, "/mijn-bestellingen", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(&order.number.to_string()));
    assert!(page.body.contains(&format!("/bestelling/{token}")));
    assert!(page.body.contains(&dutch("account-status-pending_payment")));
    assert!(page.body.contains("9,00"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_orders_the_page_says_so(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let cookie = signed_in(&app, &pool).await;
    let page = get(&app, "/mijn-bestellingen", Some(&cookie)).await;
    assert!(page.body.contains(&dutch("account-orders-empty-title")));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn using_links_shares_the_rate_limit(pool: PgPool) {
    let app = app(&pool).await;
    let (cookie, token) = session_at(&app, "/inloggen").await;
    let link = format!("/inloggen/{}", "x".repeat(43));
    for _ in 0..server::account::LOGIN_BURST {
        let reply = post(&app, &link, Some(&cookie), &format!("_csrf={token}")).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    let limited = post(&app, &link, Some(&cookie), &format!("_csrf={token}")).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    let without_token = post(&app, "/inloggen", Some(&cookie), "").await;
    assert_eq!(
        without_token.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the limit comes before the CSRF check"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn signing_in_again_ends_the_previous_login_on_that_session(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let cookie = signed_in(&app, &pool).await;
    assert_eq!(logins(&pool).await, 1);
    ask_from(&app, "192.0.2.50", KNOWN).await;
    let link = last_link(&pool).await;
    let page = get(&app, &link, Some(&cookie)).await;
    let reply = post_from(
        &app,
        "192.0.2.50",
        &link,
        Some(&cookie),
        &format!("_csrf={}", csrf_of(&page.body)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(logins(&pool).await, 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn pages_with_a_secret_or_own_data_are_not_cached(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    ask(&app, KNOWN).await;
    let link = last_link(&pool).await;
    let page = app
        .router
        .clone()
        .oneshot(with_peer(Request::get(&link).body(Body::empty()).unwrap()))
        .await
        .unwrap();
    assert_eq!(page.headers()[CACHE_CONTROL], "private, no-store");
    let (reply, _) = follow(&app, &link).await;
    let cookie = reply.cookie.unwrap();
    let mine = app
        .router
        .clone()
        .oneshot(with_peer(
            Request::get("/mijn-bestellingen")
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(mine.status(), StatusCode::OK);
    assert_eq!(mine.headers()[CACHE_CONTROL], "private, no-store");
}

fn with_peer(mut request: Request<Body>) -> Request<Body> {
    request
        .extensions_mut()
        .insert(ConnectInfo("192.0.2.7:4000".parse::<SocketAddr>().unwrap()));
    request
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_login_mail_expires_with_its_link(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    ask(&app, KNOWN).await;
    let expires: bool = sqlx::query_scalar(
        "SELECT expires_at = created_at + interval '30 minutes' FROM email_outbox
         WHERE kind = 'login_link'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(expires);
}
