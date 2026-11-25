//! F9b over HTTP: the bread vote on the site and its back office.
#![forbid(unsafe_code)]

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use auth::{NewStaff, Role};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use db::PgPool;
use domain::PollId;
use i18n::Translator;
use jiff::{SignedDuration, Timestamp};
use notify::polls::{self, NewOption, PollInput};
use server::AppState;
use server::config::Config;
use support::signed_in_at;
use testkit::FakeClock;
use tower::ServiceExt;

const ORIGIN: &str = "https://broodvoorweldadigheid.nl";
const NOW: &str = "2026-11-24T10:00:00Z";

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn dutch(id: &str) -> String {
    Translator::dutch().unwrap().t(id, None).unwrap()
}

async fn app(pool: &PgPool) -> Router {
    sqlx::query(
        "UPDATE company SET kvk = '12345678',
            address = 'Majoor van Swietenlaan 15, 8382 CE Frederiksoord'",
    )
    .execute(pool)
    .await
    .unwrap();
    let config = Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://unused/db".to_owned()),
        "PUBLIC_URL" => Some(ORIGIN.to_owned()),
        _ => None,
    })
    .unwrap();
    server::router(
        AppState::new(pool.clone(), Translator::dutch().unwrap(), config)
            .with_clock(Arc::new(FakeClock::new(ts(NOW)))),
    )
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    cookie: Option<String>,
    body: String,
}

async fn send(app: &Router, mut request: Request<Body>) -> Reply {
    request
        .extensions_mut()
        .insert(ConnectInfo("192.0.2.7:4000".parse::<SocketAddr>().unwrap()));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let cookie = headers
        .get(SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    Reply {
        status,
        headers,
        cookie,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn get(app: &Router, path: &str, cookie: Option<&str>) -> Reply {
    let mut request = Request::get(path);
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    send(app, request.body(Body::empty()).unwrap()).await
}

async fn post(app: &Router, path: &str, cookie: Option<&str>, body: &str) -> Reply {
    let mut request = Request::post(path).header(CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    send(app, request.body(Body::from(body.to_owned())).unwrap()).await
}

fn meta_csrf(body: &str) -> String {
    let marker = "name=\"csrf-token\" content=\"";
    let start = body.find(marker).expect("csrf meta") + marker.len();
    let end = start + body[start..].find('"').unwrap();
    body[start..end].to_owned()
}

fn csrf_of(body: &str) -> String {
    let marker = "name=\"_csrf\" value=\"";
    let start = body.find(marker).expect("csrf field") + marker.len();
    let end = start + body[start..].find('"').unwrap();
    body[start..end].to_owned()
}

fn encode(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('@', "%40")
        .replace(' ', "+")
        .replace('&', "%26")
        .replace('\n', "%0A")
}

async fn poll(pool: &PgPool, closes_in: SignedDuration, public: bool) -> (PollId, Vec<i64>) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = FakeClock::new(ts(NOW) - SignedDuration::from_hours(1));
    let id = polls::create(
        &mut conn,
        &clock,
        &PollInput {
            title: "Welk brood bakken we in december?".to_owned(),
            bake_day: None,
            closes_at: ts(NOW) + closes_in,
            public_results: public,
        },
        &[
            NewOption {
                product: None,
                name: "Spelt-walnoot".to_owned(),
            },
            NewOption {
                product: None,
                name: "Roggebrood".to_owned(),
            },
        ],
    )
    .await
    .unwrap();
    let options = polls::get(&mut conn, id)
        .await
        .unwrap()
        .options
        .iter()
        .map(|o| o.id.get())
        .collect();
    (id, options)
}

/// Opens the poll page for a session and posts a vote from it.
async fn vote(app: &Router, poll: PollId, option: i64, email: &str, wish: &str) -> Reply {
    let path = format!("/stemmen/{}", poll.get());
    let page = get(app, &path, None).await;
    let cookie = page.cookie.unwrap();
    let body = format!(
        "_csrf={}&keuze={option}&email={}&wens={}",
        csrf_of(&page.body),
        encode(email),
        encode(wish)
    );
    post(app, &path, Some(&cookie), &body).await
}

async fn confirmations(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT text_body FROM email_outbox WHERE kind = 'vote_confirmation' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn confirm_path(text: &str) -> String {
    let marker = format!("{ORIGIN}/stemmen/bevestig/");
    let start = text.find(&marker).expect("link") + ORIGIN.len();
    text[start..start + "/stemmen/bevestig/".len() + 43].to_owned()
}

async fn counts(pool: &PgPool, poll: PollId) -> Vec<i64> {
    let mut conn = pool.acquire().await.unwrap();
    polls::results(&mut conn, poll)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.votes)
        .collect()
}

async fn staff(pool: &PgPool, role: Role) -> (String, String) {
    let user = auth::staff::add(
        pool,
        &NewStaff {
            oidc_sub: format!("sub-{role}"),
            name: format!("Staff {role}"),
            email: None,
            role,
        },
    )
    .await
    .unwrap();
    signed_in_at(pool, &user, ts(NOW)).await
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_poll_page_offers_the_candidates_and_a_csrf_protected_form(pool: PgPool) {
    let (id, options) = poll(&pool, SignedDuration::from_hours(24), false).await;
    let app = app(&pool).await;
    let page = get(&app, &format!("/stemmen/{}", id.get()), None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Welk brood bakken we in december?"));
    assert!(page.body.contains(&format!("value=\"{}\"", options[0])));
    assert!(page.body.contains("Roggebrood"));
    assert!(page.body.contains(&dutch("votes-wish-hint")));
    assert!(page.body.contains("name=\"_csrf\""));
    assert!(
        !page.body.contains(&dutch("votes-results")),
        "results are private"
    );

    let index = get(&app, "/stemmen", None).await;
    assert!(
        index
            .body
            .contains(&format!("href=\"/stemmen/{}\"", id.get()))
    );

    let reply = post(
        &app,
        &format!("/stemmen/{}", id.get()),
        page.cookie.as_deref(),
        &format!("keuze={}&email=a%40example.nl", options[0]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "no CSRF token");
    assert!(confirmations(&pool).await.is_empty());
    assert_eq!(
        get(&app, "/stemmen/999999", None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_vote_counts_after_the_confirmation_link_and_its_button(pool: PgPool) {
    let (id, options) = poll(&pool, SignedDuration::from_hours(24), false).await;
    let app = app(&pool).await;
    let reply = vote(&app, id, options[1], "iemand@example.nl", "Meer rogge!").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains(&dutch("votes-check-text")));
    let mails = confirmations(&pool).await;
    assert_eq!(mails.len(), 1);
    assert_eq!(counts(&pool, id).await, [0, 0]);

    let path = confirm_path(&mails[0]);
    let page = get(&app, &path, None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(
        page.headers.get(CACHE_CONTROL).unwrap(),
        "private, no-store"
    );
    assert!(page.body.contains(&format!("action=\"{path}\"")));
    assert!(page.body.contains("Roggebrood"), "the choice to confirm");
    assert!(
        page.body.contains("Meer rogge!"),
        "and the wish, only for the link holder"
    );
    assert_eq!(
        counts(&pool, id).await,
        [0, 0],
        "opening the link changes nothing"
    );

    let cookie = page.cookie.unwrap();
    let refused = post(&app, &path, Some(&cookie), "").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "no CSRF token");
    let done = post(
        &app,
        &path,
        Some(&cookie),
        &format!("_csrf={}", csrf_of(&page.body)),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK);
    assert!(done.body.contains(&dutch("votes-counted-title")));
    assert_eq!(counts(&pool, id).await, [0, 1]);

    let again = get(&app, &path, None).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert!(again.body.contains(&dutch("votes-link-failed-title")));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn every_address_gets_the_same_answer(pool: PgPool) {
    let (id, options) = poll(&pool, SignedDuration::from_hours(24), false).await;
    let app = app(&pool).await;
    let new = vote(&app, id, options[0], "iemand@example.nl", "").await;
    let waiting = vote(&app, id, options[1], "IEMAND@example.nl", "").await;
    let path = confirm_path(&confirmations(&pool).await[0]);
    let page = get(&app, &path, None).await;
    post(
        &app,
        &path,
        page.cookie.as_deref(),
        &format!("_csrf={}", csrf_of(&page.body)),
    )
    .await;
    let voted = vote(&app, id, options[1], "iemand@example.nl", "").await;
    for reply in [&waiting, &voted] {
        assert_eq!(reply.status, new.status);
        assert_eq!(reply.body, new.body);
    }
    assert_eq!(confirmations(&pool).await.len(), 1);
    assert_eq!(counts(&pool, id).await, [1, 0]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn mistakes_show_the_form_again(pool: PgPool) {
    let (id, options) = poll(&pool, SignedDuration::from_hours(24), false).await;
    let app = app(&pool).await;
    let bad_address = vote(&app, id, options[0], "geen-adres", "").await;
    assert_eq!(bad_address.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(bad_address.body.contains(&dutch("votes-error-email")));
    assert!(bad_address.body.contains("value=\"geen-adres\""));
    let long_wish = vote(&app, id, options[0], "a@example.nl", &"x".repeat(501)).await;
    assert_eq!(long_wish.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(long_wish.body.contains(&dutch("votes-error-wish")));
    let (_, other) = poll(&pool, SignedDuration::from_hours(24), false).await;
    let wrong = vote(&app, id, other[0], "a@example.nl", "").await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(wrong.body.contains(&dutch("votes-error-choice")));
    assert!(confirmations(&pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_closed_poll_refuses_votes(pool: PgPool) {
    let (id, options) = poll(&pool, SignedDuration::ZERO, false).await;
    let app = app(&pool).await;
    let page = get(&app, &format!("/stemmen/{}", id.get()), None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(&dutch("votes-closed")));
    assert!(!page.body.contains("name=\"keuze\""));
    let path = format!("/stemmen/{}", id.get());
    let reply = post(
        &app,
        &path,
        page.cookie.as_deref(),
        &format!(
            "_csrf={}&keuze={}&email=a%40example.nl",
            meta_csrf(&page.body),
            options[0]
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert!(reply.body.contains(&dutch("votes-closed-title")));
    assert!(confirmations(&pool).await.is_empty());
    let index = get(&app, "/stemmen", None).await;
    assert!(
        !index.body.contains(&path),
        "closed and private: not listed"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn public_results_show_counts_but_never_addresses_or_wishes(pool: PgPool) {
    let (id, options) = poll(&pool, SignedDuration::from_hours(24), true).await;
    sqlx::query(
        "INSERT INTO vote (poll_id, poll_option_id, email, wish, confirmed_at, created_at)
        VALUES ($1, $2, 'geheim@example.nl', 'Een geheime wens', now(), now())",
    )
    .bind(id.get())
    .bind(options[0])
    .execute(&pool)
    .await
    .unwrap();
    let app = app(&pool).await;
    let page = get(&app, &format!("/stemmen/{}", id.get()), None).await;
    assert!(page.body.contains(&dutch("votes-results")));
    assert!(page.body.contains("Spelt-walnoot: 1"));
    assert!(!page.body.contains("geheim@example.nl"));
    assert!(!page.body.contains("Een geheime wens"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_signed_in_customer_votes_at_once(pool: PgPool) {
    sqlx::query(
        "INSERT INTO customer (email, name, created_at) VALUES ('klant@example.nl', 'Sanne', now())",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (id, options) = poll(&pool, SignedDuration::from_hours(24), false).await;
    let app = app(&pool).await;
    let login = get(&app, "/inloggen", None).await;
    let cookie = login.cookie.unwrap();
    post(
        &app,
        "/inloggen",
        Some(&cookie),
        &format!("_csrf={}&email=klant%40example.nl", csrf_of(&login.body)),
    )
    .await;
    let text: String =
        sqlx::query_scalar("SELECT text_body FROM email_outbox WHERE kind = 'login_link'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let start = text.find(&format!("{ORIGIN}/inloggen/")).unwrap() + ORIGIN.len();
    let link = &text[start..start + "/inloggen/".len() + 43];
    let page = get(&app, link, None).await;
    let signed_in = post(
        &app,
        link,
        page.cookie.as_deref(),
        &format!("_csrf={}", csrf_of(&page.body)),
    )
    .await;
    let cookie = signed_in.cookie.unwrap();

    let path = format!("/stemmen/{}", id.get());
    let page = get(&app, &path, Some(&cookie)).await;
    assert!(page.body.contains(&dutch("votes-signed-in")));
    assert!(!page.body.contains("name=\"email\""));
    let body = format!("_csrf={}&keuze={}", csrf_of(&page.body), options[0]);
    let reply = post(&app, &path, Some(&cookie), &body).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains(&dutch("votes-counted-title")));
    assert_eq!(counts(&pool, id).await, [1, 0]);
    assert!(confirmations(&pool).await.is_empty());
    let again = post(&app, &path, Some(&cookie), &body).await;
    assert!(again.body.contains(&dutch("votes-already-title")));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn votes_are_rate_limited(pool: PgPool) {
    let (id, options) = poll(&pool, SignedDuration::from_hours(24), false).await;
    let app = app(&pool).await;
    let path = format!("/stemmen/{}", id.get());
    let page = get(&app, &path, None).await;
    let cookie = page.cookie.unwrap();
    let token = csrf_of(&page.body);
    let mut statuses = Vec::new();
    for n in 0..6 {
        let body = format!(
            "_csrf={token}&keuze={}&email=lid{n}%40example.nl",
            options[0]
        );
        statuses.push(post(&app, &path, Some(&cookie), &body).await.status);
    }
    assert_eq!(statuses[..5], [StatusCode::OK; 5]);
    assert_eq!(statuses[5], StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_owner_and_baker_reach_the_polls(pool: PgPool) {
    let (id, _) = poll(&pool, SignedDuration::from_hours(24), false).await;
    let app = app(&pool).await;
    let anonymous = get(&app, "/staff/polls", None).await;
    assert_eq!(anonymous.status, StatusCode::SEE_OTHER);
    assert!(anonymous.headers.get(LOCATION).is_some());
    let (counter, _) = staff(&pool, Role::Counter).await;
    for path in [
        "/staff/polls".to_owned(),
        format!("/staff/polls/{}", id.get()),
    ] {
        assert_eq!(
            get(&app, &path, Some(&counter)).await.status,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    let (baker, _) = staff(&pool, Role::Baker).await;
    let list = get(&app, "/staff/polls", Some(&baker)).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(list.body.contains("Welk brood bakken we in december?"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_baker_creates_a_poll_and_sees_results_and_wishes(pool: PgPool) {
    let product: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
        VALUES ('Desem wit', 450, 9, 800, 900) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let app = app(&pool).await;
    let (cookie, token) = staff(&pool, Role::Baker).await;

    let form = get(&app, "/staff/polls/new", Some(&cookie)).await;
    assert_eq!(form.status, StatusCode::OK);
    assert!(form.body.contains("Desem wit"));
    let without_token = post(
        &app,
        "/staff/polls/new",
        Some(&cookie),
        "title=Vraag&closes_at=2026-12-01T20%3A00&options=Spelt%0ARogge",
    )
    .await;
    assert_eq!(without_token.status, StatusCode::FORBIDDEN);

    let too_few = post(
        &app,
        "/staff/polls/new",
        Some(&cookie),
        &format!("_csrf={token}&title=Vraag&closes_at=2026-12-01T20%3A00&options=Spelt"),
    )
    .await;
    assert_eq!(too_few.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(too_few.body.contains(&dutch("poll-error-few")));

    let created = post(
        &app,
        "/staff/polls/new",
        Some(&cookie),
        &format!(
            "_csrf={token}&title=Welk+brood%3F&closes_at=2026-12-01T20%3A00&public=ja\
            &product-{product}=ja&options=Spelt-walnoot%0A%0ARogge"
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::SEE_OTHER);
    let location = created.headers.get(LOCATION).unwrap().to_str().unwrap();
    let id: i64 = location
        .trim_start_matches("/staff/polls/")
        .split('?')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let poll = polls::get(&mut conn, PollId::new(id)).await.unwrap();
    assert_eq!(
        poll.options
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>(),
        ["Desem wit", "Spelt-walnoot", "Rogge"]
    );
    assert!(poll.public_results);
    assert_eq!(poll.closes_at, ts("2026-12-01T19:00:00Z"), "bakery time");

    sqlx::query(
        "INSERT INTO vote (poll_id, poll_option_id, email, wish, confirmed_at, created_at)
        VALUES ($1, $2, 'stemmer@example.nl', 'Graag met zaden', now(), now())",
    )
    .bind(id)
    .bind(poll.options[1].id.get())
    .execute(&pool)
    .await
    .unwrap();
    let page = get(&app, location, Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Spelt-walnoot: 1"));
    assert!(page.body.contains("Graag met zaden"));
    assert!(
        !page.body.contains("stemmer@example.nl"),
        "no addresses, also for staff"
    );

    let closed = post(
        &app,
        &format!("/staff/polls/{id}"),
        Some(&cookie),
        &format!("_csrf={token}&title=Welk+brood%3F&closes_at=2026-11-24T10%3A00"),
    )
    .await;
    assert_eq!(closed.status, StatusCode::SEE_OTHER);
    let poll = polls::get(&mut conn, PollId::new(id)).await.unwrap();
    assert!(!poll.is_open(ts(NOW)), "closed early");
    assert!(!poll.public_results);

    let reopen = post(
        &app,
        &format!("/staff/polls/{id}"),
        Some(&cookie),
        &format!("_csrf={token}&title=Welk+brood%3F&closes_at=2026-12-24T10%3A00"),
    )
    .await;
    assert_eq!(reopen.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reopen.body.contains(&dutch("poll-error-reopen")));
    let poll = polls::get(&mut conn, PollId::new(id)).await.unwrap();
    assert!(!poll.is_open(ts(NOW)), "stays closed");
}
