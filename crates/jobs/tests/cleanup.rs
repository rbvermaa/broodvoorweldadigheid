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
            votes: 0,
            old_votes: 0,
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

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_cleanup_deletes_unconfirmed_votes_once_their_link_ran_out(pool: PgPool) {
    let clock = FakeClock::new("2026-11-24T10:00:00Z".parse().unwrap());
    sqlx::query(
        "WITH p AS (
            INSERT INTO poll (title, closes_at, created_at) VALUES ('Stemming', $1::timestamptz, now())
            RETURNING id
        ), o AS (
            INSERT INTO poll_option (poll_id, name, position) SELECT id, 'Spelt', 0 FROM p
            RETURNING poll_id, id
        )
        INSERT INTO vote (poll_id, poll_option_id, email, confirmed_at, confirm_token_hash,
            confirm_expires_at, created_at)
        SELECT o.poll_id, o.id, e.email, e.confirmed, e.hash, e.expires, now()
        FROM o, (VALUES
            ('verlopen@example.nl', NULL::timestamptz, sha256('a'), $1::timestamptz - interval '1 second'),
            ('wacht@example.nl', NULL, sha256('b'), $1::timestamptz + interval '1 hour'),
            ('telt@example.nl', $1::timestamptz, NULL, NULL)
        ) e(email, confirmed, hash, expires)",
    )
    .bind(clock.now().to_string())
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        CleanupReport {
            votes: 1,
            ..CleanupReport::default()
        }
    );
    let left: Vec<String> = sqlx::query_scalar("SELECT email FROM vote ORDER BY email")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, ["telt@example.nl", "wacht@example.nl"]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_cleanup_blanks_voters_30_days_after_their_poll_closed(pool: PgPool) {
    let clock = FakeClock::new("2026-11-24T10:00:00Z".parse().unwrap());
    sqlx::query(
        "WITH p AS (
            INSERT INTO poll (title, closes_at, created_at)
            SELECT t, $1::timestamptz - d, now()
            FROM (VALUES ('31', interval '31 days'), ('29', interval '29 days'),
                ('open', interval '-1 day')) x(t, d)
            RETURNING id, title
        ), o AS (
            INSERT INTO poll_option (poll_id, name, position) SELECT id, title, 0 FROM p
            RETURNING poll_id, id, name
        )
        INSERT INTO vote (poll_id, poll_option_id, email, wish, confirmed_at, created_at)
        SELECT o.poll_id, o.id, o.name || '-' || n || '@example.nl', 'wens', now(), now()
        FROM o, generate_series(1, 2) n",
    )
    .bind(clock.now().to_string())
    .execute(&pool)
    .await
    .unwrap();
    let counts = "SELECT string_agg(o.name || ':' || (SELECT count(*) FROM vote v
            WHERE v.poll_option_id = o.id), ',' ORDER BY o.name) FROM poll_option o";
    let before: String = sqlx::query_scalar(counts).fetch_one(&pool).await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        CleanupReport {
            old_votes: 2,
            ..CleanupReport::default()
        }
    );
    let kept: Vec<String> = sqlx::query_scalar(
        "SELECT email FROM vote WHERE email IS NOT NULL AND wish IS NOT NULL ORDER BY email",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        kept,
        [
            "29-1@example.nl",
            "29-2@example.nl",
            "open-1@example.nl",
            "open-2@example.nl"
        ]
    );
    let after: String = sqlx::query_scalar(counts).fetch_one(&pool).await.unwrap();
    assert_eq!(after, before);
    assert_eq!(after, "29:2,31:2,open:2");

    assert_eq!(
        tick(&mut conn, &clock).await.unwrap(),
        CleanupReport::default(),
        "a second run changes nothing"
    );
}
