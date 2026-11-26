//! The webshop over HTTP, with a fake clock and a fake Mollie (F1a, F4, F4b, F4c, F5).
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use bakedays::{NewBakeDay, Schedule, days};
use domain::PaymentProvider;
use domain::{
    BakeDayId, CharityId, Money, OrderId, PickupLocationId, ProductId, ProviderPaymentId,
};
use i18n::{FluentArgs, Translator};
use jiff::{SignedDuration, Timestamp};
use orders::{OrderStatus, RefundReason};
use server::AppState;
use server::config::Config;
use sqlx::{PgConnection, PgPool};
use testkit::{FakeClock, FakePaymentProvider};
use tower::ServiceExt;

const PUBLIC_URL: &str = "https://shop.example.org";

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn dutch(id: &str) -> String {
    Translator::dutch().unwrap().t(id, None).unwrap()
}

fn dutch_with(id: &str, args: &[(&str, &str)]) -> String {
    let mut fluent = FluentArgs::new();
    for (key, value) in args {
        fluent.set(*key, (*value).to_owned());
    }
    Translator::dutch().unwrap().t(id, Some(&fluent)).unwrap()
}

struct Shop {
    app: Router,
    pool: PgPool,
    clock: Arc<FakeClock>,
    mollie: Arc<FakePaymentProvider>,
}

/// Wednesday 28 October 2026, before the deadline of Saturday's drop.
fn shop(pool: PgPool) -> Shop {
    let config = Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://unused/db".to_string()),
        "PUBLIC_URL" => Some(PUBLIC_URL.to_string()),
        _ => None,
    })
    .unwrap();
    let clock = Arc::new(FakeClock::new(ts("2026-10-28T12:00:00Z")));
    let mollie = Arc::new(FakePaymentProvider::new());
    let state = AppState::new(pool.clone(), Translator::dutch().unwrap(), config)
        .with_clock(clock.clone())
        .with_payments(mollie.clone());
    Shop {
        app: server::router(state),
        pool,
        clock,
        mollie,
    }
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
            .map(|v| v.to_str().unwrap())
            .unwrap_or_default()
    }
}

/// A browser: keeps the session cookie and the page's CSRF token.
struct Browser<'a> {
    shop: &'a Shop,
    cookie: String,
    token: String,
}

async fn send(app: &Router, mut request: Request<Body>) -> Reply {
    request.extensions_mut().insert(ConnectInfo(
        "192.0.2.10:5000".parse::<SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn csrf_token(html: &str) -> String {
    let start = html
        .find("<meta name=\"csrf-token\" content=\"")
        .expect("csrf meta tag")
        + "<meta name=\"csrf-token\" content=\"".len();
    html[start..].split('"').next().unwrap().to_owned()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", v.replace('@', "%40").replace(' ', "+")))
        .collect::<Vec<_>>()
        .join("&")
}

impl<'a> Browser<'a> {
    async fn open(shop: &'a Shop, path: &str) -> (Self, Reply) {
        let reply = send(&shop.app, Request::get(path).body(Body::empty()).unwrap()).await;
        let cookie = reply
            .header(SET_COOKIE)
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let token = csrf_token(&reply.body);
        (
            Self {
                shop,
                cookie,
                token,
            },
            reply,
        )
    }

    async fn get(&self, path: &str) -> Reply {
        send(
            &self.shop.app,
            Request::get(path)
                .header(COOKIE, &self.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn htmx_get(&self, path: &str) -> Reply {
        send(
            &self.shop.app,
            Request::get(path)
                .header(COOKIE, &self.cookie)
                .header("hx-request", "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    /// A form post as the page sends it, with the `_csrf` field unless `csrf` is false.
    async fn post(&self, path: &str, pairs: &[(&str, &str)], htmx: bool, csrf: bool) -> Reply {
        let mut all = pairs.to_vec();
        if csrf {
            all.push(("_csrf", &self.token));
        }
        let mut builder = Request::post(path)
            .header(COOKIE, &self.cookie)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded");
        if htmx {
            builder = builder.header("hx-request", "true");
        }
        send(
            &self.shop.app,
            builder.body(Body::from(form(&all))).unwrap(),
        )
        .await
    }

    async fn add(
        &self,
        day: BakeDayId,
        product: ProductId,
        qty: &str,
        price: Option<&str>,
    ) -> Reply {
        let qty_field = format!("aantal-{}", product.get());
        let price_field = format!("prijs-{}", product.get());
        let id = product.get().to_string();
        let mut pairs = vec![("product", id.as_str()), (qty_field.as_str(), qty)];
        if let Some(price) = price {
            pairs.push((price_field.as_str(), price));
        }
        self.post(&format!("/drops/{}/mandje", day.get()), &pairs, true, true)
            .await
    }

    async fn checkout(&self, day: BakeDayId, slot: &str) -> Reply {
        self.post(
            &format!("/drops/{}/bestellen", day.get()),
            &[
                ("naam", "Test Klant"),
                ("email", "klant@example.org"),
                ("telefoon", ""),
                ("ophaalmoment", slot),
            ],
            false,
            true,
        )
        .await
    }
}

async fn webhook(shop: &Shop, body: &str) -> Reply {
    send(
        &shop.app,
        Request::post("/webhooks/mollie")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
            .unwrap(),
    )
    .await
}

struct Drop {
    day: BakeDayId,
    slot: String,
}

fn saturday(charity: Option<CharityId>, online: bool) -> NewBakeDay {
    NewBakeDay {
        date: "2026-10-31".parse().unwrap(),
        location_id: PickupLocationId::new(1),
        online_ordering: online,
        charity_id: charity,
    }
}

async fn open_drop(conn: &mut PgConnection, clock: &FakeClock, input: &NewBakeDay) -> Drop {
    let day = days::create(conn, input).await.unwrap();
    days::update_schedule(
        conn,
        day.id,
        &Schedule {
            online_ordering: day.online_ordering,
            menu_publish_at: None,
            opens_at: Some(ts("2026-10-01T00:00:00Z")),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    days::open(conn, clock, day.id).await.unwrap();
    let slot = days::list_slots(conn, day.id).await.unwrap()[0].id;
    Drop {
        day: day.id,
        slot: slot.get().to_string(),
    }
}

struct Spec<'a> {
    name: &'a str,
    price: i64,
    pwyw: Option<(i64, i64)>,
    allergens: &'a [&'a str],
    ingredients: &'a str,
    approved: bool,
    sold_online: bool,
}

const BREAD: Spec<'static> = Spec {
    name: "Desem",
    price: 450,
    pwyw: None,
    allergens: &["cereals_containing_gluten"],
    ingredients: "Tarwebloem, water, zout",
    approved: true,
    sold_online: true,
};

async fn product(conn: &mut PgConnection, spec: &Spec<'_>) -> ProductId {
    let allergens: Vec<String> = spec.allergens.iter().map(|a| (*a).to_owned()).collect();
    let id: i64 = sqlx::query_scalar(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role) VALUES ('test-baker', 'Testbakker', 'baker')
            ON CONFLICT (oidc_sub) DO UPDATE SET name = EXCLUDED.name RETURNING id),
         kitchen AS (UPDATE company SET may_contain_approved_by = (SELECT id FROM baker),
            may_contain_approved_at = '2026-10-01T10:00:00Z' WHERE may_contain_approved_by IS NULL)
         INSERT INTO product (name_nl, description_nl, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, sold_online, allergens, ingredient_text,
            allergens_approved_by, allergens_approved_at)
         VALUES ($1, 'Lekker brood', $2, 9, $3, $4, $5, 800, 900, $6, $7, $8,
            CASE WHEN $9 THEN (SELECT id FROM baker) END, CASE WHEN $9 THEN timestamptz '2026-10-01T10:00:00Z' END)
         RETURNING id",
    )
    .bind(spec.name)
    .bind(spec.price)
    .bind(spec.pwyw.is_some())
    .bind(spec.pwyw.map(|p| p.0))
    .bind(spec.pwyw.map(|p| p.1))
    .bind(spec.sold_online)
    .bind(allergens)
    .bind(spec.ingredients)
    .bind(spec.approved)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

async fn on_range(conn: &mut PgConnection, day: BakeDayId, product: ProductId, max: Option<u32>) {
    bakedays::range::set_product(
        conn,
        day,
        &bakedays::BakeDayProduct {
            product_id: product,
            max_qty: max,
        },
    )
    .await
    .unwrap();
}

async fn charity(conn: &mut PgConnection, name: &str) -> CharityId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO charity (name, description, url)
         VALUES ($1, 'Helpt mensen', 'https://example.org/doel') RETURNING id",
    )
    .bind(name)
    .fetch_one(conn)
    .await
    .unwrap();
    CharityId::new(id)
}

async fn company_details(conn: &mut PgConnection) {
    sqlx::query("UPDATE company SET address = 'Teststraat 1, 1234 AB Testdorp', kvk = '12345678'")
        .execute(conn)
        .await
        .unwrap();
}

#[derive(Debug, sqlx::FromRow)]
struct Queued {
    kind: String,
    to_address: String,
    subject: String,
    text_body: String,
    dedupe_key: Option<String>,
}

async fn outbox(pool: &PgPool) -> Vec<Queued> {
    sqlx::query_as(
        "SELECT kind, to_address, subject, text_body, dedupe_key FROM email_outbox ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn only_order(pool: &PgPool) -> orders::Order {
    let id: i64 = sqlx::query_scalar(r#"SELECT id FROM "order""#)
        .fetch_one(pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    orders::order::get(&mut conn, OrderId::new(id))
        .await
        .unwrap()
}

async fn token_of(pool: &PgPool, order: OrderId) -> String {
    let mut conn = pool.acquire().await.unwrap();
    orders::order::status_token(&mut conn, order).await.unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_calendar_shows_public_drops_only(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let voedselbank = charity(&mut conn, "Voedselbank Drenthe").await;
    let charity_drop = open_drop(&mut conn, &shop.clock, &saturday(Some(voedselbank), true)).await;
    let normal = days::create(
        &mut conn,
        &NewBakeDay {
            date: "2026-11-07".parse().unwrap(),
            ..saturday(None, true)
        },
    )
    .await
    .unwrap();
    let production_only = days::create(
        &mut conn,
        &NewBakeDay {
            date: "2026-11-14".parse().unwrap(),
            ..saturday(None, false)
        },
    )
    .await
    .unwrap();

    let reply = send(
        &shop.app,
        Request::get("/drops").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let html = reply.body;
    assert!(
        html.contains(&format!("href=\"/drops/{}\"", charity_drop.day.get())),
        "{html}"
    );
    assert!(
        html.contains(&format!("href=\"/drops/{}\"", normal.id.get())),
        "{html}"
    );
    assert!(
        !html.contains(&format!("/drops/{}\"", production_only.id.get())),
        "{html}"
    );
    assert!(html.contains("Voedselbank Drenthe"), "{html}");
    assert!(html.contains(&dutch("drop-normal-sale")), "{html}");
    assert!(html.contains(&dutch("drop-status-open")), "{html}");
    assert!(html.contains(&dutch("drop-status-planned")), "{html}");
    assert!(html.contains("zaterdag 31 oktober 2026"), "{html}");

    let reply = send(
        &shop.app,
        Request::get(format!("/drops/{}", production_only.id.get()))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_ical_feed_is_text_calendar(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let reply = send(
        &shop.app,
        Request::get("/drops.ics").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header(CONTENT_TYPE), "text/calendar; charset=utf-8");
    assert!(
        reply.body.starts_with("BEGIN:VCALENDAR\r\n"),
        "{}",
        reply.body
    );
    assert!(
        reply
            .body
            .contains(&format!("UID:pickup-slot-{}@shop.example.org", drop.slot)),
        "{}",
        reply.body
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_menu_shows_approved_allergens_only_and_hides_offline_products(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    let unapproved = product(
        &mut conn,
        &Spec {
            name: "Eierkoek",
            allergens: &["eggs"],
            ingredients: "Geheime eieren",
            approved: false,
            ..BREAD
        },
    )
    .await;
    let frozen = product(
        &mut conn,
        &Spec {
            name: "Diepvriesbrood",
            sold_online: false,
            ..BREAD
        },
    )
    .await;
    for id in [bread, unapproved, frozen] {
        on_range(&mut conn, drop.day, id, Some(5)).await;
    }

    let (_, reply) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    assert_eq!(reply.status, StatusCode::OK);
    let html = reply.body;
    assert!(html.contains("Desem"), "{html}");
    assert!(
        html.contains(&dutch("allergen-cereals_containing_gluten")),
        "{html}"
    );
    assert!(html.contains("Tarwebloem, water, zout"), "{html}");
    assert!(html.contains("Nog 5 over"), "{html}");
    assert!(
        html.contains(&format!("name=\"aantal-{}\"", bread.get())),
        "{html}"
    );

    assert!(html.contains("Eierkoek"), "{html}");
    assert!(html.contains(&dutch("product-allergens-pending")), "{html}");
    assert!(!html.contains(&dutch("allergen-eggs")), "{html}");
    assert!(!html.contains("Geheime eieren"), "{html}");
    assert!(
        !html.contains(&format!("name=\"aantal-{}\"", unapproved.get())),
        "{html}"
    );

    assert!(!html.contains("Diepvriesbrood"), "{html}");

    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    let reply = browser.add(drop.day, unapproved, "1", None).await;
    assert!(
        reply.body.contains(&dutch("cart-error-unavailable")),
        "{}",
        reply.body
    );
    let reply = browser.add(drop.day, frozen, "1", None).await;
    assert!(
        reply.body.contains(&dutch("cart-error-unavailable")),
        "{}",
        reply.body
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn every_product_shows_the_kitchen_may_contain_list_once_approved(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    for change in [
        "UPDATE company SET may_contain = '{cereals_containing_gluten,nuts}'",
        "UPDATE company SET may_contain_approved_by = (SELECT id FROM staff_user LIMIT 1),
            may_contain_approved_at = now()",
    ] {
        sqlx::query(change).execute(&mut *conn).await.unwrap();
    }

    let (_, reply) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    let html = reply.body;
    let may_contain = html
        .split(&dutch("allergen-may-contain"))
        .nth(1)
        .expect("a may-contain list");
    assert!(may_contain.contains(&dutch("allergen-nuts")), "{html}");
    assert!(
        !may_contain.contains(&dutch("allergen-cereals_containing_gluten")),
        "what the bread contains is not repeated: {html}"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn nothing_can_be_ordered_while_the_kitchen_list_is_unapproved(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;

    sqlx::query("UPDATE company SET may_contain = '{nuts}'")
        .execute(&mut *conn)
        .await
        .unwrap();
    let (_, reply) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    let html = reply.body;
    assert!(html.contains(&dutch("product-allergens-pending")), "{html}");
    assert!(!html.contains(&dutch("allergen-nuts")), "{html}");
    assert!(
        !html.contains(&format!("name=\"aantal-{}\"", bread.get())),
        "{html}"
    );
    let reply = browser.add(drop.day, bread, "1", None).await;
    assert!(
        reply.body.contains(&dutch("cart-error-unavailable")),
        "{}",
        reply.body
    );

    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains(&dutch("checkout-error-unavailable")),
        "{}",
        reply.body
    );
    assert!(shop.mollie.created().is_empty());
    let orders: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "order""#)
        .fetch_one(&shop.pool)
        .await
        .unwrap();
    assert_eq!(orders, 0);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_recipe_change_blocks_checkout_until_the_baker_approves_again(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (recipe, v1) = recipes::recipes::create(
        &mut conn,
        &recipes::RecipeInput {
            name: "Desem".to_owned(),
            kind: recipes::RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    recipes::recipes::link_product(&mut conn, recipe.id, bread)
        .await
        .unwrap();
    let approve = async |conn: &mut PgConnection| {
        sqlx::query(
            "UPDATE product SET allergens_approved_by = (SELECT id FROM staff_user LIMIT 1),
                allergens_approved_at = now() WHERE id = $1",
        )
        .bind(bread.get())
        .execute(conn)
        .await
        .unwrap();
    };
    approve(&mut conn).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;

    recipes::recipes::freeze(&mut conn, recipe.id, v1.id, &*shop.clock)
        .await
        .unwrap();
    let v2 = recipes::recipes::new_version(&mut conn, recipe.id)
        .await
        .unwrap();
    recipes::recipes::set_current(&mut conn, recipe.id, v2.id)
        .await
        .unwrap();
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains(&dutch("checkout-error-unavailable")),
        "{}",
        reply.body
    );
    assert!(shop.mollie.created().is_empty());

    approve(&mut conn).await;
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(shop.mollie.created().len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn cart_and_checkout_need_the_csrf_token_but_the_webhook_does_not(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;

    let id = bread.get().to_string();
    let qty = format!("aantal-{id}");
    let reply = browser
        .post(
            &format!("/drops/{}/mandje", drop.day.get()),
            &[("product", &id), (&qty, "1")],
            true,
            false,
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let reply = browser
        .post(
            &format!("/drops/{}/bestellen", drop.day.get()),
            &[
                ("naam", "Test"),
                ("email", "a@example.org"),
                ("ophaalmoment", &drop.slot),
            ],
            false,
            false,
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);

    let reply = webhook(&shop, "id=tr_unknown1").await;
    assert_eq!(reply.status, StatusCode::OK, "unknown ids are acknowledged");
    for bad in ["", "id=", "id=re_123", "id=tr_1&id=tr_2", "nonsense"] {
        assert_eq!(
            webhook(&shop, bad).await.status,
            StatusCode::BAD_REQUEST,
            "{bad:?}"
        );
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_charity_order_goes_from_cart_through_mollie_to_paid(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    company_details(&mut conn).await;
    let voedselbank = charity(&mut conn, "Voedselbank").await;
    let drop = open_drop(&mut conn, &shop.clock, &saturday(Some(voedselbank), true)).await;
    let loaf = product(
        &mut conn,
        &Spec {
            name: "Donatiebrood",
            price: 600,
            pwyw: Some((300, 1500)),
            ..BREAD
        },
    )
    .await;
    on_range(&mut conn, drop.day, loaf, Some(10)).await;
    let (browser, page) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    assert!(
        page.body.contains("value=\"6,00\""),
        "suggested price prefilled"
    );

    let reply = browser.add(drop.day, loaf, "2", Some("20,00")).await;
    assert!(
        reply.body.contains(&dutch_with(
            "cart-error-price-high",
            &[("max", "€\u{a0}15,00")]
        )),
        "{}",
        reply.body
    );
    let reply = browser.add(drop.day, loaf, "11", Some("12,50")).await;
    assert!(
        reply.body.contains("Er zijn er nog maar 10 over."),
        "{}",
        reply.body
    );
    let reply = browser.add(drop.day, loaf, "2", Some("12,50")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply
            .body
            .trim_start()
            .starts_with("<section class=\"cart\" id=\"mandje\""),
        "{}",
        reply.body
    );
    assert!(reply.body.contains(&dutch("cart-added")), "{}", reply.body);
    assert!(reply.body.contains("€\u{a0}25,00"), "{}", reply.body);

    let form_page = browser
        .get(&format!("/drops/{}/afrekenen", drop.day.get()))
        .await;
    assert_eq!(form_page.status, StatusCode::OK);
    assert!(
        form_page.body.contains("Voedselbank"),
        "charity on checkout"
    );
    assert!(
        form_page
            .body
            .contains("name=\"nieuwsbrief\" value=\"ja\">"),
        "marketing unticked: {}",
        form_page.body
    );

    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(
        reply.header(LOCATION),
        "https://checkout.fake-mollie.test/tr_fake1"
    );
    let order = only_order(&shop.pool).await;
    assert_eq!(order.status, OrderStatus::PendingPayment);
    assert_eq!(order.total, Money::from_cents(2500));
    let token = token_of(&shop.pool, order.id).await;
    let created = shop.mollie.created();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].amount, Money::from_cents(2500));
    assert_eq!(created[0].order, order.id);
    assert_eq!(
        created[0].redirect_url,
        format!("{PUBLIC_URL}/bestelling/{token}")
    );
    assert_eq!(
        created[0].webhook_url,
        format!("{PUBLIC_URL}/webhooks/mollie")
    );
    assert_eq!(
        created[0]
            .idempotency_key
            .as_ref()
            .map(|k| k.as_str().to_owned()),
        Some(format!("order-{}-payment", order.id.get()))
    );
    assert!(!created[0].redirect_url.contains(&order.number.to_string()));

    let waiting = browser.htmx_get(&format!("/bestelling/{token}")).await;
    assert!(
        waiting
            .body
            .contains(&format!("hx-get=\"/bestelling/{token}\"")),
        "{}",
        waiting.body
    );
    assert!(waiting.body.contains(&dutch("order-status-waiting")));

    let id = ProviderPaymentId::new("tr_fake1").unwrap();
    shop.mollie.pay(&id);
    for _ in 0..2 {
        assert_eq!(webhook(&shop, "id=tr_fake1").await.status, StatusCode::OK);
    }
    let paid = only_order(&shop.pool).await;
    assert_eq!(paid.status, OrderStatus::Paid);
    let allocations = orders::allocations(&mut conn, paid.id).await.unwrap();
    assert_eq!(
        allocations.len(),
        1,
        "the duplicate webhook changed nothing"
    );
    assert_eq!(allocations[0].charity_id, voedselbank);
    assert_eq!(allocations[0].amount, Money::from_cents(2500));

    let queued = outbox(&shop.pool).await;
    assert_eq!(
        queued.len(),
        1,
        "one confirmation for two webhooks: {queued:?}"
    );
    let confirmation = &queued[0];
    assert_eq!(confirmation.kind, "order_confirmation");
    assert_eq!(confirmation.to_address, "klant@example.org");
    assert_eq!(
        confirmation.dedupe_key.as_deref(),
        Some(format!("order-confirmation:{}", paid.id.get()).as_str())
    );
    assert!(confirmation.subject.contains(&paid.number.to_string()));
    for expected in [
        "Hoi Test Klant,",
        "2 × Donatiebrood: €\u{a0}25,00 (jouw prijs: €\u{a0}12,50 per stuk)",
        "Voedselbank",
        "BTW 9% over €\u{a0}22,94: €\u{a0}2,06",
        "De Proef",
        "KvK 12345678",
    ] {
        assert!(
            confirmation.text_body.contains(expected),
            "{expected}: {}",
            confirmation.text_body
        );
    }
    assert!(
        confirmation
            .text_body
            .contains(&format!("{PUBLIC_URL}/bestelling/{token}")),
        "status page link: {}",
        confirmation.text_body
    );

    let done = browser.get(&format!("/bestelling/{token}")).await;
    assert_eq!(done.status, StatusCode::OK);
    assert!(
        done.body.contains(&dutch("order-status-paid")),
        "{}",
        done.body
    );
    assert!(
        !done.body.contains("hx-get=\"/bestelling/"),
        "polling stops"
    );
    let cart = browser.get(&format!("/drops/{}", drop.day.get())).await;
    assert!(
        cart.body.contains(&dutch("cart-empty")),
        "the cart is emptied"
    );

    let unknown = browser
        .get("/bestelling/00000000-0000-4000-8000-000000000000")
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let by_number = browser.get(&format!("/bestelling/{}", paid.number)).await;
    assert_eq!(by_number.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_payment_after_the_hold_is_recorded_as_refund_due(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    company_details(&mut conn).await;
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    assert_eq!(
        browser.checkout(drop.day, &drop.slot).await.status,
        StatusCode::SEE_OTHER
    );
    let order = only_order(&shop.pool).await;
    let token = token_of(&shop.pool, order.id).await;

    shop.clock.advance(SignedDuration::from_mins(20));
    let expired = browser.htmx_get(&format!("/bestelling/{token}")).await;
    assert!(
        expired.body.contains(&dutch("order-status-expired")),
        "{}",
        expired.body
    );

    shop.mollie
        .pay(&ProviderPaymentId::new("tr_fake1").unwrap());
    for _ in 0..2 {
        assert_eq!(webhook(&shop, "id=tr_fake1").await.status, StatusCode::OK);
    }
    let order = only_order(&shop.pool).await;
    assert_eq!(order.status, OrderStatus::Cancelled);
    let due = orders::refunds_due(&mut conn).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].refund_reason, Some(RefundReason::HoldExpired));
    assert_eq!(due[0].provider_payment_id, "tr_fake1");

    let queued = outbox(&shop.pool).await;
    assert_eq!(
        queued.len(),
        1,
        "no confirmation, one cancellation: {queued:?}"
    );
    assert_eq!(queued[0].kind, "order_cancelled");
    assert_eq!(
        queued[0].dedupe_key.as_deref(),
        Some(format!("refund-due:{}", due[0].id.get()).as_str())
    );
    assert!(
        queued[0]
            .text_body
            .contains(&dutch_with("email-refund", &[("amount", "€\u{a0}4,50")])),
        "{}",
        queued[0].text_body
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_kvk_details_the_payment_counts_but_no_email_is_queued(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    browser.checkout(drop.day, &drop.slot).await;
    shop.mollie
        .pay(&ProviderPaymentId::new("tr_fake1").unwrap());
    assert_eq!(webhook(&shop, "id=tr_fake1").await.status, StatusCode::OK);
    assert_eq!(only_order(&shop.pool).await.status, OrderStatus::Paid);
    assert!(outbox(&shop.pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_failed_payment_shows_on_the_status_page(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    browser.checkout(drop.day, &drop.slot).await;
    let order = only_order(&shop.pool).await;
    let token = token_of(&shop.pool, order.id).await;

    shop.mollie
        .fail(&ProviderPaymentId::new("tr_fake1").unwrap());
    assert_eq!(webhook(&shop, "id=tr_fake1").await.status, StatusCode::OK);
    let status: String =
        sqlx::query_scalar("SELECT status FROM payment WHERE provider_payment_id = 'tr_fake1'")
            .fetch_one(&shop.pool)
            .await
            .unwrap();
    assert_eq!(status, "failed");
    let page = browser.get(&format!("/bestelling/{token}")).await;
    assert!(
        page.body.contains(&dutch("order-status-failed")),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains(&format!("href=\"/drops/{}\"", drop.day.get()))
    );
    assert_eq!(
        only_order(&shop.pool).await.status,
        OrderStatus::PendingPayment
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_status_page_asks_mollie_when_the_webhook_is_late(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    browser.checkout(drop.day, &drop.slot).await;
    let order = only_order(&shop.pool).await;
    let token = token_of(&shop.pool, order.id).await;

    shop.mollie
        .pay(&ProviderPaymentId::new("tr_fake1").unwrap());
    let page = browser.get(&format!("/bestelling/{token}")).await;
    assert!(
        page.body.contains(&dutch("order-status-paid")),
        "{}",
        page.body
    );
    assert_eq!(only_order(&shop.pool).await.status, OrderStatus::Paid);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn checkout_after_the_deadline_is_refused_in_dutch(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;

    shop.clock.set(ts("2026-10-29T19:00:00Z"));
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains(&dutch("checkout-error-deadline")),
        "{}",
        reply.body
    );
    assert!(shop.mollie.created().is_empty());
    let orders: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "order""#)
        .fetch_one(&shop.pool)
        .await
        .unwrap();
    assert_eq!(orders, 0);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn checkout_checks_the_form_and_survives_a_mollie_outage(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;

    let reply = browser
        .post(
            &format!("/drops/{}/bestellen", drop.day.get()),
            &[("naam", " "), ("email", "geen-mail"), ("ophaalmoment", "")],
            false,
            true,
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    for id in [
        "checkout-error-name",
        "checkout-error-email",
        "checkout-error-pickup",
    ] {
        assert!(reply.body.contains(&dutch(id)), "{id}: {}", reply.body);
    }

    shop.mollie.set_down(true);
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        reply.body.contains(&dutch("checkout-error-payment")),
        "{}",
        reply.body
    );
    let cart = browser.get(&format!("/drops/{}", drop.day.get())).await;
    assert!(
        !cart.body.contains(&dutch("cart-empty")),
        "the cart is kept"
    );

    shop.mollie.set_down(false);
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let orders: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "order""#)
        .fetch_one(&shop.pool)
        .await
        .unwrap();
    assert_eq!(orders, 1, "no second order, no second hold");
    assert_eq!(
        shop.mollie.created()[0].order,
        only_order(&shop.pool).await.id
    );
    let cart = browser.get(&format!("/drops/{}", drop.day.get())).await;
    assert!(cart.body.contains(&dutch("cart-empty")), "{}", cart.body);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn checkout_is_rate_limited_per_client(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    let path = format!("/drops/{}/bestellen", drop.day.get());
    for _ in 0..server::shop::CHECKOUT_BURST {
        let reply = browser.post(&path, &[], false, false).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
    }
    let reply = browser.post(&path, &[], false, false).await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    let page = browser.get(&format!("/drops/{}", drop.day.get())).await;
    assert_eq!(page.status, StatusCode::OK, "only checkout is limited");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn removing_from_another_drop_keeps_the_cart(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let first = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let second = open_drop(
        &mut conn,
        &shop.clock,
        &NewBakeDay {
            date: "2026-11-07".parse().unwrap(),
            ..saturday(None, true)
        },
    )
    .await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, first.day, bread, None).await;
    on_range(&mut conn, second.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", first.day.get())).await;
    browser.add(first.day, bread, "2", None).await;
    browser.add(second.day, bread, "0", None).await;
    let page = browser.get(&format!("/drops/{}", first.day.get())).await;
    assert!(!page.body.contains(&dutch("cart-empty")), "{}", page.body);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_failed_payment_can_be_retried_when_its_hold_has_the_last_bread(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, Some(1)).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;

    shop.mollie.set_down(true);
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);

    shop.mollie.set_down(false);
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let orders: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "order""#)
        .fetch_one(&shop.pool)
        .await
        .unwrap();
    assert_eq!(orders, 1);
    assert_eq!(
        shop.mollie.created()[0].order,
        only_order(&shop.pool).await.id
    );
}

/// Each page loads the session before it takes its own connection, so a full pool
/// cannot leave every request waiting for a second one.
#[sqlx::test(migrator = "db::MIGRATOR")]
async fn pages_with_a_session_work_with_a_single_connection(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let drop = open_drop(&mut conn, &clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    std::mem::drop(conn);
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_lazy_with((*pool.connect_options()).clone());
    let shop = shop(single);
    let menu = format!("/drops/{}", drop.day.get());
    let (browser, _) = Browser::open(&shop, &menu).await;
    browser.add(drop.day, bread, "1", None).await;
    for path in [menu.clone(), format!("{menu}/afrekenen")] {
        let reply = browser.get(&path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.body);
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_cart_that_costs_nothing_is_refused_before_any_hold(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let sample = product(
        &mut conn,
        &Spec {
            name: "Proefstukje",
            price: 0,
            ..BREAD
        },
    )
    .await;
    on_range(&mut conn, drop.day, sample, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, sample, "1", None).await;

    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains(&dutch("checkout-error-free")),
        "{}",
        reply.body
    );
    let orders: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "order""#)
        .fetch_one(&shop.pool)
        .await
        .unwrap();
    assert_eq!(orders, 0);
    assert!(shop.mollie.created().is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_line_that_left_the_menu_stays_in_the_cart_as_unavailable(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    let rye = product(
        &mut conn,
        &Spec {
            name: "Roggebrood",
            price: 500,
            ..BREAD
        },
    )
    .await;
    on_range(&mut conn, drop.day, bread, None).await;
    on_range(&mut conn, drop.day, rye, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, rye, "1", None).await;
    sqlx::query("UPDATE product SET active = false WHERE id = $1")
        .bind(rye.get())
        .execute(&mut *conn)
        .await
        .unwrap();

    let panel = browser.add(drop.day, bread, "2", None).await;
    assert_eq!(panel.status, StatusCode::OK);
    assert!(panel.body.contains("Roggebrood"), "{}", panel.body);
    assert!(
        panel.body.contains(&dutch("cart-line-unavailable")),
        "{}",
        panel.body
    );
    assert!(
        panel
            .body
            .contains(&dutch_with("cart-remove", &[("product", "Roggebrood")])),
        "{}",
        panel.body
    );
    let page = browser.get(&format!("/drops/{}", drop.day.get())).await;
    for body in [&panel.body, &page.body] {
        assert!(body.contains("Roggebrood"), "{body}");
        assert!(body.contains(&dutch("cart-line-unavailable")), "{body}");
        assert!(
            !body.contains("14,00"),
            "the unavailable line is not in the total"
        );
        assert!(body.contains("9,00"), "{body}");
    }

    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains(&dutch("checkout-error-unavailable")),
        "{}",
        reply.body
    );
}

/// Holds `create_payment` until the test lets it go, like a slow Mollie.
struct SlowMollie {
    inner: FakePaymentProvider,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl PaymentProvider for SlowMollie {
    fn create_payment<'a>(
        &'a self,
        request: &'a domain::NewPayment,
    ) -> domain::ProviderFuture<'a, domain::CreatedPayment> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            self.inner.create_payment(request).await
        })
    }

    fn get_payment<'a>(
        &'a self,
        id: &'a ProviderPaymentId,
    ) -> domain::ProviderFuture<'a, domain::PaymentSnapshot> {
        self.inner.get_payment(id)
    }

    fn refund<'a>(
        &'a self,
        request: &'a domain::RefundRequest,
    ) -> domain::ProviderFuture<'a, domain::CreatedRefund> {
        self.inner.refund(request)
    }

    fn list_payment_fees<'a>(
        &'a self,
        cursor: Option<&'a domain::FeeCursor>,
    ) -> domain::ProviderFuture<'a, domain::FeePage> {
        self.inner.list_payment_fees(cursor)
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_slow_mollie_does_not_hold_a_database_connection(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = Arc::new(FakeClock::new(ts("2026-10-28T12:00:00Z")));
    let drop = open_drop(&mut conn, &clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    std::mem::drop(conn);
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(1))
        .connect_lazy_with((*pool.connect_options()).clone());
    let mollie = Arc::new(SlowMollie {
        inner: FakePaymentProvider::new(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let config = Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://unused/db".to_string()),
        "PUBLIC_URL" => Some(PUBLIC_URL.to_string()),
        _ => None,
    })
    .unwrap();
    let state = AppState::new(single.clone(), Translator::dutch().unwrap(), config)
        .with_clock(clock.clone())
        .with_payments(mollie.clone());
    let shop = Shop {
        app: server::router(state),
        pool: single,
        clock,
        mollie: Arc::new(FakePaymentProvider::new()),
    };
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;

    let (checkout, calendar) = tokio::join!(browser.checkout(drop.day, &drop.slot), async {
        mollie.entered.notified().await;
        let calendar = send(
            &shop.app,
            Request::get("/drops").body(Body::empty()).unwrap(),
        )
        .await;
        mollie.release.notify_one();
        calendar
    });
    assert_eq!(calendar.status, StatusCode::OK, "{}", calendar.body);
    assert_eq!(checkout.status, StatusCode::SEE_OTHER, "{}", checkout.body);
    assert_eq!(mollie.inner.created().len(), 1);
}

/// The deadline is Thursday 29 October 20:00 Amsterdam time (19:00 UTC).
#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_failed_payment_can_be_retried_past_the_deadline_while_its_hold_runs(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;

    shop.clock.set(ts("2026-10-29T18:59:30Z"));
    shop.mollie.set_down(true);
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);

    shop.mollie.set_down(false);
    shop.clock.set(ts("2026-10-29T19:00:30Z"));
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(
        shop.mollie.created()[0].order,
        only_order(&shop.pool).await.id
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_failed_payment_is_not_retried_after_its_hold(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;

    shop.clock.set(ts("2026-10-29T18:59:30Z"));
    shop.mollie.set_down(true);
    browser.checkout(drop.day, &drop.slot).await;

    shop.mollie.set_down(false);
    shop.clock.set(ts("2026-10-29T19:15:00Z"));
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains(&dutch("checkout-error-deadline")),
        "{}",
        reply.body
    );
    assert!(shop.mollie.created().is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_status_page_finds_a_late_payment_after_the_hold(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    company_details(&mut conn).await;
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    browser.checkout(drop.day, &drop.slot).await;
    let order = only_order(&shop.pool).await;
    let token = token_of(&shop.pool, order.id).await;

    shop.clock.advance(SignedDuration::from_mins(20));
    shop.mollie
        .pay(&ProviderPaymentId::new("tr_fake1").unwrap());
    let page = browser.get(&format!("/bestelling/{token}")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains(&dutch("order-status-refund")),
        "{}",
        page.body
    );
    let due = orders::refunds_due(&mut conn).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].refund_reason, Some(RefundReason::HoldExpired));
    let queued = outbox(&shop.pool).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert_eq!(queued[0].kind, "order_cancelled");

    let again = browser.get(&format!("/bestelling/{token}")).await;
    assert!(again.body.contains(&dutch("order-status-refund")));
    assert_eq!(outbox(&shop.pool).await.len(), 1);
}

async fn failed_first_payment(shop: &Shop, browser: &Browser<'_>, drop: &Drop) {
    shop.mollie.set_down(true);
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    shop.mollie.set_down(false);
}

async fn order_statuses(pool: &PgPool) -> Vec<(String, Option<String>)> {
    sqlx::query_as(r#"SELECT status, cancel_reason FROM "order" ORDER BY id"#)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_changed_cart_replaces_the_unpaid_order(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, Some(3)).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    failed_first_payment(&shop, &browser, &drop).await;

    browser.add(drop.day, bread, "2", None).await;
    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(
        order_statuses(&shop.pool).await,
        vec![
            (
                "cancelled".to_owned(),
                Some("replaced_by_customer".to_owned())
            ),
            ("pending_payment".to_owned(), None),
        ]
    );
    let created = shop.mollie.created();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].amount, Money::from_cents(900));
    let left = bakedays::range::availability(
        &mut conn,
        drop.day,
        &orders::Orders::new(shop.clock.as_ref()),
    )
    .await
    .unwrap();
    assert_eq!(
        left[0].remaining,
        bakedays::capacity::Remaining::Pieces(1),
        "the replaced order's hold is free"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_changed_email_replaces_the_unpaid_order(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    failed_first_payment(&shop, &browser, &drop).await;

    let reply = browser
        .post(
            &format!("/drops/{}/bestellen", drop.day.get()),
            &[
                ("naam", "Test Klant"),
                ("email", "ander@example.org"),
                ("telefoon", ""),
                ("ophaalmoment", &drop.slot),
            ],
            false,
            true,
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let statuses = order_statuses(&shop.pool).await;
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses[0].1.as_deref(), Some("replaced_by_customer"));
    let email: String = sqlx::query_scalar(
        r#"SELECT c.email FROM "order" o JOIN customer c ON c.id = o.customer_id
        WHERE o.status = 'pending_payment'"#,
    )
    .fetch_one(&shop.pool)
    .await
    .unwrap();
    assert_eq!(email, "ander@example.org");
    assert_eq!(shop.mollie.created().len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_unchanged_checkout_pays_the_same_unpaid_order(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    failed_first_payment(&shop, &browser, &drop).await;

    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(
        order_statuses(&shop.pool).await,
        vec![("pending_payment".to_owned(), None)]
    );
    assert_eq!(
        shop.mollie.created()[0].order,
        only_order(&shop.pool).await.id
    );
}

/// The replaced order's Mollie payment exists but its link never reached the customer.
/// Should it be paid anyway, it is refunded with the usual cancellation email.
#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_late_payment_of_a_replaced_order_is_refunded(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    company_details(&mut conn).await;
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    assert_eq!(
        browser.checkout(drop.day, &drop.slot).await.status,
        StatusCode::SEE_OTHER
    );
    let replaced = only_order(&shop.pool).await;
    assert!(
        orders::cancel_pending(
            &mut conn,
            shop.clock.as_ref(),
            replaced.id,
            orders::CancelReason::ReplacedByCustomer
        )
        .await
        .unwrap()
    );

    shop.mollie
        .pay(&ProviderPaymentId::new("tr_fake1").unwrap());
    assert_eq!(webhook(&shop, "id=tr_fake1").await.status, StatusCode::OK);
    let due = orders::refunds_due(&mut conn).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].refund_reason, Some(RefundReason::OrderCancelled));
    let queued = outbox(&shop.pool).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert_eq!(queued[0].kind, "order_cancelled");
}

async fn checkout_as(browser: &Browser<'_>, drop: &Drop, email: &str) -> Reply {
    browser
        .post(
            &format!("/drops/{}/bestellen", drop.day.get()),
            &[
                ("naam", "Test Klant"),
                ("email", email),
                ("telefoon", ""),
                ("ophaalmoment", &drop.slot),
            ],
            false,
            true,
        )
        .await
}

async fn held_order_is_intact(shop: &Shop) {
    assert_eq!(
        order_statuses(&shop.pool).await,
        vec![("pending_payment".to_owned(), None)],
        "the held order stays"
    );
    assert!(shop.mollie.created().is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_changed_retry_past_the_deadline_keeps_the_held_order(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    shop.clock.set(ts("2026-10-29T18:59:30Z"));
    failed_first_payment(&shop, &browser, &drop).await;

    shop.clock.set(ts("2026-10-29T19:00:30Z"));
    let reply = checkout_as(&browser, &drop, "ander@example.org").await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply
            .body
            .contains(&dutch("checkout-error-replace-deadline")),
        "{}",
        reply.body
    );
    held_order_is_intact(&shop).await;

    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(
        reply.status,
        StatusCode::SEE_OTHER,
        "unchanged it still pays"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_changed_retry_with_an_invalid_email_keeps_the_held_order(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    failed_first_payment(&shop, &browser, &drop).await;

    let reply = checkout_as(&browser, &drop, "klant x@example.org").await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains(&dutch("checkout-error-email")),
        "{}",
        reply.body
    );
    held_order_is_intact(&shop).await;
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_changed_retry_exceeding_stock_keeps_the_held_order(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    let rye = product(
        &mut conn,
        &Spec {
            name: "Roggebrood",
            price: 500,
            ..BREAD
        },
    )
    .await;
    on_range(&mut conn, drop.day, bread, None).await;
    on_range(&mut conn, drop.day, rye, Some(1)).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    failed_first_payment(&shop, &browser, &drop).await;
    browser.add(drop.day, rye, "1", None).await;

    let (other, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    other.add(drop.day, rye, "1", None).await;
    assert_eq!(
        other.checkout(drop.day, &drop.slot).await.status,
        StatusCode::SEE_OTHER
    );

    let reply = browser.checkout(drop.day, &drop.slot).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.body
    );
    let statuses = order_statuses(&shop.pool).await;
    assert_eq!(
        statuses[0],
        ("pending_payment".to_owned(), None),
        "the held order stays: {statuses:?}"
    );
    assert_eq!(statuses.len(), 2, "only the other customer's order is new");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_retry_after_paying_elsewhere_shows_the_paid_order(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    failed_first_payment(&shop, &browser, &drop).await;
    let order = only_order(&shop.pool).await;
    orders::mark_paid(
        &mut conn,
        shop.clock.as_ref(),
        order.id,
        &orders::PaymentReceived {
            reference: "tr_elsewhere".to_owned(),
            amount: order.total,
        },
    )
    .await
    .unwrap();

    let reply = checkout_as(&browser, &drop, "ander@example.org").await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let token = token_of(&shop.pool, order.id).await;
    assert_eq!(reply.header(LOCATION), format!("/bestelling/{token}"));
    assert_eq!(order_statuses(&shop.pool).await.len(), 1);
    assert!(shop.mollie.created().is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_status_page_asks_mollie_at_most_once_per_ten_seconds(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    browser.checkout(drop.day, &drop.slot).await;
    let token = token_of(&shop.pool, only_order(&shop.pool).await.id).await;
    let page = format!("/bestelling/{token}");

    assert!(
        browser
            .get(&page)
            .await
            .body
            .contains(&dutch("order-status-waiting"))
    );
    shop.mollie
        .pay(&ProviderPaymentId::new("tr_fake1").unwrap());
    shop.clock.advance(SignedDuration::from_secs(9));
    assert!(
        browser
            .get(&page)
            .await
            .body
            .contains(&dutch("order-status-waiting")),
        "no second lookup within ten seconds"
    );
    shop.clock.advance(SignedDuration::from_secs(1));
    assert!(
        browser
            .get(&page)
            .await
            .body
            .contains(&dutch("order-status-paid"))
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_payment_for_a_cancelled_day_gets_the_refund_email(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    company_details(&mut conn).await;
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    browser.checkout(drop.day, &drop.slot).await;
    days::cancel(&mut conn, shop.clock.as_ref(), drop.day, "Ziek")
        .await
        .unwrap();

    shop.mollie
        .pay(&ProviderPaymentId::new("tr_fake1").unwrap());
    assert_eq!(webhook(&shop, "id=tr_fake1").await.status, StatusCode::OK);
    let due = orders::refunds_due(&mut conn).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].refund_reason, Some(RefundReason::DayFinal));
    let queued = outbox(&shop.pool).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert_eq!(queued[0].kind, "order_cancelled");
    let token = token_of(&shop.pool, only_order(&shop.pool).await.id).await;
    let page = browser.get(&format!("/bestelling/{token}")).await;
    assert!(
        page.body.contains(&dutch("order-status-refund")),
        "{}",
        page.body
    );
}

impl Browser<'_> {
    async fn checkout_with_donation(&self, day: BakeDayId, slot: &str, donation: &str) -> Reply {
        self.post(
            &format!("/drops/{}/bestellen", day.get()),
            &[
                ("naam", "Test Klant"),
                ("email", "klant@example.org"),
                ("telefoon", ""),
                ("ophaalmoment", slot),
                ("donatie", donation),
            ],
            false,
            true,
        )
        .await
    }
}

async fn allocation(pool: &PgPool, order: OrderId) -> (String, Option<i64>) {
    sqlx::query_as("SELECT status, charity_id FROM donation_allocation WHERE order_id = $1")
        .bind(order.get())
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_donation_saved_at_checkout_is_chosen_later_through_its_link(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    company_details(&mut conn).await;
    let voedselbank = charity(&mut conn, "Voedselbank").await;
    let hospice = charity(&mut conn, "Hospice").await;
    let archived = charity(&mut conn, "Oud doel").await;
    sqlx::query("UPDATE charity SET archived_at = now() WHERE id = $1")
        .bind(archived.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(Some(voedselbank), true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;

    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "2", None).await;
    let form_page = browser
        .get(&format!("/drops/{}/afrekenen", drop.day.get()))
        .await;
    assert!(
        form_page.body.contains("name=\"donatie\""),
        "{}",
        form_page.body
    );
    assert!(form_page.body.contains("<option value=\"drop\" selected>"));
    assert!(form_page.body.contains(&format!(
        "<option value=\"{}\">Hospice</option>",
        hospice.get()
    )));
    assert!(form_page.body.contains("<option value=\"bewaren\">"));
    assert!(
        !form_page.body.contains("Oud doel"),
        "archived charities are not offered"
    );
    assert!(
        form_page.body.contains("3 maanden"),
        "the customer is told up front"
    );

    let refused = browser
        .checkout_with_donation(drop.day, &drop.slot, &archived.get().to_string())
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(refused.body.contains(&dutch("checkout-error-donation")));
    assert!(
        sqlx::query_scalar::<_, i64>(r#"SELECT count(*) FROM "order""#)
            .fetch_one(&shop.pool)
            .await
            .unwrap()
            == 0,
        "no order and no hold for a refused choice"
    );

    let reply = browser
        .checkout_with_donation(drop.day, &drop.slot, "bewaren")
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    shop.mollie
        .pay(&ProviderPaymentId::new("tr_fake1").unwrap());
    assert_eq!(webhook(&shop, "id=tr_fake1").await.status, StatusCode::OK);
    let order = only_order(&shop.pool).await;
    assert_eq!(
        allocation(&shop.pool, order.id).await,
        ("saved".to_owned(), None)
    );

    let queued = outbox(&shop.pool).await;
    let text = &queued[0].text_body;
    let start = text
        .find(&format!("{PUBLIC_URL}/donatie/kies/"))
        .expect("choose link")
        + PUBLIC_URL.len();
    let link = text[start..start + "/donatie/kies/".len() + 43].to_owned();

    let (chooser, page) = Browser::open(&shop, &link).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(
        page.header(axum::http::header::CACHE_CONTROL),
        "private, no-store"
    );
    assert!(page.body.contains(&dutch("donation-choose-title")));
    assert!(page.body.contains(&order.number.to_string()));
    assert!(page.body.contains(&format!(
        "<option value=\"{}\">Hospice</option>",
        hospice.get()
    )));

    let hospice_id = hospice.get().to_string();
    let forged = chooser
        .post(&link, &[("goed-doel", &hospice_id)], false, false)
        .await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    assert_eq!(
        allocation(&shop.pool, order.id).await,
        ("saved".to_owned(), None)
    );

    let unknown = chooser
        .post(&link, &[("goed-doel", "nee")], false, true)
        .await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(unknown.body.contains(&dutch("donation-choose-error")));

    let chosen = chooser
        .post(&link, &[("goed-doel", &hospice_id)], false, true)
        .await;
    assert_eq!(chosen.status, StatusCode::OK, "{}", chosen.body);
    assert!(chosen.body.contains(&dutch("donation-chosen-title")));
    assert_eq!(
        allocation(&shop.pool, order.id).await,
        ("earmarked".to_owned(), Some(hospice.get()))
    );

    let used = chooser.get(&link).await;
    assert_eq!(used.status, StatusCode::NOT_FOUND);
    assert!(used.body.contains(&dutch("donation-link-failed-title")));
    let junk = chooser.get("/donatie/kies/kapot").await;
    assert_eq!(junk.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_normal_sale_asks_nothing_about_donations(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    let (browser, _) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    browser.add(drop.day, bread, "1", None).await;
    let page = browser
        .get(&format!("/drops/{}/afrekenen", drop.day.get()))
        .await;
    assert!(!page.body.contains("name=\"donatie\""));
    let reply = browser
        .checkout_with_donation(drop.day, &drop.slot, "bewaren")
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "the field is ignored");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_computed_ingredient_list_shows_allergens_in_bold(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    for change in [
        "UPDATE product SET ingredient_names = '{tarwebloem,water,zout}',
            ingredient_emphasis = '{t,f,f}', ingredient_text = 'tarwebloem, water, zout'",
        "UPDATE product SET allergens_approved_by = (SELECT id FROM staff_user LIMIT 1),
            allergens_approved_at = now()",
    ] {
        sqlx::query(change).execute(&mut *conn).await.unwrap();
    }
    let (_, reply) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    assert!(
        reply
            .body
            .contains("<strong>tarwebloem</strong>, water, zout"),
        "{}",
        reply.body
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_compound_ingredient_shows_its_parts_in_brackets(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let drop = open_drop(&mut conn, &shop.clock, &saturday(None, true)).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, drop.day, bread, None).await;
    for change in [
        "UPDATE product SET ingredient_names = '{tarwebloem,vulling,suiker,boter,zout}',
            ingredient_emphasis = '{t,f,f,t,f}', ingredient_depth = '{0,0,1,1,0}',
            ingredient_text = 'tarwebloem, vulling (suiker, boter), zout'",
        "UPDATE product SET allergens_approved_by = (SELECT id FROM staff_user LIMIT 1),
            allergens_approved_at = now()",
    ] {
        sqlx::query(change).execute(&mut *conn).await.unwrap();
    }
    let (_, reply) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    assert!(
        reply.body.contains(
            "<strong>tarwebloem</strong>, vulling (suiker, <strong>boter</strong>), zout"
        ),
        "{}",
        reply.body
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_drop_page_is_titled_bread_drop_and_its_date(pool: PgPool) {
    let shop = shop(pool);
    let drop = {
        let mut conn = shop.pool.acquire().await.unwrap();
        open_drop(&mut conn, &shop.clock, &saturday(None, true)).await
    };
    let (_, reply) = Browser::open(&shop, &format!("/drops/{}", drop.day.get())).await;
    assert!(
        reply
            .body
            .contains("<title>Bread Drop zaterdag 31 oktober 2026 ·"),
        "{}",
        reply.body
    );
    assert!(
        reply
            .body
            .contains("<h1>Bread Drop zaterdag 31 oktober 2026</h1>"),
        "{}",
        reply.body
    );
}
