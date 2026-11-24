//! The back office screens: products and allergen approval (F10), pickup locations
//! (F4a), charities (F4g), bake days (F1, F3, F4c) and extra production (F7a).
#![forbid(unsafe_code)]

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use auth::{NewStaff, Role, StaffUser};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_TYPE, COOKIE, LOCATION};
use axum::http::{HeaderMap, Request, StatusCode};
use bakedays::{
    BakeDayProduct, BakeDayStatus, ExtraInput, NewBakeDay, Purpose, Schedule, days, extras, range,
};
use catalog::{AllergenInfo, CharityInput, ProductInput, charities, products};
use db::PgPool;
use domain::{Allergen, BakeDayId, CharityId, Clock, Money, PickupLocationId, ProductId, VatRate};
use i18n::{FluentArgs, Translator};
use jiff::Timestamp;
use jiff::civil::date;
use server::AppState;
use server::config::Config;
use support::{signed_in, signed_in_at};
use testkit::FakeClock;
use tower::ServiceExt;

/// Tuesday 24 November 2026, 11:00 in Frederiksoord.
const NOW: &str = "2026-11-24T10:00:00Z";
const DE_PROEF: PickupLocationId = PickupLocationId::new(1);

fn ts(s: &str) -> Timestamp {
    s.parse().expect("timestamp")
}

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
    let clock = Arc::new(FakeClock::new(ts(NOW)));
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

fn dutch_with(id: &str, args: &[(&str, &str)]) -> String {
    let mut fluent = FluentArgs::new();
    for (name, value) in args {
        fluent.set(*name, *value);
    }
    Translator::dutch()
        .expect("Dutch texts")
        .t(id, Some(&fluent))
        .expect("message")
}

fn dutch_count(id: &str, n: i64) -> String {
    let mut fluent = FluentArgs::new();
    fluent.set("n", n);
    Translator::dutch()
        .expect("Dutch texts")
        .t(id, Some(&fluent))
        .expect("message")
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    /// The body with HTML entities decoded, so Dutch texts compare as written.
    text: String,
}

impl Reply {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .map(|value| value.to_str().expect("ascii"))
            .unwrap_or_default()
    }

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
        assert_eq!(self.header(LOCATION.as_str()), location);
    }
}

fn decode(html: &str) -> String {
    html.replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
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

    fn form(&self, fields: &[(&str, &str)]) -> String {
        let mut pairs: Vec<(&str, &str)> = vec![("_csrf", &self.token)];
        pairs.extend_from_slice(fields);
        pairs
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&")
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        self.send(
            Request::post(path)
                .header(COOKIE, &self.cookie)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(self.form(fields)))
                .expect("request"),
        )
        .await
    }

    /// As htmx sends it: the token in the header, `HX-Request` set.
    async fn htmx(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        self.send(
            Request::post(path)
                .header(COOKIE, &self.cookie)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("hx-request", "true")
                .header("x-csrf-token", &self.token)
                .body(Body::from(self.form(fields)))
                .expect("request"),
        )
        .await
    }
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

async fn staff(pool: &PgPool, name: &str, role: Role) -> StaffUser {
    auth::staff::add(
        pool,
        &NewStaff {
            oidc_sub: format!("sub-{name}"),
            name: name.to_string(),
            email: None,
            role,
        },
    )
    .await
    .expect("add staff")
}

async fn client<'a>(app: &'a App, name: &str, role: Role) -> Client<'a> {
    let user = staff(&app.pool, name, role).await;
    let (cookie, token) = signed_in(&app.pool, &user).await;
    Client { app, cookie, token }
}

/// Signs `user` in at the app's clock, for a test that moves the clock past a login's lifetime.
async fn sign_in<'a>(app: &'a App, user: &StaffUser) -> Client<'a> {
    let (cookie, token) = signed_in_at(&app.pool, user, app.clock.now()).await;
    Client { app, cookie, token }
}

fn sourdough() -> ProductInput {
    ProductInput {
        name_nl: "Desembrood wit".to_owned(),
        description_nl: "Licht zuur".to_owned(),
        photo_path: None,
        price_incl: Money::from_cents(650),
        vat_rate: VatRate::Nine,
        pay_what_you_want: None,
        unit_weight_g: 800,
        dough_g_per_piece: 900,
        freezer_days: None,
        lead_time_hours: None,
        allergen_info: AllergenInfo {
            allergens: [Allergen::CerealsContainingGluten].into(),
            may_contain: [Allergen::SesameSeeds].into(),
            ingredient_text: "TARWEbloem, water, zout".to_owned(),
        },
        packaged: false,
        sold_online: true,
        estimated_cost: None,
        active: true,
    }
}

async fn product(pool: &PgPool) -> ProductId {
    let mut conn = pool.acquire().await.expect("conn");
    products::create(&mut conn, &sourdough())
        .await
        .expect("product")
        .id
}

/// A draft drop at De Proef on Saturday 5 December, baking from 04:00.
async fn saturday(pool: &PgPool) -> BakeDayId {
    let mut conn = pool.acquire().await.expect("conn");
    days::create(
        &mut conn,
        &NewBakeDay {
            date: date(2026, 12, 5),
            location_id: DE_PROEF,
            bake_starts_at: ts("2026-12-05T03:00:00Z"),
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .expect("bake day")
    .id
}

/// The fields of a valid product; `extra` replaces or adds fields.
fn product_form<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut fields = vec![
        ("name", "Spelt"),
        ("description", "Volkoren spelt"),
        ("price", "5,25"),
        ("vat_rate", "9"),
        ("unit_weight", "750"),
        ("lead_time", ""),
        ("sold_online", "ja"),
        ("active", "ja"),
        ("allergen_cereals_containing_gluten", "ja"),
        ("may_nuts", "ja"),
        ("ingredient_text", "SPELTbloem, water, zout"),
    ];
    fields.retain(|(name, _)| !extra.iter().any(|(other, _)| other == name));
    fields.extend_from_slice(extra);
    fields
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn counter_cannot_open_the_catalogue_or_bake_days(pool: PgPool) {
    let app = app(pool.clone());
    let product = product(&pool).await;
    let day = saturday(&pool).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let forbidden = dutch("staff-forbidden-text");
    for path in [
        "/staff/products".to_owned(),
        "/staff/products/new".to_owned(),
        format!("/staff/products/{}", product.get()),
        format!("/staff/products/{}/approve", product.get()),
        "/staff/locations".to_owned(),
        "/staff/charities".to_owned(),
        "/staff/bake-days".to_owned(),
        format!("/staff/bake-days/{}", day.get()),
        format!("/staff/bake-days/{}/extras", day.get()),
    ] {
        let reply = counter.get(&path).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
        reply.shows(&forbidden);
    }
    let approve = counter
        .post(
            &format!("/staff/products/{}/approve", product.get()),
            &[("allergens", "cereals_containing_gluten")],
        )
        .await;
    assert_eq!(approve.status, StatusCode::FORBIDDEN);
    let edit = counter
        .post(
            &format!("/staff/products/{}", product.get()),
            &product_form(&[]),
        )
        .await;
    assert_eq!(edit.status, StatusCode::FORBIDDEN);
    let schedule = counter
        .post(
            &format!("/staff/bake-days/{}/schedule", day.get()),
            &[("bake_starts_at", "2026-12-05T05:00")],
        )
        .await;
    assert_eq!(schedule.status, StatusCode::FORBIDDEN);

    let mut conn = pool.acquire().await.expect("conn");
    let unchanged = products::get(&mut conn, product).await.expect("product");
    assert_eq!(unchanged.name_nl, "Desembrood wit");
    assert_eq!(unchanged.approval, None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn baker_cannot_manage_charities(pool: PgPool) {
    let app = app(pool);
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    assert_eq!(
        baker.get("/staff/charities").await.status,
        StatusCode::FORBIDDEN
    );
    let reply = baker
        .post(
            "/staff/charities/new",
            &[("name", "Voedselbank"), ("url", "https://example.org")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_a_session_the_back_office_sends_you_to_login(pool: PgPool) {
    let app = app(pool);
    let anonymous = Client {
        app: &app,
        cookie: String::new(),
        token: String::new(),
    };
    let reply = anonymous.get("/staff/products").await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header("location"), "/staff/login");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_new_product_is_saved_but_not_approved(pool: PgPool) {
    let app = app(pool.clone());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;

    let page = baker.get("/staff/products/new").await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch("allergen-cereals_containing_gluten"));
    page.shows("name=\"may_molluscs\"");

    let reply = baker.post("/staff/products/new", &product_form(&[])).await;
    let location = reply.header("location").to_owned();
    assert!(
        location.starts_with("/staff/products/") && location.ends_with("?opgeslagen=1"),
        "{location}"
    );
    let saved = baker.get(&location).await;
    saved.shows(&dutch("saved"));
    saved.shows(&dutch("product-not-approved"));

    let list = baker.get("/staff/products").await;
    list.shows("Spelt");
    list.shows(&dutch("product-not-approved"));
    list.shows("€\u{a0}5,25");

    let mut conn = pool.acquire().await.expect("conn");
    let all = products::list_all(&mut conn).await.expect("list");
    let spelt = all.iter().find(|p| p.name_nl == "Spelt").expect("spelt");
    assert_eq!(spelt.pricing.price_incl(), Money::from_cents(525));
    assert_eq!(spelt.vat_rate, VatRate::Nine);
    assert_eq!(spelt.unit_weight_g, 750);
    assert_eq!(
        spelt.allergen_info.allergens,
        [Allergen::CerealsContainingGluten].into()
    );
    assert_eq!(spelt.allergen_info.may_contain, [Allergen::Nuts].into());
    assert_eq!(spelt.approval, None);
    assert_eq!(spelt.published_allergen_info(), None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn wrong_product_input_gets_dutch_messages(pool: PgPool) {
    let app = app(pool.clone());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;

    let reply = baker
        .post(
            "/staff/products/new",
            &product_form(&[("name", ""), ("price", "zes"), ("unit_weight", "0")]),
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    reply.shows(&dutch("form-has-errors"));
    reply.shows(&dutch("form-error-money"));
    reply.shows(&dutch("product-error-weight"));
    reply.shows("value=\"zes\"");

    let pwyw = baker
        .post(
            "/staff/products/new",
            &product_form(&[("pwyw", "ja"), ("min_price", "6,00"), ("max_price", "10")]),
        )
        .await;
    assert_eq!(pwyw.status, StatusCode::UNPROCESSABLE_ENTITY);
    pwyw.shows(&dutch("product-error-pwyw"));

    let check = baker
        .htmx(
            "/staff/products/check",
            &product_form(&[("price", "6,505")]),
        )
        .await;
    assert_eq!(check.status, StatusCode::OK, "htmx only swaps 2xx");
    check.shows("id=\"f-price\"");
    check.shows(&dutch("form-error-money"));

    let mut conn = pool.acquire().await.expect("conn");
    assert!(
        products::list_all(&mut conn)
            .await
            .expect("list")
            .is_empty()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn pay_what_you_want_saves_its_bounds(pool: PgPool) {
    let app = app(pool.clone());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let reply = baker
        .post(
            "/staff/products/new",
            &product_form(&[("pwyw", "ja"), ("min_price", "2,50"), ("max_price", "25")]),
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text);
    let edit = baker.get(reply.header("location")).await;
    edit.shows("value=\"2,50\"");
    edit.shows("value=\"25,00\"");
    let mut conn = pool.acquire().await.expect("conn");
    let product = &products::list_all(&mut conn).await.expect("list")[0];
    assert!(matches!(
        product.pricing,
        catalog::Pricing::PayWhatYouWant(pwyw)
            if pwyw.min() == Money::from_cents(250) && pwyw.max() == Money::from_cents(2500)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn approving_allergens_approves_exactly_what_was_shown(pool: PgPool) {
    let app = app(pool.clone());
    let id = product(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/products/{}", id.get());

    let page = baker.get(&format!("{path}/approve")).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch("allergen-cereals_containing_gluten"));
    page.shows(&dutch("allergen-sesame_seeds"));
    page.shows("TARWEbloem, water, zout");
    page.shows("name=\"allergens\" value=\"cereals_containing_gluten\"");
    page.shows("name=\"may_contain\" value=\"sesame_seeds\"");

    let reply = baker
        .post(
            &format!("{path}/approve"),
            &[
                ("allergens", "cereals_containing_gluten"),
                ("may_contain", "sesame_seeds"),
                ("ingredient_text", "TARWEbloem, water, zout"),
            ],
        )
        .await;
    reply.redirects_to(&format!("{path}?opgeslagen=1"));

    let edit = baker.get(&path).await;
    edit.shows("Goedgekeurd door Bakker Bas op dinsdag 24 november 2026 om 11:00.");
    let mut conn = pool.acquire().await.expect("conn");
    let approved = products::get(&mut conn, id).await.expect("product");
    assert_eq!(approved.approval.map(|a| a.at), Some(app.clock.now()));
    drop(conn);

    let changed = baker
        .post(
            &path,
            &[
                ("name", "Desembrood wit"),
                ("description", "Licht zuur"),
                ("price", "6,50"),
                ("vat_rate", "9"),
                ("unit_weight", "800"),
                ("sold_online", "ja"),
                ("active", "ja"),
                ("allergen_cereals_containing_gluten", "ja"),
                ("allergen_milk", "ja"),
                ("may_sesame_seeds", "ja"),
                ("ingredient_text", "TARWEbloem, water, MELK, zout"),
            ],
        )
        .await;
    changed.redirects_to(&format!("{path}?herkeuren=1&opgeslagen=1"));
    let after = baker.get(changed.header("location")).await;
    after.shows(&dutch("product-recheck"));
    after.shows(&dutch("product-not-approved"));
    let mut conn = pool.acquire().await.expect("conn");
    let cleared = products::get(&mut conn, id).await.expect("product");
    assert_eq!(cleared.approval, None, "the trigger cleared the approval");
    assert_eq!(
        cleared.dough_g_per_piece, 900,
        "fields not on the form stay"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn approving_data_that_changed_meanwhile_is_refused(pool: PgPool) {
    let app = app(pool.clone());
    let id = product(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let reply = baker
        .post(
            &format!("/staff/products/{}/approve", id.get()),
            &[
                ("allergens", "cereals_containing_gluten"),
                ("ingredient_text", "TARWEbloem, water, zout"),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    reply.shows(&dutch("approve-changed"));
    reply.shows("name=\"may_contain\" value=\"sesame_seeds\"");
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        products::get(&mut conn, id)
            .await
            .expect("product")
            .approval,
        None
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn ingredient_text_with_line_breaks_can_be_approved(pool: PgPool) {
    let app = app(pool.clone());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let created = baker
        .post(
            "/staff/products/new",
            &product_form(&[("ingredient_text", "SPELTbloem\r\nwater")]),
        )
        .await;
    let path = created
        .header("location")
        .trim_end_matches("?opgeslagen=1")
        .to_owned();
    let reply = baker
        .post(
            &format!("{path}/approve"),
            &[
                ("allergens", "cereals_containing_gluten"),
                ("may_contain", "nuts"),
                ("ingredient_text", "SPELTbloem\r\nwater"),
            ],
        )
        .await;
    reply.redirects_to(&format!("{path}?opgeslagen=1"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn owner_adds_edits_and_archives_a_charity(pool: PgPool) {
    let app = app(pool.clone());
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;

    let wrong = owner
        .post(
            "/staff/charities/new",
            &[("name", "Voedselbank"), ("url", "voedselbank.nl")],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch("charity-error-url"));

    let created = owner
        .post(
            "/staff/charities/new",
            &[
                ("name", "Voedselbank"),
                ("description", "Eten voor wie het nodig heeft"),
                ("url", "https://voedselbank.example"),
                ("iban", "nl00 test 0000 0000 00"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.text);
    let path = created
        .header("location")
        .trim_end_matches("?opgeslagen=1")
        .to_owned();
    let edited = owner
        .post(
            &path,
            &[
                ("name", "Voedselbank Frederiksoord"),
                ("url", "https://voedselbank.example"),
            ],
        )
        .await;
    edited.redirects_to(&format!("{path}?opgeslagen=1"));

    let page = owner.get(&path).await;
    page.shows(&dutch("charity-archive"));
    page.shows(&dutch_with(
        "charity-archive-text",
        &[("name", "Voedselbank Frederiksoord")],
    ));

    let archived = owner.htmx(&format!("{path}/archive"), &[]).await;
    assert_eq!(archived.status, StatusCode::OK);
    assert_eq!(
        archived.header("hx-redirect"),
        "/staff/charities?opgeslagen=1"
    );

    let list = owner.get("/staff/charities?opgeslagen=1").await;
    list.shows("Voedselbank Frederiksoord");
    list.shows(&dutch("charity-archived"));
    list.shows(&dutch("saved"));
    let after = owner.get(&path).await;
    after.shows(&dutch("charity-is-archived"));
    after.lacks(&dutch("charity-archive-confirm"));

    let mut conn = pool.acquire().await.expect("conn");
    assert!(
        charities::list_active(&mut conn)
            .await
            .expect("list")
            .is_empty()
    );
    let all = charities::list_all(&mut conn).await.expect("list");
    assert_eq!(all.len(), 1, "archived, never deleted");
    assert_eq!(all[0].archived_at, Some(app.clock.now()));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn locations_and_pickup_windows_are_edited(pool: PgPool) {
    let app = app(pool.clone());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let list = baker.get("/staff/locations").await;
    list.shows("De Proef");

    let page = baker
        .get(&format!("/staff/locations/{}", DE_PROEF.get()))
        .await;
    page.shows("zaterdag");
    page.shows("11:00–12:00");
    page.shows(&dutch_with(
        "window-deadline-rule",
        &[("days", "2"), ("time", "20:00")],
    ));

    let created = baker
        .post(
            "/staff/locations/new",
            &[("name", "Markt"), ("address", "Plein 1, Frederiksoord")],
        )
        .await;
    assert_eq!(created.status, StatusCode::SEE_OTHER);
    let path = created
        .header("location")
        .trim_end_matches("?opgeslagen=1")
        .to_owned();

    let wrong = baker
        .post(
            &format!("{path}/windows/new"),
            &[
                ("weekday", "5"),
                ("starts", "17:00"),
                ("ends", "16:00"),
                ("days_before", "40"),
                ("deadline_time", "20:00"),
                ("active", "ja"),
            ],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch("window-error-order"));
    wrong.shows(&dutch("window-error-days"));

    let window = baker
        .post(
            &format!("{path}/windows/new"),
            &[
                ("weekday", "5"),
                ("starts", "16:00"),
                ("ends", "17:30"),
                ("days_before", "1"),
                ("deadline_time", "12:00"),
                ("active", "ja"),
            ],
        )
        .await;
    window.redirects_to(&format!("{path}?opgeslagen=1"));
    let page = baker.get(&path).await;
    page.shows("vrijdag");
    page.shows("16:00–17:30");

    let check = baker
        .htmx(
            "/staff/locations/windows/check",
            &[("starts", "9:00x"), ("ends", "10:00")],
        )
        .await;
    assert_eq!(check.status, StatusCode::OK);
    check.shows(&dutch("form-error-time"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_new_bake_day_gets_its_slots_and_deadline_from_the_location(pool: PgPool) {
    let app = app(pool.clone());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;

    let page = baker.get("/staff/bake-days/new").await;
    page.shows("De Proef");

    let friday = baker
        .post(
            "/staff/bake-days/new",
            &[
                ("date", "2026-12-04"),
                ("location_id", "1"),
                ("bake_starts_at", "2026-12-04T04:00"),
            ],
        )
        .await;
    assert_eq!(friday.status, StatusCode::UNPROCESSABLE_ENTITY);
    friday.shows(&dutch_with(
        "day-error-no-window",
        &[("weekday", "vrijdag")],
    ));

    let created = baker
        .post(
            "/staff/bake-days/new",
            &[
                ("date", "2026-12-05"),
                ("location_id", "1"),
                ("bake_starts_at", "2026-12-05T04:00"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.text);
    let day = baker.get(created.header("location")).await;
    day.shows("zaterdag 5 december 2026");
    day.shows(&dutch("saved"));
    day.shows("11:00–12:00, De Proef");
    day.shows("value=\"2026-12-03T20:00\"");
    day.shows("value=\"2026-12-05T04:00\"");
    day.shows(&dutch("day-status-draft"));

    let taken = baker
        .post(
            "/staff/bake-days/new",
            &[
                ("date", "2026-12-05"),
                ("location_id", "1"),
                ("bake_starts_at", "2026-12-05T04:00"),
            ],
        )
        .await;
    assert_eq!(taken.status, StatusCode::UNPROCESSABLE_ENTITY);
    taken.shows(&dutch_with(
        "day-error-date-taken",
        &[("date", "zaterdag 5 december 2026")],
    ));

    let list = baker.get("/staff/bake-days").await;
    list.shows("zaterdag 5 december 2026");
    list.shows(&dutch("day-no-charity"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_schedule_shows_the_lead_time_warning_and_refuses_bad_times(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let bread = product(&pool).await;
    let rye = {
        let mut conn = pool.acquire().await.expect("conn");
        products::create(
            &mut conn,
            &ProductInput {
                name_nl: "Roggebrood".to_owned(),
                lead_time_hours: Some(48),
                ..sourdough()
            },
        )
        .await
        .expect("product")
        .id
    };
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}", day.get());
    let add = |product: ProductId| {
        let product = product.get().to_string();
        let baker = &baker;
        let path = &path;
        async move {
            baker
                .post(
                    &format!("{path}/range"),
                    &[
                        ("product_id", product.as_str()),
                        ("max_qty", ""),
                        ("pool_id", ""),
                        ("units", "1"),
                    ],
                )
                .await
                .redirects_to(&format!("{path}?opgeslagen=1"));
        }
    };
    let schedule = |cutoff: &'static str| {
        let baker = &baker;
        let path = &path;
        async move {
            baker
                .post(
                    &format!("{path}/schedule"),
                    &[
                        ("menu_publish_at", "2026-12-01T12:00"),
                        ("opens_at", "2026-12-01T12:00"),
                        ("cutoff_at", cutoff),
                        ("bake_starts_at", "2026-12-05T04:00"),
                    ],
                )
                .await
                .redirects_to(&format!("{path}?opgeslagen=1"));
        }
    };

    add(bread).await;
    baker.get(&path).await.lacks("zuurdesem");

    schedule("2026-12-04T10:00").await;
    baker.get(&path).await.shows(&dutch_with(
        "day-lead-time-warning-sourdough",
        &[
            ("cutoff", "vr 4 dec om 10:00"),
            ("latest", "do 3 dec om 20:00"),
        ],
    ));

    schedule("2026-12-03T15:00").await;
    baker.get(&path).await.lacks("zuurdesem");

    add(rye).await;
    baker.get(&path).await.shows(&dutch_with(
        "day-lead-time-warning-product",
        &[
            ("product", "Roggebrood"),
            ("hours", "48"),
            ("cutoff", "do 3 dec om 15:00"),
            ("latest", "do 3 dec om 04:00"),
        ],
    ));

    schedule("2026-12-03T03:00").await;
    let page = baker.get(&path).await;
    page.lacks("zuurdesem");
    page.shows("value=\"2026-12-03T03:00\"");

    let wrong = baker
        .post(
            &format!("{path}/schedule"),
            &[
                ("cutoff_at", "2026-12-05T06:00"),
                ("bake_starts_at", "2026-12-05T04:00"),
            ],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch("day-error-deadline-after-bake"));
    wrong.shows("value=\"2026-12-05T06:00\"");

    let production_only = baker
        .post(
            &format!("{path}/schedule"),
            &[
                ("opens_at", "2026-12-01T12:00"),
                ("bake_starts_at", "2026-12-05T04:00"),
                ("production_only", "ja"),
            ],
        )
        .await;
    assert_eq!(production_only.status, StatusCode::UNPROCESSABLE_ENTITY);
    production_only.shows(&dutch("day-error-opens-production-only"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn charity_choice_refuses_archived_charities(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let mut conn = pool.acquire().await.expect("conn");
    let input = |name: &str| CharityInput {
        name: name.to_owned(),
        url: "https://example.org".to_owned(),
        ..CharityInput::default()
    };
    let active = charities::create(&mut conn, &input("Voedselbank"))
        .await
        .expect("charity");
    let old = charities::create(&mut conn, &input("Oud doel"))
        .await
        .expect("charity");
    charities::archive(&mut conn, old.id, app.clock.as_ref())
        .await
        .expect("archive");
    drop(conn);
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}", day.get());

    let page = baker.get(&path).await;
    page.shows("Voedselbank");
    page.lacks("Oud doel");

    let refused = baker
        .post(
            &format!("{path}/charity"),
            &[("charity_id", &old.id.get().to_string())],
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.shows(&dutch("day-error-charity-archived"));

    let chosen = baker
        .post(
            &format!("{path}/charity"),
            &[("charity_id", &active.id.get().to_string())],
        )
        .await;
    chosen.redirects_to(&format!("{path}?opgeslagen=1"));
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        days::get(&mut conn, day).await.expect("day").charity_id,
        Some(active.id)
    );
    let back = baker
        .post(&format!("{path}/charity"), &[("charity_id", "")])
        .await;
    back.redirects_to(&format!("{path}?opgeslagen=1"));
    assert_eq!(
        days::get(&mut conn, day).await.expect("day").charity_id,
        None::<CharityId>
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_range_editor_sets_limits_and_pools(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let bread = product(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}", day.get());

    let pool_reply = baker
        .post(
            &format!("{path}/pools"),
            &[("pool_name", "Oven 1"), ("pool_max", "40")],
        )
        .await;
    pool_reply.redirects_to(&format!("{path}?opgeslagen=1"));
    let duplicate = baker
        .post(
            &format!("{path}/pools"),
            &[("pool_name", "Oven 1"), ("pool_max", "10")],
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::UNPROCESSABLE_ENTITY);
    duplicate.shows(&dutch("pool-error-duplicate"));

    let mut conn = pool.acquire().await.expect("conn");
    let oven = range::list_pools(&mut conn, day).await.expect("pools")[0].clone();
    drop(conn);
    let set = baker
        .post(
            &format!("{path}/range"),
            &[
                ("product_id", &bread.get().to_string()),
                ("max_qty", "12"),
                ("pool_id", &oven.id.get().to_string()),
                ("units", "2"),
            ],
        )
        .await;
    set.redirects_to(&format!("{path}?opgeslagen=1"));
    let page = baker.get(&path).await;
    page.shows(&dutch_count("range-max", 12));
    page.shows(&dutch_with(
        "range-in-pool",
        &[("pool", "Oven 1"), ("units", "2")],
    ));
    page.shows(&dutch_count("range-remaining", 12));

    let resized = baker
        .post(
            &format!("{path}/pools/{}", oven.id.get()),
            &[(&format!("pool_{}", oven.id.get()), "10")],
        )
        .await;
    resized.redirects_to(&format!("{path}?opgeslagen=1"));
    baker
        .get(&path)
        .await
        .shows(&dutch_count("range-remaining", 5));

    let wrong_units = baker
        .post(
            &format!("{path}/range"),
            &[("product_id", &bread.get().to_string()), ("units", "0")],
        )
        .await;
    assert_eq!(wrong_units.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong_units.shows(&dutch("range-error-units"));

    let removed = baker
        .htmx(&format!("{path}/range/{}/remove", bread.get()), &[])
        .await;
    assert_eq!(
        removed.header("hx-redirect"),
        format!("{path}?opgeslagen=1")
    );
    let mut conn = pool.acquire().await.expect("conn");
    assert!(
        range::list_products(&mut conn, day)
            .await
            .expect("range")
            .is_empty()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn copy_and_repeat_make_new_drops_and_refuse_taken_dates(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}", day.get());

    let taken = baker
        .post(&format!("{path}/copy"), &[("copy_date", "2026-12-05")])
        .await;
    assert_eq!(taken.status, StatusCode::UNPROCESSABLE_ENTITY);
    taken.shows(&dutch_with(
        "day-error-date-taken",
        &[("date", "zaterdag 5 december 2026")],
    ));

    let copied = baker
        .post(&format!("{path}/copy"), &[("copy_date", "2026-12-12")])
        .await;
    assert_eq!(copied.status, StatusCode::SEE_OTHER, "{}", copied.text);
    let copy = baker.get(copied.header("location")).await;
    copy.shows("zaterdag 12 december 2026");
    copy.shows("value=\"2026-12-10T20:00\"");

    let clash = baker
        .post(&format!("{path}/repeat"), &[("weeks", "2")])
        .await;
    assert_eq!(clash.status, StatusCode::UNPROCESSABLE_ENTITY);
    clash.shows(&dutch_with(
        "day-error-date-taken",
        &[("date", "zaterdag 12 december 2026")],
    ));

    let too_many = baker
        .post(&format!("{path}/repeat"), &[("weeks", "53")])
        .await;
    assert_eq!(too_many.status, StatusCode::UNPROCESSABLE_ENTITY);
    too_many.shows(&dutch("day-error-weeks"));

    let source = copied
        .header("location")
        .trim_end_matches("?opgeslagen=1")
        .to_owned();
    let repeated = baker
        .post(&format!("{source}/repeat"), &[("weeks", "3")])
        .await;
    repeated.redirects_to("/staff/bake-days?herhaald=3&opgeslagen=1");
    let list = baker.get(repeated.header("location")).await;
    list.shows(&dutch_count("day-repeated", 3));
    for day in ["19 december", "26 december", "2 januari 2027"] {
        list.shows(day);
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn opening_and_closing_ask_first_and_follow_the_rules(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let user = staff(&pool, "Bakker Bas", Role::Baker).await;
    let baker = sign_in(&app, &user).await;
    let path = format!("/staff/bake-days/{}", day.get());

    let page = baker.get(&path).await;
    page.shows(&dutch_with(
        "day-open-text",
        &[("date", "zaterdag 5 december 2026")],
    ));
    page.shows("data-dialog-open=\"day-open\"");

    let opened = baker.htmx(&format!("{path}/open"), &[]).await;
    assert_eq!(opened.header("hx-redirect"), format!("{path}?opgeslagen=1"));
    baker.get(&path).await.shows(&dutch_with(
        "day-close-text",
        &[("date", "zaterdag 5 december 2026")],
    ));

    let again = baker.htmx(&format!("{path}/open"), &[]).await;
    assert_eq!(again.status, StatusCode::OK);
    again.shows(&dutch("day-error-transition"));
    again.shows("role=\"alert\"");

    let closed = baker.post(&format!("{path}/close"), &[]).await;
    closed.redirects_to(&format!("{path}?opgeslagen=1"));
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        days::get(&mut conn, day).await.expect("day").status,
        BakeDayStatus::Closed
    );

    let next = days::copy_to(&mut conn, day, date(2026, 12, 12))
        .await
        .expect("copy")
        .day;
    drop(conn);
    app.clock.set(ts("2026-12-11T12:00:00Z"));
    let baker = sign_in(&app, &user).await;
    let late = baker
        .post(&format!("/staff/bake-days/{}/open", next.id.get()), &[])
        .await;
    assert_eq!(late.status, StatusCode::CONFLICT);
    late.shows(&dutch("day-error-deadline-passed"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn extra_production_counts_against_capacity(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let bread = product(&pool).await;
    let mut conn = pool.acquire().await.expect("conn");
    range::set_product(
        &mut conn,
        day,
        &BakeDayProduct {
            product_id: bread,
            max_qty: Some(10),
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await
    .expect("range");
    drop(conn);
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}/extras", day.get());
    let bread_id = bread.get().to_string();

    let page = baker.get(&path).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows("Desembrood wit");

    let added = baker
        .post(
            &path,
            &[
                ("product_id", &bread_id),
                ("qty", "4"),
                ("purpose", "freezer"),
                ("note", "voor de vriezer"),
            ],
        )
        .await;
    added.redirects_to(&format!("{path}?opgeslagen=1"));

    let too_many = baker
        .post(
            &path,
            &[
                ("product_id", &bread_id),
                ("qty", "7"),
                ("purpose", "sumup"),
            ],
        )
        .await;
    assert_eq!(too_many.status, StatusCode::UNPROCESSABLE_ENTITY);
    too_many.shows(&dutch_count("extra-error-capacity", 6));

    let mut conn = pool.acquire().await.expect("conn");
    let extra = extras::list(&mut conn, day).await.expect("extras")[0].clone();
    drop(conn);
    let n = extra.id.get();
    let row = format!("{path}/{n}");
    let qty = format!("qty-{n}");
    let purpose = format!("purpose-{n}");
    let note = format!("note-{n}");

    let changed = baker
        .htmx(&row, &[(&qty, "6"), (&purpose, "samples"), (&note, "")])
        .await;
    assert_eq!(changed.status, StatusCode::OK);
    changed.shows(&format!("id=\"extra-{n}\""));
    changed.shows(&dutch("saved"));

    let over = baker
        .htmx(&row, &[(&qty, "11"), (&purpose, "samples"), (&note, "")])
        .await;
    assert_eq!(over.status, StatusCode::OK);
    over.shows(&dutch_count("extra-error-capacity-line", 10));
    over.shows("value=\"11\"");

    let mut conn = pool.acquire().await.expect("conn");
    let stored = extras::list(&mut conn, day).await.expect("extras");
    assert_eq!(stored[0].qty, 6);
    assert_eq!(stored[0].purpose, Purpose::Samples);
    drop(conn);

    let removed = baker.htmx(&format!("{row}/remove"), &[]).await;
    assert_eq!(
        removed.header("hx-redirect"),
        format!("{path}?opgeslagen=1")
    );
    let mut conn = pool.acquire().await.expect("conn");
    assert!(
        extras::list(&mut conn, day)
            .await
            .expect("extras")
            .is_empty()
    );
    extras::add(
        &mut conn,
        day,
        bread,
        &ExtraInput {
            qty: 1,
            purpose: Purpose::Other,
            note: None,
        },
        &bakedays::NoOrders,
    )
    .await
    .expect("extras still fit");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn unknown_ids_get_the_dutch_404(pool: PgPool) {
    let app = app(pool);
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    for path in [
        "/staff/products/999",
        "/staff/products/abc",
        "/staff/bake-days/999",
        "/staff/bake-days/999/extras",
        "/staff/locations/999",
        "/staff/locations/1/windows/999",
    ] {
        let reply = baker.get(path).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        reply.shows(&dutch("error-not-found-title"));
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn counter_cannot_change_anything(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let bread = product(&pool).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let path = format!("/staff/bake-days/{}", day.get());
    let bread_id = bread.get().to_string();
    for (url, fields) in [
        (
            "/staff/locations/new".to_owned(),
            vec![("name", "X"), ("address", "Y")],
        ),
        (
            format!("{path}/range"),
            vec![("product_id", bread_id.as_str()), ("units", "1")],
        ),
        (
            format!("{path}/pools"),
            vec![("pool_name", "Oven"), ("pool_max", "4")],
        ),
        (format!("{path}/open"), vec![]),
        (format!("{path}/close"), vec![]),
        (format!("{path}/copy"), vec![("copy_date", "2026-12-12")]),
        (
            format!("{path}/extras"),
            vec![
                ("product_id", bread_id.as_str()),
                ("qty", "1"),
                ("purpose", "other"),
            ],
        ),
    ] {
        let reply = counter.post(&url, &fields).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{url}");
    }
    let mut conn = pool.acquire().await.expect("conn");
    assert!(
        range::list_products(&mut conn, day)
            .await
            .expect("range")
            .is_empty()
    );
    assert!(
        extras::list(&mut conn, day)
            .await
            .expect("extras")
            .is_empty()
    );
    assert_eq!(
        days::get(&mut conn, day).await.expect("day").status,
        BakeDayStatus::Draft
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_extra_of_another_day_is_not_found_and_a_refused_removal_shows_an_alert(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let bread = product(&pool).await;
    let mut conn = pool.acquire().await.expect("conn");
    let other = days::copy_to(&mut conn, day, date(2026, 12, 12))
        .await
        .expect("copy")
        .day;
    let extra = extras::add(
        &mut conn,
        day,
        bread,
        &ExtraInput {
            qty: 2,
            purpose: Purpose::Freezer,
            note: None,
        },
        &bakedays::NoOrders,
    )
    .await
    .expect("extra");
    drop(conn);
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let n = extra.id.get();
    let foreign = baker
        .post(
            &format!("/staff/bake-days/{}/extras/{n}/remove", other.id.get()),
            &[],
        )
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);

    let mut conn = pool.acquire().await.expect("conn");
    days::cancel(&mut conn, app.clock.as_ref(), day, "Ziek")
        .await
        .expect("cancel");
    drop(conn);
    let refused = baker
        .htmx(
            &format!("/staff/bake-days/{}/extras/{n}/remove", day.get()),
            &[],
        )
        .await;
    assert_eq!(refused.status, StatusCode::OK);
    refused.shows(&dutch("day-error-final"));
    refused.lacks("<html");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn orders_take_capacity_and_lock_the_charity(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let bread = product(&pool).await;
    let approver = staff(&pool, "Keurder Karin", Role::Owner).await;
    let mut conn = pool.acquire().await.expect("conn");
    products::approve_allergens(
        &mut conn,
        bread,
        &sourdough().allergen_info,
        approver.id,
        app.clock.as_ref(),
    )
    .await
    .expect("approve");
    range::set_product(
        &mut conn,
        day,
        &BakeDayProduct {
            product_id: bread,
            max_qty: Some(10),
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await
    .expect("range");
    let stored = days::get(&mut conn, day).await.expect("day");
    days::update_schedule(
        &mut conn,
        day,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(ts("2026-11-20T08:00:00Z")),
            cutoff_at: stored.cutoff_at,
            bake_starts_at: stored.bake_starts_at,
        },
    )
    .await
    .expect("schedule");
    days::open(&mut conn, app.clock.as_ref(), day)
        .await
        .expect("open");
    let slot = days::list_slots(&mut conn, day).await.expect("slots")[0].id;
    let mut cart = orders::Cart::new(day);
    cart.set(bread, 3, None).expect("cart");
    orders::checkout(
        &mut conn,
        app.clock.as_ref(),
        &cart,
        &orders::CustomerInput {
            email: "klant@example.org".to_owned(),
            name: "Test Klant".to_owned(),
            phone: None,
            marketing_opt_in: false,
        },
        slot,
        orders::OrderedBy::Guest,
    )
    .await
    .expect("order");
    let charity = charities::create(
        &mut conn,
        &CharityInput {
            name: "Voedselbank".to_owned(),
            url: "https://example.org".to_owned(),
            ..CharityInput::default()
        },
    )
    .await
    .expect("charity");
    drop(conn);
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}", day.get());

    baker
        .get(&path)
        .await
        .shows(&dutch_count("range-remaining", 7));

    let too_many = baker
        .post(
            &format!("{path}/extras"),
            &[
                ("product_id", &bread.get().to_string()),
                ("qty", "8"),
                ("purpose", "sumup"),
            ],
        )
        .await;
    assert_eq!(too_many.status, StatusCode::UNPROCESSABLE_ENTITY);
    too_many.shows(&dutch_count("extra-error-capacity", 7));

    let refused = baker
        .post(
            &format!("{path}/charity"),
            &[("charity_id", &charity.id.get().to_string())],
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.shows(&dutch("day-error-orders-exist"));
}
