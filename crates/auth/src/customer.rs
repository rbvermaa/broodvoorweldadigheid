//! Customer login by email magic link (spec, Security; ADR 0029). A link is 32 random
//! bytes, base64url; only its SHA-256 is stored. It works once, for [`LINK_VALID`].

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use db::PgPool;
use db::time::to_micros;
use domain::CustomerId;
use jiff::{SignedDuration, Timestamp};
use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection};

use crate::LoginId;

/// How long a link in the email works.
pub const LINK_VALID: SignedDuration = SignedDuration::from_mins(30);
/// At most [`LINKS_PER_WINDOW`] links per customer within this long.
pub const LINK_WINDOW: SignedDuration = SignedDuration::from_mins(15);
pub const LINKS_PER_WINDOW: i64 = 3;
/// A login ends after this long without a request.
pub const IDLE_TIMEOUT: SignedDuration = SignedDuration::from_hours(7 * 24);
/// A login ends this long after sign-in, however busy it is.
pub const ABSOLUTE_TIMEOUT: SignedDuration = SignedDuration::from_hours(30 * 24);

const MAX_EMAIL_LEN: usize = 254;
const TOKEN_BYTES: usize = 32;
/// base64url without padding of [`TOKEN_BYTES`].
const TOKEN_LEN: usize = 43;

#[derive(Debug, thiserror::Error)]
#[error("database error: {0}")]
pub struct Error(#[from] sqlx::Error);

/// The secret in a login link. Whoever holds it can sign in once, so it is never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkToken(String);

impl LinkToken {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for LinkToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkToken(<redacted>)")
    }
}

/// A new link, to email to `email`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginLink {
    pub customer: CustomerId,
    pub email: String,
    pub token: LinkToken,
    pub expires_at: Timestamp,
}

/// What a request for a link did. The visitor sees the same answer for each, so nobody
/// learns whether an address belongs to a customer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRequest {
    Issued(LoginLink),
    /// No customer has this address, or it is not an address.
    Unknown,
    /// The customer already got [`LINKS_PER_WINDOW`] links within [`LINK_WINDOW`].
    Throttled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Customer {
    pub id: CustomerId,
    pub email: String,
    pub name: String,
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

fn new_token() -> (LinkToken, Vec<u8>) {
    let mut bytes = [0_u8; TOKEN_BYTES];
    rand::rng().fill(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash(&token);
    (LinkToken(token), hash)
}

fn since(now: Timestamp, span: SignedDuration) -> i64 {
    to_micros(now.checked_sub(span).unwrap_or(Timestamp::UNIX_EPOCH))
}

/// Issues a link for the customer with this email address, if there is one and they are
/// under the limit, and clears links that expired. Customer emails are stored trimmed and
/// lower-cased, so `raw` is too before the lookup. The customer row stays locked until the
/// outermost transaction ends, so two requests cannot both pass the limit; call it inside
/// the transaction that queues the email.
///
/// # Errors
/// A database error.
pub async fn request_link(
    conn: &mut PgConnection,
    raw: &str,
    now: Timestamp,
) -> Result<LinkRequest, Error> {
    let email = raw.trim().to_lowercase();
    if email.is_empty() || email.len() > MAX_EMAIL_LEN {
        return Ok(LinkRequest::Unknown);
    }
    let mut tx = conn.begin().await?;
    let Some(customer) = sqlx::query!(
        "SELECT id, email FROM customer WHERE email = $1 AND erased_at IS NULL FOR UPDATE",
        email,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(LinkRequest::Unknown);
    };
    let at = to_micros(now);
    sqlx::query!(
        "DELETE FROM customer_login_link
         WHERE customer_id = $1
           AND expires_at <= timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
           AND created_at <= timestamptz 'epoch' + $3::bigint * interval '1 microsecond'",
        customer.id,
        at,
        since(now, LINK_WINDOW),
    )
    .execute(&mut *tx)
    .await?;
    let recent = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM customer_login_link
         WHERE customer_id = $1
           AND created_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'"#,
        customer.id,
        since(now, LINK_WINDOW),
    )
    .fetch_one(&mut *tx)
    .await?;
    if recent >= LINKS_PER_WINDOW {
        return Ok(LinkRequest::Throttled);
    }
    let (token, hash) = new_token();
    let expires = now.checked_add(LINK_VALID).unwrap_or(now);
    sqlx::query!(
        "INSERT INTO customer_login_link (token_hash, customer_id, created_at, expires_at)
         VALUES ($1, $2, timestamptz 'epoch' + $3::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $4::bigint * interval '1 microsecond')",
        hash,
        customer.id,
        at,
        to_micros(expires),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(LinkRequest::Issued(LoginLink {
        customer: CustomerId::new(customer.id),
        email: customer.email,
        token,
        expires_at: expires,
    }))
}

/// Whether `token` is an unused, unexpired link, for the page that asks to confirm.
/// Looking does not use it up.
///
/// # Errors
/// A database error.
pub async fn link_is_valid(
    conn: &mut PgConnection,
    token: &str,
    now: Timestamp,
) -> Result<bool, Error> {
    let Some(hash) = token_hash(token) else {
        return Ok(false);
    };
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM customer_login_link
            WHERE token_hash = $1 AND used_at IS NULL
              AND expires_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond')
         AS "valid!""#,
        hash,
        to_micros(now),
    )
    .fetch_one(conn)
    .await?)
}

/// Uses up the link and gives its customer; `None` for an unknown, used or expired link,
/// or one of an erased customer (F12).
///
/// # Errors
/// A database error.
pub async fn redeem_link(
    conn: &mut PgConnection,
    token: &str,
    now: Timestamp,
) -> Result<Option<CustomerId>, Error> {
    let Some(hash) = token_hash(token) else {
        return Ok(None);
    };
    let id = sqlx::query_scalar!(
        "UPDATE customer_login_link l
         SET used_at = timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
         FROM customer c
         WHERE l.token_hash = $1 AND l.used_at IS NULL AND c.id = l.customer_id
           AND c.erased_at IS NULL
           AND l.expires_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
         RETURNING l.customer_id",
        hash,
        to_micros(now),
    )
    .fetch_optional(conn)
    .await?;
    Ok(id.map(CustomerId::new))
}

/// Starts a login for `customer`, and clears every expired customer login.
///
/// # Errors
/// A database error.
pub async fn start(pool: &PgPool, customer: CustomerId, now: Timestamp) -> Result<LoginId, Error> {
    sqlx::query!(
        "DELETE FROM customer_login
         WHERE created_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
            OR last_seen_at <= timestamptz 'epoch' + $2::bigint * interval '1 microsecond'",
        since(now, ABSOLUTE_TIMEOUT),
        since(now, IDLE_TIMEOUT),
    )
    .execute(pool)
    .await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO customer_login (customer_id, created_at, last_seen_at)
         VALUES ($1, timestamptz 'epoch' + $2::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $2::bigint * interval '1 microsecond')
         RETURNING id",
        customer.get(),
        to_micros(now),
    )
    .fetch_one(pool)
    .await?;
    Ok(LoginId::new(id))
}

/// The customer behind a live login, which counts as seen now. A login that timed out
/// is ended and gives `None`, and so does a login of an erased customer: an erase (F12)
/// that commits between redeeming a link and starting the login leaves one behind.
///
/// # Errors
/// A database error.
pub async fn current(
    pool: &PgPool,
    login: &LoginId,
    now: Timestamp,
) -> Result<Option<Customer>, Error> {
    let row = sqlx::query!(
        "UPDATE customer_login l
         SET last_seen_at = timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
         FROM customer c
         WHERE l.id = $1 AND c.id = l.customer_id AND c.erased_at IS NULL
           AND l.created_at > timestamptz 'epoch' + $3::bigint * interval '1 microsecond'
           AND l.last_seen_at > timestamptz 'epoch' + $4::bigint * interval '1 microsecond'
         RETURNING c.id, c.email, c.name",
        login.as_str(),
        to_micros(now),
        since(now, ABSOLUTE_TIMEOUT),
        since(now, IDLE_TIMEOUT),
    )
    .fetch_optional(pool)
    .await?;
    match row {
        Some(row) => Ok(Some(Customer {
            id: CustomerId::new(row.id),
            email: row.email,
            name: row.name,
        })),
        None => {
            end(pool, login).await?;
            Ok(None)
        }
    }
}

/// # Errors
/// A database error.
pub async fn end(pool: &PgPool, login: &LoginId) -> Result<(), Error> {
    sqlx::query!("DELETE FROM customer_login WHERE id = $1", login.as_str())
        .execute(pool)
        .await?;
    Ok(())
}

/// What [`delete_expired`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expired {
    pub links: u64,
    pub logins: u64,
}

/// Deletes login links past their expiry, used or not, and logins past the idle or
/// absolute timeout. Links count against [`LINKS_PER_WINDOW`] only within
/// [`LINK_WINDOW`], which is shorter than [`LINK_VALID`], so this never lifts the limit.
///
/// # Errors
/// A database error.
pub async fn delete_expired(conn: &mut PgConnection, now: Timestamp) -> Result<Expired, Error> {
    let links = sqlx::query!(
        "DELETE FROM customer_login_link
         WHERE expires_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'",
        to_micros(now),
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let logins = sqlx::query!(
        "DELETE FROM customer_login
         WHERE created_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
            OR last_seen_at <= timestamptz 'epoch' + $2::bigint * interval '1 microsecond'",
        since(now, ABSOLUTE_TIMEOUT),
        since(now, IDLE_TIMEOUT),
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(Expired { links, logins })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_43_url_safe_characters_and_hash_to_32_bytes() {
        let (token, stored) = new_token();
        assert_eq!(token.as_str().len(), TOKEN_LEN);
        assert_eq!(stored.len(), 32);
        assert_eq!(token_hash(token.as_str()), Some(stored));
        assert_ne!(new_token().0, token);
    }

    #[test]
    fn junk_tokens_never_reach_the_database() {
        assert_eq!(token_hash(""), None);
        assert_eq!(token_hash(&"a".repeat(42)), None);
        assert_eq!(token_hash(&"a".repeat(44)), None);
        assert_eq!(token_hash(&format!("{}!", "a".repeat(42))), None);
        assert!(token_hash(&"a".repeat(43)).is_some());
    }
}
