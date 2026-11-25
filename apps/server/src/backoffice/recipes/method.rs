//! The instructions of a recipe version (F14, ADR 0049): read with clock times from a
//! start time, and on a draft edited as a free text or one step at a time through htmx.

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use jiff::Zoned;
use recipes::method::{
    self as store, MAX_DURATION_MIN, MAX_STEP_NAME, MAX_STEP_NOTES, MAX_STEPS, MAX_TEMPERATURE_C,
    MIN_TEMPERATURE_C, clean_text,
};
use recipes::schedule::{days_after, schedule, start_on};
use recipes::{Method, MethodKind, Step as RecipeStep, StepEdit};
use tower_sessions::Session;

use super::super::form::{Errors, Fields, parse_time};
use super::super::{
    Outcome, Rejection, Step, fail, is_htmx, layout, not_found, require, saved, t, t_count, t_with,
};
use super::editor::{EditView, Reply, load, refused_status, render_edit};
use super::{Message, version_path};
use crate::pages;
use crate::staff::Staff;
use crate::state::AppState;

/// One fact about a step, with its icon.
pub struct Fact {
    pub icon: &'static str,
    pub label: String,
    pub text: String,
}

pub struct StepView {
    pub time: String,
    /// "+1 dag" when the step starts on a later day than the start; else empty.
    pub later: String,
    pub name: String,
    pub facts: Vec<Fact>,
    pub notes: Vec<String>,
}

pub struct ScheduleView {
    pub steps: Vec<StepView>,
    pub end: String,
    pub end_later: String,
    /// Why the end leaves out some steps; empty when every step has a duration.
    pub untimed: String,
    pub start: String,
    pub start_hint: String,
    pub start_error: String,
}

/// The instructions as a page shows them; nothing at all when the version has none.
pub struct MethodView {
    /// The page the start time reloads.
    pub page: String,
    /// The form the start field belongs to; empty for a form of its own.
    pub form: String,
    pub paragraphs: Vec<Vec<String>>,
    pub schedule: Option<ScheduleView>,
}

impl MethodView {
    pub fn shown(&self) -> bool {
        !self.paragraphs.is_empty() || self.schedule.is_some()
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Paragraphs split on blank lines, each a list of lines; plain text only.
fn paragraphs(text: &str) -> Vec<Vec<String>> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(lines)
        .collect()
}

fn duration_text(state: &AppState, minutes: u32) -> String {
    let (hours, min) = (minutes / 60, minutes % 60);
    match (hours, min) {
        (0, min) => t_with(
            state,
            "method-duration-minutes",
            &[("min", min.to_string())],
        ),
        (hours, 0) => t_with(
            state,
            "method-duration-hours",
            &[("hours", hours.to_string())],
        ),
        (hours, min) => t_with(
            state,
            "method-duration-hours-minutes",
            &[("hours", hours.to_string()), ("min", min.to_string())],
        ),
    }
}

fn facts(state: &AppState, step: &RecipeStep) -> Vec<Fact> {
    let duration = step.duration_min.map(|minutes| Fact {
        icon: "clock",
        label: t(state, "method-step-duration-label"),
        text: duration_text(state, minutes),
    });
    let temperature = step.temperature_c.map(|c| Fact {
        icon: "thermometer",
        label: t(state, "method-step-temperature-label"),
        text: t_with(state, "method-temperature", &[("c", c.to_string())]),
    });
    duration.into_iter().chain(temperature).collect()
}

fn later(state: &AppState, start: &Zoned, at: &Zoned) -> String {
    match u32::try_from(days_after(start, at)) {
        Ok(0) | Err(_) => String::new(),
        Ok(days) => t_count(state, "method-days-later", days),
    }
}

/// The start a page asked for in `start` (`HH:MM`) on the day of `base`, else `base`.
fn start_time(state: &AppState, fields: &Fields, base: &Zoned) -> (Zoned, String, String) {
    let raw = fields.text("start");
    if raw.is_empty() {
        return (base.clone(), i18n::time(base.time()), String::new());
    }
    match parse_time(raw) {
        Some(time) => (start_on(base, time), raw.to_owned(), String::new()),
        None => (base.clone(), raw.to_owned(), t(state, "method-start-error")),
    }
}

/// What a page needs to show the instructions read-only.
pub struct Shown<'a> {
    pub page: String,
    pub form: &'a str,
    /// The start when none is asked: now, or when a batch was mixed.
    pub base: &'a Zoned,
}

pub fn view(state: &AppState, method: &Method, fields: &Fields, shown: &Shown<'_>) -> MethodView {
    let mut out = MethodView {
        page: shown.page.clone(),
        form: shown.form.to_owned(),
        paragraphs: Vec::new(),
        schedule: None,
    };
    match method {
        Method::None => {}
        Method::Text(text) => out.paragraphs = paragraphs(text),
        Method::Steps(steps) if steps.is_empty() => {}
        Method::Steps(steps) => {
            let (start, value, error) = start_time(state, fields, shown.base);
            let durations: Vec<Option<u32>> = steps.iter().map(|s| s.duration_min).collect();
            let Some(plan) = schedule(&start, &durations) else {
                return out;
            };
            let clock = |at: &Zoned| i18n::time(at.time());
            out.schedule = Some(ScheduleView {
                steps: steps
                    .iter()
                    .zip(&plan.starts)
                    .map(|(step, at)| StepView {
                        time: clock(at),
                        later: later(state, &start, at),
                        name: step.name.clone(),
                        facts: facts(state, step),
                        notes: lines(&step.notes),
                    })
                    .collect(),
                end: clock(&plan.end),
                end_later: later(state, &start, &plan.end),
                untimed: match u32::try_from(plan.untimed) {
                    Ok(0) | Err(_) => String::new(),
                    Ok(n) => t_count(state, "method-untimed", n),
                },
                start: value,
                start_hint: t_with(
                    state,
                    "method-start-hint",
                    &[("date", state.i18n.long_date(start.date()))],
                ),
                start_error: error,
            });
        }
    }
    out
}

/// The fields of one step in the edit form, as stored or as typed.
pub struct StepFields {
    pub name_field: String,
    pub duration_field: String,
    pub temperature_field: String,
    pub notes_field: String,
    pub name: String,
    pub duration: String,
    pub temperature: String,
    pub notes: String,
    pub errors: Errors,
}

impl StepFields {
    fn prefix(position: Option<usize>) -> String {
        position.map_or_else(|| "stap-nieuw".to_owned(), |p| format!("stap-{p}"))
    }

    fn named(position: Option<usize>, step: &RecipeStep) -> Self {
        let prefix = Self::prefix(position);
        Self {
            name_field: format!("{prefix}-naam"),
            duration_field: format!("{prefix}-duur"),
            temperature_field: format!("{prefix}-temperatuur"),
            notes_field: format!("{prefix}-notities"),
            name: step.name.clone(),
            duration: step.duration_min.map(|d| d.to_string()).unwrap_or_default(),
            temperature: step
                .temperature_c
                .map(|c| c.to_string())
                .unwrap_or_default(),
            notes: step.notes.clone(),
            errors: Errors::default(),
        }
    }

    /// The step as posted, with an error per field that is not right.
    fn posted(
        state: &AppState,
        fields: &Fields,
        position: Option<usize>,
    ) -> (Self, Option<RecipeStep>) {
        let mut out = Self::named(position, &RecipeStep::default());
        out.name = fields.text(&out.name_field).to_owned();
        out.duration = fields.text(&out.duration_field).to_owned();
        out.temperature = fields.text(&out.temperature_field).to_owned();
        out.notes = clean_text(fields.raw(&out.notes_field));
        if out.name.is_empty() || out.name.chars().count() > MAX_STEP_NAME {
            out.errors
                .add(&out.name_field, t(state, "method-error-step-name"));
        }
        let duration = optional(&out.duration, 1, MAX_DURATION_MIN);
        if duration.is_none() {
            out.errors
                .add(&out.duration_field, t(state, "method-error-duration"));
        }
        let temperature = optional(&out.temperature, MIN_TEMPERATURE_C, MAX_TEMPERATURE_C);
        if temperature.is_none() {
            out.errors
                .add(&out.temperature_field, t(state, "method-error-temperature"));
        }
        if out.notes.chars().count() > MAX_STEP_NOTES {
            out.errors
                .add(&out.notes_field, t(state, "method-error-notes"));
        }
        let step = match (duration, temperature) {
            (Some(duration_min), Some(temperature_c)) if out.errors.is_empty() => {
                Some(RecipeStep {
                    name: out.name.clone(),
                    duration_min,
                    temperature_c,
                    notes: out.notes.clone(),
                })
            }
            _ => None,
        };
        (out, step)
    }
}

/// An optional whole number in a range: `Some(None)` when empty, `None` when wrong.
fn optional<T>(raw: &str, min: T, max: T) -> Option<Option<T>>
where
    T: std::str::FromStr + PartialOrd,
{
    if raw.is_empty() {
        return Some(None);
    }
    raw.parse::<T>()
        .ok()
        .filter(|n| *n >= min && *n <= max)
        .map(Some)
}

pub struct StepRow {
    /// The id of the row's form; its cells point at it with `form=`.
    pub form: String,
    pub time: String,
    pub later: String,
    pub fields: StepFields,
    pub notes_open: bool,
    pub name_label: String,
    pub duration_label: String,
    pub temperature_label: String,
    pub notes_label: String,
    pub action: String,
    pub remove: String,
    /// Empty for the first step.
    pub up: String,
    /// Empty for the last step.
    pub down: String,
    pub up_label: String,
    pub down_label: String,
    pub remove_label: String,
}

/// A switch to another kind of instructions. It asks first when content would go.
pub struct Switch {
    pub kind: &'static str,
    pub label: String,
    pub dialog: String,
    pub text: String,
    pub confirm: String,
}

pub struct MethodEditor {
    pub kind: &'static str,
    /// The [`store::stamp`] of the instructions shown, posted back with every change.
    pub stamp: String,
    /// The dough weight the page shows, kept across a change.
    pub grams: String,
    pub kind_action: String,
    pub text_action: String,
    pub text: String,
    pub text_error: String,
    pub steps: Vec<StepRow>,
    pub end: String,
    pub end_later: String,
    /// The form for a new step; `None` when the version has the most steps it may have.
    pub add: Option<StepFields>,
    pub add_action: String,
    pub switches: Vec<Switch>,
    /// Content would go on a switch, so each switch asks first.
    pub asks: bool,
    pub notice: Option<Message>,
    pub saved: bool,
}

/// What the edit page shows of the instructions after a change, or what went wrong.
#[derive(Default)]
pub struct MethodState {
    pub notice: Option<Message>,
    pub saved: bool,
    pub text: Option<(String, String)>,
    /// A step as typed, and where: `None` for the new step.
    pub step: Option<(Option<usize>, StepFields)>,
    /// The stamp the typed step was made on, posted back instead of the current one so a
    /// stale page stays stale.
    pub seen: Option<String>,
}

fn switch(state: &AppState, from: MethodKind, to: MethodKind) -> Switch {
    let (label, text, confirm) = match to {
        MethodKind::None => (
            "method-remove",
            "method-remove-text",
            "method-remove-confirm",
        ),
        MethodKind::Text => (
            "method-to-text",
            "method-to-text-text",
            "method-switch-confirm",
        ),
        MethodKind::Steps => (
            "method-to-steps",
            "method-to-steps-text",
            "method-switch-confirm",
        ),
    };
    Switch {
        kind: to.code(),
        label: t(state, label),
        dialog: format!("werkwijze-{}-{}", from.code(), to.code()),
        text: t(state, text),
        confirm: t(state, confirm),
    }
}

pub fn editor(
    state: &AppState,
    base: &str,
    grams: String,
    method: &Method,
    view: MethodState,
    start: &Zoned,
) -> MethodEditor {
    let MethodState {
        notice,
        saved,
        text,
        step,
        seen,
    } = view;
    let (mut typed_at, mut typed_add) = (None, None);
    if let Some((position, fields)) = step {
        match position {
            Some(position) => typed_at = Some((position, fields)),
            None => typed_add = Some(fields),
        }
    }
    let steps: &[RecipeStep] = match method {
        Method::Steps(steps) => steps,
        _ => &[],
    };
    let last = steps.len().saturating_sub(1);
    let durations: Vec<Option<u32>> = steps.iter().map(|s| s.duration_min).collect();
    let plan = schedule(start, &durations);
    let at = |index: usize| {
        plan.as_ref()
            .and_then(|p| p.starts.get(index))
            .map_or((String::new(), String::new()), |z| {
                (i18n::time(z.time()), later(state, start, z))
            })
    };
    let (end, end_later) = plan.as_ref().map_or((String::new(), String::new()), |p| {
        (i18n::time(p.end.time()), later(state, start, &p.end))
    });
    let rows = steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let fields = match typed_at.take() {
                Some((at, fields)) if at == index => fields,
                other => {
                    typed_at = other;
                    StepFields::named(Some(index), step)
                }
            };
            let named = |id: &str| t_with(state, id, &[("name", step.name.clone())]);
            let numbered = |id: &str| t_with(state, id, &[("n", (index + 1).to_string())]);
            let action = format!("{base}/steps/{index}");
            let (time, later) = at(index);
            StepRow {
                form: format!("stap-{index}"),
                time,
                later,
                notes_open: !fields.errors.get(&fields.notes_field).is_empty(),
                name_label: numbered("method-cell-name"),
                duration_label: numbered("method-cell-duration"),
                temperature_label: numbered("method-cell-temperature"),
                notes_label: numbered("method-cell-notes"),
                fields,
                remove: format!("{action}/remove"),
                up: if index > 0 {
                    format!("{action}/up")
                } else {
                    String::new()
                },
                down: if index < last {
                    format!("{action}/down")
                } else {
                    String::new()
                },
                up_label: named("method-step-up"),
                down_label: named("method-step-down"),
                remove_label: named("method-step-remove"),
                action,
            }
        })
        .collect();
    let kind = method.kind();
    let (text, text_error) = text.unwrap_or_else(|| {
        let stored = match method {
            Method::Text(text) => text.clone(),
            _ => String::new(),
        };
        (stored, String::new())
    });
    MethodEditor {
        kind: kind.code(),
        stamp: seen.unwrap_or_else(|| store::stamp(method)),
        grams,
        kind_action: format!("{base}/method"),
        text_action: format!("{base}/text"),
        text,
        text_error,
        steps: rows,
        end,
        end_later,
        add: (steps.len() < MAX_STEPS)
            .then(|| typed_add.unwrap_or_else(|| StepFields::named(None, &RecipeStep::default()))),
        add_action: format!("{base}/steps"),
        switches: MethodKind::ALL
            .into_iter()
            .filter(|to| *to != kind)
            .map(|to| switch(state, kind, to))
            .collect(),
        asks: !method.is_empty(),
        notice,
        saved,
    }
}

fn error_text(state: &AppState, error: &recipes::Error) -> Option<String> {
    let id = match error {
        recipes::Error::Frozen => "recipe-error-frozen",
        recipes::Error::Stale => "method-error-stale",
        recipes::Error::Invalid("step") => "method-error-gone",
        recipes::Error::Invalid("steps") => "method-error-steps",
        recipes::Error::Invalid("method_text") => "method-error-text",
        recipes::Error::Invalid(_) => "method-error-step",
        _ => return None,
    };
    Some(t(state, id))
}

#[derive(Template)]
#[template(path = "staff/method_refused.html")]
struct Refused<'a> {
    layout: pages::Layout<'a>,
    text: String,
}

/// The route parameters and form of a change to the instructions.
pub struct Posted {
    id: String,
    version: String,
    fields: Fields,
}

/// A change to make, with what the page showed if it is refused.
enum Change {
    Kind(MethodKind),
    Text(String),
    Step(Option<usize>, StepEdit),
}

async fn change(
    state: AppState,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    posted: Posted,
    planned: impl FnOnce(&AppState, &Fields) -> Result<Change, Box<MethodState>>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let Posted {
            id,
            version,
            fields,
        } = posted;
        let loaded = load(&state, &id, &version).await?;
        let path = version_path(loaded.recipe.id, loaded.version.id);
        let reply = |status| Reply {
            state: &state,
            session: &session,
            staff: &staff,
            status,
        };
        let refuse = |method: MethodState| {
            let mut view = EditView::read(&state, &fields, &loaded);
            view.method = method;
            view
        };
        let change = match planned(&state, &fields) {
            Ok(change) => change,
            Err(mut method) => {
                if fields.checked("kind") && is_htmx(&headers) {
                    let text = t(&state, "method-error-kind");
                    return refused_fragment(&state, &session, text).await;
                }
                if method.step.is_some() {
                    method.seen = Some(fields.text("seen").to_owned());
                }
                let status = refused_status(&headers, None);
                return render_edit(&reply(status), &loaded, refuse(*method)).await;
            }
        };
        let seen = fields.text("seen");
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let (result, typed) = match change {
            Change::Kind(kind) => (
                store::set_kind(&mut conn, loaded.version.id, seen, kind).await,
                MethodState::default(),
            ),
            Change::Text(text) => {
                let result = store::save_text(&mut conn, loaded.version.id, &text).await;
                (
                    result,
                    MethodState {
                        text: Some((text, String::new())),
                        ..MethodState::default()
                    },
                )
            }
            Change::Step(position, edit) => {
                let fields = match &edit {
                    StepEdit::Add(step) | StepEdit::Set { step, .. } => {
                        Some((position, StepFields::named(position, step)))
                    }
                    _ => None,
                };
                (
                    store::edit_step(&mut conn, loaded.version.id, seen, edit).await,
                    MethodState {
                        step: fields,
                        ..MethodState::default()
                    },
                )
            }
        };
        drop(conn);
        let error = match result {
            Ok(_) => {
                let target = back_to(&headers, &path, &fields);
                if !is_htmx(&headers) || fields.checked("kind") {
                    return Ok(saved(&headers, &target));
                }
                let reloaded = load(&state, &id, &version).await?;
                let mut view = EditView::read(&state, &fields, &reloaded);
                view.method.saved = true;
                return render_edit(&reply(StatusCode::OK), &reloaded, view).await;
            }
            Err(recipes::Error::NotFound) => return Err(not_found(&state).into()),
            Err(error) => error,
        };
        let text = error_text(&state, &error).ok_or_else(|| fail(&state, &error))?;
        if fields.checked("kind") && is_htmx(&headers) {
            return refused_fragment(&state, &session, text).await;
        }
        let mut typed = typed;
        let mut text = text;
        if matches!(error, recipes::Error::Stale) {
            if typed.step.as_ref().is_some_and(|(at, _)| at.is_some()) {
                typed.step = None;
            }
            if let Some((lost, _)) = typed.text.take() {
                text = t_with(&state, "method-error-stale-text", &[("text", lost)]);
            }
        }
        let current = load(&state, &id, &version).await?;
        let mut view = EditView::read(&state, &fields, &current);
        view.method = MethodState {
            notice: Some(Message {
                kind: "error",
                text: text.clone(),
            }),
            ..typed
        };
        if let (recipes::Error::Invalid("method_text"), Some((_, error))) =
            (&error, view.method.text.as_mut())
        {
            text.clone_into(error);
            view.method.notice = None;
        }
        render_edit(
            &reply(refused_status(&headers, Some(&error))),
            &current,
            view,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

fn kind_change(_: &AppState, fields: &Fields) -> Result<Change, Box<MethodState>> {
    fields
        .text("kind")
        .parse()
        .map(Change::Kind)
        .map_err(|_| Box::default())
}

pub async fn set_kind(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, version)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let posted = Posted {
        id,
        version,
        fields,
    };
    change(state, staff, session, headers, posted, kind_change).await
}

pub async fn save_text(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, version)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let posted = Posted {
        id,
        version,
        fields,
    };
    change(state, staff, session, headers, posted, |_, fields| {
        Ok(Change::Text(clean_text(fields.raw("tekst"))))
    })
    .await
}

fn step_change(
    state: &AppState,
    fields: &Fields,
    position: Option<usize>,
    edit: impl FnOnce(RecipeStep) -> StepEdit,
) -> Result<Change, Box<MethodState>> {
    match StepFields::posted(state, fields, position) {
        (_, Some(step)) => Ok(Change::Step(position, edit(step))),
        (typed, None) => Err(Box::new(MethodState {
            step: Some((position, typed)),
            ..MethodState::default()
        })),
    }
}

pub async fn add_step(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, version)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let posted = Posted {
        id,
        version,
        fields,
    };
    change(state, staff, session, headers, posted, |state, fields| {
        step_change(state, fields, None, StepEdit::Add)
    })
    .await
}

/// What a post to `steps/{position}/{action}` does.
#[derive(Clone, Copy)]
enum StepAction {
    Set,
    Remove,
    Up,
    Down,
}

async fn at_step(
    state: AppState,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    (id, version, position): (String, String, String),
    fields: Fields,
    action: StepAction,
) -> Response {
    let Ok(position) = position.parse::<usize>() else {
        return not_found(&state);
    };
    let posted = Posted {
        id,
        version,
        fields,
    };
    change(
        state,
        staff,
        session,
        headers,
        posted,
        move |state, fields| match action {
            StepAction::Set => step_change(state, fields, Some(position), |step| StepEdit::Set {
                position,
                step,
            }),
            StepAction::Remove => Ok(Change::Step(None, StepEdit::Remove { position })),
            StepAction::Up => Ok(Change::Step(None, StepEdit::MoveUp { position })),
            StepAction::Down => Ok(Change::Step(None, StepEdit::MoveDown { position })),
        },
    )
    .await
}

pub async fn set_step(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(path): Path<(String, String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    at_step(
        state,
        staff,
        session,
        headers,
        path,
        fields,
        StepAction::Set,
    )
    .await
}

pub async fn remove_step(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(path): Path<(String, String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    at_step(
        state,
        staff,
        session,
        headers,
        path,
        fields,
        StepAction::Remove,
    )
    .await
}

pub async fn step_up(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(path): Path<(String, String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    at_step(state, staff, session, headers, path, fields, StepAction::Up).await
}

pub async fn step_down(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(path): Path<(String, String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    at_step(
        state,
        staff,
        session,
        headers,
        path,
        fields,
        StepAction::Down,
    )
    .await
}

/// The read-only instructions of a version, for its own page and a batch page.
///
/// # Errors
/// A rejection when the database fails.
pub async fn load_view(
    state: &AppState,
    version: domain::RecipeVersionId,
    fields: &Fields,
    shown: &Shown<'_>,
) -> Step<MethodView> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let method = store::method(&mut conn, version)
        .await
        .map_err(|e| fail(state, &e))?;
    Ok(view(state, &method, fields, shown))
}

async fn refused_fragment(state: &AppState, session: &Session, text: String) -> Outcome {
    Ok(pages::render(
        state,
        StatusCode::OK,
        &Refused {
            layout: layout(state, session).await?,
            text,
        },
    ))
}

/// The edit page as the browser shows it now (its scale may have changed since the form
/// was drawn), else at the posted weight.
fn back_to(headers: &HeaderMap, path: &str, fields: &Fields) -> String {
    let edit = format!("{path}/edit");
    let shown = ["hx-current-url", "referer"]
        .into_iter()
        .filter_map(|name| headers.get(name)?.to_str().ok())
        .find_map(|url| {
            let at = url.find(&edit)?;
            let shown = url[at..].split('#').next().unwrap_or_default();
            let (page, query) = shown.split_once('?').unwrap_or((shown, ""));
            (page == edit).then(|| {
                let kept: Vec<&str> = query
                    .split('&')
                    .filter(|p| !p.is_empty() && !p.starts_with("opgeslagen="))
                    .collect();
                format!("{edit}?{}", kept.join("&"))
            })
        });
    shown.unwrap_or_else(|| {
        let grams = fields.text("gewicht").parse::<u64>().map(|g| g.to_string());
        format!("{edit}?gewicht={}", grams.unwrap_or_default())
    })
}

/// For a start time asked through htmx: the query of the page as the browser shows it
/// (its scale may have changed since the start field was drawn) with the new start.
pub fn start_on_page(headers: &HeaderMap, uri: &axum::http::Uri) -> Option<String> {
    let own = uri.query()?;
    let current = headers.get("hx-current-url")?.to_str().ok()?;
    let current = current.split('#').next().unwrap_or_default();
    let (page, query) = current.split_once('?').unwrap_or((current, ""));
    if !page.ends_with(uri.path()) {
        return None;
    }
    let kept = query
        .split('&')
        .filter(|p| !p.is_empty() && !p.starts_with("start=") && !p.starts_with("opgeslagen="));
    let start = own.split('&').filter(|p| p.starts_with("start="));
    Some(kept.chain(start).collect::<Vec<_>>().join("&"))
}
