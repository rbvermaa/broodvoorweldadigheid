use bakedays::calendar::{PublicCharity, PublicDrop, PublicPickup, public_drops};
use bakedays::{
    BakeDay, BakeDayProduct, NewBakeDay, NoOrders, PublicStatus, Schedule, days, range,
};
use domain::ProductId;
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{FixedOrders, charity, create_day, date, product, saturday, ts};

async fn with_schedule(conn: &mut PgConnection, day: &BakeDay, schedule: Schedule) -> BakeDay {
    days::update_schedule(conn, day.id, &schedule)
        .await
        .unwrap()
}

fn schedule_of(day: &BakeDay) -> Schedule {
    Schedule {
        online_ordering: day.online_ordering,
        menu_publish_at: day.menu_publish_at,
        opens_at: day.opens_at,
        cutoff_at: day.cutoff_at,
    }
}

async fn on_range(conn: &mut PgConnection, day: &BakeDay, product: ProductId, max: Option<u32>) {
    let input = BakeDayProduct {
        product_id: product,
        max_qty: max,
        capacity_pool_id: None,
        units_per_item: 1,
    };
    range::set_product(conn, day.id, &input).await.unwrap();
}

async fn drops_at(conn: &mut PgConnection, now: &str, orders: &FixedOrders) -> Vec<PublicDrop> {
    let clock = FakeClock::new(ts(now));
    public_drops(conn, &clock, date("2026-10-01"), date("2026-12-31"), orders)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn planned_drop_shows_date_pickup_and_normal_sale_but_no_menu(
    mut conn: PoolConnection<Postgres>,
) {
    let day = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    on_range(&mut conn, &day, bread, None).await;
    let slot = days::list_slots(&mut conn, day.id).await.unwrap()[0];

    let drops = drops_at(&mut conn, "2026-10-20T12:00:00Z", &FixedOrders::default()).await;

    assert_eq!(
        drops,
        [PublicDrop {
            bake_day_id: day.id,
            date: date("2026-10-31"),
            status: PublicStatus::Planned,
            cutoff_at: Some(ts("2026-10-29T19:00:00Z")),
            pickups: vec![PublicPickup {
                slot_id: slot.id,
                location_name: "De Proef".to_owned(),
                address: "Majoor van Swietenlaan 15, 8382 CE Frederiksoord".to_owned(),
                starts_at: ts("2026-10-31T10:00:00Z"),
                ends_at: ts("2026-10-31T11:00:00Z"),
            }],
            charity: None,
            menu: None,
        }]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn menu_appears_at_its_publish_time(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    on_range(&mut conn, &day, bread, None).await;
    let day = with_schedule(
        &mut conn,
        &day,
        Schedule {
            menu_publish_at: Some(ts("2026-10-28T07:00:00Z")),
            ..schedule_of(&day)
        },
    )
    .await;
    let orders = FixedOrders::default();

    let before = drops_at(&mut conn, "2026-10-28T06:59:59Z", &orders).await;
    assert_eq!(
        (before[0].status, before[0].menu.clone()),
        (PublicStatus::Planned, None)
    );

    let after = drops_at(&mut conn, "2026-10-28T07:00:00Z", &orders).await;
    assert_eq!(after[0].status, PublicStatus::MenuAnnounced);
    let menu = after[0].menu.clone().unwrap();
    assert_eq!(
        menu.iter()
            .map(|m| (m.product_id, m.name.as_str()))
            .collect::<Vec<_>>(),
        [(bread, "Desem")]
    );
    assert_eq!(day.id, after[0].bake_day_id);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn charity_is_shown_with_its_link(mut conn: PoolConnection<Postgres>) {
    let food_bank = charity(&mut conn, "Voedselbank", false).await;
    create_day(
        &mut conn,
        &NewBakeDay {
            charity_id: Some(food_bank),
            ..saturday()
        },
    )
    .await;

    let drops = drops_at(&mut conn, "2026-10-20T12:00:00Z", &FixedOrders::default()).await;

    assert_eq!(
        drops[0].charity,
        Some(PublicCharity {
            name: "Voedselbank".to_owned(),
            description: String::new(),
            url: "https://example.org".to_owned(),
        })
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn open_drop_with_nothing_left_is_sold_out(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    on_range(&mut conn, &day, bread, Some(2)).await;
    let clock = FakeClock::new(ts("2026-10-27T12:00:00Z"));
    days::open(&mut conn, &clock, day.id).await.unwrap();

    let some_left = drops_at(
        &mut conn,
        "2026-10-27T12:00:00Z",
        &FixedOrders::pieces([(bread, 1)]),
    )
    .await;
    assert_eq!(some_left[0].status, PublicStatus::Open);
    assert!(some_left[0].menu.is_some(), "an open drop shows its menu");

    let none_left = drops_at(
        &mut conn,
        "2026-10-27T12:00:00Z",
        &FixedOrders::pieces([(bread, 2)]),
    )
    .await;
    assert_eq!(none_left[0].status, PublicStatus::SoldOut);

    let after_deadline = drops_at(
        &mut conn,
        "2026-10-29T19:00:00Z",
        &FixedOrders::pieces([(bread, 2)]),
    )
    .await;
    assert_eq!(after_deadline[0].status, PublicStatus::Closed);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn cancelled_drop_shows_no_menu(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    on_range(&mut conn, &day, bread, None).await;
    let clock = FakeClock::new(ts("2026-10-27T12:00:00Z"));
    days::open(&mut conn, &clock, day.id).await.unwrap();
    days::cancel(&mut conn, &clock, day.id, "De oven is stuk.")
        .await
        .unwrap();

    let drops = drops_at(&mut conn, "2026-10-27T12:00:00Z", &FixedOrders::default()).await;

    assert_eq!(
        (drops[0].status, drops[0].menu.clone()),
        (PublicStatus::Cancelled, None)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn production_only_days_and_days_outside_the_range_are_left_out(
    mut conn: PoolConnection<Postgres>,
) {
    create_day(
        &mut conn,
        &NewBakeDay {
            online_ordering: false,
            ..saturday()
        },
    )
    .await;
    let online = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2027-01-02"),
            ..saturday()
        },
    )
    .await;
    let clock = FakeClock::new(ts("2026-10-20T12:00:00Z"));
    assert!(
        drops_at(&mut conn, "2026-10-20T12:00:00Z", &FixedOrders::default())
            .await
            .is_empty()
    );
    let next_year = public_drops(
        &mut conn,
        &clock,
        date("2027-01-01"),
        date("2027-01-31"),
        &NoOrders,
    )
    .await
    .unwrap();
    assert_eq!(
        next_year.iter().map(|d| d.bake_day_id).collect::<Vec<_>>(),
        [online.id]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn products_not_sold_online_stay_off_the_menu_and_out_of_sold_out(
    mut conn: PoolConnection<Postgres>,
) {
    let day = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    let granola = product(&mut conn, "Granola", None).await;
    let archived = product(&mut conn, "Oud brood", None).await;
    on_range(&mut conn, &day, bread, Some(2)).await;
    on_range(&mut conn, &day, granola, None).await;
    on_range(&mut conn, &day, archived, None).await;
    sqlx::query("UPDATE product SET sold_online = false WHERE id = $1")
        .bind(granola.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE product SET active = false WHERE id = $1")
        .bind(archived.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let clock = FakeClock::new(ts("2026-10-27T12:00:00Z"));
    days::open(&mut conn, &clock, day.id).await.unwrap();

    let drops = drops_at(
        &mut conn,
        "2026-10-27T12:00:00Z",
        &FixedOrders::pieces([(bread, 2)]),
    )
    .await;

    assert_eq!(drops[0].status, PublicStatus::SoldOut);
    let menu = drops[0].menu.clone().unwrap();
    assert_eq!(
        menu.iter().map(|m| m.product_id).collect::<Vec<_>>(),
        [bread]
    );
}
