use bakedays::OrderSource;
use bakedays::{PickupSlotInput, days};
use domain::{Clock, Money, OrderId, PickupLocationId, PickupSlotId, ProductId, StaffUserId};
use orders::audit;
use orders::manage::{
    OrderFilter, cancel_by_staff, day_orders, mark_picked_up, move_pickup, payments,
};
use orders::{
    CancelReason, Error, Order, OrderStatus, Orders, PaidOutcome, PaymentReceived, RefundReason,
    mark_paid, refunds_due,
};
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{
    BREAD, DE_PROEF, OpenDay, on_range, open_day, order_as_guest, product, saturday, ts, wednesday,
};

const BAS: StaffUserId = StaffUserId::new(7);

async fn pay(conn: &mut PgConnection, clock: &FakeClock, order: &Order, reference: &str) {
    let outcome = mark_paid(
        conn,
        clock,
        order.id,
        &PaymentReceived {
            reference: reference.to_owned(),
            amount: order.total,
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, PaidOutcome::Paid { .. }), "{outcome:?}");
}

async fn paid_order(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &OpenDay,
    bread: ProductId,
    reference: &str,
) -> Order {
    let order = order_as_guest(conn, clock, shop, &[(bread, 1, None)])
        .await
        .unwrap();
    pay(conn, clock, &order, reference).await;
    orders::order::get(conn, order.id).await.unwrap()
}

async fn slot(
    conn: &mut PgConnection,
    shop: &OpenDay,
    location: PickupLocationId,
    starts: &str,
    ends: &str,
    max_orders: Option<i32>,
) -> PickupSlotId {
    days::add_slot(
        conn,
        shop.day.id,
        location,
        &PickupSlotInput {
            starts_at: ts(starts),
            ends_at: ts(ends),
            max_orders,
        },
    )
    .await
    .unwrap()
}

async fn other_location(conn: &mut PgConnection) -> PickupLocationId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO pickup_location (name, address) VALUES ('Markt', 'Brink 1, 8381 AA Vledder')
        RETURNING id",
    )
    .fetch_one(conn)
    .await
    .unwrap();
    PickupLocationId::new(id)
}

async fn history(conn: &mut PgConnection, id: OrderId) -> Vec<audit::Logged> {
    audit::history(conn, "order", id.get()).await.unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_day_list_filters_by_status_and_location(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let markt = other_location(&mut conn).await;
    let markt_slot = slot(
        &mut conn,
        &shop,
        markt,
        "2026-10-31T13:00:00Z",
        "2026-10-31T14:00:00Z",
        None,
    )
    .await;

    let paid = paid_order(&mut conn, &clock, &shop, bread, "tr_1").await;
    let pending = order_as_guest(&mut conn, &clock, &shop, &[(bread, 3, None)])
        .await
        .unwrap();
    let at_markt = paid_order(&mut conn, &clock, &shop, bread, "tr_2").await;
    move_pickup(&mut conn, &clock, at_markt.id, markt_slot, BAS)
        .await
        .unwrap();

    let all = day_orders(&mut conn, shop.day.id, &OrderFilter::default())
        .await
        .unwrap();
    let ids: Vec<_> = all.iter().map(|o| o.id).collect();
    assert_eq!(ids, [paid.id, pending.id, at_markt.id]);
    assert_eq!(all[1].pieces, 3);
    assert_eq!(all[1].total, Money::from_cents(1350));
    assert_eq!(all[1].status, OrderStatus::PendingPayment);
    assert_eq!(all[0].email, "klant@example.org");
    assert_eq!(all[0].customer_name, "Test Klant");
    assert_eq!(all[2].location, "Markt");
    assert_eq!(all[2].pickup_starts, ts("2026-10-31T13:00:00Z"));

    let only_paid = day_orders(
        &mut conn,
        shop.day.id,
        &OrderFilter {
            status: Some(OrderStatus::Paid),
            location: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        only_paid.iter().map(|o| o.id).collect::<Vec<_>>(),
        [paid.id, at_markt.id]
    );
    let at_de_proef = day_orders(
        &mut conn,
        shop.day.id,
        &OrderFilter {
            status: Some(OrderStatus::Paid),
            location: Some(DE_PROEF),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        at_de_proef.iter().map(|o| o.id).collect::<Vec<_>>(),
        [paid.id]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn staff_move_a_paid_order_to_another_slot_with_room(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let late = slot(
        &mut conn,
        &shop,
        DE_PROEF,
        "2026-10-31T12:00:00Z",
        "2026-10-31T13:00:00Z",
        Some(1),
    )
    .await;
    let first = paid_order(&mut conn, &clock, &shop, bread, "tr_1").await;
    let second = paid_order(&mut conn, &clock, &shop, bread, "tr_2").await;

    clock.advance(jiff::SignedDuration::from_hours(48));
    let moved = move_pickup(&mut conn, &clock, first.id, late, BAS)
        .await
        .unwrap();
    assert_eq!(moved.pickup_slot_id, late);
    assert_eq!(moved.status, OrderStatus::Paid);
    assert!(matches!(
        move_pickup(&mut conn, &clock, second.id, late, BAS).await,
        Err(Error::SlotFull)
    ));
    let again = move_pickup(&mut conn, &clock, first.id, late, BAS)
        .await
        .unwrap();
    assert_eq!(again, moved);

    let log = history(&mut conn, first.id).await;
    let moves: Vec<_> = log.iter().filter(|h| h.action == "pickup_moved").collect();
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].actor, "staff:7");
    assert_eq!(moves[0].at, clock.now());
    assert_eq!(
        moves[0].before.as_ref().unwrap()["pickup_slot_id"],
        shop.slot.get()
    );
    assert_eq!(
        moves[0].after.as_ref().unwrap()["pickup_slot_id"],
        late.get()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn moving_needs_an_open_order_and_a_slot_of_the_same_day(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let pending = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    assert!(matches!(
        move_pickup(&mut conn, &clock, pending.id, shop.slot, BAS).await,
        Err(Error::WrongStatus(OrderStatus::PendingPayment))
    ));

    let other_day = open_day(
        &mut conn,
        &clock,
        &bakedays::NewBakeDay {
            date: "2026-11-07".parse().unwrap(),
            bake_starts_at: ts("2026-11-07T03:00:00Z"),
            ..saturday()
        },
    )
    .await;
    let order = paid_order(&mut conn, &clock, &shop, bread, "tr_1").await;
    assert!(matches!(
        move_pickup(&mut conn, &clock, order.id, other_day.slot, BAS).await,
        Err(Error::SlotNotOnDay)
    ));
    assert!(matches!(
        move_pickup(&mut conn, &clock, OrderId::new(9999), shop.slot, BAS).await,
        Err(Error::NotFound)
    ));

    days::cancel(&mut conn, &clock, shop.day.id, "Ziek")
        .await
        .unwrap();
    assert!(matches!(
        move_pickup(&mut conn, &clock, order.id, shop.slot, BAS).await,
        Err(Error::DayFinal)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn staff_mark_a_paid_order_picked_up_once(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let order = paid_order(&mut conn, &clock, &shop, bread, "tr_1").await;
    let pending = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();

    clock.set(ts("2026-10-31T10:15:00Z"));
    let picked = mark_picked_up(&mut conn, &clock, order.id, BAS)
        .await
        .unwrap();
    assert_eq!(picked.status, OrderStatus::PickedUp);
    assert!(matches!(
        mark_picked_up(&mut conn, &clock, order.id, BAS).await,
        Err(Error::WrongStatus(OrderStatus::PickedUp))
    ));
    assert!(matches!(
        mark_picked_up(&mut conn, &clock, pending.id, BAS).await,
        Err(Error::WrongStatus(OrderStatus::PendingPayment))
    ));

    let log = history(&mut conn, order.id).await;
    let last = log.last().unwrap();
    assert_eq!(last.action, "picked_up");
    assert_eq!(last.actor, "staff:7");
    assert_eq!(last.before.as_ref().unwrap()["status"], "paid");
    assert_eq!(last.after.as_ref().unwrap()["status"], "picked_up");
    assert_eq!(
        Orders::new(&clock)
            .ordered(&mut conn, shop.day.id)
            .await
            .unwrap()[&bread],
        1,
        "a picked-up order still counts"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn cancelling_a_paid_order_frees_the_loaf_and_marks_the_payment_for_refund(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(1), None).await;
    let order = paid_order(&mut conn, &clock, &shop, bread, "tr_1").await;
    assert!(refunds_due(&mut conn).await.unwrap().is_empty());

    let cancelled = cancel_by_staff(&mut conn, &clock, order.id, BAS)
        .await
        .unwrap();
    assert_eq!(cancelled.order.status, OrderStatus::Cancelled);
    assert_eq!(cancelled.order.cancel_reason, Some(CancelReason::Staff));
    let refund = cancelled.refund.unwrap();
    assert_eq!(refund.amount, order.total);
    assert_eq!(refund.refund_reason, Some(RefundReason::OrderCancelled));
    assert_eq!(
        refunds_due(&mut conn).await.unwrap(),
        std::slice::from_ref(&refund)
    );
    assert_eq!(payments(&mut conn, order.id).await.unwrap(), [refund]);

    assert!(
        Orders::new(&clock)
            .ordered(&mut conn, shop.day.id)
            .await
            .unwrap()
            .is_empty()
    );
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();

    assert!(matches!(
        cancel_by_staff(&mut conn, &clock, order.id, BAS).await,
        Err(Error::WrongStatus(OrderStatus::Cancelled))
    ));
    let log = history(&mut conn, order.id).await;
    let last = log.last().unwrap();
    assert_eq!(last.action, "cancelled");
    assert_eq!(last.actor, "staff:7");
    assert_eq!(last.before.as_ref().unwrap()["status"], "paid");
    assert_eq!(last.after.as_ref().unwrap()["cancel_reason"], "staff");
    assert_eq!(
        last.after.as_ref().unwrap()["payment"]["refund_reason"],
        "order_cancelled"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn cancelling_a_pending_order_needs_no_refund(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let pending = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let cancelled = cancel_by_staff(&mut conn, &clock, pending.id, BAS)
        .await
        .unwrap();
    assert_eq!(cancelled.order.status, OrderStatus::Cancelled);
    assert_eq!(cancelled.order.cancel_reason, Some(CancelReason::Staff));
    assert_eq!(cancelled.refund, None);
    let log = history(&mut conn, pending.id).await;
    assert_eq!(log.last().unwrap().actor, "staff:7");

    let picked = paid_order(&mut conn, &clock, &shop, bread, "tr_9").await;
    mark_picked_up(&mut conn, &clock, picked.id, BAS)
        .await
        .unwrap();
    assert!(matches!(
        cancel_by_staff(&mut conn, &clock, picked.id, BAS).await,
        Err(Error::WrongStatus(OrderStatus::PickedUp))
    ));
}
