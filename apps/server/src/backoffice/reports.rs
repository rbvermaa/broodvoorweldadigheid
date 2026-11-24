//! Report screens for the owner: sales per BTW rate (F11), donations with charts (F11c)
//! and the bakery's contribution with drop costs (F11e), each with its CSV. The numbers
//! come from the `reports` crate; this module only reads periods and lays them out.

use std::collections::BTreeMap;

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bakedays::days;
use catalog::charities;
use domain::{BakeDayId, CharityId, Money, VatRate};
use jiff::civil::Date;
use reports::contribution::{self, Contribution, DropCostKind, NewDropCost};
use reports::{Granularity, Period, donations, sales};
use serde::Deserialize;
use tower_sessions::Session;

use super::form::{Errors, Fields, parse_date, parse_money};
use super::{
    Outcome, Rejection, Step, fail, invalid_status, layout, require, saved, t, t_count, t_with,
};
use crate::components::{BarChart, Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

/// `?van=2026-01-01&tot=2026-12-31&per=maand`; `opgeslagen` after a save.
#[derive(Debug, Default, Deserialize)]
pub struct PeriodQuery {
    van: Option<String>,
    tot: Option<String>,
    per: Option<String>,
    opgeslagen: Option<String>,
}

impl PeriodQuery {
    /// The period a form posted back in its hidden `van` and `tot` fields.
    pub fn between(from: &str, to: &str) -> Self {
        Self {
            van: Some(from.to_owned()),
            tot: Some(to.to_owned()),
            per: None,
            opgeslagen: None,
        }
    }

    pub fn saved(&self) -> bool {
        self.opgeslagen.is_some()
    }
}

/// The period form above a report.
pub struct PeriodForm {
    pub action: String,
    pub from: String,
    pub to: String,
    /// Empty for reports without a choice of day, month or quarter.
    pub granularities: Vec<SelectOption>,
    pub errors: Errors,
}

/// A period read from the query, with the form that shows it.
pub struct Chosen {
    pub period: Period,
    pub granularity: Granularity,
    pub form: PeriodForm,
}

impl Chosen {
    /// The query string for links that keep this period.
    pub fn query(&self) -> String {
        let mut query = format!("van={}&tot={}", self.period.from(), self.period.to());
        if !self.form.granularities.is_empty() {
            query.push_str("&per=");
            query.push_str(granularity_value(self.granularity));
        }
        query
    }
}

const fn granularity_value(granularity: Granularity) -> &'static str {
    match granularity {
        Granularity::Day => "dag",
        Granularity::Month => "maand",
        Granularity::Quarter => "kwartaal",
    }
}

const GRANULARITIES: [(Granularity, &str); 3] = [
    (Granularity::Day, "reports-per-day"),
    (Granularity::Month, "reports-per-month"),
    (Granularity::Quarter, "reports-per-quarter"),
];

/// This calendar year.
fn default_period(state: &AppState) -> Step<Period> {
    let today = state.clock.now_in_bakery().date();
    let from = Date::new(today.year(), 1, 1).map_err(|e| fail(state, &e))?;
    let to = Date::new(today.year(), 12, 31).map_err(|e| fail(state, &e))?;
    Period::new(from, to).map_err(|e| fail(state, &e).into())
}

/// Reads `van`, `tot` and `per`. A wrong date gets a message and the default period.
pub fn choose(
    state: &AppState,
    query: &PeriodQuery,
    action: &str,
    with_granularity: bool,
) -> Step<Chosen> {
    let fallback = default_period(state)?;
    let mut errors = Errors::default();
    let mut read = |field: &str, raw: Option<&String>, default: Date| match raw
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        None => default,
        Some(text) => parse_date(text).unwrap_or_else(|| {
            errors.add(field, t(state, "reports-error-date"));
            default
        }),
    };
    let from = read("van", query.van.as_ref(), fallback.from());
    let to = read("tot", query.tot.as_ref(), fallback.to());
    let period = Period::new(from, to).unwrap_or_else(|_| {
        errors.add("tot", t(state, "reports-error-order"));
        fallback
    });
    let granularity = GRANULARITIES
        .iter()
        .find(|(g, _)| Some(granularity_value(*g)) == query.per.as_deref())
        .map_or(Granularity::Month, |(g, _)| *g);
    let granularities = if with_granularity {
        GRANULARITIES
            .iter()
            .map(|(g, text)| SelectOption {
                value: granularity_value(*g).to_owned(),
                label: t(state, text),
                selected: *g == granularity,
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Chosen {
        period,
        granularity,
        form: PeriodForm {
            action: action.to_owned(),
            from: period.from().to_string(),
            to: period.to().to_string(),
            granularities,
            errors,
        },
    })
}

pub fn money(amount: Money) -> String {
    i18n::money(amount.cents())
}

fn rate_text(rate: VatRate) -> String {
    format!("{}%", rate.percent())
}

pub fn report_error(state: &AppState, error: &reports::Error) -> Rejection {
    fail(state, error).into()
}

/// A CSV download; it holds business data, so no cache keeps it.
pub fn csv_response(file: &str, body: String) -> Response {
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file}\""),
            ),
            (CACHE_CONTROL, "private, no-store".to_owned()),
        ],
        body,
    )
        .into_response()
}

fn csv_name(prefix: &str, period: &Period) -> String {
    format!("{prefix}-{}-{}.csv", period.from(), period.to())
}

pub async fn charity_names(state: &AppState) -> Step<BTreeMap<CharityId, String>> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    Ok(charities::list_all(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?
        .into_iter()
        .map(|c| (c.id, c.name))
        .collect())
}

#[derive(Template)]
#[template(path = "staff/reports.html")]
struct IndexPage<'a> {
    layout: Layout<'a>,
}

pub async fn index(State(state): State<AppState>, staff: Staff) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewReports)?;
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &IndexPage {
                layout: Layout::new(&state),
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[derive(Template)]
#[template(path = "staff/report_sales.html")]
struct SalesPage<'a> {
    layout: Layout<'a>,
    period: PeriodForm,
    csv: String,
    pwyw_csv: String,
    estimated: String,
    table: Table,
    pwyw: Table,
}

async fn sales_report(state: &AppState, chosen: &Chosen) -> Step<sales::SalesReport> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    sales::report(&mut conn, chosen.period, chosen.granularity)
        .await
        .map_err(|e| report_error(state, &e))
}

fn sales_rows(rows: &mut Vec<Vec<Cell>>, from: String, to: String, totals: &domain::VatTotals) {
    for rate in VatRate::ALL {
        let sum = totals.rate(rate);
        rows.push(vec![
            Cell::text(from.clone()),
            Cell::text(to.clone()),
            Cell::number(rate_text(rate)),
            Cell::number(money(sum.gross())),
            Cell::number(money(sum.vat())),
            Cell::number(money(sum.net())),
        ]);
    }
}

pub async fn sales_page(
    State(state): State<AppState>,
    staff: Staff,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewReports)?;
        let chosen = choose(&state, &query, "/staff/reports/sales", true)?;
        let report = sales_report(&state, &chosen).await?;
        let mut rows = Vec::new();
        for row in &report.rows {
            sales_rows(
                &mut rows,
                i18n::numeric_date(row.period.from()),
                i18n::numeric_date(row.period.to()),
                &row.totals,
            );
        }
        if !report.rows.is_empty() {
            sales_rows(
                &mut rows,
                t(&state, "report-total"),
                String::new(),
                &report.total,
            );
        }
        let table = Table {
            id: "omzet".to_owned(),
            caption: t(&state, "reports-sales-title"),
            head: vec![
                Cell::text(t(&state, "report-col-from")),
                Cell::text(t(&state, "report-col-to")),
                Cell::number(t(&state, "report-col-vat-rate")),
                Cell::number(t(&state, "report-col-gross")),
                Cell::number(t(&state, "report-col-vat")),
                Cell::number(t(&state, "report-col-net")),
            ],
            rows,
        };
        let pwyw = Table {
            id: "betaal-wat-je-wilt".to_owned(),
            caption: t(&state, "reports-pwyw-title"),
            head: vec![
                Cell::text(t(&state, "report-col-product")),
                Cell::number(t(&state, "report-col-pieces")),
                Cell::number(t(&state, "report-col-average-paid")),
                Cell::number(t(&state, "report-col-average-suggested")),
            ],
            rows: report
                .pay_what_you_want
                .iter()
                .map(|p| {
                    vec![
                        Cell::text(p.product_name.clone()),
                        Cell::number(p.pieces.to_string()),
                        Cell::number(money(p.average_paid())),
                        Cell::number(money(p.average_suggested())),
                    ]
                })
                .collect(),
        };
        let estimated = if report.estimated_refunds.is_empty() {
            String::new()
        } else {
            let numbers: Vec<String> = report
                .estimated_refunds
                .iter()
                .map(ToString::to_string)
                .collect();
            t_with(
                &state,
                "reports-estimated",
                &[("orders", numbers.join(", "))],
            )
        };
        let query = chosen.query();
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &SalesPage {
                layout: Layout::new(&state),
                csv: format!("/staff/reports/sales.csv?{query}"),
                pwyw_csv: format!("/staff/reports/pay-what-you-want.csv?{query}"),
                period: chosen.form,
                estimated,
                table,
                pwyw,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn sales_csv(
    State(state): State<AppState>,
    staff: Staff,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewReports)?;
        let chosen = choose(&state, &query, "", true)?;
        let report = sales_report(&state, &chosen).await?;
        let body = sales::csv(&report, &state.i18n).map_err(|e| report_error(&state, &e))?;
        Ok(csv_response(&csv_name("omzet", &chosen.period), body))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn pay_what_you_want_csv(
    State(state): State<AppState>,
    staff: Staff,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewReports)?;
        let chosen = choose(&state, &query, "", true)?;
        let report = sales_report(&state, &chosen).await?;
        let body = sales::pay_what_you_want_csv(&report, &state.i18n)
            .map_err(|e| report_error(&state, &e))?;
        Ok(csv_response(
            &csv_name("betaal-wat-je-wilt", &chosen.period),
            body,
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[derive(Template)]
#[template(path = "staff/report_donations.html")]
struct DonationsPage<'a> {
    layout: Layout<'a>,
    period: PeriodForm,
    drops_csv: String,
    charities_csv: String,
    drops: Table,
    drops_chart: Option<BarChart>,
    charities: Table,
    charts: Vec<BarChart>,
}

fn name_of(names: &BTreeMap<CharityId, String>, id: CharityId) -> String {
    names.get(&id).cloned().unwrap_or_default()
}

pub async fn donations_page(
    State(state): State<AppState>,
    staff: Staff,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewReports)?;
        let chosen = choose(&state, &query, "/staff/reports/donations", true)?;
        let names = charity_names(&state).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let drops = donations::drops(&mut conn, chosen.period)
            .await
            .map_err(|e| report_error(&state, &e))?;
        let per_charity = donations::charities(&mut conn, chosen.period, chosen.granularity)
            .await
            .map_err(|e| report_error(&state, &e))?;
        drop(conn);
        let net = |d: &donations::DropDonations| d.net().map_err(|e| report_error(&state, &e));
        let mut drop_rows = Vec::with_capacity(drops.len());
        let mut bars = Vec::with_capacity(drops.len());
        for d in &drops {
            let donated = net(d)?;
            drop_rows.push(vec![
                Cell::text(i18n::numeric_date(d.date)),
                Cell::text(name_of(&names, d.charity)),
                Cell::number(d.orders.to_string()),
                Cell::number(money(d.received)),
                Cell::number(money(d.refunds)),
                Cell::number(money(donated)),
                Cell::number(money(d.to_drop_charity)),
                Cell::number(money(d.to_other_charities)),
                Cell::number(money(d.saved)),
            ]);
            bars.push((i18n::numeric_date(d.date), donated.cents(), money(donated)));
        }
        let drops_chart = (!bars.is_empty()).then(|| {
            BarChart::new(
                "giften-per-drop",
                t(&state, "reports-donations-drops-chart"),
                bars,
            )
        });
        let charts = per_charity
            .iter()
            .filter(|c| !c.series.is_empty())
            .map(|c| {
                BarChart::new(
                    format!("giften-doel-{}", c.charity.get()),
                    t_with(
                        &state,
                        "reports-donations-charity-chart",
                        &[("charity", name_of(&names, c.charity))],
                    ),
                    c.series
                        .iter()
                        .map(|p| {
                            (
                                i18n::numeric_date(p.period.from()),
                                p.amount.cents(),
                                money(p.amount),
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        let query = chosen.query();
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &DonationsPage {
                layout: Layout::new(&state),
                drops_csv: format!("/staff/reports/donations/drops.csv?{query}"),
                charities_csv: format!("/staff/reports/donations/charities.csv?{query}"),
                period: chosen.form,
                drops: Table {
                    id: "giften-drops".to_owned(),
                    caption: t(&state, "reports-donations-drops-title"),
                    head: vec![
                        Cell::text(t(&state, "report-col-drop-date")),
                        Cell::text(t(&state, "report-col-charity")),
                        Cell::number(t(&state, "report-col-orders")),
                        Cell::number(t(&state, "report-col-received")),
                        Cell::number(t(&state, "report-col-refunds")),
                        Cell::number(t(&state, "report-col-donated")),
                        Cell::number(t(&state, "report-col-to-drop-charity")),
                        Cell::number(t(&state, "report-col-to-other-charities")),
                        Cell::number(t(&state, "report-col-saved")),
                    ],
                    rows: drop_rows,
                },
                drops_chart,
                charities: Table {
                    id: "giften-doelen".to_owned(),
                    caption: t(&state, "reports-donations-charities-title"),
                    head: vec![
                        Cell::text(t(&state, "report-col-charity")),
                        Cell::number(t(&state, "report-col-in-period")),
                        Cell::number(t(&state, "report-col-all-time")),
                        Cell::number(t(&state, "report-col-transferred")),
                        Cell::number(t(&state, "report-col-to-transfer")),
                    ],
                    rows: per_charity
                        .iter()
                        .map(|c| {
                            vec![
                                Cell::text(name_of(&names, c.charity)),
                                Cell::number(money(c.in_period)),
                                Cell::number(money(c.all_time)),
                                Cell::number(money(c.transferred)),
                                Cell::number(money(c.to_transfer)),
                            ]
                        })
                        .collect(),
                },
                charts,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn donation_drops_csv(
    State(state): State<AppState>,
    staff: Staff,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewReports)?;
        let chosen = choose(&state, &query, "", true)?;
        let names = charity_names(&state).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let drops = donations::drops(&mut conn, chosen.period)
            .await
            .map_err(|e| report_error(&state, &e))?;
        drop(conn);
        let body = donations::drops_csv(&drops, &|id| name_of(&names, id), &state.i18n)
            .map_err(|e| report_error(&state, &e))?;
        Ok(csv_response(
            &csv_name("giften-per-drop", &chosen.period),
            body,
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn donation_charities_csv(
    State(state): State<AppState>,
    staff: Staff,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewReports)?;
        let chosen = choose(&state, &query, "", true)?;
        let names = charity_names(&state).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let list = donations::charities(&mut conn, chosen.period, chosen.granularity)
            .await
            .map_err(|e| report_error(&state, &e))?;
        drop(conn);
        let body = donations::charities_csv(&list, &|id| name_of(&names, id), &state.i18n)
            .map_err(|e| report_error(&state, &e))?;
        Ok(csv_response(
            &csv_name("giften-per-doel", &chosen.period),
            body,
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

const COST_KINDS: [(DropCostKind, &str); 3] = [
    (DropCostKind::Packaging, "reports-cost-packaging"),
    (DropCostKind::Energy, "reports-cost-energy"),
    (DropCostKind::Other, "reports-cost-other"),
];

pub struct CostForm {
    pub action: String,
    pub from: String,
    pub to: String,
    pub days: Vec<SelectOption>,
    pub kinds: Vec<SelectOption>,
    pub amount: String,
    pub note: String,
    pub errors: Errors,
}

#[derive(Template)]
#[template(path = "staff/report_contribution.html")]
struct ContributionPage<'a> {
    layout: Layout<'a>,
    period: PeriodForm,
    saved: bool,
    per_euro: String,
    warnings: Vec<String>,
    summary: Table,
    drops: Table,
    charities: Table,
    cost: CostForm,
}

/// What a cost form posted, to show again with its errors.
struct Posted {
    day: String,
    kind: String,
    amount: String,
    note: String,
    errors: Errors,
}

fn covered(state: &AppState, c: &Contribution) -> Step<Money> {
    c.covered().map_err(|e| report_error(state, &e))
}

fn per_euro(state: &AppState, c: &Contribution) -> Step<String> {
    let cents = c.per_euro_cents().map_err(|e| report_error(state, &e))?;
    Ok(cents.map_or_else(String::new, |cents| {
        t_with(state, "reports-per-euro", &[("amount", i18n::money(cents))])
    }))
}

async fn render_contribution(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    chosen: Chosen,
    saved: bool,
    posted: Option<Posted>,
) -> Outcome {
    let names = charity_names(state).await?;
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let report = contribution::report(&mut conn, chosen.period)
        .await
        .map_err(|e| report_error(state, &e))?;
    let charity_days = days::list_between(&mut conn, chosen.period.from(), chosen.period.to())
        .await
        .map_err(|e| fail(state, &e))?;
    drop(conn);
    let total = &report.total;
    let summary_rows = [
        ("report-col-received", total.received),
        ("report-col-vat", total.btw),
        ("reports-fees", total.fees),
        ("reports-ingredients", total.ingredients),
        ("reports-drop-costs", total.drop_costs),
        ("reports-covered", covered(state, total)?),
    ]
    .into_iter()
    .map(|(id, amount)| vec![Cell::text(t(state, id)), Cell::number(money(amount))])
    .collect();
    let mut warnings = Vec::new();
    if total.unknown_fees > 0 {
        warnings.push(t_count(state, "reports-unknown-fees", total.unknown_fees));
    }
    if !total.missing_estimates.is_empty() {
        let products: Vec<&str> = total.missing_estimates.iter().map(String::as_str).collect();
        warnings.push(t_with(
            state,
            "reports-missing-estimates",
            &[("products", products.join(", "))],
        ));
    }
    let head = |first: Vec<Cell>| {
        let mut head = first;
        head.extend(
            [
                "report-col-received",
                "report-col-vat",
                "reports-fees",
                "reports-ingredients",
                "reports-drop-costs",
                "reports-covered",
            ]
            .map(|id| Cell::number(t(state, id))),
        );
        head
    };
    let amounts = |c: &Contribution| -> Step<Vec<Cell>> {
        Ok([
            c.received,
            c.btw,
            c.fees,
            c.ingredients,
            c.drop_costs,
            covered(state, c)?,
        ]
        .map(|m| Cell::number(money(m)))
        .into())
    };
    let mut drop_rows = Vec::with_capacity(report.drops.len());
    for d in &report.drops {
        let mut row = vec![
            Cell::text(i18n::numeric_date(d.date)),
            Cell::text(name_of(&names, d.charity)),
        ];
        row.extend(amounts(&d.contribution)?);
        drop_rows.push(row);
    }
    let mut charity_rows = Vec::with_capacity(report.charities.len());
    for c in &report.charities {
        let name = c.charity.map_or_else(
            || t(state, "reports-saved-charity"),
            |id| name_of(&names, id),
        );
        let mut row = vec![Cell::text(name)];
        row.extend(amounts(&c.contribution)?);
        charity_rows.push(row);
    }
    let posted = posted.unwrap_or(Posted {
        day: String::new(),
        kind: DropCostKind::Packaging.as_str().to_owned(),
        amount: String::new(),
        note: String::new(),
        errors: Errors::default(),
    });
    let cost = CostForm {
        action: "/staff/reports/contribution/costs".to_owned(),
        from: chosen.form.from.clone(),
        to: chosen.form.to.clone(),
        days: charity_days
            .iter()
            .filter(|d| d.charity_id.is_some())
            .map(|d| SelectOption {
                value: d.id.get().to_string(),
                label: format!(
                    "{}, {}",
                    state.i18n.long_date(d.date),
                    d.charity_id.map(|c| name_of(&names, c)).unwrap_or_default()
                ),
                selected: d.id.get().to_string() == posted.day,
            })
            .collect(),
        kinds: COST_KINDS
            .iter()
            .map(|(kind, text)| SelectOption {
                value: kind.as_str().to_owned(),
                label: t(state, text),
                selected: kind.as_str() == posted.kind,
            })
            .collect(),
        amount: posted.amount,
        note: posted.note,
        errors: posted.errors,
    };
    Ok(pages::render(
        state,
        status,
        &ContributionPage {
            layout: layout(state, session).await?,
            per_euro: per_euro(state, total)?,
            period: chosen.form,
            saved,
            warnings,
            summary: Table {
                id: "bijdrage".to_owned(),
                caption: t(state, "reports-contribution-total"),
                head: vec![
                    Cell::text(t(state, "reports-cost")),
                    Cell::number(t(state, "reports-amount")),
                ],
                rows: summary_rows,
            },
            drops: Table {
                id: "bijdrage-drops".to_owned(),
                caption: t(state, "reports-contribution-drops"),
                head: head(vec![
                    Cell::text(t(state, "report-col-drop-date")),
                    Cell::text(t(state, "report-col-charity")),
                ]),
                rows: drop_rows,
            },
            charities: Table {
                id: "bijdrage-doelen".to_owned(),
                caption: t(state, "reports-contribution-charities"),
                head: head(vec![Cell::text(t(state, "report-col-charity"))]),
                rows: charity_rows,
            },
            cost,
        },
    ))
}

pub async fn contribution_page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewReports)?;
        let chosen = choose(&state, &query, "/staff/reports/contribution", false)?;
        render_contribution(
            &state,
            &session,
            StatusCode::OK,
            chosen,
            query.saved(),
            None,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn add_cost(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ViewReports)?;
        let query = PeriodQuery::between(fields.text("van"), fields.text("tot"));
        let chosen = choose(&state, &query, "/staff/reports/contribution", false)?;
        let mut posted = Posted {
            day: fields.text("bake_day_id").to_owned(),
            kind: fields.text("kind").to_owned(),
            amount: fields.text("amount").to_owned(),
            note: fields.raw("note").to_owned(),
            errors: Errors::default(),
        };
        let day = posted.day.parse::<i64>().ok().map(BakeDayId::new);
        if day.is_none() {
            posted
                .errors
                .add("bake_day_id", t(&state, "reports-error-cost-day"));
        }
        let kind = COST_KINDS
            .iter()
            .find(|(k, _)| k.as_str() == posted.kind)
            .map(|(k, _)| *k);
        if kind.is_none() {
            posted
                .errors
                .add("kind", t(&state, "reports-error-cost-kind"));
        }
        let amount = parse_money(&posted.amount).filter(|cents| *cents > 0);
        if amount.is_none() {
            posted.errors.add("amount", t(&state, "reports-error-cost"));
        }
        let (Some(day), Some(kind), Some(amount)) = (day, kind, amount) else {
            let status = invalid_status(&headers);
            return render_contribution(&state, &session, status, chosen, false, Some(posted))
                .await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let charity_day = match days::get(&mut conn, day).await {
            Ok(found) => found.charity_id.is_some(),
            Err(bakedays::Error::NotFound) => false,
            Err(error) => return Err(fail(&state, &error).into()),
        };
        let result = if charity_day {
            contribution::add_drop_cost(
                &mut conn,
                state.clock.as_ref(),
                &NewDropCost {
                    bake_day_id: day,
                    kind,
                    amount: Money::from_cents(amount),
                    note: Some(posted.note.clone()),
                },
            )
            .await
        } else {
            Err(reports::Error::NotFound)
        };
        drop(conn);
        match result {
            Ok(_) => Ok(saved(
                &headers,
                &format!("/staff/reports/contribution?{}", chosen.query()),
            )),
            Err(reports::Error::NotFound) => {
                posted
                    .errors
                    .add("bake_day_id", t(&state, "reports-error-cost-day"));
                let status = invalid_status(&headers);
                render_contribution(&state, &session, status, chosen, false, Some(posted)).await
            }
            Err(error) => Err(report_error(&state, &error)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
