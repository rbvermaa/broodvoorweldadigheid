//! The component gallery ("Design system and UX"): every component in every state, with
//! long Dutch words. Served only when `GALLERY=true`; the visual tests use this page.

use askama::Template;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use i18n::Translator;

use crate::backoffice::SearchBox;
use crate::components::{
    ApprovedAllergens, BarChart, Cart, CartRemove, Cell, Drop, DropCharity, DropOrdering,
    DropPickup, DropStatus, Message, PickupWindow, PriceChoice, Product, ProductOrder,
    SelectOption, Table,
};
use crate::icons::{ALLERGEN_ICONS, ICONS};
use crate::pages::{self, Layout};
use crate::state::AppState;

#[derive(Template)]
#[template(path = "gallery.html")]
pub struct Gallery<'a> {
    layout: Layout<'a>,
    charities: Vec<SelectOption>,
    locations: Vec<SelectOption>,
    products: Vec<Product>,
    allergens: Vec<&'static str>,
    prices: Vec<i64>,
    windows: Vec<PickupWindow>,
    table: Table,
    search: SearchBox,
    chart: BarChart,
    drops: Vec<Drop>,
    carts: Vec<Cart>,
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
        let with = |id: &str, name: &str, value: String| {
            let mut args = i18n::FluentArgs::new();
            args.set(name, value);
            i18n.t(id, Some(&args))
        };
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
                places_left: Some(places_left),
                selected,
            })
        };
        let mut query = i18n::FluentArgs::new();
        query.set("q", "crème");
        let search = SearchBox {
            action: "/gallery".to_owned(),
            q: "crème".to_owned(),
            none: i18n.t("search-none", Some(&query))?,
            found: String::new(),
        };
        Ok(Self {
            layout,
            search,
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
            locations: vec![SelectOption {
                value: "kerkweg".to_string(),
                label: place.clone(),
                selected: false,
            }],
            products: vec![
                Product {
                    name: long.clone(),
                    description: t("gallery-product-desc")?,
                    price_cents: 650,
                    allergens: Some(ApprovedAllergens::example(
                        vec!["cereals_containing_gluten"],
                        vec!["sesame_seeds", "nuts"],
                        t("gallery-product-ingredients")?,
                    )),
                    sold_out: false,
                    remaining: Some(3),
                    order: Some(ProductOrder {
                        action: "/gallery".to_string(),
                        product_id: 1,
                        qty: "1".to_string(),
                        price: Some(PriceChoice {
                            value: "6,50".to_string(),
                            hint: t("gallery-price-hint")?,
                        }),
                        error: String::new(),
                    }),
                },
                Product {
                    name: t("gallery-product-short")?,
                    description: t("gallery-product-short-desc")?,
                    price_cents: 450,
                    allergens: Some(ApprovedAllergens::example(
                        vec!["cereals_containing_gluten", "milk"],
                        Vec::new(),
                        String::new(),
                    )),
                    sold_out: true,
                    remaining: Some(0),
                    order: None,
                },
                Product {
                    name: t("gallery-product-short")?,
                    description: t("gallery-product-short-desc")?,
                    price_cents: 450,
                    allergens: Some(ApprovedAllergens::example(
                        vec!["milk"],
                        Vec::new(),
                        String::new(),
                    )),
                    sold_out: false,
                    remaining: None,
                    order: Some(ProductOrder {
                        action: "/gallery".to_string(),
                        product_id: 2,
                        qty: "0".to_string(),
                        price: None,
                        error: t("gallery-amount-error")?,
                    }),
                },
                Product {
                    name: t("gallery-product-short")?,
                    description: t("gallery-product-short-desc")?,
                    price_cents: 450,
                    allergens: None,
                    sold_out: false,
                    remaining: None,
                    order: None,
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
            chart: BarChart::new(
                "giften",
                t("gallery-chart-caption")?,
                vec![
                    (long.clone(), 12_500, i18n::money(12_500)),
                    (t("gallery-product-short")?, 4_250, i18n::money(4_250)),
                    (t("gallery-chart-empty")?, 0, i18n::money(0)),
                ],
            ),
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
                        Cell::link(long.clone(), "#bakdag-caption"),
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
            drops: vec![
                Drop {
                    date: t("gallery-drop-date")?,
                    heading: t("gallery-drop-heading")?,
                    datetime: "2026-12-05".to_string(),
                    status: Some(DropStatus {
                        text: t("drop-status-open")?,
                        open: true,
                    }),
                    pickups: vec![DropPickup {
                        when: t("gallery-pickup-morning")?,
                        datetime: "2026-12-05T10:00+01:00".to_string(),
                        location: t("gallery-drop-place")?,
                        address: place.clone(),
                    }],
                    ordering: Some(DropOrdering {
                        label: t("drop-label-order-until")?,
                        when: t("gallery-drop-until")?,
                        text: with("drop-order-until", "when", t("gallery-drop-until")?)?,
                        datetime: "2026-12-03T20:00+01:00".to_string(),
                    }),
                    charity: Some(DropCharity {
                        name: t("gallery-charity-a")?,
                        list_text: with("drop-for-charity", "charity", t("gallery-charity-a")?)?,
                        description: t("gallery-drop-charity")?,
                        url: "https://example.org".to_string(),
                    }),
                    link: "/gallery".to_string(),
                },
                Drop {
                    date: t("gallery-drop-date-year")?,
                    heading: long.clone(),
                    datetime: "2027-01-09".to_string(),
                    status: None,
                    pickups: vec![
                        DropPickup {
                            when: t("gallery-pickup-afternoon")?,
                            datetime: "2027-01-09T14:00+01:00".to_string(),
                            location: long.clone(),
                            address: place.clone(),
                        },
                        DropPickup {
                            when: t("gallery-pickup-morning")?,
                            datetime: "2027-01-10T10:00+01:00".to_string(),
                            location: t("gallery-drop-place")?,
                            address: place.clone(),
                        },
                    ],
                    ordering: Some(DropOrdering {
                        label: t("drop-label-order-from")?,
                        when: t("gallery-drop-from")?,
                        text: with("drop-order-from", "when", t("gallery-drop-from")?)?,
                        datetime: "2027-01-04T09:00+01:00".to_string(),
                    }),
                    charity: None,
                    link: "/gallery".to_string(),
                },
            ],
            carts: vec![Cart {
                action: "/gallery".to_string(),
                lines: Table {
                    id: "mandje-voorbeeld".to_string(),
                    caption: t("gallery-table-caption")?,
                    head: vec![
                        Cell::text(t("gallery-table-product")?),
                        Cell::number(t("gallery-table-count")?),
                        Cell::number(t("gallery-table-total")?),
                    ],
                    rows: vec![vec![
                        Cell::text(long.clone()),
                        Cell::number("2"),
                        Cell::number(i18n::money(1_300)),
                    ]],
                },
                total_cents: 1_300,
                removes: vec![CartRemove {
                    product_id: 1,
                    label: t("gallery-cart-remove")?,
                }],
                checkout: "/gallery".to_string(),
                message: Some(Message {
                    kind: "success",
                    text: t("gallery-alert-success")?,
                }),
            }],
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

/// htmx gets the confirmation fragment; a plain form post goes back to the page.
pub async fn save(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !headers.contains_key("hx-request") {
        return Redirect::to("/gallery").into_response();
    }
    pages::render(
        &state,
        StatusCode::OK,
        &Saved {
            layout: Layout::new(&state),
        },
    )
}
