//! The compact drop list on /drops and the home page, and the drop page's facts (F1, F1a,
//! F4c).
#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use bakedays::{NewBakeDay, PickupLocationInput, PickupSlotInput, Schedule, days, pickup};
use domain::{BakeDayId, CharityId, PickupLocationId};
use i18n::{FluentArgs, Translator};
use jiff::Timestamp;
use server::AppState;
use server::config::Config;
use sqlx::{PgConnection, PgPool};
use testkit::FakeClock;
use tower::ServiceExt;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn dutch(id: &str) -> String {
    Translator::dutch().unwrap().t(id, None).unwrap()
}

fn dutch_with(id: &str, args: &[(&str, &str)]) -> String {
    let mut fluent = FluentArgs::new();
    for (key, value) in args {
        fluent.set(*key, (*value).to_owned());
    }
    Translator::dutch().unwrap().t(id, Some(&fluent)).unwrap()
}

struct Shop {
    app: Router,
    clock: Arc<FakeClock>,
    pool: PgPool,
}

/// Wednesday 28 October 2026.
fn shop(pool: PgPool) -> Shop {
    let config = Config::from_lookup(|key| {
        (key == "DATABASE_URL").then(|| "postgres://unused/db".to_string())
    })
    .unwrap();
    let clock = Arc::new(FakeClock::new(ts("2026-10-28T12:00:00Z")));
    let state =
        AppState::new(pool.clone(), Translator::dutch().unwrap(), config).with_clock(clock.clone());
    Shop {
        app: server::router(state),
        clock,
        pool,
    }
}

async fn get(shop: &Shop, path: &str) -> String {
    let response = shop
        .app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

/// A drop on `date` at De Proef (Saturdays 11:00–12:00, deadline Thursday 20:00).
async fn drop_on(conn: &mut PgConnection, date: &str, charity: Option<CharityId>) -> BakeDayId {
    days::create(
        conn,
        &NewBakeDay {
            date: date.parse().unwrap(),
            location_id: PickupLocationId::new(1),
            online_ordering: true,
            charity_id: charity,
        },
    )
    .await
    .unwrap()
    .id
}

async fn opens_at(conn: &mut PgConnection, day: BakeDayId, at: &str) {
    let current = days::get(conn, day).await.unwrap();
    days::update_schedule(
        conn,
        day,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(ts(at)),
            cutoff_at: current.cutoff_at,
        },
    )
    .await
    .unwrap();
}

async fn open(conn: &mut PgConnection, clock: &FakeClock, day: BakeDayId) {
    opens_at(conn, day, "2026-10-01T00:00:00Z").await;
    days::open(conn, clock, day).await.unwrap();
}

async fn charity(conn: &mut PgConnection, name: &str) -> CharityId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO charity (name, description, url)
         VALUES ($1, 'Helpt mensen in de buurt', 'https://example.org/doel') RETURNING id",
    )
    .bind(name)
    .fetch_one(conn)
    .await
    .unwrap();
    CharityId::new(id)
}

/// The markup of the compact list, or "" when the page has none.
fn rows(html: &str) -> &str {
    html.find("<ul class=\"drop-rows\"").map_or("", |start| {
        let end = html[start..].find("</ul>").unwrap() + start;
        &html[start..end]
    })
}

/// The featured open drop on the home page, or "".
fn featured(html: &str) -> &str {
    html.find("<section class=\"drop-feature\"")
        .map_or("", |start| {
            let end = html[start..].find("</section>").unwrap() + start;
            &html[start..end]
        })
}

fn link(day: BakeDayId) -> String {
    format!("href=\"/drops/{}\"", day.get())
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_calendar_lists_one_short_row_per_drop(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let voedselbank = charity(&mut conn, "Voedselbank Westerveld").await;
    let open_day = drop_on(&mut conn, "2026-10-31", Some(voedselbank)).await;
    open(&mut conn, &shop.clock, open_day).await;
    let planned = drop_on(&mut conn, "2026-11-07", None).await;
    opens_at(&mut conn, planned, "2026-11-02T08:00:00Z").await;
    let cancelled = drop_on(&mut conn, "2026-11-14", None).await;
    days::cancel(&mut conn, shop.clock.as_ref(), cancelled, "Ziek")
        .await
        .unwrap();
    let two_places = drop_on(&mut conn, "2026-11-21", None).await;
    let dorpshuis = pickup::create_location(
        &mut conn,
        &PickupLocationInput {
            name: "Dorpshuis".to_string(),
            address: "Dorpsstraat 1, 8384 EA Wilhelminaoord".to_string(),
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    days::add_slot(
        &mut conn,
        two_places,
        dorpshuis.id,
        &PickupSlotInput {
            starts_at: ts("2026-11-22T09:00:00Z"),
            ends_at: ts("2026-11-22T09:30:00Z"),
            max_orders: None,
        },
    )
    .await
    .unwrap();
    let next_year = drop_on(&mut conn, "2027-01-09", None).await;

    let html = get(&shop, "/drops").await;
    let list = rows(&html);
    assert!(!list.is_empty(), "{html}");
    for day in [open_day, planned, cancelled, two_places, next_year] {
        assert!(list.contains(&link(day)), "{list}");
    }
    assert!(
        list.contains(&format!(
            "<a {}><time datetime=\"2026-10-31\">za 31 okt</time></a>",
            link(open_day)
        )),
        "{list}"
    );
    assert!(list.contains(">za 9 jan 2027</time>"), "{list}");
    assert!(!list.contains("za 31 okt 2026"), "{list}");

    assert!(!html.contains("Normale verkoop"), "{html}");
    assert!(!list.contains("Gepland"), "{list}");
    let for_charity = dutch_with("drop-for-charity", &[("charity", "Voedselbank Westerveld")]);
    assert_eq!(list.matches(&for_charity).count(), 1, "{list}");

    assert!(list.contains(">11:00–12:00</time>"), "{list}");
    assert!(list.contains("De Proef"), "{list}");
    assert!(!list.contains("Majoor van Swietenlaan"), "{list}");
    assert!(list.contains(&dutch_with(
        "drop-order-until",
        &[("when", "do 29 okt 20:00")]
    )));
    assert!(list.contains(&dutch_with(
        "drop-order-from",
        &[("when", "ma 2 nov 09:00")]
    )));

    assert_eq!(list.matches("drop-row-status").count(), 2, "{list}");
    assert!(list.contains(&dutch("drop-status-open")), "{list}");
    assert!(list.contains(&dutch("drop-status-cancelled")), "{list}");

    let mut one = FluentArgs::new();
    one.set("n", 1);
    let more = Translator::dutch()
        .unwrap()
        .t("drop-more-pickups", Some(&one))
        .unwrap();
    assert_eq!(list.matches(&more).count(), 1, "{list}");
    assert!(!list.contains("Dorpshuis"), "{list}");
    assert!(html.contains(&dutch("drops-ical")), "{html}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_drop_page_shows_its_facts_as_a_short_list(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let voedselbank = charity(&mut conn, "Voedselbank Westerveld").await;
    let charity_day = drop_on(&mut conn, "2026-10-31", Some(voedselbank)).await;
    open(&mut conn, &shop.clock, charity_day).await;
    let normal = drop_on(&mut conn, "2026-11-07", None).await;
    let dorpshuis = pickup::create_location(
        &mut conn,
        &PickupLocationInput {
            name: "Dorpshuis".to_string(),
            address: "Dorpsstraat 1, 8384 EA Wilhelminaoord".to_string(),
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    days::add_slot(
        &mut conn,
        normal,
        dorpshuis.id,
        &PickupSlotInput {
            starts_at: ts("2026-11-08T09:00:00Z"),
            ends_at: ts("2026-11-08T09:30:00Z"),
            max_orders: None,
        },
    )
    .await
    .unwrap();

    let html = get(&shop, &format!("/drops/{}", charity_day.get())).await;
    assert!(html.contains("<dl class=\"drop-facts\""), "{html}");
    assert!(html.contains(&dutch("drop-label-pickup")), "{html}");
    assert!(html.contains(">11:00–12:00</time>"), "{html}");
    assert!(html.contains("De Proef"), "{html}");
    assert!(
        html.contains("Majoor van Swietenlaan 15, 8382 CE Frederiksoord"),
        "{html}"
    );
    assert!(html.contains(&dutch("drop-label-order-until")), "{html}");
    assert!(html.contains(">do 29 okt 20:00</time>"), "{html}");
    assert!(html.contains(&dutch("drop-label-charity")), "{html}");
    assert!(html.contains("Voedselbank Westerveld"), "{html}");
    assert!(html.contains("Helpt mensen in de buurt"), "{html}");
    assert!(html.contains(&dutch("drop-status-open")), "{html}");
    assert!(!html.contains("Normale verkoop"), "{html}");

    let html = get(&shop, &format!("/drops/{}", normal.get())).await;
    assert!(!html.contains(&dutch("drop-label-charity")), "{html}");
    assert!(!html.contains("Normale verkoop"), "{html}");
    assert!(!html.contains("class=\"drop-status"), "{html}");
    assert!(html.contains(">11:00–12:00</time>"), "{html}");
    assert!(html.contains(">zo 8 nov 10:00–10:30</time>"), "{html}");
    assert!(html.contains("Dorpshuis"), "{html}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_home_page_features_the_open_drop_and_lists_the_rest(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let voedselbank = charity(&mut conn, "Voedselbank Westerveld").await;
    let open_day = drop_on(&mut conn, "2026-10-31", Some(voedselbank)).await;
    open(&mut conn, &shop.clock, open_day).await;
    let planned = drop_on(&mut conn, "2026-11-07", None).await;

    let html = get(&shop, "/").await;
    let feature = featured(&html);
    assert!(feature.contains(&link(open_day)), "{html}");
    assert!(
        feature.contains(&format!(
            "<a class=\"button button-primary\" {}>{}</a>",
            link(open_day),
            dutch("drop-order-button")
        )),
        "{feature}"
    );
    assert!(
        feature.contains(&dutch("drop-label-order-until")),
        "{feature}"
    );
    assert!(feature.contains("Voedselbank Westerveld"), "{feature}");
    let list = rows(&html);
    assert!(list.contains(&link(planned)), "{html}");
    assert!(!list.contains(&link(open_day)), "{list}");
    assert_eq!(html.matches(&link(open_day)).count(), 1, "{html}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_an_open_drop_the_home_page_only_lists(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let first = drop_on(&mut conn, "2026-10-31", None).await;
    let second = drop_on(&mut conn, "2026-11-07", None).await;

    let html = get(&shop, "/").await;
    assert_eq!(featured(&html), "", "{html}");
    let list = rows(&html);
    assert!(list.contains(&link(first)), "{html}");
    assert!(list.contains(&link(second)), "{html}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn with_two_open_drops_the_home_page_features_the_earliest(pool: PgPool) {
    let shop = shop(pool);
    let mut conn = shop.pool.acquire().await.unwrap();
    let later = drop_on(&mut conn, "2026-11-07", None).await;
    open(&mut conn, &shop.clock, later).await;
    let earliest = drop_on(&mut conn, "2026-10-31", None).await;
    open(&mut conn, &shop.clock, earliest).await;

    let html = get(&shop, "/").await;
    let feature = featured(&html);
    assert!(feature.contains(&link(earliest)), "{html}");
    assert!(!feature.contains(&link(later)), "{feature}");
    let list = rows(&html);
    assert!(list.contains(&link(later)), "{html}");
    assert!(!list.contains(&link(earliest)), "{list}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_drops_the_home_page_shows_neither(pool: PgPool) {
    let shop = shop(pool);
    let html = get(&shop, "/").await;
    assert_eq!(featured(&html), "", "{html}");
    assert_eq!(rows(&html), "", "{html}");
}
