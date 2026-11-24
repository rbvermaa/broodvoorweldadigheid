use db::time::{from_seconds_of_day, to_seconds_of_day};
use domain::{PickupLocationId, PickupWindowId};
use jiff::civil::{Time, Weekday};
use sqlx::PgConnection;

use crate::Error;

const MAX_DEADLINE_DAYS_BEFORE: i16 = 28;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickupLocationInput {
    pub name: String,
    pub address: String,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickupLocation {
    pub id: PickupLocationId,
    pub name: String,
    pub address: String,
    pub notes: String,
}

/// The order deadline defaults to `default_deadline_days_before` days before pickup,
/// at `default_deadline_time`. Times are whole seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickupWindowInput {
    pub weekday: Weekday,
    pub starts: Time,
    pub ends: Time,
    pub default_deadline_days_before: i16,
    pub default_deadline_time: Time,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickupWindow {
    pub id: PickupWindowId,
    pub location_id: PickupLocationId,
    pub weekday: Weekday,
    pub starts: Time,
    pub ends: Time,
    pub default_deadline_days_before: i16,
    pub default_deadline_time: Time,
    pub active: bool,
}

struct LocationRow {
    id: i64,
    name: String,
    address: String,
    notes: String,
}

impl From<LocationRow> for PickupLocation {
    fn from(row: LocationRow) -> Self {
        Self {
            id: PickupLocationId::new(row.id),
            name: row.name,
            address: row.address,
            notes: row.notes,
        }
    }
}

struct WindowRow {
    id: i64,
    location_id: i64,
    weekday: i16,
    starts_s: i32,
    ends_s: i32,
    default_deadline_days_before: i16,
    default_deadline_time_s: i32,
    active: bool,
}

impl TryFrom<WindowRow> for PickupWindow {
    type Error = Error;

    fn try_from(row: WindowRow) -> Result<Self, Error> {
        let weekday = i8::try_from(row.weekday)
            .ok()
            .and_then(|day| Weekday::from_monday_one_offset(day).ok())
            .ok_or_else(|| Error::Corrupt(format!("weekday {}", row.weekday)))?;
        Ok(Self {
            id: PickupWindowId::new(row.id),
            location_id: PickupLocationId::new(row.location_id),
            weekday,
            starts: from_seconds_of_day(row.starts_s)?,
            ends: from_seconds_of_day(row.ends_s)?,
            default_deadline_days_before: row.default_deadline_days_before,
            default_deadline_time: from_seconds_of_day(row.default_deadline_time_s)?,
            active: row.active,
        })
    }
}

fn validate_location(input: &PickupLocationInput) -> Result<(), Error> {
    if input.name.trim().is_empty() {
        return Err(Error::Invalid("name"));
    }
    if input.address.trim().is_empty() {
        return Err(Error::Invalid("address"));
    }
    Ok(())
}

fn validate_window(input: &PickupWindowInput) -> Result<(), Error> {
    if [input.starts, input.ends, input.default_deadline_time]
        .iter()
        .any(|time| time.subsec_nanosecond() != 0)
    {
        return Err(Error::Invalid("time with fractional seconds"));
    }
    if input.starts >= input.ends {
        return Err(Error::Invalid("pickup window"));
    }
    if !(0..=MAX_DEADLINE_DAYS_BEFORE).contains(&input.default_deadline_days_before) {
        return Err(Error::Invalid("deadline days before"));
    }
    Ok(())
}

/// # Errors
/// [`Error::Invalid`] when the name or address is empty.
pub async fn create_location(
    conn: &mut PgConnection,
    input: &PickupLocationInput,
) -> Result<PickupLocation, Error> {
    validate_location(input)?;
    Ok(sqlx::query_as!(
        LocationRow,
        "INSERT INTO pickup_location (name, address, notes) VALUES ($1, $2, $3)
        RETURNING id, name, address, notes",
        input.name.trim(),
        input.address.trim(),
        input.notes,
    )
    .fetch_one(conn)
    .await?
    .into())
}

/// # Errors
/// [`Error::NotFound`], or [`Error::Invalid`] when the name or address is empty.
pub async fn update_location(
    conn: &mut PgConnection,
    id: PickupLocationId,
    input: &PickupLocationInput,
) -> Result<PickupLocation, Error> {
    validate_location(input)?;
    Ok(sqlx::query_as!(
        LocationRow,
        "UPDATE pickup_location SET name = $2, address = $3, notes = $4 WHERE id = $1
        RETURNING id, name, address, notes",
        id.get(),
        input.name.trim(),
        input.address.trim(),
        input.notes,
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .into())
}

/// # Errors
/// [`Error::NotFound`] when there is no such location.
pub async fn get_location(
    conn: &mut PgConnection,
    id: PickupLocationId,
) -> Result<PickupLocation, Error> {
    Ok(sqlx::query_as!(
        LocationRow,
        "SELECT id, name, address, notes FROM pickup_location WHERE id = $1",
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .into())
}

/// All locations, by name.
///
/// # Errors
/// [`Error::Db`].
pub async fn list_locations(conn: &mut PgConnection) -> Result<Vec<PickupLocation>, Error> {
    Ok(sqlx::query_as!(
        LocationRow,
        "SELECT id, name, address, notes FROM pickup_location ORDER BY name, id",
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(PickupLocation::from)
    .collect())
}

/// # Errors
/// [`Error::NotFound`] when the location does not exist, or [`Error::Invalid`].
pub async fn create_window(
    conn: &mut PgConnection,
    location_id: PickupLocationId,
    input: &PickupWindowInput,
) -> Result<PickupWindow, Error> {
    validate_window(input)?;
    sqlx::query_as!(
        WindowRow,
        r#"INSERT INTO pickup_window (location_id, weekday, starts, ends,
            default_deadline_days_before, default_deadline_time, active)
        SELECT id, $2, make_interval(secs => $3::integer)::time, make_interval(secs => $4::integer)::time,
            $5, make_interval(secs => $6::integer)::time, $7
        FROM pickup_location WHERE id = $1
        RETURNING id, location_id, weekday,
            extract(epoch FROM starts)::integer AS "starts_s!",
            extract(epoch FROM ends)::integer AS "ends_s!",
            default_deadline_days_before,
            extract(epoch FROM default_deadline_time)::integer AS "default_deadline_time_s!",
            active"#,
        location_id.get(),
        i16::from(input.weekday.to_monday_one_offset()),
        to_seconds_of_day(input.starts),
        to_seconds_of_day(input.ends),
        input.default_deadline_days_before,
        to_seconds_of_day(input.default_deadline_time),
        input.active,
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// Windows cannot move to another location.
///
/// # Errors
/// [`Error::NotFound`], or [`Error::Invalid`].
pub async fn update_window(
    conn: &mut PgConnection,
    id: PickupWindowId,
    input: &PickupWindowInput,
) -> Result<PickupWindow, Error> {
    validate_window(input)?;
    sqlx::query_as!(
        WindowRow,
        r#"UPDATE pickup_window SET weekday = $2, starts = make_interval(secs => $3::integer)::time,
            ends = make_interval(secs => $4::integer)::time, default_deadline_days_before = $5,
            default_deadline_time = make_interval(secs => $6::integer)::time, active = $7
        WHERE id = $1
        RETURNING id, location_id, weekday,
            extract(epoch FROM starts)::integer AS "starts_s!",
            extract(epoch FROM ends)::integer AS "ends_s!",
            default_deadline_days_before,
            extract(epoch FROM default_deadline_time)::integer AS "default_deadline_time_s!",
            active"#,
        id.get(),
        i16::from(input.weekday.to_monday_one_offset()),
        to_seconds_of_day(input.starts),
        to_seconds_of_day(input.ends),
        input.default_deadline_days_before,
        to_seconds_of_day(input.default_deadline_time),
        input.active,
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// A location's windows, inactive ones included, by weekday and start time.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_windows(
    conn: &mut PgConnection,
    location_id: PickupLocationId,
) -> Result<Vec<PickupWindow>, Error> {
    sqlx::query_as!(
        WindowRow,
        r#"SELECT id, location_id, weekday,
            extract(epoch FROM starts)::integer AS "starts_s!",
            extract(epoch FROM ends)::integer AS "ends_s!",
            default_deadline_days_before,
            extract(epoch FROM default_deadline_time)::integer AS "default_deadline_time_s!",
            active
        FROM pickup_window WHERE location_id = $1 ORDER BY weekday, starts, id"#,
        location_id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(PickupWindow::try_from)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(weekday: i16, starts_s: i32) -> WindowRow {
        WindowRow {
            id: 1,
            location_id: 1,
            weekday,
            starts_s,
            ends_s: 43_200,
            default_deadline_days_before: 2,
            default_deadline_time_s: 72_000,
            active: true,
        }
    }

    #[test]
    fn rows_with_bad_weekdays_or_times_are_corrupt() {
        assert!(PickupWindow::try_from(row(6, 39_600)).is_ok());
        assert!(matches!(
            PickupWindow::try_from(row(0, 39_600)),
            Err(Error::Corrupt(_))
        ));
        assert!(matches!(
            PickupWindow::try_from(row(400, 39_600)),
            Err(Error::Corrupt(_))
        ));
        assert!(matches!(
            PickupWindow::try_from(row(6, 90_000)),
            Err(Error::Corrupt(_))
        ));
    }
}
