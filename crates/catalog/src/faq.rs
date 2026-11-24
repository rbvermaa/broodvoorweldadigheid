//! Questions and answers (F4e), shown on /faq and used by the customer assistant (F23).

use domain::Clock;
use jiff::Timestamp;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::markdown::{self, Block};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FaqId(i64);

impl FaqId {
    #[must_use]
    pub const fn new(id: i64) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FaqInput {
    pub question: String,
    pub answer_md: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Faq {
    pub id: FaqId,
    pub question: String,
    pub answer_md: String,
    pub position: i32,
    pub published: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Faq {
    #[must_use]
    pub fn answer(&self) -> Vec<Block> {
        markdown::parse(&self.answer_md)
    }
}

struct Row {
    id: i64,
    question: String,
    answer_md: String,
    position: i32,
    published: bool,
    created_at_us: i64,
    updated_at_us: i64,
}

impl TryFrom<Row> for Faq {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: FaqId(row.id),
            question: row.question,
            answer_md: row.answer_md,
            position: row.position,
            published: row.published,
            created_at: db::time::from_micros(row.created_at_us)?,
            updated_at: db::time::from_micros(row.updated_at_us)?,
        })
    }
}

fn validate(input: &FaqInput) -> Result<(), Error> {
    if input.question.trim().is_empty() {
        return Err(Error::Invalid("question"));
    }
    if input.answer_md.trim().is_empty() {
        return Err(Error::Invalid("answer"));
    }
    Ok(())
}

/// Adds an unpublished entry after the last one.
///
/// # Errors
/// [`Error::Invalid`] when the question or answer is empty.
pub async fn create(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    input: &FaqInput,
) -> Result<Faq, Error> {
    validate(input)?;
    let mut tx = conn.begin().await?;
    // Two writers would otherwise read the same max(position).
    sqlx::query("LOCK TABLE faq_entry IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let faq = sqlx::query_as!(
        Row,
        r#"INSERT INTO faq_entry (question, answer_md, position, created_at, updated_at)
        VALUES ($1, $2, (SELECT coalesce(max(position) + 1, 0) FROM faq_entry),
            timestamptz 'epoch' + $3::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $3::bigint * interval '1 microsecond')
        RETURNING id, question, answer_md, position, published,
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_at_us!",
            (extract(epoch FROM updated_at) * 1000000)::bigint AS "updated_at_us!""#,
        input.question.trim(),
        input.answer_md.trim(),
        db::time::to_micros(clock.now()),
    )
    .fetch_one(&mut *tx)
    .await?
    .try_into()?;
    tx.commit().await?;
    Ok(faq)
}

/// # Errors
/// [`Error::NotFound`], or [`Error::Invalid`] when the question or answer is empty.
pub async fn update(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: FaqId,
    input: &FaqInput,
) -> Result<Faq, Error> {
    validate(input)?;
    sqlx::query_as!(
        Row,
        r#"UPDATE faq_entry SET question = $2, answer_md = $3,
            updated_at = timestamptz 'epoch' + $4::bigint * interval '1 microsecond'
        WHERE id = $1
        RETURNING id, question, answer_md, position, published,
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_at_us!",
            (extract(epoch FROM updated_at) * 1000000)::bigint AS "updated_at_us!""#,
        id.get(),
        input.question.trim(),
        input.answer_md.trim(),
        db::time::to_micros(clock.now()),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// # Errors
/// [`Error::NotFound`] when there is no such entry.
pub async fn set_published(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: FaqId,
    published: bool,
) -> Result<Faq, Error> {
    sqlx::query_as!(
        Row,
        r#"UPDATE faq_entry SET published = $2,
            updated_at = timestamptz 'epoch' + $3::bigint * interval '1 microsecond'
        WHERE id = $1
        RETURNING id, question, answer_md, position, published,
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_at_us!",
            (extract(epoch FROM updated_at) * 1000000)::bigint AS "updated_at_us!""#,
        id.get(),
        published,
        db::time::to_micros(clock.now()),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// Puts the entries in the order of `ids`, which must name every entry exactly once.
/// Either every position changes or none does.
///
/// # Errors
/// [`Error::Invalid`]`("order")` when `ids` misses, repeats or invents an entry.
pub async fn reorder(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    ids: &[FaqId],
) -> Result<(), Error> {
    let wanted: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    let updated = sqlx::query!(
        r#"UPDATE faq_entry SET position = o.position - 1,
            updated_at = timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
        FROM unnest($1::bigint[]) WITH ORDINALITY AS o(id, position)
        WHERE faq_entry.id = o.id
            AND (SELECT count(DISTINCT u) FROM unnest($1::bigint[]) AS u) = cardinality($1::bigint[])
            AND (SELECT count(*) FROM faq_entry) = cardinality($1::bigint[])
            AND (SELECT count(*) FROM faq_entry WHERE id = ANY($1::bigint[])) = cardinality($1::bigint[])"#,
        &wanted,
        db::time::to_micros(clock.now()),
    )
    .execute(conn)
    .await?
    .rows_affected();
    if !wanted.is_empty() && usize::try_from(updated).ok() == Some(wanted.len()) {
        Ok(())
    } else {
        Err(Error::Invalid("order"))
    }
}

/// # Errors
/// [`Error::NotFound`] when there is no such entry.
pub async fn get(conn: &mut PgConnection, id: FaqId) -> Result<Faq, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, question, answer_md, position, published,
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_at_us!",
            (extract(epoch FROM updated_at) * 1000000)::bigint AS "updated_at_us!"
        FROM faq_entry WHERE id = $1"#,
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// What the site and the assistant show, in order.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_published(conn: &mut PgConnection) -> Result<Vec<Faq>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, question, answer_md, position, published,
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_at_us!",
            (extract(epoch FROM updated_at) * 1000000)::bigint AS "updated_at_us!"
        FROM faq_entry WHERE published ORDER BY position"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Faq::try_from)
    .collect()
}

/// Every entry, unpublished ones included, in order; for the back office.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_all(conn: &mut PgConnection) -> Result<Vec<Faq>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, question, answer_md, position, published,
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_at_us!",
            (extract(epoch FROM updated_at) * 1000000)::bigint AS "updated_at_us!"
        FROM faq_entry ORDER BY position"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Faq::try_from)
    .collect()
}
