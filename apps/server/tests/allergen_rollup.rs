//! The back-office screens for the allergen roll-up (F17): computed values on the product's
//! approve page and the label check of a batch.
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
use bakedays::{NewBakeDay, days};
use catalog::products;
use db::PgPool;
use domain::{Allergen, BakeDayId, BatchId, Clock, IngredientId, PickupLocationId, ProductId};
use i18n::{FluentArgs, Translator};
use production::batches::{self, Adjustment};
use production::label_check;
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
const GLUTEN: Allergen = Allergen::CerealsContainingGluten;

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

    /// Every `value` of the hidden inputs named `name`, in page order.
    fn hidden(&self, name: &str) -> Vec<String> {
        let marker = format!("name=\"{name}\" value=\"");
        self.text
            .split(&marker)
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap_or_default().to_owned())
            .collect()
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

    /// Posts the hidden fields of the form on `page` that posts to `action`, as shown.
    async fn post_shown(&self, page: &Reply, action: &str) -> Reply {
        let start = page
            .text
            .find(&format!("action=\"{action}\""))
            .expect("form on the page");
        let form = &page.text[start..];
        let form = &form[..form.find("</form>").expect("form end")];
        let fields: Vec<(String, String)> = form
            .split("<input type=\"hidden\" name=\"")
            .skip(1)
            .filter_map(|rest| {
                let (name, rest) = rest.split_once("\" value=\"")?;
                Some((name.to_owned(), rest.split('"').next()?.to_owned()))
            })
            .filter(|(name, _)| name != "_csrf")
            .collect();
        let pairs: Vec<(&str, &str)> = fields
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        self.post(action, &pairs).await
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

/// A bread of 100 wheat flour, 75 water and 2 salt, with hand-entered allergen data.
async fn bakery(pool: &PgPool) -> Bakery {
    let flour = ingredient(pool, "Tarwebloem", IngredientRole::Flour, &[GLUTEN]).await;
    let water = ingredient(pool, "Water", IngredientRole::Water, &[]).await;
    let salt = ingredient(pool, "Zout", IngredientRole::Other, &[]).await;
    let spelt = ingredient(pool, "Speltbloem", IngredientRole::Flour, &[GLUTEN]).await;
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
            dough_g_per_piece, sold_online, ingredient_text)
         VALUES ('Desem wit', 450, 9, 800, 900, true, 'BLOEM, water') RETURNING id",
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
    Bakery { day, bread, spelt }
}

fn product_path(id: ProductId) -> String {
    format!("/staff/products/{}", id.get())
}

async fn batch(pool: &PgPool, b: &Bakery, clock: &FakeClock) -> BatchId {
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

fn label_path(b: &Bakery, batch: BatchId) -> String {
    format!(
        "/staff/bake-days/{}/batches/{}/etiket/{}",
        b.day.get(),
        batch.get(),
        b.bread.get()
    )
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_approve_page_shows_computed_next_to_current_values(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let page = baker
        .get(&format!("{}/approve", product_path(b.bread)))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch("rollup-title"));
    page.shows(&dutch_with(
        "rollup-source",
        &[("recipe", "Desembrood"), ("n", "1")],
    ));
    page.shows("<strong>tarwebloem</strong>, water, zout");
    page.shows(&dutch("rollup-differs"));
    page.shows(&dutch("rollup-current"));
    page.shows("BLOEM, water");

    let product = products::get(&mut pool.acquire().await.unwrap(), b.bread)
        .await
        .unwrap();
    assert_eq!(product.approval, None, "showing approves nothing");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn approving_computed_values_makes_them_the_product_data(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = product_path(b.bread);
    let page = baker.get(&format!("{path}/approve")).await;
    let reply = baker
        .post_shown(&page, &format!("{path}/approve-computed"))
        .await;
    reply.redirects_to(&format!("{path}?opgeslagen=1"));

    let product = products::get(&mut pool.acquire().await.unwrap(), b.bread)
        .await
        .unwrap();
    assert!(product.approval.is_some());
    assert_eq!(product.allergen_info.allergens, BTreeSet::from([GLUTEN]));
    assert_eq!(
        product.allergen_info.ingredient_text,
        "tarwebloem, water, zout"
    );

    let again = baker.get(&format!("{path}/approve")).await;
    again.shows(&dutch("rollup-same"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_stale_or_tampered_computed_approval_is_refused(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = product_path(b.bread);
    let action = format!("{path}/approve-computed");
    let page = baker.get(&format!("{path}/approve")).await;

    let tampered = baker
        .post(
            &action,
            &[
                ("ingredient", "water"),
                ("vet", "0"),
                ("diepte", "0"),
                ("revisie", &page.hidden("revisie")[0]),
            ],
        )
        .await;
    assert_eq!(tampered.status, StatusCode::CONFLICT);
    tampered.shows(&dutch("approve-changed"));

    let unpaired = baker
        .post(
            &action,
            &[
                ("allergens", "cereals_containing_gluten"),
                ("ingredient", "tarwebloem"),
                ("revisie", &page.hidden("revisie")[0]),
            ],
        )
        .await;
    assert_eq!(unpaired.status, StatusCode::CONFLICT);

    // The recipe changes after the page was opened.
    sqlx::query(
        "UPDATE recipe_line SET bakers_pct_x1000 = 80000
         WHERE ingredient_id = (SELECT id FROM ingredient WHERE name = 'Water')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let stale = baker.post_shown(&page, &action).await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    stale.shows(&dutch("approve-changed"));
    let product = products::get(&mut pool.acquire().await.unwrap(), b.bread)
        .await
        .unwrap();
    assert_eq!(product.approval, None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_allergen_editors_with_a_token_see_or_approve(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool).await;
    let id = batch(&pool, &b, &app.clock).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let approve_computed = format!("{}/approve-computed", product_path(b.bread));
    let label = label_path(&b, id);
    let label_approve = format!("{label}/approve");

    assert_eq!(counter.get(&label).await.status, StatusCode::FORBIDDEN);
    for path in [&approve_computed, &label_approve] {
        let refused = counter.post(path, &[("revisie", "0")]).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{path}");
        let no_token = baker.post_raw(path, &[("revisie", "0")]).await;
        assert_eq!(no_token.status, StatusCode::FORBIDDEN, "{path}");
    }
    let product = products::get(&mut pool.acquire().await.unwrap(), b.bread)
        .await
        .unwrap();
    assert_eq!(product.approval, None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_swapped_flour_is_blocked_until_the_batch_is_approved(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let kitchen = owner.get("/staff/products/may-contain").await;
    assert_eq!(kitchen.status, StatusCode::OK);
    owner
        .post("/staff/products/may-contain/approve", &[])
        .await
        .redirects_to("/staff/products/may-contain?opgeslagen=1");
    let path = product_path(b.bread);
    let page = baker.get(&format!("{path}/approve")).await;
    baker
        .post_shown(&page, &format!("{path}/approve-computed"))
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));

    let id = batch(&pool, &b, &app.clock).await;
    let batch_page = baker
        .get(&format!(
            "/staff/bake-days/{}/batches/{}",
            b.day.get(),
            id.get()
        ))
        .await;
    batch_page.shows(&dutch_with("label-check-link", &[("product", "Desem wit")]));
    let label = label_path(&b, id);
    baker
        .get(&label)
        .await
        .shows(&dutch("label-check-ok-product"));

    {
        let mut conn = pool.acquire().await.unwrap();
        let stored = batches::get(&mut conn, id).await.unwrap();
        batches::adjust(
            &mut conn,
            id,
            &Adjustment {
                lines: vec![(stored.lines[0].id, Pct::from_thousandths(0))],
                pieces: vec![],
                reason: None,
            },
            orders::audit::Actor::System("test"),
            app.clock.as_ref(),
        )
        .await
        .unwrap();
        batches::add_line(
            &mut conn,
            id,
            b.spelt,
            Pct::from_thousandths(100_000),
            None,
            orders::audit::Actor::System("test"),
            app.clock.as_ref(),
        )
        .await
        .unwrap();
    }
    let blocked = baker.get(&label).await;
    blocked.shows(&dutch("label-check-differs"));
    blocked.shows(&dutch("label-check-differs-ingredients"));
    blocked.shows("<strong>speltbloem</strong>, water, zout");
    blocked.shows(&dutch("label-check-product-values"));
    blocked.lacks(&dutch("label-check-ok-batch"));
    {
        let mut conn = pool.acquire().await.unwrap();
        assert!(matches!(
            label_check::label_allowed(&mut conn, id, b.bread).await,
            Err(production::Error::LabelBlocked(_))
        ));
    }

    let approve = format!("{label}/approve");
    let approved = baker.post_shown(&blocked, &approve).await;
    approved.redirects_to(&format!("{label}?opgeslagen=1"));
    baker
        .get(&label)
        .await
        .shows(&dutch("label-check-ok-batch"));
    let mut conn = pool.acquire().await.unwrap();
    let content = label_check::label_allowed(&mut conn, id, b.bread)
        .await
        .unwrap();
    assert_eq!(content.source, label_check::LabelSource::Batch);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_batch_approval_of_old_values_is_refused(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let id = batch(&pool, &b, &app.clock).await;
    let label = label_path(&b, id);
    let page = baker.get(&label).await;
    page.shows(&dutch("label-check-kitchen"));

    {
        let mut conn = pool.acquire().await.unwrap();
        batches::add_line(
            &mut conn,
            id,
            b.spelt,
            Pct::from_thousandths(10_000),
            None,
            orders::audit::Actor::System("test"),
            app.clock.as_ref(),
        )
        .await
        .unwrap();
    }
    let stale = baker
        .post(
            &format!("{label}/approve"),
            &[("ingredient", "tarwebloem"), ("vet", "1"), ("diepte", "0")],
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    stale.shows(&dutch("label-check-changed"));

    let elsewhere = format!(
        "/staff/bake-days/{}/batches/{}/etiket/{}",
        b.day.get(),
        id.get(),
        b.bread.get() + 1
    );
    assert_eq!(baker.get(&elsewhere).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_product_with_a_recipe_has_no_hand_approval(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = product_path(b.bread);
    let page = baker.get(&format!("{path}/approve")).await;
    page.shows(&dutch("approve-computed-only"));
    page.shows(&dutch("rollup-approve"));
    page.lacks(&format!("action=\"{path}/approve\""));
    page.lacks(&dutch("approve-submit"));

    let hand = baker
        .post(
            &format!("{path}/approve"),
            &[
                ("ingredient_text", "BLOEM, water"),
                ("revisie", &page.hidden("revisie")[0]),
            ],
        )
        .await;
    assert_eq!(hand.status, StatusCode::CONFLICT);
    hand.shows(&dutch("approve-computed-only"));
    let product = products::get(&mut pool.acquire().await.unwrap(), b.bread)
        .await
        .unwrap();
    assert_eq!(product.approval, None);
}

/// A cinnamon filling (60 sugar, 40 butter) in the bakery's bread at 20% of the flour.
async fn with_filling(pool: &PgPool) -> domain::RecipeId {
    let sugar = ingredient(pool, "Suiker", IngredientRole::Other, &[]).await;
    let butter = ingredient(pool, "Boter", IngredientRole::Other, &[Allergen::Milk]).await;
    let mut conn = pool.acquire().await.expect("conn");
    let (filling, version) = store::create(
        &mut conn,
        &RecipeInput {
            name: "Kaneelvulling".to_owned(),
            kind: RecipeKind::Filling,
        },
    )
    .await
    .expect("recipe");
    let line = |pct: u32, part| LineInput {
        pct: Pct::from_thousandths(pct * 1000),
        part,
    };
    store::save_draft(
        &mut conn,
        version.id,
        &DraftInput {
            lines: vec![
                line(60, LinePart::Ingredient(sugar)),
                line(40, LinePart::Ingredient(butter)),
            ],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("filling");
    let bread: i64 =
        sqlx::query_scalar("SELECT current_version_id FROM recipe WHERE name = 'Desembrood'")
            .fetch_one(&mut *conn)
            .await
            .expect("bread version");
    let bread = domain::RecipeVersionId::new(bread);
    let mut lines = store::lines(&mut conn, bread).await.expect("lines");
    lines.push(line(20, LinePart::Sub(version.id)));
    store::save_draft(
        &mut conn,
        bread,
        &DraftInput {
            lines,
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("bread");
    filling.id
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_filling_is_approved_and_labelled_as_a_compound_ingredient(pool: PgPool) {
    let app = app(pool.clone());
    let b = bakery(&pool).await;
    let filling = with_filling(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    owner
        .post("/staff/products/may-contain/approve", &[])
        .await
        .redirects_to("/staff/products/may-contain?opgeslagen=1");
    let path = product_path(b.bread);
    let page = baker.get(&format!("{path}/approve")).await;
    page.shows(
        "<strong>tarwebloem</strong>, water, Kaneelvulling (suiker, <strong>boter</strong>), zout",
    );
    assert_eq!(page.hidden("diepte"), ["0", "0", "0", "1", "1", "0"]);
    baker
        .post_shown(&page, &format!("{path}/approve-computed"))
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let product = products::get(&mut pool.acquire().await.unwrap(), b.bread)
        .await
        .unwrap();
    assert!(product.approval.is_some());
    assert_eq!(
        product.allergen_info.ingredient_text,
        "tarwebloem, water, Kaneelvulling (suiker, boter), zout"
    );

    let recipe = format!("/staff/recipes/{}", filling.get());
    let shown = baker.get(&recipe).await;
    shown.shows(&dutch("recipe-label-heading"));
    shown.shows(&dutch_with(
        "recipe-label-now-compound",
        &[("name", "Kaneelvulling")],
    ));
    baker
        .post(
            &format!("{recipe}/etiket"),
            &[("op_etiket", "samengesteld"), ("etiketnaam", "vulling")],
        )
        .await
        .redirects_to(&format!("{recipe}?opgeslagen=1"));
    let cleared = products::get(&mut pool.acquire().await.unwrap(), b.bread)
        .await
        .unwrap();
    assert_eq!(cleared.approval, None, "the setting changed the list");
    let page = baker.get(&format!("{path}/approve")).await;
    page.shows("water, vulling (suiker, <strong>boter</strong>), zout");
    baker
        .post_shown(&page, &format!("{path}/approve-computed"))
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));

    let id = batch(&pool, &b, &app.clock).await;
    let label = label_path(&b, id);
    baker
        .get(&label)
        .await
        .shows(&dutch("label-check-ok-product"));
    {
        let mut conn = pool.acquire().await.unwrap();
        let stored = batches::get(&mut conn, id).await.unwrap();
        let sub = stored.lines.last().unwrap().id;
        batches::adjust(
            &mut conn,
            id,
            &Adjustment {
                lines: vec![(sub, Pct::from_thousandths(200_000))],
                pieces: vec![],
                reason: None,
            },
            orders::audit::Actor::System("test"),
            app.clock.as_ref(),
        )
        .await
        .unwrap();
    }
    let blocked = baker.get(&label).await;
    blocked.shows(&dutch("label-check-differs"));
    blocked.shows("vulling (suiker, <strong>boter</strong>), <strong>tarwebloem</strong>");
    baker
        .post_shown(&blocked, &format!("{label}/approve"))
        .await
        .redirects_to(&format!("{label}?opgeslagen=1"));
    let mut conn = pool.acquire().await.unwrap();
    let content = label_check::label_allowed(&mut conn, id, b.bread)
        .await
        .unwrap();
    assert_eq!(content.source, label_check::LabelSource::Batch);
    assert!(content.order_changed);
    assert_eq!(
        content.ingredients.text(),
        "vulling (suiker, boter), tarwebloem, water, zout"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_compound_setting_needs_allergen_rights_and_a_short_name(pool: PgPool) {
    let app = app(pool.clone());
    bakery(&pool).await;
    let filling = with_filling(&pool).await;
    let recipe = format!("/staff/recipes/{}", filling.get());
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let refused = counter
        .post(&format!("{recipe}/etiket"), &[("op_etiket", "los")])
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);

    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let long = "v".repeat(81);
    let reply = baker
        .post(
            &format!("{recipe}/etiket"),
            &[("op_etiket", "samengesteld"), ("etiketnaam", &long)],
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    reply.shows(&dutch("recipe-error-label-name"));
    reply.shows(&format!("value=\"{long}\""));
    reply.shows("value=\"samengesteld\" selected");
    baker
        .post(&format!("{recipe}/etiket"), &[("op_etiket", "los")])
        .await
        .redirects_to(&format!("{recipe}?opgeslagen=1"));
    baker
        .get(&recipe)
        .await
        .shows(&dutch("recipe-label-now-merged"));
    let mut conn = pool.acquire().await.unwrap();
    let stored = store::get(&mut conn, filling).await.unwrap();
    assert_eq!(stored.label.compound, Some(false));
}
