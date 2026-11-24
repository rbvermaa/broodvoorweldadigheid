//! A customer's own data (F12, ADR 0031): export it, set marketing consent, and erase it.
//! Orders, payments and donations are business records kept for 7 years (spec,
//! Compliance), so erasing keeps them and removes what identifies the customer.

use db::time::{from_micros, to_epoch_days, to_micros};
use domain::{CustomerId, in_bakery_time};
use jiff::Timestamp;
use serde::Serialize;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::order::OrderNumber;

/// What an erased order keeps as the customer's name; the column cannot be empty.
pub const ERASED_NAME: &str = "-";

/// Unique per customer, lower-case, and never deliverable (RFC 2606 reserves `.invalid`).
fn erased_email(id: i64) -> String {
    format!("erased-{id}@erased.invalid")
}

/// Everything stored about one customer, for the download. Amounts are in cents, times
/// in UTC (RFC 3339) and dates as `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomerData {
    pub customer: Profile,
    pub orders: Vec<OrderRecord>,
    pub logins: Vec<LoginRecord>,
    pub login_links: Vec<LoginLinkRecord>,
    /// The drop-alert subscription of the same address, filled in by the caller.
    pub drop_alerts: Option<DropAlertsRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub email: String,
    pub name: String,
    pub phone: Option<String>,
    pub marketing_consent: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderRecord {
    pub number: String,
    pub bake_day: String,
    pub status: String,
    pub name: String,
    pub phone: Option<String>,
    pub marketing_opt_in: bool,
    pub total_cents: i64,
    pub created_at: String,
    pub paid_at: Option<String>,
    pub lines: Vec<LineRecord>,
    pub payments: Vec<PaymentRecord>,
    pub donations: Vec<DonationRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineRecord {
    pub product: String,
    pub qty: i32,
    pub unit_price_cents: i64,
    pub vat_rate: i16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaymentRecord {
    pub method: Option<String>,
    pub status: String,
    pub amount_cents: i64,
    pub refunded_cents: i64,
    pub received_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DonationRecord {
    pub amount_cents: i64,
    pub status: String,
    pub charity: Option<String>,
    pub choose_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoginRecord {
    pub started_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoginLinkRecord {
    pub created_at: String,
    pub expires_at: String,
    pub used_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DropAlertsRecord {
    pub signed_up_at: String,
    pub confirmed_at: Option<String>,
    pub unsubscribed_at: Option<String>,
}

impl CustomerData {
    /// # Errors
    /// [`Error::Corrupt`] if it cannot be written as JSON.
    pub fn to_json(&self) -> Result<String, Error> {
        serde_json::to_string_pretty(self).map_err(|e| Error::Corrupt(e.to_string()))
    }
}

/// A customer found by the back office.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: CustomerId,
    pub email: String,
    pub name: String,
    pub orders: i64,
}

/// What [`erase`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Erase {
    /// Erased. `email` was the customer's address, for the confirmation email; the
    /// database no longer has it.
    Erased { email: String },
    /// An order still waits for payment or pickup, and the counter needs the name.
    OpenOrders,
    /// A business customer: their details are on invoices (F11a).
    Business,
}

fn time(micros: i64) -> Result<String, Error> {
    Ok(from_micros(micros)?.to_string())
}

fn maybe_time(micros: Option<i64>) -> Result<Option<String>, Error> {
    micros.map(time).transpose()
}

/// Everything stored about `customer`, except the drop-alert subscription.
///
/// # Errors
/// [`Error::NotFound`] for an unknown or erased customer; otherwise [`Error::Db`] or
/// [`Error::Corrupt`].
pub async fn export(conn: &mut PgConnection, customer: CustomerId) -> Result<CustomerData, Error> {
    let id = customer.get();
    let profile = sqlx::query!(
        r#"SELECT email, name, phone, marketing_consent,
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_us!"
        FROM customer WHERE id = $1 AND erased_at IS NULL"#,
        id,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::NotFound)?;
    let lines = sqlx::query!(
        r#"SELECT l.order_id, p.name_nl, l.qty, l.unit_price_cents, l.vat_rate
        FROM order_line l
        JOIN "order" o ON o.id = l.order_id
        JOIN product p ON p.id = l.product_id
        WHERE o.customer_id = $1
        ORDER BY l.id"#,
        id,
    )
    .fetch_all(&mut *conn)
    .await?;
    let payments = sqlx::query!(
        r#"SELECT p.order_id, p.method, p.status, p.amount_cents,
            payment_refunded_cents(p.id) AS "refunded_cents!",
            (extract(epoch FROM p.received_at) * 1000000)::bigint AS "received_us!"
        FROM payment p JOIN "order" o ON o.id = p.order_id
        WHERE o.customer_id = $1
        ORDER BY p.id"#,
        id,
    )
    .fetch_all(&mut *conn)
    .await?;
    let donations = sqlx::query!(
        r#"SELECT a.order_id, a.amount_cents, a.status, c.name AS "charity?",
            (extract(epoch FROM a.choose_by) * 1000000)::bigint AS choose_by_us
        FROM donation_allocation a
        JOIN "order" o ON o.id = a.order_id
        LEFT JOIN charity c ON c.id = a.charity_id
        WHERE o.customer_id = $1
        ORDER BY a.id"#,
        id,
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut orders = Vec::new();
    for row in sqlx::query!(
        r#"SELECT o.id, o.number_year, o.number_seq, d.date::text AS "bake_day!", o.status,
            o.customer_name, o.customer_phone, o.marketing_opt_in, o.total_cents,
            (extract(epoch FROM o.created_at) * 1000000)::bigint AS "created_us!",
            (extract(epoch FROM o.paid_at) * 1000000)::bigint AS paid_us
        FROM "order" o JOIN bake_day d ON d.id = o.bake_day_id
        WHERE o.customer_id = $1
        ORDER BY o.created_at, o.id"#,
        id,
    )
    .fetch_all(&mut *conn)
    .await?
    {
        orders.push(OrderRecord {
            number: OrderNumber {
                year: row.number_year,
                seq: row.number_seq,
            }
            .to_string(),
            bake_day: row.bake_day,
            status: row.status,
            name: row.customer_name,
            phone: row.customer_phone,
            marketing_opt_in: row.marketing_opt_in,
            total_cents: row.total_cents,
            created_at: time(row.created_us)?,
            paid_at: maybe_time(row.paid_us)?,
            lines: lines
                .iter()
                .filter(|l| l.order_id == row.id)
                .map(|l| LineRecord {
                    product: l.name_nl.clone(),
                    qty: l.qty,
                    unit_price_cents: l.unit_price_cents,
                    vat_rate: l.vat_rate,
                })
                .collect(),
            payments: payments
                .iter()
                .filter(|p| p.order_id == row.id)
                .map(|p| {
                    Ok(PaymentRecord {
                        method: p.method.clone(),
                        status: p.status.clone(),
                        amount_cents: p.amount_cents,
                        refunded_cents: p.refunded_cents,
                        received_at: time(p.received_us)?,
                    })
                })
                .collect::<Result<_, Error>>()?,
            donations: donations
                .iter()
                .filter(|a| a.order_id == row.id)
                .map(|a| {
                    Ok(DonationRecord {
                        amount_cents: a.amount_cents,
                        status: a.status.clone(),
                        charity: a.charity.clone(),
                        choose_by: maybe_time(a.choose_by_us)?,
                    })
                })
                .collect::<Result<_, Error>>()?,
        });
    }
    let logins = sqlx::query!(
        r#"SELECT (extract(epoch FROM created_at) * 1000000)::bigint AS "created_us!",
            (extract(epoch FROM last_seen_at) * 1000000)::bigint AS "seen_us!"
        FROM customer_login WHERE customer_id = $1 ORDER BY created_at"#,
        id,
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(LoginRecord {
            started_at: time(row.created_us)?,
            last_seen_at: time(row.seen_us)?,
        })
    })
    .collect::<Result<_, Error>>()?;
    let login_links = sqlx::query!(
        r#"SELECT (extract(epoch FROM created_at) * 1000000)::bigint AS "created_us!",
            (extract(epoch FROM expires_at) * 1000000)::bigint AS "expires_us!",
            (extract(epoch FROM used_at) * 1000000)::bigint AS used_us
        FROM customer_login_link WHERE customer_id = $1 ORDER BY created_at"#,
        id,
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(LoginLinkRecord {
            created_at: time(row.created_us)?,
            expires_at: time(row.expires_us)?,
            used_at: maybe_time(row.used_us)?,
        })
    })
    .collect::<Result<_, Error>>()?;
    Ok(CustomerData {
        customer: Profile {
            email: profile.email,
            name: profile.name,
            phone: profile.phone,
            marketing_consent: profile.marketing_consent,
            created_at: time(profile.created_us)?,
        },
        orders,
        logins,
        login_links,
        drop_alerts: None,
    })
}

/// The customer with this address, trimmed and lower-cased like checkout stores it.
///
/// # Errors
/// [`Error::Db`].
pub async fn find_by_email(conn: &mut PgConnection, raw: &str) -> Result<Option<Found>, Error> {
    let email = raw.trim().to_lowercase();
    let row = sqlx::query!(
        r#"SELECT c.id, c.email, c.name,
            (SELECT count(*) FROM "order" o WHERE o.customer_id = c.id) AS "orders!"
        FROM customer c WHERE c.email = $1 AND c.erased_at IS NULL"#,
        email,
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|row| Found {
        id: CustomerId::new(row.id),
        email: row.email,
        name: row.name,
        orders: row.orders,
    }))
}

/// Records the customer's marketing consent and gives their address, so the caller can
/// sign it up for drop alerts or take it off in the same transaction.
///
/// # Errors
/// [`Error::NotFound`] for an unknown or erased customer, or [`Error::Db`].
pub async fn set_marketing_consent(
    conn: &mut PgConnection,
    customer: CustomerId,
    consent: bool,
) -> Result<String, Error> {
    sqlx::query_scalar!(
        "UPDATE customer SET marketing_consent = $2 WHERE id = $1 AND erased_at IS NULL
        RETURNING email",
        customer.get(),
        consent,
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)
}

/// Erases `customer`, unless an order still waits for payment or pickup (from today in
/// Amsterdam) or they are a business customer. Their orders keep everything the
/// bookkeeping and the charity reports need, with the name blanked and the phone number,
/// marketing wish and status link removed. Saved donations stay saved, with a choose link
/// nobody holds, so they move to the next drop's charity on their date. Login links and
/// logins are deleted, which signs every browser out. The customer row stays, for the
/// orders that point at it, without anything that identifies them.
///
/// # Errors
/// [`Error::NotFound`] for an unknown or already erased customer, or [`Error::Db`].
pub async fn erase(
    conn: &mut PgConnection,
    customer: CustomerId,
    actor: Actor,
    now: Timestamp,
) -> Result<Erase, Error> {
    let id = customer.get();
    let mut tx = conn.begin().await?;
    let email = sqlx::query_scalar!(
        "SELECT email FROM customer WHERE id = $1 AND erased_at IS NULL FOR UPDATE",
        id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    let business = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM business_account WHERE customer_id = $1) AS "b!""#,
        id,
    )
    .fetch_one(&mut *tx)
    .await?;
    if business {
        return Ok(Erase::Business);
    }
    let open = sqlx::query_scalar!(
        r#"SELECT EXISTS (
            SELECT 1 FROM "order" o JOIN bake_day d ON d.id = o.bake_day_id
            WHERE o.customer_id = $1
                AND (o.status = 'pending_payment'
                    OR (o.status IN ('paid', 'confirmed') AND d.date >= date 'epoch' + $2::integer))
        ) AS "open!""#,
        id,
        to_epoch_days(in_bakery_time(now).date()),
    )
    .fetch_one(&mut *tx)
    .await?;
    if open {
        return Ok(Erase::OpenOrders);
    }
    sqlx::query!(
        r#"UPDATE "order" SET customer_name = $2, customer_phone = NULL,
            marketing_opt_in = false, status_token = gen_random_uuid()
        WHERE customer_id = $1"#,
        id,
        ERASED_NAME,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"UPDATE donation_allocation a
        SET choose_token = NULL, choose_token_hash = sha256(convert_to(gen_random_uuid()::text, 'UTF8'))
        FROM "order" o
        WHERE o.id = a.order_id AND o.customer_id = $1 AND a.status = 'saved'"#,
        id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM customer_login_link WHERE customer_id = $1", id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM customer_login WHERE customer_id = $1", id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "UPDATE customer SET email = $2, name = $3, phone = NULL, marketing_consent = false,
            erased_at = timestamptz 'epoch' + $4::bigint * interval '1 microsecond'
        WHERE id = $1",
        id,
        erased_email(id),
        ERASED_NAME,
        to_micros(now),
    )
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        &Entry {
            entity: "customer",
            entity_id: id,
            action: "erased",
            before: None,
            after: None,
            actor,
            at: now,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Erase::Erased { email })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erased_addresses_are_unique_lower_case_and_undeliverable() {
        assert_eq!(erased_email(12), "erased-12@erased.invalid");
        assert_ne!(erased_email(1), erased_email(2));
        assert_eq!(erased_email(7), erased_email(7).to_lowercase());
    }
}
