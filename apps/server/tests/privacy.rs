//! Privacy tools over HTTP (F12, ADR 0031): a signed-in customer downloads their data,
//! sets drop-alert consent and deletes their account; the owner does export and delete
//! from the back office.
#![forbid(unsafe_code)]

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use auth::{NewStaff, Role};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE,
};
use axum::http::{HeaderMap, Request, StatusCode};
use bakedays::{NewBakeDay, Schedule, days};
use db::PgPool;
use domain::{PickupLocationId, ProductId};
use i18n::Translator;
use jiff::Timestamp;
use orders::{Cart, CustomerInput, OrderedBy};
use server::AppState;
use server::config::Config;
use support::{signed_in, signed_in_at};
use testkit::FakeClock;
use tower::ServiceExt;

const ORIGIN: &str = "https://broodvoorweldadigheid.nl";
const KNOWN: &str = "klant@example.nl";
const NOW: &str = "2026-10-28T12:00:00Z";

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn dutch(id: &str) -> String {
    Translator::dutch().unwrap().t(id, None).unwrap()
}

struct App {
    router: Router,
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
    let clock = Arc::new(FakeClock::new(ts(NOW)));
    let router = server::router(
        AppState::new(pool.clone(), Translator::dutch().unwrap(), config).with_clock(clock),
    );
    App { router }
}

async fn customer(pool: &PgPool, email: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO customer (email, name, phone, created_at)
        VALUES ($1, 'Sanne', '06 1234 5678', now()) RETURNING id",
    )
    .bind(email)
    .fetch_one(pool)
    .await
    .unwrap()
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    cookie: Option<String>,
    body: String,
}

impl Reply {
    fn header(&self, name: impl axum::http::header::AsHeaderName) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }
}

async fn send(app: &App, mut request: Request<Body>) -> Reply {
    request
        .extensions_mut()
        .insert(ConnectInfo("192.0.2.7:4000".parse::<SocketAddr>().unwrap()));
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let cookie = headers
        .get(SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    Reply {
        status,
        headers,
        cookie,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn get(app: &App, path: &str, cookie: Option<&str>) -> Reply {
    let mut request = Request::get(path);
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    send(app, request.body(Body::empty()).unwrap()).await
}

async fn post(app: &App, path: &str, cookie: Option<&str>, body: &str) -> Reply {
    let mut request = Request::post(path).header(CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    send(app, request.body(Body::from(body.to_owned())).unwrap()).await
}

async fn htmx(app: &App, path: &str, cookie: &str, token: &str) -> Reply {
    send(
        app,
        Request::post(path)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(COOKIE, cookie)
            .header("hx-request", "true")
            .header("x-csrf-token", token)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

fn csrf_of(body: &str) -> String {
    let marker = "name=\"_csrf\" value=\"";
    let start = body.find(marker).expect("csrf field") + marker.len();
    let end = start + body[start..].find('"').unwrap();
    body[start..end].to_owned()
}

/// Signs `email` in through a login link; the session cookie and the CSRF token of
/// "Mijn bestellingen".
async fn customer_signed_in(app: &App, pool: &PgPool, email: &str) -> (String, String) {
    let page = get(app, "/inloggen", None).await;
    let cookie = page.cookie.unwrap();
    let body = format!(
        "_csrf={}&email={}",
        csrf_of(&page.body),
        email.replace('@', "%40")
    );
    post(app, "/inloggen", Some(&cookie), &body).await;
    let text: String = sqlx::query_scalar(
        "SELECT text_body FROM email_outbox WHERE kind = 'login_link' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let start = text.find(&format!("{ORIGIN}/inloggen/")).unwrap() + ORIGIN.len();
    let link = &text[start..start + "/inloggen/".len() + 43];
    let page = get(app, link, None).await;
    let cookie = page.cookie.unwrap();
    let reply = post(
        app,
        link,
        Some(&cookie),
        &format!("_csrf={}", csrf_of(&page.body)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let cookie = reply.cookie.unwrap();
    let page = get(app, "/mijn-bestellingen", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    (cookie, csrf_of(&page.body))
}

async fn subscriber(pool: &PgPool, email: &str) -> Option<(bool, bool)> {
    sqlx::query_as(
        "SELECT confirmed_at IS NOT NULL, unsubscribed_at IS NOT NULL FROM subscriber
        WHERE lower(email) = lower($1)",
    )
    .bind(email)
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_orders_page_offers_download_consent_and_delete(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let (cookie, _) = customer_signed_in(&app, &pool, KNOWN).await;
    let page = get(&app, "/mijn-bestellingen", Some(&cookie)).await;
    for id in [
        "privacy-heading",
        "privacy-download",
        "privacy-drop-alerts",
        "privacy-delete",
        "privacy-delete-text",
    ] {
        assert!(page.body.contains(&dutch(id)), "{id}");
    }
    assert!(page.body.contains("href=\"/mijn-bestellingen/gegevens\""));
    assert!(
        page.body
            .contains("action=\"/mijn-bestellingen/verwijderen\"")
    );
    assert!(page.body.contains("<dialog"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_download_holds_the_customers_data_and_is_never_cached(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    assert_eq!(
        get(&app, "/mijn-bestellingen/gegevens", None)
            .await
            .header(LOCATION),
        Some("/inloggen")
    );
    let (cookie, _) = customer_signed_in(&app, &pool, KNOWN).await;
    let file = get(&app, "/mijn-bestellingen/gegevens", Some(&cookie)).await;
    assert_eq!(file.status, StatusCode::OK);
    assert_eq!(file.header(CACHE_CONTROL), Some("private, no-store"));
    assert!(
        file.header(CONTENT_TYPE)
            .unwrap()
            .starts_with("application/json")
    );
    assert!(
        file.header(CONTENT_DISPOSITION)
            .unwrap()
            .starts_with("attachment")
    );
    let json: serde_json::Value = serde_json::from_str(&file.body).unwrap();
    assert_eq!(json["customer"]["email"], KNOWN);
    assert_eq!(json["customer"]["phone"], "06 1234 5678");
    assert_eq!(json["logins"].as_array().unwrap().len(), 1);
    assert_eq!(json["login_links"].as_array().unwrap().len(), 1);
    assert!(json["drop_alerts"].is_null());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn consent_signs_the_address_up_for_drop_alerts_and_off_again(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let (cookie, token) = customer_signed_in(&app, &pool, KNOWN).await;
    let without_token = post(
        &app,
        "/mijn-bestellingen/meldingen",
        Some(&cookie),
        "meldingen=ja",
    )
    .await;
    assert_eq!(without_token.status, StatusCode::FORBIDDEN);
    assert_eq!(subscriber(&pool, KNOWN).await, None);

    let on = post(
        &app,
        "/mijn-bestellingen/meldingen",
        Some(&cookie),
        &format!("_csrf={token}&meldingen=ja"),
    )
    .await;
    assert_eq!(on.status, StatusCode::SEE_OTHER);
    assert_eq!(subscriber(&pool, KNOWN).await, Some((true, false)));
    let consent: bool = sqlx::query_scalar("SELECT marketing_consent FROM customer")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(consent);
    let confirmations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM email_outbox WHERE kind = 'subscription_confirmation'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(confirmations, 0, "the login proved the address");
    let page = get(&app, "/mijn-bestellingen?opgeslagen=1", Some(&cookie)).await;
    assert!(
        page.body
            .contains("id=\"meldingen\" name=\"meldingen\" value=\"ja\" checked")
    );

    let off = post(
        &app,
        "/mijn-bestellingen/meldingen",
        Some(&cookie),
        &format!("_csrf={token}"),
    )
    .await;
    assert_eq!(off.status, StatusCode::SEE_OTHER);
    assert_eq!(subscriber(&pool, KNOWN).await, Some((true, true)));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn deleting_needs_the_csrf_token(pool: PgPool) {
    customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let (cookie, _) = customer_signed_in(&app, &pool, KNOWN).await;
    let reply = post(&app, "/mijn-bestellingen/verwijderen", Some(&cookie), "").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM customer WHERE erased_at IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn deleting_erases_the_customer_mails_them_and_ends_the_session(pool: PgPool) {
    customer(&pool, KNOWN).await;
    sqlx::query(
        "INSERT INTO subscriber (email, confirmed_at, created_at) VALUES ($1, now(), now())",
    )
    .bind(KNOWN)
    .execute(&pool)
    .await
    .unwrap();
    let app = app(&pool).await;
    let (cookie, token) = customer_signed_in(&app, &pool, KNOWN).await;

    let reply = htmx(&app, "/mijn-bestellingen/verwijderen", &cookie, &token).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.header("hx-redirect"),
        Some("/mijn-bestellingen/verwijderd")
    );

    let (email, name, phone): (String, String, Option<String>) =
        sqlx::query_as("SELECT email, name, phone FROM customer")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!email.contains("klant"), "{email}");
    assert_ne!(name, "Sanne");
    assert_eq!(phone, None);
    assert_eq!(subscriber(&pool, KNOWN).await, None);
    let mails: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, to_address FROM email_outbox ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(mails, [("account_deleted".to_owned(), KNOWN.to_owned())]);
    let logins: i64 = sqlx::query_scalar("SELECT count(*) FROM customer_login")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(logins, 0);
    assert_eq!(
        get(&app, "/mijn-bestellingen", Some(&cookie))
            .await
            .header(LOCATION),
        Some("/inloggen")
    );
    let done = get(&app, "/mijn-bestellingen/verwijderd", None).await;
    assert!(done.body.contains(&dutch("privacy-erased-title")));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_open_order_keeps_the_account(pool: PgPool) {
    let app = app(&pool).await;
    order_for(&pool, &FakeClock::new(ts(NOW)), KNOWN).await;
    let (cookie, token) = customer_signed_in(&app, &pool, KNOWN).await;
    let reply = post(
        &app,
        "/mijn-bestellingen/verwijderen",
        Some(&cookie),
        &format!("_csrf={token}"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(
        reply.header(LOCATION),
        Some("/mijn-bestellingen?verwijderen=open")
    );
    let page = get(&app, "/mijn-bestellingen?verwijderen=open", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(&dutch("privacy-refused-open")));
}

async fn staff_user(pool: &PgPool, role: Role) -> auth::StaffUser {
    auth::staff::add(
        pool,
        &NewStaff {
            oidc_sub: format!("sub-{role}"),
            name: format!("Staff {role}"),
            email: None,
            role,
        },
    )
    .await
    .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_the_owner_reaches_the_customer_tools(pool: PgPool) {
    let id = customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    for role in [Role::Baker, Role::Counter] {
        let (cookie, token) = signed_in(&pool, &staff_user(&pool, role).await).await;
        for reply in [
            get(&app, "/staff/customers", Some(&cookie)).await,
            post(
                &app,
                "/staff/customers",
                Some(&cookie),
                &format!("_csrf={token}&email=klant%40example.nl"),
            )
            .await,
            get(
                &app,
                &format!("/staff/customers/{id}/export"),
                Some(&cookie),
            )
            .await,
            post(
                &app,
                &format!("/staff/customers/{id}/erase"),
                Some(&cookie),
                &format!("_csrf={token}"),
            )
            .await,
        ] {
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{role}");
            assert!(!reply.body.contains(KNOWN));
        }
    }
    let name: String = sqlx::query_scalar("SELECT name FROM customer")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Sanne");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_owner_finds_exports_and_erases_a_customer(pool: PgPool) {
    let id = customer(&pool, KNOWN).await;
    let app = app(&pool).await;
    let owner = staff_user(&pool, Role::Owner).await;
    let (cookie, token) = signed_in_at(&pool, &owner, ts(NOW)).await;
    let page = get(&app, "/staff/customers", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    let found = post(
        &app,
        "/staff/customers",
        Some(&cookie),
        &format!("_csrf={token}&email=+Klant%40Example.nl+"),
    )
    .await;
    assert_eq!(found.status, StatusCode::OK);
    assert!(found.body.contains("Sanne"));
    assert!(
        found
            .body
            .contains(&format!("/staff/customers/{id}/export"))
    );
    let none = post(
        &app,
        "/staff/customers",
        Some(&cookie),
        &format!("_csrf={token}&email=niemand%40example.nl"),
    )
    .await;
    assert!(none.body.contains(&dutch("staff-customers-none-title")));

    let file = get(
        &app,
        &format!("/staff/customers/{id}/export"),
        Some(&cookie),
    )
    .await;
    assert_eq!(file.status, StatusCode::OK);
    assert_eq!(file.header(CACHE_CONTROL), Some("private, no-store"));
    let json: serde_json::Value = serde_json::from_str(&file.body).unwrap();
    assert_eq!(json["customer"]["name"], "Sanne");

    let without_token = post(
        &app,
        &format!("/staff/customers/{id}/erase"),
        Some(&cookie),
        "",
    )
    .await;
    assert_eq!(without_token.status, StatusCode::FORBIDDEN);
    let erased = htmx(
        &app,
        &format!("/staff/customers/{id}/erase"),
        &cookie,
        &token,
    )
    .await;
    assert_eq!(
        erased.header("hx-redirect"),
        Some("/staff/customers?verwijderd=1")
    );
    let name: String = sqlx::query_scalar("SELECT name FROM customer")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(name, "Sanne");
    let to: String =
        sqlx::query_scalar("SELECT to_address FROM email_outbox WHERE kind = 'account_deleted'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(to, KNOWN);
    assert_eq!(
        get(
            &app,
            &format!("/staff/customers/{id}/export"),
            Some(&cookie)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let again = htmx(
        &app,
        &format!("/staff/customers/{id}/erase"),
        &cookie,
        &token,
    )
    .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
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
