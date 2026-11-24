use domain::{CharityId, Clock};
use jiff::Timestamp;
use sqlx::PgConnection;

use crate::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharityInput {
    pub name: String,
    pub description: String,
    pub url: String,
    pub logo_path: Option<String>,
    pub iban: Option<String>,
    pub anbi_rsin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Charity {
    pub id: CharityId,
    pub name: String,
    pub description: String,
    pub url: String,
    pub logo_path: Option<String>,
    pub iban: Option<String>,
    pub anbi_rsin: Option<String>,
    pub archived_at: Option<Timestamp>,
}

impl Charity {
    /// Only active charities can be chosen by customers (F4g).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.archived_at.is_none()
    }
}

struct Row {
    id: i64,
    name: String,
    description: String,
    url: String,
    logo_path: Option<String>,
    iban: Option<String>,
    anbi_rsin: Option<String>,
    archived_at_us: Option<i64>,
}

impl TryFrom<Row> for Charity {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: CharityId::new(row.id),
            name: row.name,
            description: row.description,
            url: row.url,
            logo_path: row.logo_path,
            iban: row.iban,
            anbi_rsin: row.anbi_rsin,
            archived_at: row.archived_at_us.map(db::time::from_micros).transpose()?,
        })
    }
}

fn validate(input: &CharityInput) -> Result<(), Error> {
    if input.name.trim().is_empty() {
        return Err(Error::Invalid("name"));
    }
    if input.url.trim().is_empty() {
        return Err(Error::Invalid("url"));
    }
    Ok(())
}

/// # Errors
/// [`Error::Invalid`] when the name is empty.
pub async fn create(conn: &mut PgConnection, input: &CharityInput) -> Result<Charity, Error> {
    validate(input)?;
    sqlx::query_as!(
        Row,
        r#"INSERT INTO charity (name, description, url, logo_path, iban, anbi_rsin)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, name, description, url, logo_path, iban, anbi_rsin,
            (extract(epoch FROM archived_at) * 1000000)::bigint AS archived_at_us"#,
        input.name.trim(),
        input.description,
        input.url.trim(),
        input.logo_path,
        input.iban,
        input.anbi_rsin,
    )
    .fetch_one(conn)
    .await?
    .try_into()
}

/// Archived charities can be edited and stay archived.
///
/// # Errors
/// [`Error::NotFound`], or [`Error::Invalid`] when the name is empty.
pub async fn update(
    conn: &mut PgConnection,
    id: CharityId,
    input: &CharityInput,
) -> Result<Charity, Error> {
    validate(input)?;
    sqlx::query_as!(
        Row,
        r#"UPDATE charity SET name = $2, description = $3, url = $4, logo_path = $5,
            iban = $6, anbi_rsin = $7
        WHERE id = $1
        RETURNING id, name, description, url, logo_path, iban, anbi_rsin,
            (extract(epoch FROM archived_at) * 1000000)::bigint AS archived_at_us"#,
        id.get(),
        input.name.trim(),
        input.description,
        input.url.trim(),
        input.logo_path,
        input.iban,
        input.anbi_rsin,
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// Sets `archived_at`; the row is never deleted. Archiving again keeps the first time.
///
/// # Errors
/// [`Error::NotFound`] when there is no such charity.
pub async fn archive(
    conn: &mut PgConnection,
    id: CharityId,
    clock: &dyn Clock,
) -> Result<Charity, Error> {
    sqlx::query_as!(
        Row,
        r#"UPDATE charity SET archived_at = coalesce(
            archived_at, timestamptz 'epoch' + $2::bigint * interval '1 microsecond')
        WHERE id = $1
        RETURNING id, name, description, url, logo_path, iban, anbi_rsin,
            (extract(epoch FROM archived_at) * 1000000)::bigint AS archived_at_us"#,
        id.get(),
        db::time::to_micros(clock.now()),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// # Errors
/// [`Error::NotFound`] when there is no such charity.
pub async fn get(conn: &mut PgConnection, id: CharityId) -> Result<Charity, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name, description, url, logo_path, iban, anbi_rsin,
            (extract(epoch FROM archived_at) * 1000000)::bigint AS archived_at_us
        FROM charity WHERE id = $1"#,
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// Charities customers can choose, by name.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_active(conn: &mut PgConnection) -> Result<Vec<Charity>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name, description, url, logo_path, iban, anbi_rsin,
            (extract(epoch FROM archived_at) * 1000000)::bigint AS archived_at_us
        FROM charity WHERE archived_at IS NULL ORDER BY name, id"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Charity::try_from)
    .collect()
}

/// Every charity, archived ones included, by name.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_all(conn: &mut PgConnection) -> Result<Vec<Charity>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name, description, url, logo_path, iban, anbi_rsin,
            (extract(epoch FROM archived_at) * 1000000)::bigint AS archived_at_us
        FROM charity ORDER BY name, id"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Charity::try_from)
    .collect()
}
