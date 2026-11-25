//! Business customers (F11a): their accounts, staff orders on account, and the overview of
//! on-account orders to invoice by hand, with its CSV and the form that records an invoice.

use std::collections::BTreeMap;

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bakedays::{days, pickup, range};
use domain::{BakeDayId, BusinessAccountId, OrderId, PickupSlotId, ProductId, VatRate};
use jobs::order_emails;
use orders::audit::Actor;
use orders::{Cart, CartError, CustomerInput};
use reports::invoices::{
    self, AccountDetails, BusinessAccount, Contact, InvoiceOverview, NewInvoice,
};
use serde::Deserialize;
use tower_sessions::Session;

use super::form::{Errors, Fields, parse_count, parse_date};
use super::orders::{order_path, queue_email};
use super::reports::{Chosen, PeriodForm, PeriodQuery, choose, csv_response, money, report_error};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require,
    saved, t, t_with,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

const LIST: &str = "/staff/business-accounts";
const NEW: &str = "/staff/business-accounts/new";
/// How far ahead the order form offers bake days.
const DAYS_AHEAD: i64 = 90;

fn account_path(id: BusinessAccountId) -> String {
    format!("{LIST}/{}", id.get())
}

async fn load(state: &AppState, raw_id: &str) -> Step<BusinessAccount> {
    let id = BusinessAccountId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    invoices::account_by_id(&mut conn, id)
        .await
        .map_err(|e| match e {
            reports::Error::NotFound => not_found(state).into(),
            other => report_error(state, &other),
        })
}

fn yes_no(state: &AppState, yes: bool) -> String {
    t(state, if yes { "b2b-yes" } else { "b2b-no" })
}

#[derive(Template)]
#[template(path = "staff/business_accounts.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    saved: bool,
    /// The viewer may open and change accounts.
    invoicing: bool,
    table: Table,
}

pub async fn list(
    State(state): State<AppState>,
    staff: Staff,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditOrders)?;
        let invoicing = staff.0.role.can(Permission::Invoicing);
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = invoices::list_accounts(&mut conn)
            .await
            .map_err(|e| report_error(&state, &e))?;
        drop(conn);
        let rows = all
            .iter()
            .map(|a| {
                let path = account_path(a.id);
                let mut row = vec![
                    if invoicing {
                        Cell::link(a.company_name.clone(), path.clone())
                    } else {
                        Cell::text(a.company_name.clone())
                    },
                    Cell::text(a.kvk.clone()),
                    Cell::text(a.invoice_email.clone()),
                    Cell::text(yes_no(&state, a.on_account_allowed)),
                ];
                if a.on_account_allowed {
                    row.push(Cell::link(
                        t(&state, "b2b-links-order"),
                        format!("{path}/order"),
                    ));
                } else {
                    row.push(Cell::text(String::new()));
                }
                if invoicing {
                    row.push(Cell::link(
                        t(&state, "b2b-links-invoice"),
                        format!("{path}/invoice"),
                    ));
                }
                row
            })
            .collect();
        let mut head = vec![
            Cell::text(t(&state, "b2b-col-company")),
            Cell::text(t(&state, "b2b-col-kvk")),
            Cell::text(t(&state, "b2b-col-invoice-email")),
            Cell::text(t(&state, "b2b-col-on-account")),
            Cell::text(t(&state, "b2b-links-order")),
        ];
        if invoicing {
            head.push(Cell::text(t(&state, "b2b-links-invoice")));
        }
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &ListPage {
                layout: Layout::new(&state),
                saved: flash.saved(),
                invoicing,
                table: Table {
                    id: "zakelijke-klanten".to_owned(),
                    caption: t(&state, "b2b-title"),
                    head,
                    rows,
                },
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub struct AccountForm {
    pub contact_email: String,
    pub contact_name: String,
    pub company_name: String,
    pub kvk: String,
    pub btw_id: String,
    pub invoice_address: String,
    pub invoice_email: String,
    pub on_account_allowed: bool,
}

fn plausible_email(email: &str) -> bool {
    !email.contains(char::is_whitespace)
        && email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
}

/// The field and message for details the crate refused after the form's own checks.
fn refused_field(state: &AppState, error: &reports::Error) -> Option<Errors> {
    let (field, id) = match error {
        reports::Error::Invalid("customer already has a business account") => {
            ("contact_email", "b2b-error-exists")
        }
        reports::Error::Invalid("contact email") => ("contact_email", "b2b-error-email"),
        reports::Error::Invalid("contact name") => ("contact_name", "b2b-error-text"),
        reports::Error::Invalid("company name") => ("company_name", "b2b-error-text"),
        _ => return None,
    };
    let mut errors = Errors::default();
    errors.add(field, t(state, id));
    Some(errors)
}

impl AccountForm {
    fn empty() -> Self {
        Self {
            contact_email: String::new(),
            contact_name: String::new(),
            company_name: String::new(),
            kvk: String::new(),
            btw_id: String::new(),
            invoice_address: String::new(),
            invoice_email: String::new(),
            on_account_allowed: true,
        }
    }

    fn of(account: &BusinessAccount) -> Self {
        Self {
            contact_email: account.contact_email.clone(),
            contact_name: String::new(),
            company_name: account.company_name.clone(),
            kvk: account.kvk.clone(),
            btw_id: account.btw_id.clone().unwrap_or_default(),
            invoice_address: account.invoice_address.clone(),
            invoice_email: account.invoice_email.clone(),
            on_account_allowed: account.on_account_allowed,
        }
    }

    fn posted(fields: &Fields) -> Self {
        Self {
            contact_email: fields.text("contact_email").to_owned(),
            contact_name: fields.text("contact_name").to_owned(),
            company_name: fields.text("company_name").to_owned(),
            kvk: fields.text("kvk").replace(' ', ""),
            btw_id: fields.text("btw_id").to_owned(),
            invoice_address: fields.raw("invoice_address").trim().to_owned(),
            invoice_email: fields.text("invoice_email").to_owned(),
            on_account_allowed: fields.checked("on_account_allowed"),
        }
    }

    /// The details, and for a new account also the contact.
    fn validate(&self, state: &AppState, new: bool) -> Result<(Contact, AccountDetails), Errors> {
        let mut errors = Errors::default();
        let mut required = |field: &str, value: &str| {
            if value.is_empty() {
                errors.add(field, t(state, "b2b-error-required"));
                false
            } else {
                true
            }
        };
        let mut wanted = vec![
            ("company_name", self.company_name.as_str()),
            ("kvk", self.kvk.as_str()),
            ("invoice_address", self.invoice_address.as_str()),
            ("invoice_email", self.invoice_email.as_str()),
        ];
        if new {
            wanted.push(("contact_email", self.contact_email.as_str()));
            wanted.push(("contact_name", self.contact_name.as_str()));
        }
        let filled: BTreeMap<&str, bool> = wanted
            .into_iter()
            .map(|(field, value)| (field, required(field, value)))
            .collect();
        let filled = |field: &str| filled.get(field).copied().unwrap_or(false);
        if filled("kvk") && (self.kvk.len() != 8 || !self.kvk.bytes().all(|b| b.is_ascii_digit())) {
            errors.add("kvk", t(state, "b2b-error-kvk"));
        }
        for (field, value) in [
            ("invoice_email", &self.invoice_email),
            ("contact_email", &self.contact_email),
        ] {
            if filled(field) && !plausible_email(value) {
                errors.add(field, t(state, "b2b-error-email"));
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok((
            Contact {
                email: self.contact_email.clone(),
                name: self.contact_name.clone(),
            },
            AccountDetails {
                company_name: self.company_name.clone(),
                kvk: self.kvk.clone(),
                btw_id: (!self.btw_id.is_empty()).then(|| self.btw_id.clone()),
                invoice_address: self.invoice_address.clone(),
                invoice_email: self.invoice_email.clone(),
                on_account_allowed: self.on_account_allowed,
            },
        ))
    }
}

#[derive(Template)]
#[template(path = "staff/business_account.html")]
struct AccountPage<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    /// The account's own links; `None` for a new account.
    links: Option<(String, String)>,
    new: bool,
    contact: String,
    form: AccountForm,
    errors: Errors,
    saved: bool,
}

async fn render_account(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    account: Option<&BusinessAccount>,
    form: AccountForm,
    errors: Errors,
    saved: bool,
) -> Outcome {
    let (heading, action, links) = match account {
        Some(a) => {
            let path = account_path(a.id);
            (
                a.company_name.clone(),
                path.clone(),
                Some((format!("{path}/invoice"), format!("{path}/order"))),
            )
        }
        None => (t(state, "b2b-new"), NEW.to_owned(), None),
    };
    Ok(pages::render(
        state,
        status,
        &AccountPage {
            layout: layout(state, session).await?,
            heading,
            action,
            new: links.is_none(),
            contact: t_with(
                state,
                "b2b-contact-fixed",
                &[("email", form.contact_email.clone())],
            ),
            links,
            form,
            errors,
            saved,
        },
    ))
}

pub async fn new_form(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::Invoicing)?;
        render_account(
            &state,
            &session,
            StatusCode::OK,
            None,
            AccountForm::empty(),
            Errors::default(),
            false,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn create(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Invoicing)?;
        let form = AccountForm::posted(&fields);
        let status = invalid_status(&headers);
        let (contact, details) = match form.validate(&state, true) {
            Ok(valid) => valid,
            Err(errors) => {
                return render_account(&state, &session, status, None, form, errors, false).await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = invoices::open_account(
            &mut conn,
            state.clock.as_ref(),
            Actor::Staff(staff.0.id),
            &contact,
            &details,
        )
        .await;
        drop(conn);
        match result {
            Ok(account) => Ok(saved(&headers, &account_path(account.id))),
            Err(error) => match refused_field(&state, &error) {
                Some(errors) => {
                    render_account(&state, &session, status, None, form, errors, false).await
                }
                None => Err(report_error(&state, &error)),
            },
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn show(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Invoicing)?;
        let account = load(&state, &id).await?;
        let form = AccountForm::of(&account);
        render_account(
            &state,
            &session,
            StatusCode::OK,
            Some(&account),
            form,
            Errors::default(),
            flash.saved(),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn update(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Invoicing)?;
        let account = load(&state, &id).await?;
        let mut form = AccountForm::posted(&fields);
        form.contact_email.clone_from(&account.contact_email);
        let details = match form.validate(&state, false) {
            Ok((_, details)) => details,
            Err(errors) => {
                let status = invalid_status(&headers);
                return render_account(
                    &state,
                    &session,
                    status,
                    Some(&account),
                    form,
                    errors,
                    false,
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = invoices::update_account(
            &mut conn,
            state.clock.as_ref(),
            Actor::Staff(staff.0.id),
            account.id,
            &details,
        )
        .await;
        drop(conn);
        match result {
            Ok(_) => Ok(saved(&headers, &account_path(account.id))),
            Err(error) => match refused_field(&state, &error) {
                Some(errors) => {
                    let status = invalid_status(&headers);
                    render_account(
                        &state,
                        &session,
                        status,
                        Some(&account),
                        form,
                        errors,
                        false,
                    )
                    .await
                }
                None => Err(report_error(&state, &error)),
            },
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub struct InvoiceForm {
    pub action: String,
    pub from: String,
    pub to: String,
    pub orders: Vec<i64>,
    pub invoice_number: String,
    pub invoice_date: String,
    pub errors: Errors,
}

#[derive(Template)]
#[template(path = "staff/business_invoice.html")]
struct InvoicePage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    period: PeriodForm,
    saved: bool,
    problem: String,
    csv: String,
    lines: Table,
    totals: Table,
    form: Option<InvoiceForm>,
    invoices: Table,
}

fn rate_text(rate: VatRate) -> String {
    format!("{}%", rate.percent())
}

fn line_rows(overview: &InvoiceOverview) -> Vec<Vec<Cell>> {
    overview
        .orders
        .iter()
        .flat_map(|order| {
            order.lines.iter().map(move |line| {
                vec![
                    Cell::link(order.number.to_string(), order_path(order.id)),
                    Cell::text(i18n::numeric_date(order.drop_date)),
                    Cell::text(line.product_name.clone()),
                    Cell::number(line.qty.to_string()),
                    Cell::number(money(line.unit_price_excl)),
                    Cell::number(rate_text(line.vat_rate)),
                    Cell::number(money(line.net)),
                    Cell::number(money(line.vat)),
                    Cell::number(money(line.gross)),
                ]
            })
        })
        .collect()
}

fn total_rows(state: &AppState, overview: &InvoiceOverview) -> Step<Vec<Vec<Cell>>> {
    let mut rows: Vec<Vec<Cell>> = VatRate::ALL
        .into_iter()
        .map(|rate| {
            let sum = overview.totals.rate(rate);
            vec![
                Cell::text(rate_text(rate)),
                Cell::number(money(sum.net())),
                Cell::number(money(sum.vat())),
                Cell::number(money(sum.gross())),
            ]
        })
        .collect();
    let net = overview
        .totals
        .rate(VatRate::Nine)
        .net()
        .checked_add(overview.totals.rate(VatRate::TwentyOne).net())
        .map_err(|e| fail(state, &e))?;
    let vat = overview
        .totals
        .rate(VatRate::Nine)
        .vat()
        .checked_add(overview.totals.rate(VatRate::TwentyOne).vat())
        .map_err(|e| fail(state, &e))?;
    let gross = overview.totals.gross().map_err(|e| fail(state, &e))?;
    rows.push(vec![
        Cell::text(t(state, "report-total")),
        Cell::number(money(net)),
        Cell::number(money(vat)),
        Cell::number(money(gross)),
    ]);
    Ok(rows)
}

struct Answer {
    saved: bool,
    problem: String,
    posted: Option<InvoiceForm>,
}

async fn overview(
    state: &AppState,
    account: &BusinessAccount,
    chosen: &Chosen,
) -> Step<InvoiceOverview> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    invoices::uninvoiced(&mut conn, account.customer_id, chosen.period)
        .await
        .map_err(|e| report_error(state, &e))
}

async fn render_invoice(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    account: &BusinessAccount,
    chosen: Chosen,
    answer: Answer,
) -> Outcome {
    let overview = overview(state, account, &chosen).await?;
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let made = invoices::invoices_of(&mut conn, account.customer_id)
        .await
        .map_err(|e| report_error(state, &e))?;
    drop(conn);
    let path = account_path(account.id);
    let form = if overview.orders.is_empty() {
        None
    } else {
        let orders = overview.orders.iter().map(|o| o.id.get()).collect();
        Some(match answer.posted {
            Some(posted) => InvoiceForm { orders, ..posted },
            None => InvoiceForm {
                action: format!("{path}/invoices"),
                from: chosen.form.from.clone(),
                to: chosen.form.to.clone(),
                orders,
                invoice_number: String::new(),
                invoice_date: state.clock.now_in_bakery().date().to_string(),
                errors: Errors::default(),
            },
        })
    };
    let caption = |id: &str| t(state, id);
    Ok(pages::render(
        state,
        status,
        &InvoicePage {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "b2b-invoice-heading",
                &[("company", account.company_name.clone())],
            ),
            back: path.clone(),
            csv: format!("{path}/invoice.csv?{}", chosen.query()),
            period: chosen.form,
            saved: answer.saved,
            problem: answer.problem,
            lines: Table {
                id: "te-factureren".to_owned(),
                caption: caption("b2b-invoice-orders"),
                head: vec![
                    Cell::text(t(state, "report-col-order")),
                    Cell::text(t(state, "report-col-drop-date")),
                    Cell::text(t(state, "report-col-product")),
                    Cell::number(t(state, "report-col-pieces")),
                    Cell::number(t(state, "report-col-unit-price-excl")),
                    Cell::number(t(state, "report-col-vat-rate")),
                    Cell::number(t(state, "report-col-net")),
                    Cell::number(t(state, "report-col-vat")),
                    Cell::number(t(state, "report-col-gross")),
                ],
                rows: line_rows(&overview),
            },
            totals: Table {
                id: "totalen".to_owned(),
                caption: caption("b2b-invoice-totals"),
                head: vec![
                    Cell::text(t(state, "report-col-vat-rate")),
                    Cell::number(t(state, "report-col-net")),
                    Cell::number(t(state, "report-col-vat")),
                    Cell::number(t(state, "report-col-gross")),
                ],
                rows: total_rows(state, &overview)?,
            },
            form,
            invoices: Table {
                id: "facturen".to_owned(),
                caption: caption("b2b-invoices"),
                head: vec![
                    Cell::text(t(state, "b2b-invoice-number")),
                    Cell::text(t(state, "b2b-invoice-date")),
                    Cell::text(t(state, "b2b-col-period")),
                    Cell::number(t(state, "b2b-col-orders")),
                    Cell::number(t(state, "report-col-gross")),
                ],
                rows: made
                    .iter()
                    .map(|i| {
                        vec![
                            Cell::text(i.invoice_number.clone()),
                            Cell::text(i18n::numeric_date(i.invoice_date)),
                            Cell::text(t_with(
                                state,
                                "b2b-period",
                                &[
                                    ("from", i18n::numeric_date(i.period.from())),
                                    ("to", i18n::numeric_date(i.period.to())),
                                ],
                            )),
                            Cell::number(i.orders.to_string()),
                            Cell::number(money(i.total)),
                        ]
                    })
                    .collect(),
            },
        },
    ))
}

pub async fn invoice_page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Invoicing)?;
        let account = load(&state, &id).await?;
        let action = format!("{}/invoice", account_path(account.id));
        let chosen = choose(&state, &query, &action, false)?;
        let answer = Answer {
            saved: query.saved(),
            problem: String::new(),
            posted: None,
        };
        render_invoice(&state, &session, StatusCode::OK, &account, chosen, answer).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn invoice_csv(
    State(state): State<AppState>,
    staff: Staff,
    Path(id): Path<String>,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Invoicing)?;
        let account = load(&state, &id).await?;
        let chosen = choose(&state, &query, "", false)?;
        if !chosen.form.errors.is_empty() {
            return Ok(pages::error(&state, StatusCode::BAD_REQUEST));
        }
        let overview = overview(&state, &account, &chosen).await?;
        let body = invoices::csv(&overview, &state.i18n).map_err(|e| report_error(&state, &e))?;
        let file = format!(
            "factuuroverzicht-{}-{}-{}.csv",
            account.id.get(),
            chosen.period.from(),
            chosen.period.to()
        );
        Ok(csv_response(&file, body))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Links the posted orders, those the owner saw in the overview, to the invoice.
pub async fn mark_invoiced(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Invoicing)?;
        let account = load(&state, &id).await?;
        let path = account_path(account.id);
        let query = PeriodQuery::between(fields.text("van"), fields.text("tot"));
        let chosen = choose(&state, &query, &format!("{path}/invoice"), false)?;
        let status = invalid_status(&headers);
        if !chosen.form.errors.is_empty()
            || fields.text("van").is_empty()
            || fields.text("tot").is_empty()
        {
            let answer = Answer {
                saved: false,
                problem: t(&state, "reports-error-period"),
                posted: None,
            };
            return render_invoice(&state, &session, status, &account, chosen, answer).await;
        }
        let mut posted = InvoiceForm {
            action: format!("{path}/invoices"),
            from: chosen.form.from.clone(),
            to: chosen.form.to.clone(),
            orders: Vec::new(),
            invoice_number: fields.text("invoice_number").to_owned(),
            invoice_date: fields.text("invoice_date").to_owned(),
            errors: Errors::default(),
        };
        if posted.invoice_number.is_empty() {
            posted
                .errors
                .add("invoice_number", t(&state, "b2b-error-number"));
        }
        let invoice_date = parse_date(&posted.invoice_date);
        if invoice_date.is_none() {
            posted
                .errors
                .add("invoice_date", t(&state, "reports-error-date"));
        }
        let orders: Option<Vec<OrderId>> = fields
            .all("order")
            .map(|raw| raw.trim().parse().ok().map(OrderId::new))
            .collect();
        let changed = |posted: Option<InvoiceForm>| Answer {
            saved: false,
            problem: t(&state, "b2b-error-changed"),
            posted,
        };
        let Some(orders) = orders.filter(|o| !o.is_empty()) else {
            return render_invoice(&state, &session, status, &account, chosen, changed(None)).await;
        };
        let Some(invoice_date) = invoice_date.filter(|_| posted.errors.is_empty()) else {
            let answer = Answer {
                saved: false,
                problem: String::new(),
                posted: Some(posted),
            };
            return render_invoice(&state, &session, status, &account, chosen, answer).await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = invoices::mark_invoiced(
            &mut conn,
            state.clock.as_ref(),
            Actor::Staff(staff.0.id),
            &NewInvoice {
                customer_id: account.customer_id,
                orders,
                invoice_number: posted.invoice_number.clone(),
                invoice_date,
                period: chosen.period,
            },
        )
        .await;
        drop(conn);
        match result {
            Ok(_) => Ok(saved(
                &headers,
                &format!("{path}/invoice?{}", chosen.query()),
            )),
            Err(reports::Error::DuplicateInvoiceNumber) => {
                posted
                    .errors
                    .add("invoice_number", t(&state, "b2b-error-number-used"));
                let answer = Answer {
                    saved: false,
                    problem: String::new(),
                    posted: Some(posted),
                };
                render_invoice(&state, &session, status, &account, chosen, answer).await
            }
            Err(
                reports::Error::AlreadyInvoiced(_)
                | reports::Error::NotInvoiceable(_)
                | reports::Error::Invalid(_),
            ) => {
                render_invoice(
                    &state,
                    &session,
                    status,
                    &account,
                    chosen,
                    changed(Some(posted)),
                )
                .await
            }
            Err(error) => Err(report_error(&state, &error)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// `?dag=12` picks the bake day of the order form.
#[derive(Debug, Default, Deserialize)]
pub struct DayQuery {
    dag: Option<String>,
}

pub struct PieceField {
    pub product: i64,
    pub name: String,
    pub label: String,
    pub value: String,
}

pub struct OrderForm {
    pub action: String,
    pub day: String,
    pub pieces: Vec<PieceField>,
    pub errors: Errors,
}

#[derive(Template)]
#[template(path = "staff/business_order.html")]
struct OrderPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    choose_action: String,
    days: Vec<SelectOption>,
    /// The pickup slots of the chosen day; outside `form`, so the select can take them.
    slots: Vec<SelectOption>,
    problem: String,
    form: Option<OrderForm>,
}

/// Bake days from today on that take staff orders.
async fn order_days(state: &AppState) -> Step<Vec<bakedays::BakeDay>> {
    let today = state.clock.now_in_bakery().date();
    let to = today.saturating_add(jiff::Span::new().days(DAYS_AHEAD));
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    Ok(days::list_between(&mut conn, today, to)
        .await
        .map_err(|e| fail(state, &e))?
        .into_iter()
        .filter(|d| d.online_ordering && !d.status.is_final())
        .collect())
}

/// The products on the day's range that can be sold online, by id, with their names.
async fn day_products(state: &AppState, day: BakeDayId) -> Step<Vec<(ProductId, String)>> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let on_range = range::list_products(&mut conn, day)
        .await
        .map_err(|e| fail(state, &e))?;
    let mut products = Vec::with_capacity(on_range.len());
    for entry in on_range {
        match catalog::products::get(&mut conn, entry.product_id).await {
            Ok(p) if p.active && p.sold_online => products.push((p.id, p.name_nl)),
            Ok(_) | Err(catalog::Error::NotFound) => {}
            Err(other) => return Err(fail(state, &other).into()),
        }
    }
    Ok(products)
}

fn pieces_name(product: ProductId) -> String {
    format!("qty_{}", product.get())
}

/// What a posted order form gets back: the fields as typed and why it was refused.
#[derive(Default)]
struct OrderAnswer<'f> {
    posted: Option<&'f Fields>,
    problem: String,
    errors: Errors,
}

async fn render_order(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    account: &BusinessAccount,
    chosen_day: Option<BakeDayId>,
    answer: OrderAnswer<'_>,
) -> Outcome {
    let OrderAnswer {
        posted,
        problem,
        errors,
    } = answer;
    let path = account_path(account.id);
    let days = order_days(state).await?;
    let day = chosen_day.and_then(|id| days.iter().find(|d| d.id == id));
    let (form, slots) = match day {
        None => (None, Vec::new()),
        Some(day) => {
            let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
            let slots = days::list_slots(&mut conn, day.id)
                .await
                .map_err(|e| fail(state, &e))?;
            let locations: BTreeMap<_, _> = pickup::list_locations(&mut conn)
                .await
                .map_err(|e| fail(state, &e))?
                .into_iter()
                .map(|l| (l.id, l.name))
                .collect();
            drop(conn);
            let amsterdam = i18n::Amsterdam::new();
            let chosen_slot = posted.map_or("", |f| f.text("slot"));
            let slots = slots
                .iter()
                .map(|s| SelectOption {
                    value: s.id.get().to_string(),
                    label: format!(
                        "{} {}, {}",
                        state.i18n.long_date(amsterdam.date(s.starts_at)),
                        amsterdam
                            .time_range(s.starts_at, s.ends_at)
                            .unwrap_or_default(),
                        locations.get(&s.location_id).cloned().unwrap_or_default()
                    ),
                    selected: s.id.get().to_string() == chosen_slot,
                })
                .collect();
            let pieces = day_products(state, day.id)
                .await?
                .into_iter()
                .map(|(id, name)| {
                    let field = pieces_name(id);
                    PieceField {
                        product: id.get(),
                        value: posted.map_or("", |f| f.text(&field)).to_owned(),
                        label: t_with(state, "b2b-order-pieces", &[("product", name)]),
                        name: field,
                    }
                })
                .collect();
            (
                Some(OrderForm {
                    action: format!("{path}/order"),
                    day: day.id.get().to_string(),
                    pieces,
                    errors,
                }),
                slots,
            )
        }
    };
    let days = days
        .iter()
        .map(|d| SelectOption {
            value: d.id.get().to_string(),
            label: state.i18n.long_date(d.date),
            selected: Some(d.id) == chosen_day,
        })
        .collect();
    Ok(pages::render(
        state,
        status,
        &OrderPage {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "b2b-order-heading",
                &[("company", account.company_name.clone())],
            ),
            back: LIST.to_owned(),
            choose_action: format!("{path}/order"),
            days,
            slots,
            problem,
            form,
        },
    ))
}

fn day_id(raw: &str) -> Option<BakeDayId> {
    raw.trim().parse().ok().map(BakeDayId::new)
}

pub async fn order_form(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(query): Query<DayQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditOrders)?;
        let account = load(&state, &id).await?;
        let day = query.dag.as_deref().and_then(day_id);
        render_order(
            &state,
            &session,
            StatusCode::OK,
            &account,
            day,
            OrderAnswer::default(),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// The Dutch text for a refused order on account; `None` for errors that are ours (500).
fn order_problem(
    state: &AppState,
    error: &orders::Error,
    names: &[(ProductId, String)],
) -> Option<String> {
    let id = match error {
        orders::Error::NotOnAccount => "b2b-error-not-on-account",
        orders::Error::CapacityExceeded {
            product_id,
            remaining,
        } => {
            let product = names
                .iter()
                .find(|(id, _)| id == product_id)
                .map(|(_, name)| name.clone())
                .unwrap_or_default();
            return Some(t_with(
                state,
                "b2b-error-capacity",
                &[("product", product), ("n", remaining.to_string())],
            ));
        }
        orders::Error::SlotNotOnDay => "b2b-error-slot",
        orders::Error::SlotFull => "b2b-error-slot-full",
        orders::Error::ProductionOnly
        | orders::Error::DayFinal
        | orders::Error::NotOpen
        | orders::Error::PastDeadline => "b2b-error-day",
        orders::Error::Cart(CartError::Empty) => "b2b-error-empty",
        orders::Error::Cart(_) | orders::Error::FreeOrder => "b2b-error-product",
        orders::Error::Invalid(_) => "b2b-error-details",
        _ => return None,
    };
    Some(t(state, id))
}

pub async fn place_order(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditOrders)?;
        let account = load(&state, &id).await?;
        let status = invalid_status(&headers);
        let Some(day) = day_id(fields.text("dag")) else {
            let problem = t(&state, "b2b-error-day");
            return render_order(
                &state,
                &session,
                status,
                &account,
                None,
                OrderAnswer {
                    problem,
                    ..OrderAnswer::default()
                },
            )
            .await;
        };
        let refuse = |problem: String, errors: Errors| {
            render_order(
                &state,
                &session,
                status,
                &account,
                Some(day),
                OrderAnswer {
                    posted: Some(&fields),
                    problem,
                    errors,
                },
            )
        };
        let products = day_products(&state, day).await?;
        let mut cart = Cart::new(day);
        let mut errors = Errors::default();
        let posted: Option<Vec<ProductId>> = fields
            .all("product")
            .map(|raw| raw.trim().parse().ok().map(ProductId::new))
            .collect();
        let Some(posted) = posted else {
            return refuse(t(&state, "b2b-error-product"), errors).await;
        };
        for product in posted {
            let field = pieces_name(product);
            let raw = fields.text(&field);
            if raw.is_empty() {
                continue;
            }
            match parse_count(raw).map(|qty| cart.set(product, qty, None)) {
                Some(Ok(())) => {}
                Some(Err(_)) | None => errors.add(&field, t(&state, "b2b-error-pieces")),
            }
        }
        if !errors.is_empty() {
            return refuse(String::new(), errors).await;
        }
        if cart.is_empty() {
            return refuse(t(&state, "b2b-error-empty"), errors).await;
        }
        let Ok(slot) = fields.text("slot").parse::<i64>().map(PickupSlotId::new) else {
            return refuse(t(&state, "b2b-error-slot"), errors).await;
        };
        let clock = state.clock.as_ref();
        let mut tx = state.pool.begin().await.map_err(|e| fail(&state, &e))?;
        let result = orders::checkout_on_account(
            &mut tx,
            clock,
            &cart,
            &CustomerInput {
                email: account.contact_email.clone(),
                name: account.company_name.clone(),
                phone: None,
                marketing_opt_in: false,
            },
            slot,
            staff.0.id,
        )
        .await;
        let order = match result {
            Ok(order) => order,
            Err(error) => {
                drop(tx);
                return match error {
                    orders::Error::NotFound => Err(not_found(&state).into()),
                    error => match order_problem(&state, &error, &products) {
                        Some(problem) => refuse(problem, errors).await,
                        None => Err(fail(&state, &error).into()),
                    },
                };
            }
        };
        queue_email(&state, &mut tx, order.id, async |conn| {
            order_emails::enqueue_on_account_confirmation(
                conn,
                clock,
                &state.order_emails,
                order.id,
            )
            .await
        })
        .await?;
        tx.commit().await.map_err(|e| fail(&state, &e))?;
        Ok(saved(&headers, &order_path(order.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
