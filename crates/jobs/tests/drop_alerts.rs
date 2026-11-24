//! F9a: drop alerts go out once per drop and kind, only to confirmed subscribers.
#![forbid(unsafe_code)]

use std::sync::Arc;

use bakedays::{BakeDay, BakeDayStatus, NewBakeDay, Schedule, days};
use domain::PickupLocationId;
use i18n::Translator;
use jiff::Timestamp;
use jobs::drop_alerts::{AlertReport, QueuedAlert, tick};
use jobs::{Mail, work};
use notify::subscribers::{self, Links};
use notify::{Company, Composer, DropAlertKind, EmailSender};
use sqlx::{Acquire, PgConnection, PgPool};
use testkit::{FakeClock, FakeEmailSender};

const ORIGIN: &str = "https://broodvoorweldadigheid.nl";

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn i18n() -> Translator {
    Translator::dutch().unwrap()
}

fn links() -> Links {
    Links::new(ORIGIN)
}

/// Saturday 31 October: deadline Thursday 29 October 20:00 CET.
async fn saturday(
    conn: &mut PgConnection,
    menu_at: Option<&str>,
    opens_at: Option<&str>,
) -> BakeDay {
    let day = days::create(
        conn,
        &NewBakeDay {
            date: "2026-10-31".parse().unwrap(),
            location_id: PickupLocationId::new(1),
            bake_starts_at: ts("2026-10-31T03:00:00Z"),
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .unwrap();
    schedule(conn, &day, menu_at, opens_at).await
}

async fn schedule(
    conn: &mut PgConnection,
    day: &BakeDay,
    menu_at: Option<&str>,
    opens_at: Option<&str>,
) -> BakeDay {
    days::update_schedule(
        conn,
        day.id,
        &Schedule {
            online_ordering: day.online_ordering,
            menu_publish_at: menu_at.map(ts),
            opens_at: opens_at.map(ts),
            cutoff_at: day.cutoff_at,
            bake_starts_at: day.bake_starts_at,
        },
    )
    .await
    .unwrap()
}

async fn company_details(conn: &mut PgConnection) {
    sqlx::query(
        "UPDATE company SET kvk = '12345678',
            address = 'Majoor van Swietenlaan 15, 8382 CE Frederiksoord'",
    )
    .execute(conn)
    .await
    .unwrap();
}

async fn menu_item(conn: &mut PgConnection, day: &BakeDay, name: &str) {
    let product: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
        VALUES ($1, 650, 9, 800, 900) RETURNING id",
    )
    .bind(name)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    sqlx::query("INSERT INTO bake_day_product (bake_day_id, product_id) VALUES ($1, $2)")
        .bind(day.id.get())
        .bind(product)
        .execute(conn)
        .await
        .unwrap();
}

/// The token in the last confirmation email to `to`.
async fn confirm_token(conn: &mut PgConnection, to: &str) -> String {
    let text: String = sqlx::query_scalar(
        "SELECT text_body FROM email_outbox
        WHERE kind = 'subscription_confirmation' AND to_address = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(to)
    .fetch_one(conn)
    .await
    .unwrap();
    token_after(&text, "/meldingen/bevestig/")
}

fn token_after(text: &str, path: &str) -> String {
    let start = text.find(path).expect("link") + path.len();
    text[start..start + 43].to_owned()
}

#[derive(Clone, Copy)]
enum State {
    Confirmed,
    Unconfirmed,
    Unsubscribed,
}

async fn subscriber(conn: &mut PgConnection, clock: &FakeClock, email: &str, state: State) {
    let i18n = i18n();
    let company = notify::load_company(conn).await.unwrap();
    let composer = Composer::new(&i18n, &company);
    subscribers::subscribe(conn, clock, &composer, &links(), email)
        .await
        .unwrap();
    if matches!(state, State::Unconfirmed) {
        return;
    }
    let token = confirm_token(conn, email).await;
    assert!(subscribers::confirm(conn, clock, &token).await.unwrap());
    if matches!(state, State::Unsubscribed) {
        let id: i64 = sqlx::query_scalar("SELECT id FROM subscriber WHERE email = $1")
            .bind(email)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        let url = subscribers::unsubscribe_link(conn, clock, &links(), id)
            .await
            .unwrap();
        let token = token_after(&url, "/meldingen/afmelden/");
        assert!(subscribers::unsubscribe(conn, clock, &token).await.unwrap());
    }
}

async fn alerts(conn: &mut PgConnection) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT to_address, subject, list_unsubscribe FROM email_outbox
        WHERE kind = 'drop_alert' ORDER BY id",
    )
    .fetch_all(conn)
    .await
    .unwrap()
}

async fn alert_rows(conn: &mut PgConnection) -> Vec<(i64, String, i32)> {
    sqlx::query_as("SELECT bake_day_id, kind, recipients FROM drop_alert_sent ORDER BY kind")
        .fetch_all(conn)
        .await
        .unwrap()
}

async fn setup(conn: &mut PgConnection, clock: &FakeClock) {
    company_details(conn).await;
    subscriber(conn, clock, "ja@example.nl", State::Confirmed).await;
    subscriber(conn, clock, "nog-niet@example.nl", State::Unconfirmed).await;
    subscriber(conn, clock, "weg@example.nl", State::Unsubscribed).await;
}

async fn alert_tick(conn: &mut PgConnection, clock: &FakeClock, now: &str) -> AlertReport {
    clock.set(ts(now));
    tick(conn, clock, &i18n(), &links()).await.unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn menu_alert_goes_once_and_only_to_confirmed_subscribers(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = FakeClock::new(ts("2026-10-20T08:00:00Z"));
    setup(&mut conn, &clock).await;
    let day = saturday(&mut conn, Some("2026-10-24T08:00:00Z"), None).await;
    menu_item(&mut conn, &day, "Desem spelt").await;

    let report = alert_tick(&mut conn, &clock, "2026-10-24T07:59:59Z").await;
    assert_eq!(report, AlertReport::default());

    let report = alert_tick(&mut conn, &clock, "2026-10-24T08:00:00Z").await;
    assert_eq!(
        report.queued,
        [QueuedAlert {
            bake_day: day.id,
            kind: DropAlertKind::MenuPublished,
            recipients: 1,
        }]
    );
    let sent = alerts(&mut conn).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "ja@example.nl");
    let unsubscribe = sent[0].2.clone().expect("List-Unsubscribe");
    assert!(unsubscribe.starts_with(&format!("{ORIGIN}/meldingen/afmelden/")));
    let body: String =
        sqlx::query_scalar("SELECT text_body FROM email_outbox WHERE kind = 'drop_alert'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert!(body.contains("Desem spelt"), "{body}");
    assert!(
        body.contains(&format!("{ORIGIN}/drops/{}", day.id.get())),
        "{body}"
    );
    assert!(body.contains(&unsubscribe), "{body}");

    let report = alert_tick(&mut conn, &clock, "2026-10-24T08:01:00Z").await;
    assert_eq!(report, AlertReport::default());
    assert_eq!(alerts(&mut conn).await.len(), 1);
    assert_eq!(
        alert_rows(&mut conn).await,
        [(day.id.get(), "menu_published".to_owned(), 1)]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn opening_sends_the_ordering_alert_and_unsubscribed_people_hear_nothing(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = FakeClock::new(ts("2026-10-20T08:00:00Z"));
    setup(&mut conn, &clock).await;
    subscriber(&mut conn, &clock, "later-weg@example.nl", State::Confirmed).await;
    let day = saturday(
        &mut conn,
        Some("2026-10-24T08:00:00Z"),
        Some("2026-10-26T08:00:00Z"),
    )
    .await;
    alert_tick(&mut conn, &clock, "2026-10-24T08:00:00Z").await;
    let first = alerts(&mut conn).await;
    assert_eq!(first.len(), 2);
    let leaving = first
        .iter()
        .find(|(to, _, _)| to == "later-weg@example.nl")
        .and_then(|(_, _, url)| url.clone())
        .unwrap();
    let token = token_after(&leaving, "/meldingen/afmelden/");
    assert!(
        subscribers::unsubscribe(&mut conn, &clock, &token)
            .await
            .unwrap()
    );

    clock.set(ts("2026-10-26T08:00:00Z"));
    days::open(&mut conn, &clock, day.id).await.unwrap();
    let report = alert_tick(&mut conn, &clock, "2026-10-26T08:00:00Z").await;
    assert_eq!(
        report.queued,
        [QueuedAlert {
            bake_day: day.id,
            kind: DropAlertKind::OrderingOpen,
            recipients: 1,
        }]
    );
    let all = alerts(&mut conn).await;
    assert_eq!(all.len(), 3);
    assert_eq!(all[2].0, "ja@example.nl");
    assert_ne!(all[2].1, all[0].1, "the ordering alert has its own subject");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_day_that_opens_with_its_menu_gets_only_the_ordering_alert(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = FakeClock::new(ts("2026-10-20T08:00:00Z"));
    setup(&mut conn, &clock).await;
    let day = saturday(
        &mut conn,
        Some("2026-10-26T08:00:00Z"),
        Some("2026-10-26T08:00:00Z"),
    )
    .await;
    let sender = Arc::new(FakeEmailSender::new());
    let mail = Mail {
        sender: Arc::clone(&sender) as Arc<dyn EmailSender>,
        i18n: Arc::new(i18n()),
        links: links(),
    };
    // The confirmation emails of setup() go out on the first tick too.
    clock.set(ts("2026-10-26T08:00:00Z"));
    let outcome = work(&pool, &clock, &mail).await.unwrap();
    assert_eq!(outcome.days.opened, [day.id]);
    assert_eq!(
        outcome.alerts.queued,
        [QueuedAlert {
            bake_day: day.id,
            kind: DropAlertKind::OrderingOpen,
            recipients: 1,
        }]
    );
    assert_eq!(outcome.sent.sent, 4);
    let sent = sender.sent();
    let alert = sent
        .iter()
        .find(|email| email.email.kind == notify::EmailKind::DropAlert)
        .expect("alert sent");
    assert_eq!(alert.email.to, "ja@example.nl");
    assert!(alert.email.list_unsubscribe.is_some());

    let again = work(&pool, &clock, &mail).await.unwrap();
    assert_eq!(again.alerts, AlertReport::default());
    assert_eq!(again.sent, notify::SendReport::default());
    assert_eq!(sender.sent().len(), 4);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn production_only_cancelled_and_past_deadline_drops_never_alert(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = FakeClock::new(ts("2026-10-20T08:00:00Z"));
    setup(&mut conn, &clock).await;
    let cancelled = saturday(&mut conn, Some("2026-10-24T08:00:00Z"), None).await;
    days::cancel(&mut conn, &clock, cancelled.id, "Ziek")
        .await
        .unwrap();
    let production = days::create(
        &mut conn,
        &NewBakeDay {
            date: "2026-11-07".parse().unwrap(),
            location_id: PickupLocationId::new(1),
            bake_starts_at: ts("2026-11-07T03:00:00Z"),
            online_ordering: false,
            charity_id: None,
        },
    )
    .await
    .unwrap();
    schedule(&mut conn, &production, Some("2026-10-24T08:00:00Z"), None).await;
    let late = days::create(
        &mut conn,
        &NewBakeDay {
            date: "2026-10-24".parse().unwrap(),
            location_id: PickupLocationId::new(1),
            bake_starts_at: ts("2026-10-24T03:00:00Z"),
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .unwrap();
    // Its menu came out, but the tick only runs after its deadline (22 October 20:00).
    schedule(&mut conn, &late, Some("2026-10-21T08:00:00Z"), None).await;

    let report = alert_tick(&mut conn, &clock, "2026-10-28T08:00:00Z").await;
    assert_eq!(report, AlertReport::default());
    assert!(alerts(&mut conn).await.is_empty());
    let other = days::get(&mut conn, cancelled.id).await.unwrap();
    assert_eq!(other.status, BakeDayStatus::Cancelled);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn two_ticks_at_once_queue_each_alert_once(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = FakeClock::new(ts("2026-10-20T08:00:00Z"));
    setup(&mut conn, &clock).await;
    for n in 0..5 {
        subscriber(
            &mut conn,
            &clock,
            &format!("lid{n}@example.nl"),
            State::Confirmed,
        )
        .await;
    }
    saturday(&mut conn, Some("2026-10-24T08:00:00Z"), None).await;
    clock.set(ts("2026-10-24T09:00:00Z"));

    let mut a = pool.acquire().await.unwrap();
    let mut b = pool.acquire().await.unwrap();
    let (i18n_a, i18n_b) = (i18n(), i18n());
    let (links_a, links_b) = (links(), links());
    let (ra, rb) = tokio::join!(
        tick(&mut a, &clock, &i18n_a, &links_a),
        tick(&mut b, &clock, &i18n_b, &links_b),
    );
    let queued: u32 = ra
        .unwrap()
        .queued
        .iter()
        .chain(&rb.unwrap().queued)
        .map(|q| q.recipients)
        .sum();
    assert_eq!(queued, 6);
    assert_eq!(alerts(&mut conn).await.len(), 6);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_tick_that_meets_another_mid_send_queues_nothing(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = Arc::new(FakeClock::new(ts("2026-10-20T08:00:00Z")));
    setup(&mut conn, &clock).await;
    let day = saturday(&mut conn, Some("2026-10-24T08:00:00Z"), None).await;
    clock.set(ts("2026-10-24T09:00:00Z"));

    let mut other = pool.acquire().await.unwrap();
    let mut held = other.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO drop_alert_sent (bake_day_id, kind, recipients, sent_at)
        VALUES ($1, 'menu_published', 0, now())",
    )
    .bind(day.id.get())
    .execute(&mut *held)
    .await
    .unwrap();

    let waiting = {
        let pool = pool.clone();
        let clock = Arc::clone(&clock);
        tokio::spawn(async move {
            let mut conn = pool.acquire().await.unwrap();
            tick(&mut conn, clock.as_ref(), &i18n(), &links())
                .await
                .unwrap()
        })
    };
    // The spawned tick blocks on the unique row until the holder commits.
    loop {
        let blocked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity
            WHERE wait_event_type = 'Lock' AND query LIKE 'INSERT INTO drop_alert_sent%'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        if blocked > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    held.commit().await.unwrap();
    assert_eq!(waiting.await.unwrap(), AlertReport::default());
    assert!(alerts(&mut conn).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn missing_company_details_hold_the_alert_until_fixed(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = FakeClock::new(ts("2026-10-20T08:00:00Z"));
    setup(&mut conn, &clock).await;
    let day = saturday(&mut conn, Some("2026-10-24T08:00:00Z"), None).await;
    sqlx::query("UPDATE company SET kvk = NULL")
        .execute(&mut *conn)
        .await
        .unwrap();

    let report = alert_tick(&mut conn, &clock, "2026-10-24T08:00:00Z").await;
    assert_eq!(report.failed, [(day.id, DropAlertKind::MenuPublished)]);
    assert!(alert_rows(&mut conn).await.is_empty());
    assert!(alerts(&mut conn).await.is_empty());

    company_details(&mut conn).await;
    let report = alert_tick(&mut conn, &clock, "2026-10-24T08:01:00Z").await;
    assert_eq!(report.queued.len(), 1);
    assert_eq!(alerts(&mut conn).await.len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_company_row_is_read_for_emails(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    company_details(&mut conn).await;
    assert_eq!(
        notify::load_company(&mut conn).await.unwrap(),
        Company {
            name: "Brood voor Weldadigheid".to_owned(),
            address: "Majoor van Swietenlaan 15, 8382 CE Frederiksoord".to_owned(),
            kvk: "12345678".to_owned(),
        }
    );
}
