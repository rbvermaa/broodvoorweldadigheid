//! Recipes, their versions and the scaled formula for a dough weight (F14).

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use catalog::products as catalog_products;
use domain::{ProductId, RecipeId, RecipeVersionId, in_bakery_time};
use jiff::Timestamp;
use recipes::recipes::{self as store, MAX_LEAD_TIME_HOURS, MAX_LINES};
use recipes::{
    DraftInput, Ingredient, LineInput, LinePart, Part, Pct, Recipe, RecipeInput, RecipeKind,
    ScaleError, Scaled, SubChoice, Version, ingredient_totals, ingredients,
};
use serde::Deserialize;
use tower_sessions::Session;

use super::form::{Errors, Fields, parse_pct, pct_value};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require,
    saved, t, t_with,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

const LIST: &str = "/staff/recipes";
const NEW: &str = "/staff/recipes/new";
/// Empty line rows below the existing ones on the edit form.
const BLANK_ROWS: usize = 3;
const DEFAULT_WEIGHT_G: u64 = 1000;
const MAX_WEIGHT_G: u64 = 1_000_000;

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

fn version_path(recipe: RecipeId, version: RecipeVersionId) -> String {
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
    table: Table,
    saved: bool,
}

pub async fn list(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(flash): Query<Flash>,
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

pub struct RecipeForm {
    pub name: String,
    pub kind: String,
}

impl RecipeForm {
    fn posted(fields: &Fields) -> Self {
        Self {
            name: fields.text("name").to_owned(),
            kind: fields.text("kind").to_owned(),
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

async fn details(state: &AppState, recipe: &Recipe) -> Step<RecipeDetails> {
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
            Some(details(state, r).await?),
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
            Ok(recipe) => Ok(saved(&headers, &recipe_path(recipe.id))),
            Err(recipes::Error::Duplicate) => {
                let mut errors = Errors::default();
                errors.add("name", t(&state, "recipe-error-duplicate"));
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

pub async fn link_product(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let recipe = load_recipe(&state, &id).await?;
        let path = recipe_path(recipe.id);
        let Ok(product) = fields.text("product_id").parse::<i64>() else {
            return Ok(saved(&headers, &path));
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

#[derive(Debug, Default, Deserialize)]
pub struct VersionQuery {
    gewicht: Option<String>,
    opgeslagen: Option<String>,
}

pub struct Message {
    pub kind: &'static str,
    pub text: String,
}

#[derive(Template)]
#[template(path = "staff/recipe_version.html")]
struct VersionPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    saved: bool,
    frozen: bool,
    status: String,
    edit: String,
    freeze: String,
    current: Option<String>,
    new_version: String,
    facts: Table,
    messages: Vec<Message>,
    weight: String,
    weight_error: String,
    lines: Option<Table>,
    totals: Option<Table>,
    notes: String,
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

fn flatten(state: &AppState, scaled: &[Scaled<'_>], depth: usize, rows: &mut Vec<Vec<Cell>>) {
    for line in scaled {
        let name = match &line.line.part {
            Part::Ingredient(ingredient) => ingredient.name.clone(),
            Part::Sub(sub) => sub_label(state, &sub.name, sub.version),
        };
        rows.push(vec![
            Cell::text(format!("{}{name}", "↳ ".repeat(depth))),
            Cell::number(pct_text(line.line.pct)),
            Cell::number(grams_text(line.grams)),
        ]);
        flatten(state, &line.lines, depth + 1, rows);
    }
}

pub async fn version_page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((id, version)): Path<(String, String)>,
    Query(query): Query<VersionQuery>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (recipe, version) = load_version(&state, &id, &version).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let formula = store::formula(&mut conn, version.id)
            .await
            .map_err(|e| fail(&state, &e))?;
        let raw_weight = query.gewicht.unwrap_or_default();
        let (grams, weight_error) = if raw_weight.trim().is_empty() {
            (DEFAULT_WEIGHT_G, String::new())
        } else {
            match raw_weight.trim().parse::<u64>() {
                Ok(g) if (1..=MAX_WEIGHT_G).contains(&g) => (g, String::new()),
                _ => (DEFAULT_WEIGHT_G, t(&state, "recipe-weight-error")),
            }
        };
        let mut messages = Vec::new();
        let none = || t(&state, "recipe-none");
        let analysis = formula.analysis();
        let (lines, totals) = match formula.scale(grams) {
            Ok(scaled) => {
                let mut rows = Vec::new();
                flatten(&state, &scaled, 0, &mut rows);
                let totals = ingredient_totals(&scaled)
                    .into_iter()
                    .map(|(ingredient, grams)| {
                        vec![
                            Cell::text(ingredient.name.clone()),
                            Cell::number(grams_text(grams)),
                        ]
                    })
                    .collect();
                (
                    Some(Table {
                        id: "regels".to_owned(),
                        caption: t_with(
                            &state,
                            "recipe-lines-caption",
                            &[("grams", grams_text(grams))],
                        ),
                        head: vec![
                            Cell::text(t(&state, "recipe-line-part")),
                            Cell::number(t(&state, "recipe-line-pct")),
                            Cell::number(t(&state, "recipe-line-grams")),
                        ],
                        rows,
                    }),
                    Some(Table {
                        id: "totalen".to_owned(),
                        caption: t(&state, "recipe-totals-caption"),
                        head: vec![
                            Cell::text(t(&state, "recipe-line-part")),
                            Cell::number(t(&state, "recipe-line-grams")),
                        ],
                        rows: totals,
                    }),
                )
            }
            Err(ScaleError::Empty(None)) => {
                messages.push(Message {
                    kind: "info",
                    text: t(&state, "recipe-no-lines"),
                });
                (None, None)
            }
            Err(ScaleError::Empty(Some(_))) => {
                messages.push(Message {
                    kind: "warning",
                    text: t(&state, "recipe-empty-sub"),
                });
                (None, None)
            }
        };
        let analysis = analysis.ok();
        if let Some(a) = analysis.filter(|a| a.total_flour != Pct::HUNDRED) {
            messages.push(Message {
                kind: "warning",
                text: t_with(
                    &state,
                    "recipe-flour-off",
                    &[("pct", pct_text(a.total_flour))],
                ),
            });
        }
        let pct_or_none = |pct: Option<Pct>| pct.map_or_else(none, pct_text);
        let facts = Table {
            id: "kenmerken".to_owned(),
            caption: t_with(
                &state,
                "recipe-version-number",
                &[("n", version.version.to_string())],
            ),
            head: vec![
                Cell::text(t(&state, "recipe-status")),
                Cell::text(status_text(&state, &version)),
            ],
            rows: vec![
                vec![
                    Cell::text(t(&state, "recipe-hydration")),
                    Cell::text(pct_or_none(analysis.and_then(|a| a.hydration))),
                ],
                vec![
                    Cell::text(t(&state, "recipe-total-flour")),
                    Cell::text(pct_or_none(analysis.map(|a| a.total_flour))),
                ],
                vec![
                    Cell::text(t(&state, "recipe-prefermented-flour")),
                    Cell::text(pct_or_none(analysis.and_then(|a| a.prefermented_flour))),
                ],
                vec![
                    Cell::text(t(&state, "recipe-lead-time")),
                    Cell::text(version.lead_time_hours.map_or_else(none, |hours| {
                        t_with(
                            &state,
                            "recipe-lead-time-value",
                            &[("hours", hours.to_string())],
                        )
                    })),
                ],
            ],
        };
        let path = version_path(recipe.id, version.id);
        let status = match version.frozen_at {
            Some(at) => t_with(&state, "recipe-frozen-info", &[("date", date_text(at))]),
            None => t(&state, "recipe-draft-info"),
        };
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &VersionPage {
                layout: layout(&state, &session).await?,
                heading: t_with(
                    &state,
                    "recipe-version-title",
                    &[
                        ("name", recipe.name.clone()),
                        ("n", version.version.to_string()),
                    ],
                ),
                back: recipe_path(recipe.id),
                saved: query.opgeslagen.is_some(),
                frozen: version.is_frozen(),
                status,
                edit: format!("{path}/edit"),
                freeze: format!("{path}/freeze"),
                current: (recipe.current_version_id != Some(version.id))
                    .then(|| format!("{path}/current")),
                new_version: format!("{}/versions", recipe_path(recipe.id)),
                facts,
                messages,
                weight: if weight_error.is_empty() {
                    grams.to_string()
                } else {
                    raw_weight
                },
                weight_error,
                lines,
                totals,
                notes: version.notes.clone(),
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
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

/// One line on the edit form, as typed.
pub struct LineForm {
    pub legend: String,
    pub part_field: String,
    pub pct_field: String,
    pub options: Vec<SelectOption>,
    pub pct: String,
    pub part_error: String,
    pub pct_error: String,
}

#[derive(Template)]
#[template(path = "staff/recipe_edit.html")]
struct EditPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    action: String,
    frozen_note: Option<String>,
    form_error: Option<String>,
    has_errors: bool,
    rows: Vec<LineForm>,
    lead_time: String,
    lead_time_error: String,
    notes: String,
}

fn part_value(part: LinePart) -> String {
    match part {
        LinePart::Ingredient(id) => format!("i{}", id.get()),
        LinePart::Sub(id) => format!("v{}", id.get()),
    }
}

/// The choices of the part select: ingredients first, then versions of other recipes.
struct Choices {
    ingredients: Vec<Ingredient>,
    subs: Vec<SubChoice>,
}

impl Choices {
    fn options(&self, state: &AppState, selected: &str) -> Vec<SelectOption> {
        let ingredients = self.ingredients.iter().map(|i| {
            let value = part_value(LinePart::Ingredient(i.id));
            SelectOption {
                selected: value == selected,
                value,
                label: i.name.clone(),
            }
        });
        let subs = self.subs.iter().map(|s| {
            let value = part_value(LinePart::Sub(s.version_id));
            let id = if s.frozen {
                "recipe-sub-option"
            } else {
                "recipe-sub-option-draft"
            };
            SelectOption {
                selected: value == selected,
                value,
                label: t_with(
                    state,
                    id,
                    &[
                        ("name", s.recipe_name.clone()),
                        ("n", s.version.to_string()),
                    ],
                ),
            }
        });
        ingredients.chain(subs).collect()
    }

    fn parse(&self, value: &str) -> Option<LinePart> {
        self.ingredients
            .iter()
            .map(|i| LinePart::Ingredient(i.id))
            .chain(self.subs.iter().map(|s| LinePart::Sub(s.version_id)))
            .find(|part| part_value(*part) == value)
    }
}

async fn choices(state: &AppState, recipe: RecipeId) -> Step<Choices> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    Ok(Choices {
        ingredients: ingredients::list(&mut conn)
            .await
            .map_err(|e| fail(state, &e))?,
        subs: store::sub_choices(&mut conn, recipe)
            .await
            .map_err(|e| fail(state, &e))?,
    })
}

/// A posted or stored line before checking: the part's select value and the percentage.
struct RawLine {
    part: String,
    pct: String,
}

struct EditState {
    lines: Vec<RawLine>,
    lead_time: String,
    notes: String,
    line_errors: Vec<(String, String)>,
    lead_time_error: String,
    form_error: Option<String>,
}

async fn render_edit(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    recipe: &Recipe,
    version: &Version,
    choices: &Choices,
    edit: EditState,
) -> Outcome {
    let blanks = (0..BLANK_ROWS).map(|_| RawLine {
        part: String::new(),
        pct: String::new(),
    });
    let rows = edit
        .lines
        .into_iter()
        .chain(blanks)
        .enumerate()
        .map(|(index, line)| {
            let (part_error, pct_error) = edit.line_errors.get(index).cloned().unwrap_or_default();
            LineForm {
                legend: t_with(state, "recipe-line", &[("n", (index + 1).to_string())]),
                part_field: format!("part-{index}"),
                pct_field: format!("pct-{index}"),
                options: choices.options(state, &line.part),
                pct: line.pct,
                part_error,
                pct_error,
            }
        })
        .collect();
    let path = version_path(recipe.id, version.id);
    let has_errors = edit.form_error.is_some()
        || !edit.lead_time_error.is_empty()
        || edit
            .line_errors
            .iter()
            .any(|(part, pct)| !part.is_empty() || !pct.is_empty());
    Ok(pages::render(
        state,
        status,
        &EditPage {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "recipe-edit-title",
                &[
                    ("name", recipe.name.clone()),
                    ("n", version.version.to_string()),
                ],
            ),
            back: path.clone(),
            action: format!("{path}/edit"),
            frozen_note: version.is_frozen().then(|| {
                t_with(
                    state,
                    "recipe-edit-frozen",
                    &[("n", version.version.to_string())],
                )
            }),
            form_error: edit.form_error,
            has_errors,
            rows,
            lead_time: edit.lead_time,
            lead_time_error: edit.lead_time_error,
            notes: edit.notes,
        },
    ))
}

pub async fn edit_form(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((id, version)): Path<(String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (recipe, version) = load_version(&state, &id, &version).await?;
        let choices = choices(&state, recipe.id).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let lines = store::lines(&mut conn, version.id)
            .await
            .map_err(|e| fail(&state, &e))?
            .into_iter()
            .map(|line| RawLine {
                part: part_value(line.part),
                pct: pct_value(line.pct),
            })
            .collect();
        let edit = EditState {
            lines,
            lead_time: version
                .lead_time_hours
                .map(|h| h.to_string())
                .unwrap_or_default(),
            notes: version.notes.clone(),
            line_errors: Vec::new(),
            lead_time_error: String::new(),
            form_error: None,
        };
        render_edit(
            &state,
            &session,
            StatusCode::OK,
            &recipe,
            &version,
            &choices,
            edit,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

fn posted_lines(fields: &Fields) -> Vec<RawLine> {
    (0..MAX_LINES + BLANK_ROWS + 1)
        .map(|index| RawLine {
            part: fields.text(&format!("part-{index}")).to_owned(),
            pct: fields.text(&format!("pct-{index}")).to_owned(),
        })
        .filter(|line| !line.part.is_empty() || !line.pct.is_empty())
        .collect()
}

fn check_lines(
    state: &AppState,
    choices: &Choices,
    lines: &[RawLine],
) -> (Vec<LineInput>, Vec<(String, String)>) {
    let mut inputs = Vec::new();
    let mut errors = Vec::new();
    for line in lines {
        let part = choices.parse(&line.part);
        let pct =
            parse_pct(&line.pct).filter(|pct| pct.thousandths() > 0 && *pct <= store::MAX_LINE_PCT);
        let part_error = if part.is_none() {
            t(state, "recipe-line-error-part")
        } else {
            String::new()
        };
        let pct_error = if pct.is_none() {
            t(state, "recipe-line-error-pct")
        } else {
            String::new()
        };
        if let (Some(part), Some(pct)) = (part, pct) {
            inputs.push(LineInput { pct, part });
        }
        errors.push((part_error, pct_error));
    }
    (inputs, errors)
}

pub async fn save_draft(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, version)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (recipe, version) = load_version(&state, &id, &version).await?;
        let choices = choices(&state, recipe.id).await?;
        let lines = posted_lines(&fields);
        let (inputs, line_errors) = check_lines(&state, &choices, &lines);
        let lead_time = fields.text("lead_time").to_owned();
        let lead_time_hours = if lead_time.is_empty() {
            Ok(None)
        } else {
            lead_time
                .parse::<i32>()
                .ok()
                .filter(|h| (0..=MAX_LEAD_TIME_HOURS).contains(h))
                .map(Some)
                .ok_or(())
        };
        let mut edit = EditState {
            lead_time_error: if lead_time_hours.is_err() {
                t(&state, "recipe-lead-time-error")
            } else {
                String::new()
            },
            form_error: (lines.len() > MAX_LINES).then(|| t(&state, "recipe-error-lines")),
            lines,
            lead_time,
            notes: fields.raw("notes").trim().replace("\r\n", "\n"),
            line_errors,
        };
        let refused = edit.form_error.is_some()
            || inputs.len() != edit.lines.len()
            || lead_time_hours.is_err();
        if refused {
            return render_edit(
                &state,
                &session,
                invalid_status(&headers),
                &recipe,
                &version,
                &choices,
                edit,
            )
            .await;
        }
        let input = DraftInput {
            lines: inputs,
            lead_time_hours: lead_time_hours.unwrap_or_default(),
            notes: edit.notes.clone(),
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let message = match store::save_draft(&mut conn, version.id, &input).await {
            Ok(saved_version) => {
                return Ok(saved(&headers, &version_path(recipe.id, saved_version.id)));
            }
            Err(recipes::Error::Nesting) => t(&state, "recipe-error-nesting"),
            Err(recipes::Error::Invalid("line")) => t(&state, "recipe-error-line-gone"),
            Err(recipes::Error::DraftExists(draft)) => {
                let draft = store::version(&mut conn, recipe.id, draft)
                    .await
                    .map_err(|e| fail(&state, &e))?;
                t_with(
                    &state,
                    "recipe-error-draft-exists",
                    &[("n", draft.version.to_string())],
                )
            }
            Err(other) => return Err(fail(&state, &other).into()),
        };
        edit.form_error = Some(message);
        render_edit(
            &state,
            &session,
            invalid_status(&headers),
            &recipe,
            &version,
            &choices,
            edit,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
