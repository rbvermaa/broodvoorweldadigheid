use auth::{LoginId, StaffUser};
use tower_sessions::Session;

use crate::state::AppState;

/// The session key that holds the login id, and nothing else about the staff member.
pub const STAFF_KEY: &str = "staff_login";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Session(#[from] tower_sessions::session::Error),
    #[error(transparent)]
    Staff(#[from] auth::staff::Error),
}

/// Signs `staff` in on this session, whatever way they proved who they are. A login already
/// on this session ends, so a shared browser does not keep the previous person signed in.
/// The session is emptied and gets a new id, so a planted session id and its CSRF token stop
/// working.
///
/// # Errors
/// Fails when the session store or the database cannot be written.
pub async fn sign_in(state: &AppState, session: &Session, staff: &StaffUser) -> Result<(), Error> {
    end_login(state, session).await?;
    session.clear().await;
    session.cycle_id().await?;
    let login = auth::login::start(&state.pool, staff.id, state.clock.now()).await?;
    session.insert(STAFF_KEY, login.as_str()).await?;
    Ok(())
}

/// The staff member signed in on this session, if the login is still live.
///
/// # Errors
/// Fails when the session store or the database cannot be read.
pub async fn current(state: &AppState, session: &Session) -> Result<Option<StaffUser>, Error> {
    let Some(login) = session.get::<String>(STAFF_KEY).await? else {
        return Ok(None);
    };
    let staff = auth::login::current(&state.pool, &LoginId::new(login), state.clock.now()).await?;
    if staff.is_none() {
        session.flush().await?;
    }
    Ok(staff)
}

/// Ends the login in the database first, so a request still in flight that saves this
/// session again cannot bring it back.
///
/// # Errors
/// Fails when the session store or the database cannot be written.
pub async fn sign_out(state: &AppState, session: &Session) -> Result<(), Error> {
    end_login(state, session).await?;
    session.flush().await?;
    Ok(())
}

async fn end_login(state: &AppState, session: &Session) -> Result<(), Error> {
    if let Some(login) = session.get::<String>(STAFF_KEY).await? {
        auth::login::end(&state.pool, &LoginId::new(login)).await?;
    }
    Ok(())
}
