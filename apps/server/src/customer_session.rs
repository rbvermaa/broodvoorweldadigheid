use auth::LoginId;
use auth::customer::{self, Customer};
use domain::CustomerId;
use tower_sessions::Session;

use crate::csrf;
use crate::state::AppState;

/// The session key that holds the customer's login id. Staff logins use their own key.
pub const CUSTOMER_KEY: &str = "customer_login";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Session(#[from] tower_sessions::session::Error),
    #[error(transparent)]
    Customer(#[from] customer::Error),
}

/// Signs `id` in on this session. A customer login already on it ends. The session gets
/// a new id and a new CSRF token, so a planted session id and its token stop working; the
/// cart and a staff login stay.
///
/// # Errors
/// Fails when the session store or the database cannot be written.
pub async fn sign_in(state: &AppState, session: &Session, id: CustomerId) -> Result<(), Error> {
    end_login(state, session).await?;
    session.remove::<String>(CUSTOMER_KEY).await?;
    csrf::forget(session).await?;
    session.cycle_id().await?;
    let login = customer::start(&state.pool, id, state.clock.now()).await?;
    session.insert(CUSTOMER_KEY, login.as_str()).await?;
    Ok(())
}

/// The customer signed in on this session, if the login is still live.
///
/// # Errors
/// Fails when the session store or the database cannot be read.
pub async fn current(state: &AppState, session: &Session) -> Result<Option<Customer>, Error> {
    let Some(login) = session.get::<String>(CUSTOMER_KEY).await? else {
        return Ok(None);
    };
    let found = customer::current(&state.pool, &LoginId::new(login), state.clock.now()).await?;
    if found.is_none() {
        session.remove::<String>(CUSTOMER_KEY).await?;
    }
    Ok(found)
}

/// Ends the login in the database first, so a request still in flight that saves this
/// session again cannot bring it back. The session gets a new id and CSRF token; the cart
/// and a staff login stay.
///
/// # Errors
/// Fails when the session store or the database cannot be written.
pub async fn sign_out(state: &AppState, session: &Session) -> Result<(), Error> {
    end_login(state, session).await?;
    session.remove::<String>(CUSTOMER_KEY).await?;
    csrf::forget(session).await?;
    session.cycle_id().await?;
    Ok(())
}

async fn end_login(state: &AppState, session: &Session) -> Result<(), Error> {
    if let Some(login) = session.get::<String>(CUSTOMER_KEY).await? {
        customer::end(&state.pool, &LoginId::new(login)).await?;
    }
    Ok(())
}
