//! The production list (F7) and the pickup list (F8): their data and their PDFs (ADR
//! 0016). Batches (F14a, ADR 0042).
#![forbid(unsafe_code)]

pub mod batches;
mod error;
mod paper;
mod pdf;
pub mod pickup;
pub mod production;
pub mod split;

pub use error::Error;
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
