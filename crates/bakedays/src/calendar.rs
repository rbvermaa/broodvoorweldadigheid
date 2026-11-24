use std::collections::BTreeSet;

use db::time::from_micros;
use domain::{BakeDayId, Clock, PickupSlotId, ProductId};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::PgConnection;

use crate::orders::OrderSource;
use crate::status::{BakeDayStatus, PublicStatus};
use crate::{Error, days, range};

/// One drop on the public calendar (F1a). `charity` is `None` for a normal sale; `menu`
/// is `None` until the menu is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicDrop {
    pub bake_day_id: BakeDayId,
    pub date: Date,
    pub status: PublicStatus,
    pub cutoff_at: Option<Timestamp>,
    pub pickups: Vec<PublicPickup>,
    pub charity: Option<PublicCharity>,
    pub menu: Option<Vec<MenuItem>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicPickup {
    pub slot_id: PickupSlotId,
    pub location_name: String,
    pub address: String,
    pub starts_at: Timestamp,
    pub ends_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicCharity {
    pub name: String,
    pub description: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub product_id: ProductId,
    pub name: String,
    pub description: String,
}

/// The menu is public once its publish time has passed, or once ordering has opened. A
/// cancelled drop shows no menu.
fn menu_public(day: &days::BakeDay, now: Timestamp) -> bool {
    match day.status {
        BakeDayStatus::Cancelled => false,
        BakeDayStatus::Open | BakeDayStatus::Closed | BakeDayStatus::Done => true,
        BakeDayStatus::Draft => day.menu_publish_at.is_some_and(|at| at <= now),
    }
}

/// Drops with `from <= date <= to` for the public calendar, by date. Production-only days
/// are left out. The menu and sold out only count active products sold online. A display
/// read: it takes no locks.
///
/// # Errors
/// [`Error::Db`], [`Error::Catalog`] or [`Error::Corrupt`].
pub async fn public_drops(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    from: Date,
    to: Date,
    orders: &impl OrderSource,
) -> Result<Vec<PublicDrop>, Error> {
    let now = clock.now();
    let mut drops = Vec::new();
    for day in days::list_between(conn, from, to).await? {
        if !day.online_ordering {
            continue;
        }
        let shown_menu = menu(conn, day.id).await?;
        let sold_out = if day.status == BakeDayStatus::Open {
            let shown: BTreeSet<ProductId> = shown_menu.iter().map(|m| m.product_id).collect();
            let available: Vec<_> = range::availability(conn, day.id, orders)
                .await?
                .into_iter()
                .filter(|a| shown.contains(&a.product_id))
                .collect();
            range::sold_out(&available)
        } else {
            false
        };
        let Some(status) = day.public_status(now, sold_out) else {
            continue;
        };
        let charity = match day.charity_id {
            Some(id) => {
                let charity = catalog::charities::get(conn, id).await?;
                Some(PublicCharity {
                    name: charity.name,
                    description: charity.description,
                    url: charity.url,
                })
            }
            None => None,
        };
        drops.push(PublicDrop {
            bake_day_id: day.id,
            date: day.date,
            status,
            cutoff_at: day.cutoff_at,
            pickups: pickups(conn, day.id).await?,
            charity,
            menu: menu_public(&day, now).then_some(shown_menu),
        });
    }
    Ok(drops)
}

async fn pickups(conn: &mut PgConnection, day: BakeDayId) -> Result<Vec<PublicPickup>, Error> {
    sqlx::query!(
        r#"SELECT s.id, l.name, l.address,
            (extract(epoch FROM s.starts_at) * 1000000)::bigint AS "starts_at_us!",
            (extract(epoch FROM s.ends_at) * 1000000)::bigint AS "ends_at_us!"
        FROM pickup_slot s JOIN pickup_location l ON l.id = s.location_id
        WHERE s.bake_day_id = $1 ORDER BY s.starts_at, s.id"#,
        day.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(PublicPickup {
            slot_id: PickupSlotId::new(row.id),
            location_name: row.name,
            address: row.address,
            starts_at: from_micros(row.starts_at_us)?,
            ends_at: from_micros(row.ends_at_us)?,
        })
    })
    .collect()
}

async fn menu(conn: &mut PgConnection, day: BakeDayId) -> Result<Vec<MenuItem>, Error> {
    Ok(sqlx::query!(
        "SELECT p.id, p.name_nl, p.description_nl
        FROM bake_day_product r JOIN product p ON p.id = r.product_id
        WHERE r.bake_day_id = $1 AND p.active AND p.sold_online
        ORDER BY p.name_nl, p.id",
        day.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| MenuItem {
        product_id: ProductId::new(row.id),
        name: row.name_nl,
        description: row.description_nl,
    })
    .collect())
}
