//! Pickup locations and their default pickup windows (F4a).

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bakedays::pickup::{
    self, PickupLocation, PickupLocationInput, PickupWindow, PickupWindowInput,
};
use domain::{PickupLocationId, PickupWindowId};
use jiff::civil::Weekday;
use tower_sessions::Session;

use super::form::{Errors, Fields, parse_time, time_value};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require,
    saved, t, t_count, t_with,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

const CHECK_WINDOW: &str = "/staff/locations/windows/check";
const MAX_DAYS_BEFORE: i16 = 28;

pub struct LocationForm {
    pub name: String,
    pub address: String,
    pub notes: String,
}

impl LocationForm {
    fn posted(fields: &Fields) -> Self {
        Self {
            name: fields.text("name").to_owned(),
            address: fields.raw("address").trim().to_owned(),
            notes: fields.raw("notes").trim().to_owned(),
        }
    }

    fn validate(&self, state: &AppState) -> Result<PickupLocationInput, Errors> {
        let mut errors = Errors::default();
        if self.name.is_empty() {
            errors.add("name", t(state, "location-error-name"));
        }
        if self.address.is_empty() {
            errors.add("address", t(state, "location-error-address"));
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(PickupLocationInput {
            name: self.name.clone(),
            address: self.address.clone(),
            notes: self.notes.clone(),
        })
    }
}

pub struct WindowForm {
    pub weekday: String,
    pub starts: String,
    pub ends: String,
    pub days_before: String,
    pub deadline_time: String,
    pub active: bool,
}

impl WindowForm {
    fn empty() -> Self {
        Self {
            weekday: "6".to_owned(),
            starts: "11:00".to_owned(),
            ends: "12:00".to_owned(),
            days_before: "2".to_owned(),
            deadline_time: "20:00".to_owned(),
            active: true,
        }
    }

    fn of(window: &PickupWindow) -> Self {
        Self {
            weekday: window.weekday.to_monday_one_offset().to_string(),
            starts: time_value(window.starts),
            ends: time_value(window.ends),
            days_before: window.default_deadline_days_before.to_string(),
            deadline_time: time_value(window.default_deadline_time),
            active: window.active,
        }
    }

    fn posted(fields: &Fields) -> Self {
        Self {
            weekday: fields.text("weekday").to_owned(),
            starts: fields.text("starts").to_owned(),
            ends: fields.text("ends").to_owned(),
            days_before: fields.text("days_before").to_owned(),
            deadline_time: fields.text("deadline_time").to_owned(),
            active: fields.checked("active"),
        }
    }

    fn weekday_options(&self, state: &AppState) -> Vec<SelectOption> {
        (1..=7)
            .map(|n: i8| SelectOption {
                value: n.to_string(),
                label: weekday_name(state, n),
                selected: self.weekday == n.to_string(),
            })
            .collect()
    }

    fn validate(&self, state: &AppState) -> Result<PickupWindowInput, Errors> {
        let mut errors = Errors::default();
        let weekday = self
            .weekday
            .parse::<i8>()
            .ok()
            .and_then(|n| Weekday::from_monday_one_offset(n).ok());
        if weekday.is_none() {
            errors.add("weekday", t(state, "window-error-weekday"));
        }
        let mut time = |field: &str, value: &str| {
            let parsed = parse_time(value);
            if parsed.is_none() {
                errors.add(field, t(state, "form-error-time"));
            }
            parsed
        };
        let starts = time("starts", &self.starts);
        let ends = time("ends", &self.ends);
        let deadline_time = time("deadline_time", &self.deadline_time);
        if let (Some(starts), Some(ends)) = (starts, ends)
            && starts >= ends
        {
            errors.add("ends", t(state, "window-error-order"));
        }
        let days_before = self
            .days_before
            .parse::<i16>()
            .ok()
            .filter(|days| (0..=MAX_DAYS_BEFORE).contains(days));
        if days_before.is_none() {
            errors.add("days_before", t(state, "window-error-days"));
        }
        match (weekday, starts, ends, days_before, deadline_time) {
            (Some(weekday), Some(starts), Some(ends), Some(days), Some(at))
                if errors.is_empty() =>
            {
                Ok(PickupWindowInput {
                    weekday,
                    starts,
                    ends,
                    default_deadline_days_before: days,
                    default_deadline_time: at,
                    active: self.active,
                })
            }
            _ => Err(errors),
        }
    }
}

fn weekday_name(state: &AppState, n: i8) -> String {
    t_count(state, "weekday", u32::from(n.unsigned_abs()))
}

#[derive(Template)]
#[template(path = "staff/locations.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    table: Table,
}

#[derive(Template)]
#[template(path = "staff/location.html")]
struct LocationPage<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    form: LocationForm,
    errors: Errors,
    saved: bool,
    /// Only for an existing location: its windows and where to add one.
    windows: Option<(Table, String)>,
}

#[derive(Template)]
#[template(path = "staff/window.html")]
struct WindowPage<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    back: String,
    check: &'static str,
    form: WindowForm,
    weekdays: Vec<SelectOption>,
    errors: Errors,
    saved: bool,
}

fn location_path(id: PickupLocationId) -> String {
    format!("/staff/locations/{}", id.get())
}

fn bakedays_error(state: &AppState, error: &bakedays::Error) -> Response {
    match error {
        bakedays::Error::NotFound => not_found(state),
        other => fail(state, other),
    }
}

async fn load(state: &AppState, raw_id: &str) -> Step<PickupLocation> {
    let id = PickupLocationId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    pickup::get_location(&mut conn, id)
        .await
        .map_err(|e| bakedays_error(state, &e).into())
}

async fn load_window(
    state: &AppState,
    location: &PickupLocation,
    raw_id: &str,
) -> Step<PickupWindow> {
    let id = PickupWindowId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    pickup::list_windows(&mut conn, location.id)
        .await
        .map_err(|e| fail(state, &e))?
        .into_iter()
        .find(|w| w.id == id)
        .ok_or_else(|| not_found(state).into())
}

pub async fn list(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = pickup::list_locations(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        let table = Table {
            id: "locaties".to_owned(),
            caption: t(&state, "locations-caption"),
            head: vec![
                Cell::text(t(&state, "location-name")),
                Cell::text(t(&state, "location-address")),
            ],
            rows: all
                .iter()
                .map(|l| {
                    vec![
                        Cell::link(l.name.clone(), location_path(l.id)),
                        Cell::text(l.address.clone()),
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

fn window_table(state: &AppState, location: &PickupLocation, windows: &[PickupWindow]) -> Table {
    Table {
        id: "ophaalvensters".to_owned(),
        caption: t(state, "windows-caption"),
        head: vec![
            Cell::text(t(state, "window-weekday")),
            Cell::text(t(state, "window-time")),
            Cell::text(t(state, "window-deadline")),
            Cell::text(t(state, "product-active")),
        ],
        rows: windows
            .iter()
            .map(|w| {
                let day = weekday_name(state, w.weekday.to_monday_one_offset());
                let range = i18n::time_range(w.starts, w.ends).unwrap_or_default();
                vec![
                    Cell::link(
                        day,
                        format!("{}/windows/{}", location_path(location.id), w.id.get()),
                    ),
                    Cell::text(range),
                    Cell::text(t_with(
                        state,
                        "window-deadline-rule",
                        &[
                            ("days", w.default_deadline_days_before.to_string()),
                            ("time", time_value(w.default_deadline_time)),
                        ],
                    )),
                    Cell::text(t(state, if w.active { "form-yes" } else { "form-no" })),
                ]
            })
            .collect(),
    }
}

async fn render_location(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    location: Option<&PickupLocation>,
    form: LocationForm,
    errors: Errors,
    saved: bool,
) -> Outcome {
    let windows = match location {
        Some(location) => {
            let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
            let windows = pickup::list_windows(&mut conn, location.id)
                .await
                .map_err(|e| fail(state, &e))?;
            Some((
                window_table(state, location, &windows),
                format!("{}/windows/new", location_path(location.id)),
            ))
        }
        None => None,
    };
    let (heading, action) = match location {
        Some(l) => (l.name.clone(), location_path(l.id)),
        None => (t(state, "location-new"), "/staff/locations/new".to_owned()),
    };
    Ok(pages::render(
        state,
        status,
        &LocationPage {
            layout: layout(state, session).await?,
            heading,
            action,
            form,
            errors,
            saved,
            windows,
        },
    ))
}

pub async fn new_form(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let form = LocationForm {
            name: String::new(),
            address: String::new(),
            notes: String::new(),
        };
        render_location(
            &state,
            &session,
            StatusCode::OK,
            None,
            form,
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
        require(&state, &staff, Permission::ManageBakeDays)?;
        let form = LocationForm::posted(&fields);
        let input = match form.validate(&state) {
            Ok(input) => input,
            Err(errors) => {
                let status = invalid_status(&headers);
                return render_location(&state, &session, status, None, form, errors, false).await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let location = pickup::create_location(&mut conn, &input)
            .await
            .map_err(|e| fail(&state, &e))?;
        Ok(saved(&headers, &location_path(location.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn edit_form(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let location = load(&state, &id).await?;
        let form = LocationForm {
            name: location.name.clone(),
            address: location.address.clone(),
            notes: location.notes.clone(),
        };
        render_location(
            &state,
            &session,
            StatusCode::OK,
            Some(&location),
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
        require(&state, &staff, Permission::ManageBakeDays)?;
        let location = load(&state, &id).await?;
        let form = LocationForm::posted(&fields);
        let input = match form.validate(&state) {
            Ok(input) => input,
            Err(errors) => {
                let status = invalid_status(&headers);
                return render_location(
                    &state,
                    &session,
                    status,
                    Some(&location),
                    form,
                    errors,
                    false,
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        pickup::update_location(&mut conn, location.id, &input)
            .await
            .map_err(|e| bakedays_error(&state, &e))?;
        Ok(saved(&headers, &location_path(location.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

async fn render_window(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    (location, window): (&PickupLocation, Option<&PickupWindow>),
    form: WindowForm,
    errors: Errors,
    saved: bool,
) -> Outcome {
    let action = match window {
        Some(w) => format!("{}/windows/{}", location_path(location.id), w.id.get()),
        None => format!("{}/windows/new", location_path(location.id)),
    };
    Ok(pages::render(
        state,
        status,
        &WindowPage {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "window-heading",
                &[("location", location.name.clone())],
            ),
            action,
            back: location_path(location.id),
            check: CHECK_WINDOW,
            weekdays: form.weekday_options(state),
            form,
            errors,
            saved,
        },
    ))
}

pub async fn new_window_form(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let location = load(&state, &id).await?;
        render_window(
            &state,
            &session,
            StatusCode::OK,
            (&location, None),
            WindowForm::empty(),
            Errors::default(),
            false,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Inline check for the window form; the location only matters for the heading, so the
/// first one serves.
pub async fn check_window(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let form = WindowForm::posted(&fields);
        let errors = form.validate(&state).err().unwrap_or_default();
        let location = PickupLocation {
            id: PickupLocationId::new(0),
            name: String::new(),
            address: String::new(),
            notes: String::new(),
        };
        render_window(
            &state,
            &session,
            StatusCode::OK,
            (&location, None),
            form,
            errors,
            false,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn create_window(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let location = load(&state, &id).await?;
        let form = WindowForm::posted(&fields);
        let input = match form.validate(&state) {
            Ok(input) => input,
            Err(errors) => {
                let status = invalid_status(&headers);
                return render_window(
                    &state,
                    &session,
                    status,
                    (&location, None),
                    form,
                    errors,
                    false,
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        pickup::create_window(&mut conn, location.id, &input)
            .await
            .map_err(|e| fail(&state, &e))?;
        Ok(saved(&headers, &location_path(location.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn edit_window_form(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((id, window)): Path<(String, String)>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let location = load(&state, &id).await?;
        let window = load_window(&state, &location, &window).await?;
        render_window(
            &state,
            &session,
            StatusCode::OK,
            (&location, Some(&window)),
            WindowForm::of(&window),
            Errors::default(),
            flash.saved(),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn update_window(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, window)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageBakeDays)?;
        let location = load(&state, &id).await?;
        let window = load_window(&state, &location, &window).await?;
        let form = WindowForm::posted(&fields);
        let input = match form.validate(&state) {
            Ok(input) => input,
            Err(errors) => {
                let status = invalid_status(&headers);
                return render_window(
                    &state,
                    &session,
                    status,
                    (&location, Some(&window)),
                    form,
                    errors,
                    false,
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        pickup::update_window(&mut conn, window.id, &input)
            .await
            .map_err(|e| bakedays_error(&state, &e))?;
        Ok(saved(&headers, &location_path(location.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
