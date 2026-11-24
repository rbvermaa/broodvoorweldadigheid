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
        bake_starts_at: ts("2026-10-31T03:00:00Z"),
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
            bake_starts_at: day.bake_starts_at,
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
        "INSERT INTO product (name_nl, description_nl, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, sold_online, allergens, ingredient_text,
            allergens_approved_by, allergens_approved_at)
         VALUES ($1, 'Lekker brood', $2, 9, $3, $4, $5, 800, 900, $6, $7, $8,
            CASE WHEN $9 THEN 1 END, CASE WHEN $9 THEN timestamptz '2026-10-01T10:00:00Z' END)
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
            capacity_pool_id: None,
            units_per_item: 1,
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
            bake_starts_at: ts("2026-11-07T04:00:00Z"),
            ..saturday(None, true)
        },
    )
    .await
    .unwrap();
    let production_only = days::create(
        &mut conn,
        &NewBakeDay {
            date: "2026-11-14".parse().unwrap(),
            bake_starts_at: ts("2026-11-14T04:00:00Z"),
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
            bake_starts_at: ts("2026-11-07T04:00:00Z"),
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
