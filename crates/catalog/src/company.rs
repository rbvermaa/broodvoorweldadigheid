use std::collections::BTreeSet;

use db::time::from_seconds_of_day;
use domain::{Allergen, Clock, StaffUserId};
use jiff::civil::Time;
use sqlx::PgConnection;

use crate::Error;
use crate::products::{Approval, allergens_from_db, allergens_to_db};

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

/// The kitchen-wide "may contain traces of" list (F10, ADR 0038): allergens used in the
/// kitchen that may end up in any product. Customers see it only once it is approved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KitchenMayContain {
    pub allergens: BTreeSet<Allergen>,
    pub approval: Option<Approval>,
}

struct KitchenRow {
    may_contain: Vec<String>,
    approved_by: Option<i64>,
    approved_at_us: Option<i64>,
}

impl TryFrom<KitchenRow> for KitchenMayContain {
    type Error = Error;

    fn try_from(row: KitchenRow) -> Result<Self, Error> {
        let approval = match (row.approved_by, row.approved_at_us) {
            (Some(by), Some(at)) => Some(Approval {
                by: StaffUserId::new(by),
                at: db::time::from_micros(at)?,
            }),
            _ => None,
        };
        Ok(Self {
            allergens: allergens_from_db(&row.may_contain)?,
            approval,
        })
    }
}

/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn kitchen_may_contain(conn: &mut PgConnection) -> Result<KitchenMayContain, Error> {
    sqlx::query_as!(
        KitchenRow,
        r#"SELECT may_contain, may_contain_approved_by AS approved_by,
            (extract(epoch FROM may_contain_approved_at) * 1000000)::bigint AS approved_at_us
        FROM company"#,
    )
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| Error::Corrupt("no company row".to_owned()))?
    .try_into()
}

/// [`kitchen_may_contain`], with the company row locked `FOR SHARE` until the caller's
/// transaction ends, so the list and its approval cannot change between a checkout's check
/// and its commit. A checkout takes it after its product locks (ADR 0008, ADR 0038).
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn lock_kitchen_may_contain(conn: &mut PgConnection) -> Result<KitchenMayContain, Error> {
    sqlx::query_as!(
        KitchenRow,
        r#"SELECT may_contain, may_contain_approved_by AS approved_by,
            (extract(epoch FROM may_contain_approved_at) * 1000000)::bigint AS approved_at_us
        FROM company FOR SHARE"#,
    )
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| Error::Corrupt("no company row".to_owned()))?
    .try_into()
}

/// Replaces the list. A different set of allergens clears its approval (database
/// trigger); the same set again keeps it.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn set_kitchen_may_contain(
    conn: &mut PgConnection,
    allergens: &BTreeSet<Allergen>,
) -> Result<KitchenMayContain, Error> {
    sqlx::query_as!(
        KitchenRow,
        r#"UPDATE company SET may_contain = $1
        RETURNING may_contain, may_contain_approved_by AS approved_by,
            (extract(epoch FROM may_contain_approved_at) * 1000000)::bigint AS approved_at_us"#,
        &allergens_to_db(allergens),
    )
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| Error::Corrupt("no company row".to_owned()))?
    .try_into()
}

/// Approves the list `reviewed`, which must be exactly the stored list, so a change made
/// while someone was reviewing is never approved unseen.
///
/// # Errors
/// [`Error::AllergensChanged`] when `reviewed` is out of date.
pub async fn approve_kitchen_may_contain(
    conn: &mut PgConnection,
    reviewed: &BTreeSet<Allergen>,
    by: StaffUserId,
    clock: &dyn Clock,
) -> Result<KitchenMayContain, Error> {
    sqlx::query_as!(
        KitchenRow,
        r#"UPDATE company SET
            may_contain_approved_by = $2,
            may_contain_approved_at = timestamptz 'epoch' + $3::bigint * interval '1 microsecond'
        WHERE may_contain @> $1 AND may_contain <@ $1
        RETURNING may_contain, may_contain_approved_by AS approved_by,
            (extract(epoch FROM may_contain_approved_at) * 1000000)::bigint AS approved_at_us"#,
        &allergens_to_db(reviewed),
        by.get(),
        db::time::to_micros(clock.now()),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::AllergensChanged)?
    .try_into()
}

/// Whether the public donation page is on (F11d). Off until the owner switches it on.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn public_donations(conn: &mut PgConnection) -> Result<bool, Error> {
    sqlx::query_scalar!("SELECT public_donations FROM company")
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| Error::Corrupt("no company row".to_owned()))
}

/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn set_public_donations(conn: &mut PgConnection, on: bool) -> Result<(), Error> {
    let updated = sqlx::query!("UPDATE company SET public_donations = $1", on)
        .execute(conn)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(Error::Corrupt("no company row".to_owned()));
    }
    Ok(())
}

/// The longest company address, in characters.
pub const MAX_ADDRESS_CHARS: usize = 200;

/// Sets the address on emails, receipts and labels; blank clears it.
///
/// # Errors
/// [`Error::Invalid`] when longer than [`MAX_ADDRESS_CHARS`]; [`Error::Db`] or
/// [`Error::Corrupt`].
pub async fn set_address(conn: &mut PgConnection, address: &str) -> Result<(), Error> {
    let address = address.trim();
    if address.chars().count() > MAX_ADDRESS_CHARS {
        return Err(Error::Invalid("address"));
    }
    let updated = sqlx::query!(
        "UPDATE company SET address = $1",
        Some(address).filter(|a| !a.is_empty())
    )
    .execute(conn)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(Error::Corrupt("no company row".to_owned()));
    }
    Ok(())
}
