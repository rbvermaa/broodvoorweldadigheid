use db::time::to_micros;
use domain::{CustomerId, StaffUserId};
use jiff::Timestamp;
use serde_json::Value;
use sqlx::PgConnection;

use crate::Error;

/// Who made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Customer(CustomerId),
    Staff(StaffUserId),
    /// A job or webhook, by name.
    System(&'static str),
}

impl Actor {
    /// `customer:12`, `staff:3`, `system:hold-expiry`
    #[must_use]
    pub fn to_db(self) -> String {
        match self {
            Self::Customer(id) => format!("customer:{}", id.get()),
            Self::Staff(id) => format!("staff:{}", id.get()),
            Self::System(name) => format!("system:{name}"),
        }
    }
}

/// One change to record: an entity, what happened, and its state before and after.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<'a> {
    pub entity: &'a str,
    pub entity_id: i64,
    pub action: &'a str,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub actor: Actor,
    pub at: Timestamp,
}

/// Appends an entry to the audit log; the table refuses updates and deletes. Call it in
/// the same transaction as the change.
///
/// # Errors
/// [`Error::Db`].
pub async fn record(conn: &mut PgConnection, entry: &Entry<'_>) -> Result<(), Error> {
    sqlx::query!(
        "INSERT INTO audit_log (entity, entity_id, action, before, after, actor, at)
        VALUES ($1, $2, $3, $4::text::jsonb, $5::text::jsonb, $6,
            timestamptz 'epoch' + $7::bigint * interval '1 microsecond')",
        entry.entity,
        entry.entity_id,
        entry.action,
        entry.before.as_ref().map(Value::to_string),
        entry.after.as_ref().map(Value::to_string),
        entry.actor.to_db(),
        to_micros(entry.at),
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// An audit entry as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logged {
    pub action: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub actor: String,
    pub at: Timestamp,
}

/// The audit entries of one entity, oldest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn history(
    conn: &mut PgConnection,
    entity: &str,
    entity_id: i64,
) -> Result<Vec<Logged>, Error> {
    sqlx::query!(
        r#"SELECT action, before::text AS before, after::text AS after, actor,
            (extract(epoch FROM at) * 1000000)::bigint AS "at_us!"
        FROM audit_log WHERE entity = $1 AND entity_id = $2 ORDER BY id"#,
        entity,
        entity_id,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        let json = |text: Option<String>| {
            text.map(|t| serde_json::from_str(&t))
                .transpose()
                .map_err(|e| Error::Corrupt(e.to_string()))
        };
        Ok(Logged {
            action: row.action,
            before: json(row.before)?,
            after: json(row.after)?,
            actor: row.actor,
            at: db::time::from_micros(row.at_us)?,
        })
    })
    .collect()
}
