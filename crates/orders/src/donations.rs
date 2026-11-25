//! Where the money of a charity drop goes (F4c, F4d): the customer's choice at checkout,
//! a saved donation and its choose link, and moving saved donations nobody chose for to
//! the next drop's charity. A choose link is 32 random bytes, base64url; only its SHA-256
//! is stored, like a login link (ADR 0029).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use db::time::{from_epoch_days, from_micros, to_epoch_days, to_micros};
use domain::{
    CharityId, CustomerId, DonationAllocationId, Money, OrderId, bakery_time_zone, in_bakery_time,
};
use jiff::civil::Date;
use jiff::{Span, Timestamp};
use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::audit::{self, Actor, Entry};
use crate::order::{OrderNumber, OrderStatus};

/// How long a saved donation waits for the customer's choice (F4d: default 3 months).
pub const SAVE_MONTHS: i32 = 3;

const TOKEN_BYTES: usize = 32;
/// base64url without padding of [`TOKEN_BYTES`].
const TOKEN_LEN: usize = 43;

/// What the customer chose at checkout on a charity drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DonationChoice {
    DropCharity,
    Charity(CharityId),
    Save,
}

/// The secret in a choose link. Whoever holds it can pick the charity once, so it is
/// never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct ChooseToken(String);

impl ChooseToken {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ChooseToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChooseToken(<redacted>)")
    }
}

/// A donation saved when its order was paid, with the link for its email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedDonation {
    pub allocation: DonationAllocationId,
    pub token: ChooseToken,
    pub choose_by: Timestamp,
}

/// A saved donation the customer can still choose a charity for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub allocation: DonationAllocationId,
    pub order: OrderId,
    pub number: OrderNumber,
    pub drop_date: Date,
    pub amount: Money,
    pub choose_by: Timestamp,
}

/// A saved donation the job moved to the next drop's charity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub allocation: DonationAllocationId,
    pub order: OrderId,
    pub charity: CharityId,
    pub charity_name: String,
    pub drop_date: Date,
    pub amount: Money,
    /// The order is still a sale and its customer is not erased (F12); a cancelled
    /// order's customer gets no email.
    pub notify: bool,
}

/// What [`move_next_expired`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expired {
    /// No saved donation is past its date, or the others are being moved right now.
    Nothing,
    /// One is past its date, but no coming drop has an active charity yet.
    NoDrop,
    Moved(Moved),
}

/// Whom a choice comes from: the holder of a choose link, or a signed-in customer.
#[derive(Debug, Clone, Copy)]
pub enum Chooser<'a> {
    Link(&'a str),
    Customer(CustomerId, DonationAllocationId),
}

fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// `None` for anything that cannot be one of our tokens, so junk never reaches the
/// database.
fn token_hash(token: &str) -> Option<Vec<u8>> {
    (token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| hash(token))
}

fn new_token() -> (ChooseToken, Vec<u8>) {
    let mut bytes = [0_u8; TOKEN_BYTES];
    rand::rng().fill(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash(&token);
    (ChooseToken(token), hash)
}

/// Local midnight [`SAVE_MONTHS`] after the day `paid` falls on in Amsterdam.
///
/// # Errors
/// [`Error::Corrupt`] for a date out of range.
pub fn choose_by(paid: Timestamp) -> Result<Timestamp, Error> {
    in_bakery_time(paid)
        .date()
        .checked_add(Span::new().months(SAVE_MONTHS))
        .and_then(|date| date.to_zoned(bakery_time_zone()))
        .map(|zoned| zoned.timestamp())
        .map_err(|err| Error::Corrupt(err.to_string()))
}

async fn ensure_active(conn: &mut PgConnection, charity: CharityId) -> Result<(), Error> {
    let active = sqlx::query_scalar!(
        r#"SELECT archived_at IS NULL AS "active!" FROM charity WHERE id = $1"#,
        charity.get(),
    )
    .fetch_optional(conn)
    .await?;
    if active == Some(true) {
        Ok(())
    } else {
        Err(Error::Invalid("charity"))
    }
}

/// Stores the customer's choice on an order that waits for payment; [`crate::mark_paid`]
/// follows it. Choosing the drop's own charity is the same as [`DonationChoice::DropCharity`].
///
/// # Errors
/// [`Error::NotFound`], [`Error::WrongStatus`] when the order is no longer waiting for
/// payment, [`Error::Invalid`] (`"donation"`) for another choice on a drop without
/// charity, or (`"charity"`) for an unknown or archived charity.
pub async fn choose_at_checkout(
    conn: &mut PgConnection,
    order: OrderId,
    choice: DonationChoice,
) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    let row = sqlx::query!(
        r#"SELECT o.status, d.charity_id FROM "order" o JOIN bake_day d ON d.id = o.bake_day_id
        WHERE o.id = $1 FOR UPDATE OF o"#,
        order.get(),
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    let status = OrderStatus::parse(&row.status)?;
    if status != OrderStatus::PendingPayment {
        return Err(Error::WrongStatus(status));
    }
    let (charity, saved) = match (row.charity_id, choice) {
        (_, DonationChoice::DropCharity) => (None, false),
        (None, _) => return Err(Error::Invalid("donation")),
        (Some(drop), DonationChoice::Charity(charity)) if charity.get() == drop => (None, false),
        (Some(_), DonationChoice::Charity(charity)) => {
            ensure_active(&mut tx, charity).await?;
            (Some(charity.get()), false)
        }
        (Some(_), DonationChoice::Save) => (None, true),
    };
    sqlx::query!(
        r#"UPDATE "order" SET donation_charity_id = $2, donation_saved = $3 WHERE id = $1"#,
        order.get(),
        charity,
        saved,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The customer's checkout choice for `order`: the charity chosen instead of the drop's,
/// and whether the donation is saved.
pub(crate) async fn checkout_choice(
    conn: &mut PgConnection,
    order: OrderId,
) -> Result<(Option<CharityId>, bool), Error> {
    let row = sqlx::query!(
        r#"SELECT donation_charity_id, donation_saved FROM "order" WHERE id = $1"#,
        order.get(),
    )
    .fetch_one(conn)
    .await?;
    Ok((
        row.donation_charity_id.map(CharityId::new),
        row.donation_saved,
    ))
}

/// Saves the donation of a paid order: no charity yet, a new choose link, and
/// [`choose_by`] from `now`.
pub(crate) async fn save(
    conn: &mut PgConnection,
    order: OrderId,
    amount: Money,
    now: Timestamp,
    actor: Actor,
) -> Result<SavedDonation, Error> {
    let (token, token_hash) = new_token();
    let choose_by = choose_by(now)?;
    let id = sqlx::query_scalar!(
        "INSERT INTO donation_allocation (order_id, amount_cents, status, choose_token_hash,
            choose_by, created_at)
        VALUES ($1, $2, 'saved', $3, timestamptz 'epoch' + $4::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $5::bigint * interval '1 microsecond')
        RETURNING id",
        order.get(),
        amount.cents(),
        token_hash,
        to_micros(choose_by),
        to_micros(now),
    )
    .fetch_one(&mut *conn)
    .await?;
    audit::record(
        conn,
        &Entry {
            entity: "donation_allocation",
            entity_id: id,
            action: "saved",
            before: None,
            after: Some(serde_json::json!({
                "order_id": order.get(),
                "amount_cents": amount.cents(),
                "status": "saved",
                "choose_by": choose_by.to_string(),
            })),
            actor,
            at: now,
        },
    )
    .await?;
    Ok(SavedDonation {
        allocation: DonationAllocationId::new(id),
        token,
        choose_by,
    })
}

struct SavedRow {
    id: i64,
    order_id: i64,
    number_year: i16,
    number_seq: i32,
    date_days: i32,
    amount_cents: i64,
    choose_by_us: i64,
}

fn saved_from_row(row: SavedRow) -> Result<Saved, Error> {
    Ok(Saved {
        allocation: DonationAllocationId::new(row.id),
        order: OrderId::new(row.order_id),
        number: OrderNumber {
            year: row.number_year,
            seq: row.number_seq,
        },
        drop_date: from_epoch_days(row.date_days)?,
        amount: Money::from_cents(row.amount_cents),
        choose_by: from_micros(row.choose_by_us)?,
    })
}

/// The saved donation behind a choose link, while it can still be chosen: its order is
/// still a sale and its date has not passed.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn saved_by_link(
    conn: &mut PgConnection,
    token: &str,
    now: Timestamp,
) -> Result<Option<Saved>, Error> {
    let Some(token_hash) = token_hash(token) else {
        return Ok(None);
    };
    sqlx::query_as!(
        SavedRow,
        r#"SELECT a.id, a.order_id, o.number_year, o.number_seq,
            d.date - date 'epoch' AS "date_days!", a.amount_cents,
            (extract(epoch FROM a.choose_by) * 1000000)::bigint AS "choose_by_us!"
        FROM donation_allocation a
        JOIN "order" o ON o.id = a.order_id
        JOIN bake_day d ON d.id = o.bake_day_id
        WHERE a.choose_token_hash = $1 AND a.status = 'saved'
            AND a.choose_by > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
            AND o.status IN ('paid', 'confirmed', 'picked_up')"#,
        token_hash,
        to_micros(now),
    )
    .fetch_optional(conn)
    .await?
    .map(saved_from_row)
    .transpose()
}

/// The customer's saved donations that can still be chosen, oldest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn saved_of_customer(
    conn: &mut PgConnection,
    customer: CustomerId,
    now: Timestamp,
) -> Result<Vec<Saved>, Error> {
    sqlx::query_as!(
        SavedRow,
        r#"SELECT a.id, a.order_id, o.number_year, o.number_seq,
            d.date - date 'epoch' AS "date_days!", a.amount_cents,
            (extract(epoch FROM a.choose_by) * 1000000)::bigint AS "choose_by_us!"
        FROM donation_allocation a
        JOIN "order" o ON o.id = a.order_id
        JOIN bake_day d ON d.id = o.bake_day_id
        WHERE o.customer_id = $1 AND a.status = 'saved'
            AND a.choose_by > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
            AND o.status IN ('paid', 'confirmed', 'picked_up')
        ORDER BY a.choose_by, a.id"#,
        customer.get(),
        to_micros(now),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(saved_from_row)
    .collect()
}

/// Gives a saved donation to an active charity. The link stops working. `None` when the
/// link or allocation does not lead to a donation that can still be chosen (see
/// [`saved_by_link`]); a second choice finds it chosen and changes nothing.
///
/// # Errors
/// [`Error::Invalid`] (`"charity"`) for an unknown or archived charity, [`Error::Db`] or
/// [`Error::Corrupt`].
pub async fn choose(
    conn: &mut PgConnection,
    chooser: Chooser<'_>,
    charity: CharityId,
    now: Timestamp,
) -> Result<Option<DonationAllocationId>, Error> {
    let (token_hash, customer, allocation) = match chooser {
        Chooser::Link(token) => match token_hash(token) {
            Some(hash) => (Some(hash), None, None),
            None => return Ok(None),
        },
        Chooser::Customer(customer, allocation) => {
            (None, Some(customer.get()), Some(allocation.get()))
        }
    };
    let mut tx = conn.begin().await?;
    let Some(row) = sqlx::query!(
        r#"SELECT a.id, a.order_id, a.amount_cents, o.customer_id
        FROM donation_allocation a JOIN "order" o ON o.id = a.order_id
        WHERE a.status = 'saved'
            AND a.choose_by > timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
            AND o.status IN ('paid', 'confirmed', 'picked_up')
            AND ($2::bytea IS NULL OR a.choose_token_hash = $2)
            AND ($3::bigint IS NULL OR (o.customer_id = $3 AND a.id = $4))
        FOR UPDATE OF a FOR SHARE OF o"#,
        to_micros(now),
        token_hash,
        customer,
        allocation,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };
    ensure_active(&mut tx, charity).await?;
    earmark(
        &mut tx,
        row.id,
        charity,
        "chosen",
        Actor::Customer(CustomerId::new(row.customer_id)),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(Some(DonationAllocationId::new(row.id)))
}

async fn earmark(
    conn: &mut PgConnection,
    id: i64,
    charity: CharityId,
    action: &str,
    actor: Actor,
    now: Timestamp,
) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE donation_allocation SET status = 'earmarked', charity_id = $2,
            choose_token_hash = NULL, choose_by = NULL
        WHERE id = $1",
        id,
        charity.get(),
    )
    .execute(&mut *conn)
    .await?;
    audit::record(
        conn,
        &Entry {
            entity: "donation_allocation",
            entity_id: id,
            action,
            before: Some(serde_json::json!({ "status": "saved" })),
            after: Some(serde_json::json!({
                "status": "earmarked",
                "charity_id": charity.get(),
            })),
            actor,
            at: now,
        },
    )
    .await
}

/// Moves one saved donation whose date has passed to the charity of the next drop: the
/// first drop from today (Amsterdam) that is open or closed for orders (published, not
/// draft), takes orders online and has an active charity. Call it in the transaction that queues the customer's email;
/// the allocation stays locked until then, and other workers skip it.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn move_next_expired(conn: &mut PgConnection, now: Timestamp) -> Result<Expired, Error> {
    let Some(row) = sqlx::query!(
        r#"SELECT a.id, a.order_id, a.amount_cents, o.status, c.erased_at IS NOT NULL AS "erased!"
        FROM donation_allocation a JOIN "order" o ON o.id = a.order_id
        JOIN customer c ON c.id = o.customer_id
        WHERE a.status = 'saved'
            AND a.choose_by <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
        ORDER BY a.choose_by, a.id
        LIMIT 1
        FOR UPDATE OF a SKIP LOCKED"#,
        to_micros(now),
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(Expired::Nothing);
    };
    let today = in_bakery_time(now).date();
    let Some(next) = sqlx::query!(
        r#"SELECT d.date - date 'epoch' AS "date_days!", c.id, c.name
        FROM bake_day d JOIN charity c ON c.id = d.charity_id
        WHERE c.archived_at IS NULL AND d.online_ordering
            AND d.status IN ('open', 'closed')
            AND d.date >= date 'epoch' + $1::integer
        ORDER BY d.date, d.id
        LIMIT 1"#,
        to_epoch_days(today),
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(Expired::NoDrop);
    };
    let charity = CharityId::new(next.id);
    earmark(
        conn,
        row.id,
        charity,
        "moved",
        Actor::System("saved-donations"),
        now,
    )
    .await?;
    let status = OrderStatus::parse(&row.status)?;
    Ok(Expired::Moved(Moved {
        allocation: DonationAllocationId::new(row.id),
        order: OrderId::new(row.order_id),
        charity,
        charity_name: next.name,
        drop_date: from_epoch_days(next.date_days)?,
        amount: Money::from_cents(row.amount_cents),
        notify: !row.erased
            && matches!(
                status,
                OrderStatus::Paid | OrderStatus::Confirmed | OrderStatus::PickedUp
            ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_donation_waits_three_months_to_local_midnight() {
        let paid: Timestamp = "2026-12-05T14:30:00Z".parse().unwrap();
        assert_eq!(
            choose_by(paid).unwrap(),
            "2027-03-04T23:00:00Z".parse::<Timestamp>().unwrap()
        );
    }

    #[test]
    fn the_date_follows_summer_time() {
        let paid: Timestamp = "2027-01-15T09:00:00Z".parse().unwrap();
        assert_eq!(
            choose_by(paid).unwrap(),
            "2027-04-14T22:00:00Z".parse::<Timestamp>().unwrap()
        );
    }

    #[test]
    fn only_well_formed_tokens_are_looked_up() {
        assert!(token_hash("kort").is_none());
        assert!(token_hash(&"a".repeat(43)).is_some());
        assert!(token_hash(&"!".repeat(43)).is_none());
        let (token, hash) = new_token();
        assert_eq!(token_hash(token.as_str()), Some(hash));
        assert_eq!(format!("{token:?}"), "ChooseToken(<redacted>)");
    }
}
