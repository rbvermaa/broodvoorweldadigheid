//! Pickup reminders and cancellation emails queued by jobs (F9, ADR 0022).
#![forbid(unsafe_code)]

use std::sync::Arc;

use bakedays::{NewBakeDay, Schedule, days};
use domain::{Money, OrderId, PickupLocationId, ProductId};
use i18n::Translator;
use jiff::Timestamp;
use jiff::civil::time;
use jobs::OrderEmails;
use jobs::order_emails::{enqueue_cancellation, enqueue_due_reminders, reminder_due, reminder_key};
use orders::{Cart, CustomerInput, OrderedBy, PaidOutcome, PaymentReceived};
use sqlx::{PgConnection, PgPool};
use testkit::FakeClock;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn emails() -> OrderEmails {
    OrderEmails::new(
        Arc::new(Translator::dutch().unwrap()),
        "https://shop.example.org",
    )
}

/// Saturday after the October change: pickup 11:00–12:00 CET (10:00Z).
fn saturday() -> NewBakeDay {
    NewBakeDay {
        date: "2026-10-31".parse().unwrap(),
        location_id: PickupLocationId::new(1),
        online_ordering: true,
        charity_id: None,
    }
}

async fn company_details(conn: &mut PgConnection) {
    sqlx::query("UPDATE company SET address = 'Teststraat 1, 1234 AB Testdorp', kvk = '12345678'")
        .execute(conn)
        .await
        .unwrap();
}

async fn bread(conn: &mut PgConnection) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role) VALUES ('test-baker', 'Testbakker', 'baker')
            ON CONFLICT (oidc_sub) DO UPDATE SET name = EXCLUDED.name RETURNING id),
         kitchen AS (UPDATE company SET may_contain_approved_by = (SELECT id FROM baker),
            may_contain_approved_at = '2026-08-01T10:00:00Z')
         INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online, allergens_approved_by, allergens_approved_at)
         VALUES ('Desem', 450, 9, 800, 900, true, (SELECT id FROM baker), '2026-10-01T10:00:00Z') RETURNING id",
    )
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

struct Shop {
    clock: FakeClock,
    cart: Cart,
    slot: domain::PickupSlotId,
    day: domain::BakeDayId,
}

/// An open Saturday with one bread on the range, on the Wednesday before.
async fn shop(conn: &mut PgConnection) -> Shop {
    company_details(conn).await;
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let day = days::create(conn, &saturday()).await.unwrap();
    days::update_schedule(
        conn,
        day.id,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(ts("2026-10-01T00:00:00Z")),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    days::open(conn, &clock, day.id).await.unwrap();
    let slot = days::list_slots(conn, day.id).await.unwrap()[0].id;
    let product = bread(conn).await;
    bakedays::range::set_product(
        conn,
        day.id,
        &bakedays::BakeDayProduct {
            product_id: product,
            max_qty: None,
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await
    .unwrap();
    let mut cart = Cart::new(day.id);
    cart.set(product, 2, None).unwrap();
    Shop {
        clock,
        cart,
        slot,
        day: day.id,
    }
}

async fn order(conn: &mut PgConnection, shop: &Shop, email: &str, pay: bool) -> OrderId {
    let order = orders::checkout(
        conn,
        &shop.clock,
        &shop.cart,
        &CustomerInput {
            email: email.to_owned(),
            name: "Test Klant".to_owned(),
            phone: None,
            marketing_opt_in: false,
        },
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    if pay {
        let outcome = orders::mark_paid(
            conn,
            &shop.clock,
            order.id,
            &PaymentReceived {
                reference: format!("tr_{}", order.id.get()),
                amount: order.total,
            },
        )
        .await
        .unwrap();
        assert!(matches!(outcome, PaidOutcome::Paid { .. }));
    }
    order.id
}

#[derive(Debug, sqlx::FromRow)]
struct Queued {
    kind: String,
    to_address: String,
    text_body: String,
    dedupe_key: Option<String>,
}

async fn outbox(pool: &PgPool) -> Vec<Queued> {
    sqlx::query_as("SELECT kind, to_address, text_body, dedupe_key FROM email_outbox ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn remind_at(conn: &mut PgConnection, clock: &FakeClock, now: &str) -> Vec<OrderId> {
    clock.set(ts(now));
    enqueue_due_reminders(conn, clock, &emails()).await.unwrap()
}

#[test]
fn the_reminder_is_due_the_evening_before_in_local_time() {
    let six = time(18, 0, 0, 0);
    assert_eq!(
        reminder_due(ts("2026-10-31T10:00:00Z"), six),
        ts("2026-10-30T17:00:00Z"),
        "Friday 18:00 CET after the October change"
    );
    assert_eq!(
        reminder_due(ts("2026-10-24T09:00:00Z"), six),
        ts("2026-10-23T16:00:00Z"),
        "Friday 18:00 CEST before it"
    );
    assert_eq!(
        reminder_due(ts("2026-10-26T10:00:00Z"), six),
        ts("2026-10-25T17:00:00Z"),
        "on the change day itself"
    );
    assert_eq!(
        reminder_due(ts("2026-03-30T09:00:00Z"), six),
        ts("2026-03-29T16:00:00Z"),
        "on the March change day"
    );
    assert_eq!(
        reminder_due(ts("2026-10-30T23:30:00Z"), time(7, 30, 0, 0)),
        ts("2026-10-30T06:30:00Z"),
        "the local date counts: 00:30 Saturday is still Saturday"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn one_reminder_per_paid_order_at_six_the_evening_before_after_the_clock_change(
    pool: PgPool,
) {
    let mut conn = pool.acquire().await.unwrap();
    let shop = shop(&mut conn).await;
    let paid = order(&mut conn, &shop, "betaald@example.org", true).await;
    order(&mut conn, &shop, "onbetaald@example.org", false).await;

    assert!(
        remind_at(&mut conn, &shop.clock, "2026-10-30T16:30:00Z")
            .await
            .is_empty(),
        "17:30 CET is too early, though it would be 18:30 in summer time"
    );
    assert!(
        remind_at(&mut conn, &shop.clock, "2026-10-30T16:59:59Z")
            .await
            .is_empty()
    );
    assert_eq!(
        remind_at(&mut conn, &shop.clock, "2026-10-30T17:00:00Z").await,
        [paid]
    );
    assert!(
        remind_at(&mut conn, &shop.clock, "2026-10-30T17:01:00Z")
            .await
            .is_empty(),
        "a second tick queues nothing"
    );

    let queued = outbox(&pool).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert_eq!(queued[0].kind, "pickup_reminder");
    assert_eq!(queued[0].to_address, "betaald@example.org");
    assert_eq!(
        queued[0].dedupe_key.as_deref(),
        Some(reminder_key(paid).as_str())
    );
    assert!(
        queued[0].text_body.contains("2 × Desem"),
        "{}",
        queued[0].text_body
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn no_reminder_for_cancelled_orders_or_after_pickup_started(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let shop = shop(&mut conn).await;
    let cancelled = order(&mut conn, &shop, "geannuleerd@example.org", true).await;
    sqlx::query(
        r#"UPDATE "order" SET status = 'cancelled', cancel_reason = 'staff' WHERE id = $1"#,
    )
    .bind(cancelled.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    let late = order(&mut conn, &shop, "laat@example.org", true).await;
    assert!(
        remind_at(&mut conn, &shop.clock, "2026-10-31T10:00:00Z")
            .await
            .is_empty(),
        "pickup has started"
    );
    assert_eq!(
        remind_at(&mut conn, &shop.clock, "2026-10-31T09:59:00Z").await,
        [late],
        "the cancelled order is skipped"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_staff_cancellation_is_queued_once_and_names_the_refund(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let shop = shop(&mut conn).await;
    let paid = order(&mut conn, &shop, "klant@example.org", true).await;
    let refund = Some(Money::from_cents(900));
    let first = enqueue_cancellation(&mut conn, &shop.clock, &emails(), paid, refund)
        .await
        .unwrap();
    let again = enqueue_cancellation(&mut conn, &shop.clock, &emails(), paid, refund)
        .await
        .unwrap();
    assert!(first.is_some());
    assert_eq!(again, None);
    let queued = outbox(&pool).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, "order_cancelled");
    assert!(
        queued[0]
            .text_body
            .contains("We storten €\u{a0}9,00 terug."),
        "{}",
        queued[0].text_body
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_kvk_details_nothing_is_queued(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let shop = shop(&mut conn).await;
    let paid = order(&mut conn, &shop, "klant@example.org", true).await;
    sqlx::query("UPDATE company SET kvk = NULL")
        .execute(&mut *conn)
        .await
        .unwrap();
    let error = enqueue_cancellation(&mut conn, &shop.clock, &emails(), paid, None)
        .await
        .unwrap_err();
    assert!(matches!(error, jobs::EmailError::CompanyIncomplete));
    assert!(
        enqueue_due_reminders(&mut conn, &shop.clock, &emails())
            .await
            .is_ok()
    );
    shop.clock.set(ts("2026-10-30T17:00:00Z"));
    assert!(matches!(
        enqueue_due_reminders(&mut conn, &shop.clock, &emails()).await,
        Err(jobs::EmailError::CompanyIncomplete)
    ));
    assert!(outbox(&pool).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn no_reminder_when_the_drop_is_cancelled_after_payment(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let shop = shop(&mut conn).await;
    order(&mut conn, &shop, "klant@example.org", true).await;
    days::cancel(&mut conn, &shop.clock, shop.day, "De oven is stuk")
        .await
        .unwrap();
    assert!(
        remind_at(&mut conn, &shop.clock, "2026-10-30T17:00:00Z")
            .await
            .is_empty()
    );
    assert!(outbox(&pool).await.is_empty());
}
