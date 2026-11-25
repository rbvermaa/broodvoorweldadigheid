//! A batch's allergens and ingredients against the product's approval, and the batch's
//! own approval for its labels (F17, ADR 0046).

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use domain::{BatchId, ProductId};
use production::label_check::{self, LabelBlock, LabelCheck, LabelSource};
use tower_sessions::Session;

use super::form::Fields;
use super::products::{ComputedView, codes, reviewed_computed};
use super::{
    Flash, Outcome, Rejection, Step, fail, layout, not_found, path_id, require, saved, t, t_with,
};
use crate::components::{ListedIngredient, listed};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/staff/bake-days/{id}/batches/{batch}/etiket/{product}",
            get(page),
        )
        .route(
            "/staff/bake-days/{id}/batches/{batch}/etiket/{product}/approve",
            post(approve),
        )
}

/// The label check of `product` under the batch page at `batch_path`.
pub fn path(batch_path: &str, product: ProductId) -> String {
    format!("{batch_path}/etiket/{}", product.get())
}

pub struct Notice {
    pub kind: &'static str,
    pub text: String,
}

#[derive(Template)]
#[template(path = "staff/batch_label.html")]
struct Page<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    saved: bool,
    changed: bool,
    notices: Vec<Notice>,
    computed: Option<ComputedView>,
    product_allergens: Vec<&'static str>,
    product_list: Option<Vec<ListedIngredient>>,
    approvable: bool,
}

struct Target {
    batch: BatchId,
    product: ProductId,
    batch_path: String,
}

async fn target(state: &AppState, day: &str, batch: &str, product: &str) -> Step<Target> {
    let day = super::bake_days::load(state, day).await?;
    let batch = BatchId::new(path_id(state, batch)?);
    let product = ProductId::new(path_id(state, product)?);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let stored = production::batches::get(&mut conn, batch)
        .await
        .map_err(|e| production_error(state, &e))?;
    if stored.bake_day_id != day.id || !stored.outputs.iter().any(|o| o.product_id == product) {
        return Err(not_found(state).into());
    }
    Ok(Target {
        batch,
        product,
        batch_path: format!("/staff/bake-days/{}/batches/{}", day.id.get(), batch.get()),
    })
}

fn production_error(state: &AppState, error: &production::Error) -> Rejection {
    match error {
        production::Error::NotFound | production::Error::BakeDays(bakedays::Error::NotFound) => {
            not_found(state).into()
        }
        other => fail(state, other).into(),
    }
}

fn notices(state: &AppState, check: &LabelCheck) -> (Vec<Notice>, bool) {
    let notice = |kind, id| Notice {
        kind,
        text: t(state, id),
    };
    match check.outcome() {
        Ok(content) => {
            let mut list = vec![notice(
                "success",
                match content.source {
                    LabelSource::Product => "label-check-ok-product",
                    LabelSource::Batch => "label-check-ok-batch",
                },
            )];
            if content.order_changed {
                list.push(notice("warning", "label-check-order-warning"));
            }
            (list, false)
        }
        Err(LabelBlock::KitchenNotApproved) => {
            (vec![notice("error", "label-check-kitchen")], false)
        }
        Err(LabelBlock::ProductNotApproved) => {
            (vec![notice("error", "label-check-product")], false)
        }
        Err(LabelBlock::Unweighable) => (vec![notice("error", "label-check-unweighable")], false),
        Err(LabelBlock::Differs {
            approved,
            difference,
            ..
        }) => {
            let mut list = vec![notice("error", "label-check-differs")];
            if approved.is_none() {
                list.push(notice("info", "label-check-hand"));
            }
            if difference.allergens {
                list.push(notice("warning", "label-check-differs-allergens"));
            }
            if difference.order {
                list.push(notice("warning", "label-check-differs-order"));
            } else if difference.ingredients && approved.is_some() {
                list.push(notice("warning", "label-check-differs-ingredients"));
            }
            (list, true)
        }
    }
}

async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    target: &Target,
    saved: bool,
    changed: bool,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let check = label_check::check(&mut conn, target.batch, target.product)
        .await
        .map_err(|e| production_error(state, &e))?;
    drop(conn);
    let (notices, approvable) = notices(state, &check);
    let action = format!("{}/approve", path(&target.batch_path, target.product));
    let computed = check
        .computed
        .as_ref()
        .map(|computed| ComputedView::new(t(state, "label-check-intro"), computed, false, action));
    Ok(pages::render(
        state,
        status,
        &Page {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "label-check-title",
                &[("product", check.product.name_nl.clone())],
            ),
            back: target.batch_path.clone(),
            saved,
            changed,
            notices,
            computed,
            product_allergens: codes(&check.product.allergen_info.allergens),
            product_list: check
                .product_values
                .as_ref()
                .map(|v| listed(&v.ingredients)),
            approvable,
        },
    ))
}

pub async fn page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((day, batch, product)): Path<(String, String, String)>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let target = target(&state, &day, &batch, &product).await?;
        render(
            &state,
            &session,
            StatusCode::OK,
            &target,
            flash.saved(),
            false,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Approves the batch's values exactly as the page showed them; refused with the new
/// values when the batch changed in the meantime.
pub async fn approve(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((day, batch, product)): Path<(String, String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let target = target(&state, &day, &batch, &product).await?;
        let result = match reviewed_computed(&fields) {
            Some(reviewed) => {
                let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
                label_check::approve_batch(
                    &mut conn,
                    target.batch,
                    &reviewed,
                    staff.0.id,
                    state.clock.as_ref(),
                )
                .await
            }
            None => Err(production::Error::AllergensChanged),
        };
        match result {
            Ok(()) => Ok(saved(&headers, &path(&target.batch_path, target.product))),
            Err(production::Error::AllergensChanged) => {
                render(&state, &session, StatusCode::CONFLICT, &target, false, true).await
            }
            Err(error) => Err(production_error(&state, &error)),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
