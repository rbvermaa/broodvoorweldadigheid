//! Deleting what is no longer needed (F12, ADR 0031): expired customer login links and
//! logins (ADR 0029), account-deleted emails once they went out, and every other email
//! once its retention ([`notify::outbox::RETENTION`]) is over.

use domain::Clock;
use notify::EmailKind;
use sqlx::PgConnection;

use crate::Error;

/// What one cleanup removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub login_links: u64,
    pub logins: u64,
    pub emails: u64,
    pub old_emails: u64,
}

/// # Errors
/// [`Error::Db`] or [`Error::Outbox`].
pub async fn tick(conn: &mut PgConnection, clock: &dyn Clock) -> Result<CleanupReport, Error> {
    let expired = auth::customer::delete_expired(conn, clock.now()).await?;
    let emails = notify::outbox::delete_finished(conn, EmailKind::AccountDeleted).await?;
    let old_emails = notify::outbox::delete_retired(conn, clock).await?;
    Ok(CleanupReport {
        login_links: expired.links,
        logins: expired.logins,
        emails,
        old_emails,
    })
}
