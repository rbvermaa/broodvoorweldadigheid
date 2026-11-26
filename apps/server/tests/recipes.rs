//! The back-office screens for ingredients and recipe versions (F14).
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
use db::PgPool;
use domain::{Clock, IngredientId, RecipeId, RecipeVersionId};
use i18n::{FluentArgs, Translator};
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
        let seen = self.seen(path, fields).await;
        let mut all: Vec<(&str, &str)> = vec![("_csrf", &self.token)];
        all.extend(seen.iter().map(|s| ("seen", s.as_str())));
        all.extend_from_slice(fields);
        self.post_raw(path, &all).await
    }

    /// For a line change without a `seen` field: the stamp of the lines as stored now, as an
    /// up-to-date page sends it.
    async fn seen(&self, path: &str, fields: &[(&str, &str)]) -> Option<String> {
        if !path.contains("/lines") || fields.iter().any(|(k, _)| *k == "seen") {
            return None;
        }
        let mut parts = path.split('/').skip_while(|p| *p != "versions");
        let version: i64 = parts.nth(1)?.parse().ok()?;
        let mut conn = self.app.pool.acquire().await.expect("conn");
        let lines = store::lines(&mut conn, RecipeVersionId::new(version))
            .await
            .expect("lines");
        Some(store::stamp(&lines))
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

struct Kitchen {
    flour: IngredientId,
    water: IngredientId,
    salt: IngredientId,
    levain: RecipeId,
    levain_v1: RecipeVersionId,
    dough: RecipeId,
    dough_v1: RecipeVersionId,
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

async fn recipe(pool: &PgPool, name: &str, kind: RecipeKind) -> (RecipeId, RecipeVersionId) {
    let mut conn = pool.acquire().await.expect("conn");
    let (recipe, version) = store::create(
        &mut conn,
        &RecipeInput {
            name: name.to_owned(),
            kind,
        },
    )
    .await
    .expect("recipe");
    (recipe.id, version.id)
}

/// A 1:1 levain and an empty dough recipe.
async fn kitchen(pool: &PgPool) -> Kitchen {
    let flour = ingredient(pool, "Tarwebloem", IngredientRole::Flour).await;
    let water = ingredient(pool, "Water", IngredientRole::Water).await;
    let salt = ingredient(pool, "Zeezout", IngredientRole::Other).await;
    let (levain, levain_v1) = recipe(pool, "Desem", RecipeKind::Preferment).await;
    let mut conn = pool.acquire().await.expect("conn");
    store::save_draft(
        &mut conn,
        levain_v1,
        &DraftInput {
            lines: vec![
                LineInput {
                    pct: Pct::HUNDRED,
                    part: LinePart::Ingredient(flour),
                },
                LineInput {
                    pct: Pct::HUNDRED,
                    part: LinePart::Ingredient(water),
                },
            ],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("levain");
    let (dough, dough_v1) = recipe(pool, "Desembrood", RecipeKind::Dough).await;
    Kitchen {
        flour,
        water,
        salt,
        levain,
        levain_v1,
        dough,
        dough_v1,
    }
}

fn version_path(recipe: RecipeId, version: RecipeVersionId) -> String {
    format!("/staff/recipes/{}/versions/{}", recipe.get(), version.get())
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn counter_staff_cannot_see_or_change_recipes(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let version = version_path(k.dough, k.dough_v1);
    for path in [
        "/staff/ingredients".to_owned(),
        "/staff/ingredients/new".to_owned(),
        format!("/staff/ingredients/{}", k.flour.get()),
        "/staff/recipes".to_owned(),
        "/staff/recipes/new".to_owned(),
        format!("/staff/recipes/{}", k.dough.get()),
        version.clone(),
        format!("{version}/edit"),
    ] {
        let reply = counter.get(&path).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
        reply.shows(&dough_forbidden());
    }
    for path in [
        "/staff/ingredients/new".to_owned(),
        format!("/staff/ingredients/{}", k.flour.get()),
        "/staff/recipes/new".to_owned(),
        format!("/staff/recipes/{}", k.dough.get()),
        format!("/staff/recipes/{}/versions", k.dough.get()),
        format!("/staff/recipes/{}/products", k.dough.get()),
        format!("/staff/recipes/{}/products/1/remove", k.dough.get()),
        format!("{version}/edit"),
        format!("{version}/freeze"),
        format!("{version}/current"),
    ] {
        let reply = counter.post(&path, &[("name", "X")]).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
    }
    let mut conn = pool.acquire().await.expect("conn");
    assert!(
        store::version(&mut conn, k.dough, k.dough_v1)
            .await
            .expect("version")
            .frozen_at
            .is_none()
    );
}

fn dough_forbidden() -> String {
    dutch("staff-forbidden-text")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn changes_need_the_csrf_token(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);
    for path in [
        "/staff/ingredients/new".to_owned(),
        "/staff/recipes/new".to_owned(),
        format!("/staff/ingredients/{}", k.flour.get()),
        format!("/staff/recipes/{}", k.dough.get()),
        format!("/staff/recipes/{}/versions", k.dough.get()),
        format!("/staff/recipes/{}/products", k.dough.get()),
        format!("/staff/recipes/{}/products/1/remove", k.dough.get()),
        format!("{version}/edit"),
        format!("{version}/freeze"),
        format!("{version}/current"),
    ] {
        let reply = baker
            .post_raw(&path, &[("name", "Rogge"), ("kind", "dough")])
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
    }
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(store::list(&mut conn).await.expect("list").len(), 2);
    assert!(
        store::version(&mut conn, k.dough, k.dough_v1)
            .await
            .expect("version")
            .frozen_at
            .is_none()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn baker_adds_and_edits_an_ingredient(pool: PgPool) {
    let app = app(pool.clone());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    assert_eq!(baker.get("/staff/ingredients").await.status, StatusCode::OK);
    baker
        .get("/staff/ingredients")
        .await
        .shows(&dutch("ingredients-empty-title"));

    let missing = baker.post("/staff/ingredients/new", &[("name", "")]).await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    missing.shows(&dutch("ingredient-error-name"));
    missing.shows(&dutch("ingredient-error-role"));

    let created = baker
        .post(
            "/staff/ingredients/new",
            &[
                ("name", "Speltbloem"),
                ("label_name", "SPELTbloem"),
                ("role", "flour"),
                ("supplier", "Molen De Hoop"),
                ("allergen_cereals_containing_gluten", "ja"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.text);
    let page = baker.get(created.location()).await;
    page.shows("SPELTbloem");
    page.shows("Molen De Hoop");

    let list = baker.get("/staff/ingredients").await;
    list.shows("Speltbloem");
    list.shows(&dutch("ingredient-role-flour"));
    list.shows(&dutch("allergen-cereals_containing_gluten"));

    let duplicate = baker
        .post(
            "/staff/ingredients/new",
            &[("name", "speltbloem"), ("role", "flour")],
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::UNPROCESSABLE_ENTITY);
    duplicate.shows(&dutch("ingredient-error-duplicate"));

    let path = created
        .location()
        .split('?')
        .next()
        .expect("path")
        .to_owned();
    let edited = baker
        .post(&path, &[("name", "Speltbloem grof"), ("role", "flour")])
        .await;
    assert_eq!(edited.status, StatusCode::SEE_OTHER);
    let list = baker.get("/staff/ingredients").await;
    list.shows("Speltbloem grof");
    list.shows(&dutch("ingredient-allergens-none"));
    assert_eq!(
        baker.get("/staff/ingredients/999").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        baker
            .post(
                "/staff/ingredients/999",
                &[("name", "X"), ("role", "water")]
            )
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn baker_writes_a_recipe_with_levain_and_scales_it(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);

    let choices = baker.htmx_get(&format!("{version}/search?q=Desem")).await;
    assert_eq!(choices.status, StatusCode::OK);
    choices.shows(&dutch_with(
        "recipe-sub-option-draft",
        &[("name", "Desem"), ("n", "1")],
    ));
    choices.lacks(&dutch_with(
        "recipe-sub-option-draft",
        &[("name", "Desembrood"), ("n", "1")],
    ));

    let flour = format!("i{}", k.flour.get());
    let water = format!("i{}", k.water.get());
    let salt = format!("i{}", k.salt.get());
    let levain = format!("v{}", k.levain_v1.get());
    for (part, pct) in [
        (&flour, "90"),
        (&water, "65"),
        (&salt, "2"),
        (&levain, "20"),
    ] {
        let added = baker
            .post(
                &format!("{version}/lines"),
                &[("part", part), ("pct", pct), ("gewicht", "1000")],
            )
            .await;
        assert_eq!(added.status, StatusCode::SEE_OTHER, "{}", added.text);
    }
    baker
        .post(
            &format!("{version}/edit"),
            &[("lead_time", "36"), ("notes", "Desem om 20:00 aanzetten")],
        )
        .await
        .redirects_to(&format!("{version}/edit?gewicht=1000&opgeslagen=1"));

    let page = baker.get(&format!("{version}?gewicht=1770&open=3")).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch_with("recipe-lines-caption", &[("grams", "1.770")]));
    page.shows(&dutch_with(
        "recipe-sub-option",
        &[("name", "Desem"), ("n", "1")],
    ));
    for grams in ["900", "650", "200", "100", "1.000", "750", "1.770"] {
        page.shows(&format!("<td class=\"recipe-grams\">{grams}</td>"));
    }
    page.shows("75%");
    page.shows(&dutch_with("recipe-lead-time-value", &[("hours", "36")]));
    page.shows("Desem om 20:00 aanzetten");
    page.shows(&dutch("recipe-draft-info"));
    page.lacks(&dutch_with("recipe-flour-off", &[("pct", "110%")]));

    baker
        .post(&format!("{version}/freeze"), &[])
        .await
        .redirects_to(&format!("{version}?opgeslagen=1"));
    let frozen = baker.get(&version).await;
    frozen.shows(&dutch_with("recipe-frozen-info", &[("date", "01-12-2026")]));
    let mut conn = pool.acquire().await.expect("conn");
    assert!(
        store::version(&mut conn, k.levain, k.levain_v1)
            .await
            .expect("levain")
            .is_frozen(),
        "the levain inside froze too"
    );

    baker
        .post(&format!("/staff/recipes/{}/versions", k.dough.get()), &[])
        .await;
    let versions = store::versions(&mut conn, k.dough).await.expect("versions");
    assert_eq!(versions.len(), 2);
    let v2 = version_path(k.dough, versions[0].id);
    baker
        .post(
            &format!("{v2}/lines/1"),
            &[("part", &water), ("pct", "74"), ("gewicht", "1770")],
        )
        .await
        .redirects_to(&format!("{v2}/edit?gewicht=1860&opgeslagen=1"));
    assert_eq!(
        store::lines(&mut conn, k.dough_v1).await.expect("v1")[1].pct,
        Pct::from_thousandths(65_000)
    );

    baker
        .post(&format!("{v2}/current"), &[])
        .await
        .redirects_to(&format!("{v2}?opgeslagen=1"));
    let list = baker.get("/staff/recipes").await;
    list.shows(&dutch_with("recipe-version-number", &[("n", "2")]));

    let recipe_page = baker
        .get(&format!("/staff/recipes/{}", k.dough.get()))
        .await;
    recipe_page.shows(&dutch("recipe-draft"));
    recipe_page.shows(&dutch_with("recipe-frozen-on", &[("date", "01-12-2026")]));
    let more = baker
        .post(&format!("/staff/recipes/{}/versions", k.dough.get()), &[])
        .await;
    more.redirects_to(&format!("{v2}/edit?opgeslagen=1"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_editor_explains_what_is_wrong(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let flour = format!("i{}", k.flour.get());
    let version = version_path(k.dough, k.dough_v1);

    let nothing = baker
        .post(&format!("{version}/lines"), &[("pct", "5")])
        .await;
    assert_eq!(nothing.status, StatusCode::UNPROCESSABLE_ENTITY);
    nothing.shows(&dutch("recipe-add-error-choose"));
    let negative = baker
        .post(
            &format!("{version}/lines"),
            &[("part", &flour), ("pct", "-2")],
        )
        .await;
    assert_eq!(negative.status, StatusCode::UNPROCESSABLE_ENTITY);
    negative.shows(&dutch("recipe-line-error-pct"));
    negative.shows("value=\"-2\"");
    let first_by_grams = baker
        .post(
            &format!("{version}/lines"),
            &[("part", &flour), ("gram", "500")],
        )
        .await;
    first_by_grams.shows(&dutch("recipe-add-error-first-pct"));
    let unknown = baker
        .post(
            &format!("{version}/lines"),
            &[("part", "i999"), ("pct", "5")],
        )
        .await;
    unknown.shows(&dutch("recipe-error-line-gone"));

    let levain_path = version_path(k.levain, k.levain_v1);
    baker
        .post(
            &format!("{version}/lines"),
            &[("part", &format!("v{}", k.levain_v1.get())), ("pct", "20")],
        )
        .await;
    let looped = baker
        .post(
            &format!("{levain_path}/lines"),
            &[("part", &format!("v{}", k.dough_v1.get())), ("pct", "10")],
        )
        .await;
    assert_eq!(looped.status, StatusCode::UNPROCESSABLE_ENTITY);
    looped.shows(&dutch("recipe-error-nesting"));

    baker.get(&levain_path).await.shows("100%");
    assert_eq!(
        baker.get(&version_path(k.levain, k.dough_v1)).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        baker
            .post(
                &format!("{}/lines", version_path(k.levain, k.dough_v1)),
                &[("pct", "1")]
            )
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        baker.get("/staff/recipes/abc").await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn baker_creates_a_recipe_and_links_a_product(pool: PgPool) {
    let app = app(pool.clone());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    baker
        .get("/staff/recipes")
        .await
        .shows(&dutch("recipes-empty-title"));
    let missing = baker
        .post("/staff/recipes/new", &[("name", ""), ("kind", "x")])
        .await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    missing.shows(&dutch("recipe-error-name"));
    missing.shows(&dutch("recipe-error-kind"));

    let created = baker
        .post(
            "/staff/recipes/new",
            &[("name", "Rogge"), ("kind", "dough")],
        )
        .await;
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.text);
    let path = created
        .location()
        .split('?')
        .next()
        .expect("path")
        .to_owned();
    let page = baker.get(&path).await;
    page.shows(&dutch_with("recipe-version-number", &[("n", "1")]));
    page.shows(&dutch("recipe-products-none"));

    let duplicate = baker
        .post(
            "/staff/recipes/new",
            &[("name", "rogge"), ("kind", "dough")],
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::UNPROCESSABLE_ENTITY);
    duplicate.shows(&dutch("recipe-error-duplicate"));

    let product: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece,
            allergens_approved_by, allergens_approved_at)
        VALUES ('Roggebrood', 550, 9, 800, 900, (SELECT id FROM staff_user LIMIT 1), now())
        RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .expect("product");
    baker
        .post(
            &format!("{path}/products"),
            &[("product_id", &product.to_string())],
        )
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    baker.get("/staff/recipes").await.shows("Roggebrood");
    baker
        .get(&format!("/staff/products/{product}"))
        .await
        .shows(&dutch("product-approval-cleared-recipe"));
    baker
        .post(&format!("{path}/products/{product}/remove"), &[])
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    baker.get(&path).await.shows(&dutch("recipe-products-none"));
    assert_eq!(
        baker
            .post(&format!("{path}/products"), &[("product_id", "999")])
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let renamed = baker
        .post(&path, &[("name", "Roggebrood recept"), ("kind", "dough")])
        .await;
    renamed.redirects_to(&format!("{path}?opgeslagen=1"));
    baker.get("/staff/recipes").await.shows("Roggebrood recept");
    let blank = baker.post(&path, &[("name", ""), ("kind", "dough")]).await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_frozen_version_keeps_its_ingredient_roles_and_sub_recipe_kinds(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let mut conn = pool.acquire().await.expect("conn");
    store::save_draft(
        &mut conn,
        k.dough_v1,
        &DraftInput {
            lines: vec![
                LineInput {
                    pct: Pct::HUNDRED,
                    part: LinePart::Ingredient(k.flour),
                },
                LineInput {
                    pct: Pct::from_thousandths(20_000),
                    part: LinePart::Sub(k.levain_v1),
                },
            ],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("dough");
    store::freeze(&mut conn, k.dough, k.dough_v1, app.clock.as_ref())
        .await
        .expect("freeze");
    let baker = client(&app, "Bakker Bas", Role::Baker).await;

    let water = format!("/staff/ingredients/{}", k.water.get());
    let role = baker
        .post(&water, &[("name", "Water"), ("role", "other")])
        .await;
    assert_eq!(
        role.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        role.text
    );
    role.shows(&dutch("ingredient-error-role-frozen"));
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        ingredients::get(&mut conn, k.water)
            .await
            .expect("water")
            .role,
        IngredientRole::Water
    );
    baker
        .post(&water, &[("name", "Kraanwater"), ("role", "water")])
        .await
        .redirects_to(&format!("{water}?opgeslagen=1"));

    let levain = format!("/staff/recipes/{}", k.levain.get());
    let kind = baker
        .post(&levain, &[("name", "Desem"), ("kind", "dough")])
        .await;
    assert_eq!(
        kind.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        kind.text
    );
    kind.shows(&dutch("recipe-error-kind-frozen"));
    baker
        .post(&levain, &[("name", "Desem 1:1"), ("kind", "preferment")])
        .await
        .redirects_to(&format!("{levain}?opgeslagen=1"));

    let no_product = baker
        .post(&format!("{levain}/products"), &[("product_id", "")])
        .await;
    no_product.redirects_to(&levain);

    let htmx = |fields: &[(&str, &str)]| {
        let mut all: Vec<(&str, &str)> = vec![("_csrf", &baker.token)];
        all.extend_from_slice(fields);
        Request::post(&levain)
            .header(COOKIE, &baker.cookie)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("hx-request", "true")
            .body(Body::from(Client::body(&all)))
            .expect("request")
    };
    let refused = baker
        .send(htmx(&[("name", "Desem 1:1"), ("kind", "soaker")]))
        .await;
    assert_eq!(refused.status, StatusCode::OK, "htmx swaps the form in");
    refused.shows(&dutch("recipe-error-kind-frozen"));
    let saved = baker
        .send(htmx(&[("name", "Desem"), ("kind", "preferment")]))
        .await;
    assert_eq!(saved.status, StatusCode::OK);
    assert_eq!(
        saved
            .headers
            .get("hx-redirect")
            .map(|v| v.to_str().expect("ascii")),
        Some(format!("{levain}?opgeslagen=1").as_str())
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_recipe_holds_at_most_sixty_lines(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let mut conn = pool.acquire().await.expect("conn");
    store::save_draft(
        &mut conn,
        k.dough_v1,
        &DraftInput {
            lines: vec![line(LinePart::Ingredient(k.flour), 1_000); store::MAX_LINES],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("full");
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let reply = baker
        .post(
            &format!("{}/lines", version_path(k.dough, k.dough_v1)),
            &[("part", &format!("i{}", k.salt.get())), ("pct", "1")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    reply.shows(&dutch("recipe-error-lines"));
    assert_eq!(
        stored_lines(&pool, k.dough_v1).await.len(),
        store::MAX_LINES
    );
}

impl Client<'_> {
    async fn htmx_get(&self, path: &str) -> Reply {
        self.send(
            Request::get(path)
                .header(COOKIE, &self.cookie)
                .header("hx-request", "true")
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    async fn htmx_post(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let seen = self.seen(path, fields).await;
        let mut all: Vec<(&str, &str)> = vec![("_csrf", &self.token)];
        all.extend(seen.iter().map(|s| ("seen", s.as_str())));
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

impl Reply {
    fn replace_url(&self) -> &str {
        self.headers
            .get("hx-replace-url")
            .map(|v| v.to_str().expect("ascii"))
            .unwrap_or_default()
    }
}

/// The dough of `kitchen` as flour 90%, water 65%, salt 2% and levain 20% (1:1), so all
/// flour is 100% and a dough of 1770 g has 1000 g flour.
async fn desembrood(pool: &PgPool, k: &Kitchen) {
    let mut conn = pool.acquire().await.expect("conn");
    store::save_draft(
        &mut conn,
        k.dough_v1,
        &DraftInput {
            lines: vec![
                LineInput {
                    pct: Pct::from_thousandths(90_000),
                    part: LinePart::Ingredient(k.flour),
                },
                LineInput {
                    pct: Pct::from_thousandths(65_000),
                    part: LinePart::Ingredient(k.water),
                },
                LineInput {
                    pct: Pct::from_thousandths(2_000),
                    part: LinePart::Ingredient(k.salt),
                },
                LineInput {
                    pct: Pct::from_thousandths(20_000),
                    part: LinePart::Sub(k.levain_v1),
                },
            ],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("dough");
}

async fn stored_lines(pool: &PgPool, version: RecipeVersionId) -> Vec<LineInput> {
    let mut conn = pool.acquire().await.expect("conn");
    store::lines(&mut conn, version).await.expect("lines")
}

fn line(part: LinePart, thousandths: u32) -> LineInput {
    LineInput {
        pct: Pct::from_thousandths(thousandths),
        part,
    }
}

/// A grams cell of the recipe table, as an input or as text.
fn grams_input(id: &str, value: &str) -> String {
    format!("id=\"{id}\" name=\"gram\" value=\"{value}\"")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_recipe_table_scales_live_without_saving(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    desembrood(&pool, &k).await;
    let product: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece,
            recipe_id)
        VALUES ('Desembrood', 550, 9, 800, 885, $1) RETURNING id",
    )
    .bind(k.dough.get())
    .fetch_one(&pool)
    .await
    .expect("product");
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);

    let page = baker.htmx_get(&format!("{version}?gewicht=1770")).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows("<td class=\"recipe-grams\">900</td>");
    page.shows("<td class=\"recipe-grams\">650</td>");
    page.shows("<td class=\"recipe-grams\">200</td>");
    page.shows("<td class=\"recipe-grams\">1.770</td>");
    page.shows(&dutch("recipe-hydration"));
    page.shows("75%");
    page.lacks("name=\"gram\"");

    let pieces = baker
        .htmx_get(&format!("{version}?stuks=4&product={product}&open=3"))
        .await;
    pieces.shows("<td class=\"recipe-grams\">3.540</td>");
    pieces.shows("<td class=\"recipe-grams\">400</td>");
    pieces.shows("Tarwebloem");
    pieces.shows("value=\"3\" checked");

    let wrong = baker.get(&format!("{version}?gewicht=veel")).await;
    wrong.shows(&dutch("recipe-weight-error"));
    let wrong = baker.get(&format!("{version}?stuks=veel")).await;
    wrong.shows(&dutch("recipe-pieces-error"));

    assert_eq!(stored_lines(&pool, k.dough_v1).await.len(), 4);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_draft_line_is_edited_by_percent_or_by_grams(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    desembrood(&pool, &k).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);
    let water = format!("i{}", k.water.get());
    let salt = format!("i{}", k.salt.get());

    let edit = baker.get(&format!("{version}/edit?gewicht=1770")).await;
    assert_eq!(edit.status, StatusCode::OK);
    edit.shows(&grams_input("gram-2", "20"));
    edit.shows("id=\"pct-2\" name=\"pct\" value=\"2\"");
    assert_eq!(
        edit.text.matches(&format!("id=\"{}\"", "add-part")).count(),
        0,
        "no fixed blank rows"
    );
    assert_eq!(edit.text.matches("id=\"add-q\"").count(), 1);

    let by_pct = baker
        .htmx_post(
            &format!("{version}/lines/1"),
            &[("part", &water), ("pct", "70"), ("gewicht", "1770")],
        )
        .await;
    assert_eq!(by_pct.status, StatusCode::OK, "{}", by_pct.text);
    by_pct.shows(&grams_input("gram-1", "700"));
    by_pct.shows(&grams_input("gram-0", "900"));
    by_pct.shows("<td class=\"recipe-grams\">1.820</td>");
    assert_eq!(by_pct.replace_url(), format!("{version}/edit?gewicht=1820"));
    by_pct.shows(&format!("href=\"{version}?gewicht=1820\""));
    assert_eq!(
        stored_lines(&pool, k.dough_v1).await[1].pct.thousandths(),
        70_000
    );

    let by_grams = baker
        .htmx_post(
            &format!("{version}/lines/2"),
            &[("part", &salt), ("gram", "22"), ("gewicht", "1820")],
        )
        .await;
    assert_eq!(by_grams.status, StatusCode::OK, "{}", by_grams.text);
    assert_eq!(
        stored_lines(&pool, k.dough_v1).await[2].pct.thousandths(),
        2_200
    );
    by_grams.shows(&grams_input("gram-2", "22"));
    by_grams.shows(&grams_input("gram-0", "900"));
    assert_eq!(
        by_grams.replace_url(),
        format!("{version}/edit?gewicht=1822")
    );

    let plain = baker
        .post(
            &format!("{version}/lines/2"),
            &[("part", &salt), ("pct", "2"), ("gewicht", "1822")],
        )
        .await;
    plain.redirects_to(&format!("{version}/edit?gewicht=1820&opgeslagen=1"));

    for (field, value, error) in [
        ("pct", "0", "recipe-line-error-pct"),
        ("pct", "abc", "recipe-line-error-pct"),
        ("gram", "0", "recipe-line-error-grams"),
        ("gram", "-3", "recipe-line-error-grams"),
        ("gram", "", "recipe-line-error-grams"),
    ] {
        let refused = baker
            .htmx_post(
                &format!("{version}/lines/2"),
                &[("part", &salt), (field, value), ("gewicht", "1820")],
            )
            .await;
        assert_eq!(refused.status, StatusCode::OK);
        refused.shows(&dutch(error));
    }
    let stale = baker
        .htmx_post(
            &format!("{version}/lines/1"),
            &[("part", &salt), ("pct", "3"), ("gewicht", "1820")],
        )
        .await;
    stale.shows(&dutch("recipe-error-line-gone"));
    assert_eq!(
        stored_lines(&pool, k.dough_v1).await[2].pct.thousandths(),
        2_000
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn lines_are_added_from_the_list_or_as_a_new_ingredient_and_removed(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    desembrood(&pool, &k).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);
    let salt = format!("i{}", k.salt.get());

    let found = baker.htmx_get(&format!("{version}/search?q=zout")).await;
    assert_eq!(found.status, StatusCode::OK);
    found.shows(&format!("name=\"part\" value=\"{salt}\""));
    found.shows(&dutch_with("recipe-add-new", &[("name", "zout")]));
    let levain = baker.htmx_get(&format!("{version}/search?q=des")).await;
    levain.shows(&format!("value=\"v{}\"", k.levain_v1.get()));
    levain.lacks(&format!("value=\"v{}\"", k.dough_v1.get()));
    let exact = baker.htmx_get(&format!("{version}/search?q=Zeezout")).await;
    exact.lacks(&dutch_with("recipe-add-new", &[("name", "Zeezout")]));

    let added = baker
        .htmx_post(
            &format!("{version}/lines"),
            &[
                ("gewicht", "1770"),
                ("q", "zee"),
                ("part", &salt),
                ("pct", "1"),
            ],
        )
        .await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.text);
    added.shows(&grams_input("gram-4", "10"));
    assert_eq!(
        stored_lines(&pool, k.dough_v1).await[4],
        line(LinePart::Ingredient(k.salt), 1_000)
    );

    let typed = baker
        .post(
            &format!("{version}/lines"),
            &[("gewicht", "1780"), ("q", "zeezout"), ("gram", "20")],
        )
        .await;
    typed.redirects_to(&format!("{version}/edit?gewicht=1800&opgeslagen=1"));
    assert_eq!(
        stored_lines(&pool, k.dough_v1).await[5].pct.thousandths(),
        2_000
    );

    let unknown = baker
        .htmx_post(
            &format!("{version}/lines"),
            &[("gewicht", "1800"), ("q", "Spelt"), ("pct", "5")],
        )
        .await;
    unknown.shows(&dutch("recipe-add-error-choose"));
    let no_role = baker
        .htmx_post(
            &format!("{version}/lines"),
            &[
                ("gewicht", "1800"),
                ("q", "Spelt"),
                ("pct", "5"),
                ("new", "1"),
            ],
        )
        .await;
    no_role.shows(&dutch("ingredient-error-role"));
    no_role.shows("value=\"Spelt\"");
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(ingredients::list(&mut conn).await.expect("list").len(), 3);

    let created = baker
        .htmx_post(
            &format!("{version}/lines"),
            &[
                ("gewicht", "1800"),
                ("q", "Speltbloem"),
                ("pct", "10"),
                ("new", "1"),
                ("role", "flour"),
                ("allergen_cereals_containing_gluten", "ja"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);
    created.shows("Speltbloem");
    let spelt = ingredients::list(&mut conn)
        .await
        .expect("list")
        .into_iter()
        .find(|i| i.name == "Speltbloem")
        .expect("created");
    assert_eq!(spelt.role, IngredientRole::Flour);
    assert!(
        spelt
            .allergens
            .contains(&domain::Allergen::CerealsContainingGluten)
    );
    assert_eq!(
        stored_lines(&pool, k.dough_v1).await[6],
        line(LinePart::Ingredient(spelt.id), 10_000)
    );
    let duplicate = baker
        .htmx_post(
            &format!("{version}/lines"),
            &[
                ("gewicht", "1900"),
                ("q", "speltbloem"),
                ("pct", "1"),
                ("new", "1"),
                ("role", "flour"),
            ],
        )
        .await;
    duplicate.shows(&dutch("ingredient-error-duplicate"));
    assert_eq!(stored_lines(&pool, k.dough_v1).await.len(), 7);

    let removed = baker
        .htmx_post(
            &format!("{version}/lines/6/remove"),
            &[
                ("part", &format!("i{}", spelt.id.get())),
                ("gewicht", "1900"),
            ],
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.text);
    assert_eq!(
        removed.replace_url(),
        format!("{version}/edit?gewicht=1800")
    );
    assert_eq!(stored_lines(&pool, k.dough_v1).await.len(), 6);
    let gone = baker
        .htmx_post(
            &format!("{version}/lines/6/remove"),
            &[
                ("part", &format!("i{}", spelt.id.get())),
                ("gewicht", "1800"),
            ],
        )
        .await;
    gone.shows(&dutch("recipe-error-line-gone"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn warnings_sit_on_the_line_they_are_about(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    desembrood(&pool, &k).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);
    let page = baker.get(&format!("{version}?gewicht=1770")).await;
    page.shows(&dutch_with(
        "recipe-warning-sub-draft",
        &[("name", "Desem"), ("n", "1")],
    ));
    let flour = format!("i{}", k.flour.get());
    baker
        .htmx_post(
            &format!("{version}/lines/0"),
            &[("part", &flour), ("pct", "100"), ("gewicht", "1770")],
        )
        .await
        .shows(&dutch_with("recipe-flour-off", &[("pct", "110%")]));
    let tiny = baker.get(&format!("{version}?gewicht=10")).await;
    tiny.shows(&dutch("recipe-warning-zero"));
    let (empty, empty_v1) = recipe(&pool, "Leeg", RecipeKind::Dough).await;
    baker
        .get(&version_path(empty, empty_v1))
        .await
        .shows(&dutch("recipe-no-lines"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_frozen_version_is_read_only_and_opens_a_new_draft(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    desembrood(&pool, &k).await;
    let mut conn = pool.acquire().await.expect("conn");
    store::freeze(&mut conn, k.dough, k.dough_v1, app.clock.as_ref())
        .await
        .expect("freeze");
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);
    let salt = format!("i{}", k.salt.get());

    let page = baker.get(&version).await;
    page.shows(&dutch("recipe-edit-new-version"));
    page.lacks("<input id=\"gram-");
    baker
        .get(&format!("{version}/edit"))
        .await
        .redirects_to(&version);
    for (path, fields) in [
        (
            format!("{version}/lines/2"),
            vec![("part", salt.as_str()), ("pct", "3")],
        ),
        (
            format!("{version}/lines"),
            vec![("part", salt.as_str()), ("pct", "3")],
        ),
        (
            format!("{version}/lines/2/remove"),
            vec![("part", salt.as_str())],
        ),
        (format!("{version}/edit"), vec![("lead_time", "3")]),
    ] {
        let refused = baker.post(&path, &fields).await;
        assert_eq!(refused.status, StatusCode::CONFLICT, "{path}");
        refused.shows(&dutch("recipe-error-frozen"));
    }
    assert_eq!(stored_lines(&pool, k.dough_v1).await.len(), 4);
    let versions = store::versions(&mut conn, k.dough).await.expect("versions");
    assert_eq!(versions.len(), 1);

    let opened = baker
        .post(&format!("/staff/recipes/{}/versions", k.dough.get()), &[])
        .await;
    let draft = store::versions(&mut conn, k.dough).await.expect("versions")[0].id;
    opened.redirects_to(&format!(
        "{}/edit?opgeslagen=1",
        version_path(k.dough, draft)
    ));
    assert_eq!(stored_lines(&pool, draft).await.len(), 4);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn draft_details_save_without_touching_the_lines(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    desembrood(&pool, &k).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);
    baker
        .post(
            &format!("{version}/edit"),
            &[("lead_time", "36"), ("notes", "Desem om 20:00 aanzetten")],
        )
        .await
        .redirects_to(&format!("{version}/edit?gewicht=1000&opgeslagen=1"));
    let page = baker.get(&version).await;
    page.shows(&dutch_with("recipe-lead-time-value", &[("hours", "36")]));
    page.shows("Desem om 20:00 aanzetten");
    assert_eq!(stored_lines(&pool, k.dough_v1).await.len(), 4);
    let wrong = baker
        .post(&format!("{version}/edit"), &[("lead_time", "200")])
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch("recipe-lead-time-error"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn line_edits_need_production_and_the_csrf_token(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    desembrood(&pool, &k).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);
    let salt = format!("i{}", k.salt.get());
    let fields = [
        ("part", salt.as_str()),
        ("pct", "3"),
        ("q", "Rogge"),
        ("new", "1"),
        ("role", "flour"),
    ];
    let paths = [
        format!("{version}/lines"),
        format!("{version}/lines/2"),
        format!("{version}/lines/2/remove"),
    ];
    for path in &paths {
        assert_eq!(
            counter.post(path, &fields).await.status,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(
            baker.post_raw(path, &fields).await.status,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    assert_eq!(
        counter
            .get(&format!("{version}/search?q=zout"))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let lines = stored_lines(&pool, k.dough_v1).await;
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[2].pct.thousandths(), 2_000);
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(ingredients::list(&mut conn).await.expect("list").len(), 3);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_change_from_an_out_of_date_page_is_refused(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    desembrood(&pool, &k).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);
    let salt = format!("i{}", k.salt.get());
    let page = baker.get(&format!("{version}/edit?gewicht=1770")).await;
    let shown = store::stamp(&stored_lines(&pool, k.dough_v1).await);
    page.shows(&format!("name=\"seen\" value=\"{shown}\""));

    baker
        .htmx_post(
            &format!("{version}/lines/2"),
            &[
                ("part", &salt),
                ("pct", "3"),
                ("gewicht", "1770"),
                ("seen", &shown),
            ],
        )
        .await
        .shows(&grams_input("gram-2", "30"));
    let stale = baker
        .htmx_post(
            &format!("{version}/lines/2/remove"),
            &[("part", &salt), ("gewicht", "1770"), ("seen", &shown)],
        )
        .await;
    assert_eq!(stale.status, StatusCode::OK);
    stale.shows(&dutch("recipe-error-stale"));
    assert_eq!(stored_lines(&pool, k.dough_v1).await.len(), 4);
    let plain = baker
        .post(
            &format!("{version}/lines"),
            &[("part", &salt), ("pct", "1"), ("seen", &shown)],
        )
        .await;
    assert_eq!(plain.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn search_finds_a_sub_recipe_version_by_its_number(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);
    let found = baker.htmx_get(&format!("{version}/search?q=desem+1")).await;
    found.shows(&format!("value=\"v{}\"", k.levain_v1.get()));
    let none = baker.htmx_get(&format!("{version}/search?q=desem+7")).await;
    none.lacks(&format!("value=\"v{}\"", k.levain_v1.get()));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_starter_is_added_with_its_hydration_and_counts_in_the_footer(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let mut conn = pool.acquire().await.expect("conn");
    store::save_draft(
        &mut conn,
        k.dough_v1,
        &DraftInput {
            lines: vec![
                line(LinePart::Ingredient(k.flour), 90_000),
                line(LinePart::Ingredient(k.water), 65_000),
                line(LinePart::Ingredient(k.salt), 2_000),
            ],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("dough");
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let version = version_path(k.dough, k.dough_v1);

    let search = baker
        .htmx_get(&format!("{version}/search?q=Desem+wit"))
        .await;
    search.shows("<option value=\"starter\">");
    search.shows("name=\"hydration\"");

    let fields = |hydration: &'static str| {
        [
            ("gewicht", "1570"),
            ("q", "Desem wit"),
            ("pct", "20"),
            ("new", "1"),
            ("role", "starter"),
            ("hydration", hydration),
        ]
    };
    for wrong in ["", "501", "veel"] {
        let refused = baker
            .htmx_post(&format!("{version}/lines"), &fields(wrong))
            .await;
        refused.shows(&dutch("ingredient-error-hydration"));
        refused.shows("value=\"Desem wit\"");
    }
    assert_eq!(ingredients::list(&mut conn).await.expect("list").len(), 3);
    assert_eq!(stored_lines(&pool, k.dough_v1).await.len(), 3);

    let added = baker
        .htmx_post(&format!("{version}/lines"), &fields("100"))
        .await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.text);
    let desem = ingredients::list(&mut conn)
        .await
        .expect("list")
        .into_iter()
        .find(|i| i.name == "Desem wit")
        .expect("created");
    assert_eq!(desem.role, IngredientRole::Starter);
    assert_eq!(desem.hydration, Some(Pct::HUNDRED));

    let page = baker.get(&format!("{version}?gewicht=1770")).await;
    page.shows("<td class=\"recipe-grams\">200</td>");
    page.shows(&dutch_with("recipe-starter-detail", &[("pct", "100%")]));
    page.shows("<td class=\"recipe-grams\">1.000</td>");
    page.shows("<td class=\"recipe-grams\">750</td>");
    page.shows(&dutch_with(
        "recipe-starter-split",
        &[("flour", "100"), ("water", "100")],
    ));
    page.shows("<td class=\"recipe-pct\">75%</td>");
    page.lacks(&dutch_with("recipe-flour-off", &[("pct", "110%")]));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_starter_hydration_is_edited_on_its_ingredient_page(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    desembrood(&pool, &k).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;

    let missing = baker
        .post(
            "/staff/ingredients/new",
            &[("name", "Roggedesem"), ("role", "starter")],
        )
        .await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    missing.shows(&dutch("ingredient-error-hydration"));
    let fields = [
        ("name", "Roggedesem"),
        ("role", "starter"),
        ("hydration", "80"),
    ];
    assert_eq!(
        baker
            .post_raw("/staff/ingredients/new", &fields)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        counter.post("/staff/ingredients/new", &fields).await.status,
        StatusCode::FORBIDDEN
    );
    let created = baker.post("/staff/ingredients/new", &fields).await;
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.text);
    let path = created
        .location()
        .split('?')
        .next()
        .expect("path")
        .to_owned();
    baker
        .get(&path)
        .await
        .shows("name=\"hydration\" value=\"80\"");
    baker
        .get("/staff/ingredients")
        .await
        .shows(&dutch("ingredient-role-starter"));

    let id = IngredientId::new(path.rsplit('/').next().expect("id").parse().expect("id"));
    let mut conn = pool.acquire().await.expect("conn");
    let (rye, rye_v1) = recipe(&pool, "Roggebrood", RecipeKind::Dough).await;
    store::save_draft(
        &mut conn,
        rye_v1,
        &DraftInput {
            lines: vec![
                line(LinePart::Ingredient(k.flour), 90_000),
                line(LinePart::Ingredient(id), 18_000),
            ],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("rye");
    let wetter = baker
        .post(
            &path,
            &[
                ("name", "Roggedesem"),
                ("role", "starter"),
                ("hydration", "100"),
            ],
        )
        .await;
    assert_eq!(wetter.status, StatusCode::SEE_OTHER, "{}", wetter.text);
    assert_eq!(
        ingredients::get(&mut conn, id)
            .await
            .expect("get")
            .hydration,
        Some(Pct::HUNDRED)
    );

    store::freeze(&mut conn, rye, rye_v1, app.clock.as_ref())
        .await
        .expect("freeze");
    let frozen = baker
        .post(
            &path,
            &[
                ("name", "Roggedesem"),
                ("role", "starter"),
                ("hydration", "120"),
            ],
        )
        .await;
    assert_eq!(frozen.status, StatusCode::UNPROCESSABLE_ENTITY);
    frozen.shows(&dutch("ingredient-error-hydration-frozen"));
    assert_eq!(
        ingredients::get(&mut conn, id)
            .await
            .expect("get")
            .hydration,
        Some(Pct::HUNDRED)
    );
}
