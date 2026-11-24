//! F9a: double opt-in and unsubscribe links.
#![forbid(unsafe_code)]

use i18n::Translator;
use jiff::{SignedDuration, Timestamp};
use notify::subscribers::{self, CONFIRM_VALID, Links, RESEND_AFTER, Subscribed, SubscriberError};
use notify::{Company, Composer};
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

fn company() -> Company {
    Company {
        name: "Brood voor Weldadigheid".to_owned(),
        address: "Majoor van Swietenlaan 15, 8382 CE Frederiksoord".to_owned(),
        kvk: "12345678".to_owned(),
    }
}

fn links() -> Links {
    Links::new("https://broodvoorweldadigheid.nl")
}

async fn subscribe(conn: &mut PgConnection, clock: &FakeClock, email: &str) -> Subscribed {
    let i18n = Translator::dutch().unwrap();
    let company = company();
    subscribers::subscribe(
        conn,
        clock,
        &Composer::new(&i18n, &company),
        &links(),
        email,
    )
    .await
    .unwrap()
}

async fn confirmations(conn: &mut PgConnection) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT to_address, text_body FROM email_outbox
        WHERE kind = 'subscription_confirmation' ORDER BY id",
    )
    .fetch_all(conn)
    .await
    .unwrap()
}

fn token(text: &str) -> String {
    let path = "https://broodvoorweldadigheid.nl/meldingen/bevestig/";
    let start = text.find(path).expect("confirm link") + path.len();
    text[start..start + 43].to_owned()
}

async fn last_token(conn: &mut PgConnection) -> String {
    token(&confirmations(conn).await.last().expect("an email").1)
}

async fn active(conn: &mut PgConnection) -> Vec<String> {
    subscribers::active(conn)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.email)
        .collect()
}

async fn unsubscribe_token(conn: &mut PgConnection, clock: &FakeClock) -> String {
    let id: i64 = sqlx::query_scalar("SELECT id FROM subscriber")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    let url = subscribers::unsubscribe_link(conn, clock, &links(), id)
        .await
        .unwrap();
    url.rsplit('/').next().unwrap().to_owned()
}

fn now() -> FakeClock {
    FakeClock::new("2026-11-24T10:00:00Z".parse::<Timestamp>().unwrap())
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sign_up_queues_a_confirmation_and_the_link_confirms(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    assert_eq!(
        subscribe(&mut conn, &clock, " Iemand@Example.nl ").await,
        Subscribed::ConfirmationQueued
    );
    let sent = confirmations(&mut conn).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "Iemand@Example.nl");
    assert!(active(&mut conn).await.is_empty(), "not before confirming");

    let token = last_token(&mut conn).await;
    let stored: Vec<u8> = sqlx::query_scalar("SELECT confirm_token_hash FROM subscriber")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_ne!(stored, token.as_bytes(), "only the hash is stored");

    assert!(
        subscribers::confirm(&mut conn, &clock, &token)
            .await
            .unwrap()
    );
    assert!(
        subscribers::confirm(&mut conn, &clock, &token)
            .await
            .unwrap()
    );
    assert_eq!(active(&mut conn).await, ["Iemand@Example.nl"]);
    assert_eq!(
        subscribe(&mut conn, &clock, "iemand@example.NL").await,
        Subscribed::AlreadyConfirmed
    );
    assert_eq!(confirmations(&mut conn).await.len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn confirmations_are_throttled_and_links_expire(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    subscribe(&mut conn, &clock, "iemand@example.nl").await;
    let first = last_token(&mut conn).await;
    assert_eq!(
        subscribe(&mut conn, &clock, "iemand@example.nl").await,
        Subscribed::RecentlySent
    );
    clock.advance(RESEND_AFTER - SignedDuration::from_secs(1));
    assert_eq!(
        subscribe(&mut conn, &clock, "iemand@example.nl").await,
        Subscribed::RecentlySent
    );
    clock.advance(SignedDuration::from_secs(1));
    assert_eq!(
        subscribe(&mut conn, &clock, "iemand@example.nl").await,
        Subscribed::ConfirmationQueued
    );
    assert_eq!(confirmations(&mut conn).await.len(), 2);
    let second = last_token(&mut conn).await;
    assert!(
        !subscribers::confirm(&mut conn, &clock, &first)
            .await
            .unwrap(),
        "a new link replaces the old one"
    );

    clock.advance(CONFIRM_VALID);
    assert!(
        !subscribers::confirm(&mut conn, &clock, &second)
            .await
            .unwrap()
    );
    assert!(active(&mut conn).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn unsubscribing_is_idempotent_and_signing_up_again_needs_a_new_confirmation(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = now();
    subscribe(&mut conn, &clock, "iemand@example.nl").await;
    let confirm = last_token(&mut conn).await;
    subscribers::confirm(&mut conn, &clock, &confirm)
        .await
        .unwrap();
    let unsubscribe = unsubscribe_token(&mut conn, &clock).await;

    assert!(
        subscribers::unsubscribe_link_known(&mut conn, &unsubscribe)
            .await
            .unwrap()
    );
    assert!(
        subscribers::unsubscribe(&mut conn, &clock, &unsubscribe)
            .await
            .unwrap()
    );
    assert!(
        subscribers::unsubscribe(&mut conn, &clock, &unsubscribe)
            .await
            .unwrap()
    );
    assert!(active(&mut conn).await.is_empty());
    assert!(
        !subscribers::confirm(&mut conn, &clock, &confirm)
            .await
            .unwrap(),
        "an old confirmation link cannot undo the unsubscribe"
    );

    clock.advance(RESEND_AFTER * 2);
    assert_eq!(
        subscribe(&mut conn, &clock, "iemand@example.nl").await,
        Subscribed::ConfirmationQueued
    );
    assert!(
        active(&mut conn).await.is_empty(),
        "still off until confirmed"
    );
    let again = last_token(&mut conn).await;
    assert!(
        subscribers::confirm(&mut conn, &clock, &again)
            .await
            .unwrap()
    );
    assert_eq!(active(&mut conn).await, ["iemand@example.nl"]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn signing_up_right_after_unsubscribing_sends_a_working_link(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = now();
    subscribe(&mut conn, &clock, "iemand@example.nl").await;
    let unsubscribe = unsubscribe_token(&mut conn, &clock).await;
    assert!(
        subscribers::unsubscribe(&mut conn, &clock, &unsubscribe)
            .await
            .unwrap()
    );

    assert_eq!(
        subscribe(&mut conn, &clock, "iemand@example.nl").await,
        Subscribed::ConfirmationQueued
    );
    let again = last_token(&mut conn).await;
    assert!(
        subscribers::confirm(&mut conn, &clock, &again)
            .await
            .unwrap()
    );
    assert_eq!(active(&mut conn).await, ["iemand@example.nl"]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn unknown_tokens_and_bad_addresses_change_nothing(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let unknown = "A".repeat(43);
    for token in [unknown.as_str(), "kort", ""] {
        assert!(
            !subscribers::confirm(&mut conn, &clock, token)
                .await
                .unwrap()
        );
        assert!(
            !subscribers::unsubscribe(&mut conn, &clock, token)
                .await
                .unwrap()
        );
        assert!(
            !subscribers::unsubscribe_link_known(&mut conn, token)
                .await
                .unwrap()
        );
    }
    let i18n = Translator::dutch().unwrap();
    let company = company();
    let result = subscribers::subscribe(
        &mut conn,
        &clock,
        &Composer::new(&i18n, &company),
        &links(),
        "geen adres",
    )
    .await;
    assert!(matches!(result, Err(SubscriberError::Address)));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM subscriber")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}
