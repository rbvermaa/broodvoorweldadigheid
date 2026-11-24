use db::time::{from_epoch_days, from_micros, to_epoch_days, to_micros};
use domain::{BakeDayId, CharityId, Clock, PickupLocationId, PickupSlotId, ProductId};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::pickup;
use crate::schedule::{self, LeadTimeWarning};
use crate::status::{self, BakeDayStatus, PublicStatus, StatusFacts, Transition};

const MAX_REPEAT_WEEKS: u8 = 52;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BakeDay {
    pub id: BakeDayId,
    pub date: Date,
    pub online_ordering: bool,
    pub charity_id: Option<CharityId>,
    pub menu_publish_at: Option<Timestamp>,
    pub opens_at: Option<Timestamp>,
    pub cutoff_at: Option<Timestamp>,
    pub bake_starts_at: Timestamp,
    pub status: BakeDayStatus,
    pub cancel_message: Option<String>,
}

impl BakeDay {
    #[must_use]
    pub const fn status_facts(&self) -> StatusFacts {
        StatusFacts {
            status: self.status,
            online_ordering: self.online_ordering,
            cutoff_at: self.cutoff_at,
            bake_starts_at: self.bake_starts_at,
        }
    }

    /// The status on the public drop calendar (F1a); `None` for production-only days.
    #[must_use]
    pub fn public_status(&self, now: Timestamp, sold_out: bool) -> Option<PublicStatus> {
        status::public_status(self.status_facts(), self.menu_publish_at, now, sold_out)
    }

    const fn ensure_editable(&self) -> Result<(), Error> {
        if self.status.is_final() {
            return Err(Error::Final);
        }
        Ok(())
    }
}

/// A new bake day at a pickup location. Pickup slots and the order deadline come from the
/// location's active windows for the date's weekday (F1, F4a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewBakeDay {
    pub date: Date,
    pub location_id: PickupLocationId,
    pub bake_starts_at: Timestamp,
    pub online_ordering: bool,
    pub charity_id: Option<CharityId>,
}

/// The times of a drop, changeable per drop (F1, F1a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub online_ordering: bool,
    pub menu_publish_at: Option<Timestamp>,
    pub opens_at: Option<Timestamp>,
    pub cutoff_at: Option<Timestamp>,
    pub bake_starts_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickupSlotInput {
    pub starts_at: Timestamp,
    pub ends_at: Timestamp,
    pub max_orders: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickupSlot {
    pub id: PickupSlotId,
    pub bake_day_id: BakeDayId,
    pub location_id: PickupLocationId,
    pub starts_at: Timestamp,
    pub ends_at: Timestamp,
    pub max_orders: Option<i32>,
}

struct Row {
    id: i64,
    date_days: i32,
    online_ordering: bool,
    charity_id: Option<i64>,
    menu_publish_at_us: Option<i64>,
    opens_at_us: Option<i64>,
    cutoff_at_us: Option<i64>,
    bake_starts_at_us: i64,
    status: String,
    cancel_message: Option<String>,
}

impl TryFrom<Row> for BakeDay {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        let micros = |us: Option<i64>| us.map(from_micros).transpose();
        Ok(Self {
            id: BakeDayId::new(row.id),
            date: from_epoch_days(row.date_days)?,
            online_ordering: row.online_ordering,
            charity_id: row.charity_id.map(CharityId::new),
            menu_publish_at: micros(row.menu_publish_at_us)?,
            opens_at: micros(row.opens_at_us)?,
            cutoff_at: micros(row.cutoff_at_us)?,
            bake_starts_at: from_micros(row.bake_starts_at_us)?,
            status: row
                .status
                .parse()
                .map_err(|e: status::UnknownStatus| Error::Corrupt(e.to_string()))?,
            cancel_message: row.cancel_message,
        })
    }
}

struct SlotRow {
    id: i64,
    bake_day_id: i64,
    location_id: i64,
    starts_at_us: i64,
    ends_at_us: i64,
    max_orders: Option<i32>,
}

impl TryFrom<SlotRow> for PickupSlot {
    type Error = Error;

    fn try_from(row: SlotRow) -> Result<Self, Error> {
        Ok(Self {
            id: PickupSlotId::new(row.id),
            bake_day_id: BakeDayId::new(row.bake_day_id),
            location_id: PickupLocationId::new(row.location_id),
            starts_at: from_micros(row.starts_at_us)?,
            ends_at: from_micros(row.ends_at_us)?,
            max_orders: row.max_orders,
        })
    }
}

fn micros(timestamp: Option<Timestamp>) -> Option<i64> {
    timestamp.map(to_micros)
}

fn validate_schedule(schedule: &Schedule) -> Result<(), Error> {
    if schedule.online_ordering && schedule.cutoff_at.is_none() {
        return Err(Error::Invalid("deadline"));
    }
    if !schedule.online_ordering && schedule.opens_at.is_some() {
        return Err(Error::Invalid("opening time of a production-only day"));
    }
    if let (Some(opens), Some(cutoff)) = (schedule.opens_at, schedule.cutoff_at)
        && opens >= cutoff
    {
        return Err(Error::Invalid("opening time"));
    }
    Ok(())
}

fn validate_slot(input: &PickupSlotInput) -> Result<(), Error> {
    if input.starts_at >= input.ends_at {
        return Err(Error::Invalid("pickup slot"));
    }
    if input.max_orders.is_some_and(|max| max <= 0) {
        return Err(Error::Invalid("maximum orders"));
    }
    Ok(())
}

async fn ensure_charity_active(conn: &mut PgConnection, id: CharityId) -> Result<(), Error> {
    let active = sqlx::query_scalar!(
        r#"SELECT archived_at IS NULL AS "active!" FROM charity WHERE id = $1"#,
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::Invalid("charity"))?;
    if !active {
        return Err(Error::CharityArchived);
    }
    Ok(())
}

/// Creates a draft bake day with one pickup slot per active window of the location on
/// the date's weekday. The deadline is the earliest of the windows' default deadlines.
///
/// # Errors
/// [`Error::NotFound`] when the location does not exist, [`Error::NoPickupWindow`],
/// [`Error::Invalid`] or [`Error::CharityArchived`].
pub async fn create(conn: &mut PgConnection, input: &NewBakeDay) -> Result<BakeDay, Error> {
    let mut tx = conn.begin().await?;
    pickup::get_location(&mut tx, input.location_id).await?;
    if let Some(charity) = input.charity_id {
        ensure_charity_active(&mut tx, charity).await?;
    }
    let weekday = input.date.weekday();
    let windows: Vec<_> = pickup::list_windows(&mut tx, input.location_id)
        .await?
        .into_iter()
        .filter(|w| w.active && w.weekday == weekday)
        .collect();
    if windows.is_empty() {
        return Err(Error::NoPickupWindow(weekday));
    }
    let mut slots = Vec::with_capacity(windows.len());
    let mut cutoff: Option<Timestamp> = None;
    for window in &windows {
        slots.push(PickupSlotInput {
            starts_at: schedule::bakery_timestamp(input.date, window.starts)?,
            ends_at: schedule::bakery_timestamp(input.date, window.ends)?,
            max_orders: None,
        });
        let deadline = schedule::default_cutoff(
            input.date,
            window.default_deadline_days_before,
            window.default_deadline_time,
        )?;
        cutoff = Some(cutoff.map_or(deadline, |c| c.min(deadline)));
    }
    let id = insert_day(
        &mut tx,
        input.date,
        input.charity_id,
        &Schedule {
            online_ordering: input.online_ordering,
            menu_publish_at: None,
            opens_at: None,
            cutoff_at: cutoff,
            bake_starts_at: input.bake_starts_at,
        },
    )
    .await?;
    for slot in &slots {
        validate_slot(slot)?;
        insert_slot(&mut tx, id, input.location_id, slot).await?;
    }
    let day = get(&mut tx, id).await?;
    tx.commit().await?;
    Ok(day)
}

async fn insert_day(
    conn: &mut PgConnection,
    date: Date,
    charity_id: Option<CharityId>,
    schedule: &Schedule,
) -> Result<BakeDayId, Error> {
    validate_schedule(schedule)?;
    let id = sqlx::query_scalar!(
        r#"INSERT INTO bake_day (date, online_ordering, charity_id, menu_publish_at, opens_at,
            cutoff_at, bake_starts_at)
        VALUES (date 'epoch' + $1::integer, $2, $3,
            timestamptz 'epoch' + $4::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $5::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $6::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $7::bigint * interval '1 microsecond')
        RETURNING id"#,
        to_epoch_days(date)?,
        schedule.online_ordering,
        charity_id.map(CharityId::get),
        micros(schedule.menu_publish_at),
        micros(schedule.opens_at),
        micros(schedule.cutoff_at),
        to_micros(schedule.bake_starts_at),
    )
    .fetch_one(conn)
    .await?;
    Ok(BakeDayId::new(id))
}

async fn insert_slot(
    conn: &mut PgConnection,
    day: BakeDayId,
    location: PickupLocationId,
    input: &PickupSlotInput,
) -> Result<PickupSlotId, Error> {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO pickup_slot (bake_day_id, location_id, starts_at, ends_at, max_orders)
        VALUES ($1, $2, timestamptz 'epoch' + $3::bigint * interval '1 microsecond',
            timestamptz 'epoch' + $4::bigint * interval '1 microsecond', $5)
        RETURNING id"#,
        day.get(),
        location.get(),
        to_micros(input.starts_at),
        to_micros(input.ends_at),
        input.max_orders,
    )
    .fetch_one(conn)
    .await?;
    Ok(PickupSlotId::new(id))
}

/// # Errors
/// [`Error::NotFound`] when there is no such bake day.
pub async fn get(conn: &mut PgConnection, id: BakeDayId) -> Result<BakeDay, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, date - date 'epoch' AS "date_days!", online_ordering, charity_id,
            (extract(epoch FROM menu_publish_at) * 1000000)::bigint AS menu_publish_at_us,
            (extract(epoch FROM opens_at) * 1000000)::bigint AS opens_at_us,
            (extract(epoch FROM cutoff_at) * 1000000)::bigint AS cutoff_at_us,
            (extract(epoch FROM bake_starts_at) * 1000000)::bigint AS "bake_starts_at_us!",
            status, cancel_message
        FROM bake_day WHERE id = $1"#,
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// Locks the bake day row so its status cannot change until the transaction ends.
async fn get_for_update(conn: &mut PgConnection, id: BakeDayId) -> Result<BakeDay, Error> {
    sqlx::query_scalar!("SELECT id FROM bake_day WHERE id = $1 FOR UPDATE", id.get())
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(Error::NotFound)?;
    get(conn, id).await
}

/// Bake days with `from <= date <= to`, by date.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_between(
    conn: &mut PgConnection,
    from: Date,
    to: Date,
) -> Result<Vec<BakeDay>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, date - date 'epoch' AS "date_days!", online_ordering, charity_id,
            (extract(epoch FROM menu_publish_at) * 1000000)::bigint AS menu_publish_at_us,
            (extract(epoch FROM opens_at) * 1000000)::bigint AS opens_at_us,
            (extract(epoch FROM cutoff_at) * 1000000)::bigint AS cutoff_at_us,
            (extract(epoch FROM bake_starts_at) * 1000000)::bigint AS "bake_starts_at_us!",
            status, cancel_message
        FROM bake_day
        WHERE date BETWEEN date 'epoch' + $1::integer AND date 'epoch' + $2::integer
        ORDER BY date, id"#,
        to_epoch_days(from)?,
        to_epoch_days(to)?,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(BakeDay::try_from)
    .collect()
}

/// Changes the times of one drop and whether it sells online.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Final`], or [`Error::Invalid`]: an online day needs a
/// deadline, opens before it, and a production-only day has no opening time and cannot
/// be open.
pub async fn update_schedule(
    conn: &mut PgConnection,
    id: BakeDayId,
    schedule: &Schedule,
) -> Result<BakeDay, Error> {
    validate_schedule(schedule)?;
    let mut tx = conn.begin().await?;
    let day = get_for_update(&mut tx, id).await?;
    day.ensure_editable()?;
    if !schedule.online_ordering && day.status == BakeDayStatus::Open {
        return Err(Error::Invalid("production-only while open"));
    }
    sqlx::query!(
        r#"UPDATE bake_day SET online_ordering = $2,
            menu_publish_at = timestamptz 'epoch' + $3::bigint * interval '1 microsecond',
            opens_at = timestamptz 'epoch' + $4::bigint * interval '1 microsecond',
            cutoff_at = timestamptz 'epoch' + $5::bigint * interval '1 microsecond',
            bake_starts_at = timestamptz 'epoch' + $6::bigint * interval '1 microsecond'
        WHERE id = $1"#,
        id.get(),
        schedule.online_ordering,
        micros(schedule.menu_publish_at),
        micros(schedule.opens_at),
        micros(schedule.cutoff_at),
        to_micros(schedule.bake_starts_at),
    )
    .execute(&mut *tx)
    .await?;
    let day = get(&mut tx, id).await?;
    tx.commit().await?;
    Ok(day)
}

/// A drop's charity, or no charity, can change only until the first order (F4c).
///
/// # Errors
/// [`Error::OrdersExist`] when `orders_so_far > 0`.
pub const fn ensure_charity_changeable(orders_so_far: u64) -> Result<(), Error> {
    if orders_so_far > 0 {
        return Err(Error::OrdersExist);
    }
    Ok(())
}

/// Sets the drop's charity, or `None` for a normal sale. `orders_so_far` is the number of
/// orders on the bake day, read in the caller's transaction.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Final`], [`Error::OrdersExist`], [`Error::Invalid`]
/// for an unknown charity, or [`Error::CharityArchived`].
pub async fn set_charity(
    conn: &mut PgConnection,
    id: BakeDayId,
    charity_id: Option<CharityId>,
    orders_so_far: u64,
) -> Result<BakeDay, Error> {
    ensure_charity_changeable(orders_so_far)?;
    let mut tx = conn.begin().await?;
    get_for_update(&mut tx, id).await?.ensure_editable()?;
    if let Some(charity) = charity_id {
        ensure_charity_active(&mut tx, charity).await?;
    }
    sqlx::query!(
        "UPDATE bake_day SET charity_id = $2 WHERE id = $1",
        id.get(),
        charity_id.map(CharityId::get),
    )
    .execute(&mut *tx)
    .await?;
    let day = get(&mut tx, id).await?;
    tx.commit().await?;
    Ok(day)
}

async fn transition(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: BakeDayId,
    transition: Transition,
    cancel_message: Option<&str>,
) -> Result<BakeDay, Error> {
    let mut tx = conn.begin().await?;
    let day = get_for_update(&mut tx, id).await?;
    let next = status::next_status(day.status_facts(), transition, clock.now())?;
    sqlx::query!(
        "UPDATE bake_day SET status = $2, cancel_message = $3 WHERE id = $1",
        id.get(),
        next.as_str(),
        cancel_message,
    )
    .execute(&mut *tx)
    .await?;
    let day = get(&mut tx, id).await?;
    tx.commit().await?;
    Ok(day)
}

/// Opens ordering: draft to open, before the deadline.
///
/// # Errors
/// [`Error::NotFound`] or [`Error::Transition`].
pub async fn open(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: BakeDayId,
) -> Result<BakeDay, Error> {
    transition(conn, clock, id, Transition::Open, None).await
}

/// Closes ordering: open to closed, or draft to closed for a production-only day.
///
/// # Errors
/// [`Error::NotFound`] or [`Error::Transition`].
pub async fn close(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: BakeDayId,
) -> Result<BakeDay, Error> {
    transition(conn, clock, id, Transition::Close, None).await
}

/// Marks a closed day done, once baking has started.
///
/// # Errors
/// [`Error::NotFound`] or [`Error::Transition`].
pub async fn finish(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: BakeDayId,
) -> Result<BakeDay, Error> {
    transition(conn, clock, id, Transition::Finish, None).await
}

/// Cancels a day that is not done, keeping the baker's message for customers (F2).
///
/// # Errors
/// [`Error::Invalid`] for an empty message, [`Error::NotFound`] or [`Error::Transition`].
pub async fn cancel(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    id: BakeDayId,
    message: &str,
) -> Result<BakeDay, Error> {
    if message.trim().is_empty() {
        return Err(Error::Invalid("cancel message"));
    }
    transition(conn, clock, id, Transition::Cancel, Some(message.trim())).await
}

/// A bake day's pickup slots, by start time.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_slots(conn: &mut PgConnection, day: BakeDayId) -> Result<Vec<PickupSlot>, Error> {
    sqlx::query_as!(
        SlotRow,
        r#"SELECT id, bake_day_id, location_id,
            (extract(epoch FROM starts_at) * 1000000)::bigint AS "starts_at_us!",
            (extract(epoch FROM ends_at) * 1000000)::bigint AS "ends_at_us!",
            max_orders
        FROM pickup_slot WHERE bake_day_id = $1 ORDER BY starts_at, id"#,
        day.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(PickupSlot::try_from)
    .collect()
}

/// # Errors
/// [`Error::NotFound`], [`Error::Final`] or [`Error::Invalid`].
pub async fn add_slot(
    conn: &mut PgConnection,
    day: BakeDayId,
    location: PickupLocationId,
    input: &PickupSlotInput,
) -> Result<PickupSlotId, Error> {
    validate_slot(input)?;
    let mut tx = conn.begin().await?;
    get_for_update(&mut tx, day).await?.ensure_editable()?;
    pickup::get_location(&mut tx, location).await?;
    let id = insert_slot(&mut tx, day, location, input).await?;
    tx.commit().await?;
    Ok(id)
}

/// Changes a pickup slot's times or order limit for this drop only.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Final`] or [`Error::Invalid`].
pub async fn update_slot(
    conn: &mut PgConnection,
    id: PickupSlotId,
    input: &PickupSlotInput,
) -> Result<(), Error> {
    validate_slot(input)?;
    let mut tx = conn.begin().await?;
    let day = sqlx::query_scalar!(
        "SELECT bake_day_id FROM pickup_slot WHERE id = $1",
        id.get()
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    get_for_update(&mut tx, BakeDayId::new(day))
        .await?
        .ensure_editable()?;
    sqlx::query!(
        r#"UPDATE pickup_slot SET
            starts_at = timestamptz 'epoch' + $2::bigint * interval '1 microsecond',
            ends_at = timestamptz 'epoch' + $3::bigint * interval '1 microsecond',
            max_orders = $4
        WHERE id = $1"#,
        id.get(),
        to_micros(input.starts_at),
        to_micros(input.ends_at),
        input.max_orders,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Copies a bake day to another date as a new draft: its times move by the same number of
/// days at the same local time, and its pickup slots, pools and products come along.
/// Extra production, status and cancel message do not. An archived charity is dropped.
///
/// # Errors
/// [`Error::NotFound`] or [`Error::Invalid`].
pub async fn copy_to(conn: &mut PgConnection, id: BakeDayId, date: Date) -> Result<BakeDay, Error> {
    let mut tx = conn.begin().await?;
    let source = get(&mut tx, id).await?;
    let days = to_epoch_days(date)? - to_epoch_days(source.date)?;
    let shift = |t: Timestamp| schedule::shift_days(t, days);
    let shift_opt = |t: Option<Timestamp>| t.map(shift).transpose();
    let charity_id = match source.charity_id {
        Some(charity) => sqlx::query_scalar!(
            "SELECT id FROM charity WHERE id = $1 AND archived_at IS NULL",
            charity.get(),
        )
        .fetch_optional(&mut *tx)
        .await?
        .map(CharityId::new),
        None => None,
    };
    let new_id = insert_day(
        &mut tx,
        date,
        charity_id,
        &Schedule {
            online_ordering: source.online_ordering,
            menu_publish_at: shift_opt(source.menu_publish_at)?,
            opens_at: shift_opt(source.opens_at)?,
            cutoff_at: shift_opt(source.cutoff_at)?,
            bake_starts_at: shift(source.bake_starts_at)?,
        },
    )
    .await?;
    for slot in list_slots(&mut tx, id).await? {
        let input = PickupSlotInput {
            starts_at: shift(slot.starts_at)?,
            ends_at: shift(slot.ends_at)?,
            max_orders: slot.max_orders,
        };
        insert_slot(&mut tx, new_id, slot.location_id, &input).await?;
    }
    sqlx::query!(
        "WITH pools AS (
            INSERT INTO capacity_pool (bake_day_id, name, max_units)
            SELECT $2, name, max_units FROM capacity_pool WHERE bake_day_id = $1
            RETURNING id, name
        )
        INSERT INTO bake_day_product (bake_day_id, product_id, max_qty, capacity_pool_id, units_per_item)
        SELECT $2, p.product_id, p.max_qty, new_pool.id, p.units_per_item
        FROM bake_day_product p
        LEFT JOIN capacity_pool old_pool ON old_pool.id = p.capacity_pool_id
        LEFT JOIN pools new_pool ON new_pool.name = old_pool.name
        WHERE p.bake_day_id = $1",
        id.get(),
        new_id.get(),
    )
    .execute(&mut *tx)
    .await?;
    let day = get(&mut tx, new_id).await?;
    tx.commit().await?;
    Ok(day)
}

/// Copies a bake day to the same weekday for each of the next `weeks` weeks.
///
/// # Errors
/// [`Error::Invalid`] unless `1 <= weeks <= 52`, or any error of [`copy_to`].
pub async fn repeat_weekly(
    conn: &mut PgConnection,
    id: BakeDayId,
    weeks: u8,
) -> Result<Vec<BakeDay>, Error> {
    if !(1..=MAX_REPEAT_WEEKS).contains(&weeks) {
        return Err(Error::Invalid("number of weeks"));
    }
    let mut tx = conn.begin().await?;
    let source = get(&mut tx, id).await?;
    let mut copies = Vec::with_capacity(usize::from(weeks));
    for week in 1..=i32::from(weeks) {
        let date = from_epoch_days(to_epoch_days(source.date)? + 7 * week)?;
        copies.push(copy_to(&mut tx, id, date).await?);
    }
    tx.commit().await?;
    Ok(copies)
}

/// Warns when the deadline leaves less than the longest lead time of the drop's products
/// before baking starts (F1).
///
/// # Errors
/// [`Error::NotFound`], [`Error::Catalog`] or [`Error::Db`].
pub async fn lead_time_warning(
    conn: &mut PgConnection,
    id: BakeDayId,
) -> Result<Option<LeadTimeWarning>, Error> {
    let day = get(conn, id).await?;
    let products = sqlx::query_scalar!(
        "SELECT product_id FROM bake_day_product WHERE bake_day_id = $1 ORDER BY product_id",
        id.get(),
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut lead_times = Vec::with_capacity(products.len());
    for product in products {
        let product = ProductId::new(product);
        lead_times.push((
            product,
            catalog::products::lead_time_hours(conn, product).await?,
        ));
    }
    Ok(schedule::lead_time_warning(
        day.cutoff_at,
        day.bake_starts_at,
        &lead_times,
    ))
}
