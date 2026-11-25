//! The production list (F7), the pickup list (F8) and the ingredient needs (F15): their
//! data and their PDFs. See ADR 0016 and ADR 0041.
#![forbid(unsafe_code)]

mod error;
pub mod needs;
mod paper;
mod pdf;
pub mod pickup;
pub mod production;

pub use error::Error;
pub use needs::{
    Dough, DoughProduct, IngredientNeeds, Missing, MissingReason, ingredient_needs,
    ingredient_needs_csv, ingredient_needs_pdf, weight,
};
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
