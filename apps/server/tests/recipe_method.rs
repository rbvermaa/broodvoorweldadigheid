//! The instructions of a recipe version on its pages: free text or steps with times
//! (F14, ADR 0049).
#![forbid(unsafe_code)]

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use auth::{NewStaff, Role};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_TYPE, COOKIE, LOCATION};
use axum::http::{HeaderMap, Request, StatusCode};
use db::PgPool;
use domain::{Clock, RecipeId, RecipeVersionId};
use i18n::{FluentArgs, Translator};
use recipes::method::{self, stamp};
use recipes::{Method, MethodKind, RecipeInput, RecipeKind, Step, recipes as store};
use server::AppState;
use server::config::Config;
use support::signed_in_at;
use testkit::FakeClock;
use tower::ServiceExt;

/// Friday 4 December 2026, 09:58 in Frederiksoord.
const NOW: &str = "2026-12-04T08:58:00Z";

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

/// The answer as sent: entities are not decoded, so escaping shows.
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    text: String,
}

fn escaped(text: &str) -> String {
    text.replace('&', "&#38;")
        .replace('<', "&#60;")
        .replace('>', "&#62;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
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
    fn shows_text(&self, text: &str) {
        self.shows(&escaped(text));
    }

    #[track_caller]
    fn lacks(&self, text: &str) {
        assert!(!self.text.contains(text), "unexpected {text:?}");
    }

    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .map(|v| v.to_str().expect("ascii"))
            .unwrap_or_default()
    }

    #[track_caller]
    fn redirects_to(&self, location: &str) {
        assert_eq!(self.status, StatusCode::SEE_OTHER, "{}", self.text);
        assert_eq!(self.header(LOCATION.as_str()), location);
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
            text: std::str::from_utf8(&bytes).expect("utf-8").to_owned(),
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

    async fn post_as(&self, path: &str, fields: &[(&str, &str)], htmx: bool) -> Reply {
        let mut all: Vec<(&str, &str)> = vec![("_csrf", &self.token)];
        all.extend_from_slice(fields);
        self.post_raw(path, &all, htmx).await
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        self.post_as(path, fields, false).await
    }

    async fn post_htmx(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        self.post_as(path, fields, true).await
    }

    async fn post_raw(&self, path: &str, fields: &[(&str, &str)], htmx: bool) -> Reply {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let mut request = Request::post(path)
            .header(COOKIE, &self.cookie)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded");
        if htmx {
            request = request.header("hx-request", "true");
        }
        self.send(request.body(Body::from(body)).expect("request"))
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

async fn recipe(pool: &PgPool) -> (RecipeId, RecipeVersionId) {
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
    (recipe.id, version.id)
}

fn version_path(recipe: RecipeId, version: RecipeVersionId) -> String {
    format!("/staff/recipes/{}/versions/{}", recipe.get(), version.get())
}

async fn stored(pool: &PgPool, version: RecipeVersionId) -> Method {
    let mut conn = pool.acquire().await.expect("conn");
    method::method(&mut conn, version).await.expect("method")
}

async fn seen(pool: &PgPool, version: RecipeVersionId) -> String {
    stamp(&stored(pool, version).await)
}

fn step(name: &str, minutes: Option<u32>) -> Step {
    Step {
        name: name.to_owned(),
        duration_min: minutes,
        temperature_c: None,
        notes: String::new(),
    }
}

async fn with_steps(pool: &PgPool, version: RecipeVersionId, steps: &[Step]) {
    let mut conn = pool.acquire().await.expect("conn");
    let s = seen(pool, version).await;
    method::set_kind(&mut conn, version, &s, MethodKind::Steps)
        .await
        .expect("steps");
    for one in steps {
        let s = seen(pool, version).await;
        method::edit_step(&mut conn, version, &s, recipes::StepEdit::Add(one.clone()))
            .await
            .expect("step");
    }
}

fn names(method: &Method) -> Vec<String> {
    match method {
        Method::Steps(steps) => steps.iter().map(|s| s.name.clone()).collect(),
        other => panic!("not steps: {other:?}"),
    }
}

fn time(text: &str) -> String {
    format!("<span class=\"method-time\">{text}</span>")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn baker_writes_steps_one_at_a_time(pool: PgPool) {
    let app = app(pool.clone());
    let (recipe_id, v1) = recipe(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = version_path(recipe_id, v1);

    let edit = baker.get(&format!("{path}/edit")).await;
    edit.shows_text(&dutch("method-none-hint"));
    edit.shows_text(&dutch("method-to-text"));
    edit.shows_text(&dutch("method-to-steps"));
    edit.lacks("data-dialog-open=\"werkwijze-");
    let read = baker.get(&path).await;
    read.lacks("id=\"werkwijze\"");

    let s = seen(&pool, v1).await;
    baker
        .post(
            &format!("{path}/method"),
            &[("kind", "steps"), ("seen", &s), ("gewicht", "1800")],
        )
        .await
        .redirects_to(&format!("{path}/edit?gewicht=1800&opgeslagen=1"));
    assert_eq!(stored(&pool, v1).await, Method::Steps(Vec::new()));
    baker
        .get(&format!("{path}/edit"))
        .await
        .shows_text(&dutch("method-steps-empty"));

    for (name, minutes, temperature) in [("Autolyse", "30", ""), ("Bulk", "240", "24")] {
        let s = seen(&pool, v1).await;
        let added = baker
            .post_htmx(
                &format!("{path}/steps"),
                &[
                    ("seen", &s),
                    ("stap-nieuw-naam", name),
                    ("stap-nieuw-duur", minutes),
                    ("stap-nieuw-temperatuur", temperature),
                    ("stap-nieuw-notities", "Afdekken.\r\nNiet te warm."),
                ],
            )
            .await;
        assert_eq!(added.status, StatusCode::OK);
        added.shows("id=\"werkwijze\"");
        added.shows(&format!("value=\"{name}\""));
        added.shows_text(&dutch("saved"));
    }
    let Method::Steps(steps) = stored(&pool, v1).await else {
        panic!("steps")
    };
    assert_eq!(
        steps[1],
        Step {
            name: "Bulk".to_owned(),
            duration_min: Some(240),
            temperature_c: Some(24),
            notes: "Afdekken.\nNiet te warm.".to_owned(),
        }
    );

    let s = seen(&pool, v1).await;
    let refused = baker
        .post(
            &format!("{path}/steps"),
            &[
                ("seen", &s),
                ("stap-nieuw-naam", ""),
                ("stap-nieuw-duur", "0"),
                ("stap-nieuw-temperatuur", "400"),
            ],
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.shows_text(&dutch("method-error-step-name"));
    refused.shows_text(&dutch("method-error-duration"));
    refused.shows_text(&dutch("method-error-temperature"));
    refused.shows("value=\"400\"");
    assert_eq!(names(&stored(&pool, v1).await), ["Autolyse", "Bulk"]);

    let s = seen(&pool, v1).await;
    baker
        .post_htmx(
            &format!("{path}/steps/0"),
            &[
                ("seen", &s),
                ("stap-0-naam", "Autolyse"),
                ("stap-0-duur", "45"),
            ],
        )
        .await
        .shows("value=\"45\"");
    let s = seen(&pool, v1).await;
    baker
        .post(&format!("{path}/steps/1/up"), &[("seen", &s)])
        .await
        .redirects_to(&format!("{path}/edit?gewicht=&opgeslagen=1"));
    assert_eq!(names(&stored(&pool, v1).await), ["Bulk", "Autolyse"]);
    let s = seen(&pool, v1).await;
    baker
        .post_htmx(&format!("{path}/steps/0/down"), &[("seen", &s)])
        .await;
    assert_eq!(names(&stored(&pool, v1).await), ["Autolyse", "Bulk"]);

    let page = baker.get(&format!("{path}?start=21:00")).await;
    page.shows(&time("21:00"));
    page.shows(&time("21:45"));
    page.shows(
        "<span class=\"method-time\">01:45 <span class=\"method-later\">+1 dag</span></span>",
    );
    page.shows("Afdekken.<br>Niet te warm.");
    page.shows_text(&dutch_with("method-duration-hours", &[("hours", "4")]));
    page.shows_text(&dutch_with("method-temperature", &[("c", "24")]));
    page.shows("value=\"21:00\" form=\"schaal\"");

    let now = baker.get(&path).await;
    now.shows("value=\"10:00\"");
    now.shows(&time("10:45"));
    let wrong = baker.get(&format!("{path}?start=morgen")).await;
    wrong.shows_text(&dutch("method-start-error"));
    wrong.shows(&time("10:00"));

    let s = seen(&pool, v1).await;
    baker
        .post_htmx(&format!("{path}/steps/0/remove"), &[("seen", &s)])
        .await;
    assert_eq!(names(&stored(&pool, v1).await), ["Bulk"]);
    let gone = baker
        .post_htmx(
            &format!("{path}/steps/5/remove"),
            &[("seen", &seen(&pool, v1).await)],
        )
        .await;
    gone.shows_text(&dutch("method-error-gone"));
    let s = seen(&pool, v1).await;
    let untimed = baker
        .post_htmx(
            &format!("{path}/steps"),
            &[("seen", &s), ("stap-nieuw-naam", "Bakken")],
        )
        .await;
    assert_eq!(untimed.status, StatusCode::OK);
    let page = baker.get(&path).await;
    page.shows_text("1 stap heeft geen duur en telt niet mee.");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn free_text_is_shown_as_plain_paragraphs(pool: PgPool) {
    let app = app(pool.clone());
    let (recipe_id, v1) = recipe(&pool).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = version_path(recipe_id, v1);
    let s = seen(&pool, v1).await;
    baker
        .post(&format!("{path}/method"), &[("kind", "text"), ("seen", &s)])
        .await;
    let text = "Meng <b>alles</b> & kneed.\r\nLaat rusten.\r\n\r\n\r\n<script>alert(1)</script>";
    let saved = baker
        .post_htmx(&format!("{path}/text"), &[("tekst", text)])
        .await;
    assert_eq!(saved.status, StatusCode::OK);
    saved.shows_text(&dutch("saved"));
    assert_eq!(
        stored(&pool, v1).await,
        Method::Text(
            "Meng <b>alles</b> & kneed.\nLaat rusten.\n\n\n<script>alert(1)</script>".to_owned()
        )
    );

    let page = baker.get(&path).await;
    page.shows(
        "<p class=\"method-text\">Meng &#60;b&#62;alles&#60;/b&#62; &#38; kneed.<br>Laat rusten.</p>",
    );
    page.shows("<p class=\"method-text\">&#60;script&#62;alert(1)&#60;/script&#62;</p>");
    page.lacks("<script>alert");
    page.lacks("id=\"start\"");

    let long = "x".repeat(10_001);
    let refused = baker
        .post(&format!("{path}/text"), &[("tekst", &long)])
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.shows_text(&dutch("method-error-text"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn switching_kind_asks_first_and_refuses_an_out_of_date_page(pool: PgPool) {
    let app = app(pool.clone());
    let (recipe_id, v1) = recipe(&pool).await;
    with_steps(&pool, v1, &[step("Bulk", Some(60))]).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = version_path(recipe_id, v1);

    let edit = baker.get(&format!("{path}/edit")).await;
    edit.shows("data-dialog-open=\"werkwijze-steps-text\"");
    edit.shows("data-dialog-open=\"werkwijze-steps-none\"");
    edit.shows_text(&dutch("method-to-text-text"));
    edit.shows_text(&dutch("method-remove-text"));

    let old = seen(&pool, v1).await;
    with_steps(&pool, v1, &[step("Vormen", None)]).await;
    let stale = baker
        .post_htmx(
            &format!("{path}/method"),
            &[("kind", "text"), ("seen", &old)],
        )
        .await;
    assert_eq!(stale.status, StatusCode::OK);
    stale.shows_text(&dutch("method-error-stale"));
    stale.lacks("<html");
    let plain = baker
        .post(
            &format!("{path}/method"),
            &[("kind", "none"), ("seen", &old)],
        )
        .await;
    assert_eq!(plain.status, StatusCode::UNPROCESSABLE_ENTITY);
    plain.shows_text(&dutch("method-error-stale"));
    assert_eq!(names(&stored(&pool, v1).await), ["Bulk", "Vormen"]);

    let stale_step = baker
        .post_htmx(
            &format!("{path}/steps"),
            &[("seen", &old), ("stap-nieuw-naam", "Rijzen")],
        )
        .await;
    stale_step.shows_text(&dutch("method-error-stale"));
    stale_step.shows("value=\"Rijzen\"");

    let unknown = baker
        .post(
            &format!("{path}/method"),
            &[("kind", "tekst"), ("seen", &seen(&pool, v1).await)],
        )
        .await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);

    let s = seen(&pool, v1).await;
    let switched = baker
        .post_htmx(&format!("{path}/method"), &[("kind", "text"), ("seen", &s)])
        .await;
    assert_eq!(
        switched.header("hx-redirect"),
        format!("{path}/edit?gewicht=&opgeslagen=1")
    );
    assert_eq!(stored(&pool, v1).await, Method::Text(String::new()));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_frozen_version_shows_its_steps_and_refuses_changes(pool: PgPool) {
    let app = app(pool.clone());
    let (recipe_id, v1) = recipe(&pool).await;
    with_steps(&pool, v1, &[step("Bulk", Some(60))]).await;
    let mut conn = pool.acquire().await.expect("conn");
    store::freeze(&mut conn, recipe_id, v1, app.clock.as_ref())
        .await
        .expect("freeze");
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = version_path(recipe_id, v1);
    let s = seen(&pool, v1).await;
    for (url, fields) in [
        (format!("{path}/method"), vec![("kind", "text")]),
        (format!("{path}/text"), vec![("tekst", "Meng")]),
        (format!("{path}/steps"), vec![("stap-nieuw-naam", "Vormen")]),
        (format!("{path}/steps/0"), vec![("stap-0-naam", "Rijzen")]),
        (format!("{path}/steps/0/remove"), vec![]),
    ] {
        let mut all = vec![("seen", s.as_str())];
        all.extend(fields);
        let refused = baker.post(&url, &all).await;
        assert_eq!(refused.status, StatusCode::CONFLICT, "{url}");
        refused.shows_text(&dutch("recipe-error-frozen"));
    }
    assert_eq!(names(&stored(&pool, v1).await), ["Bulk"]);
    baker.get(&path).await.shows(&time("10:00"));

    let v2 = store::new_version(&mut conn, recipe_id)
        .await
        .expect("draft")
        .id;
    let page = baker
        .get(&format!("{}/edit", version_path(recipe_id, v2)))
        .await;
    page.shows("value=\"Bulk\"");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn instruction_changes_need_production_and_the_csrf_token(pool: PgPool) {
    let app = app(pool.clone());
    let (recipe_id, v1) = recipe(&pool).await;
    with_steps(&pool, v1, &[step("Bulk", Some(60))]).await;
    let counter = client(&app, "Toonbank Tineke", Role::Counter).await;
    let baker = client(&app, "Bakker Bas", Role::Baker).await;
    let path = version_path(recipe_id, v1);
    let s = seen(&pool, v1).await;
    let fields = [
        ("seen", s.as_str()),
        ("kind", "none"),
        ("tekst", "Meng"),
        ("stap-nieuw-naam", "Vormen"),
        ("stap-0-naam", "Vormen"),
    ];
    for url in [
        format!("{path}/method"),
        format!("{path}/text"),
        format!("{path}/steps"),
        format!("{path}/steps/0"),
        format!("{path}/steps/0/remove"),
        format!("{path}/steps/0/up"),
        format!("{path}/steps/0/down"),
    ] {
        let forbidden = counter.post(&url, &fields).await;
        assert_eq!(forbidden.status, StatusCode::FORBIDDEN, "counter {url}");
        let no_token = baker.post_raw(&url, &fields, true).await;
        assert_eq!(no_token.status, StatusCode::FORBIDDEN, "csrf {url}");
    }
    assert_eq!(
        counter.get(&path).await.status,
        StatusCode::FORBIDDEN,
        "read"
    );
    assert_eq!(names(&stored(&pool, v1).await), ["Bulk"]);
    let missing = baker
        .post(&format!("{path}/steps/x/up"), &[("seen", &s)])
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let other = baker
        .post(
            &format!("/staff/recipes/{}/versions/999999/steps", recipe_id.get()),
            &[("seen", &s), ("stap-nieuw-naam", "Vormen")],
        )
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);
}
