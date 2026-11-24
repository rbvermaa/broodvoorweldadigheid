use std::sync::Arc;

use auth::StaffUser;
use db::PgPool;
use server::{csrf, session, staff_session};
use tower_sessions::Session;

/// A session in the store with `staff` signed in, as any sign-in method leaves it. Returns
/// the `Cookie` header value and the session's CSRF token.
pub async fn signed_in(pool: &PgPool, staff: &StaffUser) -> (String, String) {
    let session = Session::new(None, Arc::new(session::store(pool.clone())), None);
    session
        .insert(staff_session::STAFF_KEY, staff.id.get())
        .await
        .expect("insert staff id");
    let token = csrf::token(&session).await.expect("csrf token");
    session.save().await.expect("save session");
    let id = session.id().expect("saved session has an id");
    (format!("{}={id}", session::COOKIE_NAME), token)
}
