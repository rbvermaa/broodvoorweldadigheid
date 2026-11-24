//! The jobs runner the server starts next to the web server (ADR 0011, ADR 0020).
#![forbid(unsafe_code)]

use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use std::time::Duration;

use server::background::{Shutdown, run_jobs, worker_id};
use sqlx::PgPool;
use testkit::FakeClock;

const BOUND: Duration = Duration::from_secs(30);

fn clock() -> Arc<FakeClock> {
    Arc::new(FakeClock::new("2026-10-28T12:00:00Z".parse().unwrap()))
}

#[tokio::test]
async fn shutdown_reaches_waiters_made_before_and_after_it() {
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let (shutdown, trigger) = Shutdown::on(async {
        stopped.await.ok();
    });
    let early = shutdown.wait();
    let trigger = tokio::spawn(trigger);
    stop.send(()).unwrap();
    trigger.await.unwrap();
    tokio::time::timeout(BOUND, early)
        .await
        .expect("early waiter");
    tokio::time::timeout(BOUND, shutdown.wait())
        .await
        .expect("late waiter");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_runner_sets_up_the_queue_and_stops_at_shutdown(pool: PgPool) {
    let (shutdown, trigger) = Shutdown::on(async {});
    trigger.await;
    let result = tokio::time::timeout(
        BOUND,
        run_jobs(pool.clone(), clock(), worker_id(), shutdown),
    )
    .await
    .expect("stops at shutdown");
    assert!(result.is_ok(), "{result:?}");
    let queue: bool = sqlx::query_scalar("SELECT to_regclass('apalis.jobs') IS NOT NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(queue, "jobs::setup ran");
}

#[tokio::test]
async fn the_runner_waits_for_the_database_and_still_stops() {
    let port = {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let pool = db::connect_lazy(
        &format!("postgres://postgres@127.0.0.1:{port}/postgres"),
        Duration::from_millis(300),
    )
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let (shutdown, trigger) = Shutdown::on(async {
        stopped.await.ok();
    });
    tokio::spawn(trigger);
    let runner = tokio::spawn(run_jobs(pool, clock(), worker_id(), shutdown));
    stop.send(()).unwrap();
    let result = tokio::time::timeout(BOUND, runner)
        .await
        .expect("stops while the database is down")
        .unwrap();
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn worker_ids_carry_the_process_id() {
    assert!(
        worker_id().ends_with(&format!("-{}", std::process::id())),
        "{}",
        worker_id()
    );
}
