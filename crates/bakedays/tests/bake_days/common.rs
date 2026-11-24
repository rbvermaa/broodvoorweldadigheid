use bakedays::{BakeDay, NewBakeDay, days};
use domain::{CharityId, PickupLocationId, ProductId};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::PgConnection;

pub fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

pub fn date(s: &str) -> Date {
    s.parse().unwrap()
}

pub const DE_PROEF: PickupLocationId = PickupLocationId::new(1);

/// Saturday after the October change: pickup 11:00 CET, deadline Thursday 20:00 CET.
pub fn saturday() -> NewBakeDay {
    NewBakeDay {
        date: date("2026-10-31"),
        location_id: DE_PROEF,
        bake_starts_at: ts("2026-10-31T03:00:00Z"),
        online_ordering: true,
        charity_id: None,
    }
}

pub async fn create_day(conn: &mut PgConnection, input: &NewBakeDay) -> BakeDay {
    days::create(conn, input).await.unwrap()
}

pub async fn product(
    conn: &mut PgConnection,
    name: &str,
    lead_time_hours: Option<i32>,
) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece, lead_time_hours)
         VALUES ($1, 500, 9, 800, 900, $2) RETURNING id",
    )
    .bind(name)
    .bind(lead_time_hours)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

pub async fn charity(conn: &mut PgConnection, name: &str, archived: bool) -> CharityId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO charity (name, url, archived_at)
         VALUES ($1, 'https://example.org', CASE WHEN $2 THEN now() END) RETURNING id",
    )
    .bind(name)
    .bind(archived)
    .fetch_one(conn)
    .await
    .unwrap();
    CharityId::new(id)
}
