use std::sync::Arc;

use auth::StaffUser;
use db::PgPool;
use jiff::Timestamp;
use server::{csrf, session, staff_session};
use tower_sessions::Session;

/// A session in the store with `staff` signed in at `now`, as any sign-in method leaves it.
/// Returns the `Cookie` header value and the session's CSRF token. `now` must be the app's
/// clock: a login started at another time can count as expired, and starting one clears
/// the logins that look expired at that time.
pub async fn signed_in_at(pool: &PgPool, staff: &StaffUser, now: Timestamp) -> (String, String) {
    let login = auth::login::start(pool, staff.id, now)
        .await
        .expect("start login");
    let session = Session::new(None, Arc::new(session::store(pool.clone())), None);
    session
        .insert(staff_session::STAFF_KEY, login.as_str())
        .await
        .expect("insert login id");
    let token = csrf::token(&session).await.expect("csrf token");
    session.save().await.expect("save session");
    let id = session.id().expect("saved session has an id");
    (format!("{}={id}", session::COOKIE_NAME), token)
}
