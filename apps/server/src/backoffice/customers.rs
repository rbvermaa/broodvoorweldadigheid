//! Export and erase a customer's data on their request (F12, ADR 0031). Owner only. The
//! address is searched with a POST, so it never lands in a URL.

use askama::Template;
use auth::Permission;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use domain::CustomerId;
use orders::audit::Actor;
use serde::Deserialize;
use tower_sessions::Session;

use super::{Outcome, Rejection, fail, layout, not_found, path_id, require, t};
use crate::components::{Cell, Table};
use crate::pages::{self, Layout};
use crate::privacy::{self, Erased};
use crate::staff::Staff;
use crate::state::AppState;

pub const PATH: &str = "/staff/customers";

#[derive(Template)]
#[template(path = "staff/customers.html")]
struct CustomersPage<'a> {
    layout: Layout<'a>,
    email: String,
    /// An alert kind for `notice`; no alert when `notice` is empty.
    notice_kind: &'static str,
    notice: String,
    searched: bool,
    found: Option<FoundCustomer>,
}

struct FoundCustomer {
    table: Table,
    export: String,
    erase: String,
}

/// `?verwijderd=1` after an erase, `?geweigerd=open|zakelijk` when it was refused.
#[derive(Debug, Default, Deserialize)]
pub struct Notice {
    verwijderd: Option<String>,
    geweigerd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Search {
    #[serde(default)]
    email: String,
}

fn notice(state: &AppState, query: &Notice) -> (&'static str, String) {
    if query.verwijderd.is_some() {
        return ("success", t(state, "staff-customers-erased"));
    }
    match query.geweigerd.as_deref() {
        Some("open") => ("warning", t(state, "staff-customers-refused-open")),
        Some("zakelijk") => ("warning", t(state, "staff-customers-refused-business")),
        _ => ("info", String::new()),
    }
}

async fn render(
    state: &AppState,
    session: &Session,
    email: String,
    (notice_kind, notice): (&'static str, String),
    searched: bool,
    found: Option<FoundCustomer>,
) -> Outcome {
    Ok(pages::render(
        state,
        StatusCode::OK,
        &CustomersPage {
            layout: layout(state, session).await?,
            email,
            notice_kind,
            notice,
            searched,
            found,
        },
    ))
}

pub async fn page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Query(query): Query<Notice>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Privacy)?;
        let notice = notice(&state, &query);
        render(&state, &session, String::new(), notice, false, None).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn search(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Form(input): Form<Search>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Privacy)?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let found = orders::privacy::find_by_email(&mut conn, &input.email)
            .await
            .map_err(|e| fail(&state, &e))?;
        std::mem::drop(conn);
        let found = found.map(|c| {
            let path = format!("{PATH}/{}", c.id.get());
            FoundCustomer {
                table: Table {
                    id: "klant".to_owned(),
                    caption: t(&state, "staff-customers-caption"),
                    head: vec![
                        Cell::text(t(&state, "staff-customers-name")),
                        Cell::text(t(&state, "staff-customers-email")),
                        Cell::number(t(&state, "staff-customers-orders")),
                    ],
                    rows: vec![vec![
                        Cell::text(c.name),
                        Cell::text(c.email),
                        Cell::number(c.orders.to_string()),
                    ]],
                },
                export: format!("{path}/export"),
                erase: format!("{path}/erase"),
            }
        });
        let email = input.email.trim().to_owned();
        render(
            &state,
            &session,
            email,
            ("info", String::new()),
            true,
            found,
        )
        .await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn export(
    State(state): State<AppState>,
    staff: Staff,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Privacy)?;
        let id = CustomerId::new(path_id(&state, &id)?);
        match privacy::export_json(&state, id).await {
            Ok(json) => Ok(privacy::json_download(
                json,
                &format!("klant-{}.json", id.get()),
            )),
            Err(privacy::PrivacyError::Orders(orders::Error::NotFound)) => {
                Err(not_found(&state).into())
            }
            Err(error) => Err(fail(&state, &error).into()),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn erase(
    State(state): State<AppState>,
    staff: Staff,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Privacy)?;
        let id = CustomerId::new(path_id(&state, &id)?);
        match privacy::erase(&state, id, Actor::Staff(staff.0.id)).await {
            Ok(Erased::Done) => Ok(privacy::go(&headers, "/staff/customers?verwijderd=1")),
            Ok(Erased::OpenOrders) => Ok(privacy::go(&headers, "/staff/customers?geweigerd=open")),
            Ok(Erased::Business) => {
                Ok(privacy::go(&headers, "/staff/customers?geweigerd=zakelijk"))
            }
            Ok(Erased::NotFound) => Err(not_found(&state).into()),
            Err(error) => Err(fail(&state, &error).into()),
        }
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
