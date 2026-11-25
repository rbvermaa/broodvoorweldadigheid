//! The back office screens: products and allergen approval (F10), pickup locations
//! (F4a), charities (F4g), bake days (F1, F3, F4c), extra production (F7a), and the
//! production (F7) and pickup (F8) lists, and the ingredient needs (F15).
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
use support::signed_in_at;
use testkit::{FakeClock, FakePaymentProvider};
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
    mollie: Arc<FakePaymentProvider>,
}

fn app(pool: PgPool) -> App {
    let config = Config::from_lookup(|key| {
        (key == "DATABASE_URL").then(|| "postgres://unused/db".to_string())
    })
    .expect("config");
    let clock = Arc::new(FakeClock::new(ts(NOW)));
    let mollie = Arc::new(FakePaymentProvider::new());
    let state = AppState::new(
        pool.clone(),
        Translator::dutch().expect("Dutch texts"),
        config,
    )
    .with_clock(clock.clone())
    .with_payments(mollie.clone());
    App {
        router: server::router(state),
        pool,
        clock,
        mollie,
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
        .replace("&#38;", "&")
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
        let pdf = headers
            .get(CONTENT_TYPE)
            .is_some_and(|v| v == "application/pdf");
        let text = if pdf {
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            decode(std::str::from_utf8(&bytes).expect("utf-8"))
        };
        Reply {
            status,
            headers,
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
    let (cookie, token) = signed_in_at(&app.pool, &user, app.clock.now()).await;
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
    saturday_for(pool, None).await
}

async fn saturday_for(pool: &PgPool, charity_id: Option<CharityId>) -> BakeDayId {
    let mut conn = pool.acquire().await.expect("conn");
    days::create(
        &mut conn,
        &NewBakeDay {
            date: date(2026, 12, 5),
            location_id: DE_PROEF,
            online_ordering: true,
            charity_id,
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
            &[("cutoff_at", "2026-12-03T20:00")],
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
    page.lacks("name=\"lead_time\"");

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

    let too_high = dutch_with(
        "product-error-price-high",
        &[("max", &i18n::money(100_000))],
    );
    for price in ["4.500", "1.000,01"] {
        let reply = baker
            .post("/staff/products/new", &product_form(&[("price", price)]))
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{price}");
        reply.shows(&too_high);
    }
    let pwyw_high = baker
        .post(
            "/staff/products/new",
            &product_form(&[
                ("pwyw", "ja"),
                ("min_price", "2,50"),
                ("max_price", "2.500"),
            ]),
        )
        .await;
    assert_eq!(pwyw_high.status, StatusCode::UNPROCESSABLE_ENTITY);
    pwyw_high.shows(&too_high);

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
    page.lacks("name=\"bake_starts_at\"");

    let friday = baker
        .post(
            "/staff/bake-days/new",
            &[("date", "2026-12-04"), ("location_id", "1")],
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
            &[("date", "2026-12-05"), ("location_id", "1")],
        )
        .await;
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.text);
    let day = baker.get(created.header("location")).await;
    day.shows("zaterdag 5 december 2026");
    day.shows(&dutch("saved"));
    day.shows("11:00–12:00, De Proef");
    day.shows("value=\"2026-12-03T20:00\"");
    day.lacks("name=\"bake_starts_at\"");
    day.shows(&dutch("day-status-draft"));

    let taken = baker
        .post(
            "/staff/bake-days/new",
            &[("date", "2026-12-05"), ("location_id", "1")],
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
async fn the_only_pickup_location_is_chosen_for_a_new_bake_day(pool: PgPool) {
    let app = app(pool.clone());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let placeholder = format!(
        "<option value=\"\">{}</option>",
        dutch("select-placeholder")
    );
    let location_select = |page: &Reply| {
        let from = page
            .text
            .find("id=\"location_id\"")
            .expect("location select");
        let to = from + page.text[from..].find("</select>").expect("end of select");
        page.text[from..to].to_owned()
    };

    let page = baker.get("/staff/bake-days/new").await;
    let select = location_select(&page);
    assert!(
        select.contains("<option value=\"1\" selected>De Proef</option>"),
        "{select}"
    );
    assert!(!select.contains(&placeholder), "{select}");

    let created = baker
        .post(
            "/staff/bake-days/new",
            &[("date", "2026-12-05"), ("location_id", "1")],
        )
        .await;
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.text);

    baker
        .post(
            "/staff/locations/new",
            &[("name", "Markt"), ("address", "Plein 1, Frederiksoord")],
        )
        .await;
    let page = baker.get("/staff/bake-days/new").await;
    let select = location_select(&page);
    assert!(select.contains(&placeholder), "{select}");
    assert!(!select.contains(" selected"), "{select}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_only_charity_is_suggested_for_a_new_bake_day_but_can_be_left_out(pool: PgPool) {
    let app = app(pool.clone());
    let mut conn = pool.acquire().await.expect("conn");
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
    let placeholder = format!(
        "<option value=\"\">{}</option>",
        dutch("select-placeholder")
    );
    let chosen = format!(
        "<option value=\"{}\" selected>Voedselbank</option>",
        charity.id.get()
    );
    let charity_select = |page: &Reply| {
        let from = page.text.find("id=\"charity_id\"").expect("charity select");
        let to = from + page.text[from..].find("</select>").expect("end of select");
        page.text[from..to].to_owned()
    };

    let page = baker.get("/staff/bake-days/new").await;
    let select = charity_select(&page);
    assert!(select.contains(&chosen), "{select}");
    assert!(select.contains(&placeholder), "{select}");

    let refused = baker
        .post(
            "/staff/bake-days/new",
            &[("date", ""), ("location_id", "1"), ("charity_id", "")],
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    let select = charity_select(&refused);
    assert!(!select.contains(" selected"), "{select}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_schedule_warns_only_for_the_sourdough_rule_and_refuses_bad_times(pool: PgPool) {
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
                    &[("product_id", product.as_str()), ("max_qty", "")],
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
    baker.get(&path).await.lacks("zuurdesem");

    schedule("2026-12-03T03:00").await;
    let page = baker.get(&path).await;
    page.lacks("zuurdesem");
    page.shows("value=\"2026-12-03T03:00\"");

    let wrong = baker
        .post(
            &format!("{path}/schedule"),
            &[("cutoff_at", "2026-12-05T05:01")],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch_with(
        "day-error-deadline-after-bake",
        &[("time", "05:00")],
    ));
    wrong.shows("value=\"2026-12-05T05:01\"");

    let production_only = baker
        .post(
            &format!("{path}/schedule"),
            &[("opens_at", "2026-12-01T12:00"), ("production_only", "ja")],
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
async fn the_range_editor_sets_limits(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let bread = product(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}", day.get());

    let set = baker
        .post(
            &format!("{path}/range"),
            &[("product_id", &bread.get().to_string()), ("max_qty", "12")],
        )
        .await;
    set.redirects_to(&format!("{path}?opgeslagen=1"));
    let page = baker.get(&path).await;
    page.shows(&dutch_count("range-max", 12));
    page.shows(&dutch_count("range-remaining", 12));

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
async fn changing_a_limit_keeps_the_hidden_pool(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let bread = product(&pool).await;
    let spelt = {
        let mut conn = pool.acquire().await.expect("conn");
        products::create(
            &mut conn,
            &ProductInput {
                name_nl: "Speltbrood".to_owned(),
                ..sourdough()
            },
        )
        .await
        .expect("product")
        .id
    };
    let mut conn = pool.acquire().await.expect("conn");
    let oven = range::create_pool(&mut conn, day, "Oven 1", 40)
        .await
        .expect("pool");
    range::set_product(
        &mut conn,
        day,
        &BakeDayProduct {
            product_id: bread,
            max_qty: Some(12),
            capacity_pool_id: Some(oven.id),
            units_per_item: 2,
        },
    )
    .await
    .expect("range");
    drop(conn);
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}", day.get());
    let id = bread.get();

    let page = baker.get(&path).await;
    page.shows(&format!("action=\"{path}/range/{id}\""));
    page.shows(&format!(
        "id=\"max_qty_{id}\" name=\"max_qty_{id}\" value=\"12\""
    ));
    page.lacks(&format!("units_{id}"));
    page.lacks("Oven 1");
    page.lacks("pool_id");
    page.lacks(">Desembrood wit</option>");
    page.shows(">Speltbrood</option>");

    let max = format!("max_qty_{id}");
    let changed = baker
        .post(&format!("{path}/range/{id}"), &[(&max, "20")])
        .await;
    changed.redirects_to(&format!("{path}?opgeslagen=1"));
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        range::list_products(&mut conn, day).await.expect("range"),
        [BakeDayProduct {
            product_id: bread,
            max_qty: Some(20),
            capacity_pool_id: Some(oven.id),
            units_per_item: 2,
        }]
    );
    drop(conn);

    let stale_add = baker
        .post(
            &format!("{path}/range"),
            &[("product_id", &id.to_string()), ("max_qty", "8")],
        )
        .await;
    stale_add.redirects_to(&format!("{path}?opgeslagen=1"));
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        range::list_products(&mut conn, day).await.expect("range")[0].capacity_pool_id,
        Some(oven.id)
    );
    drop(conn);

    let absent = baker
        .post(
            &format!("{path}/range/{}", spelt.get()),
            &[(&format!("max_qty_{}", spelt.get()), "5")],
        )
        .await;
    assert_eq!(absent.status, StatusCode::NOT_FOUND);
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        range::list_products(&mut conn, day)
            .await
            .expect("range")
            .len(),
        1
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_day_without_pickup_slots_cannot_go_online(pool: PgPool) {
    let app = app(pool.clone());
    let friday = {
        let mut conn = pool.acquire().await.expect("conn");
        days::create(
            &mut conn,
            &NewBakeDay {
                date: date(2026, 12, 4),
                location_id: DE_PROEF,
                online_ordering: false,
                charity_id: None,
            },
        )
        .await
        .expect("bake day")
        .id
    };
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}", friday.get());

    let online = baker
        .post(
            &format!("{path}/schedule"),
            &[("cutoff_at", "2026-12-02T20:00")],
        )
        .await;
    assert_eq!(online.status, StatusCode::UNPROCESSABLE_ENTITY);
    online.shows(&dutch("day-error-no-pickup-slot"));

    sqlx::query(
        "UPDATE bake_day SET online_ordering = true, cutoff_at = '2026-12-02T19:00:00Z' WHERE id = $1",
    )
    .bind(friday.get())
    .execute(&pool)
    .await
    .expect("online");
    let opened = baker.htmx(&format!("{path}/open"), &[]).await;
    assert_eq!(opened.status, StatusCode::OK);
    opened.shows(&dutch("day-error-no-pickup-slot"));
    let mut conn = pool.acquire().await.expect("conn");
    assert_eq!(
        days::get(&mut conn, friday).await.expect("day").status,
        BakeDayStatus::Draft
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
            vec![("product_id", bread_id.as_str()), ("max_qty", "1")],
        ),
        (format!("{path}/range/{bread_id}"), vec![("max_qty", "1")]),
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

/// An open drop on Saturday 5 December with bread on the range, a second slot at another
/// location, and a paid order of two loaves.
struct PaidOrder {
    day: BakeDayId,
    order: orders::Order,
    markt_slot: domain::PickupSlotId,
    payment: domain::ProviderPaymentId,
}

async fn paid_order(app: &App) -> PaidOrder {
    paid_order_for(app, None).await
}

/// [`paid_order`] on a drop for `charity`.
async fn paid_order_for(app: &App, charity: Option<CharityId>) -> PaidOrder {
    let pool = &app.pool;
    let day = saturday_for(pool, charity).await;
    let bread = product(pool).await;
    let approver = staff(pool, "Keurder Karin", Role::Owner).await;
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
            max_qty: None,
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
        },
    )
    .await
    .expect("schedule");
    days::open(&mut conn, app.clock.as_ref(), day)
        .await
        .expect("open");
    let slot = days::list_slots(&mut conn, day).await.expect("slots")[0].id;
    let markt: i64 = sqlx::query_scalar(
        "INSERT INTO pickup_location (name, address) VALUES ('Markt Vledder', 'Brink 1, 8381 AA Vledder')
        RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .expect("location");
    let markt_slot = days::add_slot(
        &mut conn,
        day,
        PickupLocationId::new(markt),
        &bakedays::PickupSlotInput {
            starts_at: ts("2026-12-05T12:00:00Z"),
            ends_at: ts("2026-12-05T13:00:00Z"),
            max_orders: Some(1),
        },
    )
    .await
    .expect("slot");
    let mut cart = orders::Cart::new(day);
    cart.set(bread, 2, None).expect("cart");
    let order = orders::checkout(
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
    let payment = pay_at_mollie(app, &order).await;
    let outcome = orders::mark_paid(
        &mut conn,
        app.clock.as_ref(),
        order.id,
        &orders::PaymentReceived {
            reference: payment.to_string(),
            amount: order.total,
        },
    )
    .await
    .expect("paid");
    assert!(matches!(outcome, orders::PaidOutcome::Paid { .. }));
    let order = orders::order::get(&mut conn, order.id)
        .await
        .expect("order");
    PaidOrder {
        day,
        order,
        markt_slot,
        payment,
    }
}

/// Creates and pays a payment for `order` at the fake Mollie.
async fn pay_at_mollie(app: &App, order: &orders::Order) -> domain::ProviderPaymentId {
    use domain::PaymentProvider as _;
    let created = app
        .mollie
        .create_payment(&domain::NewPayment {
            amount: order.total,
            description: format!("Bestelling {}", order.number),
            redirect_url: "https://shop.test/bestelling".into(),
            webhook_url: "https://shop.test/webhooks/mollie".into(),
            order: order.id,
            methods: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("payment");
    app.mollie.pay(&created.id);
    created.id
}

async fn stored_order(pool: &PgPool, id: domain::OrderId) -> orders::Order {
    let mut conn = pool.acquire().await.expect("conn");
    orders::order::get(&mut conn, id).await.expect("order")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn counter_sees_orders_and_ticks_off_pickups_but_cannot_change_them(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let order_path = format!("/staff/orders/{}", paid.order.id.get());

    let list = counter
        .get(&format!("/staff/bake-days/{}/orders", paid.day.get()))
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text);
    list.shows("Test Klant");
    list.shows(&paid.order.number.to_string());
    list.shows(&dutch("staff-order-status-paid"));
    let detail = counter.get(&order_path).await;
    assert_eq!(detail.status, StatusCode::OK);
    detail.shows("klant@example.org");
    detail.lacks(&dutch("staff-order-cancel"));
    detail.lacks(&dutch("staff-order-move-save"));

    let markt = paid.markt_slot.get().to_string();
    for (url, fields) in [
        (format!("{order_path}/cancel"), vec![]),
        (
            format!("{order_path}/pickup"),
            vec![("slot", markt.as_str())],
        ),
    ] {
        let reply = counter.post(&url, &fields).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{url}");
    }
    let unchanged = stored_order(&pool, paid.order.id).await;
    assert_eq!(unchanged.status, orders::OrderStatus::Paid);
    assert_eq!(unchanged.pickup_slot_id, paid.order.pickup_slot_id);

    counter
        .post(&format!("{order_path}/picked-up"), &[])
        .await
        .redirects_to(&format!("{order_path}?opgeslagen=1"));
    assert_eq!(
        stored_order(&pool, paid.order.id).await.status,
        orders::OrderStatus::PickedUp
    );
    let history = counter.get(&order_path).await;
    history.shows(&dutch("staff-order-action-picked_up"));
    history.shows("Toonbank Tineke");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn order_changes_need_the_csrf_token(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let order_path = format!("/staff/orders/{}", paid.order.id.get());
    for action in ["cancel", "picked-up", "pickup"] {
        let reply = owner
            .send(
                Request::post(format!("{order_path}/{action}"))
                    .header(COOKIE, &owner.cookie)
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("slot={}", paid.markt_slot.get())))
                    .expect("request"),
            )
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{action}");
    }
    let unchanged = stored_order(&pool, paid.order.id).await;
    assert_eq!(unchanged.status, orders::OrderStatus::Paid);
    assert_eq!(unchanged.pickup_slot_id, paid.order.pickup_slot_id);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_baker_cancels_an_unpaid_order(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;

    let mut conn = pool.acquire().await.expect("conn");
    let mut cart = orders::Cart::new(paid.day);
    cart.set(paid.order.lines[0].product_id, 1, None)
        .expect("cart");
    let pending = orders::checkout(
        &mut conn,
        app.clock.as_ref(),
        &cart,
        &orders::CustomerInput {
            email: "later@example.org".to_owned(),
            name: "Late Klant".to_owned(),
            phone: None,
            marketing_opt_in: false,
        },
        paid.order.pickup_slot_id,
        orders::OrderedBy::Guest,
    )
    .await
    .expect("order");
    drop(conn);
    let pending_path = format!("/staff/orders/{}", pending.id.get());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    baker.get(&pending_path).await.shows(&dutch_with(
        "staff-order-cancel-text",
        &[
            ("number", &pending.number.to_string()),
            ("name", "Late Klant"),
        ],
    ));
    baker
        .post(&format!("{pending_path}/cancel"), &[])
        .await
        .redirects_to(&format!("{pending_path}?opgeslagen=1"));
    let cancelled = stored_order(&pool, pending.id).await;
    assert_eq!(cancelled.status, orders::OrderStatus::Cancelled);
    assert_eq!(cancelled.cancel_reason, Some(orders::CancelReason::Staff));
    let after = baker.get(&pending_path).await;
    after.shows(&dutch("staff-order-status-cancelled"));
    after.shows(&dutch("staff-order-action-cancelled"));
    after.shows("Bakker Bas");
    after.lacks(&dutch("staff-order-cancel"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_baker_moves_an_order_and_the_list_filters_by_location_and_status(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let order_path = format!("/staff/orders/{}", paid.order.id.get());
    let list_path = format!("/staff/bake-days/{}/orders", paid.day.get());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let markt = paid.markt_slot.get().to_string();

    baker
        .post(&format!("{order_path}/pickup"), &[("slot", &markt)])
        .await
        .redirects_to(&format!("{order_path}?opgeslagen=1"));
    assert_eq!(
        stored_order(&pool, paid.order.id).await.pickup_slot_id,
        paid.markt_slot
    );
    baker
        .get(&order_path)
        .await
        .shows(&dutch("staff-order-action-pickup_moved"));

    let mut conn = pool.acquire().await.expect("conn");
    let markt_id: i64 =
        sqlx::query_scalar("SELECT id FROM pickup_location WHERE name = 'Markt Vledder'")
            .fetch_one(&mut *conn)
            .await
            .expect("markt");
    drop(conn);
    baker
        .get(&format!("{list_path}?locatie={markt_id}"))
        .await
        .shows("Test Klant");
    baker
        .get(&format!("{list_path}?locatie={}", DE_PROEF.get()))
        .await
        .lacks("Test Klant");
    baker
        .get(&format!("{list_path}?status=cancelled"))
        .await
        .lacks("Test Klant");
    baker
        .get(&format!("{list_path}?status=paid&locatie="))
        .await
        .shows("Test Klant");
    let bad = baker.get(&format!("{list_path}?status=kwijt")).await;
    assert_eq!(bad.status, StatusCode::OK);
    bad.shows("Test Klant");

    let other = baker
        .post(&format!("{order_path}/pickup"), &[("slot", "999999")])
        .await;
    assert_eq!(other.status, StatusCode::UNPROCESSABLE_ENTITY);
    other.shows(&dutch("staff-order-error-slot"));
}

async fn company_details(pool: &PgPool) {
    sqlx::query("UPDATE company SET address = 'Teststraat 1, 1234 AB Testdorp', kvk = '12345678'")
        .execute(pool)
        .await
        .expect("company");
}

/// Kind and plain text of every queued email to `to`, oldest first.
async fn emails_to(pool: &PgPool, to: &str) -> Vec<(String, String)> {
    sqlx::query_as("SELECT kind, text_body FROM email_outbox WHERE to_address = $1 ORDER BY id")
        .bind(to)
        .fetch_all(pool)
        .await
        .expect("outbox")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_baker_cancels_a_paid_order_and_mollie_refunds_it_once(pool: PgPool) {
    let app = app(pool.clone());
    company_details(&pool).await;
    let paid = paid_order(&app).await;
    let path = format!("/staff/orders/{}", paid.order.id.get());
    let amount = i18n::money(paid.order.total.cents());
    let baker = client(&app, "Bakker Bas", Role::Baker).await;

    baker.get(&path).await.shows(&dutch_with(
        "staff-order-cancel-paid-text",
        &[
            ("number", &paid.order.number.to_string()),
            ("name", "Test Klant"),
            ("amount", &amount),
        ],
    ));
    baker
        .post(&format!("{path}/cancel"), &[])
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let cancelled = stored_order(&pool, paid.order.id).await;
    assert_eq!(cancelled.status, orders::OrderStatus::Cancelled);
    assert_eq!(cancelled.cancel_reason, Some(orders::CancelReason::Staff));
    assert_eq!(app.mollie.refunds(&paid.payment), [paid.order.total]);
    let emails = emails_to(&pool, "klant@example.org").await;
    let cancellation: Vec<_> = emails
        .iter()
        .filter(|(kind, _)| kind == "order_cancelled")
        .collect();
    assert_eq!(cancellation.len(), 1);
    assert!(cancellation[0].1.contains(&amount), "{}", cancellation[0].1);

    let page = baker.get(&path).await;
    page.shows(&dutch("staff-order-refund-done"));
    page.shows(&dutch("staff-order-action-refund_done"));
    page.lacks(&dutch("staff-order-refund-retry"));

    let again = baker.post(&format!("{path}/cancel"), &[]).await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
    again.shows(&dutch("staff-order-error-status"));
    assert_eq!(app.mollie.refunds(&paid.payment).len(), 1);
    assert_eq!(
        emails_to(&pool, "klant@example.org").await.len(),
        emails.len()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_failed_refund_shows_and_is_retried_once(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let path = format!("/staff/orders/{}", paid.order.id.get());
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    app.mollie.refuse_refunds(true);
    owner
        .post(&format!("{path}/cancel"), &[])
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    assert_eq!(
        stored_order(&pool, paid.order.id).await.status,
        orders::OrderStatus::Cancelled
    );
    assert!(app.mollie.refunds(&paid.payment).is_empty());
    let page = owner.get(&path).await;
    page.shows(&dutch("staff-order-refund-failed"));
    page.shows(&dutch("staff-order-refund-retry"));
    let refund: i64 = sqlx::query_scalar("SELECT id FROM refund")
        .fetch_one(&pool)
        .await
        .expect("refund");
    let retry = format!("{path}/refunds/{refund}/retry");

    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    assert_eq!(
        counter.post(&retry, &[]).await.status,
        StatusCode::FORBIDDEN
    );
    let no_token = owner
        .send(
            Request::post(&retry)
                .header(COOKIE, &owner.cookie)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(no_token.status, StatusCode::FORBIDDEN);
    assert_eq!(
        owner
            .post(&format!("{path}/refunds/999999/retry"), &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    app.mollie.refuse_refunds(false);
    owner
        .post(&retry, &[])
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    owner
        .post(&retry, &[])
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    assert_eq!(app.mollie.refunds(&paid.payment), [paid.order.total]);
    owner
        .get(&path)
        .await
        .shows(&dutch("staff-order-refund-done"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn moving_an_order_withdraws_its_unsent_reminder(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let path = format!("/staff/orders/{}", paid.order.id.get());
    let key = jobs::order_emails::reminder_key(paid.order.id);
    sqlx::query(
        "INSERT INTO email_outbox (dedupe_key, kind, to_address, subject, text_body, html_body,
            next_attempt_at, created_at)
        VALUES ($1, 'pickup_reminder', 'klant@example.org', 'Morgen', 'Hoi', '<p>Hoi</p>', now(),
            now())",
    )
    .bind(&key)
    .execute(&pool)
    .await
    .expect("reminder");
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    baker
        .post(
            &format!("{path}/pickup"),
            &[("slot", &paid.markt_slot.get().to_string())],
        )
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM email_outbox WHERE dedupe_key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(left, 0);
}

async fn slots_of(pool: &PgPool, day: BakeDayId) -> Vec<bakedays::PickupSlot> {
    let mut conn = pool.acquire().await.expect("conn");
    days::list_slots(&mut conn, day).await.expect("slots")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_baker_edits_adds_and_removes_pickup_slots(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}", day.get());
    let slot = slots_of(&pool, day).await[0];
    let id = slot.id.get();
    let edit = format!("{path}/slots/{id}");
    let date_field = format!("slot_date_{id}");
    let starts_field = format!("slot_starts_{id}");
    let ends_field = format!("slot_ends_{id}");

    let page = baker.get(&path).await;
    page.shows(&format!(r#"name="{date_field}" value="2026-12-05""#));
    page.shows(&format!(r#"name="{starts_field}" value="11:00""#));
    page.shows(&format!(r#"name="{ends_field}" value="12:00""#));
    page.shows(&dutch("slot-add"));
    page.lacks(&dutch("slot-remove"));

    let fields = |date: &'static str, starts: &'static str, ends: &'static str| {
        vec![
            (date_field.as_str(), date),
            (starts_field.as_str(), starts),
            (ends_field.as_str(), ends),
        ]
    };
    for (date, starts, ends, message) in [
        ("2026-12-05", "13:00", "12:00", dutch("slot-error-order")),
        (
            "2026-12-05",
            "04:30",
            "06:00",
            dutch_with("slot-error-before-bake", &[("time", "05:00")]),
        ),
        ("", "13:00", "14:00", dutch("form-error-date")),
        ("2026-12-05", "", "14:00", dutch("form-error-time")),
    ] {
        let refused = baker.post(&edit, &fields(date, starts, ends)).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{starts}");
        refused.shows(&message);
        refused.shows(&format!(r#"name="{starts_field}" value="{starts}""#));
    }
    assert_eq!(slots_of(&pool, day).await, [slot]);

    baker
        .post(&edit, &fields("2026-12-06", "13:00", "14:30"))
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let moved = slots_of(&pool, day).await[0];
    assert_eq!(
        (moved.starts_at, moved.ends_at),
        (ts("2026-12-06T12:00:00Z"), ts("2026-12-06T13:30:00Z"))
    );
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM email_outbox")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(queued, 0, "no orders, so no emails");

    let add = format!("{path}/slots");
    let refused = baker
        .post(
            &add,
            &[
                ("slot_location_new", "1"),
                ("slot_date_new", "2026-12-05"),
                ("slot_starts_new", "16:00"),
                ("slot_ends_new", "15:00"),
            ],
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.shows(&dutch("slot-error-order"));
    baker
        .post(
            &add,
            &[
                ("slot_location_new", "1"),
                ("slot_date_new", "2026-12-05"),
                ("slot_starts_new", "15:00"),
                ("slot_ends_new", "16:00"),
            ],
        )
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
    let slots = slots_of(&pool, day).await;
    assert_eq!(slots.len(), 2);
    let added = slots[0];
    assert_eq!(
        (added.starts_at, added.ends_at, added.location_id),
        (
            ts("2026-12-05T14:00:00Z"),
            ts("2026-12-05T15:00:00Z"),
            DE_PROEF
        )
    );
    baker.get(&path).await.shows(&dutch("slot-remove"));

    let removed = baker
        .htmx(&format!("{path}/slots/{}/remove", added.id.get()), &[])
        .await;
    assert_eq!(
        removed.header("hx-redirect"),
        format!("{path}?opgeslagen=1")
    );
    assert_eq!(slots_of(&pool, day).await, [moved]);

    let last = format!("{edit}/remove");
    let refused = baker.htmx(&last, &[]).await;
    assert_eq!(refused.status, StatusCode::OK);
    refused.shows(&dutch("day-error-no-pickup-slot"));
    let refused = baker.post(&last, &[]).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    refused.shows(&dutch("day-error-no-pickup-slot"));
    assert_eq!(slots_of(&pool, day).await, [moved]);

    // Saturday 5 December, 12:00 in Frederiksoord: baking has started.
    app.clock.set(ts("2026-12-05T11:00:00Z"));
    let late = staff(&pool, "Bakker Bea", Role::Baker).await;
    let baker = sign_in(&app, &late).await;
    for url in [edit.clone(), add.clone()] {
        let refused = baker
            .post(
                &url,
                &[
                    (date_field.as_str(), "2026-12-05"),
                    (starts_field.as_str(), "10:30"),
                    (ends_field.as_str(), "11:30"),
                    ("slot_location_new", "1"),
                    ("slot_date_new", "2026-12-05"),
                    ("slot_starts_new", "10:30"),
                    ("slot_ends_new", "12:00"),
                ],
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{url}");
        refused.shows(&dutch("slot-error-past"));
    }
    assert_eq!(slots_of(&pool, day).await, [moved]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_owner_and_baker_change_slots_and_only_with_the_csrf_token(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let path = format!("/staff/bake-days/{}", day.get());
    let slot = slots_of(&pool, day).await[0];
    let id = slot.id.get();
    let date_field = format!("slot_date_{id}");
    let starts_field = format!("slot_starts_{id}");
    let ends_field = format!("slot_ends_{id}");
    let edit_fields = vec![
        (date_field.as_str(), "2026-12-05"),
        (starts_field.as_str(), "13:00"),
        (ends_field.as_str(), "14:00"),
    ];
    let add_fields = vec![
        ("slot_location_new", "1"),
        ("slot_date_new", "2026-12-05"),
        ("slot_starts_new", "15:00"),
        ("slot_ends_new", "16:00"),
    ];
    let requests = [
        (format!("{path}/slots/{id}"), edit_fields),
        (format!("{path}/slots"), add_fields),
        (format!("{path}/slots/{id}/remove"), Vec::new()),
    ];

    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    for (url, fields) in &requests {
        let reply = counter.post(url, fields).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{url}");
    }
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    for (url, fields) in &requests {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let reply = owner
            .send(
                Request::post(url.as_str())
                    .header(COOKIE, &owner.cookie)
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{url}");
    }
    assert_eq!(slots_of(&pool, day).await, [slot]);

    owner
        .post(&requests[0].0, &requests[0].1)
        .await
        .redirects_to(&format!("{path}?opgeslagen=1"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn moving_a_slot_with_orders_asks_first_emails_the_customers_and_withdraws_reminders(
    pool: PgPool,
) {
    let app = app(pool.clone());
    company_details(&pool).await;
    let paid = paid_order(&app).await;
    let path = format!("/staff/bake-days/{}", paid.day.get());
    let slot = paid.order.pickup_slot_id;
    let id = slot.get();
    let key = jobs::order_emails::reminder_key(paid.order.id);
    sqlx::query(
        "INSERT INTO email_outbox (dedupe_key, kind, to_address, subject, text_body, html_body,
            next_attempt_at, created_at)
        VALUES ($1, 'pickup_reminder', 'klant@example.org', 'Morgen', 'Hoi', '<p>Hoi</p>', now(),
            now())",
    )
    .bind(&key)
    .execute(&pool)
    .await
    .expect("reminder");
    let baker = client(&app, "Bakker Bas", Role::Baker).await;

    let page = baker.get(&path).await;
    page.shows(&dutch_count("slot-move-text", 1));
    page.shows(&format!(r#"data-dialog-open="slot-{id}""#));
    page.shows(&format!(r#"id="slot-{id}""#));
    page.shows(r#"name="bevestigd" value="ja""#);

    let date_field = format!("slot_date_{id}");
    let starts_field = format!("slot_starts_{id}");
    let ends_field = format!("slot_ends_{id}");
    let move_to = |starts: &'static str| {
        vec![
            (date_field.as_str(), "2026-12-05"),
            (starts_field.as_str(), starts),
            (ends_field.as_str(), "15:30"),
            ("bevestigd", "ja"),
        ]
    };
    let pickup_moved = async || -> Vec<String> {
        emails_to(&pool, "klant@example.org")
            .await
            .into_iter()
            .filter(|(kind, _)| kind == "pickup_moved")
            .map(|(_, text)| text)
            .collect()
    };
    let refused = baker
        .htmx(&format!("{path}/slots/{id}"), &move_to("16:00"))
        .await;
    assert_eq!(refused.status, StatusCode::OK);
    refused.shows(&dutch("slot-error-order"));
    assert!(pickup_moved().await.is_empty());

    let unconfirmed = baker
        .post(&format!("{path}/slots/{id}"), &move_to("14:00")[..3])
        .await;
    assert_eq!(unconfirmed.status, StatusCode::CONFLICT);
    unconfirmed.shows(&dutch("slot-error-confirm"));
    assert!(pickup_moved().await.is_empty());
    assert_eq!(
        slots_of(&pool, paid.day).await[0].starts_at,
        ts("2026-12-05T10:00:00Z")
    );

    let moved = baker
        .htmx(&format!("{path}/slots/{id}"), &move_to("14:00"))
        .await;
    assert_eq!(moved.header("hx-redirect"), format!("{path}?opgeslagen=1"));
    let notices = pickup_moved().await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("14:00"), "{}", notices[0]);
    assert!(notices[0].contains(&paid.order.number.to_string()));
    assert!(
        !emails_to(&pool, "klant@example.org")
            .await
            .iter()
            .any(|(kind, _)| kind == "pickup_reminder"),
        "the unsent reminder is withdrawn, so the job queues one with the new time"
    );

    baker
        .htmx(&format!("{path}/slots/{id}"), &move_to("15:00"))
        .await;
    let notices = pickup_moved().await;
    assert_eq!(
        notices.len(),
        1,
        "the unsent notice is replaced: {notices:?}"
    );
    assert!(notices[0].contains("15:00–15:30"), "{}", notices[0]);

    baker
        .htmx(&format!("{path}/slots/{id}"), &move_to("15:00"))
        .await;
    assert_eq!(
        pickup_moved().await,
        notices,
        "saving the same times again sends nothing"
    );

    let refused = baker.htmx(&format!("{path}/slots/{id}/remove"), &[]).await;
    refused.shows(&dutch("slot-error-has-orders"));

    let markt = paid.markt_slot.get();
    baker
        .get(&path)
        .await
        .shows(&format!(r#"data-dialog-open="slot-remove-{markt}""#));
    sqlx::query(
        r#"INSERT INTO "order" (number_year, number_seq, customer_id, customer_name, bake_day_id,
            pickup_slot_id, channel, payment_mode, status, cancel_reason, total_cents, created_at)
        SELECT number_year, number_seq + 1, customer_id, customer_name, bake_day_id, $2, channel,
            payment_mode, 'cancelled', 'hold_expired', total_cents, created_at
        FROM "order" WHERE id = $1"#,
    )
    .bind(paid.order.id.get())
    .bind(markt)
    .execute(&pool)
    .await
    .expect("cancelled order");
    baker
        .get(&path)
        .await
        .lacks(&format!(r#"data-dialog-open="slot-remove-{markt}""#));
    assert_eq!(slots_of(&pool, paid.day).await.len(), 2);

    let order_path = format!("/staff/orders/{}", paid.order.id.get());
    baker
        .post(
            &format!("{order_path}/pickup"),
            &[("slot", &markt.to_string())],
        )
        .await
        .redirects_to(&format!("{order_path}?opgeslagen=1"));
    assert!(
        pickup_moved().await.is_empty(),
        "moving or cancelling the order withdraws the unsent notice"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn bake_day_page_links_the_sick_day_cancel_for_the_owner_only(pool: PgPool) {
    let app = app(pool.clone());
    let day = saturday(&pool).await;
    let page = format!("/staff/bake-days/{}", day.get());
    let link = format!("href=\"/staff/bake-days/{}/cancel\"", day.get());
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;

    let shown = owner.get(&page).await;
    shown.shows(&dutch("staff-day-cancel-sick"));
    shown.shows(&link);
    let hidden = baker.get(&page).await;
    assert_eq!(hidden.status, StatusCode::OK);
    hidden.lacks(&dutch("staff-day-cancel-sick"));
    hidden.lacks(&link);

    let mut conn = pool.acquire().await.expect("conn");
    days::cancel(&mut conn, app.clock.as_ref(), day, "Ziek")
        .await
        .expect("cancel");
    owner.get(&page).await.lacks(&link);

    sqlx::query("UPDATE bake_day SET status = 'done', cancel_message = NULL WHERE id = $1")
        .bind(day.get())
        .execute(&mut *conn)
        .await
        .expect("done");
    drop(conn);
    owner.get(&page).await.lacks(&link);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_the_owner_cancels_a_bake_day_and_can_run_it_again(pool: PgPool) {
    let app = app(pool.clone());
    company_details(&pool).await;
    let paid = paid_order(&app).await;
    let cancel = format!("/staff/bake-days/{}/cancel", paid.day.get());
    let list = format!("/staff/bake-days/{}/orders", paid.day.get());

    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    baker
        .get(&list)
        .await
        .lacks(&dutch("staff-orders-cancel-day"));
    assert_eq!(baker.get(&cancel).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        baker.post(&cancel, &[("bericht", "Ziek")]).await.status,
        StatusCode::FORBIDDEN
    );

    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    owner
        .get(&list)
        .await
        .shows(&dutch("staff-orders-cancel-day"));
    let page = owner.get(&cancel).await;
    assert_eq!(page.status, StatusCode::OK);
    page.shows(&dutch("staff-sick-day-warning"));
    page.shows(&i18n::money(paid.order.total.cents()));
    let no_token = owner
        .send(
            Request::post(&cancel)
                .header(COOKIE, &owner.cookie)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("bericht=Ziek"))
                .expect("request"),
        )
        .await;
    assert_eq!(no_token.status, StatusCode::FORBIDDEN);
    let empty = owner.post(&cancel, &[("bericht", " ")]).await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    empty.shows(&dutch("staff-sick-day-error-message"));
    assert_eq!(
        stored_order(&pool, paid.order.id).await.status,
        orders::OrderStatus::Paid
    );

    app.mollie.refuse_refunds(true);
    owner
        .post(&cancel, &[("bericht", "Ik ben ziek, sorry!")])
        .await
        .redirects_to(&format!("{cancel}?opgeslagen=1"));
    let mut conn = pool.acquire().await.expect("conn");
    let day = days::get(&mut conn, paid.day).await.expect("day");
    drop(conn);
    assert_eq!(day.status, BakeDayStatus::Cancelled);
    assert_eq!(day.cancel_message.as_deref(), Some("Ik ben ziek, sorry!"));
    let order = stored_order(&pool, paid.order.id).await;
    assert_eq!(order.status, orders::OrderStatus::Cancelled);
    assert_eq!(
        order.cancel_reason,
        Some(orders::CancelReason::DayCancelled)
    );
    let after = owner.get(&cancel).await;
    after.shows(&dutch("staff-sick-day-cancelled"));
    after.shows("Ik ben ziek, sorry!");
    after.shows(&dutch("staff-order-refund-failed"));
    after.shows(&dutch("staff-sick-day-retry"));
    assert!(app.mollie.refunds(&paid.payment).is_empty());

    app.mollie.refuse_refunds(false);
    for _ in 0..2 {
        owner
            .post(&cancel, &[])
            .await
            .redirects_to(&format!("{cancel}?opgeslagen=1"));
    }
    assert_eq!(app.mollie.refunds(&paid.payment), [paid.order.total]);
    owner
        .get(&cancel)
        .await
        .shows(&dutch("staff-sick-day-all-done"));
    let sick: Vec<_> = emails_to(&pool, "klant@example.org")
        .await
        .into_iter()
        .filter(|(kind, _)| kind == "sick_day_cancelled")
        .collect();
    assert_eq!(sick.len(), 1);
    assert!(sick[0].1.contains("Ik ben ziek, sorry!"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_production_list_adds_extras_to_orders_on_screen_and_as_pdf(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let bread = paid.order.lines[0].product_id;
    let mut conn = pool.acquire().await.expect("conn");
    extras::add(
        &mut conn,
        paid.day,
        bread,
        &ExtraInput {
            qty: 3,
            purpose: Purpose::Freezer,
            note: None,
        },
        &bakedays::NoOrders,
    )
    .await
    .expect("extra");
    drop(conn);
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = format!("/staff/bake-days/{}/production", paid.day.get());

    let page = baker.get(&path).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    page.shows(&dutch("production-title"));
    page.shows("Desembrood wit");
    page.shows(&dutch("production-extra-freezer"));
    page.shows("<td class=\"table-number\">2</td><td class=\"table-number\">3</td>");
    page.shows("<td class=\"table-number\">5</td></tr>");
    page.shows(&format!("href=\"{path}/pdf\""));

    let pdf = baker.get(&format!("{path}/pdf")).await;
    assert_eq!(pdf.status, StatusCode::OK);
    assert_eq!(pdf.header("content-type"), "application/pdf");
    assert_eq!(
        pdf.header("content-disposition"),
        "inline; filename=\"productielijst-2026-12-05.pdf\""
    );
    assert!(pdf.text.starts_with("%PDF-"));

    let day_page = baker
        .get(&format!("/staff/bake-days/{}", paid.day.get()))
        .await;
    day_page.shows(&format!("href=\"{path}\""));
    day_page.shows(&format!(
        "href=\"/staff/bake-days/{}/pickup-list\"",
        paid.day.get()
    ));

    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    for url in [path.clone(), format!("{path}/pdf")] {
        assert_eq!(
            counter.get(&url).await.status,
            StatusCode::FORBIDDEN,
            "{url}"
        );
    }
    let orders = counter
        .get(&format!("/staff/bake-days/{}/orders", paid.day.get()))
        .await;
    orders.lacks(&format!("href=\"{path}\""));
    orders.shows(&format!(
        "href=\"/staff/bake-days/{}/pickup-list\"",
        paid.day.get()
    ));
    assert_eq!(
        baker.get("/staff/bake-days/999999/production").await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn ingredient_needs_show_as_a_shopping_list_with_pdf_and_csv(pool: PgPool) {
    use recipes::{
        DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput,
        RecipeKind, ingredients, recipes as store,
    };

    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let bread = paid.order.lines[0].product_id;
    let mut conn = pool.acquire().await.expect("conn");
    let granola = products::create(
        &mut conn,
        &ProductInput {
            name_nl: "Granola".to_owned(),
            ..sourdough()
        },
    )
    .await
    .expect("product")
    .id;
    for (product, qty) in [(bread, 3), (granola, 4)] {
        extras::add(
            &mut conn,
            paid.day,
            product,
            &ExtraInput {
                qty,
                purpose: Purpose::Freezer,
                note: None,
            },
            &bakedays::NoOrders,
        )
        .await
        .expect("extra");
    }
    let mut ingredient = async |name: &str, role| {
        ingredients::create(
            &mut conn,
            &IngredientInput {
                name: name.to_owned(),
                label_name: String::new(),
                role,
                supplier_name: String::new(),
                allergens: std::collections::BTreeSet::new(),
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
            name: "Wit".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .expect("recipe");
    store::save_draft(
        &mut conn,
        version.id,
        &DraftInput {
            lines: vec![
                LineInput {
                    pct: Pct::from_thousandths(100_000),
                    part: LinePart::Ingredient(flour),
                },
                LineInput {
                    pct: Pct::from_thousandths(80_000),
                    part: LinePart::Ingredient(water),
                },
            ],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .expect("lines");
    store::link_product(&mut conn, recipe.id, bread)
        .await
        .expect("link");
    drop(conn);

    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let day = format!("/staff/bake-days/{}", paid.day.get());
    let path = format!("{day}/ingredients");
    // 2 ordered + 3 extra × 900 g = 4500 g at 180%.
    let page = baker.get(&path).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    page.shows(&dutch("needs-title"));
    page.shows("<td>Tarwebloem</td><td class=\"table-number\">2,500 kg</td>");
    page.shows("<td>Water</td><td class=\"table-number\">2,000 kg</td>");
    page.shows("4,500 kg");
    page.shows(&dutch_with(
        "needs-dough-name",
        &[("recipe", "Wit"), ("version", "1")],
    ));
    page.shows(&dutch("needs-missing"));
    page.shows("<td>Granola</td><td class=\"table-number\">4</td>");
    page.shows(&dutch("needs-reason-no-recipe"));
    page.shows(&format!("href=\"{path}/pdf\""));
    page.shows(&format!("href=\"{path}/csv\""));
    page.shows(&format!("href=\"{day}/production\""));

    let pdf = baker.get(&format!("{path}/pdf")).await;
    assert_eq!(pdf.status, StatusCode::OK);
    assert_eq!(pdf.header("content-type"), "application/pdf");
    assert_eq!(
        pdf.header("content-disposition"),
        "inline; filename=\"boodschappenlijst-2026-12-05.pdf\""
    );
    assert!(pdf.text.starts_with("%PDF-"));

    let csv = baker.get(&format!("{path}/csv")).await;
    assert_eq!(csv.status, StatusCode::OK);
    assert_eq!(csv.header("content-type"), "text/csv; charset=utf-8");
    assert_eq!(
        csv.header("content-disposition"),
        "attachment; filename=\"boodschappenlijst-2026-12-05.csv\""
    );
    assert!(
        csv.text.contains("Tarwebloem;2500\r\nWater;2000\r\n"),
        "{}",
        csv.text
    );
    assert!(csv.text.contains("Granola, 4 stuks"), "{}", csv.text);

    baker.get(&day).await.shows(&format!("href=\"{path}\""));
    baker
        .get(&format!("{day}/production"))
        .await
        .shows(&format!("href=\"{path}\""));

    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    for url in [path.clone(), format!("{path}/pdf"), format!("{path}/csv")] {
        assert_eq!(
            counter.get(&url).await.status,
            StatusCode::FORBIDDEN,
            "{url}"
        );
    }
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    assert_eq!(owner.get(&path).await.status, StatusCode::OK);
    for url in [
        "/staff/bake-days/999999/ingredients",
        "/staff/bake-days/999999/ingredients/pdf",
        "/staff/bake-days/999999/ingredients/csv",
    ] {
        assert_eq!(baker.get(url).await.status, StatusCode::NOT_FOUND, "{url}");
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_counter_ticks_off_pickups_on_the_pickup_list_and_can_undo(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let list = format!("/staff/bake-days/{}/pickup-list", paid.day.get());
    let n = paid.order.id.get();
    let card = format!("bestelling-{n}");
    let tick = format!("{list}/{n}/picked-up");
    let undo = format!("{list}/{n}/undo");

    let page = counter.get(&list).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    page.shows(&dutch("pickup-list-title"));
    page.shows("Test Klant");
    page.shows(&paid.order.number.to_string());
    page.shows("2× Desembrood wit");
    page.shows(&dutch("pickup-list-status-paid"));
    page.shows("Markt Vledder");
    page.shows(&dutch("pickup-list-empty"));
    page.shows(&format!("id=\"{card}\""));
    page.shows(&format!("action=\"{tick}\""));
    page.shows(&format!("href=\"{list}/pdf\""));

    counter
        .post(&tick, &[])
        .await
        .redirects_to(&format!("{list}?opgeslagen=1#{card}"));
    assert_eq!(
        stored_order(&pool, paid.order.id).await.status,
        orders::OrderStatus::PickedUp
    );
    counter
        .post(&tick, &[])
        .await
        .redirects_to(&format!("{list}?opgeslagen=1#{card}"));

    let undone = counter.htmx(&undo, &[]).await;
    assert_eq!(undone.status, StatusCode::OK);
    undone.shows(&format!("id=\"{card}\""));
    undone.shows(&format!("action=\"{tick}\""));
    undone.shows(&dutch("saved"));
    assert_eq!(
        stored_order(&pool, paid.order.id).await.status,
        orders::OrderStatus::Paid
    );

    let ticked = counter.htmx(&tick, &[]).await;
    assert_eq!(ticked.status, StatusCode::OK);
    ticked.shows(&format!("action=\"{undo}\""));
    ticked.shows(&dutch("pickup-list-status-picked-up"));

    let history = counter.get(&format!("/staff/orders/{n}")).await;
    history.shows(&dutch("staff-order-action-picked_up_undone"));

    let pdf = counter.get(&format!("{list}/pdf")).await;
    assert_eq!(pdf.status, StatusCode::OK);
    assert_eq!(pdf.header("content-type"), "application/pdf");
    assert_eq!(
        pdf.header("content-disposition"),
        "inline; filename=\"ophaallijst-2026-12-05.pdf\""
    );
    assert!(pdf.text.starts_with("%PDF-"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn pickup_ticks_need_the_token_and_an_order_of_that_day(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let list = format!("/staff/bake-days/{}/pickup-list", paid.day.get());
    let n = paid.order.id.get();
    for action in ["picked-up", "undo"] {
        let reply = baker
            .send(
                Request::post(format!("{list}/{n}/{action}"))
                    .header(COOKIE, &baker.cookie)
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{action}");
    }
    assert_eq!(
        stored_order(&pool, paid.order.id).await.status,
        orders::OrderStatus::Paid
    );

    let other = saturday_after(&pool).await;
    let elsewhere = format!("/staff/bake-days/{}/pickup-list/{n}/picked-up", other.get());
    assert_eq!(
        baker.post(&elsewhere, &[]).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        baker
            .post(&format!("{list}/999999/picked-up"), &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        stored_order(&pool, paid.order.id).await.status,
        orders::OrderStatus::Paid
    );
}

/// A draft drop on Saturday 12 December.
async fn saturday_after(pool: &PgPool) -> BakeDayId {
    let mut conn = pool.acquire().await.expect("conn");
    days::create(
        &mut conn,
        &NewBakeDay {
            date: date(2026, 12, 12),
            location_id: DE_PROEF,
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .expect("bake day")
    .id
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn refused_pickup_ticks_say_why(pool: PgPool) {
    let app = app(pool.clone());
    let paid = paid_order(&app).await;
    let list = format!("/staff/bake-days/{}/pickup-list", paid.day.get());

    let mut conn = pool.acquire().await.expect("conn");
    let mut cart = orders::Cart::new(paid.day);
    cart.set(paid.order.lines[0].product_id, 1, None)
        .expect("cart");
    let pending = orders::checkout(
        &mut conn,
        app.clock.as_ref(),
        &cart,
        &orders::CustomerInput {
            email: "later@example.org".to_owned(),
            name: "Late Klant".to_owned(),
            phone: None,
            marketing_opt_in: false,
        },
        paid.order.pickup_slot_id,
        orders::OrderedBy::Guest,
    )
    .await
    .expect("order");
    drop(conn);
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let off_list = counter
        .htmx(&format!("{list}/{}/picked-up", pending.id.get()), &[])
        .await;
    assert_eq!(off_list.status, StatusCode::OK);
    assert_eq!(off_list.header("hx-retarget"), "#status");
    assert_eq!(off_list.header("hx-reselect"), "#status");
    off_list.shows(&dutch("staff-order-error-status"));

    let n = paid.order.id.get();
    counter
        .post(&format!("{list}/{n}/picked-up"), &[])
        .await
        .redirects_to(&format!("{list}?opgeslagen=1#bestelling-{n}"));
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    owner
        .post(
            &format!("/staff/bake-days/{}/cancel", paid.day.get()),
            &[("bericht", "Ik ben ziek, sorry!")],
        )
        .await;
    let undo = counter.post(&format!("{list}/{n}/undo"), &[]).await;
    assert_eq!(undo.status, StatusCode::UNPROCESSABLE_ENTITY);
    undo.shows(&dutch("staff-order-error-day"));
    assert_eq!(
        stored_order(&pool, paid.order.id).await.status,
        orders::OrderStatus::PickedUp
    );
}

/// A paid order of two loaves (€ 13,00) on a drop for the Voedselbank.
async fn charity_order(app: &App) -> (CharityId, PaidOrder) {
    let mut conn = app.pool.acquire().await.expect("conn");
    let charity = charities::create(
        &mut conn,
        &CharityInput {
            name: "Voedselbank".to_owned(),
            url: "https://example.org".to_owned(),
            ..CharityInput::default()
        },
    )
    .await
    .expect("charity")
    .id;
    drop(conn);
    (charity, paid_order_for(app, Some(charity)).await)
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_the_owner_opens_the_reports(pool: PgPool) {
    let app = app(pool.clone());
    let (charity, _) = charity_order(&app).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let payout = format!("/staff/reports/payouts/{}", charity.get());
    let pages = [
        "/staff/reports",
        "/staff/reports/sales",
        "/staff/reports/sales.csv",
        "/staff/reports/pay-what-you-want.csv",
        "/staff/reports/donations",
        "/staff/reports/donations/drops.csv",
        "/staff/reports/donations/charities.csv",
        "/staff/reports/contribution",
        "/staff/reports/payouts",
        payout.as_str(),
    ];
    for url in pages {
        for who in [&baker, &counter] {
            assert_eq!(who.get(url).await.status, StatusCode::FORBIDDEN, "{url}");
        }
        assert_eq!(owner.get(url).await.status, StatusCode::OK, "{url}");
    }
    owner
        .get("/staff/reports")
        .await
        .shows("href=\"/staff/reports/sales\"");
    for url in [
        format!("{payout}/transfers"),
        "/staff/reports/contribution/costs".to_owned(),
    ] {
        assert_eq!(
            baker.post(&url, &[]).await.status,
            StatusCode::FORBIDDEN,
            "{url}"
        );
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_sales_report_shows_btw_per_month_and_exports_csv(pool: PgPool) {
    let app = app(pool.clone());
    paid_order(&app).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;

    let page = owner.get("/staff/reports/sales").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    page.shows("value=\"2026-01-01\"");
    page.shows("value=\"2026-12-31\"");
    page.shows(&i18n::money(1300));
    page.shows(&i18n::money(107));
    page.shows(&i18n::money(1193));
    page.shows("href=\"/staff/reports/sales.csv?van=2026-01-01&tot=2026-12-31&per=maand\"");

    let days = owner
        .get("/staff/reports/sales?van=2026-11-01&tot=2026-11-30&per=dag")
        .await;
    days.shows(&i18n::numeric_date(date(2026, 11, 24)));

    let csv = owner
        .get("/staff/reports/sales.csv?van=2026-11-01&tot=2026-11-30&per=maand")
        .await;
    assert_eq!(csv.status, StatusCode::OK);
    assert_eq!(csv.header("content-type"), "text/csv; charset=utf-8");
    assert_eq!(
        csv.header("content-disposition"),
        "attachment; filename=\"omzet-2026-11-01-2026-11-30.csv\""
    );
    csv.shows("Van;Tot en met;BTW-tarief");
    csv.shows("13,00;1,07;11,93");

    let wrong = owner
        .get("/staff/reports/sales?van=gisteren&tot=2026-01-01")
        .await;
    assert_eq!(wrong.status, StatusCode::OK);
    wrong.shows(&dutch("reports-error-date"));
    let backwards = owner
        .get("/staff/reports/sales?van=2026-12-01&tot=2026-01-01")
        .await;
    backwards.shows(&dutch("reports-error-order"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn donation_reports_show_drops_charities_and_charts(pool: PgPool) {
    let app = app(pool.clone());
    charity_order(&app).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;

    let page = owner.get("/staff/reports/donations").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    page.shows("Voedselbank");
    page.shows(&i18n::money(1300));
    page.shows("<svg class=\"chart-svg\"");
    page.shows(&dutch("reports-donations-drops-chart"));
    page.shows("class=\"chart-bar\" x=\"0\" y=\"26\" width=\"480\"");
    page.shows(&format!("<li>05-12-2026: {}</li>", i18n::money(1300)));

    let drops = owner.get("/staff/reports/donations/drops.csv").await;
    assert_eq!(drops.header("content-type"), "text/csv; charset=utf-8");
    drops.shows("05-12-2026;Voedselbank;1;13,00;0,00;13,00;13,00;0,00;0,00");
    let charities = owner.get("/staff/reports/donations/charities.csv").await;
    charities.shows("Voedselbank;13,00;13,00;0,00;13,00");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_owner_records_a_transfer_of_the_amount_due(pool: PgPool) {
    let app = app(pool.clone());
    let (charity, _) = charity_order(&app).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;

    let today = owner.get("/staff/reports/payouts").await;
    today.shows("value=\"2026-11-24\"");
    let list = owner
        .get("/staff/reports/payouts?van=2026-01-01&tot=2026-12-31")
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text);
    list.shows("Voedselbank");
    list.shows(&i18n::money(1300));
    let detail_path = format!("/staff/reports/payouts/{}", charity.get());
    list.shows(&format!(
        "href=\"{detail_path}?van=2026-01-01&tot=2026-12-31\""
    ));

    let detail = owner
        .get(&format!("{detail_path}?van=2026-01-01&tot=2026-12-31"))
        .await;
    assert_eq!(detail.status, StatusCode::OK);
    detail.shows("value=\"13,00\"");
    let transfers = format!("{detail_path}/transfers");
    let fields = |amount: &'static str, reference: &'static str| {
        vec![
            ("van", "2026-01-01"),
            ("tot", "2026-12-31"),
            ("amount", amount),
            ("transferred_on", "2026-12-07"),
            ("bank_reference", reference),
        ]
    };

    let no_token = owner
        .send(
            Request::post(&transfers)
                .header(COOKIE, &owner.cookie)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("amount=13%2C00"))
                .expect("request"),
        )
        .await;
    assert_eq!(no_token.status, StatusCode::FORBIDDEN);

    let wrong = owner.post(&transfers, &fields("12,00", "BVW-1")).await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch_with(
        "reports-error-amount",
        &[("due", &i18n::money(1300))],
    ));
    let mut early = fields("13,00", "BVW-1");
    early[1] = ("tot", "2026-12-01");
    let before_the_drop = owner.post(&transfers, &early).await;
    assert_eq!(before_the_drop.status, StatusCode::UNPROCESSABLE_ENTITY);
    before_the_drop.shows(&dutch("reports-nothing-due"));
    let mut garbled = fields("13,00", "BVW-1");
    garbled[1] = ("tot", "kerst");
    let bad_period = owner.post(&transfers, &garbled).await;
    assert_eq!(bad_period.status, StatusCode::UNPROCESSABLE_ENTITY);
    bad_period.shows(&dutch("reports-error-period"));
    let no_reference = owner.post(&transfers, &fields("13,00", " ")).await;
    assert_eq!(no_reference.status, StatusCode::UNPROCESSABLE_ENTITY);
    no_reference.shows(&dutch("reports-error-reference"));

    owner
        .post(&transfers, &fields("13,00", "BVW-1"))
        .await
        .redirects_to(&format!(
            "{detail_path}?van=2026-01-01&tot=2026-12-31&opgeslagen=1"
        ));
    let after = owner
        .get(&format!("{detail_path}?van=2026-01-01&tot=2026-12-31"))
        .await;
    after.shows("BVW-1");
    after.shows(&i18n::numeric_date(date(2026, 12, 7)));
    after.shows(&dutch("reports-nothing-due"));

    let again = owner.post(&transfers, &fields("13,00", "BVW-2")).await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
    again.shows(&dutch("reports-nothing-due"));
    assert_eq!(
        owner.get("/staff/reports/payouts/999999").await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_contribution_shows_what_the_bakery_covers(pool: PgPool) {
    let app = app(pool.clone());
    let (_, paid) = charity_order(&app).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let costs = "/staff/reports/contribution/costs";
    let day = paid.day.get().to_string();

    let page = owner.get("/staff/reports/contribution").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    page.shows(&i18n::money(107));
    page.shows(&dutch_count("reports-unknown-fees", 1));
    page.shows("Desembrood wit");

    let wrong = owner
        .post(
            costs,
            &[
                ("bake_day_id", &day),
                ("kind", "packaging"),
                ("amount", "nul"),
            ],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.shows(&dutch("reports-error-cost"));

    owner
        .post(
            costs,
            &[
                ("bake_day_id", &day),
                ("kind", "packaging"),
                ("amount", "2,50"),
                ("note", "zakken"),
            ],
        )
        .await
        .redirects_to("/staff/reports/contribution?van=2026-01-01&tot=2026-12-31&opgeslagen=1");
    let after = owner.get("/staff/reports/contribution").await;
    after.shows(&i18n::money(250));
    after.shows(&i18n::money(357));
    after.shows(&dutch_with(
        "reports-per-euro",
        &[("amount", &i18n::money(27))],
    ));
}

/// An open drop on Saturday 5 December with the approved sourdough (€ 6,50, 9%) on its range.
async fn open_drop(app: &App) -> (BakeDayId, ProductId) {
    let pool = &app.pool;
    let day = saturday(pool).await;
    let bread = product(pool).await;
    let approver = staff(pool, "Keurder Karin", Role::Owner).await;
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
        },
    )
    .await
    .expect("schedule");
    days::open(&mut conn, app.clock.as_ref(), day)
        .await
        .expect("open");
    (day, bread)
}

fn business_form<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut fields = vec![
        ("contact_email", "inkoop@dehoek.example"),
        ("contact_name", "Inkoop De Hoek"),
        ("company_name", "Café De Hoek"),
        ("kvk", "12345678"),
        ("btw_id", "NL123456789B01"),
        ("invoice_address", "Dorpsstraat 1, 8382 AA Frederiksoord"),
        ("invoice_email", "factuur@dehoek.example"),
        ("on_account_allowed", "ja"),
    ];
    fields.retain(|(name, _)| !extra.iter().any(|(other, _)| other == name));
    fields.extend_from_slice(extra);
    fields
}

/// Opens the account through the form and returns its page.
async fn business_account(owner: &Client<'_>) -> String {
    let reply = owner
        .post("/staff/business-accounts/new", &business_form(&[]))
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text);
    reply
        .header(LOCATION.as_str())
        .trim_end_matches("?opgeslagen=1")
        .to_owned()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_the_owner_manages_business_accounts_and_invoices(pool: PgPool) {
    let app = app(pool.clone());
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;

    let without_token = owner
        .send(
            Request::post("/staff/business-accounts/new")
                .header(COOKIE, &owner.cookie)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("company_name=X"))
                .expect("request"),
        )
        .await;
    assert_eq!(without_token.status, StatusCode::FORBIDDEN);
    assert_eq!(
        baker
            .post("/staff/business-accounts/new", &business_form(&[]))
            .await
            .status,
        StatusCode::FORBIDDEN
    );

    let account = business_account(&owner).await;
    let invoice = format!("{account}/invoice");
    let csv = format!("{account}/invoice.csv");
    assert_eq!(
        owner.get(&format!("{csv}?van=gisteren")).await.status,
        StatusCode::BAD_REQUEST
    );
    let list = baker.get("/staff/business-accounts").await;
    assert_eq!(list.status, StatusCode::OK);
    list.shows(&format!("href=\"{account}/order\""));
    list.lacks(&format!("href=\"{account}\""));
    list.lacks("href=\"/staff/business-accounts/new\"");
    assert_eq!(
        counter.get("/staff/business-accounts").await.status,
        StatusCode::FORBIDDEN
    );
    owner
        .get("/staff/business-accounts")
        .await
        .shows(&format!("href=\"{account}/invoice\""));
    for url in [
        "/staff/business-accounts/new",
        account.as_str(),
        invoice.as_str(),
        csv.as_str(),
    ] {
        for who in [&baker, &counter] {
            assert_eq!(who.get(url).await.status, StatusCode::FORBIDDEN, "{url}");
        }
        assert_eq!(owner.get(url).await.status, StatusCode::OK, "{url}");
    }
    for url in [account.clone(), format!("{account}/invoices")] {
        assert_eq!(
            baker.post(&url, &business_form(&[])).await.status,
            StatusCode::FORBIDDEN,
            "{url}"
        );
    }
    let order = format!("{account}/order");
    assert_eq!(baker.get(&order).await.status, StatusCode::OK);
    assert_eq!(counter.get(&order).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        counter.post(&order, &[]).await.status,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn wrong_business_account_input_gets_dutch_messages(pool: PgPool) {
    let app = app(pool.clone());
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let reply = owner
        .post(
            "/staff/business-accounts/new",
            &business_form(&[
                ("kvk", "1234"),
                ("invoice_email", "geen mail"),
                ("company_name", " "),
            ]),
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    reply.shows(&dutch("b2b-error-kvk"));
    reply.shows(&dutch("b2b-error-email"));
    reply.shows(&dutch("b2b-error-required"));

    business_account(&owner).await;
    let again = owner
        .post("/staff/business-accounts/new", &business_form(&[]))
        .await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
    again.shows(&dutch("b2b-error-exists"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_baker_orders_on_account_and_the_owner_invoices_it(pool: PgPool) {
    let app = app(pool.clone());
    company_details(&pool).await;
    let (day, bread) = open_drop(&app).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let account = business_account(&owner).await;
    owner
        .get("/staff/business-accounts")
        .await
        .shows("Café De Hoek");

    let order_page = format!("{account}/order");
    let form = baker.get(&format!("{order_page}?dag={}", day.get())).await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.text);
    form.shows("Desembrood wit");
    let mut conn = pool.acquire().await.expect("conn");
    let slot = days::list_slots(&mut conn, day).await.expect("slots")[0].id;
    drop(conn);
    let day_id = day.get().to_string();
    let slot_id = slot.get().to_string();
    let pieces = format!("qty_{}", bread.get());
    let bread_id = bread.get().to_string();

    let empty = baker
        .post(
            &order_page,
            &[
                ("dag", &day_id),
                ("slot", &slot_id),
                ("product", &bread_id),
                (&pieces, "0"),
            ],
        )
        .await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    empty.shows(&dutch("b2b-error-empty"));
    let gone = baker
        .post(
            &order_page,
            &[
                ("dag", &day_id),
                ("slot", &slot_id),
                ("product", "999999"),
                ("qty_999999", "1"),
            ],
        )
        .await;
    assert_eq!(gone.status, StatusCode::UNPROCESSABLE_ENTITY);
    gone.shows(&dutch("b2b-error-product"));
    let too_many = baker
        .post(
            &order_page,
            &[
                ("dag", &day_id),
                ("slot", &slot_id),
                ("product", &bread_id),
                (&pieces, "11"),
            ],
        )
        .await;
    assert_eq!(too_many.status, StatusCode::UNPROCESSABLE_ENTITY);
    too_many.shows(&dutch_with(
        "b2b-error-capacity",
        &[("product", "Desembrood wit"), ("n", "10")],
    ));

    let placed = baker
        .post(
            &order_page,
            &[
                ("dag", &day_id),
                ("slot", &slot_id),
                ("product", &bread_id),
                (&pieces, "2"),
            ],
        )
        .await;
    assert_eq!(placed.status, StatusCode::SEE_OTHER, "{}", placed.text);
    let order_id: i64 = sqlx::query_scalar(
        r#"SELECT id FROM "order" WHERE payment_mode = 'on_account' AND status = 'confirmed'"#,
    )
    .fetch_one(&pool)
    .await
    .expect("order on account");
    placed.redirects_to(&format!("/staff/orders/{order_id}?opgeslagen=1"));
    let queued: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT kind, to_address, cc_address, text_body FROM email_outbox ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("outbox");
    assert_eq!(queued.len(), 1, "{queued:?}");
    let (kind, to, cc, text) = &queued[0];
    assert_eq!(kind, "order_confirmation");
    assert_eq!(to, "inkoop@dehoek.example");
    assert_eq!(cc.as_deref(), Some("factuur@dehoek.example"));
    assert!(text.contains("op rekening"), "{text}");
    let order = stored_order(&pool, domain::OrderId::new(order_id)).await;
    assert_eq!(order.total, Money::from_cents(1300));

    let overview = owner
        .get(&format!("{account}/invoice?van=2026-12-01&tot=2026-12-31"))
        .await;
    assert_eq!(overview.status, StatusCode::OK, "{}", overview.text);
    overview.shows(&order.number.to_string());
    overview.shows(&i18n::money(1193));
    overview.shows(&i18n::money(107));
    overview.shows(&i18n::money(1300));
    overview.shows(&format!(
        "href=\"{account}/invoice.csv?van=2026-12-01&tot=2026-12-31\""
    ));
    let november = owner
        .get(&format!("{account}/invoice?van=2026-11-01&tot=2026-11-30"))
        .await;
    november.lacks(&order.number.to_string());
    november.shows(&dutch("b2b-invoice-none"));

    let csv = owner
        .get(&format!(
            "{account}/invoice.csv?van=2026-12-01&tot=2026-12-31"
        ))
        .await;
    assert_eq!(csv.status, StatusCode::OK);
    assert_eq!(csv.header("content-type"), "text/csv; charset=utf-8");
    csv.shows("Desembrood wit;2;5,96;9%;11,93;1,07;13,00");

    let mark = format!("{account}/invoices");
    let id = order_id.to_string();
    let fields = |number: &'static str| {
        vec![
            ("van", "2026-12-01"),
            ("tot", "2026-12-31"),
            ("invoice_number", number),
            ("invoice_date", "2026-12-06"),
        ]
    };
    let mut without_number = fields("");
    without_number.push(("order", &id));
    let refused = owner.post(&mark, &without_number).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.shows(&dutch("b2b-error-number"));

    let mut good = fields("2026-F001");
    good.push(("order", &id));
    owner.post(&mark, &good).await.redirects_to(&format!(
        "{account}/invoice?van=2026-12-01&tot=2026-12-31&opgeslagen=1"
    ));
    let after = owner
        .get(&format!("{account}/invoice?van=2026-12-01&tot=2026-12-31"))
        .await;
    after.shows(&dutch("b2b-invoice-none"));
    after.shows("2026-F001");
    let twice = owner.post(&mark, &good).await;
    assert_eq!(twice.status, StatusCode::UNPROCESSABLE_ENTITY);
    twice.shows(&dutch("b2b-error-changed"));

    let closed = owner
        .post(
            &account,
            &business_form(&[])
                .into_iter()
                .filter(|(name, _)| *name != "on_account_allowed")
                .collect::<Vec<_>>(),
        )
        .await;
    assert_eq!(closed.status, StatusCode::SEE_OTHER, "{}", closed.text);
    let refused = baker
        .post(
            &order_page,
            &[
                ("dag", &day_id),
                ("slot", &slot_id),
                ("product", &bread_id),
                (&pieces, "1"),
            ],
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.shows(&dutch("b2b-error-not-on-account"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_order_on_account_is_placed_without_its_email_when_company_details_are_missing(
    pool: PgPool,
) {
    let app = app(pool.clone());
    let (day, bread) = open_drop(&app).await;
    let owner = client(&app, "Eigenaar Eva", Role::Owner).await;
    let account = business_account(&owner).await;
    let mut conn = pool.acquire().await.expect("conn");
    let slot = days::list_slots(&mut conn, day).await.expect("slots")[0].id;
    drop(conn);
    let pieces = format!("qty_{}", bread.get());
    let placed = owner
        .post(
            &format!("{account}/order"),
            &[
                ("dag", day.get().to_string().as_str()),
                ("slot", slot.get().to_string().as_str()),
                ("product", bread.get().to_string().as_str()),
                (pieces.as_str(), "1"),
            ],
        )
        .await;
    assert_eq!(placed.status, StatusCode::SEE_OTHER, "{}", placed.text);
    let orders: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "order" WHERE payment_mode = 'on_account'"#)
            .fetch_one(&pool)
            .await
            .expect("orders");
    assert_eq!(orders, 1);
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM email_outbox")
        .fetch_one(&pool)
        .await
        .expect("outbox");
    assert_eq!(queued, 0);
}
