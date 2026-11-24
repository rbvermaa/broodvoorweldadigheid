//! The production list (F7) and the pickup list (F8) of a bake day, on screen and as PDF.
//! Ticking off on the pickup list uses the same action as the order page (ADR 0016).

use askama::Template;
use auth::Permission;
use axum::extract::{Path, Query, State};
use axum::http::HeaderValue;
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, HeaderName};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use bakedays::{BakeDay, Purpose};
use domain::OrderId;
use orders::manage;
use orders::{OrderStatus, PaymentMode};
use production::{PickupList, PickupOrder, ProductionList};
use tower_sessions::Session;

use super::bake_days::{self, day_path};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, is_htmx, layout, not_found, path_id,
    require, t, t_with,
};
use crate::components::{Cell, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

fn production_path(day: &BakeDay) -> String {
    format!("{}/production", day_path(day.id))
}

fn pickup_path(day: &BakeDay) -> String {
    format!("{}/pickup-list", day_path(day.id))
}

fn orders_path(day: &BakeDay) -> String {
    format!("{}/orders", day_path(day.id))
}

fn production_error(state: &AppState, error: &production::Error) -> Rejection {
    match error {
        production::Error::BakeDays(bakedays::Error::NotFound) => not_found(state).into(),
        other => fail(state, other).into(),
    }
}

async fn load_production(state: &AppState, raw_id: &str) -> Step<ProductionList> {
    let day = bake_days::load(state, raw_id).await?;
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    production::production_list(&mut conn, day.id)
        .await
        .map_err(|e| production_error(state, &e))
}

async fn load_pickups(state: &AppState, day: &BakeDay) -> Step<PickupList> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    production::pickup_list(&mut conn, day.id)
        .await
        .map_err(|e| production_error(state, &e))
}

fn count(n: u64) -> String {
    i18n::decimal(i64::try_from(n).unwrap_or(i64::MAX), 0)
}

#[derive(Template)]
#[template(path = "staff/production.html")]
struct ProductionPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    pdf: String,
    table: Table,
}

const fn purpose_text(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::Freezer => "production-extra-freezer",
        Purpose::SumUp => "production-extra-sumup",
        Purpose::Samples => "production-extra-samples",
        Purpose::Other => "production-extra-other",
    }
}

fn production_table(state: &AppState, list: &ProductionList) -> Table {
    let mut head = vec![
        Cell::text(t(state, "production-product")),
        Cell::number(t(state, "production-ordered")),
    ];
    head.extend(Purpose::ALL.map(|p| Cell::number(t(state, purpose_text(p)))));
    head.push(Cell::number(t(state, "production-to-bake")));

    let mut sum = production::Extras::default();
    let mut ordered = 0;
    let mut rows: Vec<Vec<Cell>> = list
        .products
        .iter()
        .map(|p| {
            ordered += p.ordered;
            sum.freezer += p.extras.freezer;
            sum.sumup += p.extras.sumup;
            sum.samples += p.extras.samples;
            sum.other += p.extras.other;
            let mut row = vec![Cell::text(p.name.clone()), Cell::number(count(p.ordered))];
            row.extend(Purpose::ALL.map(|purpose| Cell::number(count(p.extras.get(purpose)))));
            row.push(Cell::number(count(p.to_bake())));
            row
        })
        .collect();
    if !rows.is_empty() {
        let mut row = vec![
            Cell::text(t(state, "production-sum")),
            Cell::number(count(ordered)),
        ];
        row.extend(Purpose::ALL.map(|purpose| Cell::number(count(sum.get(purpose)))));
        row.push(Cell::number(count(ordered + sum.total())));
        rows.push(row);
    }
    Table {
        id: "productie".to_owned(),
        caption: t(state, "production-title"),
        head,
        rows,
    }
}

pub async fn production_page(
    State(state): State<AppState>,
    staff: Staff,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let list = load_production(&state, &id).await?;
        let path = production_path(&list.day);
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &ProductionPage {
                layout: Layout::new(&state),
                heading: t_with(
                    &state,
                    "lists-production-heading",
                    &[("date", state.i18n.long_date(list.day.date))],
                ),
                back: day_path(list.day.id),
                pdf: format!("{path}/pdf"),
                table: production_table(&state, &list),
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Typst is CPU work, so it runs off the async workers.
async fn pdf_response<T, F>(state: &AppState, list: T, file: String, render: F) -> Outcome
where
    T: Send + 'static,
    F: FnOnce(&T, &i18n::Translator) -> Result<Vec<u8>, production::Error> + Send + 'static,
{
    let i18n = state.i18n.clone();
    let bytes = tokio::task::spawn_blocking(move || render(&list, &i18n))
        .await
        .map_err(|e| fail(state, &e))?
        .map_err(|e| fail(state, &e))?;
    Ok((
        StatusCode::OK,
        [
            (CONTENT_TYPE, "application/pdf".to_owned()),
            (CONTENT_DISPOSITION, format!("inline; filename=\"{file}\"")),
            (CACHE_CONTROL, "private, no-store".to_owned()),
        ],
        bytes,
    )
        .into_response())
}

pub async fn production_pdf(
    State(state): State<AppState>,
    staff: Staff,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let list = load_production(&state, &id).await?;
        let file = format!("productielijst-{}.pdf", list.day.date);
        pdf_response(&state, list, file, production::production_pdf).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub struct OrderCard {
    pub anchor: String,
    pub name: String,
    pub number: String,
    pub link: String,
    pub phone: String,
    pub phone_href: String,
    pub lines: Vec<String>,
    pub status: String,
    pub picked_up: bool,
    pub action: String,
    pub saved: bool,
    pub error: String,
}

pub struct SlotSection {
    pub heading: String,
    pub orders: Vec<OrderCard>,
}

#[derive(Template)]
#[template(path = "staff/pickup_list.html")]
struct PickupPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    pdf: String,
    saved: bool,
    problem: String,
    slots: Vec<SlotSection>,
}

/// The answer to a tick, shown on that order's card.
struct Answer {
    order: OrderId,
    saved: bool,
    error: String,
}

fn status_text(order: &PickupOrder) -> &'static str {
    match (order.status, order.payment_mode) {
        (OrderStatus::PickedUp, _) => "pickup-list-status-picked-up",
        (OrderStatus::Confirmed, PaymentMode::OnAccount) => "pickup-list-status-on-account",
        (OrderStatus::Paid, _) => "pickup-list-status-paid",
        _ => "pickup-list-status-confirmed",
    }
}

fn card(state: &AppState, list: &str, order: &PickupOrder, answer: Option<&Answer>) -> OrderCard {
    let n = order.order_id.get();
    let answer = answer.filter(|a| a.order == order.order_id);
    let phone = order.customer_phone.clone().unwrap_or_default();
    OrderCard {
        anchor: format!("bestelling-{n}"),
        name: order.customer_name.clone(),
        number: order.number.to_string(),
        link: super::orders::order_path(order.order_id),
        phone_href: phone
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '+')
            .collect(),
        phone,
        lines: order
            .lines
            .iter()
            .map(|line| {
                t_with(
                    state,
                    "pickup-list-line",
                    &[
                        ("qty", count(u64::from(line.qty))),
                        ("product", line.name.clone()),
                    ],
                )
            })
            .collect(),
        status: t(state, status_text(order)),
        picked_up: order.picked_up,
        action: format!(
            "{list}/{n}/{}",
            if order.picked_up { "undo" } else { "picked-up" }
        ),
        saved: answer.is_some_and(|a| a.saved),
        error: answer.map(|a| a.error.clone()).unwrap_or_default(),
    }
}

async fn render_pickups(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    day: &BakeDay,
    saved: bool,
    answer: Option<Answer>,
    htmx: bool,
) -> Outcome {
    let list = load_pickups(state, day).await?;
    let path = pickup_path(day);
    let zone = i18n::Amsterdam::new();
    let slots = list
        .slots
        .iter()
        .map(|slot| SlotSection {
            heading: t_with(
                state,
                "pickup-list-slot",
                &[
                    ("day", state.i18n.short_date(zone.date(slot.slot.starts_at))),
                    (
                        "range",
                        zone.time_range(slot.slot.starts_at, slot.slot.ends_at)
                            .unwrap_or_default(),
                    ),
                    ("location", slot.location.clone()),
                ],
            ),
            orders: slot
                .orders
                .iter()
                .map(|order| card(state, &path, order, answer.as_ref()))
                .collect(),
        })
        .collect();
    let shown = |id: OrderId| {
        list.slots
            .iter()
            .flat_map(|s| &s.orders)
            .any(|o| o.order_id == id)
    };
    let problem = answer
        .filter(|a| !shown(a.order))
        .map(|a| a.error)
        .unwrap_or_default();
    let retarget = htmx && !problem.is_empty();
    let mut response = pages::render(
        state,
        status,
        &PickupPage {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "lists-pickup-heading",
                &[("date", state.i18n.long_date(day.date))],
            ),
            back: orders_path(day),
            pdf: format!("{path}/pdf"),
            saved,
            problem,
            slots,
        },
    );
    if retarget {
        // The order left the list meanwhile, so its card cannot show the refusal.
        let headers = response.headers_mut();
        for name in ["hx-retarget", "hx-reselect"] {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_static("#status"),
            );
        }
        headers.insert(
            HeaderName::from_static("hx-reswap"),
            HeaderValue::from_static("outerHTML"),
        );
    }
    Ok(response)
}

pub async fn pickup_page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::PickupList)?;
        let day = bake_days::load(&state, &id).await?;
        render_pickups(
            &state,
            &session,
            StatusCode::OK,
            &day,
            flash.saved(),
            None,
            false,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn pickup_pdf(
    State(state): State<AppState>,
    staff: Staff,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::PickupList)?;
        let day = bake_days::load(&state, &id).await?;
        let list = load_pickups(&state, &day).await?;
        let file = format!("ophaallijst-{}.pdf", day.date);
        pdf_response(&state, list, file, production::pickup_pdf).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[derive(Clone, Copy)]
enum Tick {
    PickedUp,
    Undo,
}

/// The order, if it belongs to `day`; anything else is a 404.
async fn day_order(state: &AppState, day: &BakeDay, raw_id: &str) -> Step<OrderId> {
    let id = OrderId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    match orders::order::get(&mut conn, id).await {
        Ok(order) if order.bake_day_id == day.id => Ok(id),
        Ok(_) | Err(orders::Error::NotFound) => Err(not_found(state).into()),
        Err(error) => Err(fail(state, &error).into()),
    }
}

async fn tick(
    state: AppState,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    (id, order): (String, String),
    what: Tick,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::PickupList)?;
        let day = bake_days::load(&state, &id).await?;
        let order = day_order(&state, &day, &order).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let clock = state.clock.as_ref();
        let result = match what {
            Tick::PickedUp => manage::mark_picked_up(&mut conn, clock, order, staff.0.id)
                .await
                .map(drop),
            Tick::Undo => orders::undo_picked_up(&mut conn, clock, order, staff.0.id)
                .await
                .map(drop),
        };
        drop(conn);
        let error = match result {
            Ok(()) | Err(orders::Error::WrongStatus(OrderStatus::PickedUp)) => None,
            Err(orders::Error::DayFinal) => Some(t(&state, "staff-order-error-day")),
            Err(orders::Error::WrongStatus(_) | orders::Error::NotPickable(_)) => {
                Some(t(&state, "staff-order-error-status"))
            }
            Err(orders::Error::NotFound) => return Err(not_found(&state).into()),
            Err(error) => return Err(fail(&state, &error).into()),
        };
        match error {
            None if !is_htmx(&headers) => Ok(Redirect::to(&format!(
                "{}?opgeslagen=1#bestelling-{}",
                pickup_path(&day),
                order.get()
            ))
            .into_response()),
            None => {
                let answer = Answer {
                    order,
                    saved: true,
                    error: String::new(),
                };
                render_pickups(
                    &state,
                    &session,
                    StatusCode::OK,
                    &day,
                    false,
                    Some(answer),
                    true,
                )
                .await
            }
            Some(error) => {
                let answer = Answer {
                    order,
                    saved: false,
                    error,
                };
                let status = invalid_status(&headers);
                render_pickups(
                    &state,
                    &session,
                    status,
                    &day,
                    false,
                    Some(answer),
                    is_htmx(&headers),
                )
                .await
            }
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn picked_up(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(ids): Path<(String, String)>,
) -> Response {
    tick(state, staff, session, headers, ids, Tick::PickedUp).await
}

pub async fn undo(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(ids): Path<(String, String)>,
) -> Response {
    tick(state, staff, session, headers, ids, Tick::Undo).await
}
