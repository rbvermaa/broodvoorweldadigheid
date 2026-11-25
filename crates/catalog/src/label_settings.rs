//! What a product's labels need beyond its allergen data (F18, F18a, ADR 0047).

use domain::ProductId;
use sqlx::PgConnection;

use crate::Error;

/// The longest shelf life or freezer period, in days.
pub const MAX_DAYS: u16 = 3650;
/// The longest storage instruction, in characters.
pub const MAX_STORAGE_CHARS: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelSettings {
    /// Sold packaged, so it gets a product label (F18).
    pub packaged: bool,
    /// Best-before is the bake date plus this; no product label without it.
    pub shelf_life_days: Option<u16>,
    /// Best-before of a freezer label is the freeze date plus this; no freezer label
    /// without it.
    pub freezer_days: Option<u16>,
    /// The product label's storage instruction; empty means none, which blocks it.
    pub storage: String,
}

struct Row {
    packaged: bool,
    shelf_life_days: Option<i32>,
    freezer_days: Option<i32>,
    storage_nl: String,
}

fn days(value: Option<i32>) -> Result<Option<u16>, Error> {
    value
        .map(|v| {
            u16::try_from(v)
                .ok()
                .filter(|d| (1..=MAX_DAYS).contains(d))
                .ok_or_else(|| Error::Corrupt(format!("{v} days")))
        })
        .transpose()
}

/// # Errors
/// [`Error::NotFound`] for an unknown product.
pub async fn get(conn: &mut PgConnection, product: ProductId) -> Result<LabelSettings, Error> {
    let row = sqlx::query_as!(
        Row,
        "SELECT packaged, shelf_life_days, freezer_days, storage_nl FROM product WHERE id = $1",
        product.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(LabelSettings {
        packaged: row.packaged,
        shelf_life_days: days(row.shelf_life_days)?,
        freezer_days: days(row.freezer_days)?,
        storage: row.storage_nl,
    })
}

/// # Errors
/// [`Error::Invalid`] for a period outside 1 to [`MAX_DAYS`] or a storage instruction
/// over [`MAX_STORAGE_CHARS`]; [`Error::NotFound`] for an unknown product.
pub async fn set(
    conn: &mut PgConnection,
    product: ProductId,
    settings: &LabelSettings,
) -> Result<(), Error> {
    let valid = |d: Option<u16>| d.is_none_or(|d| (1..=MAX_DAYS).contains(&d));
    if !valid(settings.shelf_life_days) {
        return Err(Error::Invalid("shelf life"));
    }
    if !valid(settings.freezer_days) {
        return Err(Error::Invalid("freezer period"));
    }
    let storage = settings.storage.trim();
    if storage.chars().count() > MAX_STORAGE_CHARS {
        return Err(Error::Invalid("storage instruction"));
    }
    let updated = sqlx::query!(
        "UPDATE product SET packaged = $2, shelf_life_days = $3, freezer_days = $4,
            storage_nl = $5
        WHERE id = $1",
        product.get(),
        settings.packaged,
        settings.shelf_life_days.map(i32::from),
        settings.freezer_days.map(i32::from),
        storage,
    )
    .execute(conn)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}
