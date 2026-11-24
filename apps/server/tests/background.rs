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
async fn the_runner_registers_its_worker_and_stops_at_shutdown(pool: PgPool) {
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let (shutdown, trigger) = Shutdown::on(async {
        stopped.await.ok();
    });
    tokio::spawn(trigger);
    let id = format!("test-{}", std::process::id());
    let runner = tokio::spawn(run_jobs(pool.clone(), clock(), id.clone(), shutdown));
    let registered = async {
        loop {
            let found: Option<i64> =
                sqlx::query_scalar("SELECT count(*) FROM apalis.workers WHERE id = $1")
                    .bind(format!("bake-day-ticks-{id}"))
                    .fetch_one(&pool)
                    .await
                    .ok();
            if found.unwrap_or(0) > 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(BOUND, registered)
        .await
        .expect("the worker registers");
    stop.send(()).unwrap();
    tokio::time::timeout(BOUND, runner)
        .await
        .expect("stops at shutdown")
        .unwrap();
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
    tokio::time::timeout(BOUND, runner)
        .await
        .expect("stops while the database is down")
        .unwrap();
}

#[test]
fn worker_ids_carry_the_process_id() {
    assert!(
        worker_id().ends_with(&format!("-{}", std::process::id())),
        "{}",
        worker_id()
    );
}
