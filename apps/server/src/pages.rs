use askama::Template;
use axum::extract::Request;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use i18n::{FluentArgs, Translator};
use tower_sessions::Session;

use crate::assets;
use crate::customer_session::CUSTOMER_KEY;
use crate::icons::Icon;
use crate::state::AppState;

/// An `i64`, however many references deep askama passes it.
pub trait Whole {
    fn whole(&self) -> i64;
}

impl Whole for i64 {
    fn whole(&self) -> i64 {
        *self
    }
}

impl<T: Whole + ?Sized> Whole for &T {
    fn whole(&self) -> i64 {
        (**self).whole()
    }
}

tokio::task_local! {
    static SIGNED_IN: bool;
    static DONATIONS_LINK: bool;
    static REQUEST_PATH: String;
}

/// Tells the pages of this request their own path, for the staff menu.
pub async fn request_path(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    REQUEST_PATH.scope(path, next.run(request)).await
}

/// Tells the pages of this request whether a customer is signed in, for the site header.
/// Only the session key is read, not the login row: a login that ended shows "Mijn
/// bestellingen", which then sends to the login page.
pub async fn header_login(session: Session, request: Request, next: Next) -> Response {
    let signed_in = session
        .get::<String>(CUSTOMER_KEY)
        .await
        .is_ok_and(|login| login.is_some());
    SIGNED_IN.scope(signed_in, next.run(request)).await
}

/// Tells the pages of this request whether the footer links to the public donation page
/// (F11d). Reads the cached switch, so another server sees a change within
/// [`crate::public_donations::FRESH`].
pub async fn footer_donations(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let on = state
        .public_donations
        .is_on(&state.pool)
        .await
        .unwrap_or_else(|error| {
            tracing::error!(%error, "cannot read the donation page switch");
            false
        });
    DONATIONS_LINK.scope(on, next.run(request)).await
}

/// What every page passes to `base.html`.
pub struct Layout<'a> {
    i18n: &'a Translator,
    csrf_token: Option<String>,
    signed_in: bool,
    donations_link: bool,
    path: String,
}

impl<'a> Layout<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self {
            i18n: &state.i18n,
            csrf_token: None,
            signed_in: SIGNED_IN.try_with(|signed_in| *signed_in).unwrap_or(false),
            donations_link: DONATIONS_LINK.try_with(|on| *on).unwrap_or(false),
            path: REQUEST_PATH.try_with(Clone::clone).unwrap_or_default(),
        }
    }

    /// For pages with a form: the token goes into `<meta name="csrf-token">`, which
    /// app.js sends with every HTMX request.
    #[must_use]
    pub fn with_csrf_token(mut self, token: String) -> Self {
        self.csrf_token = Some(token);
        self
    }

    pub fn t(&self, id: &str) -> askama::Result<String> {
        self.i18n.t(id, None).map_err(askama::Error::custom)
    }

    /// For messages that select on a count, such as plurals.
    pub fn t_count(&self, id: &str, n: impl Whole) -> askama::Result<String> {
        let mut args = FluentArgs::new();
        args.set("n", n.whole());
        self.i18n.t(id, Some(&args)).map_err(askama::Error::custom)
    }

    pub fn money(&self, cents: impl Whole) -> String {
        i18n::money(cents.whole())
    }

    /// The Dutch name of a domain allergen code.
    pub fn allergen(&self, code: &str) -> askama::Result<String> {
        self.t(&format!("allergen-{code}"))
    }

    pub fn allergen_icon(&self, code: &str) -> askama::Result<Icon> {
        Icon::allergen(code).map_err(askama::Error::custom)
    }

    /// A decorative icon, hidden from screen readers.
    pub fn icon(&self, name: &str) -> askama::Result<Icon> {
        Icon::named(name).map_err(askama::Error::custom)
    }

    /// An icon that stands alone, read out as `label`.
    pub fn icon_labelled(&self, name: &str, label: &str) -> askama::Result<Icon> {
        Ok(self.icon(name)?.labelled(label))
    }

    pub fn asset(&self, name: &str) -> String {
        assets::find(name).map_or_else(
            || {
                tracing::error!(name, "template links an unknown asset");
                String::new()
            },
            assets::Asset::url,
        )
    }

    pub fn csrf_token(&self) -> Option<&str> {
        self.csrf_token.as_deref()
    }

    /// The `aria-current` value of the staff menu entry at `entry`: `page` on that page,
    /// `true` on a page in its section, empty elsewhere.
    pub fn nav_current(&self, entry: &str) -> &'static str {
        if crate::backoffice::nav::current(&self.path) != Some(entry) {
            ""
        } else if self.path == entry {
            "page"
        } else {
            "true"
        }
    }

    /// Whether the site header links to "Mijn bestellingen" instead of the login page.
    pub fn signed_in(&self) -> bool {
        self.signed_in
    }

    /// Whether the footer links to the public donation page.
    pub fn donations_link(&self) -> bool {
        self.donations_link
    }
}

#[derive(Template)]
#[template(path = "home.html")]
struct Home<'a> {
    layout: Layout<'a>,
    featured: Option<crate::components::Drop>,
    drops: Vec<crate::components::Drop>,
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    layout: Layout<'a>,
    title: &'static str,
    text: &'static str,
}

pub fn render(state: &AppState, status: StatusCode, template: &impl Template) -> Response {
    match template.render() {
        Ok(html) => (status, Html(html)).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot render page");
            if status == StatusCode::INTERNAL_SERVER_ERROR {
                bare_server_error(state)
            } else {
                self::error(state, StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
}

/// Fluent message ids for the title and text of an error page.
pub fn error_messages(status: StatusCode) -> (&'static str, &'static str) {
    match status {
        StatusCode::FORBIDDEN => ("error-forbidden-title", "error-forbidden-text"),
        StatusCode::NOT_FOUND => ("error-not-found-title", "error-not-found-text"),
        StatusCode::METHOD_NOT_ALLOWED => ("error-method-title", "error-method-text"),
        StatusCode::TOO_MANY_REQUESTS => ("error-too-many-title", "error-too-many-text"),
        _ => ("error-server-title", "error-server-text"),
    }
}

pub fn error(state: &AppState, status: StatusCode) -> Response {
    let (title, text) = error_messages(status);
    error_with(state, status, title, text)
}

/// The error page with its own Fluent title and text, for when the generic text per status
/// does not fit.
pub fn error_with(
    state: &AppState,
    status: StatusCode,
    title: &'static str,
    text: &'static str,
) -> Response {
    let page = ErrorPage {
        layout: Layout::new(state),
        title,
        text,
    };
    render(state, status, &page)
}

fn bare_server_error(state: &AppState) -> Response {
    let text = state
        .i18n
        .t("error-server-title", None)
        .unwrap_or_else(|_| "500".to_string());
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        text,
    )
        .into_response()
}

/// The shop's htmx actions need a CSRF token, so the home page starts a session. Without
/// its drops the home page still shows, so a database hiccup does not take it down.
pub async fn home(
    axum::extract::State(state): axum::extract::State<AppState>,
    session: tower_sessions::Session,
) -> Response {
    let token = match crate::csrf::token(&session).await {
        Ok(token) => token,
        Err(error) => {
            tracing::error!(%error, "cannot start a session");
            return self::error(&state, StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let (featured, drops) = crate::shop::for_home(&state).await.unwrap_or_else(|error| {
        tracing::error!(%error, "home page without drops");
        (None, Vec::new())
    });
    render(
        &state,
        StatusCode::OK,
        &Home {
            layout: Layout::new(&state).with_csrf_token(token),
            featured,
            drops,
        },
    )
}

pub async fn not_found(axum::extract::State(state): axum::extract::State<AppState>) -> Response {
    error(&state, StatusCode::NOT_FOUND)
}

pub async fn method_not_allowed(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
    error(&state, StatusCode::METHOD_NOT_ALLOWED)
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;
    use crate::config::Config;

    fn state() -> AppState {
        let pool = db::connect_lazy(
            "postgres://unused@127.0.0.1:1/db",
            std::time::Duration::ZERO,
        )
        .expect("lazy pool");
        let config = Config::from_lookup(|key| {
            (key == "DATABASE_URL").then(|| "postgres://unused/db".to_string())
        })
        .expect("config");
        AppState::new(pool, Translator::dutch().expect("Dutch texts"), config)
    }

    async fn body(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf-8")
    }

    #[derive(Template)]
    #[template(source = "{{ layout.t(\"no-such-message\")? }}", ext = "html")]
    struct Broken<'a> {
        layout: Layout<'a>,
    }

    #[tokio::test]
    async fn a_page_that_cannot_render_becomes_the_dutch_500_page() {
        let state = state();
        let response = render(
            &state,
            StatusCode::OK,
            &Broken {
                layout: Layout::new(&state),
            },
        );
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let html = body(response).await;
        assert!(html.contains(&state.i18n.t("error-server-text", None).expect("text")));
    }

    #[tokio::test]
    async fn a_broken_500_page_falls_back_to_plain_dutch_text() {
        let state = state();
        let response = render(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            &Broken {
                layout: Layout::new(&state),
            },
        );
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            body(response).await,
            state.i18n.t("error-server-title", None).expect("title")
        );
    }

    #[tokio::test]
    async fn csrf_token_goes_into_a_meta_tag() {
        let state = state();
        let page = ErrorPage {
            layout: Layout::new(&state).with_csrf_token("abc_-123".to_string()),
            title: "error-not-found-title",
            text: "error-not-found-text",
        };
        let html = page.render().expect("render");
        assert!(
            html.contains("<meta name=\"csrf-token\" content=\"abc_-123\">"),
            "{html}"
        );
    }

    #[tokio::test]
    async fn unknown_asset_links_nothing() {
        let state = state();
        assert_eq!(Layout::new(&state).asset("nope.css"), "");
        assert!(
            Layout::new(&state)
                .asset("app.css")
                .starts_with("/static/app.css?v=")
        );
    }
}
