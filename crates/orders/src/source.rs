use std::collections::BTreeMap;

use bakedays::OrderSource;
use db::time::to_micros;
use domain::{BakeDayId, Clock, ProductId};
use sqlx::PgConnection;

/// The orders that take capacity, for bakedays (ADR 0008, ADR 0012): paid, confirmed or
/// picked up, or waiting for payment within their hold.
#[derive(Clone, Copy)]
pub struct Orders<'a> {
    clock: &'a dyn Clock,
}

impl<'a> Orders<'a> {
    #[must_use]
    pub const fn new(clock: &'a dyn Clock) -> Self {
        Self { clock }
    }
}

fn corrupt(what: &str, value: i64) -> bakedays::Error {
    bakedays::Error::Corrupt(format!("{what} {value}"))
}

impl OrderSource for Orders<'_> {
    async fn ordered(
        &self,
        conn: &mut PgConnection,
        day: BakeDayId,
    ) -> Result<BTreeMap<ProductId, u32>, bakedays::Error> {
        let now = to_micros(self.clock.now());
        sqlx::query!(
            r#"SELECT l.product_id, sum(l.qty)::bigint AS "qty!"
            FROM order_line l JOIN "order" o ON o.id = l.order_id
            WHERE o.bake_day_id = $1
                AND (o.status IN ('paid', 'confirmed', 'picked_up')
                    OR (o.status = 'pending_payment'
                        AND o.hold_expires_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'))
            GROUP BY l.product_id"#,
            day.get(),
            now,
        )
        .fetch_all(conn)
        .await?
        .into_iter()
        .map(|row| {
            let qty = u32::try_from(row.qty).map_err(|_| corrupt("ordered", row.qty))?;
            Ok((ProductId::new(row.product_id), qty))
        })
        .collect()
    }

    /// Orders that came in: every order except one cancelled before it was paid. A
    /// pending order counts until the release job cancels it (ADR 0012).
    async fn count(&self, conn: &mut PgConnection, day: BakeDayId) -> Result<u64, bakedays::Error> {
        let count = sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!" FROM "order"
            WHERE bake_day_id = $1 AND (status <> 'cancelled' OR paid_at IS NOT NULL)"#,
            day.get(),
        )
        .fetch_one(conn)
        .await?;
        u64::try_from(count).map_err(|_| corrupt("order count", count))
    }
}
