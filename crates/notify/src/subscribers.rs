//! Drop-alert subscribers (F9a, ADR 0021): sign-up with double opt-in, and unsubscribe
//! links that work without a login. Tokens are random and only their SHA-256 hash is
//! stored.

use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use domain::{BakeDayId, Clock};
use jiff::{SignedDuration, Timestamp};
use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection};

use crate::compose::{ComposeError, Composer, SubscriptionConfirmation};
use crate::outbox::{self, OutboxError};

/// How long a confirmation link works.
pub const CONFIRM_VALID: SignedDuration = SignedDuration::from_hours(7 * 24);
/// A new confirmation email for the same address goes out at most this often.
pub const RESEND_AFTER: SignedDuration = SignedDuration::from_mins(5);
const MAX_EMAIL_LEN: usize = 254;
const TOKEN_BYTES: usize = 32;
/// base64url without padding of [`TOKEN_BYTES`].
const TOKEN_LEN: usize = 43;

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
    if email.len() > MAX_EMAIL_LEN || lettre::Address::from_str(email).is_err() {
        return Err(SubscriberError::Address);
    }
    Ok(email.to_owned())
}

fn new_token() -> (String, Vec<u8>) {
    let mut bytes = [0_u8; TOKEN_BYTES];
    rand::rng().fill(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash(&token);
    (token, hash)
}

fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// `None` for anything that cannot be one of our tokens, so junk never reaches the
/// database.
fn token_hash(token: &str) -> Option<Vec<u8>> {
    (token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| hash(token))
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

/// Takes the subscriber behind an unsubscribe link off the list and voids an open
/// confirmation link; `false` for an unknown link. Safe to repeat.
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
    let done = sqlx::query_scalar!(
        "UPDATE subscriber s SET
            unsubscribed_at = coalesce(s.unsubscribed_at,
                timestamptz 'epoch' + $2::bigint * interval '1 microsecond'),
            confirm_token_hash = NULL,
            confirm_expires_at = NULL
        FROM subscriber_unsubscribe_token t
        WHERE t.token_hash = $1 AND t.subscriber_id = s.id
        RETURNING s.id",
        hash,
        micros(clock.now()),
    )
    .fetch_optional(conn)
    .await?;
    Ok(done.is_some())
}

/// Confirmed subscribers who have not unsubscribed: the only ones drop alerts go to.
///
/// # Errors
/// [`SubscriberError::Db`].
pub async fn active(conn: &mut PgConnection) -> Result<Vec<Subscriber>, SubscriberError> {
    Ok(sqlx::query_as!(
        Subscriber,
        "SELECT id, email FROM subscriber
        WHERE confirmed_at IS NOT NULL AND unsubscribed_at IS NULL
        ORDER BY id",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_43_url_safe_characters_and_hash_to_32_bytes() {
        let (token, stored) = new_token();
        assert_eq!(token.len(), TOKEN_LEN);
        assert_eq!(stored.len(), 32);
        assert_eq!(token_hash(&token), Some(stored));
        assert_ne!(new_token().0, token);
    }

    #[test]
    fn junk_tokens_never_reach_the_database() {
        assert_eq!(token_hash(""), None);
        assert_eq!(token_hash("abc"), None);
        assert_eq!(token_hash(&"a".repeat(44)), None);
        assert_eq!(token_hash(&format!("{}!", "a".repeat(42))), None);
        assert!(token_hash(&"a-_".repeat(14).chars().chain(['Z']).collect::<String>()).is_some());
    }

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
    }
}
