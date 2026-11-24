use std::borrow::Cow;
use std::sync::LazyLock;

use axum::extract::{Path, RawQuery, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::pages;
use crate::state::AppState;

pub const TOKENS_CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/tokens.css"));
pub const FONTS_CSS: &str = include_str!("../../../design/fonts.css");
pub const APP_CSS: &str = include_str!("../../../design/app.css");
pub const COMPONENTS_CSS: &str = include_str!("../../../design/components.css");
pub const APP_JS: &str = include_str!("../../../design/app.js");
/// The logo and the icons cut from it, made by `make logo` from docs/media/image1.png.
pub const LOGO_PNG: &[u8] = include_bytes!("../../../design/logo.png");
pub const FAVICON_PNG: &[u8] = include_bytes!("../../../design/favicon.png");
pub const TOUCH_ICON_PNG: &[u8] = include_bytes!("../../../design/apple-touch-icon.png");
/// `HTMX_JS` is the store path of the pinned htmx release (nix/htmx.nix), set by the flake
/// for builds and the dev shell.
pub const HTMX_JS: &str = include_str!(env!(
    "HTMX_JS",
    "HTMX_JS is not set; build inside `nix develop` or with `nix build`"
));
/// `FONTS_DIR` holds the pinned Oswald and Inter woff2 files (nix/fonts.nix).
pub const OSWALD_WOFF2: &[u8] = include_bytes!(concat!(
    env!(
        "FONTS_DIR",
        "FONTS_DIR is not set; build inside `nix develop` or with `nix build`"
    ),
    "/oswald.woff2"
));
pub const INTER_WOFF2: &[u8] = include_bytes!(concat!(
    env!(
        "FONTS_DIR",
        "FONTS_DIR is not set; build inside `nix develop` or with `nix build`"
    ),
    "/inter.woff2"
));

const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const WOFF2: &str = "font/woff2";
const PNG: &str = "image/png";
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub const REVALIDATE: &str = "no-cache";

pub struct Asset {
    /// The path below `/static/`.
    pub name: &'static str,
    pub content_type: &'static str,
    pub body: Cow<'static, [u8]>,
    /// First 16 hex digits of the SHA-256 of `body`.
    pub hash: String,
}

impl Asset {
    fn new(
        name: &'static str,
        content_type: &'static str,
        body: impl Into<Cow<'static, [u8]>>,
    ) -> Self {
        let body = body.into();
        let digest = Sha256::digest(&body);
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

/// The icons, stylesheets and scripts `base.html` links, in order.
pub const LINKED: [&str; 8] = [
    "favicon.png",
    "apple-touch-icon.png",
    "fonts.css",
    "tokens.css",
    "app.css",
    "components.css",
    "htmx.min.js",
    "app.js",
];

pub static ASSETS: LazyLock<Vec<Asset>> = LazyLock::new(|| {
    let fonts = [
        Asset::new("fonts/oswald.woff2", WOFF2, OSWALD_WOFF2),
        Asset::new("fonts/inter.woff2", WOFF2, INTER_WOFF2),
    ];
    let fonts_css = with_font_urls(FONTS_CSS, &fonts);
    let mut assets = vec![
        Asset::new("fonts.css", CSS, fonts_css.into_bytes()),
        Asset::new("tokens.css", CSS, TOKENS_CSS.as_bytes()),
        Asset::new("app.css", CSS, APP_CSS.as_bytes()),
        Asset::new("components.css", CSS, COMPONENTS_CSS.as_bytes()),
        Asset::new("htmx.min.js", JS, HTMX_JS.as_bytes()),
        Asset::new("app.js", JS, APP_JS.as_bytes()),
        Asset::new("logo.png", PNG, LOGO_PNG),
        Asset::new("favicon.png", PNG, FAVICON_PNG),
        Asset::new("apple-touch-icon.png", PNG, TOUCH_ICON_PNG),
    ];
    assets.extend(fonts);
    assets
});

/// design/fonts.css names each font as `url("fonts/NAME")`; this points it at the
/// font's hashed URL, so a new font file also changes the stylesheet's URL.
fn with_font_urls(css: &str, fonts: &[Asset]) -> String {
    fonts.iter().fold(css.to_owned(), |css, font| {
        css.replace(
            &format!("url(\"{}\")", font.name),
            &format!("url(\"{}\")", font.url()),
        )
    })
}

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
        asset.body.clone(),
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
        assert!(TOKENS_CSS.contains("  --color-ink: #111111;\n"));
        assert!(TOKENS_CSS.contains("  --color-paper: #ffffff;\n"));
        assert!(
            !TOKENS_CSS.contains("--color-accent"),
            "style Stempel has no accent"
        );
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
    fn fonts_are_the_pinned_woff2_files_linked_from_fonts_css() {
        let css = std::str::from_utf8(&find("fonts.css").expect("fonts.css").body)
            .expect("utf-8")
            .to_owned();
        for name in ["fonts/oswald.woff2", "fonts/inter.woff2"] {
            let font = find(name).expect(name);
            assert!(font.body.starts_with(b"wOF2"), "{name} is not woff2");
            assert_eq!(font.content_type, "font/woff2");
            assert!(css.contains(&format!("url(\"{}\")", font.url())), "{css}");
        }
        assert_eq!(css.matches("url(\"").count(), 2, "{css}");
        assert!(css.contains("font-family: \"Oswald\""), "{css}");
        assert!(css.contains("font-family: \"Inter\""), "{css}");
    }

    /// Width and height from the PNG header.
    fn png_size(png: &[u8]) -> (u32, u32) {
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG");
        let word = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().expect("4 bytes"));
        (word(16), word(20))
    }

    #[test]
    fn brand_images_are_square_pngs_of_the_expected_size() {
        for (name, side) in [
            ("logo.png", 420),
            ("favicon.png", 64),
            ("apple-touch-icon.png", 180),
        ] {
            let asset = find(name).expect(name);
            assert_eq!(asset.content_type, "image/png", "{name}");
            assert_eq!(png_size(&asset.body), (side, side), "{name}");
        }
    }

    #[test]
    fn every_linked_asset_exists() {
        for name in LINKED {
            assert!(find(name).is_some(), "{name}");
        }
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
