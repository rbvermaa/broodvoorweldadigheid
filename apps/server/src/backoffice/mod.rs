//! The back office under `/staff`. Every handler takes [`Staff`] and checks a permission
//! (ADR 0010). Forms post normally and redirect: to the list after an object's own form, else
//! back with `?opgeslagen=1` (ADR 0052); htmx only adds inline
//! checks and partial updates on top (ADR 0019).

pub mod bake_days;
pub mod batch_labels;
pub mod batches;
pub mod business;
pub mod charities;
pub mod costing;
pub mod customers;
pub mod extras;
pub mod form;
pub mod ingredients;
pub mod kitchen;
pub mod label_files;
pub mod lists;
pub mod locations;
pub mod nav;
pub mod orders;
pub mod payouts;
pub mod polls;
pub mod product_labels;
pub mod products;
pub mod recipes;
pub mod reports;
pub mod settings;
pub mod sick_day;

use std::fmt::Display;

use auth::Permission;
use axum::Router;
use axum::http::header::HeaderName;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use i18n::FluentArgs;
use serde::Deserialize;
use tower_sessions::Session;

use crate::components::Table;
use crate::csrf;
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

// One line per feature, sorted (ADR 0044).
const FEATURES: &[fn() -> Router<AppState>] = &[
    bake_days::routes,
    batch_labels::routes,
    batches::routes,
    business::routes,
    charities::routes,
    costing::routes,
    customers::routes,
    extras::routes,
    ingredients::routes,
    kitchen::routes,
    label_files::routes,
    lists::routes,
    locations::routes,
    orders::routes,
    payouts::routes,
    polls::routes,
    product_labels::routes,
    products::routes,
    recipes::routes,
    reports::routes,
    settings::routes,
    sick_day::routes,
];

pub fn routes() -> Router<AppState> {
    FEATURES
        .iter()
        .fold(Router::new(), |router, feature| router.merge(feature()))
}

/// A response that ends the handler early: a refusal, a 404 or a 500 page. Boxed, so
/// results stay small.
pub struct Rejection(Box<Response>);

impl From<Response> for Rejection {
    fn from(response: Response) -> Self {
        Self(Box::new(response))
    }
}

impl Rejection {
    pub fn into_response(self) -> Response {
        *self.0
    }
}

pub type Step<T> = Result<T, Rejection>;
pub type Outcome = Step<Response>;

/// `?opgeslagen=1` after a save that stays on its page. On a list, `?opgeslagen=<id>`,
/// `?toegevoegd=<id>` or `?gearchiveerd=<id>` names the row saved on its own page (ADR 0052).
#[derive(Debug, Default, Deserialize)]
pub struct Flash {
    opgeslagen: Option<String>,
    toegevoegd: Option<String>,
    gearchiveerd: Option<String>,
}

impl Flash {
    pub fn saved(&self) -> bool {
        self.opgeslagen.is_some()
    }

    /// What a list shows after a save: the row that links to `path(id)`, highlighted and
    /// named. When that row is not in `table`, only the plain confirmation.
    pub fn row(&self, state: &AppState, table: &Table, path: impl Fn(i64) -> String) -> SavedRow {
        let (id, message) = match (&self.toegevoegd, &self.gearchiveerd, &self.opgeslagen) {
            (Some(id), _, _) => (id, "added-row"),
            (None, Some(id), _) => (id, "archived-row"),
            (None, None, Some(id)) => (id, "saved-row"),
            (None, None, None) => return SavedRow::default(),
        };
        let Ok(id) = id.parse() else {
            return SavedRow::default();
        };
        let href = path(id);
        match table.name_of(&href) {
            Some(name) => SavedRow {
                text: t_with(state, message, &[("name", name.to_owned())]),
                href,
            },
            None => SavedRow {
                text: t(state, "saved"),
                href: String::new(),
            },
        }
    }
}

/// The confirmation on a list after a save; `href` is the highlighted row's link.
#[derive(Debug, Default)]
pub struct SavedRow {
    pub text: String,
    pub href: String,
}

/// `?q=` above a long list (F24, ADR 0051).
#[derive(Debug, Default, Deserialize)]
pub struct Search {
    #[serde(default)]
    q: String,
}

impl Search {
    const MAX_CHARS: usize = 100;

    pub fn text(&self) -> String {
        self.q.trim().chars().take(Self::MAX_CHARS).collect()
    }

    /// Whether every word of the term is in one of `fields`, ignoring case and accents.
    pub fn matches(&self, fields: &[&str]) -> bool {
        let term = i18n::search_key(&self.text());
        let haystack: Vec<String> = fields.iter().map(|f| i18n::search_key(f)).collect();
        term.split_whitespace()
            .all(|word| haystack.iter().any(|field| field.contains(word)))
    }

    /// The search field for the list at `action`, which shows `found` rows.
    pub fn view(&self, state: &AppState, action: &str, found: usize) -> SearchBox {
        let q = self.text();
        SearchBox {
            action: action.to_owned(),
            none: t_with(state, "search-none", &[("q", q.clone())]),
            found: if q.is_empty() {
                String::new()
            } else {
                t_count(
                    state,
                    "search-count",
                    u32::try_from(found).unwrap_or(u32::MAX),
                )
            },
            q,
        }
    }
}

/// templates/components/search.html
#[derive(Debug, Default)]
pub struct SearchBox {
    pub action: String,
    pub q: String,
    /// "Niets gevonden voor «q»".
    pub none: String,
    /// For the live region: how many rows the search found; empty without a search.
    pub found: String,
}

pub enum RowChange {
    Saved,
    Added,
    Archived,
}

impl RowChange {
    /// `Saved` for an object that existed before the form, else `Added`.
    pub const fn of(existed: bool) -> Self {
        if existed { Self::Saved } else { Self::Added }
    }
}

/// After saving one object on its own page: back to `list`, which names and highlights
/// the row of object `id` (ADR 0052).
pub fn saved_to_list(headers: &HeaderMap, list: &str, change: RowChange, id: i64) -> Response {
    redirect(headers, list_after_save(list, change, id))
}

/// The link to `list` that names and highlights the row of object `id`.
pub fn list_after_save(list: &str, change: RowChange, id: i64) -> String {
    let key = match change {
        RowChange::Saved => "opgeslagen",
        RowChange::Added => "toegevoegd",
        RowChange::Archived => "gearchiveerd",
    };
    with_query(list, &format!("{key}={id}"))
}

pub fn require(state: &AppState, staff: &Staff, permission: Permission) -> Step<()> {
    staff
        .require(state, permission)
        .map_err(|forbidden| forbidden.into_response().into())
}

pub fn fail(state: &AppState, error: &dyn Display) -> Response {
    tracing::error!(%error, "back office");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}

pub fn not_found(state: &AppState) -> Response {
    pages::error(state, StatusCode::NOT_FOUND)
}

/// The layout with the session's CSRF token, for pages with forms.
pub async fn layout<'a>(state: &'a AppState, session: &Session) -> Step<Layout<'a>> {
    let token = csrf::token(session)
        .await
        .map_err(|error| fail(state, &error))?;
    Ok(Layout::new(state).with_csrf_token(token))
}

pub fn is_htmx(headers: &HeaderMap) -> bool {
    headers.contains_key("hx-request")
}

/// After a save that stays on its page: back to `path` with `opgeslagen=1`.
pub fn saved(headers: &HeaderMap, path: &str) -> Response {
    redirect(headers, with_query(path, "opgeslagen=1"))
}

fn with_query(path: &str, pair: &str) -> String {
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{path}{separator}{pair}")
}

/// A 303 to `target`, or for htmx an `HX-Redirect` there.
pub fn redirect(headers: &HeaderMap, target: String) -> Response {
    if is_htmx(headers) {
        match HeaderValue::try_from(target) {
            Ok(value) => (
                StatusCode::OK,
                [(HeaderName::from_static("hx-redirect"), value)],
            )
                .into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    } else {
        Redirect::to(&target).into_response()
    }
}

/// A form with mistakes goes back with 422; htmx gets 200, because it only swaps 2xx.
pub fn invalid_status(headers: &HeaderMap) -> StatusCode {
    if is_htmx(headers) {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    }
}

/// A Dutch text; a missing id is logged and shown as the id, so a page still renders.
pub fn t(state: &AppState, id: &str) -> String {
    t_with(state, id, &[])
}

pub fn t_with(state: &AppState, id: &str, args: &[(&str, String)]) -> String {
    let mut fluent = FluentArgs::new();
    for (name, value) in args {
        fluent.set(*name, value.clone());
    }
    let args = (!args.is_empty()).then_some(&fluent);
    state.i18n.t(id, args).unwrap_or_else(|error| {
        tracing::error!(%error, "missing text");
        id.to_owned()
    })
}

pub fn t_count(state: &AppState, id: &str, n: u32) -> String {
    let mut fluent = FluentArgs::new();
    fluent.set("n", i64::from(n));
    state.i18n.t(id, Some(&fluent)).unwrap_or_else(|error| {
        tracing::error!(%error, "missing text");
        id.to_owned()
    })
}

/// Parses a numeric path id; anything else is a 404.
pub fn path_id(state: &AppState, raw: &str) -> Step<i64> {
    raw.parse().map_err(|_| not_found(state).into())
}
