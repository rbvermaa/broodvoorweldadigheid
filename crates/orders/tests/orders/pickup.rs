use domain::{BakeDayId, Money, StaffUserId};
use jiff::SignedDuration;
use orders::order::{self, get};
use orders::{
    Error, Order, OrderStatus, PaymentReceived, audit, mark_paid, mark_picked_up, undo_picked_up,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

use crate::common::{
    BREAD, COOKIES, OpenDay, on_range, open_day, order_as_guest, product, saturday, wednesday,
};

const BAKER: StaffUserId = StaffUserId::new(7);

async fn paid_order(conn: &mut PgConnection, clock: &FakeClock, shop: &OpenDay) -> Order {
    let bread = product(conn, &BREAD).await;
    on_range(conn, shop, bread, None, None).await;
    let order = order_as_guest(conn, clock, shop, &[(bread, 2, None)])
        .await
        .unwrap();
    let payment = PaymentReceived {
        reference: format!("tr_{}", order.id.get()),
        amount: order.total,
    };
    mark_paid(conn, clock, order.id, &payment).await.unwrap();
    get(conn, order.id).await.unwrap()
}

async fn actions(conn: &mut PgConnection, order: &Order) -> Vec<String> {
    audit::history(conn, "order", order.id.get())
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.action)
        .collect()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn picking_up_a_paid_order_is_audited_and_can_be_undone(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let order = paid_order(&mut conn, &clock, &shop).await;

    clock.set("2026-10-31T10:05:00Z".parse().unwrap());
    let picked = mark_picked_up(&mut conn, &clock, order.id, BAKER)
        .await
        .unwrap();
    assert_eq!(picked.status, OrderStatus::PickedUp);
    assert_eq!(picked.paid_at, order.paid_at);
    assert_eq!(get(&mut conn, order.id).await.unwrap(), picked);

    let history = audit::history(&mut conn, "order", order.id.get())
        .await
        .unwrap();
    let entry = history.last().unwrap();
    assert_eq!(entry.action, "picked_up");
    assert_eq!(entry.actor, "staff:7");
    assert_eq!(entry.at, "2026-10-31T10:05:00Z".parse().unwrap());
    assert_eq!(entry.before.as_ref().unwrap()["status"], "paid");
    assert_eq!(entry.after.as_ref().unwrap()["status"], "picked_up");

    clock.advance(SignedDuration::from_mins(1));
    let undone = undo_picked_up(&mut conn, &clock, order.id, BAKER)
        .await
        .unwrap();
    assert_eq!(undone, order);
    let history = audit::history(&mut conn, "order", order.id.get())
        .await
        .unwrap();
    let entry = history.last().unwrap();
    assert_eq!(entry.action, "picked_up_undone");
    assert_eq!(entry.before.as_ref().unwrap()["status"], "picked_up");
    assert_eq!(entry.after.as_ref().unwrap()["status"], "paid");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn ticking_twice_changes_and_records_nothing(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let order = paid_order(&mut conn, &clock, &shop).await;

    let undone = undo_picked_up(&mut conn, &clock, order.id, BAKER)
        .await
        .unwrap();
    assert_eq!(undone, order);
    let picked = mark_picked_up(&mut conn, &clock, order.id, BAKER)
        .await
        .unwrap();
    let again = mark_picked_up(&mut conn, &clock, order.id, BAKER)
        .await
        .unwrap();
    assert_eq!(again, picked);
    assert_eq!(
        actions(&mut conn, &order).await,
        ["created", "paid", "picked_up"]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_confirmed_order_goes_back_to_confirmed(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let order = paid_order(&mut conn, &clock, &shop).await;
    sqlx::query(
        r#"UPDATE "order" SET status = 'confirmed', payment_mode = 'on_account', paid_at = NULL
        WHERE id = $1"#,
    )
    .bind(order.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    let confirmed = get(&mut conn, order.id).await.unwrap();

    mark_picked_up(&mut conn, &clock, order.id, BAKER)
        .await
        .unwrap();
    let undone = undo_picked_up(&mut conn, &clock, order.id, BAKER)
        .await
        .unwrap();
    assert_eq!(undone, confirmed);
    assert_eq!(undone.status, OrderStatus::Confirmed);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_paid_or_confirmed_orders_can_be_picked_up(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let cookies = product(&mut conn, &COOKIES).await;
    on_range(&mut conn, &shop, cookies, None, None).await;
    let pending = order_as_guest(&mut conn, &clock, &shop, &[(cookies, 1, None)])
        .await
        .unwrap();
    assert!(matches!(
        mark_picked_up(&mut conn, &clock, pending.id, BAKER).await,
        Err(Error::NotPickable(OrderStatus::PendingPayment))
    ));
    assert!(matches!(
        undo_picked_up(&mut conn, &clock, pending.id, BAKER).await,
        Err(Error::NotPickable(OrderStatus::PendingPayment))
    ));

    clock.advance(orders::HOLD);
    orders::release_expired_holds(&mut conn, &clock)
        .await
        .unwrap();
    assert!(matches!(
        mark_picked_up(&mut conn, &clock, pending.id, BAKER).await,
        Err(Error::NotPickable(OrderStatus::Cancelled))
    ));
    assert_eq!(
        actions(&mut conn, &pending).await,
        ["created", "hold_expired"]
    );

    assert!(matches!(
        mark_picked_up(&mut conn, &clock, domain::OrderId::new(999_999), BAKER).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        undo_picked_up(&mut conn, &clock, domain::OrderId::new(999_999), BAKER).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn firm_orders_of_a_day_leave_out_holds_and_cancelled_ones(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let paid = paid_order(&mut conn, &clock, &shop).await;
    let cookies = product(&mut conn, &COOKIES).await;
    on_range(&mut conn, &shop, cookies, None, None).await;
    let held = order_as_guest(&mut conn, &clock, &shop, &[(cookies, 1, None)])
        .await
        .unwrap();
    assert_eq!(held.status, OrderStatus::PendingPayment);
    let picked = paid_order(&mut conn, &clock, &shop).await;
    let picked = mark_picked_up(&mut conn, &clock, picked.id, BAKER)
        .await
        .unwrap();

    let firm = order::list_firm(&mut conn, shop.day.id).await.unwrap();
    assert_eq!(firm, [paid.clone(), picked]);
    assert_eq!(firm[0].lines.len(), 1);
    assert_eq!(firm[0].total, Money::from_cents(900));
    assert!(
        order::list_firm(&mut conn, BakeDayId::new(999_999))
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn undo_needs_the_pickup_in_the_audit_log(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let order = paid_order(&mut conn, &clock, &shop).await;
    sqlx::query(r#"UPDATE "order" SET status = 'picked_up' WHERE id = $1"#)
        .bind(order.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(matches!(
        undo_picked_up(&mut conn, &clock, order.id, BAKER).await,
        Err(Error::Corrupt(_))
    ));

    sqlx::query(
        "INSERT INTO audit_log (entity, entity_id, action, before, actor, at)
        VALUES ('order', $1, 'picked_up', '{\"status\": \"cancelled\"}', 'staff:7', now())",
    )
    .bind(order.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    assert!(matches!(
        undo_picked_up(&mut conn, &clock, order.id, BAKER).await,
        Err(Error::Corrupt(_))
    ));
    assert_eq!(
        get(&mut conn, order.id).await.unwrap().status,
        OrderStatus::PickedUp
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_pickup_on_a_cancelled_day_cannot_be_undone(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let order = paid_order(&mut conn, &clock, &shop).await;
    mark_picked_up(&mut conn, &clock, order.id, BAKER)
        .await
        .unwrap();
    bakedays::days::cancel(&mut conn, &clock, shop.day.id, "Ziek")
        .await
        .unwrap();

    assert!(matches!(
        undo_picked_up(&mut conn, &clock, order.id, BAKER).await,
        Err(Error::DayFinal)
    ));
    assert_eq!(
        get(&mut conn, order.id).await.unwrap().status,
        OrderStatus::PickedUp
    );
}
