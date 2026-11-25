use std::collections::HashMap;

use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use bakedays::calendar::PublicDrop;
use bakedays::capacity::Remaining;
use bakedays::{PublicStatus, range};
use catalog::{KitchenMayContain, Pricing};
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

/// Catalog products, and the kitchen-wide may-contain list that every product shows and
/// needs approved before it can be ordered (F10).
pub(crate) struct Shelf {
    pub products: Vec<catalog::Product>,
    pub kitchen: KitchenMayContain,
}

impl Shelf {
    pub fn find(&self, id: ProductId) -> Option<&catalog::Product> {
        self.products.iter().find(|p| p.id == id)
    }
}

/// Catalog products by id; unknown ids are left out.
pub(crate) async fn load_products(
    conn: &mut PgConnection,
    ids: impl IntoIterator<Item = ProductId>,
) -> Result<Shelf, catalog::Error> {
    let mut products = Vec::new();
    for id in ids {
        match catalog::products::get(conn, id).await {
            Ok(product) => products.push(product),
            Err(catalog::Error::NotFound) => {}
            Err(err) => return Err(err),
        }
    }
    let kitchen = catalog::company::kitchen_may_contain(conn).await?;
    Ok(Shelf { products, kitchen })
}

/// The product ids a drop's public menu offers.
pub(crate) fn menu_ids(drop: &PublicDrop) -> Vec<ProductId> {
    drop.menu
        .iter()
        .flatten()
        .map(|item| item.product_id)
        .collect()
}

/// The cart panel for `day`. Prices come from the catalog of this moment; checkout
/// prices and checks everything again. `shelf` holds the cart's products. A line
/// whose product is no longer on `menu` stays visible, marked as unavailable and left
/// out of the total, so the customer can remove it.
pub(crate) fn view(
    state: &AppState,
    day: BakeDayId,
    cart: Option<&Cart>,
    shelf: &Shelf,
    menu: &[ProductId],
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
        let Some(product) = shelf.find(product_id) else {
            continue;
        };
        let orderable = menu.contains(&product_id)
            && product.active
            && product.sold_online
            && product.allergens_approved(&shelf.kitchen);
        let amount_cell = if orderable {
            let unit = line.chosen_price.unwrap_or(product.pricing.price_incl());
            let amount = unit.cents().saturating_mul(i64::from(line.qty));
            total = total.saturating_add(amount);
            Cell::number(i18n::money(amount))
        } else {
            Cell::text(t.t("cart-line-unavailable", None)?)
        };
        rows.push(vec![
            Cell::text(product.name_nl.clone()),
            Cell::number(line.qty.to_string()),
            amount_cell,
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
    drop: &PublicDrop,
    form: &HashMap<String, String>,
) -> Result<Result<Change, String>, Box<Response>> {
    let t = &state.i18n;
    let msg = |id: &str, args: Option<&FluentArgs<'_>>| {
        t.t(id, args)
            .map_err(|err| Box::new(server_error(state, "missing text", &err)))
    };
    let day = drop.bake_day_id;
    let Some(product) = form
        .get("product")
        .and_then(|raw| raw.parse::<i64>().ok())
        .map(ProductId::new)
    else {
        return Err(Box::new(pages::error(state, StatusCode::BAD_REQUEST)));
    };
    let menu = menu_ids(drop);
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
        return Ok(Ok(Change {
            product,
            qty: 0,
            price: None,
        }));
    }
    if drop.status != PublicStatus::Open {
        return Ok(Err(msg("cart-error-closed", None)?));
    }
    if !menu.contains(&product) {
        return Ok(Err(msg("cart-error-unavailable", None)?));
    }
    let shelf = load_products(conn, [product])
        .await
        .map_err(|err| Box::new(server_error(state, "cannot load products", &err)))?;
    let Some(catalog) = shelf.find(product) else {
        return Ok(Err(msg("cart-error-unavailable", None)?));
    };
    if !catalog.allergens_approved(&shelf.kitchen) {
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
    Ok(Ok(Change {
        product,
        qty,
        price,
    }))
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
    let drop = match find_drop(&mut conn, &state, day).await {
        Ok(Some(drop)) => drop,
        Ok(None) => return pages::error(&state, StatusCode::NOT_FOUND),
        Err(err) => return server_error(&state, "cannot load the drop", &err),
    };
    let checked = match check(&mut conn, &state, &drop, &form).await {
        Ok(checked) => checked,
        Err(response) => return *response,
    };
    let mut cart = session_cart(&session).await;
    let message = match checked {
        Err(text) => error(text),
        Ok(change) => {
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
            Message {
                kind: if text_id == "cart-replaced" {
                    "warning"
                } else {
                    "success"
                },
                text,
            }
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
    let shelf = match load_products(&mut conn, cart.iter().flat_map(Cart::products)).await {
        Ok(shelf) => shelf,
        Err(err) => return server_error(&state, "cannot load products", &err),
    };
    let menu = menu_ids(&drop);
    match view(
        &state,
        day,
        cart.as_ref(),
        &shelf,
        &menu,
        true,
        Some(message),
    ) {
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
