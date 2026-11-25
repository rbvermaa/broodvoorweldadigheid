//! The bread vote on the site (F9b, ADR 0039): open polls, voting with a confirmation
//! link by email (or at once for a signed-in customer), and public results where the
//! baker allows them. Pages never show an address or a wish; handlers log neither.

use std::time::Duration;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Form, Router};
use domain::{PollId, PollOptionId};
use i18n::{Amsterdam, FluentArgs, I18nError};
use jiff::Timestamp;
use notify::Composer;
use notify::polls::{self, Ballot, Confirmed, Link, Poll, PollError, Voted};
use notify::subscribers::Links;
use serde::Deserialize;
use tower_sessions::Session;

use crate::account::no_store;
use crate::components::{BarChart, SelectOption};
use crate::pages::{self, Layout};
use crate::state::AppState;
use crate::{csrf, customer_session, rate_limit};

/// Votes: a burst of 5, then one every 12 seconds per client, like drop-alert sign-ups.
pub const VOTE_PERIOD: Duration = Duration::from_secs(12);
pub const VOTE_BURST: u32 = 5;

pub const INDEX_PATH: &str = "/stemmen";

pub fn routes(state: &AppState) -> Router<AppState> {
    let vote = rate_limit::apply(
        Router::new().route("/stemmen/{id}", post(vote)),
        state,
        VOTE_PERIOD,
        VOTE_BURST,
    );
    Router::new()
        .route(INDEX_PATH, get(index))
        .route("/stemmen/{id}", get(page))
        .route("/stemmen/bevestig/{token}", get(confirm_page).post(confirm))
        .merge(vote)
}

fn poll_path(id: PollId) -> String {
    format!("/stemmen/{}", id.get())
}

pub struct PollLink {
    pub title: String,
    pub href: String,
    pub when: String,
}

#[derive(Template)]
#[template(path = "votes/index.html")]
struct IndexPage<'a> {
    layout: Layout<'a>,
    polls: Vec<PollLink>,
}

#[derive(Template)]
#[template(path = "votes/poll.html")]
struct PollPage<'a> {
    layout: Layout<'a>,
    title: String,
    drop: String,
    when: String,
    open: bool,
    action: String,
    options: Vec<SelectOption>,
    signed_in: bool,
    email: String,
    wish: String,
    errors: VoteErrors,
    chart: Option<BarChart>,
}

#[derive(Default)]
pub struct VoteErrors {
    pub choice: String,
    pub email: String,
    pub wish: String,
}

impl VoteErrors {
    pub fn is_empty(&self) -> bool {
        self.choice.is_empty() && self.email.is_empty() && self.wish.is_empty()
    }
}

#[derive(Template)]
#[template(path = "votes/message.html")]
struct Message<'a> {
    layout: Layout<'a>,
    title: &'static str,
    text: &'static str,
    kind: &'static str,
    back: Option<String>,
}

#[derive(Template)]
#[template(path = "votes/confirm.html")]
struct ConfirmPage<'a> {
    layout: Layout<'a>,
    action: String,
    choice: String,
    wish: Option<String>,
}

#[derive(Deserialize)]
pub struct VoteForm {
    #[serde(default)]
    keuze: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    wens: String,
}

const CHECK_MAIL: (&str, &str) = ("votes-check-title", "votes-check-text");
const COUNTED: (&str, &str) = ("votes-counted-title", "votes-counted-text");
const ALREADY: (&str, &str) = ("votes-already-title", "votes-already-text");
const CLOSED: (&str, &str) = ("votes-closed-title", "votes-closed");
const LINK_FAILED: (&str, &str) = ("votes-link-failed-title", "votes-link-failed-text");

fn message(
    state: &AppState,
    status: StatusCode,
    (title, text): (&'static str, &'static str),
    kind: &'static str,
    back: Option<PollId>,
) -> Response {
    pages::render(
        state,
        status,
        &Message {
            layout: Layout::new(state),
            title,
            text,
            kind,
            back: back.map(poll_path),
        },
    )
}

fn server_error(state: &AppState, error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, "bread vote");
    pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
}

fn moment(state: &AppState, at: Timestamp) -> Result<String, I18nError> {
    let zone = Amsterdam::new();
    let mut args = FluentArgs::new();
    args.set("date", state.i18n.long_date(zone.date(at)));
    args.set("time", zone.time(at));
    state.i18n.t("calendar-deadline-at", Some(&args))
}

/// "Stemmen kan tot …" for an open poll, "Gesloten op …" for a closed one.
fn when(state: &AppState, closes_at: Timestamp, now: Timestamp) -> Result<String, I18nError> {
    let id = if now < closes_at {
        "votes-open-until"
    } else {
        "votes-closed-at"
    };
    let mut args = FluentArgs::new();
    args.set("at", moment(state, closes_at)?);
    state.i18n.t(id, Some(&args))
}

async fn index(State(state): State<AppState>) -> Response {
    let now = state.clock.now();
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => return server_error(&state, &error),
    };
    let summaries = match polls::list_public(&mut conn, now).await {
        Ok(found) => found,
        Err(error) => return server_error(&state, &error),
    };
    let links: Result<Vec<PollLink>, I18nError> = summaries
        .into_iter()
        .map(|s| {
            Ok(PollLink {
                when: when(&state, s.closes_at, now)?,
                title: s.title,
                href: poll_path(s.id),
            })
        })
        .collect();
    match links {
        Ok(polls) => pages::render(
            &state,
            StatusCode::OK,
            &IndexPage {
                layout: Layout::new(&state),
                polls,
            },
        ),
        Err(error) => server_error(&state, &error),
    }
}

/// A poll the site may show, or `None` for an unknown id.
async fn load(state: &AppState, raw: &str) -> Result<Option<Poll>, PollError> {
    let Ok(id) = raw.parse::<i64>() else {
        return Ok(None);
    };
    let mut conn = state.pool.acquire().await?;
    match polls::get(&mut conn, PollId::new(id)).await {
        Ok(poll) => Ok(Some(poll)),
        Err(PollError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

struct Filled {
    choice: String,
    email: String,
    wish: String,
    errors: VoteErrors,
}

impl Filled {
    fn empty() -> Self {
        Self {
            choice: String::new(),
            email: String::new(),
            wish: String::new(),
            errors: VoteErrors::default(),
        }
    }
}

async fn render_poll(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    poll: &Poll,
    filled: Filled,
) -> Response {
    let now = state.clock.now();
    let token = match csrf::token(session).await {
        Ok(token) => token,
        Err(error) => return server_error(state, &error),
    };
    let signed_in = match customer_session::current(state, session).await {
        Ok(me) => me.is_some(),
        Err(error) => return server_error(state, &error),
    };
    let chart = if poll.public_results {
        let mut conn = match state.pool.acquire().await {
            Ok(conn) => conn,
            Err(error) => return server_error(state, &error),
        };
        let tallies = match polls::results(&mut conn, poll.id).await {
            Ok(tallies) => tallies,
            Err(error) => return server_error(state, &error),
        };
        let caption = match state.i18n.t("votes-results-caption", None) {
            Ok(caption) => caption,
            Err(error) => return server_error(state, &error),
        };
        Some(BarChart::new(
            "uitslag",
            caption,
            tallies
                .into_iter()
                .map(|t| (t.name, t.votes, t.votes.to_string()))
                .collect(),
        ))
    } else {
        None
    };
    let texts = (|| -> Result<(String, String), I18nError> {
        let drop = match poll.bake_date {
            Some(date) => {
                let mut args = FluentArgs::new();
                args.set("date", state.i18n.long_date(date));
                state.i18n.t("votes-for-drop", Some(&args))?
            }
            None => String::new(),
        };
        Ok((drop, when(state, poll.closes_at, now)?))
    })();
    let (drop, when) = match texts {
        Ok(texts) => texts,
        Err(error) => return server_error(state, &error),
    };
    let options = poll
        .options
        .iter()
        .map(|o| SelectOption {
            value: o.id.get().to_string(),
            label: o.name.clone(),
            selected: o.id.get().to_string() == filled.choice,
        })
        .collect();
    pages::render(
        state,
        status,
        &PollPage {
            layout: Layout::new(state).with_csrf_token(token),
            title: poll.title.clone(),
            drop,
            when,
            open: poll.is_open(now),
            action: poll_path(poll.id),
            options,
            signed_in,
            email: filled.email,
            wish: filled.wish,
            errors: filled.errors,
            chart,
        },
    )
}

async fn page(State(state): State<AppState>, session: Session, Path(id): Path<String>) -> Response {
    match load(&state, &id).await {
        Ok(Some(poll)) => {
            render_poll(&state, &session, StatusCode::OK, &poll, Filled::empty()).await
        }
        Ok(None) => pages::error(&state, StatusCode::NOT_FOUND),
        Err(error) => server_error(&state, &error),
    }
}

/// Every well-formed vote from a visitor who is not signed in gets the same answer,
/// whether the address is new, waiting for confirmation or already voted.
async fn vote(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(input): Form<VoteForm>,
) -> Response {
    let poll = match load(&state, &id).await {
        Ok(Some(poll)) => poll,
        Ok(None) => return pages::error(&state, StatusCode::NOT_FOUND),
        Err(error) => return server_error(&state, &error),
    };
    let me = match customer_session::current(&state, &session).await {
        Ok(me) => me,
        Err(error) => return server_error(&state, &error),
    };
    let option = input
        .keuze
        .trim()
        .parse::<i64>()
        .ok()
        .map(PollOptionId::new);
    let result = match option {
        None => Err(PollError::Choice),
        Some(option) => {
            let ballot = Ballot {
                option,
                wish: &input.wens,
            };
            cast(
                &state,
                &poll,
                ballot,
                me.as_ref().map(|me| me.email.as_str()),
                &input.email,
            )
            .await
        }
    };
    let error_text = |id: &str| state.i18n.t(id, None);
    let errors = match result {
        Ok(Voted::Counted) => {
            return message(&state, StatusCode::OK, COUNTED, "success", Some(poll.id));
        }
        Ok(Voted::AlreadyVoted) if me.is_some() => {
            return message(&state, StatusCode::OK, ALREADY, "info", Some(poll.id));
        }
        Ok(_) => return message(&state, StatusCode::OK, CHECK_MAIL, "info", Some(poll.id)),
        Err(PollError::Closed) => {
            return message(
                &state,
                StatusCode::CONFLICT,
                CLOSED,
                "warning",
                Some(poll.id),
            );
        }
        Err(PollError::NotFound) => return pages::error(&state, StatusCode::NOT_FOUND),
        Err(PollError::Choice) => error_text("votes-error-choice").map(|choice| VoteErrors {
            choice,
            ..VoteErrors::default()
        }),
        Err(PollError::Address) => error_text("votes-error-email").map(|email| VoteErrors {
            email,
            ..VoteErrors::default()
        }),
        Err(PollError::Wish) => error_text("votes-error-wish").map(|wish| VoteErrors {
            wish,
            ..VoteErrors::default()
        }),
        Err(error) => return server_error(&state, &error),
    };
    match errors {
        Ok(errors) => {
            let filled = Filled {
                choice: input.keuze,
                email: input.email,
                wish: input.wens,
                errors,
            };
            render_poll(
                &state,
                &session,
                StatusCode::UNPROCESSABLE_ENTITY,
                &poll,
                filled,
            )
            .await
        }
        Err(error) => server_error(&state, &error),
    }
}

/// A signed-in customer's vote counts at once with their own address; anyone else gets a
/// confirmation link sent to the address they typed.
async fn cast(
    state: &AppState,
    poll: &Poll,
    ballot: Ballot<'_>,
    signed_in: Option<&str>,
    typed: &str,
) -> Result<Voted, PollError> {
    let mut conn = state.pool.acquire().await?;
    if let Some(email) = signed_in {
        return polls::vote_verified(&mut conn, state.clock.as_ref(), poll.id, ballot, email).await;
    }
    let company = notify::Company::load(&mut conn).await?;
    let composer = Composer::new(&state.i18n, &company);
    let links = Links::new(&state.config.public_url);
    polls::vote(
        &mut conn,
        state.clock.as_ref(),
        &composer,
        &links,
        poll.id,
        ballot,
        typed,
    )
    .await
}

/// Shows a button: mail scanners that open the link do not cast the vote.
async fn confirm_page(
    State(state): State<AppState>,
    session: Session,
    Path(token): Path<String>,
) -> Response {
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => return server_error(&state, &error),
    };
    match polls::confirm_link(&mut conn, state.clock.as_ref(), &token).await {
        Ok(Link::Open { choice, wish }) => {
            let csrf = match csrf::token(&session).await {
                Ok(csrf) => csrf,
                Err(error) => return server_error(&state, &error),
            };
            let mut args = FluentArgs::new();
            args.set("choice", choice);
            let choice = match state.i18n.t("votes-confirm-choice", Some(&args)) {
                Ok(text) => text,
                Err(error) => return server_error(&state, &error),
            };
            no_store(pages::render(
                &state,
                StatusCode::OK,
                &ConfirmPage {
                    layout: Layout::new(&state).with_csrf_token(csrf),
                    action: format!("/stemmen/bevestig/{token}"),
                    choice,
                    wish,
                },
            ))
        }
        Ok(Link::Closed(poll)) => no_store(message(
            &state,
            StatusCode::CONFLICT,
            CLOSED,
            "warning",
            Some(poll),
        )),
        Ok(Link::Unknown) => no_store(message(
            &state,
            StatusCode::NOT_FOUND,
            LINK_FAILED,
            "warning",
            None,
        )),
        Err(error) => server_error(&state, &error),
    }
}

async fn confirm(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => return server_error(&state, &error),
    };
    let page = match polls::confirm(&mut conn, state.clock.as_ref(), &token).await {
        Ok(Confirmed::Counted(poll)) => {
            message(&state, StatusCode::OK, COUNTED, "success", Some(poll))
        }
        Ok(Confirmed::Closed(poll)) => {
            message(&state, StatusCode::CONFLICT, CLOSED, "warning", Some(poll))
        }
        Ok(Confirmed::Unknown) => {
            message(&state, StatusCode::NOT_FOUND, LINK_FAILED, "warning", None)
        }
        Err(error) => return server_error(&state, &error),
    };
    no_store(page)
}

#[cfg(test)]
mod tests {
    use i18n::Translator;

    use super::*;

    #[test]
    fn every_message_page_text_exists() {
        let i18n = Translator::dutch().expect("Dutch texts");
        for (title, text) in [CHECK_MAIL, COUNTED, ALREADY, CLOSED, LINK_FAILED] {
            i18n.t(title, None).expect(title);
            i18n.t(text, None).expect(text);
        }
    }
}
