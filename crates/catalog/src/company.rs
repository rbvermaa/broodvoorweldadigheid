use db::time::from_seconds_of_day;
use jiff::civil::Time;
use sqlx::PgConnection;

use crate::Error;

/// The latest order deadline the sourdough allows: `days_before` days before the bake
/// date, at `at` local time (F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourdoughDeadline {
    pub days_before: i16,
    pub at: Time,
}

/// The bakery's sourdough deadline rule from `company`.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn sourdough_deadline(conn: &mut PgConnection) -> Result<SourdoughDeadline, Error> {
    let row = sqlx::query!(
        r#"SELECT sourdough_deadline_days_before AS days_before,
            extract(epoch FROM sourdough_deadline_time)::integer AS "at_s!"
        FROM company"#,
    )
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| Error::Corrupt("no company row".to_owned()))?;
    Ok(SourdoughDeadline {
        days_before: row.days_before,
        at: from_seconds_of_day(row.at_s)?,
    })
}

/// The trade name, address and KvK number for emails and receipts (Compliance). Address
/// and KvK number are `None` until the owner fills them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanyContact {
    pub name: String,
    pub address: Option<String>,
    pub kvk: Option<String>,
}

/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn contact(conn: &mut PgConnection) -> Result<CompanyContact, Error> {
    let row = sqlx::query!("SELECT name, address, kvk FROM company")
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| Error::Corrupt("no company row".to_owned()))?;
    let filled = |value: Option<String>| value.filter(|v| !v.trim().is_empty());
    Ok(CompanyContact {
        name: row.name,
        address: filled(row.address),
        kvk: filled(row.kvk),
    })
}
