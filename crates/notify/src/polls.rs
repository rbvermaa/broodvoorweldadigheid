//! The bread vote (F9b, ADR 0039): the baker lists candidate breads, visitors vote once
//! per confirmed email address with an optional wish of their own, and a poll closes at
//! its set time. Votes and wishes are personal data: [`of_address`] and [`forget`] serve
//! the data export and erasure of F12.

use domain::{BakeDayId, Clock, PollId, PollOptionId, ProductId};
use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use sqlx::{Acquire, PgConnection};

use crate::compose::{ComposeError, Composer, VoteConfirmation};
use crate::outbox::{self, OutboxError};
use crate::subscribers::{Links, parse_address};
use crate::token::{new_token, token_hash};

/// How long a confirmation link works at most; never past the poll's closing time.
pub const CONFIRM_VALID: SignedDuration = SignedDuration::from_hours(7 * 24);
/// A new confirmation email for the same address and poll goes out at most this often.
pub const RESEND_AFTER: SignedDuration = SignedDuration::from_mins(5);
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_OPTION_CHARS: usize = 100;
pub const MAX_WISH_CHARS: usize = 500;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    Title,
    /// A new poll must close in the future.
    ClosesInPast,
    TooFewOptions,
    TooManyOptions,
    OptionName,
    DuplicateOption,
}

#[derive(Debug, thiserror::Error)]
pub enum PollError {
    #[error("invalid poll: {0:?}")]
    Invalid(Invalid),
    #[error("poll not found")]
    NotFound,
    #[error("poll is closed")]
    Closed,
    #[error("not a candidate of this poll")]
    Choice,
    #[error("not an email address")]
    Address,
    #[error("wish is too long")]
    Wish,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Compose(#[from] ComposeError),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
}

/// What the baker sets on a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollInput {
    pub title: String,
    pub bake_day: Option<BakeDayId>,
    pub closes_at: Timestamp,
    pub public_results: bool,
}

/// A candidate bread: an existing product, or free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOption {
    pub product: Option<ProductId>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollOption {
    pub id: PollOptionId,
    pub product: Option<ProductId>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poll {
    pub id: PollId,
    pub title: String,
    pub bake_day: Option<BakeDayId>,
    pub bake_date: Option<Date>,
    pub closes_at: Timestamp,
    pub public_results: bool,
    pub options: Vec<PollOption>,
}

impl Poll {
    #[must_use]
    pub fn is_open(&self, now: Timestamp) -> bool {
        now < self.closes_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub id: PollId,
    pub title: String,
    pub closes_at: Timestamp,
    pub public_results: bool,
    /// Confirmed votes.
    pub votes: i64,
}

/// The confirmed votes for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally {
    pub option: PollOptionId,
    pub name: String,
    pub votes: i64,
}

/// A visitor's choice and optional wish, as posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ballot<'a> {
    pub option: PollOptionId,
    pub wish: &'a str,
}

/// What a vote did. A visitor who is not signed in sees the same answer for
/// `ConfirmationQueued`, `RecentlySent` and `AlreadyVoted`, so nobody learns whether an
/// address voted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voted {
    ConfirmationQueued,
    /// A confirmation for this address and poll went out less than [`RESEND_AFTER`] ago.
    RecentlySent,
    AlreadyVoted,
    /// Counted at once: the address was proven by signing in.
    Counted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmed {
    Counted(PollId),
    Closed(PollId),
    /// Unknown, expired or already used.
    Unknown,
}

/// One vote of an address, for the data export (F12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteOf {
    pub poll: String,
    pub choice: String,
    pub wish: Option<String>,
    pub voted_at: Timestamp,
    pub confirmed_at: Option<Timestamp>,
}

fn micros(at: Timestamp) -> i64 {
    db::time::to_micros(at)
}

fn at(us: i64) -> Result<Timestamp, PollError> {
    db::time::from_micros(us).map_err(|e| PollError::Corrupt(e.to_string()))
}

fn check_title(title: &str) -> Result<(), PollError> {
    if title.trim().is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(PollError::Invalid(Invalid::Title));
    }
    Ok(())
}

fn check_options(options: &[NewOption]) -> Result<(), PollError> {
    if options.len() < MIN_OPTIONS {
        return Err(PollError::Invalid(Invalid::TooFewOptions));
    }
    if options.len() > MAX_OPTIONS {
        return Err(PollError::Invalid(Invalid::TooManyOptions));
    }
    let mut seen = Vec::with_capacity(options.len());
    for option in options {
        let name = option.name.trim();
        if name.is_empty() || name.chars().count() > MAX_OPTION_CHARS {
            return Err(PollError::Invalid(Invalid::OptionName));
        }
        let key = name.to_lowercase();
        if seen.contains(&key) {
            return Err(PollError::Invalid(Invalid::DuplicateOption));
        }
        seen.push(key);
    }
    Ok(())
}

/// The trimmed wish, `None` when empty.
///
/// # Errors
/// [`PollError::Wish`] when it is longer than [`MAX_WISH_CHARS`].
pub fn parse_wish(raw: &str) -> Result<Option<String>, PollError> {
    let wish = raw.trim();
    if wish.chars().count() > MAX_WISH_CHARS {
        return Err(PollError::Wish);
    }
    Ok((!wish.is_empty()).then(|| wish.to_owned()))
}

/// Creates a poll with its candidates, in the given order.
///
/// # Errors
/// [`PollError::Invalid`] for a bad title, a closing time not in the future, or fewer
/// than [`MIN_OPTIONS`], more than [`MAX_OPTIONS`], empty, too long or repeated
/// candidates; otherwise [`PollError::Db`].
pub async fn create(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    input: &PollInput,
    options: &[NewOption],
) -> Result<PollId, PollError> {
    check_title(&input.title)?;
    check_options(options)?;
    let now = clock.now();
    if input.closes_at <= now {
        return Err(PollError::Invalid(Invalid::ClosesInPast));
    }
    let mut tx = conn.begin().await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO poll (title, bake_day_id, closes_at, public_results, created_at)
        VALUES ($1, $2, timestamptz 'epoch' + $3::bigint * interval '1 microsecond', $4,
            timestamptz 'epoch' + $5::bigint * interval '1 microsecond')
        RETURNING id",
        input.title.trim(),
        input.bake_day.map(BakeDayId::get),
        micros(input.closes_at),
        input.public_results,
        micros(now),
    )
    .fetch_one(&mut *tx)
    .await?;
    for (position, option) in (0_i32..).zip(options) {
        sqlx::query!(
            "INSERT INTO poll_option (poll_id, product_id, name, position)
            VALUES ($1, $2, $3, $4)",
            id,
            option.product.map(ProductId::get),
            option.name.trim(),
            position,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(PollId::new(id))
}

/// Changes title, drop, closing time and whether results are public. The candidates
/// stay as they are. A closing time in the past closes the poll at once.
///
/// # Errors
/// [`PollError::Invalid`] for a bad title, [`PollError::NotFound`] or [`PollError::Db`].
pub async fn update(
    conn: &mut PgConnection,
    id: PollId,
    input: &PollInput,
) -> Result<(), PollError> {
    check_title(&input.title)?;
    let updated = sqlx::query!(
        "UPDATE poll SET title = $2, bake_day_id = $3,
            closes_at = timestamptz 'epoch' + $4::bigint * interval '1 microsecond',
            public_results = $5
        WHERE id = $1",
        id.get(),
        input.title.trim(),
        input.bake_day.map(BakeDayId::get),
        micros(input.closes_at),
        input.public_results,
    )
    .execute(conn)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(PollError::NotFound);
    }
    Ok(())
}

/// # Errors
/// [`PollError::NotFound`], [`PollError::Db`] or [`PollError::Corrupt`].
pub async fn get(conn: &mut PgConnection, id: PollId) -> Result<Poll, PollError> {
    let row = sqlx::query!(
        r#"SELECT p.id, p.title, p.bake_day_id,
            d.date - date 'epoch' AS bake_date_days,
            (extract(epoch FROM p.closes_at) * 1000000)::bigint AS "closes_us!",
            p.public_results
        FROM poll p LEFT JOIN bake_day d ON d.id = p.bake_day_id
        WHERE p.id = $1"#,
        id.get(),
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(PollError::NotFound)?;
    let options = sqlx::query!(
        "SELECT id, product_id, name FROM poll_option WHERE poll_id = $1 ORDER BY position",
        id.get(),
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|o| PollOption {
        id: PollOptionId::new(o.id),
        product: o.product_id.map(ProductId::new),
        name: o.name,
    })
    .collect();
    let bake_date = row
        .bake_date_days
        .map(|days| db::time::from_epoch_days(days).map_err(|e| PollError::Corrupt(e.to_string())))
        .transpose()?;
    Ok(Poll {
        id: PollId::new(row.id),
        title: row.title,
        bake_day: row.bake_day_id.map(BakeDayId::new),
        bake_date,
        closes_at: at(row.closes_us)?,
        public_results: row.public_results,
        options,
    })
}

async fn summaries(
    conn: &mut PgConnection,
    public_from: Option<Timestamp>,
) -> Result<Vec<Summary>, PollError> {
    sqlx::query!(
        r#"SELECT p.id, p.title,
            (extract(epoch FROM p.closes_at) * 1000000)::bigint AS "closes_us!",
            p.public_results,
            (SELECT count(*) FROM vote v WHERE v.poll_id = p.id AND v.confirmed_at IS NOT NULL)
                AS "votes!"
        FROM poll p
        WHERE $1::bigint IS NULL OR p.public_results
            OR p.closes_at > timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
        ORDER BY p.closes_at DESC, p.id DESC"#,
        public_from.map(micros),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(Summary {
            id: PollId::new(row.id),
            title: row.title,
            closes_at: at(row.closes_us)?,
            public_results: row.public_results,
            votes: row.votes,
        })
    })
    .collect()
}

/// Every poll, the latest closing first, for the back office.
///
/// # Errors
/// [`PollError::Db`] or [`PollError::Corrupt`].
pub async fn list(conn: &mut PgConnection) -> Result<Vec<Summary>, PollError> {
    summaries(conn, None).await
}

/// The polls the site shows: open ones, and closed ones with public results.
///
/// # Errors
/// [`PollError::Db`] or [`PollError::Corrupt`].
pub async fn list_public(
    conn: &mut PgConnection,
    now: Timestamp,
) -> Result<Vec<Summary>, PollError> {
    summaries(conn, Some(now)).await
}

/// Confirmed votes per candidate, in the poll's order. Unconfirmed votes never count.
///
/// # Errors
/// [`PollError::Db`].
pub async fn results(conn: &mut PgConnection, id: PollId) -> Result<Vec<Tally>, PollError> {
    Ok(sqlx::query!(
        r#"SELECT o.id, o.name,
            count(v.id) FILTER (WHERE v.confirmed_at IS NOT NULL) AS "votes!"
        FROM poll_option o LEFT JOIN vote v ON v.poll_option_id = o.id
        WHERE o.poll_id = $1
        GROUP BY o.id ORDER BY o.position"#,
        id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| Tally {
        option: PollOptionId::new(row.id),
        name: row.name,
        votes: row.votes,
    })
    .collect())
}

/// The wishes of confirmed votes, oldest first, without who sent them. Staff only.
///
/// # Errors
/// [`PollError::Db`].
pub async fn wishes(conn: &mut PgConnection, id: PollId) -> Result<Vec<String>, PollError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT wish AS "wish!" FROM vote
        WHERE poll_id = $1 AND confirmed_at IS NOT NULL AND wish IS NOT NULL
        ORDER BY confirmed_at, id"#,
        id.get(),
    )
    .fetch_all(conn)
    .await?)
}

/// Locks the poll for the rest of the transaction and checks that it is open and that
/// `option` is one of its candidates. Returns the poll's title, the candidate's name
/// and the closing time.
async fn open_choice(
    conn: &mut PgConnection,
    now: Timestamp,
    poll: PollId,
    option: PollOptionId,
) -> Result<(String, String, Timestamp), PollError> {
    let row = sqlx::query!(
        r#"SELECT title, (extract(epoch FROM closes_at) * 1000000)::bigint AS "closes_us!"
        FROM poll WHERE id = $1 FOR SHARE"#,
        poll.get(),
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(PollError::NotFound)?;
    let closes_at = at(row.closes_us)?;
    if now >= closes_at {
        return Err(PollError::Closed);
    }
    let choice = sqlx::query_scalar!(
        "SELECT name FROM poll_option WHERE poll_id = $1 AND id = $2",
        poll.get(),
        option.get(),
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(PollError::Choice)?;
    Ok((row.title, choice, closes_at))
}

/// Records a vote that counts once its address is confirmed, and queues the
/// confirmation email. An address that already voted in this poll, or got a confirmation
/// less than [`RESEND_AFTER`] ago, gets no email and its vote stays as it is. A new vote
/// while one waits for confirmation replaces it and voids the old link.
///
/// # Errors
/// [`PollError::Address`], [`PollError::Wish`], [`PollError::NotFound`],
/// [`PollError::Closed`] or [`PollError::Choice`]; otherwise database, compose or outbox
/// errors.
pub async fn vote(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    composer: &Composer<'_>,
    links: &Links,
    poll: PollId,
    ballot: Ballot<'_>,
    raw_email: &str,
) -> Result<Voted, PollError> {
    let email = parse_address(raw_email).map_err(|_| PollError::Address)?;
    let wish = parse_wish(ballot.wish)?;
    let now = clock.now();
    let mut tx = conn.begin().await?;
    let (title, choice, closes_at) = open_choice(&mut tx, now, poll, ballot.option).await?;
    let row = existing_or_new(&mut tx, now, poll, ballot.option, &email, wish.as_deref()).await?;
    if row.confirmed {
        return Ok(Voted::AlreadyVoted);
    }
    let recent = now.checked_sub(RESEND_AFTER).map_or(0, micros);
    if row.sent_at_us.is_some_and(|sent| sent > recent) {
        return Ok(Voted::RecentlySent);
    }
    let (token, hash) = new_token();
    let expires = now
        .checked_add(CONFIRM_VALID)
        .unwrap_or(closes_at)
        .min(closes_at);
    sqlx::query!(
        "UPDATE vote SET poll_option_id = $2, wish = $3, confirm_token_hash = $4,
            confirm_expires_at = timestamptz 'epoch' + $5::bigint * interval '1 microsecond',
            confirm_sent_at = timestamptz 'epoch' + $6::bigint * interval '1 microsecond'
        WHERE id = $1",
        row.id,
        ballot.option.get(),
        wish,
        hash,
        micros(expires),
        micros(now),
    )
    .execute(&mut *tx)
    .await?;
    let mail = composer.vote_confirmation(&VoteConfirmation {
        to: row.email,
        poll: title,
        choice,
        confirm_url: links.vote_confirm(&token),
    })?;
    outbox::enqueue_expiring(&mut tx, clock, &mail, None, expires).await?;
    tx.commit().await?;
    Ok(Voted::ConfirmationQueued)
}

/// Records a vote that counts at once, for a customer who proved `email` by signing in
/// with a link sent to it. A vote of the address still waiting for confirmation is
/// replaced and counts.
///
/// # Errors
/// As [`vote`], without compose or outbox errors.
pub async fn vote_verified(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    poll: PollId,
    ballot: Ballot<'_>,
    email: &str,
) -> Result<Voted, PollError> {
    let email = parse_address(email).map_err(|_| PollError::Address)?;
    let wish = parse_wish(ballot.wish)?;
    let now = clock.now();
    let mut tx = conn.begin().await?;
    open_choice(&mut tx, now, poll, ballot.option).await?;
    let row = existing_or_new(&mut tx, now, poll, ballot.option, &email, wish.as_deref()).await?;
    if row.confirmed {
        return Ok(Voted::AlreadyVoted);
    }
    sqlx::query!(
        "UPDATE vote SET poll_option_id = $2, wish = $3,
            confirmed_at = timestamptz 'epoch' + $4::bigint * interval '1 microsecond',
            confirm_token_hash = NULL, confirm_expires_at = NULL
        WHERE id = $1",
        row.id,
        ballot.option.get(),
        wish,
        micros(now),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Voted::Counted)
}

struct Existing {
    id: i64,
    email: String,
    confirmed: bool,
    sent_at_us: Option<i64>,
}

/// The vote of `email` in `poll`, locked; a new unconfirmed one if there is none.
async fn existing_or_new(
    conn: &mut PgConnection,
    now: Timestamp,
    poll: PollId,
    option: PollOptionId,
    email: &str,
    wish: Option<&str>,
) -> Result<Existing, PollError> {
    sqlx::query!(
        "INSERT INTO vote (poll_id, poll_option_id, email, wish, created_at)
        VALUES ($1, $2, $3, $4, timestamptz 'epoch' + $5::bigint * interval '1 microsecond')
        ON CONFLICT (poll_id, lower(email)) DO NOTHING",
        poll.get(),
        option.get(),
        email,
        wish,
        micros(now),
    )
    .execute(&mut *conn)
    .await?;
    let row = sqlx::query!(
        r#"SELECT id, email AS "email!", confirmed_at IS NOT NULL AS "confirmed!",
            (extract(epoch FROM confirm_sent_at) * 1000000)::bigint AS sent_at_us
        FROM vote WHERE poll_id = $1 AND lower(email) = lower($2) FOR UPDATE"#,
        poll.get(),
        email,
    )
    .fetch_one(conn)
    .await?;
    Ok(Existing {
        id: row.id,
        email: row.email,
        confirmed: row.confirmed,
        sent_at_us: row.sent_at_us,
    })
}

/// Whether `token` is an unexpired confirmation link, for the page that asks to confirm.
///
/// # Errors
/// [`PollError::Db`].
pub async fn confirm_link_known(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    token: &str,
) -> Result<bool, PollError> {
    let Some(hash) = token_hash(token) else {
        return Ok(false);
    };
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM vote WHERE confirm_token_hash = $1
            AND confirm_expires_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond')
            AS "known!""#,
        hash,
        micros(clock.now()),
    )
    .fetch_one(conn)
    .await?)
}

/// Makes the vote behind an unexpired confirmation link count, while its poll is open.
/// The link works once.
///
/// # Errors
/// [`PollError::Db`] or [`PollError::Corrupt`].
pub async fn confirm(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    token: &str,
) -> Result<Confirmed, PollError> {
    let Some(hash) = token_hash(token) else {
        return Ok(Confirmed::Unknown);
    };
    let now = micros(clock.now());
    let mut tx = conn.begin().await?;
    let Some(row) = sqlx::query!(
        r#"SELECT v.id, v.poll_id,
            p.closes_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond' AS "open!"
        FROM vote v JOIN poll p ON p.id = v.poll_id
        WHERE v.confirm_token_hash = $1
            AND v.confirm_expires_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
        FOR UPDATE OF v FOR SHARE OF p"#,
        hash,
        now,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Confirmed::Unknown);
    };
    let poll = PollId::new(row.poll_id);
    if !row.open {
        return Ok(Confirmed::Closed(poll));
    }
    sqlx::query!(
        "UPDATE vote SET confirmed_at = timestamptz 'epoch' + $2::bigint * interval '1 microsecond',
            confirm_token_hash = NULL, confirm_expires_at = NULL
        WHERE id = $1",
        row.id,
        now,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Confirmed::Counted(poll))
}

/// Every vote of `email`, however it is capitalised, for the data export (F12).
///
/// # Errors
/// [`PollError::Db`] or [`PollError::Corrupt`].
pub async fn of_address(conn: &mut PgConnection, email: &str) -> Result<Vec<VoteOf>, PollError> {
    sqlx::query!(
        r#"SELECT p.title, o.name, v.wish,
            (extract(epoch FROM v.created_at) * 1000000)::bigint AS "created_us!",
            (extract(epoch FROM v.confirmed_at) * 1000000)::bigint AS confirmed_us
        FROM vote v JOIN poll p ON p.id = v.poll_id JOIN poll_option o ON o.id = v.poll_option_id
        WHERE lower(v.email) = lower($1)
        ORDER BY v.created_at, v.id"#,
        email,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(VoteOf {
            poll: row.title,
            choice: row.name,
            wish: row.wish,
            voted_at: at(row.created_us)?,
            confirmed_at: row.confirmed_us.map(at).transpose()?,
        })
    })
    .collect()
}

/// Erases `email` from the votes (F12): unconfirmed votes are deleted; confirmed ones
/// keep counting, without the address or the wish.
///
/// # Errors
/// [`PollError::Db`].
pub async fn forget(conn: &mut PgConnection, email: &str) -> Result<(), PollError> {
    let mut tx = conn.begin().await?;
    sqlx::query!(
        "DELETE FROM vote WHERE lower(email) = lower($1) AND confirmed_at IS NULL",
        email,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE vote SET email = NULL, wish = NULL, confirm_token_hash = NULL,
            confirm_expires_at = NULL, confirm_sent_at = NULL
        WHERE lower(email) = lower($1)",
        email,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes the votes whose confirmation link ran out unused; their address and wish go
/// with them. Returns how many.
///
/// # Errors
/// [`PollError::Db`].
pub async fn delete_expired(conn: &mut PgConnection, clock: &dyn Clock) -> Result<u64, PollError> {
    Ok(sqlx::query!(
        "DELETE FROM vote WHERE confirmed_at IS NULL
            AND (confirm_expires_at IS NULL
                OR confirm_expires_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond')",
        micros(clock.now()),
    )
    .execute(conn)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(name: &str) -> NewOption {
        NewOption {
            product: None,
            name: name.to_owned(),
        }
    }

    #[test]
    fn wishes_are_trimmed_and_limited() {
        assert_eq!(parse_wish("  ").unwrap(), None);
        assert_eq!(parse_wish(" Spelt ").unwrap(), Some("Spelt".to_owned()));
        assert!(parse_wish(&"é".repeat(MAX_WISH_CHARS)).is_ok());
        assert!(matches!(
            parse_wish(&"a".repeat(MAX_WISH_CHARS + 1)),
            Err(PollError::Wish)
        ));
    }

    #[test]
    fn candidates_are_checked() {
        let two = [option("Spelt"), option("Rogge")];
        assert!(check_options(&two).is_ok());
        for (bad, expected) in [
            (vec![option("Spelt")], Invalid::TooFewOptions),
            (vec![option("Spelt"), option(" ")], Invalid::OptionName),
            (
                vec![option("Spelt"), option("spelt ")],
                Invalid::DuplicateOption,
            ),
            (
                (0..=MAX_OPTIONS).map(|n| option(&n.to_string())).collect(),
                Invalid::TooManyOptions,
            ),
            (
                vec![option("Spelt"), option(&"a".repeat(MAX_OPTION_CHARS + 1))],
                Invalid::OptionName,
            ),
        ] {
            assert!(
                matches!(check_options(&bad), Err(PollError::Invalid(found)) if found == expected),
                "{expected:?}"
            );
        }
        assert!(check_title("Welk brood?").is_ok());
        assert!(check_title(" ").is_err());
        assert!(check_title(&"a".repeat(MAX_TITLE_CHARS + 1)).is_err());
    }
}
