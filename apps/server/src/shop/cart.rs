use std::collections::HashMap;

use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use bakedays::capacity::Remaining;
use bakedays::{PublicStatus, range};
use catalog::Pricing;
use domain::{BakeDayId, Money, PriceRefused, ProductId};
use i18n::FluentArgs;
use orders::cart::MAX_QTY;
use orders::{Cart, Orders};
use sqlx::PgConnection;
use tower_sessions::Session;

use super::money::parse_euros;
use super::{drop_path, find_drop, save_cart, server_error, session_cart};
use crate::components::{self, Cell, Message, Table};
use crate::pages::{self, Layout};
use crate::state::AppState;

#[derive(Template)]
#[template(
    source = "{% import \"components/cart.html\" as c %}{% call c::cart_panel(cart) %}{% endcall %}",
    ext = "html"
)]
struct CartFragment<'a> {
    layout: Layout<'a>,
    cart: components::Cart,
}

/// Catalog products by id; unknown ids are left out.
pub(crate) async fn load_products(
    conn: &mut PgConnection,
    ids: impl IntoIterator<Item = ProductId>,
) -> Result<Vec<catalog::Product>, catalog::Error> {
    let mut products = Vec::new();
    for id in ids {
        match catalog::products::get(conn, id).await {
            Ok(product) => products.push(product),
            Err(catalog::Error::NotFound) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(products)
}

/// The cart panel for `day`. Prices come from the catalog of this moment; checkout
/// prices and checks everything again.
pub(crate) fn view(
    state: &AppState,
    day: BakeDayId,
    cart: Option<&Cart>,
    products: &[catalog::Product],
    can_check_out: bool,
    message: Option<Message>,
) -> Result<components::Cart, i18n::I18nError> {
    let t = &state.i18n;
    let mut rows = Vec::new();
    let mut removes = Vec::new();
    let mut total: i64 = 0;
    for (product_id, line) in cart
        .filter(|c| c.bake_day() == day)
        .into_iter()
        .flat_map(Cart::lines)
    {
        let Some(product) = products.iter().find(|p| p.id == product_id) else {
            continue;
        };
        let unit = line.chosen_price.unwrap_or(product.pricing.price_incl());
        let amount = unit.cents().saturating_mul(i64::from(line.qty));
        total = total.saturating_add(amount);
        rows.push(vec![
            Cell::text(product.name_nl.clone()),
            Cell::number(line.qty.to_string()),
            Cell::number(i18n::money(amount)),
        ]);
        let mut args = FluentArgs::new();
        args.set("product", product.name_nl.clone());
        removes.push(components::CartRemove {
            product_id: product_id.get(),
            label: t.t("cart-remove", Some(&args))?,
        });
    }
    let checkout = if can_check_out && !rows.is_empty() {
        format!("{}/afrekenen", drop_path(day))
    } else {
        String::new()
    };
    Ok(components::Cart {
        action: format!("{}/mandje", drop_path(day)),
        lines: Table {
            id: "mandje-regels".to_string(),
            caption: t.t("checkout-summary", None)?,
            head: vec![
                Cell::text(t.t("cart-product", None)?),
                Cell::number(t.t("product-qty", None)?),
                Cell::number(t.t("order-total", None)?),
            ],
            rows,
        },
        total_cents: total,
        removes,
        checkout,
        message,
    })
}

fn error(text: String) -> Message {
    Message {
        kind: "error",
        text,
    }
}

struct Change {
    product: ProductId,
    qty: u32,
    price: Option<Money>,
}

/// Checks one add, change or remove against the drop's menu and stock. `Err` holds the
/// Dutch message for the customer.
async fn check(
    conn: &mut PgConnection,
    state: &AppState,
    day: BakeDayId,
    form: &HashMap<String, String>,
) -> Result<Result<(Change, Vec<catalog::Product>), String>, Box<Response>> {
    let t = &state.i18n;
    let msg = |id: &str, args: Option<&FluentArgs<'_>>| {
        t.t(id, args)
            .map_err(|err| Box::new(server_error(state, "missing text", &err)))
    };
    let drop = match find_drop(conn, state, day).await {
        Ok(Some(drop)) => drop,
        Ok(None) => return Err(Box::new(pages::error(state, StatusCode::NOT_FOUND))),
        Err(err) => return Err(Box::new(server_error(state, "cannot load the drop", &err))),
    };
    let Some(product) = form
        .get("product")
        .and_then(|raw| raw.parse::<i64>().ok())
        .map(ProductId::new)
    else {
        return Err(Box::new(pages::error(state, StatusCode::BAD_REQUEST)));
    };
    let on_menu = drop
        .menu
        .as_ref()
        .is_some_and(|menu| menu.iter().any(|item| item.product_id == product));
    let qty_raw = form
        .get(&format!("aantal-{}", product.get()))
        .map(|q| q.trim())
        .unwrap_or_default();
    let qty = match qty_raw.parse::<u32>() {
        Ok(qty) if qty <= MAX_QTY => qty,
        _ => {
            let mut args = FluentArgs::new();
            args.set("max", MAX_QTY);
            return Ok(Err(msg("cart-error-qty", Some(&args))?));
        }
    };
    if qty == 0 {
        let menu_ids: Vec<_> = drop
            .menu
            .iter()
            .flatten()
            .map(|item| item.product_id)
            .collect();
        let products = load_products(conn, menu_ids)
            .await
            .map_err(|err| Box::new(server_error(state, "cannot load products", &err)))?;
        return Ok(Ok((
            Change {
                product,
                qty: 0,
                price: None,
            },
            products,
        )));
    }
    if drop.status != PublicStatus::Open {
        return Ok(Err(msg("cart-error-closed", None)?));
    }
    if !on_menu {
        return Ok(Err(msg("cart-error-unavailable", None)?));
    }
    let menu_ids: Vec<_> = drop
        .menu
        .iter()
        .flatten()
        .map(|item| item.product_id)
        .collect();
    let products = load_products(conn, menu_ids)
        .await
        .map_err(|err| Box::new(server_error(state, "cannot load products", &err)))?;
    let Some(catalog) = products.iter().find(|p| p.id == product) else {
        return Ok(Err(msg("cart-error-unavailable", None)?));
    };
    if catalog.approval.is_none() {
        return Ok(Err(msg("cart-error-unavailable", None)?));
    }
    let price = match catalog.pricing {
        Pricing::Fixed(_) => None,
        Pricing::PayWhatYouWant(pwyw) => {
            let raw = form
                .get(&format!("prijs-{}", product.get()))
                .map(String::as_str)
                .unwrap_or_default();
            let Some(chosen) = parse_euros(raw) else {
                return Ok(Err(msg("cart-error-price", None)?));
            };
            match pwyw.check(chosen) {
                Ok(price) => Some(price),
                Err(PriceRefused::BelowMinimum { min, .. }) => {
                    let mut args = FluentArgs::new();
                    args.set("min", i18n::money(min.cents()));
                    return Ok(Err(msg("cart-error-price-low", Some(&args))?));
                }
                Err(PriceRefused::AboveMaximum { max, .. }) => {
                    let mut args = FluentArgs::new();
                    args.set("max", i18n::money(max.cents()));
                    return Ok(Err(msg("cart-error-price-high", Some(&args))?));
                }
            }
        }
    };
    let clock = state.clock.as_ref();
    let left = range::availability(conn, day, &Orders::new(clock))
        .await
        .map_err(|err| Box::new(server_error(state, "cannot read the stock", &err)))?
        .into_iter()
        .find(|a| a.product_id == product)
        .map(|a| a.remaining);
    if let Some(Remaining::Pieces(n)) = left
        && qty > n
    {
        let mut args = FluentArgs::new();
        args.set("n", n);
        return Ok(Err(msg("cart-error-capacity", Some(&args))?));
    }
    Ok(Ok((
        Change {
            product,
            qty,
            price,
        },
        products,
    )))
}

/// Adds, changes or removes one product (F4, F4b). htmx gets the cart panel back; a
/// plain form post goes back to the drop's page.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    session: Session,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let day = BakeDayId::new(id);
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return server_error(&state, "no database connection", &err),
    };
    let checked = match check(&mut conn, &state, day, &form).await {
        Ok(checked) => checked,
        Err(response) => return *response,
    };
    let mut cart = session_cart(&session).await;
    let (message, products) = match checked {
        Err(text) => {
            let ids: Vec<_> = cart.iter().flat_map(Cart::products).collect();
            let products = match load_products(&mut conn, ids).await {
                Ok(products) => products,
                Err(err) => return server_error(&state, "cannot load products", &err),
            };
            (error(text), products)
        }
        Ok((change, products)) => {
            let mut text_id = if change.qty == 0 {
                "cart-removed"
            } else {
                "cart-added"
            };
            let mut next = match cart.take() {
                Some(existing) if existing.bake_day() == day => existing,
                Some(other_drop) if change.qty == 0 => other_drop,
                Some(_) => {
                    text_id = "cart-replaced";
                    Cart::new(day)
                }
                None => Cart::new(day),
            };
            if next.bake_day() == day
                && let Err(err) = next.set(change.product, change.qty, change.price)
            {
                return server_error(&state, "cart refused a checked quantity", &err);
            }
            if let Err(err) = save_cart(&session, &next).await {
                return server_error(&state, "cannot save the cart", &err);
            }
            cart = Some(next);
            let text = match state.i18n.t(text_id, None) {
                Ok(text) => text,
                Err(err) => return server_error(&state, "missing text", &err),
            };
            (
                Message {
                    kind: if text_id == "cart-replaced" {
                        "warning"
                    } else {
                        "success"
                    },
                    text,
                },
                products,
            )
        }
    };
    let failed = message.kind == "error";
    if !headers.contains_key("hx-request") {
        return if failed {
            super::drops::menu_page(&state, &mut conn, &session, day, Some(message)).await
        } else {
            Redirect::to(&drop_path(day)).into_response()
        };
    }
    let token = match crate::csrf::token(&session).await {
        Ok(token) => token,
        Err(err) => return server_error(&state, "cannot read the session", &err),
    };
    match view(&state, day, cart.as_ref(), &products, true, Some(message)) {
        Ok(panel) => pages::render(
            &state,
            StatusCode::OK,
            &CartFragment {
                layout: Layout::new(&state).with_csrf_token(token),
                cart: panel,
            },
        ),
        Err(err) => server_error(&state, "cannot build the cart", &err),
    }
}
