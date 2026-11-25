use std::collections::HashMap;

use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use bakedays::calendar::PublicDrop;
use bakedays::{BakeDayStatus, PublicStatus, days};
use domain::{
    BakeDayId, CharityId, IdempotencyKey, NewPayment, OrderId, PaymentMethod, PickupSlotId,
};
use i18n::{Amsterdam, FluentArgs};
use orders::donations::{self, DonationChoice};
use orders::{Cart, CustomerInput, Error as OrderError, Order, OrderStatus, OrderedBy};
use sqlx::{Acquire, PgConnection};
use tower_sessions::Session;

use super::{
    cart, drop_path, drop_view, find_drop, load_session, save_cart, server_error, session_cart,
};
use crate::components::{self, Message, PickupWindow, SelectOption};
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
    /// Where the donation goes, on a charity drop (F4d).
    donation: Option<DonationField>,
    message: Option<Message>,
}

struct DonationField {
    options: Vec<SelectOption>,
    hint: String,
}

const DROP_CHARITY: &str = "drop";
const SAVE: &str = "bewaren";

/// What the customer typed, and a Dutch message per field that is wrong.
#[derive(Default)]
struct Fields {
    name: String,
    email: String,
    phone: String,
    slot: String,
    marketing: bool,
    donation: String,
    name_error: String,
    email_error: String,
    phone_error: String,
    slot_error: String,
    donation_error: String,
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
            donation: get("donatie"),
            ..Self::default()
        }
    }

    fn has_errors(&self) -> bool {
        !(self.name_error.is_empty()
            && self.email_error.is_empty()
            && self.phone_error.is_empty()
            && self.slot_error.is_empty()
            && self.donation_error.is_empty())
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

/// The donation select of a charity drop: its own charity first, then the other active
/// charities, then saving it for later. `None` on a normal sale.
async fn donation_field(
    conn: &mut PgConnection,
    state: &AppState,
    drop: &PublicDrop,
    chosen: &str,
) -> Result<Option<DonationField>, String> {
    let (Some(charity), Some(drop_charity)) = (
        drop.charity.as_ref(),
        days::get(conn, drop.bake_day_id)
            .await
            .map_err(|err| err.to_string())?
            .charity_id,
    ) else {
        return Ok(None);
    };
    let chosen = if chosen.is_empty() {
        DROP_CHARITY
    } else {
        chosen
    };
    let option = |value: String, label: String| SelectOption {
        selected: value == chosen,
        value,
        label,
    };
    let mut args = FluentArgs::new();
    args.set("charity", charity.name.clone());
    let mut options = vec![option(
        DROP_CHARITY.to_owned(),
        text(state, "checkout-donation-drop", Some(&args)),
    )];
    for other in catalog::charities::list_active(conn)
        .await
        .map_err(|err| err.to_string())?
    {
        if other.id != drop_charity {
            options.push(option(other.id.get().to_string(), other.name));
        }
    }
    options.push(option(
        SAVE.to_owned(),
        text(state, "checkout-donation-save", None),
    ));
    let mut args = FluentArgs::new();
    args.set("months", donations::SAVE_MONTHS);
    Ok(Some(DonationField {
        options,
        hint: text(state, "checkout-donation-hint", Some(&args)),
    }))
}

/// The customer's donation choice, if the form holds one this drop offers.
async fn donation_choice(
    conn: &mut PgConnection,
    drop: &PublicDrop,
    value: &str,
) -> Result<Option<DonationChoice>, OrderError> {
    if drop.charity.is_none() {
        return Ok(Some(DonationChoice::DropCharity));
    }
    Ok(match value.trim() {
        "" | DROP_CHARITY => Some(DonationChoice::DropCharity),
        SAVE => Some(DonationChoice::Save),
        id => match id.parse::<i64>() {
            Ok(id) => catalog::charities::list_active(conn)
                .await?
                .iter()
                .any(|charity| charity.id.get() == id)
                .then_some(DonationChoice::Charity(CharityId::new(id))),
            Err(_) => None,
        },
    })
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
    let shelf = match cart::load_products(conn, cart.products()).await {
        Ok(shelf) => shelf,
        Err(err) => return server_error(state, "cannot load products", &err),
    };
    let windows = match windows(conn, state, drop, &form.slot).await {
        Ok(windows) => windows,
        Err(err) => return server_error(state, "cannot list pickup slots", &err),
    };
    let donation = match donation_field(conn, state, drop, &form.donation).await {
        Ok(donation) => donation,
        Err(err) => return server_error(state, "cannot list charities", &err),
    };
    let page = drop_view(state, drop, false).and_then(|view| {
        Ok(CheckoutPage {
            layout: Layout::new(state).with_csrf_token(token),
            drop: view,
            action: format!("{}/bestellen", drop_path(drop.bake_day_id)),
            cart: cart::view(
                state,
                drop.bake_day_id,
                Some(cart),
                &shelf,
                &cart::menu_ids(drop),
                false,
                None,
            )?,
            form,
            windows,
            donation,
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
    if let Err(err) = load_session(&session).await {
        return server_error(&state, "cannot start a session", &err);
    }
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
        OrderError::Invalid("charity" | "donation") => {
            form.donation_error = text(state, "checkout-error-donation", None);
        }
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
        OrderError::FreeOrder => return Some(error_text("checkout-error-free")),
        _ => return Some(error_text("checkout-error-closed")),
    }
    Some(error_text("checkout-error-form"))
}

/// Creates the Mollie payment for a new order and records it. The idempotency key comes
/// from the order id, so a retried create cannot start a second payment. The caller
/// holds no pool connection: the Mollie call can take seconds, so each database step
/// takes a connection of its own and gives it back before the call.
async fn start_payment(state: &AppState, order: &Order) -> Result<String, String> {
    let token = {
        let mut conn = state.pool.acquire().await.map_err(|err| err.to_string())?;
        orders::order::status_token(&mut conn, order.id)
            .await
            .map_err(|err| err.to_string())?
    };
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
    let mut conn = state.pool.acquire().await.map_err(|err| err.to_string())?;
    orders::payment_started(
        &mut conn,
        state.clock.as_ref(),
        order.id,
        created.id.as_str(),
        order.total,
    )
    .await
    .map_err(|err| err.to_string())?;
    Ok(created.checkout_url)
}

/// The checkout page with "betalen lukt nu even niet", on a fresh connection.
async fn payment_unavailable(
    state: &AppState,
    session: &Session,
    (drop, cart): (&PublicDrop, &Cart),
    form: Fields,
) -> Response {
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return server_error(state, "no database connection", &err),
    };
    let message = Some(Message {
        kind: "error",
        text: text(state, "checkout-error-payment", None),
    });
    render_page(
        state,
        &mut conn,
        session,
        (drop, cart),
        form,
        message,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await
}

/// Places the order, replacing `old` if given, and stores the donation choice in the same
/// transaction, so a refused choice leaves no order, hold or cancelled order behind.
async fn place(
    conn: &mut PgConnection,
    clock: &dyn domain::Clock,
    (cart, customer, slot): (&Cart, &CustomerInput, PickupSlotId),
    old: Option<OrderId>,
    choice: DonationChoice,
) -> Result<Order, OrderError> {
    let mut tx = conn.begin().await?;
    let order = match old {
        Some(old) => {
            orders::checkout_replacing(&mut tx, clock, cart, customer, slot, OrderedBy::Guest, old)
                .await?
        }
        None => orders::checkout(&mut tx, clock, cart, customer, slot, OrderedBy::Guest).await?,
    };
    donations::choose_at_checkout(&mut tx, order.id, choice).await?;
    tx.commit().await?;
    Ok(order)
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
    let unpaid = match remembered_order(&state, &mut conn, &session, &drop).await {
        Ok(Remembered::Nothing) => None,
        Ok(Remembered::Unpaid(order)) => Some(order),
        Ok(Remembered::Settled(order)) => {
            return to_status_page(&state, &mut conn, &session, &order).await;
        }
        Err(response) => return *response,
    };
    let mut form = Fields::from_form(&raw);
    let slot = check_fields(&state, &mut form);
    let choice = match donation_choice(&mut conn, &drop, &form.donation).await {
        Ok(Some(choice)) => choice,
        Ok(None) => {
            form.donation_error = text(&state, "checkout-error-donation", None);
            DonationChoice::DropCharity
        }
        Err(err) => return server_error(&state, "cannot check the donation choice", &err),
    };
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
    if let Some(unpaid) = &unpaid {
        match orders::same_as_order(&mut conn, unpaid, &cart, &customer, slot).await {
            Ok(true) => {
                match donations::choose_at_checkout(&mut conn, unpaid.id, choice).await {
                    Ok(()) => {}
                    Err(OrderError::WrongStatus(_)) => {
                        return to_status_page(&state, &mut conn, &session, unpaid).await;
                    }
                    Err(OrderError::Invalid(_)) => {
                        form.donation_error = text(&state, "checkout-error-donation", None);
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
                    }
                    Err(err) => {
                        return server_error(&state, "cannot store the donation choice", &err);
                    }
                }
                std::mem::drop(conn);
                return retry_payment(&state, &session, (&drop, &cart), unpaid, form).await;
            }
            Ok(false) => {}
            Err(err) => return server_error(&state, "cannot compare the unpaid order", &err),
        }
    }
    let clock = state.clock.as_ref();
    let placed = place(
        &mut conn,
        clock,
        (&cart, &customer, slot),
        unpaid.as_ref().map(|old| old.id),
        choice,
    )
    .await;
    let order = match placed {
        Ok(order) => order,
        Err(OrderError::NotReplaceable) => {
            let Some(old) = &unpaid else {
                return server_error(&state, "checkout failed", &OrderError::NotReplaceable);
            };
            if let Err(err) = session.remove::<i64>(UNPAID_KEY).await {
                tracing::warn!(error = %err, "cannot forget the unpaid order");
            }
            return to_status_page(&state, &mut conn, &session, old).await;
        }
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
            let message = if unpaid.is_some() && matches!(err, OrderError::PastDeadline) {
                Some(Message {
                    kind: "error",
                    text: text(&state, "checkout-error-replace-deadline", None),
                })
            } else {
                refusal(&state, &mut conn, &err, &mut form).await
            };
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
    if unpaid.is_some()
        && let Err(err) = session.remove::<i64>(UNPAID_KEY).await
    {
        tracing::warn!(error = %err, "cannot forget the replaced order");
    }
    std::mem::drop(conn);
    if let Err(err) = save_cart(&session, &Cart::new(day)).await {
        tracing::warn!(error = %err, "cannot empty the cart");
    }
    match start_payment(&state, &order).await {
        Ok(checkout_url) => Redirect::to(&checkout_url).into_response(),
        Err(err) => {
            tracing::error!(error = %err, order = order.id.get(), "cannot start the payment");
            if let Err(err) = save_cart(&session, &cart).await {
                tracing::warn!(error = %err, "cannot restore the cart");
            }
            if let Err(err) = session.insert(UNPAID_KEY, order.id.get()).await {
                tracing::warn!(error = %err, "cannot remember the unpaid order");
            }
            // tower-sessions does not save a session on a 5xx answer. The store takes its
            // own pool connection, so this runs while the handler holds none.
            if let Err(err) = session.save().await {
                tracing::warn!(error = %err, "cannot save the session");
            }
            payment_unavailable(&state, &session, (&drop, &cart), form).await
        }
    }
}

/// The order whose payment could not be started, so a second try pays that order instead
/// of placing another one and holding the stock twice.
const UNPAID_KEY: &str = "unpaid_order";

/// What became of the order the session remembers.
enum Remembered {
    Nothing,
    /// Still waiting for payment within its hold: the next checkout pays or replaces it.
    Unpaid(Order),
    /// Paid meanwhile, for example in another tab.
    Settled(Order),
}

/// The order the session remembers for this drop. It stays payable while its hold runs;
/// the deadline and stock play no part, since the order was placed in time and its hold
/// keeps its stock (spec, payment within the hold). Only a cancelled or finished bake day
/// ends it early. The session forgets an order that can no longer be paid; a database
/// error is a 500 and forgets nothing.
async fn remembered_order(
    state: &AppState,
    conn: &mut PgConnection,
    session: &Session,
    drop: &PublicDrop,
) -> Result<Remembered, Box<Response>> {
    let id = match session.get::<i64>(UNPAID_KEY).await {
        Ok(Some(id)) => OrderId::new(id),
        Ok(None) => return Ok(Remembered::Nothing),
        Err(err) => {
            tracing::warn!(error = %err, "cannot read the unpaid order");
            return Ok(Remembered::Nothing);
        }
    };
    let order = match orders::order::get(conn, id).await {
        Ok(order) if order.bake_day_id == drop.bake_day_id => Some(order),
        Ok(_) | Err(OrderError::NotFound) => None,
        Err(err) => {
            return Err(Box::new(server_error(
                state,
                "cannot load the unpaid order",
                &err,
            )));
        }
    };
    let now = state.clock.now();
    let remembered = match order {
        Some(order)
            if matches!(
                order.status,
                OrderStatus::Paid | OrderStatus::Confirmed | OrderStatus::PickedUp
            ) =>
        {
            Remembered::Settled(order)
        }
        Some(order)
            if order.status == OrderStatus::PendingPayment
                && order.hold_expires_at.is_some_and(|hold| now < hold) =>
        {
            let day = match days::get(conn, drop.bake_day_id).await {
                Ok(day) => day,
                Err(err) => {
                    return Err(Box::new(server_error(
                        state,
                        "cannot load the bake day",
                        &err,
                    )));
                }
            };
            if matches!(day.status, BakeDayStatus::Cancelled | BakeDayStatus::Done) {
                Remembered::Nothing
            } else {
                return Ok(Remembered::Unpaid(order));
            }
        }
        _ => Remembered::Nothing,
    };
    if let Err(err) = session.remove::<i64>(UNPAID_KEY).await {
        tracing::warn!(error = %err, "cannot forget the unpaid order");
    }
    Ok(remembered)
}

/// Sends the customer to the status page of an order that was paid meanwhile, instead of
/// placing a second one.
async fn to_status_page(
    state: &AppState,
    conn: &mut PgConnection,
    session: &Session,
    order: &Order,
) -> Response {
    let token = match orders::order::status_token(conn, order.id).await {
        Ok(token) => token,
        Err(err) => return server_error(state, "cannot load the status token", &err),
    };
    if let Err(err) = save_cart(session, &Cart::new(order.bake_day_id)).await {
        tracing::warn!(error = %err, "cannot empty the cart");
    }
    Redirect::to(&format!("/bestelling/{token}")).into_response()
}

/// Pays an unpaid order again, when the customer sent the same checkout
/// ([`orders::same_as_order`]). The idempotency key makes Mollie return
/// the same payment if the first try did reach it.
async fn retry_payment(
    state: &AppState,
    session: &Session,
    (drop, cart): (&PublicDrop, &Cart),
    order: &Order,
    form: Fields,
) -> Response {
    match start_payment(state, order).await {
        Ok(checkout_url) => {
            if let Err(err) = session.remove::<i64>(UNPAID_KEY).await {
                tracing::warn!(error = %err, "cannot forget the unpaid order");
            }
            if let Err(err) = save_cart(session, &Cart::new(drop.bake_day_id)).await {
                tracing::warn!(error = %err, "cannot empty the cart");
            }
            Redirect::to(&checkout_url).into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, order = order.id.get(), "cannot start the payment");
            payment_unavailable(state, session, (drop, cart), form).await
        }
    }
}
