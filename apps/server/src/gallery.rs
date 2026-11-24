//! The component gallery ("Design system and UX"): every component in every state, with
//! long Dutch words. Served only when `GALLERY=true`; the visual tests use this page.

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use i18n::Translator;

use crate::components::{Cell, PickupWindow, Product, SelectOption, Table};
use crate::icons::{ALLERGEN_ICONS, ICONS};
use crate::pages::{self, Layout};
use crate::state::AppState;

#[derive(Template)]
#[template(path = "gallery.html")]
pub struct Gallery<'a> {
    layout: Layout<'a>,
    charities: Vec<SelectOption>,
    products: Vec<Product>,
    allergens: Vec<&'static str>,
    prices: Vec<i64>,
    windows: Vec<PickupWindow>,
    table: Table,
    icons: Vec<&'static str>,
}

#[derive(Template)]
#[template(path = "saved.html")]
struct Saved<'a> {
    layout: Layout<'a>,
}

impl<'a> Gallery<'a> {
    /// # Errors
    /// Fails when a gallery text is missing from the Fluent files.
    pub fn new(layout: Layout<'a>, i18n: &Translator) -> Result<Self, i18n::I18nError> {
        let t = |id: &str| i18n.t(id, None);
        let long = t("gallery-long-word")?;
        let place = t("gallery-pickup-place")?;
        let window = |value: &str,
                      day: &str,
                      time: &str,
                      places_left,
                      selected|
         -> Result<_, i18n::I18nError> {
            Ok(PickupWindow {
                value: value.to_owned(),
                day: t(day)?,
                time: t(time)?,
                place: place.clone(),
                places_left,
                selected,
            })
        };
        Ok(Self {
            layout,
            charities: vec![
                SelectOption {
                    value: "voedselbank".to_string(),
                    label: t("gallery-charity-a")?,
                    selected: false,
                },
                SelectOption {
                    value: "kinderhulp".to_string(),
                    label: t("gallery-charity-b")?,
                    selected: true,
                },
            ],
            products: vec![
                Product {
                    name: long.clone(),
                    description: t("gallery-product-desc")?,
                    price_cents: 650,
                    allergens: vec!["cereals_containing_gluten"],
                    may_contain: vec!["sesame_seeds", "nuts"],
                    sold_out: false,
                },
                Product {
                    name: t("gallery-product-short")?,
                    description: t("gallery-product-short-desc")?,
                    price_cents: 450,
                    allergens: vec!["cereals_containing_gluten", "milk"],
                    may_contain: Vec::new(),
                    sold_out: true,
                },
            ],
            allergens: ALLERGEN_ICONS.iter().map(|(code, _)| *code).collect(),
            prices: vec![0, 450, 123_456, -450],
            windows: vec![
                window(
                    "vr-middag",
                    "gallery-pickup-fri",
                    "gallery-pickup-afternoon",
                    1,
                    false,
                )?,
                window(
                    "za-ochtend",
                    "gallery-pickup-sat",
                    "gallery-pickup-morning",
                    12,
                    true,
                )?,
                window(
                    "zo-ochtend",
                    "gallery-pickup-sun",
                    "gallery-pickup-morning",
                    0,
                    false,
                )?,
            ],
            table: Table {
                id: "bakdag".to_string(),
                caption: t("gallery-table-caption")?,
                head: vec![
                    Cell::text(t("gallery-table-product")?),
                    Cell::number(t("gallery-table-count")?),
                    Cell::number(t("gallery-table-total")?),
                ],
                rows: vec![
                    vec![
                        Cell::text(long),
                        Cell::number("12"),
                        Cell::number(i18n::money(7_800)),
                    ],
                    vec![
                        Cell::text(t("gallery-product-short")?),
                        Cell::number("3"),
                        Cell::number(i18n::money(1_350)),
                    ],
                ],
            },
            icons: ICONS.iter().map(|(name, _)| *name).collect(),
        })
    }
}

/// The confirmation dialog posts here, so it starts a session for the CSRF token.
pub async fn page(State(state): State<AppState>, session: tower_sessions::Session) -> Response {
    let token = match crate::csrf::token(&session).await {
        Ok(token) => token,
        Err(error) => {
            tracing::error!(%error, "cannot start a session");
            return pages::error(&state, StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    match Gallery::new(Layout::new(&state).with_csrf_token(token), &state.i18n) {
        Ok(gallery) => pages::render(&state, StatusCode::OK, &gallery),
        Err(error) => {
            tracing::error!(%error, "cannot build the gallery");
            pages::error(&state, StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn save(State(state): State<AppState>) -> Response {
    pages::render(
        &state,
        StatusCode::OK,
        &Saved {
            layout: Layout::new(&state),
        },
    )
}
