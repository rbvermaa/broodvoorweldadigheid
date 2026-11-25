use db::PgPool;
use db::time::to_micros;
use domain::StaffUserId;
use jiff::{SignedDuration, Timestamp};

use crate::staff::{Error, Row, StaffUser};

/// A login ends after this long without a request.
pub const IDLE_TIMEOUT: SignedDuration = SignedDuration::from_hours(12);
/// A login ends this long after sign-in, however busy it is.
pub const ABSOLUTE_TIMEOUT: SignedDuration = SignedDuration::from_hours(7 * 24);

/// The random id of one signed-in browser, kept in its session. Whoever holds it is that
/// staff member or customer, so it is never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginId(String);

impl LoginId {
    #[must_use]
    pub fn new(id: String) -> Self {
        Self(id)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for LoginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoginId(<redacted>)")
    }
}

/// Starts a login for `staff`, and clears every expired login, so they do not pile up.
///
/// # Errors
/// A database error.
pub async fn start(pool: &PgPool, staff: StaffUserId, now: Timestamp) -> Result<LoginId, Error> {
    let (now, idle, absolute) = bounds(now);
    sqlx::query!(
        "DELETE FROM staff_login
         WHERE created_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
            OR last_seen_at <= timestamptz 'epoch' + $2::bigint * interval '1 microsecond'",
        absolute,
        idle,
    )
    .execute(pool)
    .await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO staff_login (staff_id, created_at, last_seen_at)
         VALUES ($1, timestamptz 'epoch' + $2::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $2::bigint * interval '1 microsecond')
         RETURNING id",
        staff.get(),
        now,
    )
    .fetch_one(pool)
    .await?;
    Ok(LoginId(id))
}

/// The active staff member behind a live login, which counts as seen now. A login that
/// timed out, or whose staff member is inactive, is ended and gives `None`.
///
/// # Errors
/// A database error, or a role the code does not know.
pub async fn current(
    pool: &PgPool,
    login: &LoginId,
    now: Timestamp,
) -> Result<Option<StaffUser>, Error> {
    let (now, idle, absolute) = bounds(now);
    let row = sqlx::query_as!(
        Row,
        "UPDATE staff_login l
         SET last_seen_at = timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
         FROM staff_user s
         WHERE l.id = $1 AND s.id = l.staff_id AND s.active
           AND l.created_at > timestamptz 'epoch' + $3::bigint * interval '1 microsecond'
           AND l.last_seen_at > timestamptz 'epoch' + $4::bigint * interval '1 microsecond'
         RETURNING s.id, s.oidc_sub, s.name, s.email, s.role, s.active",
        login.as_str(),
        now,
        absolute,
        idle,
    )
    .fetch_optional(pool)
    .await?;
    match row {
        Some(row) => Ok(Some(row.try_into()?)),
        None => {
            end(pool, login).await?;
            Ok(None)
        }
    }
}

/// # Errors
/// A database error.
pub async fn end(pool: &PgPool, login: &LoginId) -> Result<(), Error> {
    sqlx::query!("DELETE FROM staff_login WHERE id = $1", login.as_str())
        .execute(pool)
        .await?;
    Ok(())
}

/// `now`, and the moments before which a login is idle or too old, in epoch microseconds.
fn bounds(now: Timestamp) -> (i64, i64, i64) {
    let since = |timeout: SignedDuration| {
        to_micros(now.checked_sub(timeout).unwrap_or(Timestamp::UNIX_EPOCH))
    };
    (to_micros(now), since(IDLE_TIMEOUT), since(ABSOLUTE_TIMEOUT))
}
