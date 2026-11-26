//! Ingredients: name, what they count as, and their allergens (F14).

use std::collections::BTreeSet;

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use domain::{Allergen, IngredientId};
use recipes::{
    Ingredient, IngredientInput, IngredientRole, MAX_STARTER_HYDRATION, Pct, ingredients,
};
use tower_sessions::Session;

use super::form::{Errors, Fields, parse_pct, pct_value};
use super::products::AllergenBox;
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require,
    saved, t,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/staff/ingredients", get(list))
        .route("/staff/ingredients/new", get(new_form).post(create))
        .route("/staff/ingredients/{id}", get(edit_form).post(update))
}

const NEW: &str = "/staff/ingredients/new";

pub const fn role_label(role: IngredientRole) -> &'static str {
    match role {
        IngredientRole::Flour => "ingredient-role-flour",
        IngredientRole::Water => "ingredient-role-water",
        IngredientRole::Other => "ingredient-role-other",
        IngredientRole::Starter => "ingredient-role-starter",
    }
}

/// A starter's hydration as typed, from 0 to 500%.
pub fn parse_hydration(input: &str) -> Option<Pct> {
    parse_pct(input).filter(|h| *h <= MAX_STARTER_HYDRATION)
}

pub struct IngredientForm {
    pub name: String,
    pub label_name: String,
    pub role: String,
    pub hydration: String,
    pub supplier: String,
    pub allergens: Vec<AllergenBox>,
}

fn boxes(checked: impl Fn(Allergen) -> bool) -> Vec<AllergenBox> {
    Allergen::ALL
        .into_iter()
        .map(|allergen| AllergenBox {
            field: format!("allergen_{}", allergen.code()),
            code: allergen.code(),
            checked: checked(allergen),
        })
        .collect()
}

impl IngredientForm {
    fn empty() -> Self {
        Self {
            name: String::new(),
            label_name: String::new(),
            role: String::new(),
            hydration: String::new(),
            supplier: String::new(),
            allergens: boxes(|_| false),
        }
    }

    fn of(ingredient: &Ingredient) -> Self {
        Self {
            name: ingredient.name.clone(),
            label_name: ingredient.label_name.clone(),
            role: ingredient.role.code().to_owned(),
            hydration: ingredient.hydration.map(pct_value).unwrap_or_default(),
            supplier: ingredient.supplier_name.clone(),
            allergens: boxes(|a| ingredient.allergens.contains(&a)),
        }
    }

    fn posted(fields: &Fields) -> Self {
        Self {
            name: fields.text("name").to_owned(),
            label_name: fields.text("label_name").to_owned(),
            role: fields.text("role").to_owned(),
            hydration: fields.text("hydration").to_owned(),
            supplier: fields.text("supplier").to_owned(),
            allergens: boxes(|a| fields.checked(&format!("allergen_{}", a.code()))),
        }
    }

    fn validate(&self, state: &AppState) -> Result<IngredientInput, Errors> {
        let mut errors = Errors::default();
        if self.name.is_empty() {
            errors.add("name", t(state, "ingredient-error-name"));
        }
        let role = self.role.parse::<IngredientRole>();
        if role.is_err() {
            errors.add("role", t(state, "ingredient-error-role"));
        }
        let hydration = match role {
            Ok(IngredientRole::Starter) => {
                let parsed = parse_hydration(&self.hydration);
                if parsed.is_none() {
                    errors.add("hydration", t(state, "ingredient-error-hydration"));
                }
                parsed
            }
            _ => None,
        };
        match role {
            Ok(role) if errors.is_empty() => Ok(IngredientInput {
                name: self.name.clone(),
                label_name: self.label_name.clone(),
                role,
                hydration,
                supplier_name: self.supplier.clone(),
                allergens: Allergen::ALL
                    .into_iter()
                    .zip(&self.allergens)
                    .filter(|(_, b)| b.checked)
                    .map(|(a, _)| a)
                    .collect::<BTreeSet<_>>(),
            }),
            _ => Err(errors),
        }
    }

    fn role_options(&self, state: &AppState) -> Vec<SelectOption> {
        IngredientRole::ALL
            .into_iter()
            .map(|role| SelectOption {
                value: role.code().to_owned(),
                label: t(state, role_label(role)),
                selected: role.code() == self.role,
            })
            .collect()
    }
}

#[derive(Template)]
#[template(path = "staff/ingredients.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    table: Table,
    saved: bool,
}

#[derive(Template)]
#[template(path = "staff/ingredient.html")]
struct IngredientPage<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    form: IngredientForm,
    roles: Vec<SelectOption>,
    errors: Errors,
    saved: bool,
}

fn ingredient_path(id: IngredientId) -> String {
    format!("/staff/ingredients/{}", id.get())
}

pub fn allergen_names(state: &AppState, allergens: &BTreeSet<Allergen>) -> String {
    if allergens.is_empty() {
        return t(state, "ingredient-allergens-none");
    }
    allergens
        .iter()
        .map(|a| t(state, &format!("allergen-{}", a.code())))
        .collect::<Vec<_>>()
        .join(", ")
}

pub async fn list(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = ingredients::list(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        let table = Table {
            id: "ingredienten".to_owned(),
            caption: t(&state, "ingredients-caption"),
            head: vec![
                Cell::text(t(&state, "ingredient-name")),
                Cell::text(t(&state, "ingredient-role")),
                Cell::text(t(&state, "ingredient-allergens")),
                Cell::text(t(&state, "ingredient-supplier")),
            ],
            rows: all
                .iter()
                .map(|i| {
                    vec![
                        Cell::link(i.name.clone(), ingredient_path(i.id)),
                        Cell::text(t(&state, role_label(i.role))),
                        Cell::text(allergen_names(&state, &i.allergens)),
                        Cell::text(i.supplier_name.clone()),
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
    ingredient: Option<&Ingredient>,
    form: IngredientForm,
    errors: Errors,
    saved: bool,
) -> Outcome {
    let (heading, action) = match ingredient {
        Some(i) => (i.name.clone(), ingredient_path(i.id)),
        None => (t(state, "ingredient-new"), NEW.to_owned()),
    };
    Ok(pages::render(
        state,
        status,
        &IngredientPage {
            layout: layout(state, session).await?,
            heading,
            action,
            roles: form.role_options(state),
            form,
            errors,
            saved,
        },
    ))
}

async fn load(state: &AppState, raw_id: &str) -> Step<Ingredient> {
    let id = IngredientId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    ingredients::get(&mut conn, id).await.map_err(|e| match e {
        recipes::Error::NotFound => not_found(state).into(),
        other => fail(state, &other).into(),
    })
}

pub async fn new_form(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        render_form(
            &state,
            &session,
            StatusCode::OK,
            None,
            IngredientForm::empty(),
            Errors::default(),
            false,
        )
        .await
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
        require(&state, &staff, Permission::EditAllergens)?;
        let ingredient = load(&state, &id).await?;
        render_form(
            &state,
            &session,
            StatusCode::OK,
            Some(&ingredient),
            IngredientForm::of(&ingredient),
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
    save(state, staff, session, headers, None, fields).await
}

pub async fn update(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    save(state, staff, session, headers, Some(id), fields).await
}

async fn save(
    state: AppState,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    id: Option<String>,
    fields: Fields,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let existing = match &id {
            Some(id) => Some(load(&state, id).await?),
            None => None,
        };
        let form = IngredientForm::posted(&fields);
        let refused = |form, errors| {
            render_form(
                &state,
                &session,
                invalid_status(&headers),
                existing.as_ref(),
                form,
                errors,
                false,
            )
        };
        let input = match form.validate(&state) {
            Ok(input) => input,
            Err(errors) => return refused(form, errors).await,
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = match &existing {
            Some(i) => ingredients::update(&mut conn, i.id, &input).await,
            None => ingredients::create(&mut conn, &input).await,
        };
        match result {
            Ok(ingredient) => Ok(saved(&headers, &ingredient_path(ingredient.id))),
            Err(recipes::Error::Duplicate) => {
                let mut errors = Errors::default();
                errors.add("name", t(&state, "ingredient-error-duplicate"));
                refused(form, errors).await
            }
            Err(recipes::Error::InUse) => {
                let mut errors = Errors::default();
                if existing.as_ref().is_some_and(|i| i.role == input.role) {
                    errors.add("hydration", t(&state, "ingredient-error-hydration-frozen"));
                } else {
                    errors.add("role", t(&state, "ingredient-error-role-frozen"));
                }
                refused(form, errors).await
            }
            Err(other) => Err(fail(&state, &other).into()),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
