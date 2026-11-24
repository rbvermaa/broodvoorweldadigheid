use bakedays::calendar::{self, Announcement};
use db::time::to_micros;
use domain::{BakeDayId, Clock};
use i18n::Translator;
use notify::subscribers::{self, Links, SubscriberError};
use notify::{Composer, DropAlert, DropAlertKind, Pickup};
use sqlx::{Acquire, PgConnection};

use crate::Error;

/// The alerts one tick queued, and those it could not compose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlertReport {
    pub queued: Vec<QueuedAlert>,
    pub failed: Vec<(BakeDayId, DropAlertKind)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedAlert {
    pub bake_day: BakeDayId,
    pub kind: DropAlertKind,
    pub recipients: u32,
}

const fn kind_str(kind: DropAlertKind) -> &'static str {
    match kind {
        DropAlertKind::MenuPublished => "menu_published",
        DropAlertKind::OrderingOpen => "ordering_open",
    }
}

fn parse_kind(kind: &str) -> Option<DropAlertKind> {
    [DropAlertKind::MenuPublished, DropAlertKind::OrderingOpen]
        .into_iter()
        .find(|k| kind_str(*k) == kind)
}

/// Queues the drop alerts that are due (F9a): "menu published" for an online draft whose
/// `menu_publish_at` has passed, "ordering open" for an open day, both only before the
/// deadline. Production-only and cancelled drops never alert, and a draft that is due to
/// open gets no menu alert, only the ordering alert once it opens. Run it after
/// [`crate::bake_days::tick`].
///
/// Each drop and kind goes out once: the `drop_alert_sent` row is inserted in the same
/// transaction that queues the emails, so a second tick, or a second server, finds it and
/// queues nothing. A drop that cannot be composed (no pickup slot, company details
/// missing) is logged, left without a row and tried again next tick.
///
/// # Errors
/// [`Error::Db`] or [`Error::BakeDays`].
pub async fn tick(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    i18n: &Translator,
    links: &Links,
) -> Result<AlertReport, Error> {
    let now = to_micros(clock.now());
    let due = sqlx::query!(
        r#"SELECT d.id, k.kind AS "kind!"
        FROM bake_day d
        CROSS JOIN (VALUES ('menu_published'), ('ordering_open')) AS k (kind)
        WHERE d.online_ordering
            AND (d.cutoff_at IS NULL
                OR d.cutoff_at > timestamptz 'epoch' + $1::bigint * interval '1 microsecond')
            AND ((k.kind = 'menu_published' AND d.status = 'draft'
                    AND d.menu_publish_at <= timestamptz 'epoch' + $1::bigint * interval '1 microsecond'
                    AND (d.opens_at IS NULL
                        OR d.opens_at > timestamptz 'epoch' + $1::bigint * interval '1 microsecond'))
                OR (k.kind = 'ordering_open' AND d.status = 'open'))
            AND NOT EXISTS (SELECT 1 FROM drop_alert_sent s
                WHERE s.bake_day_id = d.id AND s.kind = k.kind)
        ORDER BY d.date, d.id, k.kind"#,
        now,
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut report = AlertReport::default();
    if due.is_empty() {
        return Ok(report);
    }
    let company = notify::Company::load(conn).await?;
    let composer = Composer::new(i18n, &company);
    for row in due {
        let Some(kind) = parse_kind(&row.kind) else {
            continue;
        };
        let day = BakeDayId::new(row.id);
        match send(conn, clock, &composer, links, day, kind).await {
            Ok(Some(recipients)) => report.queued.push(QueuedAlert {
                bake_day: day,
                kind,
                recipients,
            }),
            Ok(None) => {}
            Err(Failure::Skip(reason)) => {
                tracing::error!(bake_day = day.get(), ?kind, reason, "drop alert not sent");
                report.failed.push((day, kind));
            }
            Err(Failure::Fatal(err)) => return Err(err),
        }
    }
    Ok(report)
}

enum Failure {
    Skip(String),
    Fatal(Error),
}

impl From<sqlx::Error> for Failure {
    fn from(err: sqlx::Error) -> Self {
        Self::Fatal(err.into())
    }
}

impl From<SubscriberError> for Failure {
    fn from(err: SubscriberError) -> Self {
        match err {
            SubscriberError::Db(err) => Self::Fatal(err.into()),
            other => Self::Skip(other.to_string()),
        }
    }
}

/// One drop and kind in its own transaction. `None` when the day no longer qualifies
/// or another tick already sent it.
async fn send(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    composer: &Composer<'_>,
    links: &Links,
    day: BakeDayId,
    kind: DropAlertKind,
) -> Result<Option<u32>, Failure> {
    let now = to_micros(clock.now());
    let mut tx = conn.begin().await?;
    // FOR SHARE: a staff edit or cancel holds the row FOR UPDATE; then this waits for the
    // next tick instead of alerting about a day that is changing.
    let still_due = sqlx::query_scalar!(
        "SELECT id FROM bake_day
        WHERE id = $1 AND online_ordering
            AND (cutoff_at IS NULL
                OR cutoff_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond')
            AND (($3 = 'menu_published' AND status = 'draft'
                    AND menu_publish_at <= timestamptz 'epoch' + $2::bigint * interval '1 microsecond'
                    AND (opens_at IS NULL
                        OR opens_at > timestamptz 'epoch' + $2::bigint * interval '1 microsecond'))
                OR ($3 = 'ordering_open' AND status = 'open'))
        FOR SHARE SKIP LOCKED",
        day.get(),
        now,
        kind_str(kind),
    )
    .fetch_optional(&mut *tx)
    .await?;
    if still_due.is_none() {
        return Ok(None);
    }
    let claimed = sqlx::query_scalar!(
        "INSERT INTO drop_alert_sent (bake_day_id, kind, recipients, sent_at)
        VALUES ($1, $2, 0, timestamptz 'epoch' + $3::bigint * interval '1 microsecond')
        ON CONFLICT DO NOTHING
        RETURNING bake_day_id",
        day.get(),
        kind_str(kind),
        now,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if claimed.is_none() {
        return Ok(None);
    }
    let drop = match calendar::announcement(&mut tx, day).await {
        Ok(drop) => drop,
        Err(bakedays::Error::Db(err)) => return Err(err.into()),
        Err(err) => return Err(Failure::Skip(err.to_string())),
    };
    let Some(pickup) = first_pickup(&drop) else {
        return Err(Failure::Skip("the drop has no pickup slot".to_owned()));
    };
    let mut recipients: u32 = 0;
    for subscriber in subscribers::active(&mut tx).await? {
        let unsubscribe_url =
            subscribers::unsubscribe_link(&mut tx, clock, links, subscriber.id).await?;
        let email = composer
            .drop_alert(&DropAlert {
                to: subscriber.email,
                kind,
                drop_date: drop.date,
                pickup: pickup.clone(),
                deadline: drop.cutoff_at,
                charity: drop.charity.as_ref().map(|c| c.name.clone()),
                menu: drop.menu.iter().map(|m| m.name.clone()).collect(),
                shop_url: links.drop_page(day),
                unsubscribe_url,
            })
            .map_err(|err| Failure::Skip(err.to_string()))?;
        let key = format!(
            "drop-alert:{}:{}:{}",
            day.get(),
            kind_str(kind),
            subscriber.id
        );
        notify::enqueue(&mut tx, clock, &email, Some(&key))
            .await
            .map_err(|err| match err {
                notify::OutboxError::Db(err) => Failure::from(err),
                other => Failure::Skip(other.to_string()),
            })?;
        recipients += 1;
    }
    sqlx::query!(
        "UPDATE drop_alert_sent SET recipients = $3 WHERE bake_day_id = $1 AND kind = $2",
        day.get(),
        kind_str(kind),
        i32::try_from(recipients).unwrap_or(i32::MAX),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(recipients))
}

/// Alerts name one ophaalmoment: the first.
fn first_pickup(drop: &Announcement) -> Option<Pickup> {
    drop.pickups.first().map(|p| Pickup {
        starts: p.starts_at,
        ends: p.ends_at,
        location: p.location_name.clone(),
        address: p.address.clone(),
    })
}
