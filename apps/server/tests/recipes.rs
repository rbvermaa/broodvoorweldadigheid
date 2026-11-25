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

    let form = baker.get(&format!("{version}/edit")).await;
    assert_eq!(form.status, StatusCode::OK);
    form.shows(&dutch_with(
        "recipe-sub-option-draft",
        &[("name", "Desem"), ("n", "1")],
    ));
    form.lacks(&dutch_with(
        "recipe-sub-option-draft",
        &[("name", "Desembrood"), ("n", "1")],
    ));

    let flour = format!("i{}", k.flour.get());
    let water = format!("i{}", k.water.get());
    let salt = format!("i{}", k.salt.get());
    let levain = format!("v{}", k.levain_v1.get());
    let saved = baker
        .post(
            &format!("{version}/edit"),
            &[
                ("part-0", &flour),
                ("pct-0", "90"),
                ("part-1", &water),
                ("pct-1", "65"),
                ("part-2", &salt),
                ("pct-2", "2"),
                ("part-3", &levain),
                ("pct-3", "20"),
                ("part-4", ""),
                ("pct-4", ""),
                ("lead_time", "36"),
                ("notes", "Desem om 20:00 aanzetten"),
            ],
        )
        .await;
    saved.redirects_to(&format!("{version}?opgeslagen=1"));

    let page = baker.get(&format!("{version}?gewicht=1770")).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch_with("recipe-lines-caption", &[("grams", "1.770")]));
    page.shows(&dutch_with(
        "recipe-sub-option",
        &[("name", "Desem"), ("n", "1")],
    ));
    for text in [
        "<td class=\"table-number\">900</td>",
        "<td class=\"table-number\">650</td>",
    ] {
        page.shows(text);
    }
    page.shows("↳ Tarwebloem");
    page.shows("<td class=\"table-number\">1.000</td>");
    page.shows("<td class=\"table-number\">750</td>");
    page.shows("75%");
    page.shows(&dutch_with("recipe-lead-time-value", &[("hours", "36")]));
    page.shows("Desem om 20:00 aanzetten");
    page.shows(&dutch("recipe-draft-info"));
    page.lacks(&dutch_with("recipe-flour-off", &[("pct", "110%")]));

    let bad_weight = baker.get(&format!("{version}?gewicht=veel")).await;
    bad_weight.shows(&dutch("recipe-weight-error"));

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

    let edit_frozen = baker.get(&format!("{version}/edit")).await;
    edit_frozen.shows(&dutch_with("recipe-edit-frozen", &[("n", "1")]));
    let next = baker
        .post(
            &format!("{version}/edit"),
            &[
                ("part-0", &flour),
                ("pct-0", "100"),
                ("part-1", &water),
                ("pct-1", "74"),
            ],
        )
        .await;
    assert_eq!(next.status, StatusCode::SEE_OTHER);
    let versions = store::versions(&mut conn, k.dough).await.expect("versions");
    assert_eq!(versions.len(), 2);
    next.redirects_to(&format!(
        "{}?opgeslagen=1",
        version_path(k.dough, versions[0].id)
    ));
    assert_eq!(
        store::lines(&mut conn, k.dough_v1).await.expect("v1").len(),
        4
    );

    let again = baker
        .post(
            &format!("{version}/edit"),
            &[("part-0", &flour), ("pct-0", "100")],
        )
        .await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
    again.shows(&dutch_with("recipe-error-draft-exists", &[("n", "2")]));

    let v2 = version_path(k.dough, versions[0].id);
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
async fn the_edit_form_explains_what_is_wrong(pool: PgPool) {
    let app = app(pool.clone());
    let k = kitchen(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let flour = format!("i{}", k.flour.get());
    let version = version_path(k.dough, k.dough_v1);

    let wrong = baker
        .post(
            &format!("{version}/edit"),
            &[
                ("part-0", &flour),
                ("pct-0", "-2"),
                ("part-1", ""),
                ("pct-1", "5"),
                ("lead_time", "200"),
            ],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch("recipe-line-error-pct"));
    wrong.shows(&dutch("recipe-line-error-part"));
    wrong.shows(&dutch("recipe-lead-time-error"));
    wrong.shows("value=\"-2\"");

    let levain_path = version_path(k.levain, k.levain_v1);
    let dough_v1 = format!("v{}", k.dough_v1.get());
    baker
        .post(
            &format!("{version}/edit"),
            &[
                ("part-0", &flour),
                ("pct-0", "90"),
                ("part-1", &format!("v{}", k.levain_v1.get())),
                ("pct-1", "20"),
            ],
        )
        .await
        .redirects_to(&format!("{version}?opgeslagen=1"));
    let looped = baker
        .post(
            &format!("{levain_path}/edit"),
            &[
                ("part-0", &flour),
                ("pct-0", "100"),
                ("part-1", &dough_v1),
                ("pct-1", "10"),
            ],
        )
        .await;
    assert_eq!(looped.status, StatusCode::UNPROCESSABLE_ENTITY);
    looped.shows(&dutch("recipe-error-nesting"));

    let empty = baker.get(&levain_path).await;
    empty.shows("100%");
    let (leeg_id, leeg_v1) = recipe(&pool, "Leeg", RecipeKind::Dough).await;
    let leeg = baker.get(&version_path(leeg_id, leeg_v1)).await;
    leeg.shows(&dutch("recipe-no-lines"));
    assert_eq!(
        baker.get(&version_path(k.levain, k.dough_v1)).await.status,
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
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
        VALUES ('Roggebrood', 550, 9, 800, 900) RETURNING id",
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
