//! Public site pages: the FAQ (F4e) and the contact form (F4f, ADR 0018).

use std::sync::Arc;
use std::time::Duration;

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use catalog::markdown::Block;
use domain::Clock;
use i18n::FluentArgs;
use jiff::SignedDuration;
use notify::{Company, ComposeError, Composer, ContactMessage};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tower_sessions::Session;

use crate::csrf;
use crate::pages::{self, Layout};
use crate::rate_limit;
use crate::state::AppState;

/// Microseconds since the epoch when the contact form was shown (ADR 0018).
const FORM_SHOWN_AT: &str = "contact_form_shown_at";
pub const MIN_FILL_TIME: SignedDuration = SignedDuration::from_secs(3);
/// Ten messages at once per client, then one more every five minutes.
pub const SEND_BURST: u32 = 10;
const SEND_PERIOD: Duration = Duration::from_secs(5 * 60);
pub const NAME_MAX: usize = 100;
pub const MESSAGE_MAX: usize = 5000;
const EMAIL_MAX: usize = 254;

#[derive(Clone)]
struct SiteClock(Arc<dyn Clock>);

/// `/faq` and `/contact`. Only sending the contact form is rate limited.
pub fn routes(state: &AppState, clock: Arc<dyn Clock>) -> Router<AppState> {
    let send = rate_limit::apply(
        Router::new().route("/contact", post(send)),
        state,
        SEND_PERIOD,
        SEND_BURST,
    );
    Router::new()
        .route("/faq", get(faq))
        .route("/contact", get(contact))
        .merge(send)
        .layer(Extension(SiteClock(clock)))
}

struct Entry {
    id: i64,
    question: String,
    answer: Vec<Block>,
}

#[derive(Template)]
#[template(path = "faq.html")]
struct FaqPage<'a> {
    layout: Layout<'a>,
    entries: Vec<Entry>,
}

async fn faq(State(state): State<AppState>) -> Response {
    let published = match state.pool.acquire().await {
        Ok(mut conn) => catalog::faq::list_published(&mut conn).await,
        Err(error) => Err(catalog::Error::Db(error)),
    };
    let entries = match published {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!(%error, "cannot load the FAQ");
            return pages::error(&state, StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let page = FaqPage {
        layout: Layout::new(&state),
        entries: entries
            .into_iter()
            .map(|entry| Entry {
                id: entry.id.get(),
                answer: entry.answer(),
                question: entry.question,
            })
            .collect(),
    };
    pages::render(&state, StatusCode::OK, &page)
}

#[derive(Debug, Default, Deserialize)]
pub struct ContactForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    message: String,
    /// The spam trap; people never see it.
    #[serde(default)]
    website: String,
}

/// Translated error texts; empty means no error.
#[derive(Default)]
struct Errors {
    form: String,
    name: String,
    email: String,
    message: String,
}

impl Errors {
    fn any(&self) -> bool {
        [&self.form, &self.name, &self.email, &self.message]
            .iter()
            .any(|text| !text.is_empty())
    }
}

#[derive(Template)]
#[template(path = "contact.html")]
struct ContactPage<'a> {
    layout: Layout<'a>,
    form: ContactForm,
    errors: Errors,
}

#[derive(Template)]
#[template(path = "contact-sent.html")]
struct SentPage<'a> {
    layout: Layout<'a>,
}

async fn contact(
    State(state): State<AppState>,
    Extension(SiteClock(clock)): Extension<SiteClock>,
    session: Session,
) -> Response {
    let token = match csrf::token(&session).await {
        Ok(token) => token,
        Err(error) => return session_error(&state, &error),
    };
    if let Err(error) = mark_shown(&session, clock.as_ref()).await {
        return session_error(&state, &error);
    }
    form_page(
        &state,
        token,
        StatusCode::OK,
        ContactForm::default(),
        Errors::default(),
    )
}

async fn send(
    State(state): State<AppState>,
    Extension(SiteClock(clock)): Extension<SiteClock>,
    session: Session,
    Form(form): Form<ContactForm>,
) -> Response {
    let token = match csrf::token(&session).await {
        Ok(token) => token,
        Err(error) => return session_error(&state, &error),
    };
    if !form.website.trim().is_empty() {
        tracing::info!("contact form: spam trap filled in, nothing sent");
        return sent(&state);
    }
    let shown = match session.get::<i64>(FORM_SHOWN_AT).await {
        Ok(shown) => shown,
        Err(error) => return session_error(&state, &error),
    };
    let now = clock.now().as_microsecond();
    let too_fast = shown.is_none_or(|shown| {
        now.saturating_sub(shown) < i64::try_from(MIN_FILL_TIME.as_micros()).unwrap_or(i64::MAX)
    });
    if too_fast {
        if shown.is_none()
            && let Err(error) = mark_shown(&session, clock.as_ref()).await
        {
            return session_error(&state, &error);
        }
        let errors = Errors {
            form: text(&state, "contact-error-too-fast"),
            ..Errors::default()
        };
        return form_page(
            &state,
            token,
            StatusCode::UNPROCESSABLE_ENTITY,
            form,
            errors,
        );
    }
    let errors = validate(&state, &form);
    if errors.any() {
        return form_page(
            &state,
            token,
            StatusCode::UNPROCESSABLE_ENTITY,
            form,
            errors,
        );
    }
    let dedupe_key = shown.map(|shown| submission_key(&session, shown));
    if let Err(error) = queue(&state, clock.as_ref(), &form, dedupe_key.as_deref()).await {
        tracing::error!(%error, "contact form: message not queued");
        return pages::error(&state, StatusCode::INTERNAL_SERVER_ERROR);
    }
    sent(&state)
}

/// Queues the message for the bakery. Nothing is stored apart from the outbox row.
async fn queue(
    state: &AppState,
    clock: &dyn Clock,
    form: &ContactForm,
    dedupe_key: Option<&str>,
) -> Result<(), QueueError> {
    let to = state
        .config
        .contact_email
        .clone()
        .ok_or(QueueError::NoAddress)?;
    let mut conn = state.pool.acquire().await?;
    let company = Company::load(&mut conn).await?;
    let email = Composer::new(&state.i18n, &company).contact_message(&ContactMessage {
        to,
        name: form.name.trim().to_owned(),
        email: form.email.trim().to_owned(),
        message: form.message.trim().to_owned(),
    })?;
    notify::enqueue(&mut conn, clock, &email, dedupe_key).await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum QueueError {
    #[error("BAKERY_CONTACT_EMAIL is not set")]
    NoAddress,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Compose(#[from] ComposeError),
    #[error(transparent)]
    Outbox(#[from] notify::OutboxError),
}

fn validate(state: &AppState, form: &ContactForm) -> Errors {
    let name = form.name.trim();
    let email = form.email.trim();
    let message = form.message.trim();
    let mut errors = Errors::default();
    if name.is_empty() || form.name.chars().any(char::is_control) {
        errors.name = text(state, "contact-error-name");
    } else if name.chars().count() > NAME_MAX {
        errors.name = text(state, "contact-error-name-long");
    }
    if email.len() > EMAIL_MAX || !notify::is_address(email) {
        errors.email = text(state, "contact-error-email");
    }
    if message.is_empty()
        || message
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        errors.message = text(state, "contact-error-message");
    } else if message.chars().count() > MESSAGE_MAX {
        let mut args = FluentArgs::new();
        args.set("max", MESSAGE_MAX);
        errors.message = state
            .i18n
            .t("contact-error-message-long", Some(&args))
            .unwrap_or_default();
    }
    errors
}

/// One key per shown form, so a double submit queues one email. Hashed, because the
/// session id is a credential.
fn submission_key(session: &Session, shown: i64) -> String {
    let id = session.id().map(|id| id.to_string()).unwrap_or_default();
    let digest = Sha256::digest(format!("{id}:{shown}"));
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("contact:{hex}")
}

fn text(state: &AppState, id: &str) -> String {
    state.i18n.t(id, None).unwrap_or_else(|error| {
        tracing::error!(%error, id, "missing contact form text");
        String::new()
    })
}

async fn mark_shown(
    session: &Session,
    clock: &dyn Clock,
) -> Result<(), tower_sessions::session::Error> {
    session
        .insert(FORM_SHOWN_AT, clock.now().as_microsecond())
        .await
}

fn sent(state: &AppState) -> Response {
    pages::render(
        state,
        StatusCode::OK,
        &SentPage {
            layout: Layout::new(state),
        },
    )
}

fn form_page(
    state: &AppState,
    token: String,
    status: StatusCode,
    form: ContactForm,
    errors: Errors,
) -> Response {
    let page = ContactPage {
        layout: Layout::new(state).with_csrf_token(token),
        form,
        errors,
    };
    pages::render(state, status, &page)
}

fn session_error(state: &AppState, error: &tower_sessions::session::Error) -> Response {
    tracing::error!(%error, "cannot use the session");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}
