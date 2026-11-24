use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bakedays::{BakeDay, Error, NewBakeDay, OrderSource, days, extras};
use domain::{BakeDayId, CharityId, PickupLocationId, ProductId};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::{PgConnection, PgPool};

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
        online_ordering: true,
        charity_id: None,
    }
}

/// The Saturday a week after [`saturday`].
pub fn next_saturday() -> NewBakeDay {
    NewBakeDay {
        date: date("2026-11-07"),
        ..saturday()
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

/// Orders as a fixed answer, for tests that do not race.
#[derive(Debug, Default)]
pub struct FixedOrders {
    pub ordered: BTreeMap<ProductId, u32>,
    pub count: u64,
}

impl FixedOrders {
    pub fn pieces<const N: usize>(ordered: [(ProductId, u32); N]) -> Self {
        Self {
            ordered: BTreeMap::from(ordered),
            count: 1,
        }
    }

    pub fn count(count: u64) -> Self {
        Self {
            count,
            ..Self::default()
        }
    }
}

impl OrderSource for FixedOrders {
    async fn ordered(
        &self,
        _conn: &mut PgConnection,
        _day: BakeDayId,
    ) -> Result<BTreeMap<ProductId, u32>, Error> {
        Ok(self.ordered.clone())
    }

    async fn count(&self, _conn: &mut PgConnection, _day: BakeDayId) -> Result<u64, Error> {
        Ok(self.count)
    }
}

/// Orders kept in a test table, read inside the caller's transaction like the real
/// orders crate will.
pub struct TableOrders;

impl TableOrders {
    pub async fn create_table(conn: &mut PgConnection) {
        sqlx::query(
            "CREATE TABLE test_order_line (bake_day_id bigint, product_id bigint, qty integer)",
        )
        .execute(conn)
        .await
        .unwrap();
    }

    pub async fn insert(conn: &mut PgConnection, day: BakeDayId, product: ProductId, qty: i32) {
        sqlx::query("INSERT INTO test_order_line VALUES ($1, $2, $3)")
            .bind(day.get())
            .bind(product.get())
            .bind(qty)
            .execute(conn)
            .await
            .unwrap();
    }
}

impl OrderSource for TableOrders {
    async fn ordered(
        &self,
        conn: &mut PgConnection,
        day: BakeDayId,
    ) -> Result<BTreeMap<ProductId, u32>, Error> {
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT product_id, sum(qty)::bigint FROM test_order_line WHERE bake_day_id = $1 GROUP BY product_id",
        )
        .bind(day.get())
        .fetch_all(conn)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(product, qty)| (ProductId::new(product), u32::try_from(qty).unwrap()))
            .collect())
    }

    async fn count(&self, conn: &mut PgConnection, day: BakeDayId) -> Result<u64, Error> {
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM test_order_line WHERE bake_day_id = $1")
                .bind(day.get())
                .fetch_one(conn)
                .await?;
        Ok(u64::try_from(count).unwrap())
    }
}

pub async fn extras_total(conn: &mut PgConnection, day: BakeDayId) -> BTreeMap<ProductId, u32> {
    let mut total = BTreeMap::new();
    for extra in extras::list(conn, day).await.unwrap() {
        *total.entry(extra.product_id).or_default() += extra.qty;
    }
    total
}

pub async fn backend_pid(conn: &mut PgConnection) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(conn)
        .await
        .unwrap()
}

/// Waits until the backend `pid` is blocked on a lock held by another session.
pub async fn wait_until_blocked(pool: &PgPool, pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut conn = pool.acquire().await.unwrap();
    loop {
        let blocked: bool = sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
            .bind(pid)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        if blocked {
            return;
        }
        assert!(Instant::now() < deadline, "backend {pid} never blocked");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
