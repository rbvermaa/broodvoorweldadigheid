#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedays::{BakeDay, BakeDayStatus, NewBakeDay, Schedule, days};
use domain::{Clock, PickupLocationId};
use jiff::Timestamp;
use jobs::bake_days::tick;
use jobs::scheduler::{TickStorage, tick_key};
use jobs::{TickReport, schedule_tick, setup};
use sqlx::pool::PoolConnection;
use sqlx::{Acquire, PgConnection, PgPool, Postgres};
use testkit::FakeClock;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

/// Saturday after the October change: pickup 11:00 CET, deadline Thursday 20:00 CET.
fn saturday() -> NewBakeDay {
    NewBakeDay {
        date: "2026-10-31".parse().unwrap(),
        location_id: PickupLocationId::new(1),
        bake_starts_at: ts("2026-10-31T03:00:00Z"),
        online_ordering: true,
        charity_id: None,
    }
}

async fn day_opening_at(conn: &mut PgConnection, input: &NewBakeDay, opens_at: &str) -> BakeDay {
    let day = days::create(conn, input).await.unwrap();
    days::update_schedule(
        conn,
        day.id,
        &Schedule {
            online_ordering: day.online_ordering,
            menu_publish_at: None,
            opens_at: Some(ts(opens_at)),
            cutoff_at: day.cutoff_at,
            bake_starts_at: day.bake_starts_at,
        },
    )
    .await
    .unwrap()
}

async fn status(conn: &mut PgConnection, day: &BakeDay) -> BakeDayStatus {
    days::get(conn, day.id).await.unwrap().status
}

async fn tick_at(conn: &mut PgConnection, clock: &FakeClock, now: &str) -> TickReport {
    clock.set(ts(now));
    tick(conn, clock).await.unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn opens_at_opens_at_and_closes_exactly_at_the_deadline_after_the_clock_change(
    mut conn: PoolConnection<Postgres>,
) {
    // Monday 26 October 09:00 CET, the day after summer time ends.
    let day = day_opening_at(&mut conn, &saturday(), "2026-10-26T08:00:00Z").await;
    assert_eq!(day.cutoff_at, Some(ts("2026-10-29T19:00:00Z")));
    let clock = FakeClock::new(ts("2026-10-25T00:00:00Z"));

    let report = tick_at(&mut conn, &clock, "2026-10-26T07:59:59Z").await;
    assert_eq!(report, TickReport::default());
    assert_eq!(status(&mut conn, &day).await, BakeDayStatus::Draft);

    let report = tick_at(&mut conn, &clock, "2026-10-26T08:00:00Z").await;
    assert_eq!(report.opened, [day.id]);
    assert_eq!(status(&mut conn, &day).await, BakeDayStatus::Open);

    let report = tick_at(&mut conn, &clock, "2026-10-29T18:59:59Z").await;
    assert_eq!(report, TickReport::default());
    assert_eq!(status(&mut conn, &day).await, BakeDayStatus::Open);

    let report = tick_at(&mut conn, &clock, "2026-10-29T19:00:00Z").await;
    assert_eq!(report.closed, [day.id]);
    assert_eq!(status(&mut conn, &day).await, BakeDayStatus::Closed);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_second_tick_changes_nothing(mut conn: PoolConnection<Postgres>) {
    let day = day_opening_at(&mut conn, &saturday(), "2026-10-26T08:00:00Z").await;
    let clock = FakeClock::new(ts("2026-10-27T12:00:00Z"));
    assert_eq!(tick(&mut conn, &clock).await.unwrap().opened, [day.id]);
    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        TickReport::default()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn days_without_opening_time_production_only_or_past_deadline_stay_shut(
    mut conn: PoolConnection<Postgres>,
) {
    let no_opening_time = days::create(&mut conn, &saturday()).await.unwrap();
    let production_only = days::create(
        &mut conn,
        &NewBakeDay {
            date: "2026-11-14".parse().unwrap(),
            bake_starts_at: ts("2026-11-14T03:00:00Z"),
            online_ordering: false,
            ..saturday()
        },
    )
    .await
    .unwrap();
    let missed = day_opening_at(
        &mut conn,
        &NewBakeDay {
            date: "2026-11-07".parse().unwrap(),
            bake_starts_at: ts("2026-11-07T03:00:00Z"),
            ..saturday()
        },
        "2026-11-02T08:00:00Z",
    )
    .await;
    let clock = FakeClock::new(ts("2026-11-05T19:00:00Z"));

    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        TickReport::default()
    );

    for day in [&no_opening_time, &production_only, &missed] {
        assert_eq!(status(&mut conn, day).await, BakeDayStatus::Draft);
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn cancelled_days_are_left_alone(mut conn: PoolConnection<Postgres>) {
    let day = day_opening_at(&mut conn, &saturday(), "2026-10-26T08:00:00Z").await;
    let clock = FakeClock::new(ts("2026-10-25T12:00:00Z"));
    days::cancel(&mut conn, &clock, day.id, "Ziek")
        .await
        .unwrap();
    clock.set(ts("2026-10-27T12:00:00Z"));
    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        TickReport::default()
    );
    assert_eq!(status(&mut conn, &day).await, BakeDayStatus::Cancelled);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_day_locked_by_another_session_waits_for_the_next_tick(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let day = day_opening_at(&mut conn, &saturday(), "2026-10-26T08:00:00Z").await;
    let clock = FakeClock::new(ts("2026-10-27T12:00:00Z"));

    let mut other = pool.acquire().await.unwrap();
    let mut held = other.begin().await.unwrap();
    sqlx::query("SELECT id FROM bake_day WHERE id = $1 FOR UPDATE")
        .bind(day.id.get())
        .execute(&mut *held)
        .await
        .unwrap();

    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        TickReport::default()
    );
    held.rollback().await.unwrap();

    assert_eq!(tick(&mut conn, &clock).await.unwrap().opened, [day.id]);
}

async fn queued_ticks(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT idempotency_key FROM apalis.jobs WHERE idempotency_key LIKE 'bake-day-tick:%'
        ORDER BY idempotency_key",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn one_tick_is_queued_per_minute_however_many_servers_ask(pool: PgPool) {
    setup(&pool).await.unwrap();
    let mut first_server = TickStorage::new(&pool);
    let mut second_server = TickStorage::new(&pool);
    let clock = FakeClock::new(ts("2026-10-29T18:59:05Z"));

    assert!(schedule_tick(&mut first_server, &clock).await.unwrap());
    clock.set(ts("2026-10-29T18:59:55Z"));
    assert!(!schedule_tick(&mut second_server, &clock).await.unwrap());
    clock.set(ts("2026-10-29T19:00:00Z"));
    assert!(schedule_tick(&mut second_server, &clock).await.unwrap());

    assert_eq!(
        queued_ticks(&pool).await,
        [
            tick_key(ts("2026-10-29T18:59:00Z")),
            tick_key(ts("2026-10-29T19:00:00Z")),
        ]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn setup_can_run_twice(pool: PgPool) {
    setup(&pool).await.unwrap();
    setup(&pool).await.unwrap();
}

async fn workers(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM apalis.workers WHERE id LIKE 'bake-day-ticks-%'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn two_servers_work_the_queue_and_open_the_day_once(pool: PgPool) {
    setup(&pool).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let day = day_opening_at(&mut conn, &saturday(), "2026-10-26T08:00:00Z").await;
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(ts("2026-10-27T12:00:00Z")));
    let mut servers = Vec::new();
    for name in ["server-a", "server-b"] {
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(jobs::run(
            pool.clone(),
            Arc::clone(&clock),
            jobs::Mail {
                sender: Arc::new(testkit::FakeEmailSender::new()),
                i18n: Arc::new(i18n::Translator::dutch().unwrap()),
                links: notify::subscribers::Links::new("https://broodvoorweldadigheid.nl"),
            },
            name.to_owned(),
            async {
                stopped.await.ok();
            },
        ));
        servers.push((stop, handle));
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    while status(&mut conn, &day).await != BakeDayStatus::Open || workers(&pool).await < 2 {
        assert!(
            Instant::now() < deadline,
            "the day never opened on two workers"
        );
        assert!(
            servers.iter().all(|(_, handle)| !handle.is_finished()),
            "a server stopped early"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for (stop, handle) in servers {
        stop.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }
    assert_eq!(
        queued_ticks(&pool).await,
        [tick_key(ts("2026-10-27T12:00:00Z"))]
    );
}
