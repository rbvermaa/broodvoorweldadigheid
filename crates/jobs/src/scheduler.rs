use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use apalis::prelude::{BoxDynError, Data, TaskBuilder, TaskSink, TaskSinkError, WorkerBuilder};
use apalis_postgres::PostgresStorage;
use domain::Clock;
use jiff::{RoundMode, Timestamp, TimestampRound, Unit};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::time::MissedTickBehavior;

use crate::Error;
use crate::bake_days::{self, TickReport};

const TICK_EVERY: Duration = Duration::from_secs(60);
const TICK_ATTEMPTS: usize = 3;

/// The every-minute job that opens and closes bake days. `minute` is the start of the
/// minute it was scheduled for and makes the job unique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BakeDayTick {
    pub minute: Timestamp,
}

pub type TickStorage = PostgresStorage<BakeDayTick>;

/// The clock the worker hands to each tick.
#[derive(Clone)]
pub struct TickClock(pub Arc<dyn Clock>);

/// Creates or upgrades the apalis tables in their own `apalis` schema.
///
/// # Errors
/// [`Error::Queue`] when the apalis migrations fail.
pub async fn setup(pool: &PgPool) -> Result<(), Error> {
    PostgresStorage::<()>::setup(pool).await?;
    Ok(())
}

/// `bake-day-tick:2026-10-29T19:00:00Z`
#[must_use]
pub fn tick_key(minute: Timestamp) -> String {
    format!("bake-day-tick:{minute}")
}

fn start_of_minute(at: Timestamp) -> Timestamp {
    at.round(
        TimestampRound::new()
            .smallest(Unit::Minute)
            .mode(RoundMode::Trunc),
    )
    .unwrap_or(at)
}

/// Queues the tick for the current minute. Every server calls this; the idempotency key
/// keeps one job per minute, so `false` means another server queued it first.
///
/// # Errors
/// [`Error::Queue`] for anything but a duplicate key.
pub fn schedule_tick<'a>(
    storage: &'a mut TickStorage,
    clock: &dyn Clock,
) -> impl Future<Output = Result<bool, Error>> + Send + 'a {
    push_tick(storage, start_of_minute(clock.now()))
}

async fn push_tick(storage: &mut TickStorage, minute: Timestamp) -> Result<bool, Error> {
    let task = TaskBuilder::new(BakeDayTick { minute })
        .idempotency_key(tick_key(minute))
        .max_attempts(TICK_ATTEMPTS)
        .build();
    match storage.push_task(task).await {
        Ok(()) => Ok(true),
        Err(TaskSinkError::PushError(apalis_postgres::Error::Database(err)))
            if err
                .as_database_error()
                .is_some_and(|db| db.is_unique_violation()) =>
        {
            Ok(false)
        }
        Err(TaskSinkError::PushError(err)) => Err(err.into()),
        Err(err) => Err(Error::Worker(err.to_string())),
    }
}

/// Runs one queued tick against the current time, not the minute it was queued for, so a
/// late run still opens and closes what is due.
///
/// # Errors
/// Any [`Error`] from [`bake_days::tick`]; apalis retries the job.
pub async fn handle_tick(
    tick: BakeDayTick,
    pool: Data<PgPool>,
    clock: Data<TickClock>,
) -> Result<(), BoxDynError> {
    let mut conn = pool.acquire().await?;
    // Boxed as a `dyn Send` future: without it rustc cannot prove the sqlx futures inside
    // are `Send` for every lifetime, and spawning `run` fails to compile.
    let due: Pin<Box<dyn Future<Output = Result<TickReport, Error>> + Send + '_>> =
        Box::pin(bake_days::tick(&mut conn, clock.0.0.as_ref()));
    let report = due.await?;
    if !report.opened.is_empty() || !report.closed.is_empty() {
        tracing::info!(
            minute = %tick.minute,
            opened = ?report.opened,
            closed = ?report.closed,
            "bake day tick"
        );
    }
    Ok(())
}

/// Queues a tick every minute and works the queue until `shutdown` completes. `worker_id`
/// must differ per running server: apalis registers each worker under it.
///
/// # Errors
/// [`Error::Worker`] when the worker stops with an error.
pub async fn run<F>(
    pool: PgPool,
    clock: Arc<dyn Clock>,
    worker_id: String,
    shutdown: F,
) -> Result<(), Error>
where
    F: Future<Output = ()> + Send + 'static,
{
    let mut storage = TickStorage::new(&pool);
    let worker = WorkerBuilder::new(format!("bake-day-ticks-{worker_id}"))
        .backend(storage.clone())
        .data(pool)
        .data(TickClock(Arc::clone(&clock)))
        .build(handle_tick);
    let schedule = async move {
        let mut every_minute = tokio::time::interval(TICK_EVERY);
        every_minute.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            every_minute.tick().await;
            if let Err(err) = schedule_tick(&mut storage, clock.as_ref()).await {
                tracing::error!(error = %err, "cannot queue the bake day tick");
            }
        }
    };
    tokio::select! {
        result = worker.run_until(async move {
            shutdown.await;
            Ok::<(), std::io::Error>(())
        }) => result.map_err(|err| Error::Worker(err.to_string())),
        () = schedule => Ok(()),
    }
}
