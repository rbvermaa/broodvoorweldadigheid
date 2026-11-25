//! Charities: add, edit and archive (F4g). Archived charities stay in the history.

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use catalog::{Charity, CharityInput, charities};
use domain::CharityId;
use tower_sessions::Session;

use super::form::{Errors, Fields};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require,
    saved, t, t_with,
};
use crate::components::{Cell, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/staff/charities", get(list))
        .route("/staff/charities/new", get(new_form).post(create))
        .route("/staff/charities/check", post(check))
        .route("/staff/charities/{id}", get(edit_form).post(update))
        .route("/staff/charities/{id}/archive", post(archive))
}

const CHECK: &str = "/staff/charities/check";

pub struct CharityForm {
    pub name: String,
    pub description: String,
    pub url: String,
    pub iban: String,
    pub anbi_rsin: String,
}

impl CharityForm {
    fn empty() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            url: String::new(),
            iban: String::new(),
            anbi_rsin: String::new(),
        }
    }

    fn of(charity: &Charity) -> Self {
        Self {
            name: charity.name.clone(),
            description: charity.description.clone(),
            url: charity.url.clone(),
            iban: charity.iban.clone().unwrap_or_default(),
            anbi_rsin: charity.anbi_rsin.clone().unwrap_or_default(),
        }
    }

    fn posted(fields: &Fields) -> Self {
        Self {
            name: fields.text("name").to_owned(),
            description: fields.raw("description").trim().to_owned(),
            url: fields.text("url").to_owned(),
            iban: fields.text("iban").to_owned(),
            anbi_rsin: fields.text("anbi_rsin").to_owned(),
        }
    }

    fn validate(
        &self,
        state: &AppState,
        existing: Option<&Charity>,
    ) -> Result<CharityInput, Errors> {
        let mut errors = Errors::default();
        if self.name.is_empty() {
            errors.add("name", t(state, "charity-error-name"));
        }
        if !(self.url.starts_with("https://") || self.url.starts_with("http://"))
            || self.url.contains(char::is_whitespace)
        {
            errors.add("url", t(state, "charity-error-url"));
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let optional = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        Ok(CharityInput {
            name: self.name.clone(),
            description: self.description.clone(),
            url: self.url.clone(),
            logo_path: existing.and_then(|c| c.logo_path.clone()),
            iban: optional(&self.iban.replace(' ', "").to_uppercase()),
            anbi_rsin: optional(&self.anbi_rsin),
        })
    }
}

#[derive(Template)]
#[template(path = "staff/charities.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    table: Table,
    saved: bool,
}

#[derive(Template)]
#[template(path = "staff/charity.html")]
struct CharityPage<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    check: &'static str,
    form: CharityForm,
    errors: Errors,
    saved: bool,
    /// For an existing, active charity: the archive action and what it will do.
    archive: Option<(String, String)>,
    archived: bool,
}

fn charity_path(id: CharityId) -> String {
    format!("/staff/charities/{}", id.get())
}

fn catalog_error(state: &AppState, error: &catalog::Error) -> Response {
    match error {
        catalog::Error::NotFound => not_found(state),
        other => fail(state, other),
    }
}

async fn load(state: &AppState, raw_id: &str) -> Step<Charity> {
    let id = CharityId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    charities::get(&mut conn, id)
        .await
        .map_err(|e| catalog_error(state, &e).into())
}

pub async fn list(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageCharities)?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = charities::list_all(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        let table = Table {
            id: "goede-doelen".to_owned(),
            caption: t(&state, "charities-caption"),
            head: vec![
                Cell::text(t(&state, "charity-name")),
                Cell::text(t(&state, "charity-url")),
                Cell::text(t(&state, "charity-status")),
            ],
            rows: all
                .iter()
                .map(|c| {
                    vec![
                        Cell::link(c.name.clone(), charity_path(c.id)),
                        Cell::text(c.url.clone()),
                        Cell::text(t(
                            &state,
                            if c.is_active() {
                                "charity-active"
                            } else {
                                "charity-archived"
                            },
                        )),
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
                saved: flash.saved(),
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

async fn render_form(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    charity: Option<&Charity>,
    form: CharityForm,
    errors: Errors,
    saved: bool,
) -> Outcome {
    let (heading, action) = match charity {
        Some(c) => (c.name.clone(), charity_path(c.id)),
        None => (t(state, "charity-new"), "/staff/charities/new".to_owned()),
    };
    let archive = charity.filter(|c| c.is_active()).map(|c| {
        (
            format!("{}/archive", charity_path(c.id)),
            t_with(state, "charity-archive-text", &[("name", c.name.clone())]),
        )
    });
    Ok(pages::render(
        state,
        status,
        &CharityPage {
            layout: layout(state, session).await?,
            heading,
            action,
            check: CHECK,
            form,
            errors,
            saved,
            archive,
            archived: charity.is_some_and(|c| !c.is_active()),
        },
    ))
}

pub async fn new_form(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageCharities)?;
        render_form(
            &state,
            &session,
            StatusCode::OK,
            None,
            CharityForm::empty(),
            Errors::default(),
            false,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn check(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageCharities)?;
        let form = CharityForm::posted(&fields);
        let errors = form.validate(&state, None).err().unwrap_or_default();
        render_form(&state, &session, StatusCode::OK, None, form, errors, false).await
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
        require(&state, &staff, Permission::ManageCharities)?;
        let form = CharityForm::posted(&fields);
        let input = match form.validate(&state, None) {
            Ok(input) => input,
            Err(errors) => {
                return render_form(
                    &state,
                    &session,
                    invalid_status(&headers),
                    None,
                    form,
                    errors,
                    false,
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let charity = charities::create(&mut conn, &input)
            .await
            .map_err(|e| fail(&state, &e))?;
        Ok(saved(&headers, &charity_path(charity.id)))
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
        require(&state, &staff, Permission::ManageCharities)?;
        let charity = load(&state, &id).await?;
        let form = CharityForm::of(&charity);
        render_form(
            &state,
            &session,
            StatusCode::OK,
            Some(&charity),
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
        require(&state, &staff, Permission::ManageCharities)?;
        let charity = load(&state, &id).await?;
        let form = CharityForm::posted(&fields);
        let input = match form.validate(&state, Some(&charity)) {
            Ok(input) => input,
            Err(errors) => {
                return render_form(
                    &state,
                    &session,
                    invalid_status(&headers),
                    Some(&charity),
                    form,
                    errors,
                    false,
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        charities::update(&mut conn, charity.id, &input)
            .await
            .map_err(|e| catalog_error(&state, &e))?;
        Ok(saved(&headers, &charity_path(charity.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn archive(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::ManageCharities)?;
        let id = CharityId::new(path_id(&state, &id)?);
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        charities::archive(&mut conn, id, state.clock.as_ref())
            .await
            .map_err(|e| catalog_error(&state, &e))?;
        Ok(saved(&headers, "/staff/charities"))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
