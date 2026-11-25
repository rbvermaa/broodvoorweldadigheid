#![forbid(unsafe_code)]

use std::fmt::Write as _;

use bakedays::{
    BakeDayProduct, ExtraInput, NewBakeDay, PickupSlotInput, Purpose, Schedule, days, extras, range,
};
use domain::{BakeDayId, CapacityPoolId, PickupLocationId, PickupSlotId, ProductId, StaffUserId};
use jiff::Timestamp;
use orders::{Cart, CustomerInput, Order, OrderedBy, Orders, PaymentReceived};
use production::{PickupList, ProductionList, pickup_list, production_list};
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

const DE_PROEF: PickupLocationId = PickupLocationId::new(1);
const BAKER: StaffUserId = StaffUserId::new(3);

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

struct Shop {
    day: BakeDayId,
    first: PickupSlotId,
    second: PickupSlotId,
}

/// Saturday 31 October 2026, open on Wednesday, with a second slot 12:00–13:00.
async fn shop(conn: &mut PgConnection, clock: &FakeClock, date: &str) -> Shop {
    let date: jiff::civil::Date = date.parse().unwrap();
    let day = days::create(
        conn,
        &NewBakeDay {
            date,
            location_id: DE_PROEF,
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
            opens_at: Some(ts("2026-10-01T00:00:00Z")),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    days::open(conn, clock, day.id).await.unwrap();
    let first = days::list_slots(conn, day.id).await.unwrap()[0];
    let second = days::add_slot(
        conn,
        day.id,
        DE_PROEF,
        &PickupSlotInput {
            starts_at: first.ends_at,
            ends_at: first.ends_at + jiff::SignedDuration::from_hours(1),
            max_orders: None,
        },
    )
    .await
    .unwrap();
    Shop {
        day: day.id,
        first: first.id,
        second,
    }
}

async fn product(conn: &mut PgConnection, name: &str, sold_online: bool) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role) VALUES ('test-baker', 'Testbakker', 'baker')
            ON CONFLICT (oidc_sub) DO UPDATE SET name = EXCLUDED.name RETURNING id),
         kitchen AS (UPDATE company SET may_contain_approved_by = (SELECT id FROM baker),
            may_contain_approved_at = '2026-08-01T10:00:00Z')
         INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online, allergens_approved_by, allergens_approved_at)
         VALUES ($1, 450, 9, 800, 900, $2, (SELECT id FROM baker), '2026-10-01T10:00:00Z') RETURNING id",
    )
    .bind(name)
    .bind(sold_online)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

async fn on_range(
    conn: &mut PgConnection,
    day: BakeDayId,
    product_id: ProductId,
    pool: Option<CapacityPoolId>,
    units_per_item: u32,
) {
    range::set_product(
        conn,
        day,
        &BakeDayProduct {
            product_id,
            max_qty: None,
            capacity_pool_id: pool,
            units_per_item,
        },
    )
    .await
    .unwrap();
}

async fn order(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &Shop,
    slot: PickupSlotId,
    name: &str,
    phone: Option<&str>,
    lines: &[(ProductId, u32)],
) -> Order {
    let mut cart = Cart::new(shop.day);
    for &(product, qty) in lines {
        cart.set(product, qty, None).unwrap();
    }
    let customer = CustomerInput {
        email: format!("{}@example.org", name.to_lowercase().replace(' ', ".")),
        name: name.to_owned(),
        phone: phone.map(str::to_owned),
        marketing_opt_in: false,
    };
    orders::checkout(conn, clock, &cart, &customer, slot, OrderedBy::Guest)
        .await
        .unwrap()
}

async fn paid(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &Shop,
    slot: PickupSlotId,
    name: &str,
    phone: Option<&str>,
    lines: &[(ProductId, u32)],
) -> Order {
    let order = order(conn, clock, shop, slot, name, phone, lines).await;
    let payment = PaymentReceived {
        reference: format!("tr_{}", order.id.get()),
        amount: order.total,
    };
    orders::mark_paid(conn, clock, order.id, &payment)
        .await
        .unwrap();
    order
}

async fn extra(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &Shop,
    product: ProductId,
    qty: u32,
    purpose: Purpose,
) {
    extras::add(
        conn,
        shop.day,
        product,
        &ExtraInput {
            qty,
            purpose,
            note: None,
        },
        &Orders::new(clock),
    )
    .await
    .unwrap();
}

fn production_table(list: &ProductionList) -> String {
    let mut out = format!("{}\n", list.day.date);
    for p in &list.products {
        let e = p.extras;
        writeln!(
            out,
            "{}: ordered {} freezer {} sumup {} samples {} other {} -> bake {}",
            p.name,
            p.ordered,
            e.freezer,
            e.sumup,
            e.samples,
            e.other,
            p.to_bake()
        )
        .unwrap();
    }
    out
}

fn pickup_table(list: &PickupList) -> String {
    let mut out = format!("{}\n", list.day.date);
    for slot in &list.slots {
        writeln!(
            out,
            "{} – {} {}",
            slot.slot.starts_at, slot.slot.ends_at, slot.location
        )
        .unwrap();
        for o in &slot.orders {
            let lines: Vec<String> = o
                .lines
                .iter()
                .map(|l| format!("{}x {}", l.qty, l.name))
                .collect();
            writeln!(
                out,
                "  [{}] {} {} {} {} {}: {}",
                if o.picked_up { "x" } else { " " },
                o.number,
                o.customer_name,
                o.customer_phone.as_deref().unwrap_or("-"),
                o.status,
                o.payment_mode,
                lines.join(", ")
            )
            .unwrap();
        }
    }
    out
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_production_list_counts_firm_orders_and_extras(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let other_day = self::shop(&mut conn, &clock, "2026-11-07").await;
    let oven = range::create_pool(&mut conn, shop.day, "Oven", 100)
        .await
        .unwrap();
    range::create_pool(&mut conn, shop.day, "Koelkast", 20)
        .await
        .unwrap();
    let desem = product(&mut conn, "Desem", true).await;
    let koekjes = product(&mut conn, "koekjes", true).await;
    let spelt = product(&mut conn, "Spelt", true).await;
    let granola = product(&mut conn, "Granola", false).await;
    on_range(&mut conn, shop.day, desem, Some(oven.id), 2).await;
    on_range(&mut conn, shop.day, koekjes, Some(oven.id), 1).await;
    on_range(&mut conn, shop.day, spelt, None, 1).await;
    on_range(&mut conn, other_day.day, desem, None, 1).await;

    paid(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Anna Test",
        None,
        &[(desem, 2), (koekjes, 3)],
    )
    .await;
    let picked = paid(
        &mut conn,
        &clock,
        &shop,
        shop.second,
        "Bob Test",
        None,
        &[(desem, 1)],
    )
    .await;
    orders::mark_picked_up(&mut conn, &clock, picked.id, BAKER)
        .await
        .unwrap();
    order(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Held Test",
        None,
        &[(desem, 5), (spelt, 1)],
    )
    .await;
    paid(
        &mut conn,
        &clock,
        &other_day,
        other_day.first,
        "Later Test",
        None,
        &[(desem, 7)],
    )
    .await;
    extra(&mut conn, &clock, &shop, desem, 4, Purpose::Freezer).await;
    extra(&mut conn, &clock, &shop, desem, 2, Purpose::Samples).await;
    extra(&mut conn, &clock, &shop, desem, 1, Purpose::Samples).await;
    extra(&mut conn, &clock, &shop, granola, 12, Purpose::SumUp).await;

    let list = production_list(&mut conn, shop.day).await.unwrap();
    insta::assert_snapshot!(production_table(&list), @r"
    2026-10-31
    Desem: ordered 3 freezer 4 sumup 0 samples 3 other 0 -> bake 10
    Granola: ordered 0 freezer 0 sumup 12 samples 0 other 0 -> bake 12
    koekjes: ordered 3 freezer 0 sumup 0 samples 0 other 0 -> bake 3
    ");

    clock.advance(orders::HOLD);
    orders::release_expired_holds(&mut conn, &clock)
        .await
        .unwrap();
    assert_eq!(production_list(&mut conn, shop.day).await.unwrap(), list);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_empty_day_has_an_empty_production_list(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let list = production_list(&mut conn, shop.day).await.unwrap();
    assert!(list.products.is_empty());
    assert!(matches!(
        production_list(&mut conn, BakeDayId::new(999_999)).await,
        Err(production::Error::BakeDays(bakedays::Error::NotFound))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_pickup_list_groups_by_slot_and_sorts_by_name(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let desem = product(&mut conn, "Desem", true).await;
    let koekjes = product(&mut conn, "Koekjes", true).await;
    on_range(&mut conn, shop.day, desem, None, 1).await;
    on_range(&mut conn, shop.day, koekjes, None, 1).await;

    paid(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Piet Test",
        None,
        &[(desem, 1)],
    )
    .await;
    paid(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "anna Test",
        Some("0600000000"),
        &[(desem, 2), (koekjes, 1)],
    )
    .await;
    let picked = paid(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Piet Test",
        None,
        &[(koekjes, 2)],
    )
    .await;
    orders::mark_picked_up(&mut conn, &clock, picked.id, BAKER)
        .await
        .unwrap();
    order(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Held Test",
        None,
        &[(desem, 1)],
    )
    .await;
    paid(
        &mut conn,
        &clock,
        &shop,
        shop.second,
        "Zoë Test",
        None,
        &[(desem, 1)],
    )
    .await;
    let late = order(
        &mut conn,
        &clock,
        &shop,
        shop.second,
        "Late Test",
        None,
        &[(desem, 1)],
    )
    .await;
    clock.advance(orders::HOLD);
    orders::release_expired_holds(&mut conn, &clock)
        .await
        .unwrap();
    assert_eq!(
        orders::order::get(&mut conn, late.id).await.unwrap().status,
        orders::OrderStatus::Cancelled
    );

    let list = pickup_list(&mut conn, shop.day).await.unwrap();
    insta::assert_snapshot!(pickup_table(&list), @r"
    2026-10-31
    2026-10-31T10:00:00Z – 2026-10-31T11:00:00Z De Proef
      [ ] 2026-0002 anna Test 0600000000 paid online: 2x Desem, 1x Koekjes
      [ ] 2026-0001 Piet Test - paid online: 1x Desem
      [x] 2026-0003 Piet Test - picked_up online: 2x Koekjes
    2026-10-31T11:00:00Z – 2026-10-31T12:00:00Z De Proef
      [ ] 2026-0005 Zoë Test - paid online: 1x Desem
    ");

    let undone = orders::undo_picked_up(&mut conn, &clock, picked.id, BAKER)
        .await
        .unwrap();
    let list = pickup_list(&mut conn, shop.day).await.unwrap();
    let ticked = &list.slots[0].orders[2];
    assert_eq!(ticked.order_id, undone.id);
    assert!(!ticked.picked_up);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_day_without_orders_still_lists_its_slots(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let list = pickup_list(&mut conn, shop.day).await.unwrap();
    assert_eq!(list.slots.len(), 2);
    assert!(list.slots.iter().all(|s| s.orders.is_empty()));
    assert!(matches!(
        pickup_list(&mut conn, BakeDayId::new(999_999)).await,
        Err(production::Error::BakeDays(bakedays::Error::NotFound))
    ));
}
