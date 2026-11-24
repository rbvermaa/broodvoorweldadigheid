use axum::http::HeaderValue;
use axum::http::header::{
    CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::response::Response;

/// No inline scripts or styles: that also enforces the "no inline styles" rule from
/// "Design system and UX" in the browser. `form-action 'self'` needs the Mollie host
/// added once checkout redirects there.
pub const CSP: &str = "default-src 'self'; base-uri 'none'; object-src 'none'; \
    frame-ancestors 'none'; form-action 'self'";

pub async fn headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(
        REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}
