use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use bakedays::calendar::public_drops;
use bakedays::capacity::Remaining;
use bakedays::{PublicStatus, range};
use catalog::Pricing;
use domain::BakeDayId;
use i18n::FluentArgs;
use jiff::ToSpan;
use orders::Orders;
use sqlx::PgConnection;
use tower_sessions::Session;

use super::money::field_value;
use super::{
    cart, drop_path, drop_view, find_drop, load_session, server_error, session_cart, today,
};
use crate::components::{self, ApprovedAllergens, Message, PriceChoice, ProductOrder};
use crate::pages::{self, Layout};
use crate::state::AppState;

const CALENDAR_WEEKS: i32 = 12;
const ICAL_DAYS_BACK: i32 = 31;
const ICAL_DAYS_AHEAD: i32 = 366;

#[derive(Template)]
#[template(path = "shop/drops.html")]
struct Calendar<'a> {
    layout: Layout<'a>,
    drops: Vec<components::Drop>,
}

#[derive(Template)]
#[template(path = "shop/drop.html")]
struct DropPage<'a> {
    layout: Layout<'a>,
    drop: components::Drop,
    /// `None` until the menu is published.
    menu: Option<Vec<components::Product>>,
    orderable: bool,
    cart: components::Cart,
}

/// The public drop calendar (F1a): the coming weeks, planned drops included.
pub async fn calendar(State(state): State<AppState>) -> Response {
    let from = today(&state);
    let to = from.checked_add(CALENDAR_WEEKS.weeks()).unwrap_or(from);
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return server_error(&state, "no database connection", &err),
    };
    let clock = state.clock.as_ref();
    let drops = match public_drops(&mut conn, clock, from, to, &Orders::new(clock)).await {
        Ok(drops) => drops,
        Err(err) => return server_error(&state, "cannot list drops", &err),
    };
    let views: Result<Vec<_>, _> = drops
        .iter()
        .map(|drop| drop_view(&state, drop, true))
        .collect();
    match views {
        Ok(drops) => pages::render(
            &state,
            StatusCode::OK,
            &Calendar {
                layout: Layout::new(&state),
                drops,
            },
        ),
        Err(err) => server_error(&state, "cannot show drops", &err),
    }
}

/// The iCal feed of the public drops (F1a), from a month back to a year ahead.
pub async fn ical(State(state): State<AppState>) -> Response {
    let today = today(&state);
    let from = today.checked_sub(ICAL_DAYS_BACK.days()).unwrap_or(today);
    let to = today.checked_add(ICAL_DAYS_AHEAD.days()).unwrap_or(today);
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return server_error(&state, "no database connection", &err),
    };
    let clock = state.clock.as_ref();
    let drops = match public_drops(&mut conn, clock, from, to, &Orders::new(clock)).await {
        Ok(drops) => drops,
        Err(err) => return server_error(&state, "cannot list drops", &err),
    };
    match bakedays::ical::render(&drops, &state.i18n, state.config.public_host(), clock.now()) {
        Ok(body) => (
            [
                (CONTENT_TYPE, "text/calendar; charset=utf-8"),
                (CACHE_CONTROL, "public, max-age=900"),
            ],
            body,
        )
            .into_response(),
        Err(err) => server_error(&state, "cannot write the iCal feed", &err),
    }
}

/// A drop's page: its facts, the menu once published, and the cart (F1a, F4, F4b, F4c).
pub async fn menu(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    session: Session,
) -> Response {
    if let Err(err) = load_session(&session).await {
        return server_error(&state, "cannot start a session", &err);
    }
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return server_error(&state, "no database connection", &err),
    };
    menu_page(&state, &mut conn, &session, BakeDayId::new(id), None).await
}

fn pwyw_hint(state: &AppState, pwyw: &domain::PayWhatYouWant) -> Result<String, i18n::I18nError> {
    let mut args = FluentArgs::new();
    args.set("min", i18n::money(pwyw.min().cents()));
    args.set("max", i18n::money(pwyw.max().cents()));
    args.set("suggested", i18n::money(pwyw.suggested().cents()));
    state.i18n.t("product-pwyw-hint", Some(&args))
}

pub(crate) async fn menu_page(
    state: &AppState,
    conn: &mut PgConnection,
    session: &Session,
    day: BakeDayId,
    message: Option<Message>,
) -> Response {
    let token = match crate::csrf::token(session).await {
        Ok(token) => token,
        Err(err) => return server_error(state, "cannot start a session", &err),
    };
    let drop = match find_drop(conn, state, day).await {
        Ok(Some(drop)) => drop,
        Ok(None) => return pages::error(state, StatusCode::NOT_FOUND),
        Err(err) => return server_error(state, "cannot load the drop", &err),
    };
    let orderable = drop.status == PublicStatus::Open;
    let menu_ids = cart::menu_ids(&drop);
    let products = match cart::load_products(conn, menu_ids.iter().copied()).await {
        Ok(products) => products,
        Err(err) => return server_error(state, "cannot load products", &err),
    };
    let clock = state.clock.as_ref();
    let stock = if drop.menu.is_some() {
        match range::availability(conn, day, &Orders::new(clock)).await {
            Ok(stock) => stock,
            Err(err) => return server_error(state, "cannot read the stock", &err),
        }
    } else {
        Vec::new()
    };
    let cart = session_cart(session).await.filter(|c| c.bake_day() == day);
    let action = format!("{}/mandje", drop_path(day));
    let tiles: Result<Option<Vec<_>>, i18n::I18nError> = drop
        .menu
        .as_ref()
        .map(|menu| {
            menu.iter()
                .filter_map(|item| products.iter().find(|p| p.id == item.product_id))
                .map(|product| {
                    let remaining =
                        stock
                            .iter()
                            .find(|a| a.product_id == product.id)
                            .and_then(|a| match a.remaining {
                                Remaining::Pieces(n) => Some(i64::from(n)),
                                Remaining::Unlimited => None,
                            });
                    let in_cart = cart
                        .as_ref()
                        .and_then(|c| c.lines().find(|(id, _)| *id == product.id))
                        .map(|(_, line)| line);
                    let allergens = ApprovedAllergens::from_catalog(product);
                    let order = if orderable && allergens.is_some() {
                        let price = match product.pricing {
                            Pricing::Fixed(_) => None,
                            Pricing::PayWhatYouWant(pwyw) => Some(PriceChoice {
                                value: field_value(
                                    in_cart
                                        .and_then(|line| line.chosen_price)
                                        .unwrap_or(pwyw.suggested()),
                                ),
                                hint: pwyw_hint(state, &pwyw)?,
                            }),
                        };
                        Some(ProductOrder {
                            action: action.clone(),
                            product_id: product.id.get(),
                            qty: in_cart.map_or(1, |line| line.qty).to_string(),
                            price,
                            error: String::new(),
                        })
                    } else {
                        None
                    };
                    Ok(components::Product {
                        name: product.name_nl.clone(),
                        description: product.description_nl.clone(),
                        price_cents: product.pricing.price_incl().cents(),
                        allergens,
                        sold_out: remaining == Some(0),
                        remaining,
                        order,
                    })
                })
                .collect()
        })
        .transpose();
    let tiles = match tiles {
        Ok(tiles) => tiles,
        Err(err) => return server_error(state, "cannot show the menu", &err),
    };
    let cart_products =
        match cart::load_products(conn, cart.iter().flat_map(orders::Cart::products)).await {
            Ok(products) => products,
            Err(err) => return server_error(state, "cannot load products", &err),
        };
    let status = if message.as_ref().is_some_and(|m| m.kind == "error") {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let page = drop_view(state, &drop, false).and_then(|view| {
        Ok(DropPage {
            layout: Layout::new(state).with_csrf_token(token),
            drop: view,
            menu: tiles,
            orderable,
            cart: cart::view(
                state,
                day,
                cart.as_ref(),
                &cart_products,
                &menu_ids,
                orderable,
                message,
            )?,
        })
    });
    match page {
        Ok(page) => pages::render(state, status, &page),
        Err(err) => server_error(state, "cannot show the drop", &err),
    }
}
