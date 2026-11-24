//! Extra production per bake day (F7a): mobile first, one small form per line.

use std::collections::BTreeMap;

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bakedays::capacity::Remaining;
use bakedays::{
    BakeDay, ExtraInput, NoOrders, ProductionExtra, ProductionExtraId, Purpose, extras, range,
};
use domain::ProductId;
use tower_sessions::Session;

use super::bake_days::{self, bakedays_error, day_path, problem};
use super::form::{Errors, Fields, parse_count};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, is_htmx, layout, not_found, path_id,
    require, saved, t, t_count, t_with,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

const fn purpose_message(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::Freezer => "extra-purpose-freezer",
        Purpose::SumUp => "extra-purpose-sumup",
        Purpose::Samples => "extra-purpose-samples",
        Purpose::Other => "extra-purpose-other",
    }
}

fn purpose_options(state: &AppState, selected: &str) -> Vec<SelectOption> {
    Purpose::ALL
        .into_iter()
        .map(|p| SelectOption {
            value: p.as_str().to_owned(),
            label: t(state, purpose_message(p)),
            selected: p.as_str() == selected,
        })
        .collect()
}

pub struct ExtraRow {
    pub anchor: String,
    pub action: String,
    pub heading: String,
    pub qty_field: String,
    pub purpose_field: String,
    pub note_field: String,
    pub qty: String,
    pub purposes: Vec<SelectOption>,
    pub note: String,
    pub error: String,
    pub saved: bool,
    pub dialog: String,
    pub remove_action: String,
    pub remove_text: String,
}

pub struct AddForm {
    pub product_id: String,
    pub qty: String,
    pub purpose: String,
    pub note: String,
}

#[derive(Template)]
#[template(path = "staff/extras.html")]
struct ExtrasPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    action: String,
    editable: bool,
    saved: bool,
    rows: Vec<ExtraRow>,
    products: Vec<SelectOption>,
    purposes: Vec<SelectOption>,
    add: AddForm,
    errors: Errors,
    capacity: Table,
}

/// What a post changed, so the page shows the answer in the right place.
enum Answer {
    Plain {
        saved: bool,
    },
    Add {
        form: AddForm,
        errors: Errors,
    },
    Row {
        id: ProductionExtraId,
        posted: Option<(String, String, String)>,
        error: String,
        saved: bool,
    },
}

fn row(
    state: &AppState,
    path: &str,
    date: &str,
    names: &BTreeMap<ProductId, String>,
    extra: &ProductionExtra,
    answer: &Answer,
) -> ExtraRow {
    let id = extra.id.get();
    let name = names.get(&extra.product_id).cloned().unwrap_or_default();
    let (mut qty, mut purpose, mut note) = (
        extra.qty.to_string(),
        extra.purpose.as_str().to_owned(),
        extra.note.clone().unwrap_or_default(),
    );
    let (mut error, mut saved) = (String::new(), false);
    if let Answer::Row {
        id: answered,
        posted,
        error: e,
        saved: s,
    } = answer
        && *answered == extra.id
    {
        if let Some((q, p, n)) = posted {
            (qty, purpose, note) = (q.clone(), p.clone(), n.clone());
        }
        error.clone_from(e);
        saved = *s;
    }
    ExtraRow {
        anchor: format!("extra-{id}"),
        action: format!("{path}/extras/{id}"),
        heading: t_with(
            state,
            "extra-row-heading",
            &[
                ("product", name.clone()),
                ("purpose", t(state, purpose_message(extra.purpose))),
            ],
        ),
        qty_field: format!("qty-{id}"),
        purpose_field: format!("purpose-{id}"),
        note_field: format!("note-{id}"),
        purposes: purpose_options(state, &purpose),
        qty,
        note,
        error,
        saved,
        dialog: format!("remove-extra-{id}"),
        remove_action: format!("{path}/extras/{id}/remove"),
        remove_text: t_with(
            state,
            "extra-remove-text",
            &[
                ("qty", extra.qty.to_string()),
                ("product", name),
                ("purpose", t(state, purpose_message(extra.purpose))),
                ("date", date.to_owned()),
            ],
        ),
    }
}

async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    day: &BakeDay,
    answer: Answer,
) -> Outcome {
    let (names, all) = bake_days::product_names(state).await?;
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let list = extras::list(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let items = range::list_products(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let available: BTreeMap<ProductId, Remaining> =
        range::availability(&mut conn, day.id, &NoOrders)
            .await
            .map_err(|e| fail(state, &e))?
            .into_iter()
            .map(|a| (a.product_id, a.remaining))
            .collect();
    drop(conn);
    let path = day_path(day.id);
    let date = state.i18n.long_date(day.date);
    let mut extra_totals: BTreeMap<ProductId, u32> = BTreeMap::new();
    for extra in &list {
        *extra_totals.entry(extra.product_id).or_default() += extra.qty;
    }
    let capacity = Table {
        id: "capaciteit".to_owned(),
        caption: t(state, "extras-capacity"),
        head: vec![
            Cell::text(t(state, "product-name")),
            Cell::number(t(state, "extras-extra")),
            Cell::number(t(state, "extras-left")),
        ],
        rows: items
            .iter()
            .map(|item| {
                let left = match available.get(&item.product_id) {
                    Some(Remaining::Pieces(n)) => n.to_string(),
                    Some(Remaining::Unlimited) | None => t(state, "range-unlimited"),
                };
                vec![
                    Cell::text(names.get(&item.product_id).cloned().unwrap_or_default()),
                    Cell::number(
                        extra_totals
                            .get(&item.product_id)
                            .copied()
                            .unwrap_or(0)
                            .to_string(),
                    ),
                    Cell::number(left),
                ]
            })
            .collect(),
    };
    let rows = list
        .iter()
        .map(|extra| row(state, &path, &date, &names, extra, &answer))
        .collect();
    let (add, errors, saved) = match answer {
        Answer::Add { form, errors } => (form, errors, false),
        Answer::Plain { saved } => (empty_add(), Errors::default(), saved),
        Answer::Row { .. } => (empty_add(), Errors::default(), false),
    };
    let on_range: Vec<ProductId> = items.iter().map(|i| i.product_id).collect();
    let mut choosable: Vec<_> = all.iter().filter(|p| p.active).collect();
    choosable.sort_by_key(|p| !on_range.contains(&p.id));
    Ok(pages::render(
        state,
        status,
        &ExtrasPage {
            layout: layout(state, session).await?,
            heading: t_with(state, "extras-heading", &[("date", date)]),
            back: path.clone(),
            action: format!("{path}/extras"),
            editable: !day.status.is_final(),
            saved,
            rows,
            products: choosable
                .into_iter()
                .map(|p| SelectOption {
                    value: p.id.get().to_string(),
                    label: p.name_nl.clone(),
                    selected: p.id.get().to_string() == add.product_id,
                })
                .collect(),
            purposes: purpose_options(state, &add.purpose),
            add,
            errors,
            capacity,
        },
    ))
}

fn empty_add() -> AddForm {
    AddForm {
        product_id: String::new(),
        qty: String::new(),
        purpose: Purpose::Freezer.as_str().to_owned(),
        note: String::new(),
    }
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
        render(
            &state,
            &session,
            StatusCode::OK,
            &day,
            Answer::Plain {
                saved: flash.saved(),
            },
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

fn note(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

pub async fn add(
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
        let form = AddForm {
            product_id: fields.text("product_id").to_owned(),
            qty: fields.text("qty").to_owned(),
            purpose: fields.text("purpose").to_owned(),
            note: fields.raw("note").to_owned(),
        };
        let mut errors = Errors::default();
        let product = form.product_id.parse::<i64>().ok();
        if product.is_none() {
            errors.add("product_id", t(&state, "range-error-product"));
        }
        let qty = parse_count(&form.qty).filter(|n| *n > 0);
        if qty.is_none() {
            errors.add("qty", t(&state, "extra-error-qty"));
        }
        let purpose = form.purpose.parse::<Purpose>().ok();
        if purpose.is_none() {
            errors.add("purpose", t(&state, "extra-error-purpose"));
        }
        let (Some(product), Some(qty), Some(purpose)) = (product, qty, purpose) else {
            let status = invalid_status(&headers);
            return render(&state, &session, status, &day, Answer::Add { form, errors }).await;
        };
        let input = ExtraInput {
            qty,
            purpose,
            note: note(&form.note),
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = extras::add(
            &mut conn,
            day.id,
            ProductId::new(product),
            &input,
            &NoOrders,
        )
        .await;
        drop(conn);
        match result {
            Ok(_) => Ok(saved(&headers, &format!("{}/extras", day_path(day.id)))),
            Err(error) => {
                let Some(problem) = problem(&state, &error) else {
                    return Err(bakedays_error(&state, &error).into());
                };
                let field = if problem.field.is_empty() {
                    "qty"
                } else {
                    problem.field
                };
                errors.add(field, problem.text);
                let status = invalid_status(&headers);
                render(&state, &session, status, &day, Answer::Add { form, errors }).await
            }
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

async fn load_extra(state: &AppState, day: &BakeDay, raw_id: &str) -> Step<ProductionExtra> {
    let id = ProductionExtraId::new(path_id(state, raw_id)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    extras::list(&mut conn, day.id)
        .await
        .map_err(|e| fail(state, &e))?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| not_found(state).into())
}

/// Changes one line. htmx swaps in just that line (and the capacity table); without
/// JavaScript the page reloads.
pub async fn update(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, extra)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let day = bake_days::load(&state, &id).await?;
        let extra = load_extra(&state, &day, &extra).await?;
        let n = extra.id.get();
        let posted = (
            fields.text(&format!("qty-{n}")).to_owned(),
            fields.text(&format!("purpose-{n}")).to_owned(),
            fields.raw(&format!("note-{n}")).to_owned(),
        );
        let qty = parse_count(&posted.0).filter(|q| *q > 0);
        let purpose = posted.1.parse::<Purpose>().ok();
        let refuse = |error: String, posted| Answer::Row {
            id: extra.id,
            posted: Some(posted),
            error,
            saved: false,
        };
        let (Some(qty), Some(purpose)) = (qty, purpose) else {
            let text = t(
                &state,
                if qty.is_none() {
                    "extra-error-qty"
                } else {
                    "extra-error-purpose"
                },
            );
            let status = invalid_status(&headers);
            return render(&state, &session, status, &day, refuse(text, posted)).await;
        };
        let input = ExtraInput {
            qty,
            purpose,
            note: note(&posted.2),
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = extras::update(&mut conn, extra.id, &input, &NoOrders).await;
        drop(conn);
        match result {
            Ok(_) if is_htmx(&headers) => {
                let answer = Answer::Row {
                    id: extra.id,
                    posted: None,
                    error: String::new(),
                    saved: true,
                };
                render(&state, &session, StatusCode::OK, &day, answer).await
            }
            Ok(_) => Ok(saved(&headers, &format!("{}/extras", day_path(day.id)))),
            Err(bakedays::Error::CapacityExceeded { remaining, .. }) => {
                let text = t_count(&state, "extra-error-capacity-line", remaining);
                let status = invalid_status(&headers);
                render(&state, &session, status, &day, refuse(text, posted)).await
            }
            Err(error) => {
                let Some(problem) = problem(&state, &error) else {
                    return Err(bakedays_error(&state, &error).into());
                };
                let status = invalid_status(&headers);
                render(&state, &session, status, &day, refuse(problem.text, posted)).await
            }
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn remove(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, extra)): Path<(String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let day = bake_days::load(&state, &id).await?;
        let extra = load_extra(&state, &day, &extra).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = extras::remove(&mut conn, extra.id).await;
        drop(conn);
        match result {
            Ok(()) => Ok(saved(&headers, &format!("{}/extras", day_path(day.id)))),
            Err(error) => {
                let Some(problem) = problem(&state, &error) else {
                    return Err(bakedays_error(&state, &error).into());
                };
                let answer = Answer::Row {
                    id: extra.id,
                    posted: None,
                    error: problem.text,
                    saved: false,
                };
                render(&state, &session, invalid_status(&headers), &day, answer).await
            }
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_purpose_has_a_text() {
        let translator = i18n::Translator::dutch().expect("Dutch texts");
        for purpose in Purpose::ALL {
            translator.t(purpose_message(purpose), None).expect("text");
        }
    }

    #[test]
    fn capacity_errors_say_how_many_fit() {
        let translator = i18n::Translator::dutch().expect("Dutch texts");
        let mut args = i18n::FluentArgs::new();
        args.set("n", 2);
        let text = translator
            .t("extra-error-capacity", Some(&args))
            .expect("text");
        assert!(text.contains('2'), "{text}");
    }
}
