use domain::PaymentStatus;
use jiff::SignedDuration;
use orders::{
    Error, OrderStatus, PaidOutcome, PaymentReceived, latest_payment_status, mark_paid,
    payment_started, record_payment_status, slot_orders,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};

use crate::common::{BREAD, on_range, open_day, order_as_guest, product, saturday, wednesday};

async fn payment_rows(conn: &mut PgConnection) -> Vec<(String, String)> {
    sqlx::query_as("SELECT provider_payment_id, status FROM payment ORDER BY id")
        .fetch_all(conn)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_started_payment_becomes_paid_in_place(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();

    payment_started(&mut conn, &clock, order.id, "tr_1", order.total)
        .await
        .unwrap();
    payment_started(&mut conn, &clock, order.id, "tr_1", order.total)
        .await
        .unwrap();
    assert_eq!(
        latest_payment_status(&mut conn, order.id).await.unwrap(),
        Some(("tr_1".to_owned(), PaymentStatus::Open))
    );

    let pending = record_payment_status(
        &mut conn,
        &clock,
        order.id,
        "tr_1",
        order.total,
        PaymentStatus::Pending,
    )
    .await
    .unwrap();
    assert!(pending);

    let outcome = mark_paid(
        &mut conn,
        &clock,
        order.id,
        &PaymentReceived {
            reference: "tr_1".to_owned(),
            amount: order.total,
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, PaidOutcome::Paid { .. }), "{outcome:?}");
    assert_eq!(
        payment_rows(&mut conn).await,
        [("tr_1".to_owned(), "paid".to_owned())]
    );

    let late = record_payment_status(
        &mut conn,
        &clock,
        order.id,
        "tr_1",
        order.total,
        PaymentStatus::Expired,
    )
    .await
    .unwrap();
    assert!(!late, "a paid payment keeps its status");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_failed_payment_is_recorded_once_and_leaves_the_order_pending(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let other = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();

    for expected in [true, false] {
        let changed = record_payment_status(
            &mut conn,
            &clock,
            order.id,
            "tr_9",
            order.total,
            PaymentStatus::Failed,
        )
        .await
        .unwrap();
        assert_eq!(changed, expected);
    }
    assert_eq!(
        orders::order::get(&mut conn, order.id)
            .await
            .unwrap()
            .status,
        OrderStatus::PendingPayment
    );
    assert_eq!(
        latest_payment_status(&mut conn, order.id).await.unwrap(),
        Some(("tr_9".to_owned(), PaymentStatus::Failed))
    );
    assert!(matches!(
        record_payment_status(
            &mut conn,
            &clock,
            other.id,
            "tr_9",
            other.total,
            PaymentStatus::Canceled
        )
        .await,
        Err(Error::PaymentOfOtherOrder)
    ));
    assert!(matches!(
        payment_started(&mut conn, &clock, other.id, "tr_9", other.total).await,
        Err(Error::PaymentOfOtherOrder)
    ));
    assert!(matches!(
        record_payment_status(
            &mut conn,
            &clock,
            order.id,
            "tr_10",
            order.total,
            PaymentStatus::Paid
        )
        .await,
        Err(Error::Invalid(_))
    ));
    assert!(
        orders::refunds_due(&mut conn).await.unwrap().is_empty(),
        "an unpaid payment owes nothing"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn status_tokens_find_the_order_and_nothing_else(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let first = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let second = order_as_guest(&mut conn, &clock, &shop, &[(bread, 2, None)])
        .await
        .unwrap();

    let token = orders::order::status_token(&mut conn, first.id)
        .await
        .unwrap();
    let other = orders::order::status_token(&mut conn, second.id)
        .await
        .unwrap();
    assert_ne!(token, other);
    assert_eq!(
        orders::order::by_status_token(&mut conn, &token)
            .await
            .unwrap()
            .id,
        first.id
    );
    for bad in [
        "2026-0001".to_owned(),
        token.to_uppercase(),
        "00000000-0000-4000-8000-000000000000".to_owned(),
    ] {
        assert!(
            matches!(
                orders::order::by_status_token(&mut conn, &bad).await,
                Err(Error::NotFound)
            ),
            "{bad}"
        );
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn slot_orders_count_live_holds_and_paid_orders(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    assert_eq!(
        slot_orders(&mut conn, shop.slot, domain::Clock::now(&clock))
            .await
            .unwrap(),
        1
    );
    mark_paid(
        &mut conn,
        &clock,
        order.id,
        &PaymentReceived {
            reference: "tr_1".to_owned(),
            amount: order.total,
        },
    )
    .await
    .unwrap();
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    assert_eq!(
        slot_orders(&mut conn, shop.slot, domain::Clock::now(&clock))
            .await
            .unwrap(),
        2
    );
    clock.advance(SignedDuration::from_mins(20));
    assert_eq!(
        slot_orders(&mut conn, shop.slot, domain::Clock::now(&clock))
            .await
            .unwrap(),
        1
    );
}
