use db::time::{to_epoch_days, to_micros};
use domain::{BakeDayId, BatchId, Clock};
use sqlx::PgConnection;

use crate::Error;

/// The bake days one tick split into batches, and the batches made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SplitReport {
    pub days: Vec<BakeDayId>,
    pub created: Vec<BatchId>,
}

/// Splits each closed bake day into batches once its order deadline has passed and no
/// order is still within its payment hold, so an order paid just after the deadline is in
/// the split (F14a).
/// Days whose date is already past are left alone, so setting the mixer capacity later
/// does not fill old days. Without a mixer capacity nothing happens; the back office says
/// so. Safe to run twice and from two servers: each day is split once
/// ([`production::batches::split_at_deadline`]).
///
/// # Errors
/// [`Error::Db`], or the first [`Error::Production`] after trying every due day; a day that
/// failed is tried again next tick.
pub async fn tick(conn: &mut PgConnection, clock: &dyn Clock) -> Result<SplitReport, Error> {
    let mut report = SplitReport::default();
    if production::batches::capacity(conn).await?.is_none() {
        return Ok(report);
    }
    let now = clock.now();
    let due = sqlx::query_scalar!(
        r#"SELECT d.id FROM bake_day d
        WHERE d.status = 'closed'
            AND d.cutoff_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
            AND d.date >= date 'epoch' + $2::integer
            AND NOT EXISTS (SELECT 1 FROM batch_split s WHERE s.bake_day_id = d.id)
            AND NOT EXISTS (
                SELECT 1 FROM "order" o
                WHERE o.bake_day_id = d.id AND o.status = 'pending_payment'
                    AND o.hold_expires_at > timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
            )
        ORDER BY d.id"#,
        to_micros(now),
        to_epoch_days(clock.now_in_bakery().date()),
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut first_error = None;
    for id in due.into_iter().map(BakeDayId::new) {
        let split = match production::batches::split_at_deadline(conn, id, clock).await {
            Ok(split) => split,
            Err(error) => {
                tracing::error!(bake_day = id.get(), %error, "cannot split bake day into batches");
                first_error.get_or_insert(error);
                continue;
            }
        };
        if !split.already_split {
            for skipped in &split.skipped {
                tracing::warn!(
                    bake_day = id.get(),
                    product = skipped.product_id.get(),
                    reason = ?skipped.reason,
                    "product not split into batches"
                );
            }
            report.days.push(id);
            report.created.extend(split.created);
        }
    }
    match first_error {
        Some(error) => Err(error.into()),
        None => Ok(report),
    }
}
