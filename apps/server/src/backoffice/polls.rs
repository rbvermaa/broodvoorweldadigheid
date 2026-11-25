//! Bread votes (F9b, ADR 0039): create a poll, see its results and the wishes, and
//! change title, drop, closing time and whether results show on the site.

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Router;
use axum::routing::get;
use bakedays::days;
use catalog::products;
use domain::{BakeDayId, PollId, ProductId};
use jiff::Timestamp;
use notify::polls::{self, Invalid, NewOption, Poll, PollError, PollInput};
use tower_sessions::Session;

use super::form::{Errors, Fields, local_value, parse_local};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require,
    saved, t, t_with,
};
use crate::components::{BarChart, Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(PATH, get(list))
        .route("/staff/polls/new", get(new_form).post(create))
        .route("/staff/polls/{id}", get(show).post(update))
}

pub const PATH: &str = "/staff/polls";
/// Drops from this many days back can still be picked.
const DAYS_BACK: i64 = 7;
const DAYS_AHEAD: i64 = 366;

fn poll_path(id: PollId) -> String {
    format!("{PATH}/{}", id.get())
}

/// The settings every poll has, as typed.
pub struct SettingsForm {
    pub title: String,
    pub closes_at: String,
    pub bake_day: String,
    pub public: bool,
}

impl SettingsForm {
    fn empty() -> Self {
        Self {
            title: String::new(),
            closes_at: String::new(),
            bake_day: String::new(),
            public: false,
        }
    }

    fn of(poll: &Poll) -> Self {
        Self {
            title: poll.title.clone(),
            closes_at: local_value(poll.closes_at),
            bake_day: poll
                .bake_day
                .map(|d| d.get().to_string())
                .unwrap_or_default(),
            public: poll.public_results,
        }
    }

    fn posted(fields: &Fields) -> Self {
        Self {
            title: fields.text("title").to_owned(),
            closes_at: fields.text("closes_at").to_owned(),
            bake_day: fields.text("bake_day").to_owned(),
            public: fields.checked("public"),
        }
    }

    /// The input, or the errors; `days` are the drops that may be picked.
    fn input(&self, state: &AppState, days: &[DayChoice]) -> Result<PollInput, Errors> {
        let mut errors = Errors::default();
        if self.title.is_empty() || self.title.chars().count() > polls::MAX_TITLE_CHARS {
            errors.add("title", t(state, "poll-error-title"));
        }
        let closes_at = parse_local(&self.closes_at);
        if closes_at.is_none() {
            errors.add("closes_at", t(state, "poll-error-closes"));
        }
        let bake_day = if self.bake_day.is_empty() {
            None
        } else {
            let found = days.iter().find(|d| d.value == self.bake_day);
            if found.is_none() {
                errors.add("bake_day", t(state, "poll-error-bake-day"));
            }
            found.map(|d| d.id)
        };
        match closes_at {
            Some(closes_at) if errors.is_empty() => Ok(PollInput {
                title: self.title.clone(),
                bake_day,
                closes_at,
                public_results: self.public,
            }),
            _ => Err(errors),
        }
    }
}

pub struct DayChoice {
    id: BakeDayId,
    value: String,
    label: String,
}

/// A product that can be a candidate: its checkbox name and label.
pub struct ProductChoice {
    pub field: String,
    pub name: String,
    pub checked: bool,
    id: ProductId,
}

#[derive(Template)]
#[template(path = "staff/polls.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    table: Table,
}

#[derive(Template)]
#[template(path = "staff/poll_new.html")]
struct NewPage<'a> {
    layout: Layout<'a>,
    form: SettingsForm,
    days: Vec<SelectOption>,
    products: Vec<ProductChoice>,
    options: String,
    errors: Errors,
}

#[derive(Template)]
#[template(path = "staff/poll.html")]
struct PollPage<'a> {
    layout: Layout<'a>,
    title: String,
    status: String,
    total: i64,
    chart: BarChart,
    wishes: Vec<String>,
    action: String,
    site_link: String,
    form: SettingsForm,
    days: Vec<SelectOption>,
    errors: Errors,
    saved: bool,
}

fn poll_error(state: &AppState, error: &PollError) -> Response {
    match error {
        PollError::NotFound => not_found(state),
        other => fail(state, other),
    }
}

fn status_text(state: &AppState, closes_at: Timestamp) -> String {
    let id = if state.clock.now() < closes_at {
        "poll-open"
    } else {
        "poll-closed"
    };
    t(state, id)
}

fn moment(state: &AppState, at: Timestamp) -> String {
    let zone = i18n::Amsterdam::new();
    t_with(
        state,
        "calendar-deadline-at",
        &[
            ("date", state.i18n.short_date(zone.date(at))),
            ("time", zone.time(at)),
        ],
    )
}

/// Drops from a week ago to a year ahead, and the drop `current` is tied to.
async fn day_choices(state: &AppState, current: Option<&Poll>) -> Step<Vec<DayChoice>> {
    let today = state.clock.now_in_bakery().date();
    let from = today.saturating_sub(jiff::Span::new().days(DAYS_BACK));
    let to = today.saturating_add(jiff::Span::new().days(DAYS_AHEAD));
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let list = days::list_between(&mut conn, from, to)
        .await
        .map_err(|e| fail(state, &e))?;
    let mut choices: Vec<DayChoice> = list
        .into_iter()
        .map(|d| DayChoice {
            id: d.id,
            value: d.id.get().to_string(),
            label: state.i18n.long_date(d.date),
        })
        .collect();
    if let Some((id, date)) = current.and_then(|p| p.bake_day.zip(p.bake_date))
        && !choices.iter().any(|d| d.id == id)
    {
        choices.insert(
            0,
            DayChoice {
                id,
                value: id.get().to_string(),
                label: state.i18n.long_date(date),
            },
        );
    }
    Ok(choices)
}

fn day_options(days: &[DayChoice], chosen: &str) -> Vec<SelectOption> {
    days.iter()
        .map(|d| SelectOption {
            value: d.value.clone(),
            label: d.label.clone(),
            selected: d.value == chosen,
        })
        .collect()
}

pub async fn list(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::Polls)?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = polls::list(&mut conn).await.map_err(|e| fail(&state, &e))?;
        let table = Table {
            id: "stemmingen".to_owned(),
            caption: t(&state, "polls-caption"),
            head: vec![
                Cell::text(t(&state, "poll-title")),
                Cell::text(t(&state, "poll-closes")),
                Cell::text(t(&state, "poll-status")),
                Cell::number(t(&state, "poll-votes")),
            ],
            rows: all
                .iter()
                .map(|p| {
                    vec![
                        Cell::link(p.title.clone(), poll_path(p.id)),
                        Cell::text(moment(&state, p.closes_at)),
                        Cell::text(status_text(&state, p.closes_at)),
                        Cell::number(p.votes.to_string()),
                    ]
                })
                .collect(),
        };
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &ListPage {
                layout: layout(&state, &session).await?,
                table,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

async fn product_choices(state: &AppState, fields: Option<&Fields>) -> Step<Vec<ProductChoice>> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let active = products::list_active(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    Ok(active
        .into_iter()
        .map(|p| {
            let field = format!("product-{}", p.id.get());
            ProductChoice {
                checked: fields.is_some_and(|f| f.checked(&field)),
                field,
                name: p.name_nl,
                id: p.id,
            }
        })
        .collect())
}

async fn render_new(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    form: SettingsForm,
    products: Vec<ProductChoice>,
    options: String,
    errors: Errors,
) -> Outcome {
    let days = day_choices(state, None).await?;
    let days = day_options(&days, &form.bake_day);
    Ok(pages::render(
        state,
        status,
        &NewPage {
            layout: layout(state, session).await?,
            form,
            days,
            products,
            options,
            errors,
        },
    ))
}

pub async fn new_form(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::Polls)?;
        let products = product_choices(&state, None).await?;
        render_new(
            &state,
            &session,
            StatusCode::OK,
            SettingsForm::empty(),
            products,
            String::new(),
            Errors::default(),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

fn invalid_message(state: &AppState, invalid: Invalid) -> (&'static str, String) {
    match invalid {
        Invalid::Title => ("title", t(state, "poll-error-title")),
        Invalid::ClosesInPast => ("closes_at", t(state, "poll-error-closes")),
        Invalid::TooFewOptions => ("options", t(state, "poll-error-few")),
        Invalid::TooManyOptions => ("options", t(state, "poll-error-many")),
        Invalid::OptionName => ("options", t(state, "poll-error-option")),
        Invalid::DuplicateOption => ("options", t(state, "poll-error-duplicate")),
        Invalid::Reopen => ("closes_at", t(state, "poll-error-reopen")),
    }
}

pub async fn create(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Polls)?;
        let form = SettingsForm::posted(&fields);
        let products = product_choices(&state, Some(&fields)).await?;
        let options_text = fields.raw("options").to_owned();
        let mut candidates: Vec<NewOption> = products
            .iter()
            .filter(|p| p.checked)
            .map(|p| NewOption {
                product: Some(p.id),
                name: p.name.clone(),
            })
            .collect();
        candidates.extend(
            options_text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| NewOption {
                    product: None,
                    name: line.to_owned(),
                }),
        );
        let days = day_choices(&state, None).await?;
        let errors = match form.input(&state, &days) {
            Ok(input) => {
                let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
                match polls::create(&mut conn, state.clock.as_ref(), &input, &candidates).await {
                    Ok(id) => return Ok(saved(&headers, &poll_path(id))),
                    Err(PollError::Invalid(invalid)) => {
                        let mut errors = Errors::default();
                        let (field, text) = invalid_message(&state, invalid);
                        errors.add(field, text);
                        errors
                    }
                    Err(other) => return Err(fail(&state, &other).into()),
                }
            }
            Err(errors) => errors,
        };
        render_new(
            &state,
            &session,
            invalid_status(&headers),
            form,
            products,
            options_text,
            errors,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

async fn load(state: &AppState, raw: &str) -> Step<Poll> {
    let id = PollId::new(path_id(state, raw)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    polls::get(&mut conn, id)
        .await
        .map_err(|e| poll_error(state, &e).into())
}

async fn render_poll(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    poll: &Poll,
    form: SettingsForm,
    errors: Errors,
    saved: bool,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let tallies = polls::results(&mut conn, poll.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let wishes = polls::wishes(&mut conn, poll.id)
        .await
        .map_err(|e| fail(state, &e))?;
    drop(conn);
    let total: i64 = tallies.iter().map(|t| t.votes).sum();
    let chart = BarChart::new(
        "uitslag",
        t(state, "poll-results-caption"),
        tallies
            .into_iter()
            .map(|t| (t.name, t.votes, t.votes.to_string()))
            .collect(),
    );
    let days = day_choices(state, Some(poll)).await?;
    let status_line = format!(
        "{} · {} · {}",
        status_text(state, poll.closes_at),
        moment(state, poll.closes_at),
        t(
            state,
            if poll.public_results {
                "poll-public-yes"
            } else {
                "poll-public-no"
            }
        ),
    );
    Ok(pages::render(
        state,
        status,
        &PollPage {
            layout: layout(state, session).await?,
            title: poll.title.clone(),
            status: status_line,
            total,
            chart,
            wishes,
            action: poll_path(poll.id),
            site_link: format!("/stemmen/{}", poll.id.get()),
            days: day_options(&days, &form.bake_day),
            form,
            errors,
            saved,
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
        require(&state, &staff, Permission::Polls)?;
        let poll = load(&state, &id).await?;
        let form = SettingsForm::of(&poll);
        render_poll(
            &state,
            &session,
            StatusCode::OK,
            &poll,
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
        require(&state, &staff, Permission::Polls)?;
        let poll = load(&state, &id).await?;
        let form = SettingsForm::posted(&fields);
        let days = day_choices(&state, Some(&poll)).await?;
        let errors = match form.input(&state, &days) {
            Ok(input) => {
                let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
                match polls::update(&mut conn, state.clock.as_ref(), poll.id, &input).await {
                    Ok(()) => return Ok(saved(&headers, &poll_path(poll.id))),
                    Err(PollError::Invalid(invalid)) => {
                        let mut errors = Errors::default();
                        let (field, text) = invalid_message(&state, invalid);
                        errors.add(field, text);
                        errors
                    }
                    Err(other) => return Err(poll_error(&state, &other).into()),
                }
            }
            Err(errors) => errors,
        };
        render_poll(
            &state,
            &session,
            invalid_status(&headers),
            &poll,
            form,
            errors,
            false,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
