//! Drop-alert subscribers (F9a, ADR 0021): sign-up with double opt-in, and unsubscribe
//! links that work without a login. Tokens are random and only their SHA-256 hash is
//! stored.

use domain::{BakeDayId, Clock};
use jiff::{SignedDuration, Timestamp};
use sqlx::{Acquire, PgConnection};

use crate::compose::{ComposeError, Composer, SubscriptionConfirmation};
use crate::outbox::{self, OutboxError};
use crate::token::{new_token, token_hash};

/// How long a confirmation link works.
pub const CONFIRM_VALID: SignedDuration = SignedDuration::from_hours(7 * 24);
/// A new confirmation email for the same address goes out at most this often.
pub const RESEND_AFTER: SignedDuration = SignedDuration::from_mins(5);
const MAX_EMAIL_LEN: usize = 254;

#[derive(Debug, thiserror::Error)]
pub enum SubscriberError {
    #[error("not an email address")]
    Address,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Compose(#[from] ComposeError),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
}

/// What a sign-up did. The visitor sees the same answer for each, so nobody learns
/// whether an address is on the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscribed {
    ConfirmationQueued,
    AlreadyConfirmed,
    /// A confirmation went out less than [`RESEND_AFTER`] ago.
    RecentlySent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscriber {
    pub id: i64,
    pub email: String,
}

/// The public links in subscriber emails, built from the site's origin such as
/// `https://broodvoorweldadigheid.nl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Links {
    origin: String,
}

impl Links {
    #[must_use]
    pub fn new(origin: &str) -> Self {
        Self {
            origin: origin.trim_end_matches('/').to_owned(),
        }
    }

    #[must_use]
    pub fn confirm(&self, token: &str) -> String {
        format!("{}/meldingen/bevestig/{token}", self.origin)
    }

    #[must_use]
    pub fn unsubscribe(&self, token: &str) -> String {
        format!("{}/meldingen/afmelden/{token}", self.origin)
    }

    #[must_use]
    pub fn vote_confirm(&self, token: &str) -> String {
        format!("{}/stemmen/bevestig/{token}", self.origin)
    }

    #[must_use]
    pub fn drop_page(&self, bake_day: BakeDayId) -> String {
        format!("{}/drops/{}", self.origin, bake_day.get())
    }
}

/// The trimmed address, or [`SubscriberError::Address`].
///
/// # Errors
/// [`SubscriberError::Address`] when it is not an email address.
pub fn parse_address(raw: &str) -> Result<String, SubscriberError> {
    let email = raw.trim();
    if email.len() > MAX_EMAIL_LEN || !crate::is_address(email) {
        return Err(SubscriberError::Address);
    }
    Ok(email.to_owned())
}

fn micros(at: Timestamp) -> i64 {
    db::time::to_micros(at)
}

/// Signs `raw` up and queues a confirmation email, unless the address is already
/// confirmed or got one less than [`RESEND_AFTER`] ago. An address that unsubscribed
/// gets a new confirmation and stays off the list until it confirms.
///
/// # Errors
/// [`SubscriberError::Address`] for a bad address; otherwise database, compose or outbox
/// errors.
pub async fn subscribe(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    composer: &Composer<'_>,
    links: &Links,
    raw: &str,
) -> Result<Subscribed, SubscriberError> {
    let email = parse_address(raw)?;
    let now = clock.now();
    let mut tx = conn.begin().await?;
    sqlx::query!(
        "INSERT INTO subscriber (email, created_at)
        VALUES ($1, timestamptz 'epoch' + $2::bigint * interval '1 microsecond')
        ON CONFLICT ((lower(email))) DO NOTHING",
        email,
        micros(now),
    )
    .execute(&mut *tx)
    .await?;
    let row = sqlx::query!(
        r#"SELECT id, email,
            confirmed_at IS NOT NULL AND unsubscribed_at IS NULL AS "active!",
            (extract(epoch FROM confirm_sent_at) * 1000000)::bigint AS sent_at_us
        FROM subscriber WHERE lower(email) = lower($1) FOR UPDATE"#,
        email,
    )
    .fetch_one(&mut *tx)
    .await?;
    if row.active {
        return Ok(Subscribed::AlreadyConfirmed);
    }
    let recent = now.checked_sub(RESEND_AFTER).map_or(0, micros);
    if row.sent_at_us.is_some_and(|sent| sent > recent) {
        return Ok(Subscribed::RecentlySent);
    }
    let (token, hash) = new_token();
    let expires = now.checked_add(CONFIRM_VALID).unwrap_or(now);
    sqlx::query!(
        "UPDATE subscriber SET confirm_token_hash = $2,
            confirm_expires_at = timestamptz 'epoch' + $3::bigint * interval '1 microsecond',
            confirm_sent_at = timestamptz 'epoch' + $4::bigint * interval '1 microsecond'
        WHERE id = $1",
        row.id,
        hash,
        micros(expires),
        micros(now),
    )
    .execute(&mut *tx)
    .await?;
    let email = composer.subscription_confirmation(&SubscriptionConfirmation {
        to: row.email,
        confirm_url: links.confirm(&token),
    })?;
    outbox::enqueue(&mut tx, clock, &email, None).await?;
    tx.commit().await?;
    Ok(Subscribed::ConfirmationQueued)
}

/// Confirms the address behind an unexpired confirmation link; `false` for an unknown or
/// expired one. Opening the link again while it is valid changes nothing.
///
/// # Errors
/// [`SubscriberError::Db`].
pub async fn confirm(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    token: &str,
) -> Result<bool, SubscriberError> {
    let Some(hash) = token_hash(token) else {
        return Ok(false);
    };
    let confirmed = sqlx::query_scalar!(
        "UPDATE subscriber SET
            confirmed_at = CASE WHEN confirmed_at IS NULL OR unsubscribed_at IS NOT NULL
                THEN timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
                ELSE confirmed_at END,
            unsubscribed_at = NULL
        WHERE confirm_token_hash = $1
            AND confirm_expires_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
        RETURNING id",
        hash,
        micros(clock.now()),
    )
    .fetch_optional(conn)
    .await?;
    Ok(confirmed.is_some())
}

/// Whether `token` is a known unsubscribe link, for the page that asks to confirm.
///
/// # Errors
/// [`SubscriberError::Db`].
pub async fn unsubscribe_link_known(
    conn: &mut PgConnection,
    token: &str,
) -> Result<bool, SubscriberError> {
    let Some(hash) = token_hash(token) else {
        return Ok(false);
    };
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM subscriber_unsubscribe_token WHERE token_hash = $1)
            AS "known!""#,
        hash,
    )
    .fetch_one(conn)
    .await?)
}

/// Takes the subscriber behind an unsubscribe link off the list, voids an open
/// confirmation link (signing up again sends a new one at once) and withdraws drop alerts
/// to them still waiting in the outbox; `false` for an unknown link. Safe to repeat.
///
/// # Errors
/// [`SubscriberError::Db`].
pub async fn unsubscribe(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    token: &str,
) -> Result<bool, SubscriberError> {
    let Some(hash) = token_hash(token) else {
        return Ok(false);
    };
    let mut tx = conn.begin().await?;
    let done = sqlx::query_scalar!(
        "UPDATE subscriber s SET
            unsubscribed_at = coalesce(s.unsubscribed_at,
                timestamptz 'epoch' + $2::bigint * interval '1 microsecond'),
            confirm_token_hash = NULL,
            confirm_expires_at = NULL,
            confirm_sent_at = NULL
        FROM subscriber_unsubscribe_token t
        WHERE t.token_hash = $1 AND t.subscriber_id = s.id
        RETURNING s.id",
        hash,
        micros(clock.now()),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = done else {
        return Ok(false);
    };
    // A separate statement, so it sees alerts that a tick holding this row through
    // [`active`] committed while the UPDATE waited.
    sqlx::query!(
        "DELETE FROM email_outbox o USING subscriber s
        WHERE s.id = $1 AND o.kind = 'drop_alert' AND o.status = 'pending'
            AND lower(o.to_address) = lower(s.email)",
        id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Confirmed subscribers who have not unsubscribed: the only ones drop alerts go to.
/// The rows stay locked `FOR SHARE` until the caller's transaction ends, so an
/// unsubscribe waits for alerts being queued and can then withdraw them.
///
/// # Errors
/// [`SubscriberError::Db`].
pub async fn active(conn: &mut PgConnection) -> Result<Vec<Subscriber>, SubscriberError> {
    Ok(sqlx::query_as!(
        Subscriber,
        "SELECT id, email FROM subscriber
        WHERE confirmed_at IS NOT NULL AND unsubscribed_at IS NULL
        ORDER BY id FOR SHARE",
    )
    .fetch_all(conn)
    .await?)
}

/// A new unsubscribe link for one email to `subscriber`. Each alert gets its own, so
/// no token is ever stored in clear.
///
/// # Errors
/// [`SubscriberError::Db`].
pub async fn unsubscribe_link(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    links: &Links,
    subscriber: i64,
) -> Result<String, SubscriberError> {
    let (token, hash) = new_token();
    sqlx::query!(
        "INSERT INTO subscriber_unsubscribe_token (token_hash, subscriber_id, created_at)
        VALUES ($1, $2, timestamptz 'epoch' + $3::bigint * interval '1 microsecond')",
        hash,
        subscriber,
        micros(clock.now()),
    )
    .execute(conn)
    .await?;
    Ok(links.unsubscribe(&token))
}

/// The drop-alert subscription of one address, for a customer's data export (F12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub created_at: Timestamp,
    pub confirmed_at: Option<Timestamp>,
    pub unsubscribed_at: Option<Timestamp>,
}

impl Subscription {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.confirmed_at.is_some() && self.unsubscribed_at.is_none()
    }
}

/// The subscription of `email`, however it is capitalised.
///
/// # Errors
/// [`SubscriberError::Db`].
pub async fn of_address(
    conn: &mut PgConnection,
    email: &str,
) -> Result<Option<Subscription>, SubscriberError> {
    let Some(row) = sqlx::query!(
        r#"SELECT (extract(epoch FROM created_at) * 1000000)::bigint AS "created_us!",
            (extract(epoch FROM confirmed_at) * 1000000)::bigint AS confirmed_us,
            (extract(epoch FROM unsubscribed_at) * 1000000)::bigint AS unsubscribed_us
        FROM subscriber WHERE lower(email) = lower($1)"#,
        email,
    )
    .fetch_optional(conn)
    .await?
    else {
        return Ok(None);
    };
    let at =
        |us: i64| db::time::from_micros(us).map_err(|e| SubscriberError::Corrupt(e.to_string()));
    Ok(Some(Subscription {
        created_at: at(row.created_us)?,
        confirmed_at: row.confirmed_us.map(at).transpose()?,
        unsubscribed_at: row.unsubscribed_us.map(at).transpose()?,
    }))
}

/// Puts `email` on the list without a confirmation email: for a customer who proved the
/// address by signing in with a link sent to it. An open confirmation link is voided.
///
/// # Errors
/// [`SubscriberError::Address`] for a bad address, or [`SubscriberError::Db`].
pub async fn subscribe_verified(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    raw: &str,
) -> Result<(), SubscriberError> {
    let email = parse_address(raw)?;
    let now = micros(clock.now());
    sqlx::query!(
        "INSERT INTO subscriber (email, confirmed_at, created_at)
        VALUES ($1, timestamptz 'epoch' + $2::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $2::bigint * interval '1 microsecond')
        ON CONFLICT ((lower(email))) DO UPDATE SET
            confirmed_at = CASE
                WHEN subscriber.confirmed_at IS NULL OR subscriber.unsubscribed_at IS NOT NULL
                THEN EXCLUDED.confirmed_at ELSE subscriber.confirmed_at END,
            unsubscribed_at = NULL,
            confirm_token_hash = NULL,
            confirm_expires_at = NULL,
            confirm_sent_at = NULL",
        email,
        now,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Takes `email` off the list like an unsubscribe link does, if it is on it: voids an
/// open confirmation link and withdraws drop alerts to it still waiting in the outbox.
///
/// # Errors
/// [`SubscriberError::Db`].
pub async fn withdraw(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    email: &str,
) -> Result<(), SubscriberError> {
    let mut tx = conn.begin().await?;
    sqlx::query!(
        "UPDATE subscriber SET
            unsubscribed_at = coalesce(unsubscribed_at,
                timestamptz 'epoch' + $2::bigint * interval '1 microsecond'),
            confirm_token_hash = NULL,
            confirm_expires_at = NULL,
            confirm_sent_at = NULL
        WHERE lower(email) = lower($1)",
        email,
        micros(clock.now()),
    )
    .execute(&mut *tx)
    .await?;
    withdraw_alerts(&mut tx, email).await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes the subscriber with `email`, their unsubscribe tokens and the drop alerts to
/// them still waiting in the outbox (F12).
///
/// # Errors
/// [`SubscriberError::Db`].
pub async fn forget(conn: &mut PgConnection, email: &str) -> Result<(), SubscriberError> {
    let mut tx = conn.begin().await?;
    sqlx::query!(
        "DELETE FROM subscriber WHERE lower(email) = lower($1)",
        email
    )
    .execute(&mut *tx)
    .await?;
    withdraw_alerts(&mut tx, email).await?;
    tx.commit().await?;
    Ok(())
}

/// A separate statement after the change to the subscriber row, so it sees alerts that a
/// tick holding that row through [`active`] committed while the change waited.
async fn withdraw_alerts(conn: &mut PgConnection, email: &str) -> Result<(), SubscriberError> {
    sqlx::query!(
        "DELETE FROM email_outbox
        WHERE kind = 'drop_alert' AND status = 'pending' AND lower(to_address) = lower($1)",
        email,
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_trimmed_and_checked() {
        assert_eq!(
            parse_address("  Iemand@Example.NL ").unwrap(),
            "Iemand@Example.NL"
        );
        for bad in ["", "geen-adres", "a@", "@b.nl", "a b@c.nl"] {
            assert!(parse_address(bad).is_err(), "{bad}");
        }
        let long = format!("{}@example.nl", "a".repeat(250));
        assert!(parse_address(&long).is_err());
    }

    #[test]
    fn links_use_the_origin_without_a_trailing_slash() {
        let links = Links::new("https://broodvoorweldadigheid.nl/");
        assert_eq!(
            links.confirm("t"),
            "https://broodvoorweldadigheid.nl/meldingen/bevestig/t"
        );
        assert_eq!(
            links.unsubscribe("t"),
            "https://broodvoorweldadigheid.nl/meldingen/afmelden/t"
        );
        assert_eq!(
            links.drop_page(BakeDayId::new(7)),
            "https://broodvoorweldadigheid.nl/drops/7"
        );
        assert_eq!(
            links.vote_confirm("t"),
            "https://broodvoorweldadigheid.nl/stemmen/bevestig/t"
        );
    }
}
