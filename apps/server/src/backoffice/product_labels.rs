//! A product's label settings: packaged, shelf life, freezer period and storage
//! instruction (F18, F18a, ADR 0047).

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use catalog::label_settings::{self, LabelSettings, MAX_DAYS, MAX_STORAGE_CHARS};
use domain::ProductId;
use tower_sessions::Session;

use super::form::{Errors, Fields};
use super::{
    Flash, Outcome, Rejection, RowChange, fail, invalid_status, layout, not_found, path_id,
    require, saved_to_list, t_with,
};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/staff/products/{id}/etiket", get(page).post(save))
}

fn path(product: ProductId) -> String {
    format!("/staff/products/{}/etiket", product.get())
}

#[derive(Debug, Default)]
struct Values {
    packaged: bool,
    shelf_life_days: String,
    freezer_days: String,
    storage: String,
}

impl Values {
    fn from_settings(settings: &LabelSettings) -> Self {
        let days = |d: Option<u16>| d.map(|d| d.to_string()).unwrap_or_default();
        Self {
            packaged: settings.packaged,
            shelf_life_days: days(settings.shelf_life_days),
            freezer_days: days(settings.freezer_days),
            storage: settings.storage.clone(),
        }
    }

    fn from_fields(fields: &Fields) -> Self {
        Self {
            packaged: fields.checked("packaged"),
            shelf_life_days: fields.text("shelf_life_days").to_owned(),
            freezer_days: fields.text("freezer_days").to_owned(),
            storage: fields.text("storage").to_owned(),
        }
    }

    fn parse(&self, state: &AppState) -> Result<LabelSettings, Errors> {
        let mut errors = Errors::default();
        let mut days = |name: &str, raw: &str| -> Option<u16> {
            if raw.is_empty() {
                return None;
            }
            let parsed = raw
                .parse::<u16>()
                .ok()
                .filter(|d| (1..=MAX_DAYS).contains(d));
            if parsed.is_none() {
                errors.add(
                    name,
                    t_with(
                        state,
                        "product-labels-days-invalid",
                        &[("max", MAX_DAYS.to_string())],
                    ),
                );
            }
            parsed
        };
        let shelf_life_days = days("shelf_life_days", &self.shelf_life_days);
        let freezer_days = days("freezer_days", &self.freezer_days);
        if self.storage.chars().count() > MAX_STORAGE_CHARS {
            errors.add(
                "storage",
                t_with(
                    state,
                    "product-labels-storage-too-long",
                    &[("max", MAX_STORAGE_CHARS.to_string())],
                ),
            );
        }
        if errors.is_empty() {
            Ok(LabelSettings {
                packaged: self.packaged,
                shelf_life_days,
                freezer_days,
                storage: self.storage.clone(),
            })
        } else {
            Err(errors)
        }
    }
}

#[derive(Template)]
#[template(path = "staff/product_labels.html")]
struct Page<'a> {
    layout: Layout<'a>,
    heading: String,
    action: String,
    back: String,
    saved: bool,
    values: Values,
    errors: Errors,
}

async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    product: ProductId,
    values: Values,
    errors: Errors,
    saved: bool,
) -> Outcome {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let name = catalog::products::get(&mut conn, product)
        .await
        .map_err(|e| catalog_error(state, &e))?
        .name_nl;
    drop(conn);
    Ok(pages::render(
        state,
        status,
        &Page {
            layout: layout(state, session).await?,
            heading: t_with(state, "product-labels-title", &[("product", name)]),
            action: path(product),
            back: format!("/staff/products/{}", product.get()),
            saved,
            values,
            errors,
        },
    ))
}

fn catalog_error(state: &AppState, error: &catalog::Error) -> Rejection {
    match error {
        catalog::Error::NotFound => not_found(state).into(),
        other => fail(state, other).into(),
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
        let product = ProductId::new(path_id(&state, &id)?);
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let settings = label_settings::get(&mut conn, product)
            .await
            .map_err(|e| catalog_error(&state, &e))?;
        drop(conn);
        render(
            &state,
            &session,
            StatusCode::OK,
            product,
            Values::from_settings(&settings),
            Errors::default(),
            flash.saved(),
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn save(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let product = ProductId::new(path_id(&state, &id)?);
        let values = Values::from_fields(&fields);
        let settings = match values.parse(&state) {
            Ok(settings) => settings,
            Err(errors) => {
                return render(
                    &state,
                    &session,
                    invalid_status(&headers),
                    product,
                    values,
                    errors,
                    false,
                )
                .await;
            }
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        label_settings::set(&mut conn, product, &settings)
            .await
            .map_err(|e| catalog_error(&state, &e))?;
        tracing::info!(
            staff = staff.0.id.get(),
            product = product.get(),
            "label settings saved"
        );
        Ok(saved_to_list(
            &headers,
            super::products::LIST,
            RowChange::Saved,
            product.get(),
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
