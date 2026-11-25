use std::time::{Duration, Instant};

use bakedays::{BakeDay, BakeDayProduct, NewBakeDay, Schedule, days, range};
use domain::{CharityId, PickupLocationId, PickupSlotId, ProductId};
use jiff::Timestamp;
use orders::{Cart, CustomerInput, Error, Order, OrderedBy, checkout};
use sqlx::{PgConnection, PgPool};
use testkit::FakeClock;

pub fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

pub const DE_PROEF: PickupLocationId = PickupLocationId::new(1);

/// Saturday after the October change: pickup 11:00 CET, deadline Thursday 20:00 CET
/// (2026-10-29T19:00:00Z).
pub fn saturday() -> NewBakeDay {
    NewBakeDay {
        date: "2026-10-31".parse().unwrap(),
        location_id: DE_PROEF,
        online_ordering: true,
        charity_id: None,
    }
}

/// Wednesday before the deadline of [`saturday`].
pub fn wednesday() -> FakeClock {
    FakeClock::new(ts("2026-10-28T12:00:00Z"))
}

pub struct OpenDay {
    pub day: BakeDay,
    pub slot: PickupSlotId,
}

/// Creates the bake day and opens it at the clock's time.
pub async fn open_day(conn: &mut PgConnection, clock: &FakeClock, input: &NewBakeDay) -> OpenDay {
    let day = days::create(conn, input).await.unwrap();
    let day = days::update_schedule(
        conn,
        day.id,
        &Schedule {
            online_ordering: day.online_ordering,
            menu_publish_at: None,
            opens_at: Some(ts("2026-10-01T00:00:00Z")),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    let day = days::open(conn, clock, day.id).await.unwrap();
    let slot = days::list_slots(conn, day.id).await.unwrap()[0].id;
    OpenDay { day, slot }
}

pub struct ProductSpec {
    pub name: &'static str,
    pub price: i64,
    pub vat: i16,
    pub pwyw: Option<(i64, i64)>,
    pub sold_online: bool,
}

pub const BREAD: ProductSpec = ProductSpec {
    name: "Desem",
    price: 450,
    vat: 9,
    pwyw: None,
    sold_online: true,
};

pub const DONATION_LOAF: ProductSpec = ProductSpec {
    name: "Donatiebrood",
    price: 600,
    vat: 9,
    pwyw: Some((300, 1500)),
    sold_online: true,
};

pub const COOKIES: ProductSpec = ProductSpec {
    name: "Koekjes",
    price: 395,
    vat: 21,
    pwyw: None,
    sold_online: true,
};

/// An active product with approved allergen data; also approves the kitchen-wide
/// may-contain list.
pub async fn product(conn: &mut PgConnection, spec: &ProductSpec) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role) VALUES ('test-baker', 'Testbakker', 'baker')
            ON CONFLICT (oidc_sub) DO UPDATE SET name = EXCLUDED.name RETURNING id),
         kitchen AS (UPDATE company SET may_contain_approved_by = (SELECT id FROM baker),
            may_contain_approved_at = '2026-10-01T10:00:00Z')
         INSERT INTO product (name_nl, price_incl_cents, vat_rate, pay_what_you_want,
            min_price_cents, max_price_cents, unit_weight_g, dough_g_per_piece, sold_online,
            allergens_approved_by, allergens_approved_at)
         VALUES ($1, $2, $3, $4, $5, $6, 800, 900, $7, (SELECT id FROM baker), '2026-10-01T10:00:00Z') RETURNING id",
    )
    .bind(spec.name)
    .bind(spec.price)
    .bind(spec.vat)
    .bind(spec.pwyw.is_some())
    .bind(spec.pwyw.map(|p| p.0))
    .bind(spec.pwyw.map(|p| p.1))
    .bind(spec.sold_online)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

pub async fn on_range(
    conn: &mut PgConnection,
    shop: &OpenDay,
    product_id: ProductId,
    max_qty: Option<u32>,
) {
    range::set_product(
        conn,
        shop.day.id,
        &BakeDayProduct {
            product_id,
            max_qty,
        },
    )
    .await
    .unwrap();
}

pub async fn charity(conn: &mut PgConnection, name: &str) -> CharityId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO charity (name, url) VALUES ($1, 'https://example.org') RETURNING id",
    )
    .bind(name)
    .fetch_one(conn)
    .await
    .unwrap();
    CharityId::new(id)
}

pub fn guest(email: &str) -> CustomerInput {
    CustomerInput {
        email: email.to_owned(),
        name: "Test Klant".to_owned(),
        phone: None,
        marketing_opt_in: false,
    }
}

pub fn cart(shop: &OpenDay, lines: &[(ProductId, u32, Option<i64>)]) -> Cart {
    let mut cart = Cart::new(shop.day.id);
    for &(product, qty, price) in lines {
        cart.set(product, qty, price.map(domain::Money::from_cents))
            .unwrap();
    }
    cart
}

pub async fn order_as_guest(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &OpenDay,
    lines: &[(ProductId, u32, Option<i64>)],
) -> Result<Order, Error> {
    checkout(
        conn,
        clock,
        &cart(shop, lines),
        &guest("klant@example.org"),
        shop.slot,
        OrderedBy::Guest,
    )
    .await
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
