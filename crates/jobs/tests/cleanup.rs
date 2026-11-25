//! F12: the cleanup of expired customer login links and logins, and of account-deleted
//! emails once they went out (ADR 0031).
#![forbid(unsafe_code)]

use domain::Clock;
use jiff::Timestamp;
use jobs::CleanupReport;
use jobs::cleanup::tick;
use sqlx::PgPool;
use testkit::FakeClock;

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_cleanup_deletes_expired_logins_and_sent_account_deleted_emails(pool: PgPool) {
    let now: Timestamp = "2026-11-24T10:00:00Z".parse().unwrap();
    let clock = FakeClock::new(now);
    sqlx::query(
        "WITH c AS (INSERT INTO customer (email, name, created_at)
            VALUES ('klant@example.nl', 'Klant', now()) RETURNING id),
        links AS (INSERT INTO customer_login_link (token_hash, customer_id, created_at, expires_at)
            SELECT sha256(v::bytea), c.id, $1::timestamptz - interval '1 hour', $1::timestamptz + e
            FROM c, (VALUES ('a', interval '-1 minute'), ('b', interval '1 minute')) t(v, e))
        INSERT INTO customer_login (customer_id, created_at, last_seen_at)
        SELECT c.id, $1::timestamptz - d, $1::timestamptz - d
        FROM c, (VALUES (interval '8 days'), (interval '1 day')) t(d)",
    )
    .bind(now.to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO email_outbox (kind, to_address, subject, text_body, html_body, status,
            next_attempt_at, created_at, sent_at)
        VALUES
            ('account_deleted', 'weg@example.nl', 'S', 't', 'h', 'sent', now(), now(), now()),
            ('account_deleted', 'ook@example.nl', 'S', 't', 'h', 'pending', now(), now(), NULL),
            ('login_link', 'klant@example.nl', 'S', 't', 'h', 'sent', now(), now(), now())",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        CleanupReport {
            login_links: 1,
            logins: 1,
            emails: 1,
            old_emails: 0,
        }
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM customer_login_link").await,
        1
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM customer_login").await, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM email_outbox").await, 2);
    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        CleanupReport::default()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_cleanup_deletes_emails_90_days_after_they_were_sent_or_failed(pool: PgPool) {
    let clock = FakeClock::new("2026-11-24T10:00:00Z".parse().unwrap());
    sqlx::query(
        "INSERT INTO email_outbox (kind, to_address, subject, text_body, html_body, status,
            next_attempt_at, created_at, sent_at, failed_at)
        SELECT 'order_confirmation', status || '-' || days || '@example.nl', 'S', 't', 'h',
            status, at, at,
            CASE WHEN status = 'sent' THEN at END, CASE WHEN status = 'failed' THEN at END
        FROM (VALUES ('sent', 89), ('sent', 91), ('failed', 89), ('failed', 91),
                ('pending', 91)) t(status, days),
            LATERAL (SELECT $1::timestamptz - days * interval '1 day' AS at) a",
    )
    .bind(clock.now().to_string())
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        CleanupReport {
            old_emails: 2,
            ..CleanupReport::default()
        }
    );
    let left: Vec<String> =
        sqlx::query_scalar("SELECT to_address FROM email_outbox ORDER BY to_address")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        left,
        [
            "failed-89@example.nl",
            "pending-91@example.nl",
            "sent-89@example.nl"
        ]
    );
}
