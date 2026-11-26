//! Recipes, their versions and the scaled formula for a dough weight (F14).

mod editor;
pub mod method;

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use catalog::products as catalog_products;
use domain::{ProductId, RecipeId, RecipeVersionId, in_bakery_time};
use jiff::Timestamp;
use recipes::recipes as store;
use recipes::{LabelSetting, Pct, Recipe, RecipeInput, RecipeKind, Version};
use tower_sessions::Session;

use super::form::{Errors, Fields, pct_value};
use super::{
    Flash, Outcome, Rejection, RowChange, SavedRow, Search, SearchBox, Step, fail, invalid_status,
    layout, not_found, path_id, require, saved, saved_to_list, t, t_with,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(LIST, get(list))
        .route("/staff/recipes/new", get(new_form).post(create))
        .route("/staff/recipes/{id}", get(show).post(update))
        .route("/staff/recipes/{id}/versions", post(new_version))
        .route("/staff/recipes/{id}/etiket", post(set_label))
        .route("/staff/recipes/{id}/products", post(link_product))
        .route(
            "/staff/recipes/{id}/products/{product}/remove",
            post(unlink_product),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}",
            get(editor::version_page),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/edit",
            get(editor::edit_page).post(editor::save_details),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/lines",
            post(editor::add_line),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/lines/{position}",
            post(editor::set_line),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/lines/{position}/remove",
            post(editor::remove_line),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/search",
            get(editor::search),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/method",
            post(method::set_kind),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/text",
            post(method::save_text),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/steps",
            post(method::add_step),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/steps/{position}",
            post(method::set_step),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/steps/{position}/remove",
            post(method::remove_step),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/steps/{position}/up",
            post(method::step_up),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/steps/{position}/down",
            post(method::step_down),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/freeze",
            post(freeze),
        )
        .route(
            "/staff/recipes/{id}/versions/{version}/current",
            post(make_current),
        )
}

const LIST: &str = "/staff/recipes";
const NEW: &str = "/staff/recipes/new";

const fn kind_label(kind: RecipeKind) -> &'static str {
    match kind {
        RecipeKind::Dough => "recipe-kind-dough",
        RecipeKind::Preferment => "recipe-kind-preferment",
        RecipeKind::Soaker => "recipe-kind-soaker",
        RecipeKind::Filling => "recipe-kind-filling",
        RecipeKind::Topping => "recipe-kind-topping",
    }
}

fn recipe_path(id: RecipeId) -> String {
    format!("{LIST}/{}", id.get())
}

pub(super) fn version_path(recipe: RecipeId, version: RecipeVersionId) -> String {
    format!("{LIST}/{}/versions/{}", recipe.get(), version.get())
}

fn date_text(at: Timestamp) -> String {
    i18n::numeric_date(in_bakery_time(at).date())
}

fn pct_text(pct: Pct) -> String {
    format!("{}%", pct_value(pct))
}

fn recipe_error(state: &AppState, error: &recipes::Error) -> Rejection {
    match error {
        recipes::Error::NotFound => not_found(state).into(),
        other => fail(state, other).into(),
    }
}

async fn load_recipe(state: &AppState, raw_id: &str) -> Step<Recipe> {
    let id = RecipeId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    store::get(&mut conn, id)
        .await
        .map_err(|e| recipe_error(state, &e))
}

async fn load_version(
    state: &AppState,
    raw_recipe: &str,
    raw_version: &str,
) -> Step<(Recipe, Version)> {
    let recipe = load_recipe(state, raw_recipe).await?;
    let id = RecipeVersionId::new(path_id(state, raw_version)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let version = store::version(&mut conn, recipe.id, id)
        .await
        .map_err(|e| recipe_error(state, &e))?;
    Ok((recipe, version))
}

#[derive(Template)]
#[template(path = "staff/recipes.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    search: SearchBox,
    table: Table,
    saved: SavedRow,
}

pub async fn list(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(flash): Query<Flash>,
    Query(search): Query<Search>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = store::list(&mut conn).await.map_err(|e| fail(&state, &e))?;
        let table = Table {
            id: "recepten".to_owned(),
            caption: t(&state, "recipes-caption"),
            head: vec![
                Cell::text(t(&state, "recipe-name")),
                Cell::text(t(&state, "recipe-kind")),
                Cell::text(t(&state, "recipe-current")),
                Cell::text(t(&state, "recipe-products")),
            ],
            rows: all
                .iter()
                .filter(|summary| {
                    search.matches(&[&summary.recipe.name, &summary.products.join(" ")])
                })
                .map(|summary| {
                    vec![
                        Cell::link(summary.recipe.name.clone(), recipe_path(summary.recipe.id)),
                        Cell::text(t(&state, kind_label(summary.recipe.kind))),
                        Cell::text(summary.current_version.map_or_else(
                            || t(&state, "recipe-none"),
                            |n| t_with(&state, "recipe-version-number", &[("n", n.to_string())]),
                        )),
                        Cell::text(summary.products.join(", ")),
                    ]
                })
                .collect(),
        };
        let saved = flash.row(&state, &table, |id| recipe_path(RecipeId::new(id)));
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &ListPage {
                layout: layout(&state, &session).await?,
                search: search.view(&state, LIST, Vec::new()),
                table,
                saved,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub struct RecipeForm {
    pub name: String,
    pub kind: String,
    /// A refused label setting, shown again instead of the stored one.
    pub label: Option<LabelSetting>,
}

impl RecipeForm {
    fn posted(fields: &Fields) -> Self {
        Self {
            name: fields.text("name").to_owned(),
            kind: fields.text("kind").to_owned(),
            label: None,
        }
    }

    fn validate(&self, state: &AppState) -> Result<RecipeInput, Errors> {
        let mut errors = Errors::default();
        if self.name.is_empty() {
            errors.add("name", t(state, "recipe-error-name"));
        }
        match self.kind.parse::<RecipeKind>() {
            Ok(kind) if errors.is_empty() => Ok(RecipeInput {
                name: self.name.clone(),
                kind,
            }),
            Ok(_) => Err(errors),
            Err(_) => {
                errors.add("kind", t(state, "recipe-error-kind"));
                Err(errors)
            }
        }
    }

    fn kinds(&self, state: &AppState) -> Vec<SelectOption> {
        RecipeKind::ALL
            .into_iter()
            .map(|kind| SelectOption {
                value: kind.code().to_owned(),
                label: t(state, kind_label(kind)),
                selected: kind.code() == self.kind,
            })
            .collect()
    }
}

pub struct ProductRow {
    pub name: String,
    pub remove: String,
}

/// What an existing recipe's page shows besides the form.
pub struct RecipeDetails {
    pub versions: Table,
    pub new_version: String,
    pub products: Vec<ProductRow>,
    pub link_action: String,
    pub product_options: Vec<SelectOption>,
    pub label: LabelForm,
}

/// How the recipe shows on the ingredient list of recipes that use it (F17).
pub struct LabelForm {
    pub action: String,
    pub modes: Vec<SelectOption>,
    pub name: String,
    /// What the ingredient list shows now.
    pub now: String,
}

const LABEL_MODES: [(&str, Option<bool>, &str); 3] = [
    ("soort", None, "recipe-label-mode-kind"),
    ("samengesteld", Some(true), "recipe-label-mode-compound"),
    ("los", Some(false), "recipe-label-mode-merged"),
];

/// `shown` is what the form holds: the stored setting, or the one posted when it was refused.
fn label_form(state: &AppState, recipe: &Recipe, shown: &LabelSetting, path: &str) -> LabelForm {
    LabelForm {
        action: format!("{path}/etiket"),
        modes: LABEL_MODES
            .iter()
            .map(|&(value, compound, label)| SelectOption {
                value: value.to_owned(),
                label: t(state, label),
                selected: compound == shown.compound,
            })
            .collect(),
        name: shown.name.clone(),
        now: match recipe.compound_name() {
            Some(name) => t_with(
                state,
                "recipe-label-now-compound",
                &[("name", name.to_owned())],
            ),
            None => t(state, "recipe-label-now-merged"),
        },
    }
}

#[derive(Template)]
#[template(path = "staff/recipe.html")]
struct RecipePage<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    form: RecipeForm,
    kinds: Vec<SelectOption>,
    errors: Errors,
    saved: bool,
    details: Option<RecipeDetails>,
}

async fn details(
    state: &AppState,
    recipe: &Recipe,
    posted_label: Option<&LabelSetting>,
) -> Step<RecipeDetails> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let versions = store::versions(&mut conn, recipe.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let linked = store::products(&mut conn, recipe.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let all_products = catalog_products::list_all(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    let path = recipe_path(recipe.id);
    Ok(RecipeDetails {
        versions: Table {
            id: "versies".to_owned(),
            caption: t(state, "recipe-versions"),
            head: vec![
                Cell::text(t(state, "recipe-version")),
                Cell::text(t(state, "recipe-status")),
                Cell::text(t(state, "recipe-in-use")),
            ],
            rows: versions
                .iter()
                .map(|v| {
                    vec![
                        Cell::link(
                            t_with(
                                state,
                                "recipe-version-number",
                                &[("n", v.version.to_string())],
                            ),
                            version_path(recipe.id, v.id),
                        ),
                        Cell::text(status_text(state, v)),
                        Cell::text(if recipe.current_version_id == Some(v.id) {
                            t(state, "form-yes")
                        } else {
                            String::new()
                        }),
                    ]
                })
                .collect(),
        },
        new_version: format!("{path}/versions"),
        products: linked
            .iter()
            .map(|p| ProductRow {
                name: p.name.clone(),
                remove: format!("{path}/products/{}/remove", p.id.get()),
            })
            .collect(),
        link_action: format!("{path}/products"),
        product_options: all_products
            .iter()
            .filter(|p| !linked.iter().any(|l| l.id == p.id))
            .map(|p| SelectOption {
                value: p.id.get().to_string(),
                label: p.name_nl.clone(),
                selected: false,
            })
            .collect(),
        label: label_form(state, recipe, posted_label.unwrap_or(&recipe.label), &path),
    })
}

fn status_text(state: &AppState, version: &Version) -> String {
    match version.frozen_at {
        Some(at) => t_with(state, "recipe-frozen-on", &[("date", date_text(at))]),
        None => t(state, "recipe-draft"),
    }
}

async fn render_recipe(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    recipe: Option<&Recipe>,
    form: RecipeForm,
    errors: Errors,
    saved: bool,
) -> Outcome {
    let (heading, action, details) = match recipe {
        Some(r) => (
            r.name.clone(),
            recipe_path(r.id),
            Some(details(state, r, form.label.as_ref()).await?),
        ),
        None => (t(state, "recipe-new"), NEW.to_owned(), None),
    };
    Ok(pages::render(
        state,
        status,
        &RecipePage {
            layout: layout(state, session).await?,
            heading,
            action,
            kinds: form.kinds(state),
            form,
            errors,
            saved,
            details,
        },
    ))
}

pub async fn new_form(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let form = RecipeForm {
            name: String::new(),
            kind: RecipeKind::Dough.code().to_owned(),
            label: None,
        };
        render_recipe(
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

pub async fn show(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let recipe = load_recipe(&state, &id).await?;
        let form = RecipeForm {
            name: recipe.name.clone(),
            kind: recipe.kind.code().to_owned(),
            label: None,
        };
        render_recipe(
            &state,
            &session,
            StatusCode::OK,
            Some(&recipe),
            form,
            Errors::default(),
            flash.saved(),
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
    save_recipe(state, staff, session, headers, None, fields).await
}

pub async fn update(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    save_recipe(state, staff, session, headers, Some(id), fields).await
}

async fn save_recipe(
    state: AppState,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    id: Option<String>,
    fields: Fields,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let existing = match &id {
            Some(id) => Some(load_recipe(&state, id).await?),
            None => None,
        };
        let form = RecipeForm::posted(&fields);
        let input = match form.validate(&state) {
            Ok(input) => input,
            Err(errors) => {
                return render_recipe(
                    &state,
                    &session,
                    invalid_status(&headers),
                    existing.as_ref(),
                    form,
                    errors,
                    false,
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = match &existing {
            Some(r) => store::update(&mut conn, r.id, &input).await,
            None => store::create(&mut conn, &input).await.map(|(r, _)| r),
        };
        match result {
            Ok(recipe) => Ok(saved_to_list(
                &headers,
                LIST,
                RowChange::of(existing.is_some()),
                recipe.id.get(),
            )),
            Err(error @ (recipes::Error::Duplicate | recipes::Error::InUse)) => {
                let mut errors = Errors::default();
                if matches!(error, recipes::Error::Duplicate) {
                    errors.add("name", t(&state, "recipe-error-duplicate"));
                } else {
                    errors.add("kind", t(&state, "recipe-error-kind-frozen"));
                }
                render_recipe(
                    &state,
                    &session,
                    invalid_status(&headers),
                    existing.as_ref(),
                    form,
                    errors,
                    false,
                )
                .await
            }
            Err(other) => Err(recipe_error(&state, &other)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn new_version(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let recipe = load_recipe(&state, &id).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let draft = store::new_version(&mut conn, recipe.id)
            .await
            .map_err(|e| recipe_error(&state, &e))?;
        Ok(saved(
            &headers,
            &format!("{}/edit", version_path(recipe.id, draft.id)),
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Sets how the recipe shows on the ingredient list of recipes that use it (F17). It
/// changes their products' allergen data, so it needs EditAllergens as well.
pub async fn set_label(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        require(&state, &staff, Permission::EditAllergens)?;
        let recipe = load_recipe(&state, &id).await?;
        let Some(&(_, compound, _)) = LABEL_MODES
            .iter()
            .find(|(value, _, _)| *value == fields.text("op_etiket"))
        else {
            return Ok(Redirect::to(&recipe_path(recipe.id)).into_response());
        };
        let setting = LabelSetting {
            compound,
            name: fields.text("etiketnaam").to_owned(),
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        match store::set_label(&mut conn, recipe.id, &setting).await {
            Ok(updated) => Ok(saved(&headers, &recipe_path(updated.id))),
            Err(recipes::Error::Invalid(_)) => {
                let mut errors = Errors::default();
                errors.add("etiketnaam", t(&state, "recipe-error-label-name"));
                let form = RecipeForm {
                    name: recipe.name.clone(),
                    kind: recipe.kind.code().to_owned(),
                    label: Some(setting),
                };
                render_recipe(
                    &state,
                    &session,
                    invalid_status(&headers),
                    Some(&recipe),
                    form,
                    errors,
                    false,
                )
                .await
            }
            Err(other) => Err(recipe_error(&state, &other)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn link_product(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        require(&state, &staff, Permission::EditAllergens)?;
        let recipe = load_recipe(&state, &id).await?;
        let path = recipe_path(recipe.id);
        let Ok(product) = fields.text("product_id").parse::<i64>() else {
            return Ok(Redirect::to(&path).into_response());
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        store::link_product(&mut conn, recipe.id, ProductId::new(product))
            .await
            .map_err(|e| recipe_error(&state, &e))?;
        Ok(saved(&headers, &path))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn unlink_product(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Path((id, product)): Path<(String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        require(&state, &staff, Permission::EditAllergens)?;
        let recipe = load_recipe(&state, &id).await?;
        let product = ProductId::new(path_id(&state, &product)?);
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        store::unlink_product(&mut conn, recipe.id, product)
            .await
            .map_err(|e| fail(&state, &e))?;
        Ok(saved(&headers, &recipe_path(recipe.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub struct Message {
    pub kind: &'static str,
    pub text: String,
}

fn grams_text(grams: u64) -> String {
    i18n::decimal(i64::try_from(grams).unwrap_or(i64::MAX), 0)
}

fn sub_label(state: &AppState, name: &str, version: i32) -> String {
    t_with(
        state,
        "recipe-sub-option",
        &[("name", name.to_owned()), ("n", version.to_string())],
    )
}

pub async fn freeze(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Path((id, version)): Path<(String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (recipe, version) = load_version(&state, &id, &version).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        store::freeze(&mut conn, recipe.id, version.id, state.clock.as_ref())
            .await
            .map_err(|e| recipe_error(&state, &e))?;
        Ok(saved(&headers, &version_path(recipe.id, version.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn make_current(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Path((id, version)): Path<(String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (recipe, version) = load_version(&state, &id, &version).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        store::set_current(&mut conn, recipe.id, version.id)
            .await
            .map_err(|e| recipe_error(&state, &e))?;
        Ok(saved(&headers, &version_path(recipe.id, version.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
