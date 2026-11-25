//! The batch split at the order deadline (F14a).
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use bakedays::{
    BakeDay, BakeDayProduct, ExtraInput, NewBakeDay, Purpose, Schedule, days, extras, range,
};
use domain::{BakeDayId, PickupLocationId, ProductId, StaffUserId};
use jiff::Timestamp;
use jobs::batches::tick;
use orders::Orders;
use orders::audit::Actor;
use production::batches;
use recipes::recipes as store;
use recipes::{
    DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput, RecipeKind,
    ingredients,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

const OWNER: Actor = Actor::Staff(StaffUserId::new(1));

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

/// Saturday 5 December 2026, open from 20 November; deadline Thursday 3 December 20:00 CET.
async fn open_day(conn: &mut PgConnection, clock: &FakeClock, date: &str) -> BakeDay {
    let day = days::create(
        conn,
        &NewBakeDay {
            date: date.parse().unwrap(),
            location_id: PickupLocationId::new(1),
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .unwrap();
    days::update_schedule(
        conn,
        day.id,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(ts("2026-11-20T00:00:00Z")),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    days::open(conn, clock, day.id).await.unwrap()
}

/// A bread of 1000 g dough on a 100/70 recipe, with `pieces` extra on the day.
async fn bread(
    conn: &mut PgConnection,
    clock: &FakeClock,
    day: BakeDayId,
    pieces: u32,
) -> ProductId {
    let mut lines = Vec::new();
    for (name, role, pct) in [
        ("Bloem", IngredientRole::Flour, 100_000),
        ("Water", IngredientRole::Water, 70_000),
    ] {
        let id = ingredients::create(
            conn,
            &IngredientInput {
                name: name.to_owned(),
                label_name: String::new(),
                role,
                supplier_name: String::new(),
                allergens: BTreeSet::new(),
            },
        )
        .await
        .unwrap()
        .id;
        lines.push(LineInput {
            pct: Pct::from_thousandths(pct),
            part: LinePart::Ingredient(id),
        });
    }
    let (recipe, version) = store::create(
        conn,
        &RecipeInput {
            name: "Brood".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    store::save_draft(
        conn,
        version.id,
        &DraftInput {
            lines,
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    let product: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece, sold_online)
        VALUES ('Brood', 500, 9, 900, 1000, true) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let product = ProductId::new(product);
    store::link_product(conn, recipe.id, product).await.unwrap();
    range::set_product(
        conn,
        day,
        &BakeDayProduct {
            product_id: product,
            max_qty: None,
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await
    .unwrap();
    extras::add(
        conn,
        day,
        product,
        &ExtraInput {
            qty: pieces,
            purpose: Purpose::SumUp,
            note: None,
        },
        &Orders::new(clock),
    )
    .await
    .unwrap();
    product
}

async fn close_at(conn: &mut PgConnection, clock: &FakeClock, now: &str) {
    clock.set(ts(now));
    jobs::bake_days::tick(conn, clock).await.unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_split_runs_once_when_the_deadline_passes(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-25T10:00:00Z"));
    let day = open_day(&mut conn, &clock, "2026-12-05").await;
    assert_eq!(day.cutoff_at, Some(ts("2026-12-03T19:00:00Z")));
    bread(&mut conn, &clock, day.id, 25).await;
    batches::set_capacity(&mut conn, Some(10_000), OWNER, &clock)
        .await
        .unwrap();

    close_at(&mut conn, &clock, "2026-12-03T18:59:59Z").await;
    let report = tick(&mut conn, &clock).await.unwrap();
    assert!(report.created.is_empty(), "before the deadline");

    close_at(&mut conn, &clock, "2026-12-03T19:00:00Z").await;
    let report = tick(&mut conn, &clock).await.unwrap();
    assert_eq!(report.days, [day.id]);
    assert_eq!(report.created.len(), 3);

    clock.set(ts("2026-12-03T19:01:00Z"));
    let again = tick(&mut conn, &clock).await.unwrap();
    assert_eq!(again, jobs::SplitReport::default());
    let list = batches::list(&mut conn, day.id).await.unwrap();
    assert_eq!(list.len(), 3);
    assert!(list.iter().all(|b| b.dough_g() <= 10_000));
    assert_eq!(
        list.iter()
            .map(production::batches::Batch::dough_g)
            .sum::<u64>(),
        25_000
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_a_capacity_the_split_waits(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-25T10:00:00Z"));
    let day = open_day(&mut conn, &clock, "2026-12-05").await;
    bread(&mut conn, &clock, day.id, 5).await;
    close_at(&mut conn, &clock, "2026-12-03T19:00:00Z").await;
    let report = tick(&mut conn, &clock).await.unwrap();
    assert_eq!(report, jobs::SplitReport::default());
    assert!(batches::list(&mut conn, day.id).await.unwrap().is_empty());

    clock.set(ts("2026-12-04T08:00:00Z"));
    batches::set_capacity(&mut conn, Some(10_000), OWNER, &clock)
        .await
        .unwrap();
    let report = tick(&mut conn, &clock).await.unwrap();
    assert_eq!(
        report.days,
        [day.id],
        "the capacity arrived before the bake date"
    );

    let past = open_day(&mut conn, &clock, "2026-12-12").await;
    bread_again(&mut conn, &clock, past.id).await;
    close_at(&mut conn, &clock, "2026-12-13T09:00:00Z").await;
    let report = tick(&mut conn, &clock).await.unwrap();
    assert!(
        report.days.is_empty(),
        "a day whose date has passed is not split"
    );
}

/// The product of [`bread`] on another day.
async fn bread_again(conn: &mut PgConnection, clock: &FakeClock, day: BakeDayId) {
    let product = ProductId::new(
        sqlx::query_scalar("SELECT id FROM product WHERE name_nl = 'Brood'")
            .fetch_one(&mut *conn)
            .await
            .unwrap(),
    );
    range::set_product(
        conn,
        day,
        &BakeDayProduct {
            product_id: product,
            max_qty: None,
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await
    .unwrap();
    extras::add(
        conn,
        day,
        product,
        &ExtraInput {
            qty: 3,
            purpose: Purpose::SumUp,
            note: None,
        },
        &Orders::new(clock),
    )
    .await
    .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_split_waits_for_orders_still_in_their_payment_hold(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-11-25T10:00:00Z"));
    let day = open_day(&mut conn, &clock, "2026-12-05").await;
    let product = bread(&mut conn, &clock, day.id, 2).await;
    batches::set_capacity(&mut conn, Some(10_000), OWNER, &clock)
        .await
        .unwrap();
    sqlx::query(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role)
            VALUES ('test-baker', 'Testbakker', 'baker') RETURNING id)
        UPDATE product SET allergens_approved_by = (SELECT id FROM baker),
            allergens_approved_at = '2026-11-01T10:00:00Z' WHERE id = $1",
    )
    .bind(product.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    clock.set(ts("2026-12-03T18:58:00Z"));
    let slot = days::list_slots(&mut conn, day.id).await.unwrap()[0].id;
    let mut cart = orders::Cart::new(day.id);
    cart.set(product, 3, None).unwrap();
    let customer = orders::CustomerInput {
        email: "sanne@example.org".to_owned(),
        name: "Sanne".to_owned(),
        phone: None,
        marketing_opt_in: false,
    };
    let order = orders::checkout(
        &mut conn,
        &clock,
        &cart,
        &customer,
        slot,
        orders::OrderedBy::Guest,
    )
    .await
    .unwrap();

    close_at(&mut conn, &clock, "2026-12-03T19:00:00Z").await;
    let report = tick(&mut conn, &clock).await.unwrap();
    assert!(report.days.is_empty(), "an order is still in its hold");

    clock.set(ts("2026-12-03T19:05:00Z"));
    orders::mark_paid(
        &mut conn,
        &clock,
        order.id,
        &orders::PaymentReceived {
            reference: "tr_1".to_owned(),
            amount: order.total,
        },
    )
    .await
    .unwrap();
    let report = tick(&mut conn, &clock).await.unwrap();
    assert_eq!(report.days, [day.id]);
    let list = batches::list(&mut conn, day.id).await.unwrap();
    assert_eq!(
        list[0].outputs[0].planned_pieces, 5,
        "3 paid after the deadline plus 2 extra"
    );
}
