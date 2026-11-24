use askama::Template;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use i18n::Translator;

use crate::assets;
use crate::state::AppState;

/// What every page passes to `base.html`.
pub struct Layout<'a> {
    i18n: &'a Translator,
    csrf_token: Option<String>,
}

impl<'a> Layout<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self {
            i18n: &state.i18n,
            csrf_token: None,
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
}

#[derive(Template)]
#[template(path = "home.html")]
struct Home<'a> {
    layout: Layout<'a>,
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
        StatusCode::TOO_MANY_REQUESTS => ("error-too-many-title", "error-too-many-text"),
        _ => ("error-server-title", "error-server-text"),
    }
}

pub fn error(state: &AppState, status: StatusCode) -> Response {
    let (title, text) = error_messages(status);
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

pub async fn home(axum::extract::State(state): axum::extract::State<AppState>) -> Response {
    render(
        &state,
        StatusCode::OK,
        &Home {
            layout: Layout::new(&state),
        },
    )
}

pub async fn not_found(axum::extract::State(state): axum::extract::State<AppState>) -> Response {
    error(&state, StatusCode::NOT_FOUND)
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
