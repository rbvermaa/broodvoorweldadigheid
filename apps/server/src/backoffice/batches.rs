//! Batches per bake day (F14a, ADR 0042): batches started by hand, adjusting a batch with grams that recalculate on the server, the lock at mixing and
//! the pieces after baking.

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use bakedays::BakeDay;
use domain::{BatchId, IngredientId, ProductId};
use orders::audit::Actor;
use production::batches::{self, Actual, Adjustment, Batch, LineRef, Status};
use recipes::{Part, Pct, ScaleError, Scaled, ingredient_totals, ingredients};
use tower_sessions::Session;

use super::bake_days::{self, day_path};
use super::form::{Errors, Fields, parse_count, parse_pct, pct_value};
use super::recipes::{Message, version_path};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, is_htmx, layout, not_found, path_id,
    require, saved, t, t_with,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/staff/bake-days/{id}/batches", get(page))
        .route("/staff/bake-days/{id}/batches/new", post(start))
        .route(
            "/staff/bake-days/{id}/batches/{batch}",
            get(batch_page).post(save),
        )
        .route(
            "/staff/bake-days/{id}/batches/{batch}/preview",
            post(preview),
        )
        .route(
            "/staff/bake-days/{id}/batches/{batch}/lines",
            post(add_line),
        )
        .route("/staff/bake-days/{id}/batches/{batch}/mixed", post(mixed))
        .route(
            "/staff/bake-days/{id}/batches/{batch}/actuals",
            post(actuals),
        )
        .route(
            "/staff/bake-days/{id}/batches/{batch}/promote",
            post(promote),
        )
}

fn list_path(day: &BakeDay) -> String {
    format!("{}/batches", day_path(day.id))
}

fn batch_path(day: &BakeDay, id: BatchId) -> String {
    format!("{}/batches/{}", day_path(day.id), id.get())
}

/// Grams as kilograms with up to three decimals: `12,5`.
fn kg_text(grams: u64) -> String {
    let whole = grams / 1000;
    let fraction = format!("{:03}", grams % 1000);
    let fraction = fraction.trim_end_matches('0');
    let whole = i18n::decimal(i64::try_from(whole).unwrap_or(i64::MAX), 0);
    if fraction.is_empty() {
        whole
    } else {
        format!("{whole},{fraction}")
    }
}

fn grams_text(grams: u64) -> String {
    i18n::decimal(i64::try_from(grams).unwrap_or(i64::MAX), 0)
}

fn pct_text(pct: Pct) -> String {
    format!("{}%", pct_value(pct))
}

const fn status_message(status: Status) -> &'static str {
    match status {
        Status::Planned => "batch-status-planned",
        Status::Adjusted => "batch-status-adjusted",
        Status::Mixed => "batch-status-mixed",
        Status::Baked => "batch-status-baked",
    }
}

fn actor(staff: &Staff) -> Actor {
    Actor::Staff(staff.0.id)
}

fn production_error(state: &AppState, error: &production::Error) -> Rejection {
    match error {
        production::Error::NotFound | production::Error::BakeDays(bakedays::Error::NotFound) => {
            not_found(state).into()
        }
        other => fail(state, other).into(),
    }
}

pub struct ManualForm {
    pub product_id: String,
    pub pieces: String,
}

#[derive(Template)]
#[template(path = "staff/batches.html")]
struct ListPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    saved: bool,
    editable: bool,
    messages: Vec<Message>,
    batches: Option<Table>,
    products: Vec<SelectOption>,
    manual: ManualForm,
    errors: Errors,
    manual_action: String,
}

enum ListAnswer {
    Plain { saved: bool },
    Manual { form: ManualForm, errors: Errors },
}

async fn render_list(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    day: &BakeDay,
    answer: ListAnswer,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let list = batches::list(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let startable = batches::startable_products(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let unbatched = batches::unbatched(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    drop(conn);
    let path = list_path(day);
    let mut messages = Vec::new();
    let (mut manual, mut errors, mut saved) = (
        ManualForm {
            product_id: String::new(),
            pieces: String::new(),
        },
        Errors::default(),
        false,
    );
    match answer {
        ListAnswer::Plain { saved: s } => saved = s,
        ListAnswer::Manual { form, errors: e } => {
            manual = form;
            errors = e;
        }
    }
    messages.extend(unbatched.into_iter().map(|(_, name, n)| Message {
        kind: "warning",
        text: t_with(
            state,
            "batches-unbatched",
            &[("product", name), ("n", n.to_string())],
        ),
    }));
    let batches = (!list.is_empty()).then(|| Table {
        id: "batches".to_owned(),
        caption: t(state, "batches-caption"),
        head: vec![
            Cell::text(t(state, "batch-code")),
            Cell::text(t(state, "batch-recipe")),
            Cell::number(t(state, "batch-pieces")),
            Cell::number(t(state, "batch-dough")),
            Cell::text(t(state, "batch-status")),
        ],
        rows: list
            .iter()
            .map(|batch| {
                let pieces: u32 = batch.outputs.iter().map(|o| o.planned_pieces).sum();
                vec![
                    Cell::link(batch.code.to_string(), batch_path(day, batch.id)),
                    Cell::text(t_with(
                        state,
                        "batch-recipe-version",
                        &[
                            ("name", batch.recipe_name.clone()),
                            ("n", batch.version.to_string()),
                        ],
                    )),
                    Cell::number(pieces.to_string()),
                    Cell::number(t_with(
                        state,
                        "batch-kg",
                        &[("kg", kg_text(batch.dough_g()))],
                    )),
                    Cell::text(t(state, status_message(batch.status))),
                ]
            })
            .collect(),
    });
    let products = startable
        .into_iter()
        .map(|(id, name)| SelectOption {
            value: id.get().to_string(),
            label: name,
            selected: id.get().to_string() == manual.product_id,
        })
        .collect();
    Ok(pages::render(
        state,
        status,
        &ListPage {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "batches-heading",
                &[("date", state.i18n.long_date(day.date))],
            ),
            back: day_path(day.id),
            saved,
            editable: !day.status.is_final(),
            messages,
            batches,
            products,
            manual,
            errors,
            manual_action: format!("{path}/new"),
        },
    ))
}

pub async fn page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path(id): Path<String>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let day = bake_days::load(&state, &id).await?;
        let answer = ListAnswer::Plain {
            saved: flash.saved(),
        };
        render_list(&state, &session, StatusCode::OK, &day, answer).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn start(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let day = bake_days::load(&state, &id).await?;
        let form = ManualForm {
            product_id: fields.text("product_id").to_owned(),
            pieces: fields.text("pieces").to_owned(),
        };
        let mut errors = Errors::default();
        let product = form.product_id.parse::<i64>().ok().map(ProductId::new);
        if product.is_none() {
            errors.add("product_id", t(&state, "batches-manual-error-product"));
        }
        let pieces = parse_count(&form.pieces).filter(|n| (1..=batches::MAX_PIECES).contains(n));
        if pieces.is_none() {
            errors.add("pieces", t(&state, "batches-manual-error-pieces"));
        }
        let refuse = |form, errors| ListAnswer::Manual { form, errors };
        let (Some(product), Some(pieces)) = (product, pieces) else {
            let status = invalid_status(&headers);
            return render_list(&state, &session, status, &day, refuse(form, errors)).await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = batches::start_manual(
            &mut conn,
            day.id,
            product,
            pieces,
            actor(&staff),
            state.clock.as_ref(),
        )
        .await;
        drop(conn);
        match result {
            Ok(batch) => Ok(saved(&headers, &batch_path(&day, batch))),
            Err(
                error @ (production::Error::NotFound
                | production::Error::Invalid(_)
                | production::Error::DayFinal),
            ) => {
                let (field, text) = match error {
                    production::Error::DayFinal => ("product_id", "day-final"),
                    production::Error::Invalid("pieces") => {
                        ("pieces", "batches-manual-error-pieces")
                    }
                    _ => ("product_id", "batches-manual-error-recipe"),
                };
                errors.add(field, t(&state, text));
                let status = invalid_status(&headers);
                render_list(&state, &session, status, &day, refuse(form, errors)).await
            }
            Err(error) => Err(production_error(&state, &error)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub struct LineField {
    pub name: String,
    pub label: String,
    pub value: String,
    pub error: String,
}

pub struct ActualField {
    pub pieces_name: String,
    pub pieces_label: String,
    pub pieces: String,
    pub waste_name: String,
    pub waste_label: String,
    pub waste: String,
}

pub struct AddForm {
    pub ingredient: String,
    pub pct: String,
    pub reason: String,
}

#[derive(Template)]
#[template(path = "staff/batch.html")]
struct BatchPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    saved: bool,
    locked: bool,
    editable: bool,
    facts: Table,
    lines: Option<Table>,
    totals: Option<Table>,
    messages: Vec<Message>,
    action: String,
    preview: String,
    line_fields: Vec<LineField>,
    piece_fields: Vec<LineField>,
    reason: String,
    reason_error: String,
    has_errors: bool,
    ingredients: Vec<SelectOption>,
    add: AddForm,
    add_errors: Errors,
    add_action: String,
    mix_text: String,
    mix_action: String,
    actual_fields: Vec<ActualField>,
    actuals_error: String,
    actuals_action: String,
    promotable: bool,
    promoted: Option<(String, String)>,
    promote_action: String,
    label_links: Vec<(String, String)>,
}

/// What the baker posted, applied to the batch as far as it reads; with the errors of the
/// fields that did not.
struct Posted {
    batch: Batch,
    raw: Vec<(String, String)>,
    errors: Errors,
    reason: String,
}

fn line_field(id: domain::BatchLineId) -> String {
    format!("pct-{}", id.get())
}

fn pieces_field(product: ProductId) -> String {
    format!("pieces-{}", product.get())
}

fn apply(state: &AppState, mut batch: Batch, fields: &Fields) -> Posted {
    let mut raw = Vec::new();
    let mut errors = Errors::default();
    for line in &mut batch.lines {
        let name = line_field(line.id);
        let value = fields.text(&name);
        match parse_pct(value).filter(|p| *p <= recipes::recipes::MAX_LINE_PCT) {
            Some(pct) => line.actual_pct = pct,
            None => errors.add(&name, t(state, "batch-error-pct")),
        }
        raw.push((name, value.to_owned()));
    }
    for output in &mut batch.outputs {
        let name = pieces_field(output.product_id);
        let value = fields.text(&name);
        match parse_count(value).filter(|n| *n <= batches::MAX_PIECES) {
            Some(n) => output.planned_pieces = n,
            None => errors.add(&name, t(state, "batch-error-pieces")),
        }
        raw.push((name, value.to_owned()));
    }
    let reason = fields.text("reason").to_owned();
    if reason.chars().count() > batches::MAX_REASON_CHARS {
        errors.add("reason", t(state, "batch-error-reason"));
    }
    Posted {
        batch,
        raw,
        errors,
        reason,
    }
}

fn line_name(state: &AppState, line: &Part) -> String {
    match line {
        Part::Ingredient(ingredient) => ingredient.name.clone(),
        Part::Sub(sub) => t_with(
            state,
            "batch-sub",
            &[("name", sub.name.clone()), ("n", sub.version.to_string())],
        ),
    }
}

fn ref_name(state: &AppState, part: &LineRef) -> String {
    match part {
        LineRef::Ingredient(ingredient) => ingredient.name.clone(),
        LineRef::Sub { name, version, .. } => t_with(
            state,
            "batch-sub",
            &[("name", name.clone()), ("n", version.to_string())],
        ),
    }
}

fn sub_rows(state: &AppState, scaled: &[Scaled<'_>], depth: usize, rows: &mut Vec<Vec<Cell>>) {
    for line in scaled {
        rows.push(vec![
            Cell::text(format!(
                "{}{}",
                "↳ ".repeat(depth),
                line_name(state, &line.line.part)
            )),
            Cell::text(t(state, "batch-line-none")),
            Cell::number(pct_text(line.line.pct)),
            Cell::number(grams_text(line.grams)),
        ]);
        sub_rows(state, &line.lines, depth + 1, rows);
    }
}

/// The grams table, the totals per ingredient and any message, for the batch as given.
async fn grams(
    state: &AppState,
    batch: &Batch,
) -> Step<(Option<Table>, Option<Table>, Vec<Message>)> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let formula = batches::formula(&mut conn, batch)
        .await
        .map_err(|e| fail(state, &e))?;
    drop(conn);
    let dough = batch.dough_g();
    let scaled = match formula.scale(dough) {
        Ok(scaled) => scaled,
        Err(ScaleError::Empty(_)) => {
            let message = Message {
                kind: "info",
                text: t(state, "batch-no-lines"),
            };
            return Ok((None, None, vec![message]));
        }
    };
    let mut rows = Vec::new();
    for (line, scaled) in batch.lines.iter().zip(&scaled) {
        let name = ref_name(state, &line.part);
        let name = match line.base_pct {
            None => t_with(state, "batch-line-added", &[("name", name)]),
            Some(_) if line.is_changed() => t_with(state, "batch-line-changed", &[("name", name)]),
            Some(_) => name,
        };
        rows.push(vec![
            Cell::text(name),
            Cell::number(
                line.base_pct
                    .map_or_else(|| t(state, "batch-line-none"), pct_text),
            ),
            Cell::number(pct_text(line.actual_pct)),
            Cell::number(grams_text(scaled.grams)),
        ]);
        sub_rows(state, &scaled.lines, 1, &mut rows);
    }
    let totals = ingredient_totals(&scaled)
        .into_iter()
        .map(|(ingredient, grams)| {
            vec![
                Cell::text(ingredient.name.clone()),
                Cell::number(grams_text(grams)),
            ]
        })
        .collect();
    Ok((
        Some(Table {
            id: "regels".to_owned(),
            caption: t_with(
                state,
                "batch-lines-caption",
                &[("grams", grams_text(dough))],
            ),
            head: vec![
                Cell::text(t(state, "batch-line")),
                Cell::number(t(state, "batch-line-base")),
                Cell::number(t(state, "batch-line-actual")),
                Cell::number(t(state, "batch-line-grams")),
            ],
            rows,
        }),
        Some(Table {
            id: "totalen".to_owned(),
            caption: t(state, "batch-totals-caption"),
            head: vec![
                Cell::text(t(state, "batch-line")),
                Cell::number(t(state, "batch-line-grams")),
            ],
            rows: totals,
        }),
        Vec::new(),
    ))
}

enum BatchAnswer {
    Plain {
        saved: bool,
    },
    Adjust(Posted),
    Add {
        form: AddForm,
        errors: Errors,
    },
    Actuals {
        raw: Vec<(String, String)>,
        error: String,
    },
    /// A change the batch refused as it is; the text says why.
    Refused(String),
}

fn posted_or(raw: &[(String, String)], name: &str, fallback: String) -> String {
    raw.iter()
        .find(|(n, _)| n == name)
        .map_or(fallback, |(_, v)| v.clone())
}

/// The percentage and pieces fields, showing what was posted where there is a post.
fn adjust_fields(
    state: &AppState,
    batch: &Batch,
    raw: &[(String, String)],
    errors: Option<&Errors>,
) -> (Vec<LineField>, Vec<LineField>) {
    let error = |name: &str| errors.map(|e| e.get(name).to_owned()).unwrap_or_default();
    let lines = batch
        .lines
        .iter()
        .map(|line| {
            let name = line_field(line.id);
            LineField {
                label: t_with(
                    state,
                    "batch-line-pct",
                    &[("name", ref_name(state, &line.part))],
                ),
                value: posted_or(raw, &name, pct_value(line.actual_pct)),
                error: error(&name),
                name,
            }
        })
        .collect();
    let pieces = batch
        .outputs
        .iter()
        .map(|output| {
            let name = pieces_field(output.product_id);
            LineField {
                label: t_with(
                    state,
                    "batch-output-pieces",
                    &[("product", output.name.clone())],
                ),
                value: posted_or(raw, &name, output.planned_pieces.to_string()),
                error: error(&name),
                name,
            }
        })
        .collect();
    (lines, pieces)
}

/// Pieces and waste per product; before anything is recorded the planned pieces and no
/// waste.
fn actual_fields(state: &AppState, batch: &Batch, raw: &[(String, String)]) -> Vec<ActualField> {
    batch
        .outputs
        .iter()
        .map(|output| {
            let id = output.product_id.get();
            let (pieces_name, waste_name) = (format!("actual-{id}"), format!("waste-{id}"));
            let pieces = output.actual_pieces.unwrap_or(output.planned_pieces);
            let waste = output.waste_pieces.unwrap_or(0);
            ActualField {
                pieces_label: t_with(
                    state,
                    "batch-actual-pieces",
                    &[("product", output.name.clone())],
                ),
                pieces: posted_or(raw, &pieces_name, pieces.to_string()),
                waste_label: t_with(
                    state,
                    "batch-actual-waste",
                    &[("product", output.name.clone())],
                ),
                waste: posted_or(raw, &waste_name, waste.to_string()),
                pieces_name,
                waste_name,
            }
        })
        .collect()
}

fn facts(state: &AppState, batch: &Batch) -> Table {
    let row = |label: &str, value: String| vec![Cell::text(t(state, label)), Cell::text(value)];
    Table {
        id: "batch".to_owned(),
        caption: t(state, "batch-facts"),
        head: vec![
            Cell::text(t(state, "batch-status")),
            Cell::text(t(state, status_message(batch.status))),
        ],
        rows: vec![
            row(
                "batch-recipe",
                t_with(
                    state,
                    "batch-recipe-version",
                    &[
                        ("name", batch.recipe_name.clone()),
                        ("n", batch.version.to_string()),
                    ],
                ),
            ),
            row(
                "batch-dough",
                t_with(state, "batch-kg", &[("kg", kg_text(batch.dough_g()))]),
            ),
        ],
    }
}

fn empty_add() -> AddForm {
    AddForm {
        ingredient: String::new(),
        pct: String::new(),
        reason: String::new(),
    }
}

async fn render_batch(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    day: &BakeDay,
    stored: Batch,
    answer: BatchAnswer,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let all_ingredients = ingredients::list(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    let promoted = batches::promoted_version(&mut conn, stored.id)
        .await
        .map_err(|e| fail(state, &e))?
        .map(|v| {
            let text = t_with(state, "batch-promoted", &[("n", v.version.to_string())]);
            (text, version_path(v.recipe_id, v.id))
        });
    drop(conn);
    let path = batch_path(day, stored.id);
    let mut saved = false;
    let mut add = (empty_add(), Errors::default());
    let mut actuals = (Vec::new(), String::new());
    let mut adjusting = None;
    let mut refused = None;
    match answer {
        BatchAnswer::Plain { saved: s } => saved = s,
        BatchAnswer::Adjust(posted) => adjusting = Some(posted),
        BatchAnswer::Add { form, errors } => add = (form, errors),
        BatchAnswer::Actuals { raw, error } => actuals = (raw, error),
        BatchAnswer::Refused(text) => refused = Some(text),
    }
    let batch = adjusting.as_ref().map_or(&stored, |p| &p.batch);
    let (lines, totals, mut messages) = grams(state, batch).await?;
    let locked = stored.status.is_locked();
    if locked {
        let text = t(state, "batch-locked");
        messages.insert(0, Message { kind: "info", text });
    }
    if let Some(text) = refused {
        messages.insert(
            0,
            Message {
                kind: "error",
                text,
            },
        );
    }
    let (line_fields, piece_fields) = adjust_fields(
        state,
        batch,
        adjusting.as_ref().map_or(&[], |p| p.raw.as_slice()),
        adjusting.as_ref().map(|p| &p.errors),
    );
    let (add, add_errors) = add;
    let ingredients = all_ingredients
        .iter()
        .map(|i| SelectOption {
            value: i.id.get().to_string(),
            label: i.name.clone(),
            selected: i.id.get().to_string() == add.ingredient,
        })
        .collect();
    let code = stored.code.to_string();
    Ok(pages::render(
        state,
        status,
        &BatchPage {
            layout: layout(state, session).await?,
            heading: t_with(state, "batch-heading", &[("code", code.clone())]),
            back: list_path(day),
            saved,
            locked,
            editable: !locked && !day.status.is_final(),
            facts: facts(state, &stored),
            lines,
            totals,
            messages,
            action: path.clone(),
            preview: format!("{path}/preview"),
            line_fields,
            piece_fields,
            reason_error: adjusting
                .as_ref()
                .map(|p| p.errors.get("reason").to_owned())
                .unwrap_or_default(),
            has_errors: adjusting.as_ref().is_some_and(|p| !p.errors.is_empty()),
            reason: adjusting.map(|p| p.reason).unwrap_or_default(),
            ingredients,
            add,
            add_errors,
            add_action: format!("{path}/lines"),
            mix_text: t_with(state, "batch-mix-text", &[("code", code)]),
            mix_action: format!("{path}/mixed"),
            actual_fields: actual_fields(state, &stored, &actuals.0),
            actuals_error: actuals.1,
            actuals_action: format!("{path}/actuals"),
            promotable: promoted.is_none()
                && stored.lines.iter().any(|l| {
                    l.is_changed() && (l.base_pct.is_some() || l.actual_pct.thousandths() > 0)
                }),
            promoted,
            promote_action: format!("{path}/promote"),
            label_links: stored
                .outputs
                .iter()
                .map(|o| {
                    (
                        super::batch_labels::path(&path, o.product_id),
                        t_with(state, "label-check-link", &[("product", o.name.clone())]),
                    )
                })
                .collect(),
        },
    ))
}

async fn load_batch(state: &AppState, raw_day: &str, raw_batch: &str) -> Step<(BakeDay, Batch)> {
    let day = bake_days::load(state, raw_day).await?;
    let id = BatchId::new(path_id(state, raw_batch)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let batch = batches::get(&mut conn, id)
        .await
        .map_err(|e| production_error(state, &e))?;
    if batch.bake_day_id != day.id {
        return Err(not_found(state).into());
    }
    Ok((day, batch))
}

pub async fn batch_page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((id, batch)): Path<(String, String)>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load_batch(&state, &id, &batch).await?;
        let answer = BatchAnswer::Plain {
            saved: flash.saved(),
        };
        render_batch(&state, &session, StatusCode::OK, &day, batch, answer).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// The grams for what is in the form, without saving: htmx swaps in the grams table.
pub async fn preview(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((id, batch)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load_batch(&state, &id, &batch).await?;
        let posted = apply(&state, batch.clone(), &fields);
        render_batch(
            &state,
            &session,
            StatusCode::OK,
            &day,
            batch,
            BatchAnswer::Adjust(posted),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// A refused change to a mixed batch, or on a finished day: the page with its lock notice.
async fn locked_answer(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    day: &BakeDay,
    raw_batch: BatchId,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let batch = batches::get(&mut conn, raw_batch)
        .await
        .map_err(|e| production_error(state, &e))?;
    drop(conn);
    let status = if headers.contains_key("hx-request") {
        StatusCode::OK
    } else {
        StatusCode::CONFLICT
    };
    render_batch(
        state,
        session,
        status,
        day,
        batch,
        BatchAnswer::Plain { saved: false },
    )
    .await
}

pub async fn save(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, batch)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load_batch(&state, &id, &batch).await?;
        if batch.status.is_locked() || day.status.is_final() {
            return locked_answer(&state, &session, &headers, &day, batch.id).await;
        }
        let posted = apply(&state, batch.clone(), &fields);
        if !posted.errors.is_empty() {
            let status = invalid_status(&headers);
            return render_batch(
                &state,
                &session,
                status,
                &day,
                batch,
                BatchAnswer::Adjust(posted),
            )
            .await;
        }
        let adjustment = Adjustment {
            lines: posted
                .batch
                .lines
                .iter()
                .map(|l| (l.id, l.actual_pct))
                .collect(),
            pieces: posted
                .batch
                .outputs
                .iter()
                .map(|o| (o.product_id, o.planned_pieces))
                .collect(),
            reason: Some(posted.reason.clone()),
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = batches::adjust(
            &mut conn,
            batch.id,
            &adjustment,
            actor(&staff),
            state.clock.as_ref(),
        )
        .await;
        drop(conn);
        match result {
            Ok(()) => Ok(saved(&headers, &batch_path(&day, batch.id))),
            Err(production::Error::Invalid("empty")) => {
                let mut posted = posted;
                if let Some(first) = posted.batch.lines.first() {
                    posted
                        .errors
                        .add(&line_field(first.id), t(&state, "batch-error-empty"));
                }
                let status = invalid_status(&headers);
                render_batch(
                    &state,
                    &session,
                    status,
                    &day,
                    batch,
                    BatchAnswer::Adjust(posted),
                )
                .await
            }
            Err(production::Error::Locked) => {
                locked_answer(&state, &session, &headers, &day, batch.id).await
            }
            Err(error) => Err(production_error(&state, &error)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn add_line(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, batch)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load_batch(&state, &id, &batch).await?;
        if batch.status.is_locked() || day.status.is_final() {
            return locked_answer(&state, &session, &headers, &day, batch.id).await;
        }
        let form = AddForm {
            ingredient: fields.text("ingredient").to_owned(),
            pct: fields.text("add_pct").to_owned(),
            reason: fields.text("add_reason").to_owned(),
        };
        let mut errors = Errors::default();
        let ingredient = form.ingredient.parse::<i64>().ok().map(IngredientId::new);
        if ingredient.is_none() {
            errors.add("ingredient", t(&state, "batch-add-error-ingredient"));
        }
        let pct = parse_pct(&form.pct)
            .filter(|p| p.thousandths() > 0 && *p <= recipes::recipes::MAX_LINE_PCT);
        if pct.is_none() {
            errors.add("add_pct", t(&state, "batch-add-error-pct"));
        }
        if form.reason.chars().count() > batches::MAX_REASON_CHARS {
            errors.add("add_reason", t(&state, "batch-error-reason"));
        }
        let (Some(ingredient), Some(pct), true) = (ingredient, pct, errors.is_empty()) else {
            let status = invalid_status(&headers);
            let answer = BatchAnswer::Add { form, errors };
            return render_batch(&state, &session, status, &day, batch, answer).await;
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = batches::add_line(
            &mut conn,
            batch.id,
            ingredient,
            pct,
            Some(&form.reason),
            actor(&staff),
            state.clock.as_ref(),
        )
        .await;
        drop(conn);
        match result {
            Ok(()) => Ok(saved(&headers, &batch_path(&day, batch.id))),
            Err(production::Error::Locked) => {
                locked_answer(&state, &session, &headers, &day, batch.id).await
            }
            Err(production::Error::Invalid("lines")) => {
                errors.add("ingredient", t(&state, "batch-add-error-full"));
                let status = invalid_status(&headers);
                let answer = BatchAnswer::Add { form, errors };
                render_batch(&state, &session, status, &day, batch, answer).await
            }
            Err(production::Error::Recipes(recipes::Error::NotFound)) => {
                errors.add("ingredient", t(&state, "batch-add-error-ingredient"));
                let status = invalid_status(&headers);
                let answer = BatchAnswer::Add { form, errors };
                render_batch(&state, &session, status, &day, batch, answer).await
            }
            Err(error) => Err(production_error(&state, &error)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn mixed(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, batch)): Path<(String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load_batch(&state, &id, &batch).await?;
        if batch.status.is_locked() || day.status.is_final() {
            return locked_answer(&state, &session, &headers, &day, batch.id).await;
        }
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result =
            batches::mark_mixed(&mut conn, batch.id, actor(&staff), state.clock.as_ref()).await;
        drop(conn);
        match result {
            Ok(()) => Ok(saved(&headers, &batch_path(&day, batch.id))),
            Err(production::Error::Invalid("empty")) => {
                let answer = BatchAnswer::Refused(t(&state, "batch-error-empty"));
                let status = invalid_status(&headers);
                render_batch(&state, &session, status, &day, batch, answer).await
            }
            Err(production::Error::Locked) => {
                locked_answer(&state, &session, &headers, &day, batch.id).await
            }
            Err(error) => Err(production_error(&state, &error)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn actuals(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, batch)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load_batch(&state, &id, &batch).await?;
        let mut raw = Vec::new();
        let mut values = Vec::new();
        let mut error = String::new();
        for output in &batch.outputs {
            let id = output.product_id.get();
            let (pieces_name, waste_name) = (format!("actual-{id}"), format!("waste-{id}"));
            let pieces = fields.text(&pieces_name).to_owned();
            let waste = fields.text(&waste_name).to_owned();
            match (
                parse_count(&pieces).filter(|n| *n <= batches::MAX_PIECES),
                parse_count(&waste),
            ) {
                (Some(p), Some(w)) if w <= p => values.push(Actual {
                    product_id: output.product_id,
                    pieces: p,
                    waste: w,
                }),
                (Some(_), Some(_)) => error = t(&state, "batch-error-waste"),
                _ => error = t(&state, "batch-error-pieces"),
            }
            raw.push((pieces_name, pieces));
            raw.push((waste_name, waste));
        }
        if !batch.status.is_locked() {
            let answer = BatchAnswer::Refused(t(&state, "batch-error-not-mixed"));
            let status = invalid_status(&headers);
            return render_batch(&state, &session, status, &day, batch, answer).await;
        }
        if !error.is_empty() {
            let status = invalid_status(&headers);
            let answer = BatchAnswer::Actuals { raw, error };
            return render_batch(&state, &session, status, &day, batch, answer).await;
        }
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = batches::record_actuals(
            &mut conn,
            batch.id,
            &values,
            actor(&staff),
            state.clock.as_ref(),
        )
        .await;
        drop(conn);
        match result {
            Ok(()) => Ok(saved(&headers, &batch_path(&day, batch.id))),
            Err(error) => Err(production_error(&state, &error)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Saves the batch's formula as the next version of its recipe (F14b) and opens it.
pub async fn promote(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, batch)): Path<(String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load_batch(&state, &id, &batch).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = batches::promote(
            &mut conn,
            batch.id,
            &state.i18n,
            actor(&staff),
            state.clock.as_ref(),
        )
        .await;
        let (status, text) = match result {
            Ok(version) => {
                return Ok(saved(
                    &headers,
                    &version_path(version.recipe_id, version.id),
                ));
            }
            Err(production::Error::Invalid("unchanged")) => (
                invalid_status(&headers),
                t(&state, "batch-promote-unchanged"),
            ),
            Err(production::Error::Recipes(recipes::Error::DraftExists(draft))) => {
                let draft = recipes::recipes::version(&mut conn, batch.recipe_id, draft)
                    .await
                    .map_err(|e| fail(&state, &e))?;
                let status = if is_htmx(&headers) {
                    StatusCode::OK
                } else {
                    StatusCode::CONFLICT
                };
                let n = draft.version.to_string();
                (
                    status,
                    t_with(&state, "batch-promote-draft-exists", &[("n", n)]),
                )
            }
            Err(error) => return Err(production_error(&state, &error)),
        };
        drop(conn);
        render_batch(
            &state,
            &session,
            status,
            &day,
            batch,
            BatchAnswer::Refused(text),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kilograms_show_the_dutch_way() {
        assert_eq!(kg_text(12_500), "12,5");
        assert_eq!(kg_text(9_000), "9");
        assert_eq!(kg_text(1_250_125), "1.250,125");
    }

    #[test]
    fn every_status_has_a_text() {
        let translator = i18n::Translator::dutch().expect("Dutch texts");
        for status in [
            Status::Planned,
            Status::Adjusted,
            Status::Mixed,
            Status::Baked,
        ] {
            translator.t(status_message(status), None).expect("text");
        }
    }
}
