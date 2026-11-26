//! Orders per bake day, order details with the audit history, and the staff actions on an
//! order: another ophaalmoment, picked up, cancel with refund and retrying a refund (F6,
//! ADR 0025, ADR 0028).

use std::collections::BTreeMap;

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use bakedays::{BakeDay, days, pickup};
use domain::{OrderId, PickupLocationId, PickupSlotId, RefundId, StaffUserId};
use jobs::order_emails;
use orders::audit::{self, Logged};
use orders::manage::{self, OrderFilter};
use orders::{OrderDetails, OrderNumber, OrderStatus, Payment, Refund, RefundState};
use serde::Deserialize;
use sqlx::Acquire;
use tower_sessions::Session;

use super::bake_days::{self, alert_fragment, day_path, status_message};
use super::form::Fields;
use super::{
    Flash, Outcome, Rejection, Search, SearchBox, Step, fail, invalid_status, is_htmx, layout,
    not_found, path_id, require, saved, t, t_with,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/staff/orders", get(days_list))
        .route("/staff/bake-days/{id}/orders", get(list))
        .route("/staff/orders/{id}", get(show))
        .route("/staff/orders/{id}/pickup", post(move_pickup))
        .route("/staff/orders/{id}/picked-up", post(picked_up))
        .route("/staff/orders/{id}/cancel", post(cancel))
        .route(
            "/staff/orders/{id}/refunds/{refund}/retry",
            post(retry_refund),
        )
}

const DAYS_BACK: i32 = 28;
const DAYS_AHEAD: i32 = 366;

const fn status_text(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::PendingPayment => "staff-order-status-pending_payment",
        OrderStatus::Paid => "staff-order-status-paid",
        OrderStatus::Confirmed => "staff-order-status-confirmed",
        OrderStatus::PickedUp => "staff-order-status-picked_up",
        OrderStatus::Cancelled => "staff-order-status-cancelled",
        OrderStatus::Refunded => "staff-order-status-refunded",
    }
}

fn action_text(state: &AppState, action: &str) -> String {
    let id = match action {
        "created" => "staff-order-action-created",
        "paid" => "staff-order-action-paid",
        "payment_refused" => "staff-order-action-payment_refused",
        "payment_status" => "staff-order-action-payment_status",
        "hold_expired" => "staff-order-action-hold_expired",
        "cancelled" => "staff-order-action-cancelled",
        "picked_up" => "staff-order-action-picked_up",
        "picked_up_undone" => "staff-order-action-picked_up_undone",
        "pickup_moved" => "staff-order-action-pickup_moved",
        "invoiced" => "staff-order-action-invoiced",
        "refund_done" => "staff-order-action-refund_done",
        "refund_failed" => "staff-order-action-refund_failed",
        other => return other.to_owned(),
    };
    t(state, id)
}

const fn refund_state_text(refund: RefundState) -> &'static str {
    match refund {
        RefundState::Requested => "staff-order-refund-requested",
        RefundState::Done => "staff-order-refund-done",
        RefundState::Failed => "staff-order-refund-failed",
    }
}

/// What the customer sees on the refund at their bank.
pub fn refund_description(state: &AppState, number: OrderNumber) -> String {
    t_with(
        state,
        "refund-description",
        &[("number", number.to_string())],
    )
}

/// Sends `refunds` to the payment provider one by one. Runs after the cancel committed, so
/// a failure is logged and kept on the refund for staff to retry, never shown as an error.
pub async fn send_refunds(state: &AppState, refunds: &[Refund]) -> Step<()> {
    if refunds.is_empty() {
        return Ok(());
    }
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    for refund in refunds {
        let sent = orders::issue(
            &mut conn,
            state.payments.as_ref(),
            state.clock.as_ref(),
            refund.id,
            &refund_description(state, refund.order_number),
        )
        .await;
        match sent {
            Ok(sent) if sent.state == RefundState::Done => {}
            Ok(sent) => {
                tracing::warn!(refund = refund.id.get(), error = ?sent.last_error, "refund not sent");
            }
            Err(error) => {
                tracing::error!(refund = refund.id.get(), %error, "refund not sent");
            }
        }
    }
    Ok(())
}

pub fn order_path(id: OrderId) -> String {
    format!("/staff/orders/{}", id.get())
}

/// How staff see a customer's name or address; an erased customer (F12) shows a label.
pub fn customer_name(state: &AppState, erased: bool, text: &str) -> String {
    if erased {
        t(state, "customer-erased")
    } else {
        text.to_owned()
    }
}

pub fn customer_cell(state: &AppState, erased: bool, text: &str) -> Cell {
    if erased {
        Cell::muted(t(state, "customer-erased"))
    } else {
        Cell::text(text)
    }
}

fn day_orders_path(day: &BakeDay) -> String {
    format!("{}/orders", day_path(day.id))
}

fn when(at: jiff::Timestamp) -> String {
    i18n::Amsterdam::new()
        .local(at)
        .strftime("%d-%m-%Y %H:%M")
        .to_string()
}

#[derive(Template)]
#[template(path = "staff/orders_days.html")]
struct DaysPage<'a> {
    layout: Layout<'a>,
    table: Table,
    search: SearchBox,
}

/// The bake days of the last four weeks and ahead, each linking to its orders.
pub async fn days_list(
    State(state): State<AppState>,
    staff: Staff,
    Query(search): Query<Search>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewOrders)?;
        let today = state.clock.now_in_bakery().date();
        let from = today.saturating_sub(jiff::Span::new().days(DAYS_BACK));
        let to = today.saturating_add(jiff::Span::new().days(DAYS_AHEAD));
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let list = days::list_between(&mut conn, from, to)
            .await
            .map_err(|e| fail(&state, &e))?;
        drop(conn);
        let rows = list
            .iter()
            .filter(|day| {
                search.matches(&[
                    &state.i18n.long_date(day.date),
                    &i18n::numeric_date(day.date),
                ])
            })
            .map(|day| {
                vec![
                    Cell::link(state.i18n.long_date(day.date), day_orders_path(day)),
                    Cell::text(t(&state, status_message(day.status))),
                ]
            })
            .collect();
        let table = Table {
            id: "bakdagen".to_owned(),
            caption: t(&state, "staff-orders-days-caption"),
            head: vec![
                Cell::text(t(&state, "day-date")),
                Cell::text(t(&state, "day-status")),
            ],
            rows,
        };
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &DaysPage {
                layout: Layout::new(&state),
                search: search.view(&state, "/staff/orders", Vec::new()),
                table,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    status: Option<String>,
    locatie: Option<String>,
}

#[derive(Template)]
#[template(path = "staff/orders.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    cancel_day: Option<String>,
    pickup_list: String,
    production: Option<String>,
    statuses: Vec<SelectOption>,
    locations: Vec<SelectOption>,
    table: Table,
    search: SearchBox,
}

fn any_option(state: &AppState, selected: bool) -> SelectOption {
    SelectOption {
        value: String::new(),
        label: t(state, "staff-orders-filter-any"),
        selected,
    }
}

/// The orders of one bake day, filtered by `status` and `locatie`. An unknown filter
/// value is ignored.
pub async fn list(
    State(state): State<AppState>,
    staff: Staff,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
    Query(search): Query<Search>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewOrders)?;
        let day = bake_days::load(&state, &id).await?;
        let status = query
            .status
            .as_deref()
            .and_then(|s| OrderStatus::ALL.iter().copied().find(|v| v.as_str() == s));
        let location = query
            .locatie
            .as_deref()
            .and_then(|l| l.parse().ok())
            .map(PickupLocationId::new);
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let list = manage::day_orders(&mut conn, day.id, &OrderFilter { status, location })
            .await
            .map_err(|e| fail(&state, &e))?;
        let slots = days::list_slots(&mut conn, day.id)
            .await
            .map_err(|e| fail(&state, &e))?;
        let all_locations = pickup::list_locations(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        drop(conn);
        let mut statuses = vec![any_option(&state, status.is_none())];
        statuses.extend(OrderStatus::ALL.iter().map(|s| SelectOption {
            value: s.as_str().to_owned(),
            label: t(&state, status_text(*s)),
            selected: Some(*s) == status,
        }));
        let mut locations = vec![any_option(&state, location.is_none())];
        locations.extend(
            all_locations
                .iter()
                .filter(|l| slots.iter().any(|s| s.location_id == l.id))
                .map(|l| SelectOption {
                    value: l.id.get().to_string(),
                    label: l.name.clone(),
                    selected: Some(l.id) == location,
                }),
        );
        let amsterdam = i18n::Amsterdam::new();
        let rows = list
            .iter()
            .filter(|o| {
                let number = o.number.to_string();
                if o.customer_erased {
                    search.matches(&[&number])
                } else {
                    search.matches(&[&number, &o.customer_name, &o.email])
                }
            })
            .map(|o| {
                let range = amsterdam
                    .time_range(o.pickup_starts, o.pickup_ends)
                    .unwrap_or_default();
                vec![
                    Cell::link(o.number.to_string(), order_path(o.id)),
                    customer_cell(&state, o.customer_erased, &o.customer_name),
                    Cell::text(format!("{range}, {}", o.location)),
                    Cell::number(o.pieces.to_string()),
                    Cell::number(i18n::money(o.total.cents())),
                    Cell::text(t(&state, status_text(o.status))),
                ]
            })
            .collect();
        let table = Table {
            id: "bestellingen".to_owned(),
            caption: t(&state, "staff-orders-caption"),
            head: vec![
                Cell::text(t(&state, "staff-order-number")),
                Cell::text(t(&state, "staff-order-customer")),
                Cell::text(t(&state, "staff-order-pickup")),
                Cell::number(t(&state, "staff-order-pieces")),
                Cell::number(t(&state, "staff-order-total")),
                Cell::text(t(&state, "staff-order-status")),
            ],
            rows,
        };
        let cancel_day = (day.status != bakedays::BakeDayStatus::Done
            && staff.0.role.can(Permission::CloseBakeDay))
        .then(|| super::sick_day::cancel_path(day.id));
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &ListPage {
                layout: Layout::new(&state),
                cancel_day,
                pickup_list: format!("{}/pickup-list", day_path(day.id)),
                production: staff
                    .0
                    .role
                    .can(Permission::Production)
                    .then(|| format!("{}/production", day_path(day.id))),
                heading: t_with(
                    &state,
                    "staff-orders-day-heading",
                    &[("date", state.i18n.long_date(day.date))],
                ),
                search: search.view(
                    &state,
                    &day_orders_path(&day),
                    [
                        ("status", query.status.clone()),
                        ("locatie", query.locatie.clone()),
                    ]
                    .into_iter()
                    .filter_map(|(name, value)| Some((name.to_owned(), value?)))
                    .filter(|(_, value)| !value.is_empty())
                    .collect(),
                ),
                action: day_orders_path(&day),
                statuses,
                locations,
                table,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// A confirm dialog: the button label, the text saying what will happen, and the target.
pub struct Confirm {
    pub dialog: String,
    pub label: String,
    pub text: String,
    pub action: String,
}

#[derive(Template)]
#[template(path = "staff/order.html")]
struct DetailPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    saved: bool,
    problem: String,
    facts: Vec<(String, Cell)>,
    lines: Table,
    payments: Table,
    refunds: Table,
    history: Table,
    move_action: String,
    slots: Vec<SelectOption>,
    actions: Vec<Confirm>,
}

async fn load_details(state: &AppState, raw_id: &str) -> Step<OrderDetails> {
    let id = OrderId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    match orders::details(&mut conn, id).await {
        Ok(details) => Ok(details),
        Err(orders::Error::NotFound) => Err(not_found(state).into()),
        Err(error) => Err(fail(state, &error).into()),
    }
}

async fn actor_name(state: &AppState, actor: &str, names: &mut BTreeMap<i64, String>) -> String {
    let Some((kind, rest)) = actor.split_once(':') else {
        return actor.to_owned();
    };
    match kind {
        "staff" => {
            let Ok(id) = rest.parse::<i64>() else {
                return actor.to_owned();
            };
            if let Some(name) = names.get(&id) {
                return name.clone();
            }
            let name = match auth::staff::find(&state.pool, StaffUserId::new(id)).await {
                Ok(Some(user)) => user.name,
                _ => actor.to_owned(),
            };
            names.insert(id, name.clone());
            name
        }
        "customer" => t(state, "staff-order-actor-customer"),
        _ => t(state, "staff-order-actor-system"),
    }
}

fn payment_state(state: &AppState, payment: &Payment, refunds: &[Refund]) -> String {
    if refunds.iter().any(|r| r.payment_id == payment.id) {
        t(state, "staff-order-payment-refund-requested")
    } else if payment.refunded.cents() > 0 {
        t_with(
            state,
            "staff-order-payment-refunded",
            &[("amount", i18n::money(payment.refunded.cents()))],
        )
    } else if payment.refund_reason.is_some() {
        t(state, "staff-order-payment-refund-due")
    } else {
        t(state, "staff-order-payment-ok")
    }
}

const fn awaits_pickup(status: OrderStatus) -> bool {
    matches!(status, OrderStatus::Paid | OrderStatus::Confirmed)
}

async fn render(
    state: &AppState,
    session: &Session,
    staff: &Staff,
    status: StatusCode,
    details: &OrderDetails,
    saved: bool,
    problem: String,
) -> Outcome {
    let order = &details.order;
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let day = days::get(&mut conn, order.bake_day_id)
        .await
        .map_err(|e| fail(state, &e))?;
    let slots = days::list_slots(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let location_names: BTreeMap<PickupLocationId, String> = pickup::list_locations(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?
        .into_iter()
        .map(|l| (l.id, l.name))
        .collect();
    let invoiced = manage::invoiced(&mut conn, order.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let payments = manage::payments(&mut conn, order.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let refunds = orders::of_order(&mut conn, order.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let logged: Vec<Logged> = audit::history(&mut conn, "order", order.id.get())
        .await
        .map_err(|e| fail(state, &e))?;
    drop(conn);

    let amsterdam = i18n::Amsterdam::new();
    let pickup = format!(
        "{} {}, {}",
        state.i18n.long_date(amsterdam.date(details.pickup.starts)),
        amsterdam
            .time_range(details.pickup.starts, details.pickup.ends)
            .unwrap_or_default(),
        details.pickup.location
    );
    let number = order.number.to_string();
    let mut facts = vec![
        (
            t(state, "staff-order-status"),
            Cell::text(t(state, status_text(order.status))),
        ),
        (
            t(state, "staff-order-customer"),
            customer_cell(state, order.customer_erased, &order.customer_name),
        ),
        (
            t(state, "staff-order-email"),
            customer_cell(state, order.customer_erased, &details.email),
        ),
    ];
    if let Some(phone) = &order.customer_phone {
        facts.push((t(state, "staff-order-phone"), Cell::text(phone.clone())));
    }
    facts.push((t(state, "staff-order-pickup"), Cell::text(pickup)));
    facts.push((
        t(state, "staff-order-total"),
        Cell::text(i18n::money(order.total.cents())),
    ));

    let lines = Table {
        id: "producten".to_owned(),
        caption: t(state, "staff-order-lines-caption"),
        head: vec![
            Cell::text(t(state, "staff-order-product")),
            Cell::number(t(state, "staff-order-qty")),
            Cell::number(t(state, "staff-order-price")),
        ],
        rows: order
            .lines
            .iter()
            .zip(&details.products)
            .map(|(line, name)| {
                vec![
                    Cell::text(name.clone()),
                    Cell::number(line.qty.to_string()),
                    Cell::number(i18n::money(line.unit_price.cents())),
                ]
            })
            .collect(),
    };
    let payments_table = Table {
        id: "betalingen".to_owned(),
        caption: t(state, "staff-order-payments-caption"),
        head: vec![
            Cell::text(t(state, "staff-order-payment-reference")),
            Cell::number(t(state, "staff-order-payment-amount")),
            Cell::text(t(state, "staff-order-status")),
        ],
        rows: payments
            .iter()
            .map(|p| {
                vec![
                    Cell::text(p.provider_payment_id.clone()),
                    Cell::number(i18n::money(p.amount.cents())),
                    Cell::text(payment_state(state, p, &refunds)),
                ]
            })
            .collect(),
    };
    let refunds_table = Table {
        id: "terugbetalingen".to_owned(),
        caption: t(state, "staff-order-refunds-caption"),
        head: vec![
            Cell::text(t(state, "staff-order-history-when")),
            Cell::number(t(state, "staff-order-payment-amount")),
            Cell::text(t(state, "staff-order-status")),
        ],
        rows: refunds
            .iter()
            .map(|r| {
                vec![
                    Cell::text(when(r.requested_at)),
                    Cell::number(i18n::money(r.amount.cents())),
                    Cell::text(t(state, refund_state_text(r.state))),
                ]
            })
            .collect(),
    };
    let mut names = BTreeMap::new();
    let mut history_rows = Vec::with_capacity(logged.len());
    for entry in &logged {
        history_rows.push(vec![
            Cell::text(when(entry.at)),
            Cell::text(actor_name(state, &entry.actor, &mut names).await),
            Cell::text(action_text(state, &entry.action)),
        ]);
    }
    let history = Table {
        id: "geschiedenis".to_owned(),
        caption: t(state, "staff-order-history-caption"),
        head: vec![
            Cell::text(t(state, "staff-order-history-when")),
            Cell::text(t(state, "staff-order-history-who")),
            Cell::text(t(state, "staff-order-history-what")),
        ],
        rows: history_rows,
    };

    let path = order_path(order.id);
    let role = staff.0.role;
    let open = awaits_pickup(order.status);
    let movable = open && !invoiced && !day.status.is_final() && role.can(Permission::EditOrders);
    let slot_options = if movable {
        slots
            .iter()
            .map(|s| SelectOption {
                value: s.id.get().to_string(),
                label: format!(
                    "{} {}, {}",
                    state.i18n.long_date(amsterdam.date(s.starts_at)),
                    amsterdam
                        .time_range(s.starts_at, s.ends_at)
                        .unwrap_or_default(),
                    location_names
                        .get(&s.location_id)
                        .cloned()
                        .unwrap_or_default()
                ),
                selected: s.id == order.pickup_slot_id,
            })
            .collect()
    } else {
        Vec::new()
    };
    let who = [
        ("number", number.clone()),
        (
            "name",
            customer_name(state, order.customer_erased, &order.customer_name),
        ),
    ];
    let mut actions = Vec::new();
    if open && day.status != bakedays::BakeDayStatus::Cancelled && role.can(Permission::PickupList)
    {
        actions.push(Confirm {
            dialog: "picked-up".to_owned(),
            label: t(state, "staff-order-picked-up"),
            text: t_with(state, "staff-order-picked-up-text", &who),
            action: format!("{path}/picked-up"),
        });
    }
    if order.status == OrderStatus::PendingPayment && role.can(Permission::EditOrders) {
        actions.push(Confirm {
            dialog: "cancel".to_owned(),
            label: t(state, "staff-order-cancel"),
            text: t_with(state, "staff-order-cancel-text", &who),
            action: format!("{path}/cancel"),
        });
    }
    if open && !invoiced && !day.status.is_final() && role.can(Permission::Refund) {
        let paid: i64 = payments
            .iter()
            .filter(|p| p.refund_reason.is_none())
            .map(|p| p.amount.cents() - p.refunded.cents())
            .sum();
        let text = if paid > 0 {
            let mut args = who.to_vec();
            args.push(("amount", i18n::money(paid)));
            t_with(state, "staff-order-cancel-paid-text", &args)
        } else {
            t_with(state, "staff-order-cancel-text", &who)
        };
        actions.push(Confirm {
            dialog: "cancel".to_owned(),
            label: t(state, "staff-order-cancel"),
            text,
            action: format!("{path}/cancel"),
        });
    }
    if role.can(Permission::Refund) {
        for refund in refunds.iter().filter(|r| r.state != RefundState::Done) {
            actions.push(Confirm {
                dialog: format!("refund-{}", refund.id.get()),
                label: t(state, "staff-order-refund-retry"),
                text: t_with(
                    state,
                    "staff-order-refund-retry-text",
                    &[
                        ("amount", i18n::money(refund.amount.cents())),
                        ("number", number.clone()),
                    ],
                ),
                action: format!("{path}/refunds/{}/retry", refund.id.get()),
            });
        }
    }

    Ok(pages::render(
        state,
        status,
        &DetailPage {
            layout: layout(state, session).await?,
            heading: t_with(state, "staff-order-heading", &[("number", number)]),
            back: day_orders_path(&day),
            saved,
            problem,
            facts,
            lines,
            payments: payments_table,
            refunds: refunds_table,
            history,
            move_action: format!("{path}/pickup"),
            slots: slot_options,
            actions,
        },
    ))
}

pub async fn show(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewOrders)?;
        let details = load_details(&state, &id).await?;
        render(
            &state,
            &session,
            &staff,
            StatusCode::OK,
            &details,
            flash.saved(),
            String::new(),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// The Dutch text for a refused order change; `None` for errors that are ours (500).
fn problem(state: &AppState, error: &orders::Error) -> Option<String> {
    let id = match error {
        orders::Error::WrongStatus(_) => "staff-order-error-status",
        orders::Error::DayFinal => "staff-order-error-day",
        orders::Error::SlotNotOnDay => "staff-order-error-slot",
        orders::Error::SlotFull => "staff-order-error-slot-full",
        orders::Error::Invoiced => "staff-order-error-invoiced",
        _ => return None,
    };
    Some(t(state, id))
}

/// After an action: the saved page, or the refusal as an alert (htmx) or on the page.
async fn answer(
    state: &AppState,
    session: &Session,
    staff: &Staff,
    headers: &HeaderMap,
    details: &OrderDetails,
    result: Result<(), orders::Error>,
) -> Outcome {
    match result {
        Ok(()) => Ok(saved(headers, &order_path(details.order.id))),
        Err(orders::Error::NotFound) => Err(not_found(state).into()),
        Err(error) => {
            let Some(text) = problem(state, &error) else {
                return Err(fail(state, &error).into());
            };
            if is_htmx(headers) {
                return Ok(alert_fragment(state, text));
            }
            let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
            let fresh = orders::details(&mut conn, details.order.id)
                .await
                .map_err(|e| fail(state, &e))?;
            drop(conn);
            render(
                state,
                session,
                staff,
                invalid_status(headers),
                &fresh,
                false,
                text,
            )
            .await
        }
    }
}

pub async fn move_pickup(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditOrders)?;
        let details = load_details(&state, &id).await?;
        let result = match fields.text("slot").parse::<i64>() {
            Ok(slot) => {
                let mut tx = state.pool.begin().await.map_err(|e| fail(&state, &e))?;
                let moved = manage::move_pickup(
                    &mut tx,
                    state.clock.as_ref(),
                    details.order.id,
                    PickupSlotId::new(slot),
                    staff.0.id,
                )
                .await;
                match moved {
                    Ok(order) => {
                        order_emails::withdraw_reminder(&mut tx, order.id)
                            .await
                            .map_err(|e| fail(&state, &e))?;
                        tx.commit().await.map_err(|e| fail(&state, &e))?;
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            Err(_) => Err(orders::Error::SlotNotOnDay),
        };
        answer(&state, &session, &staff, &headers, &details, result).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn picked_up(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::PickupList)?;
        let details = load_details(&state, &id).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = manage::mark_picked_up(
            &mut conn,
            state.clock.as_ref(),
            details.order.id,
            staff.0.id,
        )
        .await
        .map(drop);
        drop(conn);
        answer(&state, &session, &staff, &headers, &details, result).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Queues `build`'s email in a savepoint of `tx`; a failure is logged and the change still
/// counts (ADR 0022).
pub async fn queue_email<F>(
    state: &AppState,
    tx: &mut sqlx::PgConnection,
    order: OrderId,
    build: F,
) -> Step<()>
where
    F: AsyncFnOnce(
        &mut sqlx::PgConnection,
    ) -> Result<Option<notify::OutboxId>, order_emails::EmailError>,
{
    let mut email_tx = tx.begin().await.map_err(|e| fail(state, &e))?;
    match build(&mut email_tx).await {
        Ok(_) => email_tx.commit().await.map_err(|e| fail(state, &e))?,
        Err(error) => {
            tracing::error!(order = order.get(), %error, "order changed without its email");
            email_tx.rollback().await.map_err(|e| fail(state, &e))?;
        }
    }
    Ok(())
}

/// Cancels an order and queues the cancellation email in the same transaction. An unpaid
/// order needs `EditOrders`; a paid or confirmed one needs `Refund` and gets a refund of what
/// was paid, sent to the provider after the commit (ADR 0028).
pub async fn cancel(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditOrders)?;
        let details = load_details(&state, &id).await?;
        let unpaid = details.order.status == OrderStatus::PendingPayment;
        if !unpaid {
            require(&state, &staff, Permission::Refund)?;
        }
        let clock = state.clock.as_ref();
        let order = details.order.id;
        let mut tx = state.pool.begin().await.map_err(|e| fail(&state, &e))?;
        let result = if unpaid {
            manage::cancel_by_staff(&mut tx, clock, order, staff.0.id)
                .await
                .map(|_| (None, Vec::new()))
        } else {
            orders::cancel_paid(&mut tx, clock, order, staff.0.id)
                .await
                .map(|cancelled| (cancelled.refunded(), cancelled.refunds))
        };
        let result = match result {
            Ok((refund, refunds)) => {
                queue_email(&state, &mut tx, order, async |conn| {
                    order_emails::enqueue_cancellation(
                        conn,
                        clock,
                        &state.order_emails,
                        order,
                        refund,
                    )
                    .await
                })
                .await?;
                order_emails::withdraw_reminder(&mut tx, order)
                    .await
                    .map_err(|e| fail(&state, &e))?;
                tx.commit().await.map_err(|e| fail(&state, &e))?;
                send_refunds(&state, &refunds).await?;
                Ok(())
            }
            Err(error) => {
                drop(tx);
                Err(error)
            }
        };
        answer(&state, &session, &staff, &headers, &details, result).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Sends a refund of the order again, after it failed or was never sent (ADR 0028).
pub async fn retry_refund(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, refund)): Path<(String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Refund)?;
        let details = load_details(&state, &id).await?;
        let refund = RefundId::new(path_id(&state, &refund)?);
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let refunds = orders::of_order(&mut conn, details.order.id)
            .await
            .map_err(|e| fail(&state, &e))?;
        drop(conn);
        let Some(found) = refunds.into_iter().find(|r| r.id == refund) else {
            return Err(not_found(&state).into());
        };
        send_refunds(&state, std::slice::from_ref(&found)).await?;
        answer(&state, &session, &staff, &headers, &details, Ok(())).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
