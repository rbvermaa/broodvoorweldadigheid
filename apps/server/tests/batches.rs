//! The back-office screens for batches (F14a).
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
use axum::http::{HeaderMap, Request, StatusCode};
use bakedays::{BakeDayProduct, ExtraInput, NewBakeDay, Purpose, days, extras, range};
use db::PgPool;
use domain::{BakeDayId, BatchId, Clock, IngredientId, PickupLocationId, ProductId};
use i18n::{FluentArgs, Translator};
use orders::Orders;
use production::batches::{self, Status};
use recipes::{
    DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput, RecipeKind,
    ingredients, recipes as store,
};
use server::AppState;
use server::config::Config;
use support::signed_in_at;
use testkit::FakeClock;
use tower::ServiceExt;

const NOW: &str = "2026-12-01T10:00:00Z";

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

    fn location(&self) -> &str {
        self.headers
            .get(LOCATION)
            .map(|v| v.to_str().expect("ascii"))
            .unwrap_or_default()
    }

    #[track_caller]
    fn redirects_to(&self, location: &str) {
        assert_eq!(self.status, StatusCode::SEE_OTHER, "{}", self.text);
        assert_eq!(self.location(), location);
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
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("body");
        Reply {
            status,
            headers,
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

    fn body(fields: &[(&str, &str)]) -> String {
        fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&")
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let mut all: Vec<(&str, &str)> = vec![("_csrf", &self.token)];
        all.extend_from_slice(fields);
        self.post_raw(path, &all).await
    }

    async fn post_raw(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        self.send(
            Request::post(path)
                .header(COOKIE, &self.cookie)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(Self::body(fields)))
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

impl Client<'_> {
    async fn post_htmx(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let mut all: Vec<(&str, &str)> = vec![("_csrf", &self.token)];
        all.extend_from_slice(fields);
        self.send(
            Request::post(path)
                .header(COOKIE, &self.cookie)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("hx-request", "true")
                .body(Body::from(Self::body(&all)))
                .expect("request"),
        )
        .await
    }
}

struct Bakery {
    day: BakeDayId,
    bread: ProductId,
    rye: IngredientId,
}

async fn ingredient(pool: &PgPool, name: &str, role: IngredientRole) -> IngredientId {
    let mut conn = pool.acquire().await.expect("conn");
    ingredients::create(
        &mut conn,
        &IngredientInput {
            name: name.to_owned(),
            label_name: String::new(),
            role,
            hydration: None,
            supplier_name: String::new(),
            allergens: BTreeSet::new(),
        },
    )
    .await
    .expect("ingredient")
    .id
}

/// Saturday 5 December 2026 with 25 extra breads of 900 g dough from a 100/75/2 recipe.
async fn bakery(pool: &PgPool, clock: &FakeClock) -> Bakery {
    let flour = ingredient(pool, "Tarwebloem", IngredientRole::Flour).await;
    let water = ingredient(pool, "Water", IngredientRole::Water).await;
    let salt = ingredient(pool, "Zeezout", IngredientRole::Other).await;
    let rye = ingredient(pool, "Roggebloem", IngredientRole::Flour).await;
    let mut conn = pool.acquire().await.expect("conn");
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
         VALUES ('Desem wit', 450, 9, 800, 900, true) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .expect("product");
    let bread = ProductId::new(bread);
    store::link_product(&mut conn, recipe.id, bread)
        .await
        .expect("link");
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
    range::set_product(
        &mut conn,
        day,
        &BakeDayProduct {
            product_id: bread,
            max_qty: None,
        },
    )
    .await
    .expect("range");
    extras::add(
        &mut conn,
        day,
        bread,
        &ExtraInput {
            qty: 25,
            purpose: Purpose::Freezer,
            note: None,
        },
        &Orders::new(clock),
    )
    .await
    .expect("extra");
    Bakery { day, bread, rye }
}

fn list_path(day: BakeDayId) -> String {
    format!("/staff/bake-days/{}/batches", day.get())
}

fn batch_path(day: BakeDayId, batch: BatchId) -> String {
    format!("{}/{}", list_path(day), batch.get())
}

async fn manual_batch(pool: &PgPool, b: &Bakery, clock: &FakeClock) -> BatchId {
    let mut conn = pool.acquire().await.expect("conn");
    batches::start_manual(
        &mut conn,
        b.day,
        b.bread,
        10,
        orders::audit::Actor::System("test"),
        clock,
    )
    .await
    .expect("batch")
}

async fn stored(pool: &PgPool, id: BatchId) -> batches::Batch {
    let mut conn = pool.acquire().await.expect("conn");
    batches::get(&mut conn, id).await.expect("batch")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn counter_staff_cannot_see_or_change_batches(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool, &app.clock).await;
    let id = manual_batch(&pool, &b, &app.clock).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let list = list_path(b.day);
    let batch = batch_path(b.day, id);
    for path in [list.clone(), batch.clone()] {
        let reply = counter.get(&path).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
        reply.shows(&dutch("staff-forbidden-text"));
    }
    for path in [
        format!("{list}/new"),
        batch.clone(),
        format!("{batch}/preview"),
        format!("{batch}/lines"),
        format!("{batch}/mixed"),
        format!("{batch}/actuals"),
        format!("{batch}/promote"),
    ] {
        let reply = counter.post(&path, &[("pieces", "10")]).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(stored(&pool, id).await.status, Status::Planned);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn changes_need_the_csrf_token(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool, &app.clock).await;
    let id = manual_batch(&pool, &b, &app.clock).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let list = list_path(b.day);
    let batch = batch_path(b.day, id);
    for path in [
        format!("{list}/new"),
        batch.clone(),
        format!("{batch}/preview"),
        format!("{batch}/lines"),
        format!("{batch}/mixed"),
        format!("{batch}/actuals"),
        format!("{batch}/promote"),
    ] {
        let reply = baker.post_raw(&path, &[("pieces", "10")]).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(stored(&pool, id).await.status, Status::Planned);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_batches_page_lists_pieces_still_in_no_batch(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool, &app.clock).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let list = list_path(b.day);

    let day_page = baker
        .get(&format!("/staff/bake-days/{}", b.day.get()))
        .await;
    day_page.shows(&format!("href=\"{list}\""));

    let page = baker.get(&list).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows("25 stuks Desem wit zitten nog niet in een batch.");
    page.lacks("Maak batches");

    baker
        .post(
            &format!("{list}/new"),
            &[("product_id", &b.bread.get().to_string()), ("pieces", "25")],
        )
        .await;
    let page = baker.get(&list).await;
    page.lacks("zitten nog niet in een batch");
    page.shows("261205-1");

    let mut conn = pool.acquire().await.expect("conn");
    let list = batches::list(&mut conn, b.day).await.expect("list");
    assert_eq!(list.len(), 1);
    let page = baker.get(&batch_path(b.day, list[0].id)).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows("Batch 261205-1");
    page.shows(&dutch_with("batch-lines-caption", &[("grams", "22.500")]));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn grams_recalculate_on_the_server_without_saving(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool, &app.clock).await;
    let id = manual_batch(&pool, &b, &app.clock).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let batch = stored(&pool, id).await;
    let path = batch_path(b.day, id);
    let page = baker.get(&path).await;
    page.shows(&format!("hx-post=\"{path}/preview\""));
    page.shows(&dutch_with("batch-lines-caption", &[("grams", "9.000")]));

    let pct = |i: usize| format!("pct-{}", batch.lines[i].id.get());
    let pieces = format!("pieces-{}", b.bread.get());
    let (p0, p1, p2) = (pct(0), pct(1), pct(2));
    let fields = [
        (p0.as_str(), "100"),
        (p1.as_str(), "70"),
        (p2.as_str(), "2"),
        (pieces.as_str(), "12"),
    ];
    let reply = baker.post_htmx(&format!("{path}/preview"), &fields).await;
    assert_eq!(reply.status, StatusCode::OK);
    reply.shows("id=\"grams\"");
    reply.shows(&dutch_with("batch-lines-caption", &[("grams", "10.800")]));
    reply.shows(&dutch_with("batch-line-changed", &[("name", "Water")]));
    // 10 800 g over 172%: flour 6 279, water 4 395, salt 126.
    reply.shows("6.279");
    reply.shows("4.395");
    let unchanged = stored(&pool, id).await;
    assert_eq!(unchanged, batch, "a preview saves nothing");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn baker_adjusts_mixes_and_records_the_bake(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool, &app.clock).await;
    let id = manual_batch(&pool, &b, &app.clock).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let batch = stored(&pool, id).await;
    let path = batch_path(b.day, id);
    let pct = |i: usize| format!("pct-{}", batch.lines[i].id.get());
    let pieces = format!("pieces-{}", b.bread.get());

    let wrong = baker
        .post(
            &path,
            &[
                (pct(0).as_str(), "100"),
                (pct(1).as_str(), "veel"),
                (pct(2).as_str(), "2"),
                (pieces.as_str(), "10"),
            ],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch("batch-error-pct"));
    wrong.shows("value=\"veel\"");

    baker
        .post(
            &path,
            &[
                (pct(0).as_str(), "100"),
                (pct(1).as_str(), "72,5"),
                (pct(2).as_str(), "2"),
                (pieces.as_str(), "10"),
                ("reason", "nieuwe bloem"),
            ],
        )
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let adjusted = stored(&pool, id).await;
    assert_eq!(adjusted.status, Status::Adjusted);
    assert_eq!(adjusted.lines[1].actual_pct, Pct::from_thousandths(72_500));
    assert_eq!(adjusted.lines[1].reason.as_deref(), Some("nieuwe bloem"));
    let mut conn = pool.acquire().await.expect("conn");
    let history = orders::audit::history(&mut conn, "batch", id.get())
        .await
        .expect("history");
    assert!(history.last().expect("entry").actor.starts_with("staff:"));
    drop(conn);

    baker
        .post(
            &format!("{path}/lines"),
            &[
                ("ingredient", &b.rye.get().to_string()),
                ("add_pct", "10"),
                ("add_reason", ""),
            ],
        )
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let page = baker.get(&path).await;
    page.shows(&dutch_with("batch-line-added", &[("name", "Roggebloem")]));

    baker
        .post(&format!("{path}/mixed"), &[])
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let page = baker.get(&path).await;
    page.shows(&dutch("batch-locked"));
    page.lacks(&format!("hx-post=\"{path}/preview\""));
    let refused = baker
        .post(&path, &[(pct(1).as_str(), "60"), (pieces.as_str(), "10")])
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    refused.shows(&dutch("batch-locked"));
    let refused = baker.post(&format!("{path}/mixed"), &[]).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(
        stored(&pool, id).await.lines[1].actual_pct,
        Pct::from_thousandths(72_500)
    );

    let actual = format!("actual-{}", b.bread.get());
    let waste = format!("waste-{}", b.bread.get());
    let wrong = baker
        .post(
            &format!("{path}/actuals"),
            &[(actual.as_str(), "9"), (waste.as_str(), "10")],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch("batch-error-waste"));
    baker
        .post(
            &format!("{path}/actuals"),
            &[(actual.as_str(), "9"), (waste.as_str(), "1")],
        )
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let baked = stored(&pool, id).await;
    assert_eq!(baked.status, Status::Baked);
    assert_eq!(baked.outputs[0].actual_pieces, Some(9));
    assert_eq!(baked.outputs[0].waste_pieces, Some(1));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn baker_starts_a_batch_by_hand(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool, &app.clock).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let list = list_path(b.day);
    let page = baker.get(&list).await;
    page.shows(&format!("value=\"{}\"", b.bread.get()));
    let wrong = baker
        .post(
            &format!("{list}/new"),
            &[("product_id", &b.bread.get().to_string()), ("pieces", "0")],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch("batches-manual-error-pieces"));
    let reply = baker
        .post(
            &format!("{list}/new"),
            &[("product_id", &b.bread.get().to_string()), ("pieces", "40")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let mut conn = pool.acquire().await.expect("conn");
    let made = batches::list(&mut conn, b.day).await.expect("list");
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].dough_g(), 36_000);
    assert_eq!(
        reply.location(),
        format!("{}?opgeslagen=1", batch_path(b.day, made[0].id))
    );
    let other_day = baker
        .get(&format!(
            "/staff/bake-days/999/batches/{}",
            made[0].id.get()
        ))
        .await;
    assert_eq!(other_day.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn baker_saves_an_adjusted_batch_as_the_next_recipe_version(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool, &app.clock).await;
    let id = manual_batch(&pool, &b, &app.clock).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let batch = stored(&pool, id).await;
    let path = batch_path(b.day, id);
    let promote = format!("{path}/promote");

    let unchanged = owner.post(&promote, &[]).await;
    assert_eq!(unchanged.status, StatusCode::UNPROCESSABLE_ENTITY);
    unchanged.shows(&dutch("batch-promote-unchanged"));

    let pct = |i: usize| format!("pct-{}", batch.lines[i].id.get());
    let pieces = format!("pieces-{}", b.bread.get());
    owner
        .post(
            &path,
            &[
                (pct(0).as_str(), "100"),
                (pct(1).as_str(), "72,5"),
                (pct(2).as_str(), "2"),
                (pieces.as_str(), "10"),
                ("reason", "nieuwe bloem"),
            ],
        )
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    owner.get(&path).await.shows(&dutch("batch-promote"));

    let reply = owner.post(&promote, &[]).await;
    let mut conn = pool.acquire().await.expect("conn");
    let version = batches::promoted_version(&mut conn, id)
        .await
        .expect("read")
        .expect("promoted");
    drop(conn);
    let version_path = format!(
        "/staff/recipes/{}/versions/{}",
        batch.recipe_id.get(),
        version.id.get()
    );
    reply.redirects_to(&format!("{version_path}?opgeslagen=1"));
    assert_eq!(version.version, 2);
    assert!(version.notes.contains("nieuwe bloem"));
    owner.get(&version_path).await.shows("nieuwe bloem");

    owner
        .post(&promote, &[])
        .await
        .redirects_to(&format!("{version_path}?opgeslagen=1"));
    let page = owner.get(&path).await;
    page.shows(&dutch_with("batch-promoted", &[("n", "2")]));
    page.shows(&version_path);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_recipe_draft_blocks_saving_a_batch_as_a_version(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool, &app.clock).await;
    let id = manual_batch(&pool, &b, &app.clock).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let batch = stored(&pool, id).await;
    let mut conn = pool.acquire().await.expect("conn");
    batches::adjust(
        &mut conn,
        id,
        &batches::Adjustment {
            lines: vec![(batch.lines[1].id, Pct::from_thousandths(70_000))],
            ..batches::Adjustment::default()
        },
        orders::audit::Actor::System("test"),
        app.clock.as_ref(),
    )
    .await
    .expect("adjust");
    store::new_version(&mut conn, batch.recipe_id)
        .await
        .expect("draft");
    drop(conn);

    let reply = baker
        .post(&format!("{}/promote", batch_path(b.day, id)), &[])
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    reply.shows(&dutch_with("batch-promote-draft-exists", &[("n", "2")]));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_batch_page_shows_the_steps_of_its_version_with_times(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool, &app.clock).await;
    let mut conn = pool.acquire().await.expect("conn");
    let version: i64 =
        sqlx::query_scalar("SELECT current_version_id FROM recipe WHERE name = 'Desembrood'")
            .fetch_one(&mut *conn)
            .await
            .expect("version");
    let version = domain::RecipeVersionId::new(version);
    let seen = |m: &recipes::Method| recipes::method::stamp(m);
    let none = recipes::method::method(&mut conn, version)
        .await
        .expect("method");
    recipes::method::set_kind(&mut conn, version, &seen(&none), recipes::MethodKind::Steps)
        .await
        .expect("steps");
    for (name, minutes) in [("Autolyse", 30), ("Bulk", 240)] {
        let now = recipes::method::method(&mut conn, version)
            .await
            .expect("method");
        let step = recipes::Step {
            name: name.to_owned(),
            duration_min: Some(minutes),
            temperature_c: None,
            notes: String::new(),
        };
        recipes::method::edit_step(
            &mut conn,
            version,
            &seen(&now),
            recipes::StepEdit::Add(step),
        )
        .await
        .expect("step");
    }
    let id = manual_batch(&pool, &b, &app.clock).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = batch_path(b.day, id);

    let page = baker.get(&path).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch("method-title"));
    page.shows("<span class=\"method-time\">11:00</span>");
    page.shows("<span class=\"method-time\">11:30</span>");
    page.shows("<span class=\"method-time\">15:30</span>");
    page.lacks("action=\"/staff/recipes");

    let late = baker.get(&format!("{path}?start=22:00")).await;
    late.shows("<span class=\"method-time\">22:30</span>");
    late.shows(
        "<span class=\"method-time\">02:30 <span class=\"method-later\">+1 dag</span></span>",
    );
}
