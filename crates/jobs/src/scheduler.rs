use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use apalis::prelude::{BoxDynError, Data, TaskBuilder, TaskSink, TaskSinkError, WorkerBuilder};
use apalis_postgres::PostgresStorage;
use domain::{Clock, OrderId};
use i18n::Translator;
use jiff::{RoundMode, Timestamp, TimestampRound, Unit};
use notify::subscribers::Links;
use notify::{EmailSender, OutgoingEmail, SendError, SendFuture, SendReport};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::time::MissedTickBehavior;

use crate::Error;
use crate::bake_days::{self, TickReport};
use crate::batches::{self, SplitReport};
use crate::cleanup::{self, CleanupReport};
use crate::drop_alerts::{self, AlertReport};
use crate::order_emails::{self, OrderEmails};
use crate::saved_donations::{self, MoveReport};

const TICK_EVERY: Duration = Duration::from_secs(60);
const TICK_ATTEMPTS: usize = 3;
/// Emails handed to the relay per tick at most; the rest wait for the next minute.
pub const SEND_BATCH: u32 = 500;
/// Sending stops after this much time in one tick, also in the middle of an email, so a
/// slow relay cannot keep a tick, and its connection, busy past the next one.
pub const SEND_BUDGET: Duration = Duration::from_secs(40);

/// The every-minute job that opens and closes bake days, releases expired order holds
/// and queues due pickup reminders. `minute` is the start of the minute it was scheduled
/// for and makes the job unique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BakeDayTick {
    pub minute: Timestamp,
}

pub type TickStorage = PostgresStorage<BakeDayTick>;

/// The clock the worker hands to each tick.
#[derive(Clone)]
pub struct TickClock(pub Arc<dyn Clock>);

/// One instant for the bake-day and alert steps, so a day whose menu and opening time
/// fall in the same minute is opened before the alerts look at it.
struct Frozen(Timestamp);

impl Clock for Frozen {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// What the tick needs to write and send email: drop alerts (F9a), pickup reminders (F9)
/// and the outbox (ADR 0014). Without a `sender` (no SMTP relay configured) emails are
/// still queued and wait in the outbox.
#[derive(Clone)]
pub struct Mail {
    pub sender: Option<Arc<dyn EmailSender>>,
    pub i18n: Arc<Translator>,
    pub links: Links,
    pub orders: Arc<OrderEmails>,
}

/// What one tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickOutcome {
    pub days: TickReport,
    pub batches: SplitReport,
    pub released: Vec<OrderId>,
    pub reminded: Vec<OrderId>,
    pub donations: MoveReport,
    pub alerts: AlertReport,
    pub sent: SendReport,
    pub cleaned: CleanupReport,
}

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

/// One tick's work, in order: open and close bake days, split closed days into batches
/// (F14a), release expired order holds, queue
/// due pickup reminders and move saved donations past their date (an error in those two is
/// logged and tried again next tick), queue
/// the drop alerts that are now due, hand due emails to the relay, then delete expired
/// customer logins and finished account-deleted emails. Every step is safe to repeat, so a retry
/// of the whole tick changes nothing that already happened. Alerts wait while the bake-day
/// step fails, so a day that should open never gets the menu alert instead; emails are
/// sent either way. The first error is returned.
///
/// # Errors
/// The first [`Error`] of [`bake_days::tick`], [`batches::tick`],
/// [`orders::release_expired_holds`],
/// [`drop_alerts::tick`], [`notify::send_pending`] or [`cleanup::tick`].
pub fn work<'a>(
    pool: &'a PgPool,
    clock: &'a dyn Clock,
    mail: &'a Mail,
) -> Pin<Box<dyn Future<Output = Result<TickOutcome, Error>> + Send + 'a>> {
    // Boxed as a `dyn Send` future: without it rustc cannot prove the sqlx futures inside
    // are `Send` for every lifetime, and spawning `run` fails to compile.
    Box::pin(async move {
        let mut conn = pool.acquire().await?;
        let mut outcome = TickOutcome::default();
        let mut first_error = None;
        let frozen = Frozen(clock.now());
        let days_ok = match bake_days::tick(&mut conn, &frozen).await {
            Ok(report) => {
                outcome.days = report;
                true
            }
            Err(err) => {
                first_error = Some(err);
                false
            }
        };
        match batches::tick(&mut conn, &frozen).await {
            Ok(report) => outcome.batches = report,
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
        match orders::release_expired_holds(&mut conn, &frozen).await {
            Ok(released) => outcome.released = released,
            Err(err) => {
                first_error.get_or_insert(err.into());
            }
        }
        match order_emails::enqueue_due_reminders(&mut conn, &frozen, &mail.orders).await {
            Ok(reminded) => outcome.reminded = reminded,
            Err(error @ order_emails::EmailError::CompanyIncomplete) => {
                tracing::warn!(%error, "no pickup reminders");
            }
            Err(error) => tracing::error!(%error, "cannot queue pickup reminders"),
        }
        match saved_donations::tick(&mut conn, &frozen, &mail.orders).await {
            Ok(report) => outcome.donations = report,
            Err(error @ order_emails::EmailError::CompanyIncomplete) => {
                tracing::warn!(%error, "saved donations wait");
            }
            Err(error) => tracing::error!(%error, "cannot move saved donations"),
        }
        if days_ok {
            match drop_alerts::tick(&mut conn, &frozen, &mail.i18n, &mail.links).await {
                Ok(report) => outcome.alerts = report,
                Err(err) => {
                    first_error.get_or_insert(err);
                }
            }
        }
        if let Some(sender) = &mail.sender {
            match drain(&mut conn, sender.as_ref(), clock).await {
                Ok(report) => outcome.sent = report,
                Err(err) => {
                    first_error.get_or_insert(err.into());
                }
            }
        }
        match cleanup::tick(&mut conn, &frozen).await {
            Ok(report) => outcome.cleaned = report,
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(outcome),
        }
    })
}

/// An [`EmailSender`] that gives up at `deadline` with [`SendError::Transient`], so the
/// outbox records a retry instead of waiting on a relay that stopped answering.
struct Deadline<'a> {
    inner: &'a dyn EmailSender,
    deadline: tokio::time::Instant,
}

impl EmailSender for Deadline<'_> {
    fn send<'a>(&'a self, email: &'a OutgoingEmail) -> SendFuture<'a> {
        Box::pin(async move {
            tokio::time::timeout_at(self.deadline, self.inner.send(email))
                .await
                .unwrap_or_else(|_| {
                    Err(SendError::Transient(
                        "the relay did not answer within the tick's send budget".to_owned(),
                    ))
                })
        })
    }
}

/// Sends one email at a time until none is due, [`SEND_BATCH`] went out or
/// [`SEND_BUDGET`] is used up.
async fn drain(
    conn: &mut sqlx::PgConnection,
    sender: &dyn EmailSender,
    clock: &dyn Clock,
) -> Result<SendReport, notify::OutboxError> {
    let sender = Deadline {
        inner: sender,
        deadline: tokio::time::Instant::now() + SEND_BUDGET,
    };
    let mut total = SendReport::default();
    for _ in 0..SEND_BATCH {
        if tokio::time::Instant::now() >= sender.deadline {
            break;
        }
        let report = notify::send_pending(conn, &sender, clock, 1).await?;
        if report == SendReport::default() {
            break;
        }
        total.sent += report.sent;
        total.retrying += report.retrying;
        total.failed += report.failed;
    }
    Ok(total)
}

/// Runs one queued tick against the current time, not the minute it was queued for, so a
/// late run still does what is due.
///
/// # Errors
/// Any [`Error`] from [`work`]; apalis retries the job.
pub async fn handle_tick(
    tick: BakeDayTick,
    pool: Data<PgPool>,
    clock: Data<TickClock>,
    mail: Data<Mail>,
) -> Result<(), BoxDynError> {
    let outcome = work(&pool, clock.0.0.as_ref(), &mail).await?;
    let days = &outcome.days;
    if !days.opened.is_empty() || !days.closed.is_empty() || !outcome.released.is_empty() {
        tracing::info!(
            minute = %tick.minute,
            opened = ?days.opened,
            closed = ?days.closed,
            released = ?outcome.released,
            "bake day tick"
        );
    }
    if !outcome.batches.created.is_empty() {
        tracing::info!(minute = %tick.minute, days = ?outcome.batches.days, batches = ?outcome.batches.created, "batches made");
    }
    if !outcome.reminded.is_empty() {
        tracing::info!(minute = %tick.minute, orders = ?outcome.reminded, "pickup reminders queued");
    }
    if !outcome.donations.moved.is_empty() {
        tracing::info!(minute = %tick.minute, allocations = ?outcome.donations.moved, "saved donations moved");
    }
    if !outcome.alerts.queued.is_empty() {
        tracing::info!(minute = %tick.minute, alerts = ?outcome.alerts.queued, "drop alerts queued");
    }
    if outcome.sent != SendReport::default() {
        tracing::info!(
            minute = %tick.minute,
            sent = outcome.sent.sent,
            retrying = outcome.sent.retrying,
            failed = outcome.sent.failed,
            "emails"
        );
    }
    let cleaned = outcome.cleaned;
    if cleaned != CleanupReport::default() {
        tracing::info!(
            minute = %tick.minute,
            login_links = cleaned.login_links,
            logins = cleaned.logins,
            emails = cleaned.emails,
            old_emails = cleaned.old_emails,
            "cleanup"
        );
    }
    Ok(())
}

async fn queue_tick(storage: &mut TickStorage, clock: &dyn Clock) {
    if let Err(err) = schedule_tick(storage, clock).await {
        tracing::error!(error = %err, "cannot queue the bake day tick");
    }
}

/// Queues a tick every minute and works the queue until `shutdown` completes. `worker_id`
/// must differ per running server: apalis registers each worker under it.
///
/// # Errors
/// [`Error::Worker`] when the worker stops with an error.
pub async fn run<F>(
    pool: PgPool,
    clock: Arc<dyn Clock>,
    mail: Mail,
    worker_id: String,
    shutdown: F,
) -> Result<(), Error>
where
    F: Future<Output = ()> + Send + 'static,
{
    let mut storage = TickStorage::new(&pool);
    // Before the worker starts: an idle worker fetches again only when its task is woken,
    // at the latest by the 30 s apalis heartbeat, so its first fetch must see this tick.
    queue_tick(&mut storage, clock.as_ref()).await;
    let worker = WorkerBuilder::new(format!("bake-day-ticks-{worker_id}"))
        .backend(storage.clone())
        .data(pool)
        .data(TickClock(Arc::clone(&clock)))
        .data(mail)
        .build(handle_tick);
    let schedule = async move {
        let mut every_minute =
            tokio::time::interval_at(tokio::time::Instant::now() + TICK_EVERY, TICK_EVERY);
        every_minute.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            every_minute.tick().await;
            queue_tick(&mut storage, clock.as_ref()).await;
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

#[cfg(test)]
mod tests {
    use notify::{Email, EmailKind};

    use super::*;

    struct Silent;

    impl EmailSender for Silent {
        fn send<'a>(&'a self, _: &'a OutgoingEmail) -> SendFuture<'a> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn a_relay_that_stops_answering_is_temporary_at_the_deadline() {
        let email = OutgoingEmail {
            id: "0b5e7c2a-1111-4222-8333-944455556666".to_owned(),
            date: "2026-12-01T08:00:00Z".parse().unwrap(),
            email: Email {
                kind: EmailKind::PickupReminder,
                to: "sanne@example.org".to_owned(),
                subject: "Ophalen".to_owned(),
                text: "Hoi Sanne,\n".to_owned(),
                html: "<p>Hoi Sanne,</p>".to_owned(),
                list_unsubscribe: None,
                reply_to: None,
                cc: None,
            },
        };
        let sender = Deadline {
            inner: &Silent,
            deadline: tokio::time::Instant::now() + Duration::from_millis(20),
        };
        let result = tokio::time::timeout(Duration::from_secs(5), sender.send(&email))
            .await
            .expect("the deadline ends the send");
        assert!(matches!(result, Err(SendError::Transient(_))), "{result:?}");
    }
}
