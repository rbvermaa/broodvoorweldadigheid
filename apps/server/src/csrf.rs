use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use tower_sessions::Session;

use crate::pages;
use crate::state::AppState;

pub const FIELD: &str = "_csrf";
pub const HEADER: &str = "x-csrf-token";
const SESSION_KEY: &str = "csrf_token";
const TOKEN_BYTES: usize = 32;
/// Same as axum's default body limit, so a form that passes here also passes `Form`.
const FORM_LIMIT: usize = 2 * 1024 * 1024;

/// The session's CSRF token, created on first use. Put it in a hidden `_csrf` field or
/// pass it to `Layout::with_csrf_token`.
///
/// # Errors
/// Fails when the session store cannot be read or written.
pub async fn token(session: &Session) -> Result<String, tower_sessions::session::Error> {
    if let Some(token) = session.get::<String>(SESSION_KEY).await? {
        return Ok(token);
    }
    let mut bytes = [0_u8; TOKEN_BYTES];
    rand::rng().fill(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    session.insert(SESSION_KEY, &token).await?;
    Ok(token)
}

/// Rejects every request that can change state unless it carries the session's token,
/// in the `X-CSRF-Token` header (HTMX, through app.js) or a urlencoded `_csrf` field.
pub async fn verify(
    State(state): State<AppState>,
    session: Session,
    request: Request,
    next: Next,
) -> Response {
    if is_safe(request.method()) {
        return next.run(request).await;
    }
    let expected = match session.get::<String>(SESSION_KEY).await {
        Ok(Some(token)) => token,
        Ok(None) => return forbidden(&state, "no CSRF token in the session"),
        Err(error) => {
            tracing::error!(%error, "cannot load the session");
            return pages::error(&state, StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    if let Some(sent) = request.headers().get(HEADER) {
        return if constant_time_eq(sent.as_bytes(), expected.as_bytes()) {
            next.run(request).await
        } else {
            forbidden(&state, "wrong CSRF header")
        };
    }
    if !is_urlencoded_form(&request) {
        return forbidden(&state, "no CSRF header");
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = to_bytes(body, FORM_LIMIT).await else {
        return forbidden(&state, "form body unreadable or too large");
    };
    if form_field(&bytes, FIELD).is_some_and(|sent| constant_time_eq(sent, expected.as_bytes())) {
        next.run(Request::from_parts(parts, Body::from(bytes)))
            .await
    } else {
        forbidden(&state, "missing or wrong CSRF form field")
    }
}

fn forbidden(state: &AppState, reason: &'static str) -> Response {
    tracing::warn!(reason, "CSRF check failed");
    pages::error(state, StatusCode::FORBIDDEN)
}

fn is_safe(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

fn is_urlencoded_form(request: &Request) -> bool {
    request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| {
            mime.trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        })
}

/// The raw value of the first `name=` pair. The token is base64url and the name is plain
/// ASCII, so neither is ever percent-encoded.
fn form_field<'a>(body: &'a [u8], name: &str) -> Option<&'a [u8]> {
    body.split(|&b| b == b'&').find_map(|pair| {
        let mut parts = pair.splitn(2, |&b| b == b'=');
        (parts.next()? == name.as_bytes()).then(|| parts.next().unwrap_or_default())
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_field_finds_the_first_matching_pair() {
        assert_eq!(form_field(b"a=1&_csrf=abc&b=2", FIELD), Some(&b"abc"[..]));
        assert_eq!(form_field(b"_csrf=one&_csrf=two", FIELD), Some(&b"one"[..]));
        assert_eq!(form_field(b"_csrf", FIELD), Some(&b""[..]));
        assert_eq!(form_field(b"x_csrf=abc", FIELD), None);
        assert_eq!(form_field(b"", FIELD), None);
    }

    #[test]
    fn constant_time_eq_compares_length_and_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn only_read_only_methods_skip_the_check() {
        for method in [Method::GET, Method::HEAD, Method::OPTIONS, Method::TRACE] {
            assert!(is_safe(&method), "{method}");
        }
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(!is_safe(&method), "{method}");
        }
    }
}
