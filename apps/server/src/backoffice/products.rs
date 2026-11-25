//! Products and the allergen approval (F10, F4b).

use std::collections::BTreeSet;

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use catalog::{
    AllergenInfo, ComputedAllergens, IngredientList, ListedIngredient, Pricing, Product,
    ProductInput, products,
};
use domain::{Allergen, Money, PayWhatYouWant, ProductId, VatRate};
use serde::Deserialize;
use tower_sessions::Session;

use super::form::{Errors, Fields, money_value, parse_money};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require,
    saved, t, t_with,
};
use crate::components::{Cell, ListedIngredient as Listed, SelectOption, Table, listed};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/staff/products", get(list))
        .route("/staff/products/new", get(new_form).post(create))
        .route("/staff/products/check", post(check))
        .route("/staff/products/{id}", get(edit_form).post(update))
        .route(
            "/staff/products/{id}/approve",
            get(approve_form).post(approve),
        )
        .route(
            "/staff/products/{id}/approve-computed",
            post(approve_computed),
        )
}

const CHECK: &str = "/staff/products/check";
/// The highest price a product may have, so that a Dutch thousands dot typed as a decimal
/// point (`4.500`) is caught.
const MAX_PRICE_CENTS: i64 = 100_000;

pub struct AllergenBox {
    pub field: String,
    pub code: &'static str,
    pub checked: bool,
}

/// What the product form shows, as typed.
pub struct ProductForm {
    pub name: String,
    pub description: String,
    pub price: String,
    pub vat_rate: String,
    pub pwyw: bool,
    pub min_price: String,
    pub max_price: String,
    pub unit_weight: String,
    /// `None` when the post had no such field: the stored value stays.
    pub dough_weight: Option<String>,
    pub sold_online: bool,
    pub active: bool,
    pub allergens: Vec<AllergenBox>,
    pub ingredient_text: String,
}

pub(super) fn boxes(prefix: &str, checked: impl Fn(Allergen) -> bool) -> Vec<AllergenBox> {
    Allergen::ALL
        .into_iter()
        .map(|allergen| AllergenBox {
            field: format!("{prefix}_{}", allergen.code()),
            code: allergen.code(),
            checked: checked(allergen),
        })
        .collect()
}

impl ProductForm {
    pub fn dough_value(&self) -> &str {
        self.dough_weight.as_deref().unwrap_or_default()
    }

    fn empty() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            price: String::new(),
            vat_rate: VatRate::Nine.percent().to_string(),
            pwyw: false,
            min_price: String::new(),
            max_price: String::new(),
            unit_weight: String::new(),
            dough_weight: Some(String::new()),
            sold_online: true,
            active: true,
            allergens: boxes("allergen", |_| false),
            ingredient_text: String::new(),
        }
    }

    fn of(product: &Product) -> Self {
        let (min, max) = match product.pricing {
            Pricing::Fixed(_) => (String::new(), String::new()),
            Pricing::PayWhatYouWant(pwyw) => (
                money_value(pwyw.min().cents()),
                money_value(pwyw.max().cents()),
            ),
        };
        let info = &product.allergen_info;
        Self {
            name: product.name_nl.clone(),
            description: product.description_nl.clone(),
            price: money_value(product.pricing.price_incl().cents()),
            vat_rate: product.vat_rate.percent().to_string(),
            pwyw: matches!(product.pricing, Pricing::PayWhatYouWant(_)),
            min_price: min,
            max_price: max,
            unit_weight: product.unit_weight_g.to_string(),
            dough_weight: Some(product.dough_g_per_piece.to_string()),
            sold_online: product.sold_online,
            active: product.active,
            allergens: boxes("allergen", |a| info.allergens.contains(&a)),
            ingredient_text: info.ingredient_text.clone(),
        }
    }

    fn posted(fields: &Fields) -> Self {
        Self {
            name: fields.text("name").to_owned(),
            description: fields.raw("description").to_owned(),
            price: fields.text("price").to_owned(),
            vat_rate: fields.text("vat_rate").to_owned(),
            pwyw: fields.checked("pwyw"),
            min_price: fields.text("min_price").to_owned(),
            max_price: fields.text("max_price").to_owned(),
            unit_weight: fields.text("unit_weight").to_owned(),
            dough_weight: fields
                .all("dough_weight")
                .next()
                .map(|v| v.trim().to_owned()),
            sold_online: fields.checked("sold_online"),
            active: fields.checked("active"),
            allergens: boxes("allergen", |a| {
                fields.checked(&format!("allergen_{}", a.code()))
            }),
            ingredient_text: one_newline(fields.raw("ingredient_text").trim()),
        }
    }

    fn vat_options(&self) -> Vec<SelectOption> {
        VatRate::ALL
            .into_iter()
            .map(|rate| {
                let value = rate.percent().to_string();
                SelectOption {
                    label: format!("{value}%"),
                    selected: value == self.vat_rate,
                    value,
                }
            })
            .collect()
    }

    fn allergen_info(&self) -> AllergenInfo {
        let set = |boxes: &[AllergenBox]| -> BTreeSet<Allergen> {
            boxes
                .iter()
                .filter(|b| b.checked)
                .filter_map(|b| b.code.parse().ok())
                .collect()
        };
        AllergenInfo {
            allergens: set(&self.allergens),
            ingredient_text: self.ingredient_text.clone(),
        }
    }

    /// The input for the catalogue, or the Dutch message per wrong field. Fields the form
    /// does not show are taken from `existing`.
    fn validate(
        &self,
        state: &AppState,
        existing: Option<&Product>,
    ) -> Result<ProductInput, Errors> {
        let mut errors = Errors::default();
        if self.name.is_empty() {
            errors.add("name", t(state, "product-error-name"));
        }
        let money = |errors: &mut Errors, field: &str, value: &str| {
            let cents = parse_money(value);
            match cents {
                None => errors.add(field, t(state, "form-error-money")),
                Some(c) if c > MAX_PRICE_CENTS => errors.add(
                    field,
                    t_with(
                        state,
                        "product-error-price-high",
                        &[("max", i18n::money(MAX_PRICE_CENTS))],
                    ),
                ),
                Some(_) => {}
            }
            cents.map(Money::from_cents)
        };
        let price = money(&mut errors, "price", &self.price);
        let vat_rate = VatRate::ALL
            .into_iter()
            .find(|rate| rate.percent().to_string() == self.vat_rate);
        if vat_rate.is_none() {
            errors.add("vat_rate", t(state, "product-error-vat"));
        }
        let pwyw = if self.pwyw {
            let min = money(&mut errors, "min_price", &self.min_price);
            let max = money(&mut errors, "max_price", &self.max_price);
            match (price, min, max) {
                (Some(price), Some(min), Some(max)) => {
                    if PayWhatYouWant::new(price, min, max).is_err() {
                        errors.add("min_price", t(state, "product-error-pwyw"));
                    }
                    Some((min, max))
                }
                _ => None,
            }
        } else {
            None
        };
        let unit_weight = self.unit_weight.parse::<i32>().ok().filter(|g| *g > 0);
        if unit_weight.is_none() {
            errors.add("unit_weight", t(state, "product-error-weight"));
        }
        let dough_weight = match self.dough_weight.as_deref() {
            None => Some(existing.map_or(0, |p| p.dough_g_per_piece)),
            Some("") => Some(0),
            Some(raw) => raw
                .parse::<i32>()
                .ok()
                .filter(|g| (0..=1_000_000).contains(g)),
        };
        if dough_weight.is_none() {
            errors.add("dough_weight", t(state, "product-error-dough-weight"));
        }
        match (price, vat_rate, unit_weight) {
            (Some(price_incl), Some(vat_rate), Some(unit_weight_g)) if errors.is_empty() => {
                Ok(ProductInput {
                    name_nl: self.name.clone(),
                    description_nl: self.description.trim().to_owned(),
                    photo_path: existing.and_then(|p| p.photo_path.clone()),
                    price_incl,
                    vat_rate,
                    pay_what_you_want: pwyw,
                    unit_weight_g,
                    dough_g_per_piece: dough_weight.unwrap_or(0),
                    freezer_days: existing.and_then(|p| p.freezer_days),
                    lead_time_hours: existing.and_then(|p| p.lead_time_hours),
                    allergen_info: self.allergen_info(),
                    packaged: existing.is_some_and(|p| p.packaged),
                    sold_online: self.sold_online,
                    estimated_cost: existing.and_then(|p| p.estimated_cost),
                    active: self.active,
                })
            }
            _ => Err(errors),
        }
    }
}

/// The approval as the edit page shows it.
pub struct ApprovalView {
    pub approved: bool,
    pub text: String,
}

#[derive(Template)]
#[template(path = "staff/product.html")]
struct ProductPage<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    check: &'static str,
    form: ProductForm,
    vat_options: Vec<SelectOption>,
    errors: Errors,
    saved: bool,
    approval: Option<ApprovalView>,
    approve_href: String,
    recheck: bool,
}

#[derive(Template)]
#[template(path = "staff/products.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    table: Table,
    kitchen_unapproved: bool,
}

#[derive(Template)]
#[template(path = "staff/product_approve.html")]
struct ApprovePage<'a> {
    layout: Layout<'a>,
    name: String,
    action: String,
    back: String,
    allergens: Vec<&'static str>,
    ingredient_text: String,
    current_list: Option<Vec<Listed>>,
    revision: i64,
    changed: bool,
    computed: Option<ComputedView>,
    uncomputable: bool,
    computed_only: bool,
}

/// Values computed from a recipe or batch, with what their approval form sends back.
pub struct ComputedView {
    pub source: String,
    pub allergens: Vec<&'static str>,
    pub ingredients: Vec<Listed>,
    pub same: bool,
    pub action: String,
}

impl ComputedView {
    pub(super) fn new(
        source: String,
        computed: &ComputedAllergens,
        same: bool,
        action: String,
    ) -> Self {
        Self {
            source,
            allergens: codes(&computed.allergens),
            ingredients: listed(&computed.ingredients),
            same,
            action,
        }
    }
}

/// The values an approval form sent back; `None` when the names and marks do not pair up.
pub(super) fn reviewed_computed(fields: &Fields) -> Option<ComputedAllergens> {
    let names: Vec<&str> = fields.all("ingredient").collect();
    let marks: Vec<&str> = fields.all("vet").collect();
    if names.len() != marks.len() {
        return None;
    }
    let ingredients = names
        .into_iter()
        .zip(marks)
        .map(|(name, mark)| ListedIngredient {
            name: name.to_owned(),
            emphasised: mark == "1",
        })
        .collect();
    Some(ComputedAllergens {
        allergens: fields
            .all("allergens")
            .filter_map(|code| code.parse().ok())
            .collect(),
        ingredients: IngredientList(ingredients),
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct EditFlash {
    opgeslagen: Option<String>,
    herkeuren: Option<String>,
}

fn product_path(id: ProductId) -> String {
    format!("/staff/products/{}", id.get())
}

async fn approval_view(state: &AppState, product: &Product) -> Step<ApprovalView> {
    let missing = match product.approval_cleared {
        None => "product-not-approved",
        Some(catalog::ApprovalCleared::Recipe) => "product-approval-cleared-recipe",
        Some(catalog::ApprovalCleared::Ingredient) => "product-approval-cleared-ingredient",
    };
    approval_text(state, product.approval, missing).await
}

/// Who approved and when, or `missing` when nobody has.
pub(super) async fn approval_text(
    state: &AppState,
    approval: Option<catalog::Approval>,
    missing: &str,
) -> Step<ApprovalView> {
    let Some(approval) = approval else {
        return Ok(ApprovalView {
            approved: false,
            text: t(state, missing),
        });
    };
    let by = auth::staff::find(&state.pool, approval.by)
        .await
        .map_err(|error| fail(state, &error))?
        .map_or_else(|| approval.by.get().to_string(), |staff| staff.name);
    let at = i18n::Amsterdam::new().local(approval.at);
    Ok(ApprovalView {
        approved: true,
        text: super::t_with(
            state,
            "product-approved-by",
            &[
                ("name", by),
                ("date", state.i18n.long_date(at.date())),
                ("time", i18n::time(at.time())),
            ],
        ),
    })
}

fn catalog_error(state: &AppState, error: &catalog::Error) -> Response {
    match error {
        catalog::Error::NotFound => not_found(state),
        other => fail(state, other),
    }
}

async fn load(state: &AppState, raw_id: &str) -> Step<Product> {
    let id = ProductId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    products::get(&mut conn, id)
        .await
        .map_err(|e| catalog_error(state, &e).into())
}

pub async fn list(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    run_list(&state, &staff, &session)
        .await
        .unwrap_or_else(Rejection::into_response)
}

async fn run_list(state: &AppState, staff: &Staff, session: &Session) -> Outcome {
    require(state, staff, Permission::EditAllergens)?;
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let all = products::list_all(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    let kitchen = catalog::company::kitchen_may_contain(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    let yes_no = |b: bool| t(state, if b { "form-yes" } else { "form-no" });
    let table = Table {
        id: "producten".to_owned(),
        caption: t(state, "products-caption"),
        head: vec![
            Cell::text(t(state, "product-name")),
            Cell::number(t(state, "product-price")),
            Cell::number(t(state, "product-vat")),
            Cell::text(t(state, "product-sold-online")),
            Cell::text(t(state, "product-active")),
            Cell::text(t(state, "product-allergen-status")),
        ],
        rows: all
            .iter()
            .map(|p| {
                vec![
                    Cell::link(p.name_nl.clone(), product_path(p.id)),
                    Cell::number(i18n::money(p.pricing.price_incl().cents())),
                    Cell::number(format!("{}%", p.vat_rate.percent())),
                    Cell::text(yes_no(p.sold_online)),
                    Cell::text(yes_no(p.active)),
                    Cell::text(t(
                        state,
                        if p.approval.is_some() {
                            "product-approved"
                        } else {
                            "product-not-approved"
                        },
                    )),
                ]
            })
            .collect(),
    };
    Ok(pages::render(
        state,
        StatusCode::OK,
        &ListPage {
            layout: layout(state, session).await?,
            table,
            kitchen_unapproved: kitchen.approval.is_none(),
        },
    ))
}

pub async fn new_form(State(state): State<AppState>, staff: Staff, session: Session) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let form = ProductForm::empty();
        render_form(
            &state,
            &session,
            StatusCode::OK,
            None,
            form,
            Errors::default(),
            &Flash::default(),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

async fn render_form(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    product: Option<(&Product, bool)>,
    form: ProductForm,
    errors: Errors,
    flash: &Flash,
) -> Outcome {
    let (heading, action, approval, approve_href, recheck) = match product {
        Some((product, recheck)) => (
            product.name_nl.clone(),
            product_path(product.id),
            Some(approval_view(state, product).await?),
            format!("{}/approve", product_path(product.id)),
            recheck,
        ),
        None => (
            t(state, "product-new"),
            "/staff/products/new".to_owned(),
            None,
            String::new(),
            false,
        ),
    };
    Ok(pages::render(
        state,
        status,
        &ProductPage {
            layout: layout(state, session).await?,
            heading,
            action,
            check: CHECK,
            vat_options: form.vat_options(),
            form,
            errors,
            saved: flash.saved(),
            approval,
            approve_href,
            recheck,
        },
    ))
}

pub async fn create(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let form = ProductForm::posted(&fields);
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
                    &Flash::default(),
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let product = products::create(&mut conn, &input)
            .await
            .map_err(|e| fail(&state, &e))?;
        Ok(saved(&headers, &product_path(product.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Inline check while typing: the form page with this input's errors. htmx picks out
/// the field that changed.
pub async fn check(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let form = ProductForm::posted(&fields);
        let errors = form.validate(&state, None).err().unwrap_or_default();
        render_form(
            &state,
            &session,
            StatusCode::OK,
            None,
            form,
            errors,
            &Flash::default(),
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
    Query(flash): Query<EditFlash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let product = load(&state, &id).await?;
        let form = ProductForm::of(&product);
        render_form(
            &state,
            &session,
            StatusCode::OK,
            Some((&product, flash.herkeuren.is_some())),
            form,
            Errors::default(),
            &Flash {
                opgeslagen: flash.opgeslagen,
            },
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
        require(&state, &staff, Permission::EditAllergens)?;
        let before = load(&state, &id).await?;
        let form = ProductForm::posted(&fields);
        let input = match form.validate(&state, Some(&before)) {
            Ok(input) => input,
            Err(errors) => {
                return render_form(
                    &state,
                    &session,
                    invalid_status(&headers),
                    Some((&before, false)),
                    form,
                    errors,
                    &Flash::default(),
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let after = products::update(&mut conn, before.id, &input)
            .await
            .map_err(|e| catalog_error(&state, &e))?;
        let path = product_path(after.id);
        if before.approval.is_some() && after.approval.is_none() {
            return Ok(saved(&headers, &format!("{path}?herkeuren=1")));
        }
        Ok(saved(&headers, &path))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub(super) fn codes(set: &BTreeSet<Allergen>) -> Vec<&'static str> {
    set.iter().map(|a| a.code()).collect()
}

async fn render_approve(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    product: &Product,
    changed: bool,
) -> Outcome {
    let info = &product.allergen_info;
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let rolled = recipes::rollup::for_product(&mut conn, product.id)
        .await
        .map_err(|e| fail(state, &e))?;
    drop(conn);
    let uncomputable = rolled.as_ref().is_some_and(|r| r.computed.is_none());
    let computed = rolled.and_then(|rolled| {
        let computed = rolled.computed?;
        let same = computed.allergens == info.allergens
            && product.ingredient_list.as_ref() == Some(&computed.ingredients);
        Some(ComputedView::new(
            t_with(
                state,
                "rollup-source",
                &[
                    ("recipe", rolled.recipe_name),
                    ("n", rolled.version.to_string()),
                ],
            ),
            &computed,
            same,
            format!("{}/approve-computed", product_path(product.id)),
        ))
    });
    Ok(pages::render(
        state,
        status,
        &ApprovePage {
            layout: layout(state, session).await?,
            name: product.name_nl.clone(),
            action: format!("{}/approve", product_path(product.id)),
            back: product_path(product.id),
            allergens: codes(&info.allergens),
            ingredient_text: info.ingredient_text.clone(),
            current_list: product.ingredient_list.as_ref().map(listed),
            revision: product.allergen_revision,
            changed,
            computed,
            uncomputable,
            computed_only: product.has_recipe,
        },
    ))
}

/// Approves the values the recipe computes, when they are still what the page showed
/// (F17). They become the product's allergens and ingredient text.
pub async fn approve_computed(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let product = load(&state, &id).await?;
        let result = match reviewed_computed(&fields) {
            Some(reviewed) => {
                let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
                recipes::rollup::approve_product(
                    &mut conn,
                    product.id,
                    &reviewed,
                    revision(&fields),
                    staff.0.id,
                    state.clock.as_ref(),
                )
                .await
                .map(|_| ())
            }
            None => Err(recipes::Error::AllergensChanged),
        };
        match result {
            Ok(()) => Ok(saved(&headers, &product_path(product.id))),
            Err(recipes::Error::AllergensChanged | recipes::Error::NotFound) => {
                let product = load(&state, &id).await?;
                render_approve(&state, &session, StatusCode::CONFLICT, &product, true).await
            }
            Err(error) => Err(fail(&state, &error).into()),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn approve_form(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let product = load(&state, &id).await?;
        render_approve(&state, &session, StatusCode::OK, &product, false).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Approves exactly the data the page showed: it comes back in hidden fields, and the
/// catalogue refuses it when the product changed in the meantime.
pub async fn approve(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let product = load(&state, &id).await?;
        let reviewed = reviewed(&fields);
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = products::approve_allergens(
            &mut conn,
            product.id,
            &reviewed,
            revision(&fields),
            staff.0.id,
            state.clock.as_ref(),
        )
        .await;
        drop(conn);
        match result {
            Ok(_) => Ok(saved(&headers, &product_path(product.id))),
            Err(catalog::Error::AllergensChanged) => {
                render_approve(&state, &session, StatusCode::CONFLICT, &product, true).await
            }
            Err(catalog::Error::ComputedOnly) => {
                let product = load(&state, &id).await?;
                render_approve(&state, &session, StatusCode::CONFLICT, &product, false).await
            }
            Err(error) => Err(catalog_error(&state, &error).into()),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// A missing or broken revision never matches, so the approval is refused.
fn revision(fields: &Fields) -> i64 {
    fields.raw("revisie").parse().unwrap_or(-1)
}

fn reviewed(fields: &Fields) -> AllergenInfo {
    let set = |name: &str| -> BTreeSet<Allergen> {
        fields
            .all(name)
            .filter_map(|code| code.parse().ok())
            .collect()
    };
    AllergenInfo {
        allergens: set("allergens"),
        ingredient_text: one_newline(fields.raw("ingredient_text")),
    }
}

/// Browsers send line breaks as CRLF; the catalogue keeps LF, so an approval compares equal.
fn one_newline(text: &str) -> String {
    text.replace("\r\n", "\n")
}
