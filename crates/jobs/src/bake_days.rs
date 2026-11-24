use bakedays::{BakeDay, days};
use db::time::to_micros;
use domain::{BakeDayId, Clock};
use jiff::Timestamp;
use sqlx::{Acquire, PgConnection};

use crate::Error;

/// The bake days one tick opened and closed, and those whose transition was refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReport {
    pub opened: Vec<BakeDayId>,
    pub closed: Vec<BakeDayId>,
    pub refused: Vec<BakeDayId>,
}

/// One instant for the whole tick, so the rows picked and the transition checks agree.
struct Frozen(Timestamp);

impl Clock for Frozen {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// Opens online drafts whose `opens_at` has passed while their deadline has not, and
/// closes open days whose deadline has passed (F1). Menu publication needs no write: the
/// public status follows `menu_publish_at` (ADR 0008).
///
/// Safe to run twice and from two servers at once: rows are taken `FOR UPDATE SKIP
/// LOCKED`, so a row another session holds is left for the next tick, and a row already
/// moved no longer matches. A refused transition is reported and does not stop the rest.
///
/// # Errors
/// [`Error::Db`], or [`Error::BakeDays`] for anything but a refused transition.
pub async fn tick(conn: &mut PgConnection, clock: &dyn Clock) -> Result<TickReport, Error> {
    let clock = Frozen(clock.now());
    let now = to_micros(clock.0);
    let mut tx = conn.begin().await?;
    let to_open = sqlx::query_scalar!(
        "SELECT id FROM bake_day
        WHERE status = 'draft' AND online_ordering
            AND opens_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
            AND cutoff_at > timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
        ORDER BY id FOR UPDATE SKIP LOCKED",
        now,
    )
    .fetch_all(&mut *tx)
    .await?;
    let to_close = sqlx::query_scalar!(
        "SELECT id FROM bake_day
        WHERE status = 'open'
            AND cutoff_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
        ORDER BY id FOR UPDATE SKIP LOCKED",
        now,
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut report = TickReport::default();
    for id in to_open.into_iter().map(BakeDayId::new) {
        let result = days::open(&mut tx, &clock, id).await;
        record(&mut report.opened, &mut report.refused, id, result)?;
    }
    for id in to_close.into_iter().map(BakeDayId::new) {
        let result = days::close(&mut tx, &clock, id).await;
        record(&mut report.closed, &mut report.refused, id, result)?;
    }
    tx.commit().await?;
    Ok(report)
}

fn record(
    done: &mut Vec<BakeDayId>,
    refused: &mut Vec<BakeDayId>,
    id: BakeDayId,
    result: Result<BakeDay, bakedays::Error>,
) -> Result<(), Error> {
    match result {
        Ok(_) => done.push(id),
        Err(bakedays::Error::Transition(reason)) => {
            tracing::warn!(bake_day = id.get(), %reason, "bake day transition refused");
            refused.push(id);
        }
        Err(err) => return Err(err.into()),
    }
    Ok(())
}
