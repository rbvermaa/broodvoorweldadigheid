//! The email outbox (ADR 0014): business code queues an email in its own transaction, and
//! [`send_pending`] hands due emails to the relay, retrying with backoff.

use domain::Clock;
use jiff::{SignedDuration, Timestamp};
use sqlx::{Acquire, PgConnection};

use crate::email::{Email, EmailKind, EmailSender, OutgoingEmail, SendError};

/// Waits before attempts 2, 3, …; after the last one an email is marked failed.
pub const BACKOFF: [SignedDuration; 7] = [
    SignedDuration::from_mins(1),
    SignedDuration::from_mins(5),
    SignedDuration::from_mins(15),
    SignedDuration::from_hours(1),
    SignedDuration::from_hours(3),
    SignedDuration::from_hours(6),
    SignedDuration::from_hours(12),
];
pub const MAX_ATTEMPTS: i32 = 8;

const MAX_ERROR_LEN: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutboxId(i64);

impl OutboxId {
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("corrupt outbox row {id}: {what}")]
    Corrupt { id: i64, what: String },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SendReport {
    pub sent: u32,
    pub retrying: u32,
    pub failed: u32,
}

/// Queues an email. Call it inside the transaction of the change the email reports, so
/// both commit or neither does. With a `dedupe_key` that is already queued, nothing is
/// added and the result is `None`.
///
/// # Errors
/// [`OutboxError::Db`].
pub async fn enqueue(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    email: &Email,
    dedupe_key: Option<&str>,
) -> Result<Option<OutboxId>, OutboxError> {
    let now = db::time::to_micros(clock.now());
    let id = sqlx::query_scalar!(
        r#"INSERT INTO email_outbox (dedupe_key, kind, to_address, subject, text_body, html_body,
            list_unsubscribe, next_attempt_at, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7,
            timestamptz 'epoch' + $8::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $8::bigint * interval '1 microsecond')
        ON CONFLICT (dedupe_key) DO NOTHING
        RETURNING id"#,
        dedupe_key,
        email.kind.as_str(),
        email.to,
        email.subject,
        email.text,
        email.html,
        email.list_unsubscribe,
        now,
    )
    .fetch_optional(conn)
    .await?;
    Ok(id.map(OutboxId))
}

struct Due {
    id: i64,
    message_id: String,
    kind: String,
    to_address: String,
    subject: String,
    text_body: String,
    html_body: String,
    list_unsubscribe: Option<String>,
    attempts: i32,
}

enum Outcome {
    Sent,
    Retry(Timestamp, String),
    Failed(String),
}

/// Sends up to `batch` due emails, oldest first. Each email is locked with
/// `FOR UPDATE SKIP LOCKED` in its own transaction while it is sent, so several workers
/// never send the same email, and a crash re-sends at most the one email in flight, with
/// the same Message-ID.
///
/// # Errors
/// [`OutboxError`] when the database fails; send failures are recorded on the row instead.
pub async fn send_pending(
    conn: &mut PgConnection,
    sender: &dyn EmailSender,
    clock: &dyn Clock,
    batch: u32,
) -> Result<SendReport, OutboxError> {
    let mut report = SendReport::default();
    for _ in 0..batch {
        let now = clock.now();
        let mut tx = conn.begin().await?;
        let Some(due) = sqlx::query_as!(
            Due,
            r#"SELECT id, message_id::text AS "message_id!", kind, to_address, subject,
                text_body, html_body, list_unsubscribe, attempts
            FROM email_outbox
            WHERE status = 'pending'
                AND next_attempt_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
            ORDER BY next_attempt_at, id
            LIMIT 1
            FOR UPDATE SKIP LOCKED"#,
            db::time::to_micros(now),
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            break;
        };
        let id = due.id;
        let attempts = due.attempts.saturating_add(1);
        let outcome = match outgoing(due, now) {
            Ok(email) => match sender.send(&email).await {
                Ok(()) => Outcome::Sent,
                Err(SendError::Transient(error)) => match retry_at(now, attempts) {
                    Some(at) => Outcome::Retry(at, error),
                    None => Outcome::Failed(error),
                },
                Err(SendError::Permanent(error)) => Outcome::Failed(error),
            },
            Err(error) => Outcome::Failed(error.to_string()),
        };
        match outcome {
            Outcome::Sent => {
                sqlx::query!(
                    r#"UPDATE email_outbox SET status = 'sent', attempts = $2, last_error = NULL,
                        sent_at = timestamptz 'epoch' + $3::bigint * interval '1 microsecond'
                    WHERE id = $1"#,
                    id,
                    attempts,
                    db::time::to_micros(now),
                )
                .execute(&mut *tx)
                .await?;
                report.sent += 1;
            }
            Outcome::Retry(at, error) => {
                tracing::warn!(outbox_id = id, attempts, "email not sent, will retry");
                sqlx::query!(
                    r#"UPDATE email_outbox SET attempts = $2, last_error = $3,
                        next_attempt_at = timestamptz 'epoch' + $4::bigint * interval '1 microsecond'
                    WHERE id = $1"#,
                    id,
                    attempts,
                    truncate(&error),
                    db::time::to_micros(at),
                )
                .execute(&mut *tx)
                .await?;
                report.retrying += 1;
            }
            Outcome::Failed(error) => {
                tracing::error!(outbox_id = id, attempts, "email failed for good");
                sqlx::query!(
                    r#"UPDATE email_outbox SET status = 'failed', attempts = $2, last_error = $3
                    WHERE id = $1"#,
                    id,
                    attempts,
                    truncate(&error),
                )
                .execute(&mut *tx)
                .await?;
                report.failed += 1;
            }
        }
        tx.commit().await?;
    }
    Ok(report)
}

/// When to try again after `attempts` failed tries, or `None` when that was the last.
#[must_use]
pub fn retry_at(now: Timestamp, attempts: i32) -> Option<Timestamp> {
    if attempts >= MAX_ATTEMPTS {
        return None;
    }
    let index = usize::try_from(attempts.saturating_sub(1)).ok()?;
    let wait = BACKOFF.get(index).copied()?;
    now.checked_add(wait).ok()
}

fn outgoing(due: Due, now: Timestamp) -> Result<OutgoingEmail, OutboxError> {
    let kind = EmailKind::parse(&due.kind).ok_or_else(|| OutboxError::Corrupt {
        id: due.id,
        what: format!("unknown kind {}", due.kind),
    })?;
    Ok(OutgoingEmail {
        id: due.message_id,
        date: now,
        email: Email {
            kind,
            to: due.to_address,
            subject: due.subject,
            text: due.text_body,
            html: due.html_body,
            list_unsubscribe: due.list_unsubscribe,
        },
    })
}

fn truncate(error: &str) -> String {
    error.chars().take(MAX_ERROR_LEN).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_stops_after_the_last_attempt() {
        let now: Timestamp = "2026-12-05T10:00:00Z".parse().unwrap();
        let waits: Vec<i64> = (1..MAX_ATTEMPTS)
            .map(|attempts| {
                retry_at(now, attempts)
                    .expect("retry")
                    .duration_since(now)
                    .as_mins()
            })
            .collect();
        assert_eq!(waits, [1, 5, 15, 60, 180, 360, 720]);
        assert_eq!(retry_at(now, MAX_ATTEMPTS), None);
        assert_eq!(retry_at(now, 0), None);
        assert_eq!(usize::try_from(MAX_ATTEMPTS - 1).ok(), Some(BACKOFF.len()));
    }

    #[test]
    fn long_errors_are_cut() {
        assert_eq!(truncate(&"é".repeat(5000)).chars().count(), MAX_ERROR_LEN);
        assert_eq!(truncate("kort"), "kort");
    }
}
