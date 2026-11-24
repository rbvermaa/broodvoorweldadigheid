use std::collections::BTreeMap;

use bakedays::capacity::Remaining;
use bakedays::{
    Availability, BakeDayProduct, Error, ExtraInput, ProductionExtraId, Purpose, days, extras,
    range,
};
use domain::{BakeDayId, CapacityPoolId, ProductId};
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};
use testkit::FakeClock;

use crate::common::{create_day, product, saturday, ts};

fn on_day(
    product_id: ProductId,
    max_qty: Option<u32>,
    pool: Option<CapacityPoolId>,
    units: u32,
) -> BakeDayProduct {
    BakeDayProduct {
        product_id,
        max_qty,
        capacity_pool_id: pool,
        units_per_item: units,
    }
}

fn extra(qty: u32, purpose: Purpose) -> ExtraInput {
    ExtraInput {
        qty,
        purpose,
        note: None,
    }
}

fn none() -> BTreeMap<ProductId, u32> {
    BTreeMap::new()
}

async fn remaining(
    conn: &mut sqlx::PgConnection,
    day: BakeDayId,
    ordered: &BTreeMap<ProductId, u32>,
) -> Vec<Remaining> {
    range::availability(conn, day, ordered)
        .await
        .unwrap()
        .into_iter()
        .map(|Availability { remaining, .. }| remaining)
        .collect()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn pools_are_named_per_day_and_resizable(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    let other = create_day(&mut conn, &saturday()).await;
    let oven = range::create_pool(&mut conn, day.id, " Oven ", 30)
        .await
        .unwrap();
    assert_eq!(
        (oven.name.as_str(), oven.max_units, oven.bake_day_id),
        ("Oven", 30, day.id)
    );
    range::create_pool(&mut conn, other.id, "Oven", 10)
        .await
        .unwrap();
    assert!(matches!(
        range::create_pool(&mut conn, day.id, "Oven", 5).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        range::create_pool(&mut conn, day.id, " ", 5).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        range::create_pool(&mut conn, day.id, "Groot", u32::MAX).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        range::create_pool(&mut conn, BakeDayId::new(999), "Oven", 5).await,
        Err(Error::NotFound)
    ));
    range::resize_pool(&mut conn, oven.id, 24).await.unwrap();
    assert!(matches!(
        range::resize_pool(&mut conn, CapacityPoolId::new(999), 24).await,
        Err(Error::NotFound)
    ));
    let pools = range::list_pools(&mut conn, day.id).await.unwrap();
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0].max_units, 24);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn range_products_are_set_replaced_and_removed(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    let other = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    let foreign_pool = range::create_pool(&mut conn, other.id, "Oven", 10)
        .await
        .unwrap();
    range::set_product(&mut conn, day.id, &on_day(bread, Some(10), None, 1))
        .await
        .unwrap();
    range::set_product(&mut conn, day.id, &on_day(bread, None, None, 3))
        .await
        .unwrap();
    assert_eq!(
        range::list_products(&mut conn, day.id).await.unwrap(),
        [on_day(bread, None, None, 3)]
    );
    let refused = [
        on_day(ProductId::new(999), None, None, 1),
        on_day(bread, None, Some(foreign_pool.id), 1),
        on_day(bread, None, None, 0),
        on_day(bread, Some(u32::MAX), None, 1),
    ];
    for input in refused {
        assert!(
            matches!(
                range::set_product(&mut conn, day.id, &input).await,
                Err(Error::Invalid(_))
            ),
            "{input:?}"
        );
    }
    range::remove_product(&mut conn, day.id, bread)
        .await
        .unwrap();
    assert!(matches!(
        range::remove_product(&mut conn, day.id, bread).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn extras_take_capacity_like_orders(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    let rolls = product(&mut conn, "Bolletjes", None).await;
    let oven = range::create_pool(&mut conn, day.id, "Oven", 30)
        .await
        .unwrap();
    range::set_product(
        &mut conn,
        day.id,
        &on_day(bread, Some(12), Some(oven.id), 2),
    )
    .await
    .unwrap();
    range::set_product(&mut conn, day.id, &on_day(rolls, None, Some(oven.id), 1))
        .await
        .unwrap();
    assert_eq!(
        remaining(&mut conn, day.id, &none()).await,
        [Remaining::Pieces(12), Remaining::Pieces(30)]
    );

    let freezer = extras::add(
        &mut conn,
        day.id,
        bread,
        &extra(4, Purpose::Freezer),
        &none(),
    )
    .await
    .unwrap();
    assert_eq!(
        (
            freezer.bake_day_id,
            freezer.product_id,
            freezer.qty,
            freezer.purpose
        ),
        (day.id, bread, 4, Purpose::Freezer)
    );
    let ordered = BTreeMap::from([(bread, 5), (rolls, 3)]);
    // Bread: min(12 - 9, (30 - 21) / 2) = 3. Rolls: 30 - 21 = 9.
    assert_eq!(
        remaining(&mut conn, day.id, &ordered).await,
        [Remaining::Pieces(3), Remaining::Pieces(9)]
    );
    assert!(matches!(
        extras::add(&mut conn, day.id, bread, &extra(4, Purpose::SumUp), &ordered).await,
        Err(Error::CapacityExceeded { product_id, remaining: 3 }) if product_id == bread
    ));
    extras::add(
        &mut conn,
        day.id,
        rolls,
        &extra(9, Purpose::Samples),
        &ordered,
    )
    .await
    .unwrap();
    assert_eq!(
        remaining(&mut conn, day.id, &ordered).await,
        [Remaining::Pieces(0), Remaining::Pieces(0)]
    );
    assert_eq!(
        range::extras_by_product(&mut conn, day.id, None)
            .await
            .unwrap(),
        BTreeMap::from([(bread, 4), (rolls, 9)])
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn raising_an_extra_must_fit_but_lowering_always_works(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    range::set_product(&mut conn, day.id, &on_day(bread, Some(10), None, 1))
        .await
        .unwrap();
    let freezer = extras::add(
        &mut conn,
        day.id,
        bread,
        &extra(6, Purpose::Freezer),
        &none(),
    )
    .await
    .unwrap();
    let raised = extras::update(
        &mut conn,
        freezer.id,
        &ExtraInput {
            qty: 10,
            purpose: Purpose::Other,
            note: Some(" Voor de markt ".to_owned()),
        },
        &none(),
    )
    .await
    .unwrap();
    assert_eq!(
        (raised.qty, raised.purpose, raised.note.as_deref()),
        (10, Purpose::Other, Some("Voor de markt"))
    );
    assert!(matches!(
        extras::update(&mut conn, freezer.id, &extra(11, Purpose::Other), &none()).await,
        Err(Error::CapacityExceeded { remaining: 10, .. })
    ));

    range::set_product(&mut conn, day.id, &on_day(bread, Some(2), None, 1))
        .await
        .unwrap();
    let lowered = extras::update(
        &mut conn,
        freezer.id,
        &ExtraInput {
            qty: 8,
            purpose: Purpose::Other,
            note: Some("  ".to_owned()),
        },
        &none(),
    )
    .await
    .unwrap();
    assert_eq!((lowered.qty, lowered.note), (8, None));
    assert!(matches!(
        extras::update(
            &mut conn,
            ProductionExtraId::new(999),
            &extra(1, Purpose::Other),
            &none()
        )
        .await,
        Err(Error::NotFound)
    ));
    extras::remove(&mut conn, freezer.id).await.unwrap();
    assert!(extras::list(&mut conn, day.id).await.unwrap().is_empty());
    assert!(matches!(
        extras::remove(&mut conn, freezer.id).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn extras_of_products_off_the_range_are_unlimited(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    let granola = product(&mut conn, "Granola", None).await;
    let added = extras::add(
        &mut conn,
        day.id,
        granola,
        &extra(500, Purpose::SumUp),
        &none(),
    )
    .await
    .unwrap();
    assert_eq!(extras::list(&mut conn, day.id).await.unwrap(), [added]);
    for input in [extra(0, Purpose::SumUp), extra(u32::MAX, Purpose::SumUp)] {
        assert!(matches!(
            extras::add(&mut conn, day.id, granola, &input, &none()).await,
            Err(Error::Invalid("quantity"))
        ));
    }
    assert!(matches!(
        extras::add(
            &mut conn,
            day.id,
            ProductId::new(999),
            &extra(1, Purpose::SumUp),
            &none()
        )
        .await,
        Err(Error::Invalid("product"))
    ));
    assert!(matches!(
        extras::add(
            &mut conn,
            BakeDayId::new(999),
            granola,
            &extra(1, Purpose::SumUp),
            &none()
        )
        .await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn nothing_changes_once_the_day_is_done_or_cancelled(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let day = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    let oven = range::create_pool(&mut conn, day.id, "Oven", 30)
        .await
        .unwrap();
    range::set_product(&mut conn, day.id, &on_day(bread, None, None, 1))
        .await
        .unwrap();
    let freezer = extras::add(
        &mut conn,
        day.id,
        bread,
        &extra(2, Purpose::Freezer),
        &none(),
    )
    .await
    .unwrap();
    days::cancel(&mut conn, &clock, day.id, "Ziek")
        .await
        .unwrap();

    assert!(matches!(
        extras::add(
            &mut conn,
            day.id,
            bread,
            &extra(1, Purpose::Freezer),
            &none()
        )
        .await,
        Err(Error::Final)
    ));
    assert!(matches!(
        extras::update(&mut conn, freezer.id, &extra(1, Purpose::Freezer), &none()).await,
        Err(Error::Final)
    ));
    assert!(matches!(
        extras::remove(&mut conn, freezer.id).await,
        Err(Error::Final)
    ));
    assert!(matches!(
        range::create_pool(&mut conn, day.id, "Tweede", 5).await,
        Err(Error::Final)
    ));
    assert!(matches!(
        range::resize_pool(&mut conn, oven.id, 5).await,
        Err(Error::Final)
    ));
    assert!(matches!(
        range::set_product(&mut conn, day.id, &on_day(bread, None, None, 1)).await,
        Err(Error::Final)
    ));
    assert!(matches!(
        range::remove_product(&mut conn, day.id, bread).await,
        Err(Error::Final)
    ));
    assert!(matches!(
        days::set_charity(&mut conn, day.id, None, 0).await,
        Err(Error::Final)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn two_writers_never_both_take_the_last_piece(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let day = create_day(&mut conn, &saturday()).await;
    let bread = product(&mut conn, "Desem", None).await;
    let rolls = product(&mut conn, "Bolletjes", None).await;
    let oven = range::create_pool(&mut conn, day.id, "Oven", 1)
        .await
        .unwrap();
    for product_id in [bread, rolls] {
        range::set_product(
            &mut conn,
            day.id,
            &on_day(product_id, None, Some(oven.id), 1),
        )
        .await
        .unwrap();
    }
    drop(conn);

    let take = |product_id| {
        let pool = pool.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            extras::add(
                &mut conn,
                day.id,
                product_id,
                &extra(1, Purpose::Samples),
                &none(),
            )
            .await
        }
    };
    for _ in 0..5 {
        let (a, b) = tokio::join!(take(bread), take(rolls));
        assert!(
            a.is_err() || b.is_err(),
            "both writers took the last piece: {a:?} {b:?}"
        );
        let mut conn = pool.acquire().await.unwrap();
        let used: u32 = range::extras_by_product(&mut conn, day.id, None)
            .await
            .unwrap()
            .values()
            .sum();
        assert_eq!(used, 1);
        for extra in extras::list(&mut conn, day.id).await.unwrap() {
            extras::remove(&mut conn, extra.id).await.unwrap();
        }
    }
}

#[test]
fn purposes_round_trip_through_text() {
    for purpose in Purpose::ALL {
        assert_eq!(purpose.as_str().parse(), Ok(purpose));
        assert_eq!(purpose.to_string(), purpose.as_str());
    }
    assert!("gift".parse::<Purpose>().is_err());
}
