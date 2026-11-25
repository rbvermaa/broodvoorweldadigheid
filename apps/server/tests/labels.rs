//! The back-office screens for product and freezer labels (F18, F18a): label settings, the
//! company address, freezer pieces and the label files of a batch.
#![forbid(unsafe_code)]

mod support;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use auth::{NewStaff, Role};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE, COOKIE, LOCATION};
use axum::http::{HeaderMap, Request, StatusCode};
use bakedays::{NewBakeDay, days};
use catalog::label_settings::{self, LabelSettings};
use catalog::{ComputedAllergens, IngredientList, ListedIngredient, company, products};
use db::PgPool;
use domain::{
    Allergen, BakeDayId, BatchId, Clock, IngredientId, PickupLocationId, ProductId, StaffUserId,
};
use i18n::{FluentArgs, Translator};
use orders::audit::Actor;
use production::batches::{self, Actual};
use recipes::{
    DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput, RecipeKind,
    ingredients, recipes as store, rollup,
};
use server::AppState;
use server::config::Config;
use support::signed_in_at;
use testkit::FakeClock;
use tower::ServiceExt;

const NOW: &str = "2026-12-05T15:00:00Z";
const GLUTEN: Allergen = Allergen::CerealsContainingGluten;
const SYSTEM: Actor = Actor::System("test");

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
    dutch_with(id, &[])
}

fn dutch_with(id: &str, args: &[(&str, &str)]) -> String {
    let mut fluent = FluentArgs::new();
    for (name, value) in args {
        fluent.set(*name, *value);
    }
    let args = (!args.is_empty()).then_some(&fluent);
    Translator::dutch()
        .expect("Dutch texts")
        .t(id, args)
        .expect("message")
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
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

    #[track_caller]
    fn lacks(&self, text: &str) {
        assert!(!self.text.contains(text), "unexpected {text:?}");
    }

    #[track_caller]
    fn redirects_to(&self, location: &str) {
        assert_eq!(self.status, StatusCode::SEE_OTHER, "{}", self.text);
        let found = self
            .headers
            .get(LOCATION)
            .map(|v| v.to_str().expect("ascii"))
            .unwrap_or_default();
        assert_eq!(found, location);
    }

    fn header(&self, name: axum::http::HeaderName) -> &str {
        self.headers
            .get(name)
            .map(|v| v.to_str().expect("ascii"))
            .unwrap_or_default()
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
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .expect("body")
            .to_vec();
        let text = decode(&String::from_utf8_lossy(&bytes));
        Reply {
            status,
            headers,
            bytes,
            text,
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
        let body = all
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

struct Bakery {
    day: BakeDayId,
    bread: ProductId,
    spelt: IngredientId,
}

async fn ingredient(
    pool: &PgPool,
    name: &str,
    role: IngredientRole,
    allergens: &[Allergen],
) -> IngredientId {
    let mut conn = pool.acquire().await.expect("conn");
    ingredients::create(
        &mut conn,
        &IngredientInput {
            name: name.to_owned(),
            label_name: name.to_lowercase(),
            role,
            supplier_name: String::new(),
            allergens: allergens.iter().copied().collect(),
        },
    )
    .await
    .expect("ingredient")
    .id
}

/// A bread of 100 wheat flour, 75 water and 2 salt on 5 December 2026 with approved
/// computed allergens, an approved empty kitchen list, an address and label settings.
async fn bakery(app: &App) -> Bakery {
    let pool = &app.pool;
    let approver = auth::staff::add(
        pool,
        &NewStaff {
            oidc_sub: "sub-approver".to_owned(),
            name: "Goedkeurder".to_owned(),
            email: None,
            role: Role::Baker,
        },
    )
    .await
    .expect("approver");
    let approver: StaffUserId = approver.id;
    let flour = ingredient(pool, "Tarwebloem", IngredientRole::Flour, &[GLUTEN]).await;
    let water = ingredient(pool, "Water", IngredientRole::Water, &[]).await;
    let salt = ingredient(pool, "Zout", IngredientRole::Other, &[]).await;
    let spelt = ingredient(pool, "Speltbloem", IngredientRole::Flour, &[GLUTEN]).await;
    let mut conn = pool.acquire().await.expect("conn");
    let clock = app.clock.as_ref();
    company::approve_kitchen_may_contain(&mut conn, &BTreeSet::new(), approver, clock)
        .await
        .expect("kitchen");
    company::set_address(&mut conn, "Voorbeeldlaan 1, 1234 AB Voorbeeld")
        .await
        .expect("address");
    let (recipe, version) = store::create(
        &mut conn,
        &RecipeInput {
            name: "Desembrood".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .expect("recipe");
    let line = |pct: u32, id| LineInput {
        pct: Pct::from_thousandths(pct * 1000),
        part: LinePart::Ingredient(id),
    };
    store::save_draft(
        &mut conn,
        version.id,
        &DraftInput {
            lines: vec![line(100, flour), line(75, water), line(2, salt)],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("draft");
    let bread: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online)
         VALUES ('Desem wit', 450, 9, 800, 900, false) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .expect("product");
    let bread = ProductId::new(bread);
    store::link_product(&mut conn, recipe.id, bread)
        .await
        .expect("link");
    let revision = products::get(&mut conn, bread)
        .await
        .expect("product")
        .allergen_revision;
    let listed = |name: &str, emphasised| ListedIngredient {
        name: name.to_owned(),
        emphasised,
    };
    rollup::approve_product(
        &mut conn,
        bread,
        &ComputedAllergens {
            allergens: BTreeSet::from([GLUTEN]),
            ingredients: IngredientList(vec![
                listed("tarwebloem", true),
                listed("water", false),
                listed("zout", false),
            ]),
        },
        revision,
        approver,
        clock,
    )
    .await
    .expect("approve");
    label_settings::set(
        &mut conn,
        bread,
        &LabelSettings {
            packaged: true,
            shelf_life_days: Some(4),
            freezer_days: Some(90),
            storage: "Koel en droog bewaren.".to_owned(),
        },
    )
    .await
    .expect("settings");
    let day = days::create(
        &mut conn,
        &NewBakeDay {
            date: jiff::civil::date(2026, 12, 5),
            location_id: PickupLocationId::new(1),
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .expect("day")
    .id;
    Bakery { day, bread, spelt }
}

async fn baked_batch(app: &App, b: &Bakery, spelt: bool) -> BatchId {
    let mut conn = app.pool.acquire().await.expect("conn");
    let clock = app.clock.as_ref();
    let id = batches::start_manual(&mut conn, b.day, b.bread, 10, SYSTEM, clock)
        .await
        .expect("batch");
    if spelt {
        batches::add_line(
            &mut conn,
            id,
            b.spelt,
            Pct::from_thousandths(10_000),
            None,
            SYSTEM,
            clock,
        )
        .await
        .expect("spelt");
    }
    batches::mark_mixed(&mut conn, id, SYSTEM, clock)
        .await
        .expect("mixed");
    batches::record_actuals(
        &mut conn,
        id,
        &[Actual {
            product_id: b.bread,
            pieces: 10,
            waste: 2,
        }],
        SYSTEM,
        clock,
    )
    .await
    .expect("actuals");
    id
}

fn labels_path(b: &Bakery, batch: BatchId) -> String {
    format!(
        "/staff/bake-days/{}/batches/{}/etiketten",
        b.day.get(),
        batch.get()
    )
}

fn file_path(b: &Bakery, batch: BatchId, kind: &str, file: &str) -> String {
    format!("{}/{}/{kind}/{file}", labels_path(b, batch), b.bread.get())
}

fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "a PNG");
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    (word(16), word(20))
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_baker_downloads_the_product_label_as_png_and_csv(pool: PgPool) {
    let app = app(pool);
    let b = bakery(&app).await;
    let batch = baked_batch(&app, &b, false).await;
    let baker = client(&app, "Bakker", Role::Baker).await;

    let batch_page = baker
        .get(&format!(
            "/staff/bake-days/{}/batches/{}",
            b.day.get(),
            batch.get()
        ))
        .await;
    batch_page.shows(&labels_path(&b, batch));
    batch_page.shows(&dutch("batch-labels-link"));

    let page = baker.get(&labels_path(&b, batch)).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch_with("batch-labels-title", &[("code", "261205-1")]));
    page.shows(&dutch_with("batch-labels-copies", &[("n", "8")]));
    page.shows(&dutch_with(
        "batch-labels-file-whole",
        &[("width", "50"), ("length", "30")],
    ));
    page.shows(&file_path(&b, batch, "product", "1.png"));
    page.shows(&dutch("batch-labels-gap-no-frozen"));

    let png = baker.get(&file_path(&b, batch, "product", "1.png")).await;
    assert_eq!(png.status, StatusCode::OK);
    assert_eq!(png.header(CONTENT_TYPE), "image/png");
    assert_eq!(
        png.header(CONTENT_DISPOSITION),
        "attachment; filename=\"261205-1-product-1.png\""
    );
    assert_eq!(png_size(&png.bytes), (591, 354));

    let csv = baker
        .get(&file_path(&b, batch, "product", "etiketten.csv"))
        .await;
    assert_eq!(csv.status, StatusCode::OK);
    assert_eq!(csv.header(CONTENT_TYPE), "text/csv; charset=utf-8");
    csv.shows("261205-1-product-1.png;Productetiket;Desem wit;TARWEBLOEM, water, zout;");
    csv.shows(";09-12-2026;");

    for missing in ["2.png", "0.png", "x.png", "etiket.pdf"] {
        let reply = baker.get(&file_path(&b, batch, "product", missing)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{missing}");
    }
    let unknown = baker.get(&file_path(&b, batch, "anders", "1.png")).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn counter_staff_get_no_labels(pool: PgPool) {
    let app = app(pool);
    let b = bakery(&app).await;
    let batch = baked_batch(&app, &b, false).await;
    let counter = client(&app, "Balie", Role::Counter).await;
    for path in [
        labels_path(&b, batch),
        file_path(&b, batch, "product", "1.png"),
        file_path(&b, batch, "product", "etiketten.csv"),
        format!("/staff/products/{}/etiket", b.bread.get()),
    ] {
        assert_eq!(
            counter.get(&path).await.status,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    let post = counter
        .post(
            &format!("{}/{}/vriezer", labels_path(&b, batch), b.bread.get()),
            &[("frozen", "1")],
        )
        .await;
    assert_eq!(post.status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_blocked_batch_shows_why_and_gives_no_file(pool: PgPool) {
    let app = app(pool);
    let b = bakery(&app).await;
    let batch = baked_batch(&app, &b, true).await;
    let baker = client(&app, "Bakker", Role::Baker).await;

    let page = baker.get(&labels_path(&b, batch)).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch("label-check-differs"));
    page.lacks(&file_path(&b, batch, "product", "1.png"));

    for file in ["1.png", "etiketten.csv"] {
        let reply = baker.get(&file_path(&b, batch, "product", file)).await;
        assert_eq!(reply.status, StatusCode::CONFLICT, "{file}");
        assert_eq!(reply.header(CONTENT_TYPE), "text/html; charset=utf-8");
        reply.shows(&dutch("label-check-differs"));
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn pieces_go_into_the_freezer_and_get_freezer_labels(pool: PgPool) {
    let app = app(pool);
    let b = bakery(&app).await;
    let batch = baked_batch(&app, &b, false).await;
    let baker = client(&app, "Bakker", Role::Baker).await;
    let action = format!("{}/{}/vriezer", labels_path(&b, batch), b.bread.get());

    let too_many = baker.post(&action, &[("frozen", "9")]).await;
    assert_eq!(too_many.status, StatusCode::UNPROCESSABLE_ENTITY);
    too_many.shows(&dutch("batch-labels-error-frozen"));
    let not_a_number = baker.post(&action, &[("frozen", "veel")]).await;
    assert_eq!(not_a_number.status, StatusCode::UNPROCESSABLE_ENTITY);

    baker
        .post(&action, &[("frozen", "3")])
        .await
        .redirects_to(&format!("{}?opgeslagen=1", labels_path(&b, batch)));
    let page = baker.get(&labels_path(&b, batch)).await;
    page.shows(&dutch_with("batch-labels-copies", &[("n", "3")]));
    page.shows(&file_path(&b, batch, "vriezer", "1.png"));

    let csv = baker
        .get(&file_path(&b, batch, "vriezer", "etiketten.csv"))
        .await;
    csv.shows("261205-1-vriezer-1.png;Vriezeretiket;Desem wit;");
    csv.shows(";05-03-2027;05-12-2026;");
    let png = baker.get(&file_path(&b, batch, "vriezer", "1.png")).await;
    assert_eq!(png_size(&png.bytes), (591, 354));

    // Fewer good pieces than are frozen is refused on the batch page.
    let batch_path = format!("/staff/bake-days/{}/batches/{}", b.day.get(), batch.get());
    let fewer = baker
        .post(
            &format!("{batch_path}/actuals"),
            &[
                (&format!("actual-{}", b.bread.get()), "10"),
                (&format!("waste-{}", b.bread.get()), "8"),
            ],
        )
        .await;
    assert_eq!(fewer.status, StatusCode::UNPROCESSABLE_ENTITY);
    fewer.shows(&dutch("batch-labels-error-fewer-than-frozen"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_unbaked_batch_has_no_freezer_form(pool: PgPool) {
    let app = app(pool);
    let b = bakery(&app).await;
    let mut conn = app.pool.acquire().await.expect("conn");
    let batch = batches::start_manual(&mut conn, b.day, b.bread, 10, SYSTEM, app.clock.as_ref())
        .await
        .expect("batch");
    drop(conn);
    let baker = client(&app, "Bakker", Role::Baker).await;
    let page = baker.get(&labels_path(&b, batch)).await;
    page.shows(&dutch("batch-labels-gap-not-baked"));
    page.lacks("name=\"frozen\"");
    let post = baker
        .post(
            &format!("{}/{}/vriezer", labels_path(&b, batch), b.bread.get()),
            &[("frozen", "1")],
        )
        .await;
    assert_eq!(post.status, StatusCode::UNPROCESSABLE_ENTITY);
    post.shows(&dutch("batch-labels-gap-not-baked"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_label_settings_of_a_product_are_edited(pool: PgPool) {
    let app = app(pool);
    let b = bakery(&app).await;
    let baker = client(&app, "Bakker", Role::Baker).await;
    let path = format!("/staff/products/{}/etiket", b.bread.get());

    let product = baker
        .get(&format!("/staff/products/{}", b.bread.get()))
        .await;
    product.shows(&path);

    let page = baker.get(&path).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch_with(
        "product-labels-title",
        &[("product", "Desem wit")],
    ));
    page.shows("value=\"Koel en droog bewaren.\"");

    let wrong = baker
        .post(
            &path,
            &[
                ("shelf_life_days", "0"),
                ("freezer_days", "veel"),
                ("storage", &"x".repeat(201)),
            ],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch_with(
        "product-labels-days-invalid",
        &[("max", "3650")],
    ));
    wrong.shows(&dutch_with(
        "product-labels-storage-too-long",
        &[("max", "200")],
    ));

    baker
        .post(
            &path,
            &[
                ("packaged", "on"),
                ("shelf_life_days", "14"),
                ("freezer_days", ""),
                ("storage", "Droog bewaren."),
            ],
        )
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let mut conn = app.pool.acquire().await.expect("conn");
    assert_eq!(
        label_settings::get(&mut conn, b.bread)
            .await
            .expect("settings"),
        LabelSettings {
            packaged: true,
            shelf_life_days: Some(14),
            freezer_days: None,
            storage: "Droog bewaren.".to_owned(),
        }
    );
    let missing = baker.get("/staff/products/999999/etiket").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_owner_sets_the_address_for_labels(pool: PgPool) {
    let app = app(pool);
    let owner = client(&app, "Eigenaar", Role::Owner).await;
    owner
        .post(
            "/staff/settings/address",
            &[("address", " Dorpsweg 2, Ergens ")],
        )
        .await
        .redirects_to("/staff/settings?opgeslagen=1");
    owner
        .get("/staff/settings")
        .await
        .shows("value=\"Dorpsweg 2, Ergens\"");
    let long = owner
        .post("/staff/settings/address", &[("address", &"x".repeat(201))])
        .await;
    assert_eq!(long.status, StatusCode::UNPROCESSABLE_ENTITY);
    long.shows(&dutch_with("settings-address-too-long", &[("max", "200")]));
    let baker = client(&app, "Bakker", Role::Baker).await;
    let refused = baker
        .post("/staff/settings/address", &[("address", "Elders")])
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
}
