//! The sick-day cancel of a whole bake day (F2, ADR 0028): one page that says what will
//! happen and takes the message for customers, and afterwards lists the refunds that still
//! have to go out, with a button to send them again.

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bakedays::{BakeDay, BakeDayStatus};
use domain::BakeDayId;
use jobs::order_emails;
use tower_sessions::Session;

use super::bake_days;
use super::form::Fields;
use super::orders::{queue_email, send_refunds};
use super::{Flash, Outcome, Rejection, fail, invalid_status, layout, require, saved, t, t_with};
use crate::components::{Cell, Table};
use crate::pages;
use crate::staff::Staff;
use crate::state::AppState;

pub fn cancel_path(day: BakeDayId) -> String {
    format!("/staff/bake-days/{}/cancel", day.get())
}

#[derive(Template)]
#[template(path = "staff/bake_day_cancel.html")]
struct CancelPage<'a> {
    layout: pages::Layout<'a>,
    heading: String,
    back: String,
    action: String,
    saved: bool,
    problem: String,
    cancelled: bool,
    facts: Vec<(String, String)>,
    invoiced: String,
    refunds: Table,
    draft: String,
    message_error: String,
}

struct Shown<'a> {
    saved: bool,
    problem: String,
    draft: &'a str,
    message_error: String,
}

async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    day: &BakeDay,
    shown: Shown<'_>,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let preview = orders::day_preview(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let open = orders::open_of_day(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    drop(conn);
    let cancelled = day.status == BakeDayStatus::Cancelled;
    let facts = if cancelled {
        vec![(
            t(state, "staff-sick-day-message"),
            day.cancel_message.clone().unwrap_or_default(),
        )]
    } else {
        vec![
            (
                t(state, "staff-sick-day-orders"),
                preview.orders.to_string(),
            ),
            (
                t(state, "staff-sick-day-refunds"),
                preview.refunds.to_string(),
            ),
            (
                t(state, "staff-sick-day-amount"),
                i18n::money(preview.refund_total.cents()),
            ),
        ]
    };
    let invoiced = if preview.invoiced.is_empty() {
        String::new()
    } else {
        let numbers: Vec<String> = preview.invoiced.iter().map(ToString::to_string).collect();
        t_with(
            state,
            "staff-sick-day-invoiced",
            &[("numbers", numbers.join(", "))],
        )
    };
    let refunds = Table {
        id: "terugbetalingen".to_owned(),
        caption: t(state, "staff-sick-day-open-refunds"),
        head: vec![
            Cell::text(t(state, "staff-order-number")),
            Cell::number(t(state, "staff-order-payment-amount")),
            Cell::text(t(state, "staff-order-status")),
        ],
        rows: open
            .iter()
            .map(|r| {
                vec![
                    Cell::link(
                        r.order_number.to_string(),
                        super::orders::order_path(r.order_id),
                    ),
                    Cell::number(i18n::money(r.amount.cents())),
                    Cell::text(t(
                        state,
                        match r.state {
                            orders::RefundState::Failed => "staff-order-refund-failed",
                            _ => "staff-order-refund-requested",
                        },
                    )),
                ]
            })
            .collect(),
    };
    Ok(pages::render(
        state,
        status,
        &CancelPage {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "staff-sick-day-heading",
                &[("date", state.i18n.long_date(day.date))],
            ),
            back: format!("{}/orders", bake_days::day_path(day.id)),
            action: cancel_path(day.id),
            saved: shown.saved,
            problem: shown.problem,
            cancelled,
            facts,
            invoiced,
            refunds,
            draft: shown.draft.to_owned(),
            message_error: shown.message_error,
        },
    ))
}

/// Says what cancelling the day does, or after the cancel, which refunds are still open.
pub async fn page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::CloseBakeDay)?;
        let day = bake_days::load(&state, &id).await?;
        render(
            &state,
            &session,
            StatusCode::OK,
            &day,
            Shown {
                saved: flash.saved(),
                problem: String::new(),
                draft: "",
                message_error: String::new(),
            },
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Cancels the day with every order and queues one email per order in one transaction, then
/// sends the refunds. On a cancelled day it only sends the refunds that are still open, so
/// it is safe to post again after a partial failure.
pub async fn cancel(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::CloseBakeDay)?;
        let day = bake_days::load(&state, &id).await?;
        let message = fields.text("bericht");
        let clock = state.clock.as_ref();
        let mut tx = state.pool.begin().await.map_err(|e| fail(&state, &e))?;
        let done = match orders::cancel_day(&mut tx, clock, day.id, message, staff.0.id).await {
            Ok(done) => done,
            Err(error) => {
                drop(tx);
                let (problem, message_error) = match error {
                    orders::Error::Invalid(_) => {
                        (String::new(), t(&state, "staff-sick-day-error-message"))
                    }
                    orders::Error::DayFinal => {
                        (t(&state, "staff-sick-day-error-done"), String::new())
                    }
                    other => return Err(fail(&state, &other).into()),
                };
                return render(
                    &state,
                    &session,
                    invalid_status(&headers),
                    &day,
                    Shown {
                        saved: false,
                        problem,
                        draft: message,
                        message_error,
                    },
                )
                .await;
            }
        };
        let invoiced = done.invoiced.iter().map(|&(order, _)| (order, None));
        for (order, refund) in done.orders.iter().copied().chain(invoiced) {
            let text = done.message.as_str();
            queue_email(&state, &mut tx, order, async |conn| {
                order_emails::enqueue_sick_day(
                    conn,
                    clock,
                    &state.order_emails,
                    order,
                    text,
                    refund,
                )
                .await
            })
            .await?;
            order_emails::withdraw_reminder(&mut tx, order)
                .await
                .map_err(|e| fail(&state, &e))?;
        }
        tx.commit().await.map_err(|e| fail(&state, &e))?;
        send_refunds(&state, &done.open_refunds).await?;
        Ok(saved(&headers, &cancel_path(day.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
