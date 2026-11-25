//! Charity payouts (F11b): what each charity is owed for a period, and recording the bank
//! transfer that pays it.

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Router;
use axum::routing::{get, post};
use catalog::charities;
use domain::{CharityId, Money};
use orders::audit::Actor;
use reports::payouts::{self, NewTransfer, Payout};
use tower_sessions::Session;

use super::form::{Errors, Fields, money_value, parse_date, parse_money};
use super::reports::{
    Chosen, PeriodForm, PeriodQuery, choose_from, default_period, money, report_error,
};
use super::{
    Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require, saved, t,
    t_with,
};
use crate::components::{Cell, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/staff/reports/payouts", get(list))
        .route("/staff/reports/payouts/{id}", get(detail))
        .route(
            "/staff/reports/payouts/{id}/transfers",
            post(record),
        )
}

const LIST: &str = "/staff/reports/payouts";

fn detail_path(charity: CharityId) -> String {
    format!("{LIST}/{}", charity.get())
}

/// Payouts default to drops up to today: a drop still to come has nothing to pay yet.
fn choose_payout(state: &AppState, query: &PeriodQuery, action: &str) -> Step<Chosen> {
    choose_from(state, query, action, false, default_period(state, true)?)
}

async fn payout(state: &AppState, charity: CharityId, chosen: &Chosen) -> Step<Payout> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    payouts::payout(&mut conn, charity, chosen.period)
        .await
        .map_err(|e| report_error(state, &e))
}

fn net(state: &AppState, payout: &Payout) -> Step<Money> {
    payout.net().map_err(|e| report_error(state, &e))
}

#[derive(Template)]
#[template(path = "staff/report_payouts.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    period: PeriodForm,
    table: Table,
}

pub async fn list(
    State(state): State<AppState>,
    staff: Staff,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageCharities)?;
        let chosen = choose_payout(&state, &query, LIST)?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = charities::list_all(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        drop(conn);
        let mut rows = Vec::with_capacity(all.len());
        for charity in &all {
            let p = payout(&state, charity.id, &chosen).await?;
            rows.push(vec![
                Cell::link(
                    charity.name.clone(),
                    format!("{}?{}", detail_path(charity.id), chosen.query()),
                ),
                Cell::number(money(p.received)),
                Cell::number(money(p.refunds)),
                Cell::number(money(p.transferred)),
                Cell::number(money(p.corrections)),
                Cell::number(money(p.saved)),
                Cell::number(money(p.to_transfer)),
            ]);
        }
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &ListPage {
                layout: Layout::new(&state),
                period: chosen.form,
                table: Table {
                    id: "uitbetalingen".to_owned(),
                    caption: t(&state, "reports-payouts-title"),
                    head: vec![
                        Cell::text(t(&state, "report-col-charity")),
                        Cell::number(t(&state, "report-col-received")),
                        Cell::number(t(&state, "report-col-refunds")),
                        Cell::number(t(&state, "report-col-transferred")),
                        Cell::number(t(&state, "reports-corrections")),
                        Cell::number(t(&state, "report-col-saved")),
                        Cell::number(t(&state, "report-col-to-transfer")),
                    ],
                    rows,
                },
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub struct TransferForm {
    pub action: String,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub transferred_on: String,
    pub bank_reference: String,
    pub errors: Errors,
}

#[derive(Template)]
#[template(path = "staff/report_payout.html")]
struct DetailPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    period: PeriodForm,
    saved: bool,
    problem: String,
    facts: Table,
    transfers: Table,
    form: Option<TransferForm>,
}

async fn charity(state: &AppState, raw_id: &str) -> Step<catalog::Charity> {
    let id = CharityId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    charities::get(&mut conn, id).await.map_err(|e| match e {
        catalog::Error::NotFound => not_found(state).into(),
        other => fail(state, &other).into(),
    })
}

struct Answer {
    saved: bool,
    problem: String,
    posted: Option<TransferForm>,
}

async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    charity: &catalog::Charity,
    chosen: Chosen,
    answer: Answer,
) -> Outcome {
    let p = payout(state, charity.id, &chosen).await?;
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let done = payouts::transfers(&mut conn, charity.id)
        .await
        .map_err(|e| report_error(state, &e))?;
    drop(conn);
    let facts = [
        ("report-col-received", p.received),
        ("report-col-refunds", p.refunds),
        ("reports-net", net(state, &p)?),
        ("report-col-transferred", p.transferred),
        ("reports-corrections", p.corrections),
        ("report-col-saved", p.saved),
        ("report-col-to-transfer", p.to_transfer),
    ]
    .into_iter()
    .map(|(id, amount)| vec![Cell::text(t(state, id)), Cell::number(money(amount))])
    .collect();
    let path = detail_path(charity.id);
    let form = if p.to_transfer > Money::ZERO {
        Some(answer.posted.unwrap_or_else(|| TransferForm {
            action: format!("{path}/transfers"),
            from: chosen.form.from.clone(),
            to: chosen.form.to.clone(),
            amount: money_value(p.to_transfer.cents()),
            transferred_on: state.clock.now_in_bakery().date().to_string(),
            bank_reference: String::new(),
            errors: Errors::default(),
        }))
    } else {
        None
    };
    let problem = if answer.problem.is_empty() && form.is_none() {
        t(state, "reports-nothing-due")
    } else {
        answer.problem
    };
    Ok(pages::render(
        state,
        status,
        &DetailPage {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "reports-payout-heading",
                &[("charity", charity.name.clone())],
            ),
            back: format!("{LIST}?{}", chosen.query()),
            period: chosen.form,
            saved: answer.saved,
            problem,
            facts: Table {
                id: "uitbetaling".to_owned(),
                caption: t(state, "reports-payout-facts"),
                head: vec![
                    Cell::text(t(state, "reports-cost")),
                    Cell::number(t(state, "reports-amount")),
                ],
                rows: facts,
            },
            transfers: Table {
                id: "overboekingen".to_owned(),
                caption: t(state, "reports-transfers"),
                head: vec![
                    Cell::text(t(state, "reports-transfer-date")),
                    Cell::text(t(state, "reports-transfer-reference")),
                    Cell::number(t(state, "reports-amount")),
                ],
                rows: done
                    .iter()
                    .map(|d| {
                        vec![
                            Cell::text(i18n::numeric_date(d.transferred_on)),
                            Cell::text(d.bank_reference.clone()),
                            Cell::number(money(d.amount)),
                        ]
                    })
                    .collect(),
            },
            form,
        },
    ))
}

pub async fn detail(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageCharities)?;
        let charity = charity(&state, &id).await?;
        let chosen = choose_payout(&state, &query, &detail_path(charity.id))?;
        let answer = Answer {
            saved: query.saved(),
            problem: String::new(),
            posted: None,
        };
        render(&state, &session, StatusCode::OK, &charity, chosen, answer).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Records a transfer for drops up to the end of the shown period.
pub async fn record(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageCharities)?;
        let charity = charity(&state, &id).await?;
        let path = detail_path(charity.id);
        let query = PeriodQuery::between(fields.text("van"), fields.text("tot"));
        let chosen = choose_payout(&state, &query, &path)?;
        if !chosen.form.errors.is_empty()
            || fields.text("van").is_empty()
            || fields.text("tot").is_empty()
        {
            let answer = Answer {
                saved: false,
                problem: t(&state, "reports-error-period"),
                posted: None,
            };
            let status = invalid_status(&headers);
            return render(&state, &session, status, &charity, chosen, answer).await;
        }
        let mut posted = TransferForm {
            action: format!("{path}/transfers"),
            from: chosen.form.from.clone(),
            to: chosen.form.to.clone(),
            amount: fields.text("amount").to_owned(),
            transferred_on: fields.text("transferred_on").to_owned(),
            bank_reference: fields.text("bank_reference").to_owned(),
            errors: Errors::default(),
        };
        let amount = parse_money(&posted.amount);
        if amount.is_none() {
            posted
                .errors
                .add("amount", t(&state, "reports-error-amount-format"));
        }
        let transferred_on = parse_date(&posted.transferred_on);
        if transferred_on.is_none() {
            posted
                .errors
                .add("transferred_on", t(&state, "reports-error-date"));
        }
        if posted.bank_reference.is_empty() {
            posted
                .errors
                .add("bank_reference", t(&state, "reports-error-reference"));
        }
        let refuse = |posted: TransferForm, problem: String| Answer {
            saved: false,
            problem,
            posted: Some(posted),
        };
        let (Some(amount), Some(transferred_on), false) =
            (amount, transferred_on, posted.bank_reference.is_empty())
        else {
            let answer = refuse(posted, String::new());
            let status = invalid_status(&headers);
            return render(&state, &session, status, &charity, chosen, answer).await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = payouts::record_transfer(
            &mut conn,
            state.clock.as_ref(),
            Actor::Staff(staff.0.id),
            &NewTransfer {
                charity: charity.id,
                up_to: chosen.period.to(),
                amount: Money::from_cents(amount),
                transferred_on,
                bank_reference: posted.bank_reference.clone(),
            },
        )
        .await;
        drop(conn);
        let status = invalid_status(&headers);
        match result {
            Ok(_) => Ok(saved(&headers, &format!("{path}?{}", chosen.query()))),
            Err(reports::Error::AmountMismatch { due }) => {
                posted.errors.add(
                    "amount",
                    t_with(&state, "reports-error-amount", &[("due", money(due))]),
                );
                let answer = refuse(posted, String::new());
                render(&state, &session, status, &charity, chosen, answer).await
            }
            Err(reports::Error::NothingToTransfer { .. }) => {
                let answer = Answer {
                    saved: false,
                    problem: t(&state, "reports-nothing-due"),
                    posted: None,
                };
                render(&state, &session, status, &charity, chosen, answer).await
            }
            Err(reports::Error::Invalid(_)) => {
                posted
                    .errors
                    .add("bank_reference", t(&state, "reports-error-reference"));
                let answer = refuse(posted, String::new());
                render(&state, &session, status, &charity, chosen, answer).await
            }
            Err(reports::Error::NotFound) => Err(not_found(&state).into()),
            Err(error) => Err(report_error(&state, &error)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
