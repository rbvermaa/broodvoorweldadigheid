//! The costing screens (F16): ingredient prices, product cost and batch cost.
#![forbid(unsafe_code)]

mod support;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use auth::{NewStaff, Role};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_TYPE, COOKIE, LOCATION};
use axum::http::{Request, StatusCode};
use bakedays::{NewBakeDay, days};
use db::PgPool;
use domain::{BatchId, Clock, IngredientId, Money, PickupLocationId, ProductId, StaffUserId};
use i18n::Translator;
use jiff::civil::date;
use orders::audit::Actor;
use production::batches::{self, Actual};
use recipes::prices::{self, NewPrice};
use recipes::{
    DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput, RecipeKind,
    ingredients, recipes as store,
};
use server::AppState;
use server::config::Config;
use support::signed_in_at;
use testkit::FakeClock;
use tower::ServiceExt;

const NOW: &str = "2026-12-06T10:00:00Z";
const BAKER: Actor = Actor::Staff(StaffUserId::new(1));

struct App {
    router: Router,
    pool: PgPool,
    clock: Arc<FakeClock>,
}

fn app(pool: PgPool) -> App {
    let config = Config::from_lookup(|key| {
        (key == "DATABASE_URL").then(|| "postgres://unused/db".to_string())
    })
    .expect("config");
    let clock = Arc::new(FakeClock::new(NOW.parse().expect("now")));
    let state = AppState::new(
        pool.clone(),
        Translator::dutch().expect("Dutch texts"),
        config,
    )
    .with_clock(clock.clone());
    App {
        router: server::router(state),
        pool,
        clock,
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
    location: String,
    text: String,
}

impl Reply {
    #[track_caller]
    fn shows(&self, text: &str) {
        assert!(
            self.text.contains(text),
            "expected {text:?} in:\n{}",
            self.text
        );
    }
}

fn decode(html: &str) -> String {
    html.replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#38;", "&")
        .replace("&amp;", "&")
        .replace('\u{a0}', " ")
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_owned(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

struct Client<'a> {
    app: &'a App,
    cookie: String,
    token: String,
}

impl Client<'_> {
    async fn send(&self, mut request: Request<Body>) -> Reply {
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.10:5000".parse::<SocketAddr>().expect("addr"),
        ));
        let response = self
            .app
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let location = response
            .headers()
            .get(LOCATION)
            .map(|v| v.to_str().expect("ascii").to_owned())
            .unwrap_or_default();
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("body");
        Reply {
            status,
            location,
            text: decode(std::str::from_utf8(&bytes).expect("utf-8")),
        }
    }

    async fn get(&self, path: &str) -> Reply {
        self.send(
            Request::get(path)
                .header(COOKIE, &self.cookie)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let mut all: Vec<(&str, &str)> = vec![("_csrf", &self.token)];
        all.extend_from_slice(fields);
        self.post_raw(path, &all).await
    }

    async fn post_raw(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        self.send(
            Request::post(path)
                .header(COOKIE, &self.cookie)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
    }
}

async fn client<'a>(app: &'a App, name: &str, role: Role) -> Client<'a> {
    let user = auth::staff::add(
        &app.pool,
        &NewStaff {
            oidc_sub: format!("sub-{name}"),
            name: name.to_string(),
            email: None,
            role,
        },
    )
    .await
    .expect("add staff");
    let (cookie, token) = signed_in_at(&app.pool, &user, app.clock.now()).await;
    Client { app, cookie, token }
}

struct Kitchen {
    flour: IngredientId,
    bread: ProductId,
    batch: BatchId,
}

/// Bread from flour and water (100/80), 900 g a piece; one batch of 10 on Saturday
/// 5 December, 9 good. Flour costs €1,00 per kg from 1 December, water nothing.
async fn kitchen(pool: &PgPool) -> Kitchen {
    let mut conn = pool.acquire().await.expect("conn");
    let clock = FakeClock::new("2026-12-01T10:00:00Z".parse().expect("ts"));
    let mut ingredient = async |name: &str, role| {
        ingredients::create(
            &mut conn,
            &IngredientInput {
                name: name.to_owned(),
                label_name: String::new(),
                role,
                supplier_name: String::new(),
                allergens: BTreeSet::new(),
            },
        )
        .await
        .expect("ingredient")
        .id
    };
    let flour = ingredient("Tarwebloem", IngredientRole::Flour).await;
    let water = ingredient("Water", IngredientRole::Water).await;
    let (recipe, version) = store::create(
        &mut conn,
        &RecipeInput {
            name: "Desembrood".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .expect("recipe");
    let line = |percent: u32, id| LineInput {
        pct: Pct::from_thousandths(percent * 1000),
        part: LinePart::Ingredient(id),
    };
    store::save_draft(
        &mut conn,
        version.id,
        &DraftInput {
            lines: vec![line(100, flour), line(80, water)],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("draft");
    let bread: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online)
         VALUES ('Desem wit', 650, 9, 800, 900, true) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .expect("product");
    let bread = ProductId::new(bread);
    store::link_product(&mut conn, recipe.id, bread)
        .await
        .expect("link");
    for (id, cents) in [(flour, 100), (water, 0)] {
        prices::add(
            &mut conn,
            &clock,
            &NewPrice {
                ingredient_id: id,
                per_kg: Money::from_cents(cents),
                valid_from: date(2026, 12, 1),
                by: None,
            },
        )
        .await
        .expect("price");
    }
    let day = days::create(
        &mut conn,
        &NewBakeDay {
            date: date(2026, 12, 5),
            location_id: PickupLocationId::new(1),
            online_ordering: false,
            charity_id: None,
        },
    )
    .await
    .expect("day")
    .id;
    let batch = batches::start_manual(&mut conn, day, bread, 10, BAKER, &clock)
        .await
        .expect("batch");
    batches::mark_mixed(&mut conn, batch, BAKER, &clock)
        .await
        .expect("mixed");
    batches::record_actuals(
        &mut conn,
        batch,
        &[Actual {
            product_id: bread,
            pieces: 10,
            waste: 1,
        }],
        BAKER,
        &clock,
    )
    .await
    .expect("actuals");
    Kitchen {
        flour,
        bread,
        batch,
    }
}

fn paths(k: &Kitchen) -> [String; 4] {
    [
        "/staff/costing".to_owned(),
        format!("/staff/costing/ingredients/{}", k.flour.get()),
        format!("/staff/costing/products/{}", k.bread.get()),
        format!("/staff/costing/batches/{}", k.batch.get()),
    ]
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_the_owner_sees_costs(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let price = format!("/staff/costing/ingredients/{}", k.flour.get());
    for (name, role) in [
        ("Bakker Bas", Role::Baker),
        ("Toonbank Tineke", Role::Counter),
    ] {
        let staff = client(&app, name, role).await;
        for path in paths(&k) {
            let reply = staff.get(&path).await;
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{name} {path}");
            reply.shows(&dutch("staff-forbidden-text"));
        }
        let reply = staff
            .post(&price, &[("per_kg", "2"), ("valid_from", "2026-12-06")])
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{name}");
    }
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        prices::history(&mut conn, k.flour)
            .await
            .expect("history")
            .len(),
        1
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_price_needs_the_csrf_token(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let path = format!("/staff/costing/ingredients/{}", k.flour.get());
    let reply = owner
        .post_raw(&path, &[("per_kg", "2"), ("valid_from", "2026-12-06")])
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        prices::history(&mut conn, k.flour)
            .await
            .expect("history")
            .len(),
        1
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_owner_adds_a_price_and_keeps_the_history(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let path = format!("/staff/costing/ingredients/{}", k.flour.get());

    let page = owner.get(&path).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows("value=\"2026-12-06\"");
    page.shows("€ 1,00");

    let refused = owner
        .post(&path, &[("per_kg", "veel"), ("valid_from", "gisteren")])
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.shows(&dutch("costing-error-per-kg"));
    refused.shows(&dutch("costing-error-valid-from"));

    let reply = owner
        .post(&path, &[("per_kg", "1,25"), ("valid_from", "2026-12-06")])
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text);
    assert_eq!(reply.location, format!("{path}?opgeslagen=1"));
    let page = owner.get(&reply.location).await;
    page.shows("06-12-2026");
    page.shows("€ 1,25");
    page.shows("01-12-2026");

    let overview = owner.get("/staff/costing").await;
    assert_eq!(overview.status, StatusCode::OK);
    overview.shows(&dutch("costing-title"));
    overview.shows("Desem wit");
    // 500 g of flour at €1,25 per piece.
    overview.shows("€ 0,63");
    overview.shows("261205-1");

    let missing = owner.get("/staff/costing/ingredients/999").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn product_and_batch_pages_show_cost_and_variance(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;

    let product = owner
        .get(&format!("/staff/costing/products/{}", k.bread.get()))
        .await;
    assert_eq!(product.status, StatusCode::OK);
    product.shows("Desembrood (versie 1)");
    product.shows("500,0");
    product.shows("€ 0,50");

    let batch = owner
        .get(&format!("/staff/costing/batches/{}", k.batch.get()))
        .await;
    assert_eq!(batch.status, StatusCode::OK, "{}", batch.text);
    batch.shows("261205-1");
    // 5000 g flour: €5,00 for 9 good pieces.
    batch.shows("€ 5,00");
    batch.shows("€ 0,56");
    batch.shows(&dutch("costing-variance-yield"));
    // Standard for 9 good of 10: €4,50, so €0,50 yield.
    batch.shows("€ 4,50");

    for path in ["/staff/costing/products/999", "/staff/costing/batches/999"] {
        assert_eq!(
            owner.get(path).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
}
