//! The kitchen-wide "may contain traces of" list and its approval (F10, ADR 0038).

use std::collections::BTreeSet;

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use catalog::KitchenMayContain;
use catalog::company;
use domain::Allergen;
use tower_sessions::Session;

use super::form::Fields;
use super::products::{AllergenBox, ApprovalView, approval_text, boxes, codes};
use super::{Flash, Outcome, Rejection, fail, layout, require, saved};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub const PATH: &str = "/staff/products/may-contain";
pub const APPROVE_PATH: &str = "/staff/products/may-contain/approve";

#[derive(Template)]
#[template(path = "staff/kitchen_may_contain.html")]
struct Page<'a> {
    layout: Layout<'a>,
    action: &'static str,
    approve_action: &'static str,
    boxes: Vec<AllergenBox>,
    stored: Vec<&'static str>,
    approval: ApprovalView,
    saved: bool,
    changed: bool,
}

async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    kitchen: &KitchenMayContain,
    saved: bool,
    changed: bool,
) -> Outcome {
    Ok(pages::render(
        state,
        status,
        &Page {
            layout: layout(state, session).await?,
            action: PATH,
            approve_action: APPROVE_PATH,
            boxes: boxes("may", |a| kitchen.allergens.contains(&a)),
            stored: codes(&kitchen.allergens),
            approval: approval_text(state, kitchen.approval, "kitchen-may-contain-unapproved")
                .await?,
            saved,
            changed,
        },
    ))
}

async fn load(state: &AppState) -> Result<KitchenMayContain, Rejection> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    company::kitchen_may_contain(&mut conn)
        .await
        .map_err(|e| fail(state, &e).into())
}

pub async fn page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let kitchen = load(&state).await?;
        render(
            &state,
            &session,
            StatusCode::OK,
            &kitchen,
            flash.saved(),
            false,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn save(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let chosen: BTreeSet<Allergen> = Allergen::ALL
            .into_iter()
            .filter(|a| fields.checked(&format!("may_{}", a.code())))
            .collect();
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        company::set_kitchen_may_contain(&mut conn, &chosen)
            .await
            .map_err(|e| fail(&state, &e))?;
        Ok(saved(&headers, PATH))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// Approves exactly the list the page showed: it comes back in hidden fields, and the
/// catalogue refuses it when the list changed in the meantime.
pub async fn approve(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::EditAllergens)?;
        let reviewed: BTreeSet<Allergen> = fields
            .all("may_contain")
            .filter_map(|code| code.parse().ok())
            .collect();
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = company::approve_kitchen_may_contain(
            &mut conn,
            &reviewed,
            staff.0.id,
            state.clock.as_ref(),
        )
        .await;
        drop(conn);
        match result {
            Ok(_) => Ok(saved(&headers, PATH)),
            Err(catalog::Error::AllergensChanged) => {
                let kitchen = load(&state).await?;
                render(
                    &state,
                    &session,
                    StatusCode::CONFLICT,
                    &kitchen,
                    false,
                    true,
                )
                .await
            }
            Err(error) => Err(fail(&state, &error).into()),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
