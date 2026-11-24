//! Bake days: create, schedule, charity, range, capacity pools, copy and repeat, and
//! opening and closing ordering (F1, F1a, F3, F4c).

use std::collections::BTreeMap;

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bakedays::capacity::Remaining;
use bakedays::status::TransitionRefused;
use bakedays::{
    BakeDay, BakeDayProduct, BakeDayStatus, CapacityPool, NewBakeDay, NoOrders, Schedule, days,
    pickup, range,
};
use catalog::{Charity, Product, charities, products};
use domain::{BakeDayId, CapacityPoolId, CharityId, PickupLocationId, ProductId};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::Deserialize;
use tower_sessions::Session;

use super::form::{Errors, Fields, local_value, parse_count, parse_date, parse_local};
use super::{
    Outcome, Rejection, Step, fail, invalid_status, is_htmx, layout, not_found, path_id, require,
    saved, t, t_count, t_with,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

const DAYS_AHEAD: i32 = 366;

/// A refusal from the bake day rules, as a Dutch text for one form field. `field` is
/// empty when the message belongs to the whole page.
pub struct Problem {
    pub field: &'static str,
    pub text: String,
}

/// The Dutch text for a refused bake day write; `None` for errors that are ours (500).
pub fn problem(state: &AppState, error: &bakedays::Error) -> Option<Problem> {
    use bakedays::Error as E;
    let (field, text) = match error {
        E::Invalid(reason) => {
            let (field, id) = match *reason {
                "deadline" => ("cutoff_at", "day-error-deadline-missing"),
                "opening time of a production-only day" => {
                    ("opens_at", "day-error-opens-production-only")
                }
                "opening time" => ("opens_at", "day-error-opens-after-deadline"),
                "menu publish time" => ("menu_publish_at", "day-error-menu-after-deadline"),
                "deadline after bake start" => ("cutoff_at", "day-error-deadline-after-bake"),
                "production-only while open" => {
                    ("production_only", "day-error-production-only-open")
                }
                "charity" => ("charity_id", "day-error-charity-unknown"),
                "pool name" => ("pool_name", "pool-error-name"),
                "duplicate pool name" => ("pool_name", "pool-error-duplicate"),
                "units per item" => ("units", "range-error-units"),
                "product" => ("product_id", "range-error-product"),
                "capacity pool" => ("pool_id", "range-error-pool"),
                "number of weeks" => ("weeks", "day-error-weeks"),
                "quantity" => ("qty", "extra-error-qty"),
                "date" => ("date", "form-error-date"),
                _ => ("", "form-has-errors"),
            };
            (field, t(state, id))
        }
        E::NoPickupWindow(weekday) => (
            "location_id",
            t_with(
                state,
                "day-error-no-window",
                &[(
                    "weekday",
                    t_count(
                        state,
                        "weekday",
                        u32::from(weekday.to_monday_one_offset().unsigned_abs()),
                    ),
                )],
            ),
        ),
        E::DateTaken(date) => (
            "date",
            t_with(
                state,
                "day-error-date-taken",
                &[("date", state.i18n.long_date(*date))],
            ),
        ),
        E::CharityArchived => ("charity_id", t(state, "day-error-charity-archived")),
        E::OrdersExist => ("charity_id", t(state, "day-error-orders-exist")),
        E::Final => ("", t(state, "day-error-final")),
        E::Transition(refused) => ("", t(state, transition_message(*refused))),
        E::CapacityExceeded { remaining, .. } => {
            ("qty", t_count(state, "extra-error-capacity", *remaining))
        }
        E::NotFound | E::Catalog(_) | E::Corrupt(_) | E::Db(_) => return None,
    };
    Some(Problem { field, text })
}

const fn transition_message(refused: TransitionRefused) -> &'static str {
    match refused {
        TransitionRefused::NotAllowed { .. } => "day-error-transition",
        TransitionRefused::ProductionOnly => "day-error-production-only",
        TransitionRefused::DeadlinePassed => "day-error-deadline-passed",
        TransitionRefused::NotBaked => "day-error-not-baked",
    }
}

pub const fn status_message(status: BakeDayStatus) -> &'static str {
    match status {
        BakeDayStatus::Draft => "day-status-draft",
        BakeDayStatus::Open => "day-status-open",
        BakeDayStatus::Closed => "day-status-closed",
        BakeDayStatus::Cancelled => "day-status-cancelled",
        BakeDayStatus::Done => "day-status-done",
    }
}

pub fn day_path(id: BakeDayId) -> String {
    format!("/staff/bake-days/{}", id.get())
}

pub fn bakedays_error(state: &AppState, error: &bakedays::Error) -> Response {
    match error {
        bakedays::Error::NotFound => not_found(state),
        other => fail(state, other),
    }
}

pub async fn load(state: &AppState, raw_id: &str) -> Step<BakeDay> {
    let id = BakeDayId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    days::get(&mut conn, id)
        .await
        .map_err(|e| bakedays_error(state, &e).into())
}

fn today(state: &AppState) -> Date {
    state.clock.now_in_bakery().date()
}

fn optional_id(value: &str) -> Result<Option<i64>, ()> {
    if value.is_empty() {
        Ok(None)
    } else {
        value.parse().map(Some).map_err(|_| ())
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct DayFlash {
    opgeslagen: Option<String>,
    doel: Option<String>,
    herhaald: Option<u32>,
}

#[derive(Template)]
#[template(path = "staff/bake_days.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    table: Table,
    saved: bool,
    repeated: String,
}

pub async fn list(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(flash): Query<DayFlash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let from = today(&state);
        let to = from.saturating_add(jiff::Span::new().days(DAYS_AHEAD));
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let list = days::list_between(&mut conn, from, to)
            .await
            .map_err(|e| fail(&state, &e))?;
        let charity_names: BTreeMap<CharityId, String> = charities::list_all(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?
            .into_iter()
            .map(|c| (c.id, c.name))
            .collect();
        let location_names: BTreeMap<PickupLocationId, String> = pickup::list_locations(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?
            .into_iter()
            .map(|l| (l.id, l.name))
            .collect();
        let mut rows = Vec::with_capacity(list.len());
        for day in &list {
            let slots = days::list_slots(&mut conn, day.id)
                .await
                .map_err(|e| fail(&state, &e))?;
            let mut places: Vec<&str> = slots
                .iter()
                .filter_map(|s| location_names.get(&s.location_id).map(String::as_str))
                .collect();
            places.sort_unstable();
            places.dedup();
            rows.push(vec![
                Cell::link(state.i18n.long_date(day.date), day_path(day.id)),
                Cell::text(places.join(", ")),
                Cell::text(t(&state, status_message(day.status))),
                Cell::text(t(
                    &state,
                    if day.online_ordering {
                        "day-online"
                    } else {
                        "day-production-only"
                    },
                )),
                Cell::text(
                    day.charity_id
                        .and_then(|id| charity_names.get(&id).cloned())
                        .unwrap_or_else(|| t(&state, "day-no-charity")),
                ),
            ]);
        }
        let table = Table {
            id: "bakdagen".to_owned(),
            caption: t(&state, "days-caption"),
            head: vec![
                Cell::text(t(&state, "day-date")),
                Cell::text(t(&state, "day-location")),
                Cell::text(t(&state, "day-status")),
                Cell::text(t(&state, "day-kind")),
                Cell::text(t(&state, "day-charity")),
            ],
            rows,
        };
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &ListPage {
                layout: layout(&state, &session).await?,
                table,
                saved: flash.opgeslagen.is_some(),
                repeated: flash
                    .herhaald
                    .map(|n| t_count(&state, "day-repeated", n))
                    .unwrap_or_default(),
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub struct NewDayForm {
    pub date: String,
    pub location_id: String,
    pub bake_starts_at: String,
    pub production_only: bool,
    pub charity_id: String,
}

#[derive(Template)]
#[template(path = "staff/bake_day_new.html")]
struct NewPage<'a> {
    layout: Layout<'a>,
    form: NewDayForm,
    locations: Vec<SelectOption>,
    charities: Vec<SelectOption>,
    errors: Errors,
}

fn charity_options(list: &[Charity], selected: &str) -> Vec<SelectOption> {
    list.iter()
        .map(|c| SelectOption {
            value: c.id.get().to_string(),
            label: c.name.clone(),
            selected: c.id.get().to_string() == selected,
        })
        .collect()
}

async fn render_new(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    form: NewDayForm,
    errors: Errors,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let locations = pickup::list_locations(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?
        .into_iter()
        .map(|l| SelectOption {
            selected: l.id.get().to_string() == form.location_id,
            value: l.id.get().to_string(),
            label: l.name,
        })
        .collect();
    let active = charities::list_active(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    Ok(pages::render(
        state,
        status,
        &NewPage {
            layout: layout(state, session).await?,
            charities: charity_options(&active, &form.charity_id),
            locations,
            form,
            errors,
        },
    ))
}

pub async fn new_form(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let form = NewDayForm {
            date: String::new(),
            location_id: String::new(),
            bake_starts_at: String::new(),
            production_only: false,
            charity_id: String::new(),
        };
        render_new(&state, &session, StatusCode::OK, form, Errors::default()).await
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
        require(&state, &staff, Permission::ManageBakeDays)?;
        let form = NewDayForm {
            date: fields.text("date").to_owned(),
            location_id: fields.text("location_id").to_owned(),
            bake_starts_at: fields.text("bake_starts_at").to_owned(),
            production_only: fields.checked("production_only"),
            charity_id: fields.text("charity_id").to_owned(),
        };
        let mut errors = Errors::default();
        let date = parse_date(&form.date);
        if date.is_none() {
            errors.add("date", t(&state, "form-error-date"));
        }
        let location = form.location_id.parse::<i64>().ok();
        if location.is_none() {
            errors.add("location_id", t(&state, "day-error-location"));
        }
        let bake_starts_at = parse_local(&form.bake_starts_at);
        if bake_starts_at.is_none() {
            errors.add("bake_starts_at", t(&state, "form-error-datetime"));
        }
        let charity = optional_id(&form.charity_id);
        if charity.is_err() {
            errors.add("charity_id", t(&state, "day-error-charity-unknown"));
        }
        let input = match (date, location, bake_starts_at, charity) {
            (Some(date), Some(location), Some(bake_starts_at), Ok(charity))
                if errors.is_empty() =>
            {
                NewBakeDay {
                    date,
                    location_id: PickupLocationId::new(location),
                    bake_starts_at,
                    online_ordering: !form.production_only,
                    charity_id: charity.map(CharityId::new),
                }
            }
            _ => {
                let status = invalid_status(&headers);
                return render_new(&state, &session, status, form, errors).await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        match days::create(&mut conn, &input).await {
            Ok(day) => Ok(saved(&headers, &day_path(day.id))),
            Err(bakedays::Error::NotFound) => {
                errors.add("location_id", t(&state, "day-error-location"));
                let status = invalid_status(&headers);
                render_new(&state, &session, status, form, errors).await
            }
            Err(error) => match problem(&state, &error) {
                Some(problem) => {
                    let field = match problem.field {
                        "" | "cutoff_at" => "bake_starts_at",
                        other => other,
                    };
                    errors.add(field, problem.text);
                    let status = invalid_status(&headers);
                    render_new(&state, &session, status, form, errors).await
                }
                None => Err(fail(&state, &error).into()),
            },
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// The values of every form on the bake day page, as stored or as just posted.
pub struct DayForms {
    pub menu_publish_at: String,
    pub opens_at: String,
    pub cutoff_at: String,
    pub bake_starts_at: String,
    pub production_only: bool,
    pub charity_id: String,
    pub product_id: String,
    pub max_qty: String,
    pub pool_id: String,
    pub units: String,
    pub pool_name: String,
    pub pool_max: String,
    pub copy_date: String,
    pub weeks: String,
}

impl DayForms {
    fn of(day: &BakeDay) -> Self {
        let value = |at: Option<Timestamp>| at.map(local_value).unwrap_or_default();
        Self {
            menu_publish_at: value(day.menu_publish_at),
            opens_at: value(day.opens_at),
            cutoff_at: value(day.cutoff_at),
            bake_starts_at: local_value(day.bake_starts_at),
            production_only: !day.online_ordering,
            charity_id: day
                .charity_id
                .map(|id| id.get().to_string())
                .unwrap_or_default(),
            product_id: String::new(),
            max_qty: String::new(),
            pool_id: String::new(),
            units: "1".to_owned(),
            pool_name: String::new(),
            pool_max: String::new(),
            copy_date: String::new(),
            weeks: String::new(),
        }
    }
}

pub struct RangeRow {
    pub text: String,
    pub dialog: String,
    pub action: String,
    pub confirm: String,
}

pub struct PoolRow {
    pub field: String,
    pub label: String,
    pub value: String,
    pub action: String,
}

pub struct Action {
    pub dialog: String,
    pub label: String,
    pub text: String,
    pub action: String,
}

#[derive(Template)]
#[template(path = "staff/bake_day.html")]
struct DayPage<'a> {
    layout: Layout<'a>,
    heading: String,
    path: String,
    status: String,
    editable: bool,
    saved: bool,
    alerts: Vec<String>,
    problem: String,
    slots: Vec<String>,
    forms: DayForms,
    errors: Errors,
    charities: Vec<SelectOption>,
    product_options: Vec<SelectOption>,
    pool_options: Vec<SelectOption>,
    range: Vec<RangeRow>,
    pools: Vec<PoolRow>,
    actions: Vec<Action>,
}

/// What the page needs beyond the day itself: the posted form values and the refusal, if
/// the page answers a form that was not accepted.
pub struct Answer {
    forms: Option<DayForms>,
    errors: Errors,
    problem: String,
    saved: bool,
    alerts: Vec<String>,
}

impl Answer {
    fn plain(saved: bool) -> Self {
        Self {
            forms: None,
            errors: Errors::default(),
            problem: String::new(),
            saved,
            alerts: Vec::new(),
        }
    }

    fn refused(forms: Option<DayForms>, problem: Problem) -> Self {
        let mut errors = Errors::default();
        let general = if problem.field.is_empty() {
            problem.text
        } else {
            errors.add(problem.field, problem.text);
            String::new()
        };
        Self {
            forms,
            errors,
            problem: general,
            saved: false,
            alerts: Vec::new(),
        }
    }
}

fn product_name(names: &BTreeMap<ProductId, String>, id: ProductId) -> String {
    names
        .get(&id)
        .cloned()
        .unwrap_or_else(|| id.get().to_string())
}

fn remaining_text(state: &AppState, remaining: Option<Remaining>) -> String {
    match remaining {
        Some(Remaining::Pieces(n)) => t_count(state, "range-remaining", n),
        Some(Remaining::Unlimited) | None => t(state, "range-unlimited"),
    }
}

fn range_text(
    state: &AppState,
    name: &str,
    item: &BakeDayProduct,
    pools: &[CapacityPool],
    remaining: Option<Remaining>,
) -> String {
    let max = item.max_qty.map_or_else(
        || t(state, "range-no-max"),
        |n| t_count(state, "range-max", n),
    );
    let pool = item
        .capacity_pool_id
        .and_then(|id| pools.iter().find(|p| p.id == id))
        .map(|p| {
            t_with(
                state,
                "range-in-pool",
                &[
                    ("pool", p.name.clone()),
                    ("units", item.units_per_item.to_string()),
                ],
            )
        });
    let mut parts = vec![name.to_owned(), max];
    parts.extend(pool);
    parts.push(remaining_text(state, remaining));
    parts.join(" · ")
}

async fn lead_time_alert(
    state: &AppState,
    day: &BakeDay,
    names: &BTreeMap<ProductId, String>,
) -> Step<Option<String>> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let warning = days::lead_time_warning(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    Ok(warning.map(|w| {
        let format = |at: Timestamp| {
            let local = i18n::Amsterdam::new().local(at);
            t_with(
                state,
                "day-moment",
                &[
                    ("date", state.i18n.short_date(local.date())),
                    ("time", i18n::time(local.time())),
                ],
            )
        };
        t_with(
            state,
            "day-lead-time-warning",
            &[
                ("product", product_name(names, w.product_id)),
                ("hours", w.lead_time_hours.to_string()),
                ("cutoff", format(w.cutoff_at)),
                ("latest", format(w.latest_cutoff)),
            ],
        )
    }))
}

fn actions(state: &AppState, day: &BakeDay, date: &str) -> Vec<Action> {
    let path = day_path(day.id);
    let action = |name: &str, label: &str, text: &str| Action {
        dialog: format!("day-{name}"),
        label: t(state, label),
        text: t_with(state, text, &[("date", date.to_owned())]),
        action: format!("{path}/{name}"),
    };
    match (day.status, day.online_ordering) {
        (BakeDayStatus::Draft, true) => vec![action("open", "day-open", "day-open-text")],
        (BakeDayStatus::Open, _) => vec![action("close", "day-close", "day-close-text")],
        (BakeDayStatus::Draft, false) => vec![action(
            "close",
            "day-close-production",
            "day-close-production-text",
        )],
        _ => Vec::new(),
    }
}

async fn render_day(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    day: &BakeDay,
    answer: Answer,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let slots = days::list_slots(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let locations: BTreeMap<PickupLocationId, String> = pickup::list_locations(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?
        .into_iter()
        .map(|l| (l.id, l.name))
        .collect();
    let all_products = products::list_all(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    let names: BTreeMap<ProductId, String> = all_products
        .iter()
        .map(|p| (p.id, p.name_nl.clone()))
        .collect();
    let items = range::list_products(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let pools = range::list_pools(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let remaining: BTreeMap<ProductId, Remaining> =
        range::availability(&mut conn, day.id, &NoOrders)
            .await
            .map_err(|e| fail(state, &e))?
            .into_iter()
            .map(|a| (a.product_id, a.remaining))
            .collect();
    let mut choosable = charities::list_active(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    if let Some(current) = day.charity_id
        && !choosable.iter().any(|c| c.id == current)
    {
        let archived = charities::get(&mut conn, current)
            .await
            .map_err(|e| fail(state, &e))?;
        choosable.push(archived);
    }
    drop(conn);

    let forms = answer.forms.unwrap_or_else(|| DayForms::of(day));
    let date = state.i18n.long_date(day.date);
    let path = day_path(day.id);
    let slot_texts = slots
        .iter()
        .map(|slot| {
            let range = i18n::Amsterdam::new()
                .time_range(slot.starts_at, slot.ends_at)
                .unwrap_or_default();
            let place = locations
                .get(&slot.location_id)
                .cloned()
                .unwrap_or_default();
            format!("{range}, {place}")
        })
        .collect();
    let range_rows = items
        .iter()
        .map(|item| {
            let name = product_name(&names, item.product_id);
            RangeRow {
                text: range_text(
                    state,
                    &name,
                    item,
                    &pools,
                    remaining.get(&item.product_id).copied(),
                ),
                dialog: format!("remove-{}", item.product_id.get()),
                action: format!("{path}/range/{}/remove", item.product_id.get()),
                confirm: t_with(
                    state,
                    "range-remove-text",
                    &[("product", name), ("date", date.clone())],
                ),
            }
        })
        .collect();
    let pool_rows = pools
        .iter()
        .map(|p| PoolRow {
            field: format!("pool_{}", p.id.get()),
            label: p.name.clone(),
            value: p.max_units.to_string(),
            action: format!("{path}/pools/{}", p.id.get()),
        })
        .collect();
    let product_options = all_products
        .iter()
        .filter(|p| p.active)
        .map(|p| SelectOption {
            value: p.id.get().to_string(),
            label: p.name_nl.clone(),
            selected: p.id.get().to_string() == forms.product_id,
        })
        .collect();
    let pool_options = pools
        .iter()
        .map(|p| SelectOption {
            value: p.id.get().to_string(),
            label: p.name.clone(),
            selected: p.id.get().to_string() == forms.pool_id,
        })
        .collect();
    let mut alerts = answer.alerts;
    if let Some(warning) = lead_time_alert(state, day, &names).await? {
        alerts.push(warning);
    }
    Ok(pages::render(
        state,
        status,
        &DayPage {
            layout: layout(state, session).await?,
            heading: date.clone(),
            status: t(state, status_message(day.status)),
            editable: !day.status.is_final(),
            saved: answer.saved,
            alerts,
            problem: answer.problem,
            slots: slot_texts,
            charities: charity_options(&choosable, &forms.charity_id),
            forms,
            errors: answer.errors,
            product_options,
            pool_options,
            range: range_rows,
            pools: pool_rows,
            actions: actions(state, day, &date),
            path,
        },
    ))
}

pub async fn show(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(flash): Query<DayFlash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let mut answer = Answer::plain(flash.opgeslagen.is_some());
        if flash.doel.is_some() {
            answer.alerts.push(t(&state, "day-charity-dropped"));
        }
        render_day(&state, &session, StatusCode::OK, &day, answer).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Shows the page again with the refusal, or fails for errors that are ours.
async fn refused(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    day: &BakeDay,
    forms: Option<DayForms>,
    error: &bakedays::Error,
) -> Outcome {
    let Some(problem) = problem(state, error) else {
        return Err(bakedays_error(state, error).into());
    };
    let answer = Answer::refused(forms, problem);
    render_day(state, session, invalid_status(headers), day, answer).await
}

async fn field_error(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    day: &BakeDay,
    forms: DayForms,
    field: &'static str,
    text: String,
) -> Outcome {
    let answer = Answer::refused(Some(forms), Problem { field, text });
    render_day(state, session, invalid_status(headers), day, answer).await
}

pub async fn schedule(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let mut forms = DayForms::of(&day);
        forms.menu_publish_at = fields.text("menu_publish_at").to_owned();
        forms.opens_at = fields.text("opens_at").to_owned();
        forms.cutoff_at = fields.text("cutoff_at").to_owned();
        forms.bake_starts_at = fields.text("bake_starts_at").to_owned();
        forms.production_only = fields.checked("production_only");
        let mut errors = Errors::default();
        let mut optional = |field: &str, value: &str| -> Option<Timestamp> {
            if value.is_empty() {
                return None;
            }
            let parsed = parse_local(value);
            if parsed.is_none() {
                errors.add(field, t(&state, "form-error-datetime"));
            }
            parsed
        };
        let menu_publish_at = optional("menu_publish_at", &forms.menu_publish_at);
        let opens_at = optional("opens_at", &forms.opens_at);
        let cutoff_at = optional("cutoff_at", &forms.cutoff_at);
        let bake_starts_at = parse_local(&forms.bake_starts_at);
        if bake_starts_at.is_none() {
            errors.add("bake_starts_at", t(&state, "form-error-datetime"));
        }
        let Some(bake_starts_at) = bake_starts_at.filter(|_| errors.is_empty()) else {
            let answer = Answer {
                forms: Some(forms),
                errors,
                problem: String::new(),
                saved: false,
                alerts: Vec::new(),
            };
            return render_day(&state, &session, invalid_status(&headers), &day, answer).await;
        };
        let schedule = Schedule {
            online_ordering: !forms.production_only,
            menu_publish_at,
            opens_at,
            cutoff_at,
            bake_starts_at,
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        match days::update_schedule(&mut conn, day.id, &schedule).await {
            Ok(_) => Ok(saved(&headers, &day_path(day.id))),
            Err(error) => refused(&state, &session, &headers, &day, Some(forms), &error).await,
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn charity(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let mut forms = DayForms::of(&day);
        forms.charity_id = fields.text("charity_id").to_owned();
        let Ok(charity) = optional_id(&forms.charity_id) else {
            let text = t(&state, "day-error-charity-unknown");
            return field_error(&state, &session, &headers, &day, forms, "charity_id", text).await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        match days::set_charity(&mut conn, day.id, charity.map(CharityId::new), &NoOrders).await {
            Ok(_) => Ok(saved(&headers, &day_path(day.id))),
            Err(error) => refused(&state, &session, &headers, &day, Some(forms), &error).await,
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn set_product(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let mut forms = DayForms::of(&day);
        forms.product_id = fields.text("product_id").to_owned();
        forms.max_qty = fields.text("max_qty").to_owned();
        forms.pool_id = fields.text("pool_id").to_owned();
        forms.units = fields.text("units").to_owned();
        let Ok(Some(product)) = optional_id(&forms.product_id) else {
            let text = t(&state, "range-error-product");
            return field_error(&state, &session, &headers, &day, forms, "product_id", text).await;
        };
        let max_qty = if forms.max_qty.is_empty() {
            None
        } else if let Some(n) = parse_count(&forms.max_qty) {
            Some(n)
        } else {
            let text = t(&state, "form-error-count");
            return field_error(&state, &session, &headers, &day, forms, "max_qty", text).await;
        };
        let Ok(pool) = optional_id(&forms.pool_id) else {
            let text = t(&state, "range-error-pool");
            return field_error(&state, &session, &headers, &day, forms, "pool_id", text).await;
        };
        let Some(units) = parse_count(&forms.units).filter(|n| *n > 0) else {
            let text = t(&state, "range-error-units");
            return field_error(&state, &session, &headers, &day, forms, "units", text).await;
        };
        let item = BakeDayProduct {
            product_id: ProductId::new(product),
            max_qty,
            capacity_pool_id: pool.map(CapacityPoolId::new),
            units_per_item: units,
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        match range::set_product(&mut conn, day.id, &item).await {
            Ok(()) => Ok(saved(&headers, &day_path(day.id))),
            Err(error) => refused(&state, &session, &headers, &day, Some(forms), &error).await,
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn remove_product(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, product)): Path<(String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let product = ProductId::new(path_id(&state, &product)?);
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        match range::remove_product(&mut conn, day.id, product).await {
            Ok(()) => Ok(saved(&headers, &day_path(day.id))),
            Err(error) => transition_refused(&state, &session, &headers, &day, &error).await,
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn create_pool(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let mut forms = DayForms::of(&day);
        forms.pool_name = fields.text("pool_name").to_owned();
        forms.pool_max = fields.text("pool_max").to_owned();
        let Some(max) = parse_count(&forms.pool_max) else {
            let text = t(&state, "form-error-count");
            return field_error(&state, &session, &headers, &day, forms, "pool_max", text).await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        match range::create_pool(&mut conn, day.id, &forms.pool_name, max).await {
            Ok(_) => Ok(saved(&headers, &day_path(day.id))),
            Err(error) => refused(&state, &session, &headers, &day, Some(forms), &error).await,
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn resize_pool(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, pool)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let pool = CapacityPoolId::new(path_id(&state, &pool)?);
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let pools = range::list_pools(&mut conn, day.id)
            .await
            .map_err(|e| fail(&state, &e))?;
        if !pools.iter().any(|p| p.id == pool) {
            return Err(not_found(&state).into());
        }
        let field = format!("pool_{}", pool.get());
        let Some(max) = parse_count(fields.text(&field)) else {
            drop(conn);
            let answer = Answer::refused(
                None,
                Problem {
                    field: "",
                    text: t(&state, "pool-error-size"),
                },
            );
            return render_day(&state, &session, invalid_status(&headers), &day, answer).await;
        };
        match range::resize_pool(&mut conn, pool, max).await {
            Ok(()) => Ok(saved(&headers, &day_path(day.id))),
            Err(error) => {
                drop(conn);
                refused(&state, &session, &headers, &day, None, &error).await
            }
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn copy(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let mut forms = DayForms::of(&day);
        forms.copy_date = fields.text("copy_date").to_owned();
        let Some(date) = parse_date(&forms.copy_date) else {
            let text = t(&state, "form-error-date");
            return field_error(&state, &session, &headers, &day, forms, "copy_date", text).await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        match days::copy_to(&mut conn, day.id, date).await {
            Ok(copied) => {
                let mut path = day_path(copied.day.id);
                if copied.charity_dropped {
                    path.push_str("?doel=vervallen");
                }
                Ok(saved(&headers, &path))
            }
            Err(error) => {
                drop(conn);
                let Some(mut problem) = problem(&state, &error) else {
                    return Err(bakedays_error(&state, &error).into());
                };
                if matches!(problem.field, "date" | "") {
                    problem.field = "copy_date";
                }
                let answer = Answer::refused(Some(forms), problem);
                render_day(&state, &session, invalid_status(&headers), &day, answer).await
            }
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn repeat(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let mut forms = DayForms::of(&day);
        forms.weeks = fields.text("weeks").to_owned();
        let Some(weeks) = forms.weeks.parse::<u8>().ok() else {
            let text = t(&state, "day-error-weeks");
            return field_error(&state, &session, &headers, &day, forms, "weeks", text).await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        match days::repeat_weekly(&mut conn, day.id, weeks).await {
            Ok(copies) => {
                let count = copies.len();
                Ok(saved(
                    &headers,
                    &format!("/staff/bake-days?herhaald={count}"),
                ))
            }
            Err(error) => {
                drop(conn);
                let Some(mut problem) = problem(&state, &error) else {
                    return Err(bakedays_error(&state, &error).into());
                };
                if matches!(problem.field, "date" | "") {
                    problem.field = "weeks";
                }
                let answer = Answer::refused(Some(forms), problem);
                render_day(&state, &session, invalid_status(&headers), &day, answer).await
            }
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[derive(Template)]
#[template(path = "staff/alert.html")]
struct AlertFragment<'a> {
    layout: Layout<'a>,
    text: String,
}

/// A refused action from a confirmation dialog: htmx gets the alert in the dialog's
/// result region, a plain post gets the page with the alert.
/// The alert alone, for a confirmation dialog's result region.
pub fn alert_fragment(state: &AppState, text: String) -> Response {
    pages::render(
        state,
        StatusCode::OK,
        &AlertFragment {
            layout: Layout::new(state),
            text,
        },
    )
}

async fn transition_refused(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    day: &BakeDay,
    error: &bakedays::Error,
) -> Outcome {
    let Some(problem) = problem(state, error) else {
        return Err(bakedays_error(state, error).into());
    };
    if is_htmx(headers) {
        return Ok(alert_fragment(state, problem.text));
    }
    let answer = Answer::refused(
        None,
        Problem {
            field: "",
            text: problem.text,
        },
    );
    render_day(state, session, StatusCode::CONFLICT, day, answer).await
}

#[derive(Clone, Copy)]
enum Change {
    Open,
    Close,
}

async fn transition(
    state: AppState,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    id: String,
    change: Change,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let day = load(&state, &id).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let clock = state.clock.as_ref();
        let result = match change {
            Change::Open => days::open(&mut conn, clock, day.id).await,
            Change::Close => days::close(&mut conn, clock, day.id).await,
        };
        drop(conn);
        match result {
            Ok(_) => Ok(saved(&headers, &day_path(day.id))),
            Err(error) => transition_refused(&state, &session, &headers, &day, &error).await,
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn open(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    transition(state, staff, session, headers, id, Change::Open).await
}

pub async fn close(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    transition(state, staff, session, headers, id, Change::Close).await
}

/// Products by id, for the extra production page.
pub async fn product_names(state: &AppState) -> Step<(BTreeMap<ProductId, String>, Vec<Product>)> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let all = products::list_all(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    Ok((all.iter().map(|p| (p.id, p.name_nl.clone())).collect(), all))
}
