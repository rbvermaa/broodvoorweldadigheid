use bakedays::OrderSource;
use bakedays::{PickupSlotInput, days};
use domain::{Clock, Money, OrderId, PickupLocationId, PickupSlotId, ProductId, StaffUserId};
use orders::audit;
use orders::manage::{
    OrderFilter, cancel_by_staff, day_orders, mark_picked_up, move_pickup, payments,
};
use orders::{
    CancelReason, Error, Order, OrderStatus, Orders, PaidOutcome, PaymentReceived, mark_paid,
    refunds_due,
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
    on_range(&mut conn, &shop, bread, None).await;
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
    on_range(&mut conn, &shop, bread, None).await;
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
    on_range(&mut conn, &shop, bread, None).await;
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
    on_range(&mut conn, &shop, bread, None).await;
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
async fn staff_cancel_a_pending_order_and_free_its_loaf(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(1)).await;
    let pending = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let cancelled = cancel_by_staff(&mut conn, &clock, pending.id, BAS)
        .await
        .unwrap();
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert_eq!(cancelled.cancel_reason, Some(CancelReason::Staff));
    let log = history(&mut conn, pending.id).await;
    let last = log.last().unwrap();
    assert_eq!(last.action, "cancelled");
    assert_eq!(last.actor, "staff:7");
    assert_eq!(last.before.as_ref().unwrap()["status"], "pending_payment");
    assert!(
        Orders::new(&clock)
            .ordered(&mut conn, shop.day.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        cancel_by_staff(&mut conn, &clock, pending.id, BAS).await,
        Err(Error::WrongStatus(OrderStatus::Cancelled))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn staff_cannot_cancel_a_paid_order_yet(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let paid = paid_order(&mut conn, &clock, &shop, bread, "tr_1").await;
    assert!(matches!(
        cancel_by_staff(&mut conn, &clock, paid.id, BAS).await,
        Err(Error::WrongStatus(OrderStatus::Paid))
    ));
    let unchanged = orders::order::get(&mut conn, paid.id).await.unwrap();
    assert_eq!(unchanged, paid);
    assert!(refunds_due(&mut conn).await.unwrap().is_empty());
    assert_eq!(
        payments(&mut conn, paid.id).await.unwrap()[0].refund_reason,
        None
    );

    let completing = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO payment (order_id, provider_payment_id, amount_cents, status, received_at)
        VALUES ($1, 'tr_2', $2, 'authorized', now())",
    )
    .bind(completing.id.get())
    .bind(completing.total.cents())
    .execute(&mut *conn)
    .await
    .unwrap();
    assert!(matches!(
        cancel_by_staff(&mut conn, &clock, completing.id, BAS).await,
        Err(Error::WrongStatus(OrderStatus::PendingPayment))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_invoiced_order_keeps_its_slot_but_can_be_picked_up(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let later = slot(
        &mut conn,
        &shop,
        DE_PROEF,
        "2026-10-31T12:00:00Z",
        "2026-10-31T13:00:00Z",
        None,
    )
    .await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    sqlx::query(
        r#"WITH invoice AS (
            INSERT INTO manual_invoice (customer_id, invoice_number, invoice_date, period_from,
                period_to, total_cents, recorded_at, recorded_by)
            VALUES ($2, 'F-1', '2026-10-28', '2026-10-01', '2026-10-31', 450, now(), 'staff:1')
            RETURNING id)
        UPDATE "order" SET status = 'confirmed', payment_mode = 'on_account',
            manual_invoice_id = (SELECT id FROM invoice)
        WHERE id = $1"#,
    )
    .bind(order.id.get())
    .bind(order.customer_id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    assert!(orders::manage::invoiced(&mut conn, order.id).await.unwrap());
    assert!(matches!(
        move_pickup(&mut conn, &clock, order.id, later, BAS).await,
        Err(Error::Invoiced)
    ));
    let picked = mark_picked_up(&mut conn, &clock, order.id, BAS)
        .await
        .unwrap();
    assert_eq!(picked.status, OrderStatus::PickedUp);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn staff_cannot_mark_an_order_of_a_cancelled_day_picked_up(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let order = paid_order(&mut conn, &clock, &shop, bread, "tr_1").await;
    days::cancel(&mut conn, &clock, shop.day.id, "Ziek")
        .await
        .unwrap();
    assert!(matches!(
        mark_picked_up(&mut conn, &clock, order.id, BAS).await,
        Err(Error::DayFinal)
    ));
    assert_eq!(
        orders::order::get(&mut conn, order.id)
            .await
            .unwrap()
            .status,
        OrderStatus::Paid
    );
}
