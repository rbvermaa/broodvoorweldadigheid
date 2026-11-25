//! A batch's product and freezer labels as PNG and CSV files for the Niimbot app, and the
//! pieces that went into the freezer (F18, F18a, ADR 0047). Every file is made anew after
//! the F17 label check; a blocked label shows why instead.

use askama::Template;
use auth::Permission;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bakedays::BakeDay;
use domain::{BatchId, ProductId};
use orders::audit::Actor;
use production::batches::{self, Batch};
use production::label_check::LabelBlock;
use production::label_files::{self, BatchLabel, LabelFile, LabelGap, LabelKind};
use tower_sessions::Session;

use super::form::{Fields, parse_count};
use super::{
    Flash, Outcome, Rejection, Step, fail, invalid_status, layout, not_found, path_id, require,
    saved, t, t_with,
};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

const CSV_FILE: &str = "etiketten.csv";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/staff/bake-days/{id}/batches/{batch}/etiketten", get(page))
        .route(
            "/staff/bake-days/{id}/batches/{batch}/etiketten/{product}/vriezer",
            post(freeze),
        )
        .route(
            "/staff/bake-days/{id}/batches/{batch}/etiketten/{product}/{kind}/{file}",
            get(download),
        )
}

/// The labels page of the batch page at `batch_path`.
pub fn path(batch_path: &str) -> String {
    format!("{batch_path}/etiketten")
}

fn batch_path(day: &BakeDay, batch: BatchId) -> String {
    format!("/staff/bake-days/{}/batches/{}", day.id.get(), batch.get())
}

fn production_error(state: &AppState, error: &production::Error) -> Rejection {
    match error {
        production::Error::NotFound | production::Error::BakeDays(bakedays::Error::NotFound) => {
            not_found(state).into()
        }
        other => fail(state, other).into(),
    }
}

async fn load(state: &AppState, raw_day: &str, raw_batch: &str) -> Step<(BakeDay, Batch)> {
    let day = super::bake_days::load(state, raw_day).await?;
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

fn product_of(state: &AppState, batch: &Batch, raw: &str) -> Step<ProductId> {
    let product = ProductId::new(path_id(state, raw)?);
    if batch.outputs.iter().any(|o| o.product_id == product) {
        Ok(product)
    } else {
        Err(not_found(state).into())
    }
}

/// Why a label cannot be made, as the page says it.
fn refusal(error: &production::Error) -> Option<&'static str> {
    Some(match error {
        production::Error::LabelBlocked(block) => match block {
            LabelBlock::KitchenNotApproved => "label-check-kitchen",
            LabelBlock::ProductNotApproved => "label-check-product",
            LabelBlock::Unweighable => "label-check-unweighable",
            LabelBlock::Differs { .. } => "label-check-differs",
        },
        production::Error::LabelGap(gap) => match gap {
            LabelGap::NoAddress => "batch-labels-gap-address",
            LabelGap::NotPackaged => "batch-labels-gap-packaged",
            LabelGap::NoShelfLife => "batch-labels-gap-shelf-life",
            LabelGap::NoStorage => "batch-labels-gap-storage",
            LabelGap::NoFreezerPeriod => "batch-labels-gap-freezer-days",
            LabelGap::NotBaked => "batch-labels-gap-not-baked",
            LabelGap::NoPieces => "batch-labels-gap-no-pieces",
            LabelGap::NoFrozenPieces => "batch-labels-gap-no-frozen",
        },
        production::Error::Label(labels::Error::TooLong) => "batch-labels-too-long",
        production::Error::Label(labels::Error::TooWide) => "batch-labels-too-wide",
        _ => return None,
    })
}

const fn kind_title(kind: LabelKind) -> &'static str {
    match kind {
        LabelKind::Product => "batch-labels-product",
        LabelKind::Freezer => "batch-labels-freezer",
    }
}

pub struct Download {
    pub href: String,
    pub text: String,
}

pub struct KindView {
    pub title: String,
    /// Shown instead of the files when the label cannot be made.
    pub refused: Option<String>,
    pub copies: String,
    pub warning: Option<String>,
    pub files: Vec<Download>,
}

pub struct ProductView {
    pub name: String,
    pub check_href: String,
    pub kinds: Vec<KindView>,
    pub freeze_action: String,
    pub frozen: String,
    pub frozen_error: String,
    pub baked: bool,
}

/// What a render adds to the page: a refused download, a refused freezer count.
#[derive(Default)]
struct Answer {
    refused: Option<String>,
    frozen_error: Option<(ProductId, String, String)>,
    saved: bool,
}

#[derive(Template)]
#[template(path = "staff/label_files.html")]
struct Page<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    saved: bool,
    refused: Option<String>,
    products: Vec<ProductView>,
}

/// Typst is CPU work, so it runs off the async workers.
async fn draw(
    state: &AppState,
    label: BatchLabel,
) -> Step<(BatchLabel, Result<Vec<LabelFile>, production::Error>)> {
    let i18n = state.i18n.clone();
    tokio::task::spawn_blocking(move || {
        let files = label_files::files(&label, &i18n);
        (label, files)
    })
    .await
    .map_err(|e| fail(state, &e).into())
}

async fn kind_view(
    state: &AppState,
    batch: BatchId,
    product: ProductId,
    kind: LabelKind,
    base: &str,
) -> Step<KindView> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let result = label_files::label(&mut conn, batch, product, kind).await;
    drop(conn);
    let title = t(state, kind_title(kind));
    let label = match result {
        Ok(label) => label,
        Err(error) => match refusal(&error) {
            Some(id) => {
                return Ok(KindView {
                    title,
                    refused: Some(t(state, id)),
                    copies: String::new(),
                    warning: None,
                    files: Vec::new(),
                });
            }
            None => return Err(production_error(state, &error)),
        },
    };
    let (label, files) = draw(state, label).await?;
    let files = match files {
        Ok(files) => files,
        Err(error) => {
            return match refusal(&error) {
                Some(id) => Ok(KindView {
                    title,
                    refused: Some(t(state, id)),
                    copies: String::new(),
                    warning: None,
                    files: Vec::new(),
                }),
                None => Err(production_error(state, &error)),
            };
        }
    };
    let href = |file: &str| format!("{base}/{}/{}/{file}", product.get(), kind.slug());
    let side_id = |side| match side {
        labels::Side::Whole => "batch-labels-file-whole",
        labels::Side::Front => "batch-labels-file-front",
        labels::Side::Back => "batch-labels-file-back",
    };
    let mut downloads: Vec<Download> = files
        .iter()
        .enumerate()
        .map(|(i, file)| Download {
            href: href(&format!("{}.png", i + 1)),
            text: t_with(
                state,
                side_id(file.png.side),
                &[
                    ("width", file.png.size.width_mm.to_string()),
                    ("length", file.png.size.length_mm.to_string()),
                ],
            ),
        })
        .collect();
    downloads.push(Download {
        href: href(CSV_FILE),
        text: t(state, "batch-labels-file-csv"),
    });
    Ok(KindView {
        title,
        refused: None,
        copies: t_with(
            state,
            "batch-labels-copies",
            &[("n", label.copies().to_string())],
        ),
        warning: label
            .order_changed()
            .then(|| t(state, "label-check-order-warning")),
        files: downloads,
    })
}

async fn render(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    day: &BakeDay,
    batch: &Batch,
    answer: Answer,
) -> Outcome {
    let back = batch_path(day, batch.id);
    let base = path(&back);
    let mut products = Vec::new();
    for output in &batch.outputs {
        let mut kinds = Vec::new();
        for kind in LabelKind::ALL {
            kinds.push(kind_view(state, batch.id, output.product_id, kind, &base).await?);
        }
        let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
        let frozen = label_files::frozen(&mut conn, batch.id, output.product_id)
            .await
            .map_err(|e| production_error(state, &e))?;
        drop(conn);
        let (frozen, frozen_error) = match &answer.frozen_error {
            Some((product, raw, error)) if *product == output.product_id => {
                (raw.clone(), error.clone())
            }
            _ => (
                frozen.map(|f| f.pieces.to_string()).unwrap_or_default(),
                String::new(),
            ),
        };
        products.push(ProductView {
            name: output.name.clone(),
            check_href: super::batch_labels::path(&back, output.product_id),
            kinds,
            freeze_action: format!("{base}/{}/vriezer", output.product_id.get()),
            frozen,
            frozen_error,
            baked: output.actual_pieces.is_some(),
        });
    }
    Ok(pages::render(
        state,
        status,
        &Page {
            layout: layout(state, session).await?,
            heading: t_with(
                state,
                "batch-labels-title",
                &[("code", batch.code.to_string())],
            ),
            back,
            saved: answer.saved,
            refused: answer.refused,
            products,
        },
    ))
}

pub async fn page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((day, batch)): Path<(String, String)>,
    Query(flash): Query<Flash>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load(&state, &day, &batch).await?;
        render(
            &state,
            &session,
            StatusCode::OK,
            &day,
            &batch,
            Answer {
                saved: flash.saved(),
                ..Answer::default()
            },
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

fn file_response(content_type: &str, name: &str, body: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, content_type.to_owned()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (CACHE_CONTROL, "private, no-store".to_owned()),
        ],
        body,
    )
        .into_response()
}

/// One PNG (`1.png`, `2.png`) or the CSV of a label, made after the label check. A
/// blocked label answers 409 with the labels page and the reason.
pub async fn download(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((day, batch, product, kind, file)): Path<(String, String, String, String, String)>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load(&state, &day, &batch).await?;
        let product = product_of(&state, &batch, &product)?;
        let kind = LabelKind::from_slug(&kind).ok_or_else(|| not_found(&state))?;
        let index = if file == CSV_FILE {
            None
        } else {
            Some(
                file.strip_suffix(".png")
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| not_found(&state))?,
            )
        };
        let refused = |state: &AppState, error: &production::Error| -> Option<String> {
            refusal(error).map(|id| t(state, id))
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let result = label_files::label(&mut conn, batch.id, product, kind).await;
        drop(conn);
        let label: BatchLabel = match result {
            Ok(label) => label,
            Err(error) => {
                let Some(text) = refused(&state, &error) else {
                    return Err(production_error(&state, &error));
                };
                return render(
                    &state,
                    &session,
                    StatusCode::CONFLICT,
                    &day,
                    &batch,
                    Answer {
                        refused: Some(text),
                        ..Answer::default()
                    },
                )
                .await;
            }
        };
        let (label, files) = draw(&state, label).await?;
        let files = match files {
            Ok(files) => files,
            Err(error) => {
                let Some(text) = refused(&state, &error) else {
                    return Err(production_error(&state, &error));
                };
                return render(
                    &state,
                    &session,
                    StatusCode::CONFLICT,
                    &day,
                    &batch,
                    Answer {
                        refused: Some(text),
                        ..Answer::default()
                    },
                )
                .await;
            }
        };
        let Some(index) = index else {
            let csv = label_files::csv(&label, &files, &state.i18n)
                .map_err(|e| production_error(&state, &e))?;
            let name = format!("{}-{}.csv", label.label().batch_code, kind.slug());
            return Ok(file_response(
                "text/csv; charset=utf-8",
                &name,
                csv.into_bytes(),
            ));
        };
        let file = files
            .into_iter()
            .nth(index - 1)
            .ok_or_else(|| not_found(&state))?;
        tracing::info!(
            staff = staff.0.id.get(),
            batch = batch.id.get(),
            product = product.get(),
            kind = kind.slug(),
            "label downloaded"
        );
        Ok(file_response("image/png", &file.name, file.png.bytes))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn freeze(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((day, batch, product)): Path<(String, String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (day, batch) = load(&state, &day, &batch).await?;
        let product = product_of(&state, &batch, &product)?;
        let raw = fields.text("frozen").to_owned();
        let refuse = |error_id: &str| Some((product, raw.clone(), t(&state, error_id)));
        let error = match parse_count(&raw) {
            None => refuse("batch-labels-error-frozen"),
            Some(pieces) => {
                let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
                let result = label_files::record_frozen(
                    &mut conn,
                    batch.id,
                    product,
                    pieces,
                    Actor::Staff(staff.0.id),
                    state.clock.as_ref(),
                )
                .await;
                match result {
                    Ok(()) => None,
                    Err(production::Error::NotBaked) => refuse("batch-labels-gap-not-baked"),
                    Err(production::Error::Invalid(_)) => refuse("batch-labels-error-frozen"),
                    Err(error) => return Err(production_error(&state, &error)),
                }
            }
        };
        match error {
            None => Ok(saved(&headers, &path(&batch_path(&day, batch.id)))),
            Some(error) => {
                let status = invalid_status(&headers);
                render(
                    &state,
                    &session,
                    status,
                    &day,
                    &batch,
                    Answer {
                        frozen_error: Some(error),
                        ..Answer::default()
                    },
                )
                .await
            }
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
