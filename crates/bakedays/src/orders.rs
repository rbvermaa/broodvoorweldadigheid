use std::collections::BTreeMap;

use domain::{BakeDayId, ProductId};
use sqlx::PgConnection;

use crate::Error;

/// What bakedays needs to know about orders, which live in a higher crate (ADR 0008).
/// Every method is called inside the caller's transaction, after bakedays has taken its
/// locks, so the answer cannot go stale before the write commits.
pub trait OrderSource: Sync {
    /// Pieces taken by orders (paid, or holding capacity) per product on the day.
    fn ordered(
        &self,
        conn: &mut PgConnection,
        day: BakeDayId,
    ) -> impl Future<Output = Result<BTreeMap<ProductId, u32>, Error>> + Send;

    /// Number of orders on the day, for the charity rule (F4c).
    fn count(
        &self,
        conn: &mut PgConnection,
        day: BakeDayId,
    ) -> impl Future<Output = Result<u64, Error>> + Send;
}

/// The order source until the orders slice exists: there are no orders.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOrders;

impl OrderSource for NoOrders {
    async fn ordered(
        &self,
        _conn: &mut PgConnection,
        _day: BakeDayId,
    ) -> Result<BTreeMap<ProductId, u32>, Error> {
        Ok(BTreeMap::new())
    }

    async fn count(&self, _conn: &mut PgConnection, _day: BakeDayId) -> Result<u64, Error> {
        Ok(0)
    }
}
