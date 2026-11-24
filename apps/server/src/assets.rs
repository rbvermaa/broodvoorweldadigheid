use std::sync::LazyLock;

use axum::extract::{Path, RawQuery, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::pages;
use crate::state::AppState;

pub const TOKENS_CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/tokens.css"));
pub const APP_CSS: &str = include_str!("../../../design/app.css");
pub const APP_JS: &str = include_str!("../../../design/app.js");
/// `HTMX_JS` is the store path of the pinned htmx release (nix/htmx.nix), set by the flake
/// for builds and the dev shell.
pub const HTMX_JS: &str = include_str!(env!(
    "HTMX_JS",
    "HTMX_JS is not set; build inside `nix develop` or with `nix build`"
));

const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub const REVALIDATE: &str = "no-cache";

pub struct Asset {
    pub name: &'static str,
    pub content_type: &'static str,
    pub body: &'static str,
    /// First 16 hex digits of the SHA-256 of `body`.
    pub hash: String,
}

impl Asset {
    fn new(name: &'static str, content_type: &'static str, body: &'static str) -> Self {
        let digest = Sha256::digest(body.as_bytes());
        let hash = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        Self {
            name,
            content_type,
            body,
            hash,
        }
    }

    pub fn url(&self) -> String {
        format!("/static/{}?v={}", self.name, self.hash)
    }
}

pub static ASSETS: LazyLock<[Asset; 4]> = LazyLock::new(|| {
    [
        Asset::new("tokens.css", CSS, TOKENS_CSS),
        Asset::new("app.css", CSS, APP_CSS),
        Asset::new("htmx.min.js", JS, HTMX_JS),
        Asset::new("app.js", JS, APP_JS),
    ]
});

pub fn find(name: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.name == name)
}

/// A request whose `v` matches the content hash may be cached for good: a new deploy
/// changes the hash and so the URL. Any other request must revalidate with the ETag.
pub async fn serve(
    State(state): State<AppState>,
    Path(name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let Some(asset) = find(&name) else {
        return pages::error(&state, StatusCode::NOT_FOUND);
    };
    let etag = HeaderValue::from_str(&format!("\"{}\"", asset.hash))
        .expect("a hex ETag is a valid header value");
    let versioned = query.as_deref().is_some_and(|query| {
        query
            .split('&')
            .any(|pair| pair == format!("v={}", asset.hash))
    });
    let cache = if versioned { IMMUTABLE } else { REVALIDATE };
    let common = [
        (ETAG, etag),
        (CACHE_CONTROL, HeaderValue::from_static(cache)),
    ];
    if etag_matches(&headers, &asset.hash) {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    (
        common,
        [(CONTENT_TYPE, HeaderValue::from_static(asset.content_type))],
        asset.body,
    )
        .into_response()
}

fn etag_matches(headers: &HeaderMap, hash: &str) -> bool {
    headers
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|tag| tag == "*" || tag.trim_start_matches("W/").trim_matches('"') == hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_css_is_generated_from_design_tokens() {
        let expected = crate::tokens::generate(include_str!("../../../design/tokens.toml"))
            .expect("design/tokens.toml is valid");
        assert_eq!(TOKENS_CSS, expected);
    }

    #[test]
    fn tokens_css_has_brand_base_colours_and_fonts() {
        assert!(TOKENS_CSS.contains("  --color-black: #000000;\n"));
        assert!(TOKENS_CSS.contains("  --color-white: #ffffff;\n"));
        assert!(TOKENS_CSS.contains("  --font-heading: \"Oswald\""));
        assert!(TOKENS_CSS.contains("  --font-body: \"Inter\""));
    }

    #[test]
    fn htmx_is_the_pinned_release() {
        assert!(HTMX_JS.starts_with("var htmx=function(){"), "not htmx");
        assert!(
            HTMX_JS.contains("version:\"2.0.11\""),
            "htmx version changed"
        );
    }

    #[test]
    fn asset_hashes_are_16_hex_digits_and_distinct() {
        let mut hashes: Vec<&str> = ASSETS.iter().map(|asset| asset.hash.as_str()).collect();
        for hash in &hashes {
            assert_eq!(hash.len(), 16, "{hash}");
            assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{hash}");
        }
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(hashes.len(), ASSETS.len());
    }

    #[test]
    fn if_none_match_accepts_lists_weak_tags_and_star() {
        let with = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(IF_NONE_MATCH, HeaderValue::from_static(value));
            headers
        };
        assert!(etag_matches(&with("\"abc\""), "abc"));
        assert!(etag_matches(&with("\"x\", W/\"abc\""), "abc"));
        assert!(etag_matches(&with("*"), "abc"));
        assert!(!etag_matches(&with("\"abd\""), "abc"));
        assert!(!etag_matches(&HeaderMap::new(), "abc"));
    }
}
