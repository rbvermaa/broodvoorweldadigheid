use bakedays::BakeDayProduct;
use bakedays::{NewBakeDay, Schedule, days, range};
use domain::{
    BakeDayId, CharityId, CustomerId, Money, OrderId, PickupLocationId, PickupSlotId, ProductId,
};
use jiff::Timestamp;
use orders::{
    Cart, CustomerInput, Order, OrderedBy, PaidOutcome, PaymentReceived, checkout, mark_paid,
};
use sqlx::PgConnection;
use testkit::FakeClock;

pub fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

pub fn date(s: &str) -> jiff::civil::Date {
    s.parse().unwrap()
}

const DE_PROEF: PickupLocationId = PickupLocationId::new(1);

pub struct Shop {
    pub day: BakeDayId,
    pub slot: PickupSlotId,
}

/// An open Saturday drop (pickup at De Proef), open for orders from 1 September.
pub async fn drop_on(
    conn: &mut PgConnection,
    clock: &FakeClock,
    saturday: &str,
    charity: Option<CharityId>,
) -> Shop {
    let day = days::create(
        conn,
        &NewBakeDay {
            date: date(saturday),
            location_id: DE_PROEF,
            online_ordering: true,
            charity_id: charity,
        },
    )
    .await
    .unwrap();
    let day = days::update_schedule(
        conn,
        day.id,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(ts("2026-09-01T00:00:00Z")),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    let day = days::open(conn, clock, day.id).await.unwrap();
    let slot = days::list_slots(conn, day.id).await.unwrap()[0].id;
    Shop { day: day.id, slot }
}

pub struct ProductSpec {
    pub name: &'static str,
    pub price: i64,
    pub vat: i16,
    pub pwyw: Option<(i64, i64)>,
    pub cost: Option<i64>,
}

pub const BREAD: ProductSpec = ProductSpec {
    name: "Desem",
    price: 545,
    vat: 9,
    pwyw: None,
    cost: Some(120),
};

pub const DONATION_LOAF: ProductSpec = ProductSpec {
    name: "Donatiebrood",
    price: 600,
    vat: 9,
    pwyw: Some((300, 1500)),
    cost: None,
};

pub const COOKIES: ProductSpec = ProductSpec {
    name: "Koekjes",
    price: 363,
    vat: 21,
    pwyw: None,
    cost: Some(80),
};

pub async fn product(conn: &mut PgConnection, spec: &ProductSpec) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role) VALUES ('test-baker', 'Testbakker', 'baker')
            ON CONFLICT (oidc_sub) DO UPDATE SET name = EXCLUDED.name RETURNING id)
         INSERT INTO product (name_nl, price_incl_cents, vat_rate, pay_what_you_want,
            min_price_cents, max_price_cents, unit_weight_g, dough_g_per_piece,
            estimated_cost_cents, allergens_approved_by, allergens_approved_at)
         VALUES ($1, $2, $3, $4, $5, $6, 800, 900, $7, (SELECT id FROM baker), '2026-08-01T10:00:00Z') RETURNING id",
    )
    .bind(spec.name)
    .bind(spec.price)
    .bind(spec.vat)
    .bind(spec.pwyw.is_some())
    .bind(spec.pwyw.map(|p| p.0))
    .bind(spec.pwyw.map(|p| p.1))
    .bind(spec.cost)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

pub async fn sell(conn: &mut PgConnection, shop: &Shop, products: &[ProductId]) {
    for &product_id in products {
        range::set_product(
            conn,
            shop.day,
            &BakeDayProduct {
                product_id,
                max_qty: None,
                capacity_pool_id: None,
                units_per_item: 1,
            },
        )
        .await
        .unwrap();
    }
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

fn guest(n: u32) -> CustomerInput {
    CustomerInput {
        email: format!("klant{n}@example.org"),
        name: "Test Klant".to_owned(),
        phone: None,
        marketing_opt_in: false,
    }
}

/// Checks out and pays at the clock's time; returns the paid order.
pub async fn paid_order(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &Shop,
    lines: &[(ProductId, u32, Option<i64>)],
) -> Order {
    let mut cart = Cart::new(shop.day);
    for &(product, qty, price) in lines {
        cart.set(product, qty, price.map(Money::from_cents))
            .unwrap();
    }
    let n: u32 = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM \"order\"")
        .fetch_one(&mut *conn)
        .await
        .unwrap()
        .try_into()
        .unwrap();
    let order = checkout(conn, clock, &cart, &guest(n), shop.slot, OrderedBy::Guest)
        .await
        .unwrap();
    let outcome = mark_paid(
        conn,
        clock,
        order.id,
        &PaymentReceived {
            reference: format!("tr_test{}", order.id.get()),
            amount: order.total,
        },
    )
    .await
    .unwrap();
    match outcome {
        PaidOutcome::Paid { order, .. } => order,
        other => panic!("not paid: {other:?}"),
    }
}

/// What the payments crate does when Mollie reports a refund or a fee.
pub async fn refund(conn: &mut PgConnection, order: OrderId, cents: i64) {
    sqlx::query("UPDATE payment SET refunded_cents = refunded_cents + $2 WHERE order_id = $1 AND NOT refund_due")
        .bind(order.get())
        .bind(cents)
        .execute(conn)
        .await
        .unwrap();
}

pub async fn fee(conn: &mut PgConnection, order: OrderId, cents: i64) {
    sqlx::query("UPDATE payment SET fee_cents = $2 WHERE order_id = $1")
        .bind(order.get())
        .bind(cents)
        .execute(conn)
        .await
        .unwrap();
}

pub async fn customer(conn: &mut PgConnection, email: &str) -> CustomerId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO customer (email, name, created_at) VALUES ($1, 'Zaak', now()) RETURNING id",
    )
    .bind(email)
    .fetch_one(conn)
    .await
    .unwrap();
    CustomerId::new(id)
}

/// An on-account order, confirmed straight away, written directly so a test sets any
/// price and creation time.
pub async fn on_account_order(
    conn: &mut PgConnection,
    customer: CustomerId,
    shop: &Shop,
    lines: &[(ProductId, u32, i64, i16)],
    created_at: &str,
) -> OrderId {
    let seq: i32 = sqlx::query_scalar(
        "INSERT INTO order_number (year, last) VALUES (2026, 1)
         ON CONFLICT (year) DO UPDATE SET last = order_number.last + 1 RETURNING last",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let total: i64 = lines.iter().map(|l| i64::from(l.1) * l.2).sum();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO \"order\" (number_year, number_seq, customer_id, customer_name, bake_day_id,
            pickup_slot_id, channel, payment_mode, status, total_cents, created_at)
         VALUES (2026, $1, $2, 'Zaak', $3, $4, 'manual', 'on_account', 'confirmed', $5, $6::timestamptz)
         RETURNING id",
    )
    .bind(seq)
    .bind(customer.get())
    .bind(shop.day.get())
    .bind(shop.slot.get())
    .bind(total)
    .bind(created_at)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    for &(product, qty, price, vat) in lines {
        sqlx::query(
            "INSERT INTO order_line (order_id, product_id, qty, unit_price_cents, vat_rate)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(product.get())
        .bind(i32::try_from(qty).unwrap())
        .bind(price)
        .bind(vat)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    OrderId::new(id)
}
