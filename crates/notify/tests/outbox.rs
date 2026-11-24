#![forbid(unsafe_code)]

use std::sync::Mutex;
use std::time::Duration;

use jiff::SignedDuration;
use notify::outbox::{BACKOFF, MAX_ATTEMPTS};
use notify::{
    Email, EmailKind, EmailSender, OutgoingEmail, SendError, SendFuture, SendReport, enqueue,
    send_pending,
};
use sqlx::{Acquire, PgPool};
use testkit::{FakeClock, FakeEmailSender};
use tokio::sync::{Notify, oneshot};

fn clock() -> FakeClock {
    FakeClock::new("2026-12-01T08:00:00Z".parse().unwrap())
}

fn email(to: &str) -> Email {
    Email {
        kind: EmailKind::OrderConfirmation,
        to: to.to_owned(),
        subject: "Je bestelling 1 is bevestigd".to_owned(),
        text: "Hoi,\n".to_owned(),
        html: "<p>Hoi,</p>".to_owned(),
        list_unsubscribe: None,
        reply_to: None,
    }
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct State {
    to_address: String,
    status: String,
    attempts: i32,
    last_error: Option<String>,
}

async fn states(pool: &PgPool) -> Vec<State> {
    sqlx::query_as("SELECT to_address, status, attempts, last_error FROM email_outbox ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn send(
    pool: &PgPool,
    sender: &dyn EmailSender,
    clock: &FakeClock,
    batch: u32,
) -> SendReport {
    let mut conn = pool.acquire().await.unwrap();
    send_pending(&mut conn, sender, clock, batch).await.unwrap()
}

fn report(sent: u32, retrying: u32, failed: u32) -> SendReport {
    SendReport {
        sent,
        retrying,
        failed,
    }
}

fn recipients(sender: &FakeEmailSender) -> Vec<String> {
    sender.sent().into_iter().map(|e| e.email.to).collect()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn queued_emails_are_sent_once_in_order(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    let first = enqueue(&mut conn, &clock, &email("a@example.org"), None)
        .await
        .unwrap();
    let mut drop_alert = email("b@example.org");
    drop_alert.kind = EmailKind::DropAlert;
    drop_alert.list_unsubscribe = Some("https://broodvoorweldadigheid.nl/afmelden/x".to_owned());
    let second = enqueue(&mut conn, &clock, &drop_alert, None).await.unwrap();
    assert!(first.unwrap() < second.unwrap());

    let sender = FakeEmailSender::new();
    clock.advance(SignedDuration::from_secs(5));
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(2, 0, 0));
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(0, 0, 0));

    let sent = sender.sent();
    assert_eq!(recipients(&sender), ["a@example.org", "b@example.org"]);
    assert_eq!(sent[1].email, drop_alert);
    assert_eq!(sent[0].date, domain::Clock::now(&clock));
    assert_ne!(sent[0].id, sent[1].id);
    assert_eq!(sent[0].id.len(), 36, "a uuid: {}", sent[0].id);
    let state = states(&pool).await;
    assert!(state.iter().all(|s| s.status == "sent" && s.attempts == 1));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_email_queued_in_a_rolled_back_transaction_is_never_sent(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    let mut tx = conn.begin().await.unwrap();
    enqueue(&mut tx, &clock, &email("a@example.org"), None)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    let sender = FakeEmailSender::new();
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(0, 0, 0));
    assert!(states(&pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_dedupe_key_queues_an_email_only_once(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    let key = Some("order-confirmation:42");
    assert!(
        enqueue(&mut conn, &clock, &email("a@example.org"), key)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        enqueue(&mut conn, &clock, &email("a@example.org"), key)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        enqueue(&mut conn, &clock, &email("a@example.org"), None)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(states(&pool).await.len(), 2);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn emails_are_not_sent_before_they_are_due(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    enqueue(&mut conn, &clock, &email("a@example.org"), None)
        .await
        .unwrap();
    clock.advance(SignedDuration::from_secs(-1));
    let sender = FakeEmailSender::new();
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(0, 0, 0));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_temporary_failure_is_retried_after_the_backoff_with_the_same_message_id(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    enqueue(&mut conn, &clock, &email("a@example.org"), None)
        .await
        .unwrap();
    let id: String = sqlx::query_scalar("SELECT message_id::text FROM email_outbox")
        .fetch_one(&pool)
        .await
        .unwrap();

    let sender = FakeEmailSender::new();
    sender.fail_next(SendError::Transient("451 4.3.0 try again later".to_owned()));
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(0, 1, 0));
    assert_eq!(
        states(&pool).await,
        [State {
            to_address: "a@example.org".to_owned(),
            status: "pending".to_owned(),
            attempts: 1,
            last_error: Some("451 4.3.0 try again later".to_owned()),
        }]
    );

    clock.advance(BACKOFF[0] - SignedDuration::from_secs(1));
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(0, 0, 0));
    clock.advance(SignedDuration::from_secs(1));
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(1, 0, 0));

    assert_eq!(sender.sent()[0].id, id);
    let state = &states(&pool).await[0];
    assert_eq!((state.status.as_str(), state.attempts), ("sent", 2));
    assert_eq!(state.last_error, None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_permanent_failure_is_not_retried(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    enqueue(&mut conn, &clock, &email("a@example.org"), None)
        .await
        .unwrap();
    enqueue(&mut conn, &clock, &email("b@example.org"), None)
        .await
        .unwrap();

    let sender = FakeEmailSender::new();
    sender.fail_next(SendError::Permanent("550 5.1.1 no such user".to_owned()));
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(1, 0, 1));
    clock.advance(SignedDuration::from_hours(48));
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(0, 0, 0));
    assert_eq!(recipients(&sender), ["b@example.org"]);
    let state = &states(&pool).await[0];
    assert_eq!((state.status.as_str(), state.attempts), ("failed", 1));
    assert_eq!(state.last_error.as_deref(), Some("550 5.1.1 no such user"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_email_fails_for_good_after_the_last_attempt(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    enqueue(&mut conn, &clock, &email("a@example.org"), None)
        .await
        .unwrap();
    let sender = FakeEmailSender::new();
    for attempt in 1..=MAX_ATTEMPTS {
        sender.fail_next(SendError::Transient("timeout".to_owned()));
        let expected = if attempt < MAX_ATTEMPTS {
            report(0, 1, 0)
        } else {
            report(0, 0, 1)
        };
        assert_eq!(
            send(&pool, &sender, &clock, 10).await,
            expected,
            "attempt {attempt}"
        );
        clock.advance(SignedDuration::from_hours(12));
    }
    clock.advance(SignedDuration::from_hours(48));
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(0, 0, 0));
    let state = &states(&pool).await[0];
    assert_eq!(
        (state.status.as_str(), state.attempts),
        ("failed", MAX_ATTEMPTS)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_batch_sends_at_most_its_size(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    for to in ["a@example.org", "b@example.org", "c@example.org"] {
        enqueue(&mut conn, &clock, &email(to), None).await.unwrap();
    }
    let sender = FakeEmailSender::new();
    assert_eq!(send(&pool, &sender, &clock, 2).await, report(2, 0, 0));
    assert_eq!(send(&pool, &sender, &clock, 0).await, report(0, 0, 0));
    assert_eq!(send(&pool, &sender, &clock, 2).await, report(1, 0, 0));
    assert_eq!(
        recipients(&sender),
        ["a@example.org", "b@example.org", "c@example.org"]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_row_locked_by_another_worker_is_skipped_not_waited_for(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    enqueue(&mut conn, &clock, &email("a@example.org"), None)
        .await
        .unwrap();
    enqueue(&mut conn, &clock, &email("b@example.org"), None)
        .await
        .unwrap();

    let mut other = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM email_outbox WHERE to_address = 'a@example.org' FOR UPDATE")
        .execute(&mut *other)
        .await
        .unwrap();

    let sender = FakeEmailSender::new();
    let report_while_locked =
        tokio::time::timeout(Duration::from_secs(10), send(&pool, &sender, &clock, 10))
            .await
            .expect("send_pending must not wait for the lock");
    assert_eq!(report_while_locked, report(1, 0, 0));
    assert_eq!(recipients(&sender), ["b@example.org"]);

    other.rollback().await.unwrap();
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(1, 0, 0));
    assert_eq!(recipients(&sender), ["b@example.org", "a@example.org"]);
}

/// Holds its first email until released, recording everything it sends.
struct HoldFirst {
    entered: Mutex<Option<oneshot::Sender<()>>>,
    release: Notify,
    sent: Mutex<Vec<String>>,
}

impl EmailSender for HoldFirst {
    fn send<'a>(&'a self, email: &'a OutgoingEmail) -> SendFuture<'a> {
        Box::pin(async move {
            let first = self.entered.lock().unwrap().take();
            if let Some(entered) = first {
                entered.send(()).unwrap();
                self.release.notified().await;
            }
            self.sent.lock().unwrap().push(email.email.to.clone());
            Ok(())
        })
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn two_workers_never_send_the_same_email(pool: PgPool) {
    let clock = std::sync::Arc::new(clock());
    let mut conn = pool.acquire().await.unwrap();
    for to in ["a@example.org", "b@example.org", "c@example.org"] {
        enqueue(&mut conn, clock.as_ref(), &email(to), None)
            .await
            .unwrap();
    }
    let (entered_tx, entered_rx) = oneshot::channel();
    let holder = std::sync::Arc::new(HoldFirst {
        entered: Mutex::new(Some(entered_tx)),
        release: Notify::new(),
        sent: Mutex::new(Vec::new()),
    });

    let worker_a = {
        let (pool, holder, clock) = (pool.clone(), holder.clone(), clock.clone());
        tokio::spawn(async move {
            let mut conn = pool.acquire().await.unwrap();
            send_pending(&mut conn, holder.as_ref(), clock.as_ref(), 10)
                .await
                .unwrap()
        })
    };
    entered_rx.await.unwrap();

    let other = FakeEmailSender::new();
    assert_eq!(send(&pool, &other, &clock, 10).await, report(2, 0, 0));
    assert_eq!(recipients(&other), ["b@example.org", "c@example.org"]);

    holder.release.notify_one();
    assert_eq!(worker_a.await.unwrap(), report(1, 0, 0));
    assert_eq!(*holder.sent.lock().unwrap(), ["a@example.org"]);
    assert!(
        states(&pool)
            .await
            .iter()
            .all(|s| s.status == "sent" && s.attempts == 1)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn reply_to_survives_the_outbox(pool: PgPool) {
    let clock = clock();
    let mut conn = pool.acquire().await.unwrap();
    let mut contact = email("bakker@example.org");
    contact.kind = EmailKind::ContactMessage;
    contact.reply_to = Some("sanne@example.org".to_owned());
    enqueue(&mut conn, &clock, &contact, None).await.unwrap();

    let sender = FakeEmailSender::new();
    assert_eq!(send(&pool, &sender, &clock, 10).await, report(1, 0, 0));
    assert_eq!(sender.sent()[0].email, contact);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn company_details_come_from_the_settings_row(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let seeded = notify::Company::load(&mut conn).await.unwrap();
    assert_eq!(seeded.name, "Brood voor Weldadigheid");
    assert_eq!((seeded.address.as_str(), seeded.kvk.as_str()), ("", ""));

    sqlx::query("UPDATE company SET address = 'Dorpsstraat 1, 1234 AB Ergens', kvk = '12345678'")
        .execute(&pool)
        .await
        .unwrap();
    let filled = notify::Company::load(&mut conn).await.unwrap();
    assert_eq!(filled.address, "Dorpsstraat 1, 1234 AB Ergens");
    assert_eq!(filled.kvk, "12345678");
}
