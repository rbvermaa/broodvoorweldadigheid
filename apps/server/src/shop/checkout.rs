use std::collections::HashMap;

use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use bakedays::calendar::PublicDrop;
use bakedays::{PublicStatus, days};
use domain::{BakeDayId, IdempotencyKey, NewPayment, OrderId, PaymentMethod, PickupSlotId};
use i18n::{Amsterdam, FluentArgs};
use orders::{Cart, CustomerInput, Error as OrderError, Order, OrderStatus, OrderedBy};
use sqlx::PgConnection;
use tower_sessions::Session;

use super::{cart, drop_path, drop_view, find_drop, save_cart, server_error, session_cart};
use crate::components::{self, Message, PickupWindow};
use crate::pages::{self, Layout};
use crate::state::AppState;

#[derive(Template)]
#[template(path = "shop/checkout.html")]
struct CheckoutPage<'a> {
    layout: Layout<'a>,
    drop: components::Drop,
    action: String,
    cart: components::Cart,
    form: Fields,
    windows: Vec<PickupWindow>,
    message: Option<Message>,
}

/// What the customer typed, and a Dutch message per field that is wrong.
#[derive(Default)]
struct Fields {
    name: String,
    email: String,
    phone: String,
    slot: String,
    marketing: bool,
    name_error: String,
    email_error: String,
    phone_error: String,
    slot_error: String,
}

impl Fields {
    fn from_form(form: &HashMap<String, String>) -> Self {
        let get = |key: &str| form.get(key).cloned().unwrap_or_default();
        Self {
            name: get("naam"),
            email: get("email"),
            phone: get("telefoon"),
            slot: get("ophaalmoment"),
            marketing: form.get("nieuwsbrief").is_some_and(|v| v == "ja"),
            ..Self::default()
        }
    }

    fn has_errors(&self) -> bool {
        !(self.name_error.is_empty()
            && self.email_error.is_empty()
            && self.phone_error.is_empty()
            && self.slot_error.is_empty())
    }
}

async fn windows(
    conn: &mut PgConnection,
    state: &AppState,
    drop: &PublicDrop,
    chosen: &str,
) -> Result<Vec<PickupWindow>, String> {
    let slots = days::list_slots(conn, drop.bake_day_id)
        .await
        .map_err(|err| err.to_string())?;
    let zone = Amsterdam::new();
    let now = state.clock.now();
    let mut windows = Vec::new();
    for pickup in &drop.pickups {
        let max = slots
            .iter()
            .find(|slot| slot.id == pickup.slot_id)
            .and_then(|slot| slot.max_orders);
        let places_left = match max {
            Some(max) => {
                let taken = orders::slot_orders(conn, pickup.slot_id, now)
                    .await
                    .map_err(|err| err.to_string())?;
                Some(i64::from(max) - taken)
            }
            None => None,
        };
        let value = pickup.slot_id.get().to_string();
        windows.push(PickupWindow {
            selected: value == chosen,
            value,
            day: state.i18n.long_date(zone.date(pickup.starts_at)),
            time: zone
                .time_range(pickup.starts_at, pickup.ends_at)
                .map_err(|err| err.to_string())?,
            place: format!("{}, {}", pickup.location_name, pickup.address),
            places_left,
        });
    }
    if windows.len() == 1 && chosen.is_empty() {
        windows[0].selected = !windows[0].is_full();
    }
    Ok(windows)
}

async fn render_page(
    state: &AppState,
    conn: &mut PgConnection,
    session: &Session,
    (drop, cart): (&PublicDrop, &Cart),
    form: Fields,
    message: Option<Message>,
    status: StatusCode,
) -> Response {
    let token = match crate::csrf::token(session).await {
        Ok(token) => token,
        Err(err) => return server_error(state, "cannot read the session", &err),
    };
    let products = match cart::load_products(conn, cart.products()).await {
        Ok(products) => products,
        Err(err) => return server_error(state, "cannot load products", &err),
    };
    let windows = match windows(conn, state, drop, &form.slot).await {
        Ok(windows) => windows,
        Err(err) => return server_error(state, "cannot list pickup slots", &err),
    };
    let page = drop_view(state, drop, false).and_then(|view| {
        Ok(CheckoutPage {
            layout: Layout::new(state).with_csrf_token(token),
            drop: view,
            action: format!("{}/bestellen", drop_path(drop.bake_day_id)),
            cart: cart::view(state, drop.bake_day_id, Some(cart), &products, false, None)?,
            form,
            windows,
            message,
        })
    });
    match page {
        Ok(page) => pages::render(state, status, &page),
        Err(err) => server_error(state, "cannot show the checkout", &err),
    }
}

/// The drop and the session's cart for it, or where to send the customer instead.
async fn drop_and_cart(
    state: &AppState,
    conn: &mut PgConnection,
    session: &Session,
    day: BakeDayId,
) -> Result<(PublicDrop, Cart), Box<Response>> {
    let drop = match find_drop(conn, state, day).await {
        Ok(Some(drop)) => drop,
        Ok(None) => return Err(Box::new(pages::error(state, StatusCode::NOT_FOUND))),
        Err(err) => return Err(Box::new(server_error(state, "cannot load the drop", &err))),
    };
    match session_cart(session).await {
        Some(cart) if cart.bake_day() == day && !cart.is_empty() => Ok((drop, cart)),
        _ => Err(Box::new(Redirect::to(&drop_path(day)).into_response())),
    }
}

/// The checkout form (F4): name, email, optional phone, pickup slot and the marketing
/// wish, unticked. A charity drop says who the money goes to (F4c).
pub async fn form(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    session: Session,
) -> Response {
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return server_error(&state, "no database connection", &err),
    };
    let (drop, cart) = match drop_and_cart(&state, &mut conn, &session, BakeDayId::new(id)).await {
        Ok(found) => found,
        Err(response) => return *response,
    };
    let message = message_for_closed(&state, &drop);
    render_page(
        &state,
        &mut conn,
        &session,
        (&drop, &cart),
        Fields::default(),
        message,
        StatusCode::OK,
    )
    .await
}

fn message_for_closed(state: &AppState, drop: &PublicDrop) -> Option<Message> {
    (drop.status != PublicStatus::Open).then(|| Message {
        kind: "warning",
        text: state
            .i18n
            .t("checkout-error-closed", None)
            .unwrap_or_default(),
    })
}

fn text(state: &AppState, id: &str, args: Option<&FluentArgs<'_>>) -> String {
    state.i18n.t(id, args).unwrap_or_else(|err| {
        tracing::error!(%err, id, "missing text");
        String::new()
    })
}

/// Checks the fields the server can judge before the order exists. Name, email and phone
/// are checked again by `orders::checkout`.
fn check_fields(state: &AppState, form: &mut Fields) -> Option<PickupSlotId> {
    if form.name.trim().is_empty() {
        form.name_error = text(state, "checkout-error-name", None);
    }
    let email = form.email.trim();
    if !email
        .split_once('@')
        .is_some_and(|(l, d)| !l.is_empty() && d.contains('.'))
    {
        form.email_error = text(state, "checkout-error-email", None);
    }
    let slot = form.slot.parse::<i64>().ok().map(PickupSlotId::new);
    if slot.is_none() {
        form.slot_error = text(state, "checkout-error-pickup", None);
    }
    slot
}

/// The message for an order the shop refused; field errors go into `form`.
async fn refusal(
    state: &AppState,
    conn: &mut PgConnection,
    error: &OrderError,
    form: &mut Fields,
) -> Option<Message> {
    let error_text = |id: &str| Message {
        kind: "error",
        text: text(state, id, None),
    };
    match error {
        OrderError::Invalid("name") => form.name_error = text(state, "checkout-error-name", None),
        OrderError::Invalid("email") => {
            form.email_error = text(state, "checkout-error-email", None);
        }
        OrderError::Invalid("phone") => {
            form.phone_error = text(state, "checkout-error-phone", None);
        }
        OrderError::SlotNotOnDay => form.slot_error = text(state, "checkout-error-pickup", None),
        OrderError::SlotFull => form.slot_error = text(state, "checkout-error-slot-full", None),
        OrderError::PastDeadline => return Some(error_text("checkout-error-deadline")),
        OrderError::CapacityExceeded {
            product_id,
            remaining,
        } => {
            let name = catalog::products::get(conn, *product_id)
                .await
                .map(|p| p.name_nl)
                .unwrap_or_default();
            let mut args = FluentArgs::new();
            args.set("product", name);
            args.set("n", *remaining);
            return Some(Message {
                kind: "error",
                text: text(state, "checkout-error-capacity", Some(&args)),
            });
        }
        OrderError::Cart(_) => return Some(error_text("checkout-error-unavailable")),
        _ => return Some(error_text("checkout-error-closed")),
    }
    Some(error_text("checkout-error-form"))
}

/// Creates the Mollie payment for a new order and records it. The idempotency key comes
/// from the order id, so a retried create cannot start a second payment.
async fn start_payment(
    state: &AppState,
    conn: &mut PgConnection,
    order: &Order,
) -> Result<String, String> {
    let token = orders::order::status_token(conn, order.id)
        .await
        .map_err(|err| err.to_string())?;
    let mut args = FluentArgs::new();
    args.set("number", order.number.to_string());
    let base = &state.config.public_url;
    let request = NewPayment {
        amount: order.total,
        description: state
            .i18n
            .t("payment-description", Some(&args))
            .map_err(|err| err.to_string())?,
        redirect_url: format!("{base}/bestelling/{token}"),
        webhook_url: format!("{base}/webhooks/mollie"),
        order: order.id,
        methods: PaymentMethod::ALL.to_vec(),
        idempotency_key: Some(
            IdempotencyKey::new(format!("order-{}-payment", order.id.get()))
                .map_err(|err| err.to_string())?,
        ),
    };
    let created = state
        .payments
        .create_payment(&request)
        .await
        .map_err(|err| err.to_string())?;
    orders::payment_started(
        conn,
        state.clock.as_ref(),
        order.id,
        created.id.as_str(),
        order.total,
    )
    .await
    .map_err(|err| err.to_string())?;
    Ok(created.checkout_url)
}

/// Places the order (F4, F4b, F4c) and sends the customer to Mollie (F5). The deadline,
/// stock and prices are checked again here, never trusted from the page (ADR 0011).
pub async fn submit(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    session: Session,
    Form(raw): Form<HashMap<String, String>>,
) -> Response {
    let day = BakeDayId::new(id);
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return server_error(&state, "no database connection", &err),
    };
    let (drop, cart) = match drop_and_cart(&state, &mut conn, &session, day).await {
        Ok(found) => found,
        Err(response) => return *response,
    };
    if let Some(response) = retry_unpaid(&state, &mut conn, &session, (&drop, &cart)).await {
        return response;
    }
    let mut form = Fields::from_form(&raw);
    let slot = check_fields(&state, &mut form);
    let unprocessable = StatusCode::UNPROCESSABLE_ENTITY;
    let Some(slot) = slot.filter(|_| !form.has_errors()) else {
        let message = Some(Message {
            kind: "error",
            text: text(&state, "checkout-error-form", None),
        });
        return render_page(
            &state,
            &mut conn,
            &session,
            (&drop, &cart),
            form,
            message,
            unprocessable,
        )
        .await;
    };
    let customer = CustomerInput {
        email: form.email.clone(),
        name: form.name.clone(),
        phone: Some(form.phone.clone()).filter(|p| !p.trim().is_empty()),
        marketing_opt_in: form.marketing,
    };
    let placed = orders::checkout(
        &mut conn,
        state.clock.as_ref(),
        &cart,
        &customer,
        slot,
        OrderedBy::Guest,
    )
    .await;
    let order = match placed {
        Ok(order) => order,
        Err(OrderError::Db(err)) => return server_error(&state, "checkout failed", &err),
        Err(OrderError::NotFound) => return pages::error(&state, StatusCode::NOT_FOUND),
        Err(
            err @ (OrderError::Corrupt(_)
            | OrderError::Catalog(_)
            | OrderError::BakeDays(_)
            | OrderError::Money(_)
            | OrderError::PaymentOfOtherOrder),
        ) => return server_error(&state, "checkout failed", &err),
        Err(err) => {
            tracing::info!(error = %err, "checkout refused");
            let message = refusal(&state, &mut conn, &err, &mut form).await;
            return render_page(
                &state,
                &mut conn,
                &session,
                (&drop, &cart),
                form,
                message,
                unprocessable,
            )
            .await;
        }
    };
    if let Err(err) = save_cart(&session, &Cart::new(day)).await {
        tracing::warn!(error = %err, "cannot empty the cart");
    }
    match start_payment(&state, &mut conn, &order).await {
        Ok(checkout_url) => Redirect::to(&checkout_url).into_response(),
        Err(err) => {
            tracing::error!(error = %err, order = order.id.get(), "cannot start the payment");
            if let Err(err) = save_cart(&session, &cart).await {
                tracing::warn!(error = %err, "cannot restore the cart");
            }
            if let Err(err) = session.insert(UNPAID_KEY, order.id.get()).await {
                tracing::warn!(error = %err, "cannot remember the unpaid order");
            }
            // tower-sessions does not save a session on a 5xx answer.
            if let Err(err) = session.save().await {
                tracing::warn!(error = %err, "cannot save the session");
            }
            let message = Some(Message {
                kind: "error",
                text: text(&state, "checkout-error-payment", None),
            });
            render_page(
                &state,
                &mut conn,
                &session,
                (&drop, &cart),
                form,
                message,
                StatusCode::SERVICE_UNAVAILABLE,
            )
            .await
        }
    }
}

/// The order whose payment could not be started, so a second try pays that order instead
/// of placing another one and holding the stock twice.
const UNPAID_KEY: &str = "unpaid_order";

/// When the session remembers an order of this drop whose payment did not start and
/// whose hold still runs, tries the payment again. The idempotency key makes Mollie return
/// the same payment if the first try did reach it. `None` lets checkout go on as normal.
async fn retry_unpaid(
    state: &AppState,
    conn: &mut PgConnection,
    session: &Session,
    (drop, cart): (&PublicDrop, &Cart),
) -> Option<Response> {
    let id = match session.get::<i64>(UNPAID_KEY).await {
        Ok(Some(id)) => OrderId::new(id),
        Ok(None) => return None,
        Err(err) => {
            tracing::warn!(error = %err, "cannot read the unpaid order");
            return None;
        }
    };
    let now = state.clock.now();
    let order = orders::order::get(conn, id).await.ok().filter(|order| {
        order.bake_day_id == drop.bake_day_id
            && order.status == OrderStatus::PendingPayment
            && order.hold_expires_at.is_some_and(|hold| now < hold)
    });
    let Some(order) = order else {
        if let Err(err) = session.remove::<i64>(UNPAID_KEY).await {
            tracing::warn!(error = %err, "cannot forget the unpaid order");
        }
        return None;
    };
    match start_payment(state, conn, &order).await {
        Ok(checkout_url) => {
            if let Err(err) = session.remove::<i64>(UNPAID_KEY).await {
                tracing::warn!(error = %err, "cannot forget the unpaid order");
            }
            if let Err(err) = save_cart(session, &Cart::new(drop.bake_day_id)).await {
                tracing::warn!(error = %err, "cannot empty the cart");
            }
            Some(Redirect::to(&checkout_url).into_response())
        }
        Err(err) => {
            tracing::error!(error = %err, order = order.id.get(), "cannot start the payment");
            let message = Some(Message {
                kind: "error",
                text: text(state, "checkout-error-payment", None),
            });
            Some(
                render_page(
                    state,
                    conn,
                    session,
                    (drop, cart),
                    Fields::default(),
                    message,
                    StatusCode::SERVICE_UNAVAILABLE,
                )
                .await,
            )
        }
    }
}
