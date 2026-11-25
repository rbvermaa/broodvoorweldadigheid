use db::PgPool;
use domain::StaffUserId;

use crate::identity::Identity;
use crate::role::{Role, UnknownRole};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaffUser {
    pub id: StaffUserId,
    pub oidc_sub: String,
    pub name: String,
    pub email: Option<String>,
    pub role: Role,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewStaff {
    pub oidc_sub: String,
    pub name: String,
    pub email: Option<String>,
    pub role: Role,
}

/// Who may sign in while nobody is owner yet. Set from `BVW_BOOTSTRAP_OWNER_SUB` or
/// `BVW_BOOTSTRAP_OWNER_EMAIL`; an email only counts when the provider says it is verified.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootstrapOwner {
    pub sub: Option<String>,
    pub email: Option<String>,
}

impl BootstrapOwner {
    #[must_use]
    pub fn matches(&self, identity: &Identity) -> bool {
        let by_sub = self
            .sub
            .as_deref()
            .is_some_and(|sub| !sub.is_empty() && sub == identity.sub);
        let by_email = identity.email_verified
            && self
                .email
                .as_deref()
                .zip(identity.email.as_deref())
                .is_some_and(|(want, got)| !want.is_empty() && want.eq_ignore_ascii_case(got));
        by_sub || by_email
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("a staff user with this OIDC subject already exists")]
    DuplicateSub,
    #[error("staff user {0:?} is not active")]
    Inactive(String),
    #[error("unknown OIDC subject; the owner must add this person first")]
    Unknown,
    #[error("the last active owner cannot be removed, demoted or deactivated")]
    LastOwner,
    #[error(transparent)]
    Role(#[from] UnknownRole),
    #[error("database error: {0}")]
    Db(sqlx::Error),
}

impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        let last_owner = error
            .as_database_error()
            .and_then(|db| db.constraint())
            .is_some_and(|constraint| constraint == "staff_user_last_owner");
        if last_owner {
            Self::LastOwner
        } else {
            Self::Db(error)
        }
    }
}

/// Serializes bootstrap sign-ins, so two people matching the bootstrap setting cannot both
/// become owner. The `keep_last_owner` trigger takes the same lock.
const BOOTSTRAP_LOCK: i64 = 0x6276_775f_6f77_6e72;

pub(crate) struct Row {
    pub id: i64,
    pub oidc_sub: String,
    pub name: String,
    pub email: Option<String>,
    pub role: String,
    pub active: bool,
}

impl TryFrom<Row> for StaffUser {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: StaffUserId::new(row.id),
            oidc_sub: row.oidc_sub,
            name: row.name,
            email: row.email,
            role: row.role.parse()?,
            active: row.active,
        })
    }
}

/// # Errors
/// `DuplicateSub` when the subject is taken, or a database error.
pub async fn add(pool: &PgPool, staff: &NewStaff) -> Result<StaffUser, Error> {
    let row = sqlx::query_as!(
        Row,
        "INSERT INTO staff_user (oidc_sub, name, email, role) VALUES ($1, $2, $3, $4)
         ON CONFLICT (oidc_sub) DO NOTHING
         RETURNING id, oidc_sub, name, email, role, active",
        staff.oidc_sub,
        staff.name,
        staff.email,
        staff.role.as_str(),
    )
    .fetch_optional(pool)
    .await?;
    row.ok_or(Error::DuplicateSub)?.try_into()
}

/// # Errors
/// A database error, or a role the code does not know.
pub async fn find(pool: &PgPool, id: StaffUserId) -> Result<Option<StaffUser>, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, oidc_sub, name, email, role, active FROM staff_user WHERE id = $1",
        id.get(),
    )
    .fetch_optional(pool)
    .await?
    .map(StaffUser::try_from)
    .transpose()
}

/// # Errors
/// A database error, or a role the code does not know.
pub async fn find_by_sub(pool: &PgPool, sub: &str) -> Result<Option<StaffUser>, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, oidc_sub, name, email, role, active FROM staff_user WHERE oidc_sub = $1",
        sub,
    )
    .fetch_optional(pool)
    .await?
    .map(StaffUser::try_from)
    .transpose()
}

/// Gives a staff member another role. Takes effect on their next request.
///
/// # Errors
/// `LastOwner` when this would leave no active owner, or a database error.
pub async fn set_role(pool: &PgPool, id: StaffUserId, role: Role) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE staff_user SET role = $2 WHERE id = $1",
        id.get(),
        role.as_str(),
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Stops a staff member from signing in and ends every login they have.
///
/// # Errors
/// `LastOwner` when this would leave no active owner, or a database error.
pub async fn deactivate(pool: &PgPool, id: StaffUserId) -> Result<(), Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "UPDATE staff_user SET active = false WHERE id = $1",
        id.get()
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM staff_login WHERE staff_id = $1", id.get())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Lets a known, active person in and refreshes their name and email from the provider.
/// An unknown person only gets in as the bootstrap owner, and only while there is no owner.
///
/// # Errors
/// `Inactive` or `Unknown` when the person may not sign in, or a database error.
pub async fn sign_in(
    pool: &PgPool,
    identity: &Identity,
    bootstrap: &BootstrapOwner,
) -> Result<StaffUser, Error> {
    let row = sqlx::query_as!(
        Row,
        "UPDATE staff_user SET name = COALESCE($2, name), email = COALESCE($3, email)
         WHERE oidc_sub = $1 AND active
         RETURNING id, oidc_sub, name, email, role, active",
        identity.sub,
        identity.claimed_name(),
        identity.email,
    )
    .fetch_optional(pool)
    .await?;
    if let Some(row) = row {
        return row.try_into();
    }
    if let Some(staff) = find_by_sub(pool, &identity.sub).await? {
        return Err(Error::Inactive(staff.name));
    }
    if !bootstrap.matches(identity) {
        return Err(Error::Unknown);
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(BOOTSTRAP_LOCK)
        .execute(&mut *tx)
        .await?;
    let name = identity.display_name();
    let row = sqlx::query_as!(
        Row,
        r#"INSERT INTO staff_user (oidc_sub, name, email, role)
         SELECT $1, $2, $3, 'owner'
         WHERE NOT EXISTS (SELECT 1 FROM staff_user WHERE role = 'owner')
         ON CONFLICT (oidc_sub) DO NOTHING
         RETURNING id AS "id!", oidc_sub AS "oidc_sub!", name AS "name!", email,
            role AS "role!", active AS "active!""#,
        identity.sub,
        name,
        identity.email,
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    row.ok_or(Error::Unknown)?.try_into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(sub: &str, email: Option<&str>, verified: bool) -> Identity {
        Identity {
            sub: sub.to_string(),
            name: None,
            preferred_username: None,
            email: email.map(str::to_string),
            email_verified: verified,
        }
    }

    #[test]
    fn bootstrap_matches_the_subject() {
        let bootstrap = BootstrapOwner {
            sub: Some("abc".to_string()),
            email: None,
        };
        assert!(bootstrap.matches(&identity("abc", None, false)));
        assert!(!bootstrap.matches(&identity("abd", None, false)));
    }

    #[test]
    fn bootstrap_matches_a_verified_email_in_any_case() {
        let bootstrap = BootstrapOwner {
            sub: None,
            email: Some("Eigenaar@example.org".to_string()),
        };
        assert!(bootstrap.matches(&identity("x", Some("eigenaar@EXAMPLE.org"), true)));
        assert!(!bootstrap.matches(&identity("x", Some("eigenaar@example.org"), false)));
        assert!(!bootstrap.matches(&identity("x", Some("ander@example.org"), true)));
        assert!(!bootstrap.matches(&identity("x", None, true)));
    }

    #[test]
    fn empty_bootstrap_matches_nobody() {
        assert!(!BootstrapOwner::default().matches(&identity("", Some(""), true)));
        let blank = BootstrapOwner {
            sub: Some(String::new()),
            email: Some(String::new()),
        };
        assert!(!blank.matches(&identity("", Some(""), true)));
    }
}
