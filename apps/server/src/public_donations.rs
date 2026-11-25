//! The public donation page (F11d, ADR 0040): totals per drop and per charity, switched
//! on by the owner in the company settings.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use db::PgPool;
use domain::{CharityId, Money};
use reports::public::{self, Amount, MIN_ORDERS};
use tokio::time::Instant;

use crate::components::{BarChart, Cell, Table};
use crate::pages::{self, Layout};
use crate::state::AppState;

pub const PATH: &str = "/giften";

/// How long a server trusts its copy of the switch for the footer link.
pub const FRESH: Duration = Duration::from_secs(30);

/// Browsers may keep the page this long. `private`: the header differs for signed-in
/// customers, so no shared cache may keep it.
const CACHE: &str = "private, max-age=300";

pub fn routes() -> Router<AppState> {
    Router::new().route(PATH, get(page))
}

/// The owner's switch for the page, cached for [`FRESH`].
#[derive(Default)]
pub struct Switch {
    cached: Mutex<Option<(Instant, bool)>>,
}

impl Switch {
    /// # Errors
    /// When the database cannot be read.
    pub async fn is_on(&self, pool: &PgPool) -> Result<bool, catalog::Error> {
        let cached = *self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, on)) = cached
            && at.elapsed() < FRESH
        {
            return Ok(on);
        }
        let mut conn = pool.acquire().await?;
        let on = catalog::company::public_donations(&mut conn).await?;
        self.remember(on);
        Ok(on)
    }

    /// After the owner changed it on this server.
    pub fn remember(&self, on: bool) {
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), on));
    }
}

#[derive(Template)]
#[template(path = "donations.html")]
struct Page<'a> {
    layout: Layout<'a>,
    total: Option<String>,
    chart: Option<BarChart>,
    charities: Table,
    drops: Table,
    min_orders: i64,
}

fn server_error(state: &AppState, error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, "public donation page");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}

fn money(amount: Money) -> String {
    i18n::money(amount.cents())
}

fn text(state: &AppState, id: &str) -> String {
    state.i18n.t(id, None).unwrap_or_else(|error| {
        tracing::error!(%error, "missing text");
        id.to_owned()
    })
}

async fn page(State(state): State<AppState>) -> Response {
    let Ok(mut conn) = state.pool.acquire().await else {
        return pages::error(&state, StatusCode::INTERNAL_SERVER_ERROR);
    };
    match catalog::company::public_donations(&mut conn).await {
        Ok(true) => {}
        Ok(false) => return pages::error(&state, StatusCode::NOT_FOUND),
        Err(error) => return server_error(&state, &error),
    }
    let names: BTreeMap<CharityId, String> = match catalog::charities::list_all(&mut conn).await {
        Ok(list) => list.into_iter().map(|c| (c.id, c.name)).collect(),
        Err(error) => return server_error(&state, &error),
    };
    let totals = match public::donations(&mut conn).await {
        Ok(totals) => totals,
        Err(error) => return server_error(&state, &error),
    };
    drop(conn);

    let name = |id: CharityId| names.get(&id).cloned().unwrap_or_default();
    let hidden = text(&state, "giften-hidden");
    let shown = |amount: Amount| match amount {
        Amount::Shown(m) => money(m),
        Amount::Hidden => hidden.clone(),
    };

    let mut charities: Vec<(String, Amount)> = totals
        .charities
        .iter()
        .map(|c| (name(c.charity), c.amount))
        .collect();
    charities.sort_by(|a, b| a.0.cmp(&b.0));
    let bars: Vec<(String, i64, String)> = charities
        .iter()
        .filter_map(|(label, amount)| match amount {
            Amount::Shown(m) => Some((label.clone(), m.cents(), money(*m))),
            Amount::Hidden => None,
        })
        .collect();
    let chart = (!bars.is_empty())
        .then(|| BarChart::new("giften-per-doel", text(&state, "giften-chart"), bars));
    let mut charity_rows: Vec<Vec<Cell>> = charities
        .into_iter()
        .map(|(label, amount)| vec![Cell::text(label), Cell::number(shown(amount))])
        .collect();
    if let Some(amount) = totals.unassigned {
        charity_rows.push(vec![
            Cell::text(text(&state, "giften-unassigned")),
            Cell::number(shown(amount)),
        ]);
    }
    let drop_rows = totals
        .drops
        .iter()
        .map(|d| {
            vec![
                Cell::text(state.i18n.long_date(d.date)),
                Cell::text(name(d.charity)),
                Cell::number(shown(d.amount)),
            ]
        })
        .collect();

    let page = Page {
        layout: Layout::new(&state),
        total: match totals.total {
            Amount::Shown(m) if !totals.drops.is_empty() => Some(money(m)),
            _ => None,
        },
        chart,
        charities: Table {
            id: "giften-doelen".to_owned(),
            caption: text(&state, "giften-charities-title"),
            head: vec![
                Cell::text(text(&state, "giften-col-charity")),
                Cell::number(text(&state, "giften-col-amount")),
            ],
            rows: charity_rows,
        },
        drops: Table {
            id: "giften-drops".to_owned(),
            caption: text(&state, "giften-drops-title"),
            head: vec![
                Cell::text(text(&state, "giften-col-drop")),
                Cell::text(text(&state, "giften-col-charity")),
                Cell::number(text(&state, "giften-col-amount")),
            ],
            rows: drop_rows,
        },
        min_orders: i64::try_from(MIN_ORDERS).unwrap_or(i64::MAX),
    };
    let mut response = pages::render(&state, StatusCode::OK, &page);
    if response.status() == StatusCode::OK {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, axum::http::HeaderValue::from_static(CACHE));
    }
    response.into_response()
}
