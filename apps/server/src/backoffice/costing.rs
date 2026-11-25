//! Costing (F16, ADR 0045): ingredient prices with their history, the standard cost per
//! product and the cost and variance of baked batches. Owner only.

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use catalog::products;
use domain::{BatchId, IngredientId, Money, ProductId};
use jiff::ToSpan;
use production::costing::{self, BatchCost};
use recipes::cost::{CostError, CostLine, DoughCost, STANDARD_PIECES, Variance, round_div};
use recipes::prices::{self, NewPrice, NoCost, StandardCost};
use recipes::{Ingredient, ingredients};
use tower_sessions::Session;

use super::form::{Errors, Fields, parse_date, parse_money};
use super::reports::money;
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require,
    saved, t, t_with,
};
use crate::components::{Cell, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

/// How far back the overview lists baked batches.
const BATCH_DAYS: i32 = 60;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/staff/costing", get(overview))
        .route(
            "/staff/costing/ingredients/{id}",
            get(ingredient_page).post(add_price),
        )
        .route("/staff/costing/products/{id}", get(product_page))
        .route("/staff/costing/batches/{id}", get(batch_page))
}

fn ingredient_path(id: IngredientId) -> String {
    format!("/staff/costing/ingredients/{}", id.get())
}

fn product_path(id: ProductId) -> String {
    format!("/staff/costing/products/{}", id.get())
}

fn batch_path(id: BatchId) -> String {
    format!("/staff/costing/batches/{}", id.get())
}

fn per_kg(state: &AppState, price: Option<Money>) -> Cell {
    price.map_or_else(
        || Cell::muted(t(state, "costing-no-price")),
        |p| Cell::number(money(p)),
    )
}

fn grams(value: u64) -> String {
    i18n::decimal(i64::try_from(value).unwrap_or(i64::MAX), 0)
}

/// Thousandths of a gram as grams with one decimal: `457,6`.
fn milligrams(value: u64) -> String {
    i18n::decimal(round_div(i128::from(value), 100), 1)
}

/// Millicents ÷ `pieces` as euros to a hundredth of a cent, `€ 0,4347`, so small lines
/// still show.
fn fine_money(millicents: i128, pieces: i128) -> String {
    format!(
        "€\u{a0}{}",
        i18n::decimal(round_div(millicents, 10 * pieces), 4)
    )
}

fn missing_text(state: &AppState, cost: &DoughCost) -> Option<String> {
    let names: Vec<&str> = cost.missing().map(|i| i.name.as_str()).collect();
    (!names.is_empty()).then(|| {
        t_with(
            state,
            "costing-missing",
            &[("ingredients", names.join(", "))],
        )
    })
}

const fn no_cost_message(why: NoCost) -> &'static str {
    match why {
        NoCost::NoRecipe => "costing-why-no-recipe",
        NoCost::NoVersion => "costing-why-no-version",
        NoCost::NoLines => "costing-why-no-lines",
        NoCost::NoDough => "costing-why-no-dough",
    }
}

fn recipe_text(state: &AppState, cost: &StandardCost) -> String {
    cost.product
        .recipe
        .as_ref()
        .map_or_else(String::new, |recipe| match recipe.version {
            Some((_, n)) => t_with(
                state,
                "costing-recipe-version",
                &[("recipe", recipe.name.clone()), ("n", n.to_string())],
            ),
            None => recipe.name.clone(),
        })
}

fn per_piece_cell(state: &AppState, cost: &StandardCost) -> Cell {
    match (&cost.cost, cost.per_piece()) {
        (_, Some(amount)) => Cell::number(money(amount)),
        (Err(why), None) => Cell::muted(t(state, no_cost_message(*why))),
        (Ok(_), None) => Cell::muted(t(state, "costing-why-no-price")),
    }
}

#[derive(Template)]
#[template(path = "staff/costing.html")]
struct OverviewPage<'a> {
    layout: Layout<'a>,
    standard_date: String,
    prices: Table,
    products: Table,
    batches: Table,
}

async fn overview(State(state): State<AppState>, staff: Staff) -> Response {
    let run = async {
        require(&state, &staff, Permission::Costing)?;
        let today = prices::today(state.clock.as_ref());
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = ingredients::list(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        let valid = prices::valid_on(&mut conn, today)
            .await
            .map_err(|e| fail(&state, &e))?;
        let products = products::list_all(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        let ids: Vec<ProductId> = products.iter().map(|p| p.id).collect();
        let costs = prices::standard_costs(&mut conn, &ids, today)
            .await
            .map_err(|e| fail(&state, &e))?;
        let since = today.saturating_sub(BATCH_DAYS.days());
        let baked = costing::baked_since(&mut conn, since)
            .await
            .map_err(|e| fail(&state, &e))?;
        drop(conn);
        let prices = Table {
            id: "prijzen".to_owned(),
            caption: t(&state, "costing-prices-caption"),
            head: vec![
                Cell::text(t(&state, "costing-col-ingredient")),
                Cell::number(t(&state, "costing-col-per-kg")),
                Cell::text(t(&state, "costing-col-valid-from")),
            ],
            rows: all
                .iter()
                .map(|i| {
                    let price = valid.get(&i.id);
                    vec![
                        Cell::link(i.name.clone(), ingredient_path(i.id)),
                        per_kg(&state, price.map(|p| p.per_kg)),
                        Cell::text(
                            price.map_or_else(String::new, |p| i18n::numeric_date(p.valid_from)),
                        ),
                    ]
                })
                .collect(),
        };
        let products = Table {
            id: "producten".to_owned(),
            caption: t(&state, "costing-products-caption"),
            head: vec![
                Cell::text(t(&state, "costing-col-product")),
                Cell::text(t(&state, "costing-col-recipe")),
                Cell::number(t(&state, "costing-col-dough")),
                Cell::number(t(&state, "costing-col-per-piece")),
                Cell::number(t(&state, "costing-col-sale-price")),
            ],
            rows: products
                .iter()
                .filter_map(|p| {
                    let cost = costs.iter().find(|c| c.product.product_id == p.id)?;
                    Some(vec![
                        Cell::link(p.name_nl.clone(), product_path(p.id)),
                        Cell::text(recipe_text(&state, cost)),
                        Cell::number(grams(u64::from(cost.product.dough_g_per_piece))),
                        per_piece_cell(&state, cost),
                        Cell::number(money(p.pricing.price_incl())),
                    ])
                })
                .collect(),
        };
        let batches = Table {
            id: "batches".to_owned(),
            caption: t_with(
                &state,
                "costing-batches-caption",
                &[("days", BATCH_DAYS.to_string())],
            ),
            head: vec![
                Cell::text(t(&state, "costing-col-code")),
                Cell::text(t(&state, "costing-col-bake-date")),
                Cell::text(t(&state, "costing-col-recipe")),
            ],
            rows: baked
                .iter()
                .map(|b| {
                    vec![
                        Cell::link(b.code.to_string(), batch_path(b.id)),
                        Cell::text(i18n::numeric_date(b.bake_date)),
                        Cell::text(t_with(
                            &state,
                            "costing-recipe-version",
                            &[
                                ("recipe", b.recipe_name.clone()),
                                ("n", b.version.to_string()),
                            ],
                        )),
                    ]
                })
                .collect(),
        };
        Ok::<_, Rejection>(pages::render(
            &state,
            StatusCode::OK,
            &OverviewPage {
                layout: Layout::new(&state),
                standard_date: t_with(
                    &state,
                    "costing-standard-date",
                    &[("date", i18n::numeric_date(today))],
                ),
                prices,
                products,
                batches,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub struct PriceForm {
    pub per_kg: String,
    pub valid_from: String,
}

#[derive(Template)]
#[template(path = "staff/costing_ingredient.html")]
struct IngredientPage<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    history: Table,
    form: PriceForm,
    errors: Errors,
    saved: bool,
}

async fn load_ingredient(state: &AppState, raw_id: &str) -> Step<Ingredient> {
    let id = IngredientId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    ingredients::get(&mut conn, id).await.map_err(|e| match e {
        recipes::Error::NotFound => not_found(state).into(),
        other => fail(state, &other).into(),
    })
}

async fn render_ingredient(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    ingredient: &Ingredient,
    form: PriceForm,
    errors: Errors,
    saved: bool,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let history = prices::history(&mut conn, ingredient.id)
        .await
        .map_err(|e| fail(state, &e))?;
    drop(conn);
    let history = Table {
        id: "prijshistorie".to_owned(),
        caption: t(state, "costing-history-caption"),
        head: vec![
            Cell::text(t(state, "costing-col-valid-from")),
            Cell::number(t(state, "costing-col-per-kg")),
            Cell::text(t(state, "costing-col-entered")),
        ],
        rows: history
            .iter()
            .map(|p| {
                vec![
                    Cell::text(i18n::numeric_date(p.valid_from)),
                    Cell::number(money(p.per_kg)),
                    Cell::text(i18n::numeric_date(
                        domain::in_bakery_time(p.created_at).date(),
                    )),
                ]
            })
            .collect(),
    };
    Ok(pages::render(
        state,
        status,
        &IngredientPage {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "costing-ingredient-heading",
                &[("name", ingredient.name.clone())],
            ),
            action: ingredient_path(ingredient.id),
            history,
            form,
            errors,
            saved,
        },
    ))
}

async fn ingredient_page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Costing)?;
        let ingredient = load_ingredient(&state, &id).await?;
        let today = prices::today(state.clock.as_ref());
        let form = PriceForm {
            per_kg: String::new(),
            valid_from: today.to_string(),
        };
        render_ingredient(
            &state,
            &session,
            StatusCode::OK,
            &ingredient,
            form,
            Errors::default(),
            flash.saved(),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

async fn add_price(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Costing)?;
        let ingredient = load_ingredient(&state, &id).await?;
        let form = PriceForm {
            per_kg: fields.text("per_kg").to_owned(),
            valid_from: fields.text("valid_from").to_owned(),
        };
        let mut errors = Errors::default();
        let per_kg = parse_money(&form.per_kg).filter(|c| *c <= prices::MAX_PER_KG_CENTS);
        if per_kg.is_none() {
            errors.add("per_kg", t(&state, "costing-error-per-kg"));
        }
        let valid_from = parse_date(&form.valid_from);
        if valid_from.is_none() {
            errors.add("valid_from", t(&state, "costing-error-valid-from"));
        }
        let (Some(per_kg), Some(valid_from)) = (per_kg, valid_from) else {
            return render_ingredient(
                &state,
                &session,
                invalid_status(&headers),
                &ingredient,
                form,
                errors,
                false,
            )
            .await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        prices::add(
            &mut conn,
            state.clock.as_ref(),
            &NewPrice {
                ingredient_id: ingredient.id,
                per_kg: Money::from_cents(per_kg),
                valid_from,
                by: Some(staff.0.id),
            },
        )
        .await
        .map_err(|e| fail(&state, &e))?;
        Ok(saved(&headers, &ingredient_path(ingredient.id)))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[derive(Template)]
#[template(path = "staff/costing_product.html")]
struct ProductPage<'a> {
    layout: Layout<'a>,
    heading: String,
    standard_date: String,
    recipe: String,
    why: String,
    missing: Option<String>,
    lines: Option<Table>,
}

fn product_lines(state: &AppState, cost: &DoughCost) -> Table {
    let pieces = i128::from(STANDARD_PIECES);
    let mut rows: Vec<Vec<Cell>> = cost
        .lines
        .iter()
        .map(|line: &CostLine| {
            vec![
                Cell::text(line.ingredient.name.clone()),
                Cell::number(milligrams(line.grams)),
                per_kg(state, line.per_kg),
                Cell::number(fine_money(line.cost.get(), pieces)),
            ]
        })
        .collect();
    if let Some(total) = cost.total() {
        rows.push(vec![
            Cell::text(t(state, "costing-total")),
            Cell::text(String::new()),
            Cell::text(String::new()),
            Cell::number(money(Money::from_cents(round_div(
                total.get(),
                1000 * pieces,
            )))),
        ]);
    }
    Table {
        id: "kostprijs".to_owned(),
        caption: t(state, "costing-product-caption"),
        head: vec![
            Cell::text(t(state, "costing-col-ingredient")),
            Cell::number(t(state, "costing-col-grams")),
            Cell::number(t(state, "costing-col-per-kg")),
            Cell::number(t(state, "costing-col-cost")),
        ],
        rows,
    }
}

async fn product_page(
    State(state): State<AppState>,
    staff: Staff,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Costing)?;
        let id = ProductId::new(path_id(&state, &id)?);
        let today = prices::today(state.clock.as_ref());
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let product = products::get(&mut conn, id).await.map_err(|e| match e {
            catalog::Error::NotFound => Rejection::from(not_found(&state)),
            other => fail(&state, &other).into(),
        })?;
        let cost = prices::standard_costs(&mut conn, &[id], today)
            .await
            .map_err(|e| fail(&state, &e))?
            .pop()
            .ok_or_else(|| Rejection::from(not_found(&state)))?;
        drop(conn);
        let (why, missing, lines) = match &cost.cost {
            Err(why) => (t(&state, no_cost_message(*why)), None, None),
            Ok(piece) => (
                String::new(),
                missing_text(&state, &piece.dough),
                Some(product_lines(&state, &piece.dough)),
            ),
        };
        Ok::<_, Rejection>(pages::render(
            &state,
            StatusCode::OK,
            &ProductPage {
                layout: Layout::new(&state),
                heading: t_with(
                    &state,
                    "costing-product-heading",
                    &[("name", product.name_nl.clone())],
                ),
                standard_date: t_with(
                    &state,
                    "costing-standard-date",
                    &[("date", i18n::numeric_date(today))],
                ),
                recipe: recipe_text(&state, &cost),
                why,
                missing,
                lines,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[derive(Template)]
#[template(path = "staff/costing_batch.html")]
struct BatchPage<'a> {
    layout: Layout<'a>,
    heading: String,
    facts: String,
    standard_date: String,
    unweighable: bool,
    missing: Option<String>,
    lines: Option<Table>,
    pieces: Table,
    /// Before the actuals: the text saying so; after: the variance or why there is none.
    variance_note: String,
    variance: Option<Table>,
}

fn batch_lines(state: &AppState, cost: &DoughCost, actual: Option<Money>) -> Table {
    let mut rows: Vec<Vec<Cell>> = cost
        .lines
        .iter()
        .map(|line| {
            vec![
                Cell::text(line.ingredient.name.clone()),
                Cell::number(grams(line.grams)),
                per_kg(state, line.per_kg),
                Cell::number(fine_money(line.cost.get(), 1)),
            ]
        })
        .collect();
    if let Some(actual) = actual {
        rows.push(vec![
            Cell::text(t(state, "costing-batch-total")),
            Cell::text(String::new()),
            Cell::text(String::new()),
            Cell::number(money(actual)),
        ]);
    }
    Table {
        id: "batchkosten".to_owned(),
        caption: t(state, "costing-batch-caption"),
        head: vec![
            Cell::text(t(state, "costing-col-ingredient")),
            Cell::number(t(state, "costing-col-batch-grams")),
            Cell::number(t(state, "costing-col-per-kg")),
            Cell::number(t(state, "costing-col-cost")),
        ],
        rows,
    }
}

fn variance_table(state: &AppState, v: &Variance) -> Table {
    let rows = [
        ("costing-variance-standard", v.standard),
        ("costing-variance-recipe", v.recipe),
        ("costing-variance-price", v.price),
        ("costing-variance-yield", v.yield_loss),
        ("costing-variance-actual", v.actual),
        ("costing-variance-total", v.total()),
    ]
    .into_iter()
    .map(|(id, amount)| vec![Cell::text(t(state, id)), Cell::number(money(amount))])
    .collect();
    Table {
        id: "afwijking".to_owned(),
        caption: t(state, "costing-variance-caption"),
        head: vec![
            Cell::text(t(state, "costing-col-part")),
            Cell::number(t(state, "costing-col-amount")),
        ],
        rows,
    }
}

fn pieces_table(state: &AppState, cost: &BatchCost) -> Table {
    Table {
        id: "stuks".to_owned(),
        caption: t(state, "costing-batch-per-piece-caption"),
        head: vec![
            Cell::text(t(state, "costing-col-product")),
            Cell::number(t(state, "costing-col-planned")),
            Cell::number(t(state, "costing-col-good")),
            Cell::number(t(state, "costing-col-per-piece")),
        ],
        rows: cost
            .batch
            .outputs
            .iter()
            .map(|o| {
                let good = o
                    .actual_pieces
                    .zip(o.waste_pieces)
                    .map(|(pieces, waste)| pieces.saturating_sub(waste));
                let per_piece = cost
                    .per_piece
                    .iter()
                    .find(|(id, _)| *id == o.product_id)
                    .and_then(|(_, c)| *c);
                vec![
                    Cell::text(o.name.clone()),
                    Cell::number(o.planned_pieces.to_string()),
                    good.map_or_else(|| Cell::muted("–"), |g| Cell::number(g.to_string())),
                    per_piece.map_or_else(|| Cell::muted("–"), |c| Cell::number(money(c))),
                ]
            })
            .collect(),
    }
}

async fn batch_page(
    State(state): State<AppState>,
    staff: Staff,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Costing)?;
        let id = BatchId::new(path_id(&state, &id)?);
        let today = prices::today(state.clock.as_ref());
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let cost = costing::batch_cost(&mut conn, id, today)
            .await
            .map_err(|e| match e {
                production::Error::NotFound => Rejection::from(not_found(&state)),
                other => fail(&state, &other).into(),
            })?;
        drop(conn);
        let (variance_note, variance) = match &cost.variance {
            None => (t(&state, "costing-not-baked"), None),
            Some(Ok(v)) => (String::new(), Some(variance_table(&state, v))),
            Some(Err(CostError::MissingPrices(list))) => {
                let names: Vec<&str> = list.iter().map(|i| i.name.as_str()).collect();
                (
                    t_with(
                        &state,
                        "costing-missing",
                        &[("ingredients", names.join(", "))],
                    ),
                    None,
                )
            }
            Some(Err(CostError::NoDough)) => (t(&state, "costing-variance-no-dough"), None),
            Some(Err(CostError::Scale(_))) => (t(&state, "costing-batch-unweighable"), None),
        };
        let b = &cost.batch;
        Ok::<_, Rejection>(pages::render(
            &state,
            StatusCode::OK,
            &BatchPage {
                layout: Layout::new(&state),
                heading: t_with(
                    &state,
                    "costing-batch-heading",
                    &[("code", b.code.to_string())],
                ),
                facts: t_with(
                    &state,
                    "costing-batch-facts",
                    &[
                        ("recipe", b.recipe_name.clone()),
                        ("n", b.version.to_string()),
                        ("date", i18n::numeric_date(cost.bake_date)),
                    ],
                ),
                standard_date: t_with(
                    &state,
                    "costing-standard-date",
                    &[("date", i18n::numeric_date(cost.standard_date))],
                ),
                unweighable: cost.dough.is_err(),
                missing: cost
                    .dough
                    .as_ref()
                    .ok()
                    .and_then(|d| missing_text(&state, d)),
                lines: cost
                    .dough
                    .as_ref()
                    .ok()
                    .map(|d| batch_lines(&state, d, cost.actual())),
                pieces: pieces_table(&state, &cost),
                variance_note,
                variance,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_amounts_keep_their_detail() {
        assert_eq!(milligrams(457_627), "457,6");
        assert_eq!(fine_money(43_474, 1), "€\u{a0}0,4347");
        assert_eq!(fine_money(1_072_900, 1), "€\u{a0}10,7290");
        assert_eq!(grams(10_453), "10.453");
    }
}
