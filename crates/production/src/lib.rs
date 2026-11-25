//! The production list (F7) and the pickup list (F8): their data and their PDFs. See
//! ADR 0016. Also lays out the B2B invoice overview (F11a) for `reports` (ADR 0034).
#![forbid(unsafe_code)]

mod error;
mod invoice;
mod paper;
mod pdf;
pub mod pickup;
pub mod production;

pub use error::Error;
pub use invoice::{invoice_overview_pdf, invoice_overview_text};
pub use pickup::{PickupLine, PickupList, PickupOrder, SlotPickups, pickup_list, pickup_pdf};
pub use production::{Extras, ProductTotal, ProductionList, production_list, production_pdf};

/// A read-only transaction in which every query sees the same committed data.
async fn snapshot(
    conn: &mut sqlx::PgConnection,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, Error> {
    use sqlx::Acquire;
    let mut tx = conn.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

#[cfg(test)]
mod tests;
