//! The component gallery and the component macros ("Design system and UX").
#![forbid(unsafe_code)]

use askama::Template;
use i18n::Translator;
use server::AppState;
use server::components::Product;
use server::config::Config;
use server::gallery::Gallery;
use server::pages::Layout;

fn state() -> AppState {
    let pool = db::connect_lazy(
        "postgres://unused@127.0.0.1:1/db",
        std::time::Duration::ZERO,
    )
    .expect("lazy pool");
    let config = Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://unused/db".to_string()),
        "GALLERY" => Some("true".to_string()),
        _ => None,
    })
    .expect("config");
    AppState::new(pool, Translator::dutch().expect("Dutch texts"), config)
}

fn dutch(id: &str) -> String {
    Translator::dutch()
        .expect("Dutch texts")
        .t(id, None)
        .expect("message")
}

/// Asset hashes change with every CSS edit; the snapshot is about the markup.
fn without_asset_hashes(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("?v=") {
        out.push_str(&rest[..at]);
        let hash = &rest[at + 3..];
        let len = hash.chars().take_while(char::is_ascii_hexdigit).count();
        assert_eq!(len, 16, "asset hash");
        out.push_str("?v=HASH");
        rest = &hash[len..];
    }
    out.push_str(rest);
    out
}

fn gallery_html(state: &AppState) -> String {
    Gallery::new(
        Layout::new(state).with_csrf_token("test-token".to_string()),
        &state.i18n,
    )
    .expect("gallery texts")
    .render()
    .expect("render")
}

#[tokio::test]
async fn gallery_markup_matches_the_approved_snapshot() {
    let state = state();
    insta::assert_snapshot!("gallery", without_asset_hashes(&gallery_html(&state)));
}

#[tokio::test]
async fn gallery_shows_every_component_and_state() {
    let html = gallery_html(&state());
    for needle in [
        "class=\"button button-primary\"",
        "class=\"button button-secondary\"",
        "class=\"button button-danger\"",
        " disabled",
        "aria-busy=\"true\"",
        "aria-invalid=\"true\"",
        "type=\"email\"",
        "type=\"number\"",
        "inputmode=\"decimal\"",
        "<textarea",
        "<select",
        "type=\"checkbox\"",
        "class=\"product-tile product-tile-sold-out\"",
        "class=\"allergen-badge allergen-badge-may\"",
        "class=\"price\"",
        "type=\"radio\"",
        "<table class=\"table\">",
        "class=\"alert alert-info\" role=\"status\"",
        "class=\"alert alert-success\"",
        "class=\"alert alert-warning\"",
        "class=\"alert alert-error\" role=\"alert\"",
        "class=\"empty-state\"",
        "<dialog class=\"dialog\" id=\"bakdag-annuleren\"",
        "data-dialog-open=\"bakdag-annuleren\"",
        "<input type=\"hidden\" name=\"_csrf\" value=\"test-token\">",
        "<div id=\"bakdag-annuleren-result\" role=\"status\"></div>",
        "<p class=\"saved\">",
        "€\u{a0}1.234,56",
    ] {
        assert!(html.contains(needle), "{needle} missing");
    }
    for id in [
        "gallery-long-word",
        "gallery-dialog-text",
        "saved",
        "pickup-full",
        "product-sold-out",
        "allergen-sulphur_dioxide_and_sulphites",
    ] {
        assert!(html.contains(&dutch(id)), "{id} missing");
    }
}

#[derive(Template)]
#[template(
    source = "{% import \"components/product.html\" as p %}{% call p::product_tile(product) %}{% endcall %}",
    ext = "html"
)]
struct Tile<'a> {
    layout: Layout<'a>,
    product: Product,
}

#[tokio::test]
async fn components_escape_user_text() {
    let state = state();
    let tile = Tile {
        layout: Layout::new(&state),
        product: Product {
            name: "<script>alert(\"x\")</script>".to_string(),
            description: "Brood & \"boter\"".to_string(),
            price_cents: 450,
            allergens: vec!["milk"],
            may_contain: Vec::new(),
            sold_out: false,
        },
    }
    .render()
    .expect("render");
    assert!(!tile.contains("<script>"), "{tile}");
    assert!(tile.contains("&#60;script&#62;"), "{tile}");
    assert!(tile.contains("Brood &#38; &#34;boter&#34;"), "{tile}");
    assert!(tile.contains(&dutch("allergen-milk")), "{tile}");
}
