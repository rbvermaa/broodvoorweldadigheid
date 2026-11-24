use db::PgPool;
use domain::StaffUserId;
use sqlx::Row;
use sqlx::postgres::PgRow;

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

/// Who may log in while nobody is staff yet. Set from `BVW_BOOTSTRAP_OWNER_SUB` or
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
    #[error(transparent)]
    Role(#[from] UnknownRole),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// Serializes bootstrap logins, so two people matching the bootstrap setting cannot both
/// become owner.
const BOOTSTRAP_LOCK: i64 = 0x6276_775f_6f77_6e72;

macro_rules! columns {
    () => {
        "id, oidc_sub, name, email, role, active"
    };
}

fn from_row(row: &PgRow) -> Result<StaffUser, Error> {
    Ok(StaffUser {
        id: StaffUserId::new(row.try_get("id")?),
        oidc_sub: row.try_get("oidc_sub")?,
        name: row.try_get("name")?,
        email: row.try_get("email")?,
        role: row.try_get::<String, _>("role")?.parse()?,
        active: row.try_get("active")?,
    })
}

/// # Errors
/// `DuplicateSub` when the subject is taken, or a database error.
pub async fn add(pool: &PgPool, staff: &NewStaff) -> Result<StaffUser, Error> {
    let row = sqlx::query(concat!(
        "INSERT INTO staff_user (oidc_sub, name, email, role) VALUES ($1, $2, $3, $4)
         ON CONFLICT (oidc_sub) DO NOTHING RETURNING ",
        columns!()
    ))
    .bind(&staff.oidc_sub)
    .bind(&staff.name)
    .bind(&staff.email)
    .bind(staff.role.as_str())
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(from_row).ok_or(Error::DuplicateSub)?
}

/// # Errors
/// A database error, or a role the code does not know.
pub async fn find(pool: &PgPool, id: StaffUserId) -> Result<Option<StaffUser>, Error> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM staff_user WHERE id = $1"
    ))
    .bind(id.get())
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(from_row).transpose()
}

/// # Errors
/// A database error, or a role the code does not know.
pub async fn find_by_sub(pool: &PgPool, sub: &str) -> Result<Option<StaffUser>, Error> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM staff_user WHERE oidc_sub = $1"
    ))
    .bind(sub)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(from_row).transpose()
}

/// Lets a known, active person in and refreshes their name and email from the provider.
/// An unknown person only gets in as the bootstrap owner, and only while there is no owner.
///
/// # Errors
/// `Inactive` or `Unknown` when the person may not log in, or a database error.
pub async fn sign_in(
    pool: &PgPool,
    identity: &Identity,
    bootstrap: &BootstrapOwner,
) -> Result<StaffUser, Error> {
    let name = identity.display_name();
    let row = sqlx::query(concat!(
        "UPDATE staff_user SET name = $2, email = COALESCE($3, email)
         WHERE oidc_sub = $1 AND active RETURNING ",
        columns!()
    ))
    .bind(&identity.sub)
    .bind(name)
    .bind(&identity.email)
    .fetch_optional(pool)
    .await?;
    if let Some(row) = row {
        return from_row(&row);
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
    let row = sqlx::query(concat!(
        "INSERT INTO staff_user (oidc_sub, name, email, role)
         SELECT $1, $2, $3, 'owner'
         WHERE NOT EXISTS (SELECT 1 FROM staff_user WHERE role = 'owner')
         ON CONFLICT (oidc_sub) DO NOTHING RETURNING ",
        columns!()
    ))
    .bind(&identity.sub)
    .bind(name)
    .bind(&identity.email)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    row.as_ref().map(from_row).ok_or(Error::Unknown)?
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
