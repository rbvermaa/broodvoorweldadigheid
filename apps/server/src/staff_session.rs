use auth::StaffUser;
use domain::StaffUserId;
use tower_sessions::Session;
use tower_sessions::session::Error;

/// The session key that holds the signed-in staff member's id, and nothing else about them.
pub const STAFF_KEY: &str = "staff_id";

/// Signs `staff` in on this session, whatever way they proved who they are. The session is
/// emptied and gets a new id first, so a planted session id and its CSRF token stop working.
///
/// # Errors
/// Fails when the session store cannot be written.
pub async fn sign_in(session: &Session, staff: &StaffUser) -> Result<(), Error> {
    session.clear().await;
    session.cycle_id().await?;
    session.insert(STAFF_KEY, staff.id.get()).await
}

/// # Errors
/// Fails when the session store cannot be read.
pub async fn staff_id(session: &Session) -> Result<Option<StaffUserId>, Error> {
    Ok(session.get::<i64>(STAFF_KEY).await?.map(StaffUserId::new))
}
