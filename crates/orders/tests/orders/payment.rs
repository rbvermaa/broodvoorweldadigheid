use std::sync::Arc;

use bakedays::{NewBakeDay, PickupSlotInput, days};
use domain::{Money, OrderId};
use jiff::SignedDuration;
use orders::audit;
use orders::{
    CancelReason, Error, HOLD, Order, OrderStatus, OrderedBy, PaidOutcome, PaymentReceived,
    RefundReason, allocations, checkout, mark_paid, refunds_due, release_expired_holds,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, PgPool, Postgres};
use testkit::FakeClock;

use crate::common::{
    BREAD, DE_PROEF, DONATION_LOAF, OpenDay, backend_pid, cart, charity, guest, on_range, open_day,
    order_as_guest, product, saturday, ts, wait_until_blocked, wednesday,
};

fn paid(reference: &str, order: &Order) -> PaymentReceived {
    PaymentReceived {
        reference: reference.to_owned(),
        amount: order.total,
    }
}

async fn get(conn: &mut PgConnection, id: OrderId) -> Order {
    orders::order::get(conn, id).await.unwrap()
}

async fn actions(conn: &mut PgConnection, id: OrderId) -> Vec<String> {
    audit::history(conn, "order", id.get())
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.action)
        .collect()
}

fn refund_reason(outcome: &PaidOutcome) -> Option<RefundReason> {
    match outcome {
        PaidOutcome::RefundDue {
            reason, payment, ..
        } => {
            assert_eq!(payment.refund_reason, Some(*reason));
            Some(*reason)
        }
        _ => None,
    }
}

async fn bread_day(conn: &mut PgConnection, clock: &FakeClock) -> (OpenDay, domain::ProductId) {
    let shop = open_day(conn, clock, &saturday()).await;
    let bread = product(conn, &BREAD).await;
    on_range(conn, &shop, bread, Some(1)).await;
    (shop, bread)
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_paid_charity_order_earmarks_the_full_amount(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let voedselbank = charity(&mut conn, "Voedselbank").await;
    let shop = open_day(
        &mut conn,
        &clock,
        &NewBakeDay {
            charity_id: Some(voedselbank),
            ..saturday()
        },
    )
    .await;
    let loaf = product(&mut conn, &DONATION_LOAF).await;
    on_range(&mut conn, &shop, loaf, None).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(loaf, 2, Some(1250))])
        .await
        .unwrap();
    assert_eq!(order.total, Money::from_cents(2500));

    clock.advance(SignedDuration::from_mins(3));
    let outcome = mark_paid(&mut conn, &clock, order.id, &paid("tr_1", &order))
        .await
        .unwrap();
    let PaidOutcome::Paid {
        order: paid_order,
        allocation: Some(allocation),
        saved: None,
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(paid_order.status, OrderStatus::Paid);
    assert_eq!(paid_order.paid_at, Some(ts("2026-10-28T12:03:00Z")));
    assert_eq!(allocation.charity_id, voedselbank);
    assert_eq!(allocation.amount, Money::from_cents(2500));
    assert_eq!(
        allocations(&mut conn, order.id).await.unwrap(),
        [allocation]
    );
    let allocation_status: String =
        sqlx::query_scalar("SELECT status FROM donation_allocation WHERE id = $1")
            .bind(allocation.id.get())
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(allocation_status, "earmarked");

    let replay = mark_paid(&mut conn, &clock, order.id, &paid("tr_1", &order))
        .await
        .unwrap();
    assert_eq!(replay, PaidOutcome::AlreadyPaid(paid_order.clone()));

    let twice = mark_paid(&mut conn, &clock, order.id, &paid("tr_2", &order))
        .await
        .unwrap();
    assert_eq!(refund_reason(&twice), Some(RefundReason::PaidTwice));
    let again = mark_paid(&mut conn, &clock, order.id, &paid("tr_2", &order))
        .await
        .unwrap();
    let (
        PaidOutcome::RefundDue {
            repeated: false, ..
        },
        PaidOutcome::RefundDue {
            repeated: true,
            payment,
            reason,
            order,
        },
    ) = (&twice, &again)
    else {
        panic!("{twice:?} then {again:?}");
    };
    assert_eq!(
        twice,
        PaidOutcome::RefundDue {
            order: order.clone(),
            payment: payment.clone(),
            reason: *reason,
            repeated: false,
        }
    );
    assert_eq!(allocations(&mut conn, order.id).await.unwrap().len(), 1);
    assert_eq!(get(&mut conn, order.id).await, paid_order);

    let due = refunds_due(&mut conn).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(
        (
            due[0].provider_payment_id.as_str(),
            due[0].amount,
            due[0].refunded
        ),
        ("tr_2", Money::from_cents(2500), Money::ZERO)
    );
    sqlx::query("UPDATE payment SET refunded_cents = amount_cents WHERE id = $1")
        .bind(due[0].id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(refunds_due(&mut conn).await.unwrap().is_empty());

    assert_eq!(
        actions(&mut conn, order.id).await,
        ["created", "paid", "payment_refused"]
    );
    let history = audit::history(&mut conn, "order", order.id.get())
        .await
        .unwrap();
    assert_eq!(history[1].actor, "system:payment");
    assert_eq!(
        history[1].after.as_ref().unwrap()["payment"]["reference"],
        "tr_1"
    );
    assert_eq!(
        history[2].after.as_ref().unwrap()["payment"]["refund_reason"],
        "paid_twice"
    );
    let earmarked = audit::history(&mut conn, "donation_allocation", allocation.id.get())
        .await
        .unwrap();
    assert_eq!(earmarked[0].after.as_ref().unwrap()["amount_cents"], 2500);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_normal_sale_earmarks_nothing_and_a_wrong_amount_goes_back(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let wrong = PaymentReceived {
        reference: "tr_0".to_owned(),
        amount: Money::from_cents(1),
    };
    let outcome = mark_paid(&mut conn, &clock, order.id, &wrong)
        .await
        .unwrap();
    assert_eq!(refund_reason(&outcome), Some(RefundReason::AmountMismatch));
    assert_eq!(
        get(&mut conn, order.id).await.status,
        OrderStatus::PendingPayment
    );

    let outcome = mark_paid(&mut conn, &clock, order.id, &paid("tr_1", &order))
        .await
        .unwrap();
    assert!(
        matches!(&outcome, PaidOutcome::Paid { allocation: None, saved: None, order } if order.status == OrderStatus::Paid),
        "{outcome:?}"
    );
    assert!(allocations(&mut conn, order.id).await.unwrap().is_empty());
    assert_eq!(
        refunds_due(&mut conn).await.unwrap()[0].amount,
        Money::from_cents(1)
    );

    let other = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    assert!(matches!(
        mark_paid(&mut conn, &clock, other.id, &paid("tr_1", &other)).await,
        Err(Error::PaymentOfOtherOrder)
    ));
    assert!(matches!(
        mark_paid(&mut conn, &clock, OrderId::new(999), &paid("tr_9", &order)).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_payment_after_the_hold_is_refunded_even_with_bread_left(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(10)).await;
    let late = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let released = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    clock.advance(HOLD);

    let outcome = mark_paid(&mut conn, &clock, late.id, &paid("tr_late", &late))
        .await
        .unwrap();
    assert_eq!(refund_reason(&outcome), Some(RefundReason::HoldExpired));
    let PaidOutcome::RefundDue { order, .. } = outcome else {
        unreachable!()
    };
    assert_eq!(
        (order.status, order.cancel_reason, order.paid_at),
        (
            OrderStatus::Cancelled,
            Some(CancelReason::HoldExpired),
            None
        )
    );
    assert_eq!(
        actions(&mut conn, late.id).await,
        ["created", "payment_refused"]
    );

    assert_eq!(
        release_expired_holds(&mut conn, &clock).await.unwrap(),
        [released.id]
    );
    let outcome = mark_paid(
        &mut conn,
        &clock,
        released.id,
        &paid("tr_released", &released),
    )
    .await
    .unwrap();
    assert_eq!(refund_reason(&outcome), Some(RefundReason::HoldExpired));
    assert_eq!(refunds_due(&mut conn).await.unwrap().len(), 2);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn payments_for_cancelled_days_and_orders_go_back(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let by_staff = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE "order" SET status = 'cancelled', cancel_reason = 'staff' WHERE id = $1"#,
    )
    .bind(by_staff.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    let confirmed = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "order" SET status = 'confirmed' WHERE id = $1"#)
        .bind(confirmed.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let pending = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();

    let outcome = mark_paid(&mut conn, &clock, by_staff.id, &paid("tr_1", &by_staff))
        .await
        .unwrap();
    assert_eq!(refund_reason(&outcome), Some(RefundReason::OrderCancelled));
    assert_eq!(
        get(&mut conn, by_staff.id).await.cancel_reason,
        Some(CancelReason::Staff)
    );

    let outcome = mark_paid(&mut conn, &clock, confirmed.id, &paid("tr_2", &confirmed))
        .await
        .unwrap();
    assert_eq!(refund_reason(&outcome), Some(RefundReason::OrderConfirmed));
    assert_eq!(
        get(&mut conn, confirmed.id).await.status,
        OrderStatus::Confirmed
    );

    days::cancel(&mut conn, &clock, shop.day.id, "Ziek")
        .await
        .unwrap();
    let outcome = mark_paid(&mut conn, &clock, pending.id, &paid("tr_3", &pending))
        .await
        .unwrap();
    assert_eq!(refund_reason(&outcome), Some(RefundReason::DayFinal));
    assert_eq!(
        get(&mut conn, pending.id).await.cancel_reason,
        Some(CancelReason::DayCancelled)
    );
    assert_eq!(refunds_due(&mut conn).await.unwrap().len(), 3);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_payment_waiting_on_the_release_job_at_expiry_goes_back(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = wednesday();
    let (shop, bread) = bread_day(&mut conn, &clock).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    drop(conn);

    let mut job = pool.begin().await.unwrap();
    let expiry = FakeClock::new(ts("2026-10-28T12:15:00Z"));
    assert_eq!(
        release_expired_holds(&mut job, &expiry).await.unwrap(),
        [order.id]
    );

    let mut payer = pool.acquire().await.unwrap();
    let pid = backend_pid(&mut payer).await;
    let payment = paid("tr_1", &order);
    let task = tokio::spawn(async move {
        let just_in_time = FakeClock::new(ts("2026-10-28T12:14:59Z"));
        mark_paid(&mut payer, &just_in_time, order.id, &payment).await
    });
    wait_until_blocked(&pool, pid).await;
    job.commit().await.unwrap();
    let outcome = task.await.unwrap().unwrap();
    assert_eq!(refund_reason(&outcome), Some(RefundReason::HoldExpired));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_release_job_skips_an_order_being_paid(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = wednesday();
    let (shop, bread) = bread_day(&mut conn, &clock).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();

    let mut payer = pool.begin().await.unwrap();
    let just_in_time = FakeClock::new(ts("2026-10-28T12:14:59Z"));
    let outcome = mark_paid(&mut payer, &just_in_time, order.id, &paid("tr_1", &order))
        .await
        .unwrap();
    assert!(matches!(outcome, PaidOutcome::Paid { .. }), "{outcome:?}");

    let expiry = FakeClock::new(ts("2026-10-28T12:15:00Z"));
    assert!(
        release_expired_holds(&mut conn, &expiry)
            .await
            .unwrap()
            .is_empty()
    );
    payer.commit().await.unwrap();
    assert!(
        release_expired_holds(&mut conn, &expiry)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(get(&mut conn, order.id).await.status, OrderStatus::Paid);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_payment_that_waits_past_its_hold_for_a_checkout_goes_back(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = wednesday();
    let (shop, bread) = bread_day(&mut conn, &clock).await;
    let sloth = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let later_slot = days::add_slot(
        &mut conn,
        shop.day.id,
        DE_PROEF,
        &PickupSlotInput {
            starts_at: ts("2026-10-31T11:00:00Z"),
            ends_at: ts("2026-10-31T12:00:00Z"),
            max_orders: None,
        },
    )
    .await
    .unwrap();
    drop(conn);

    let expired = FakeClock::new(ts("2026-10-28T12:15:00Z"));
    let mut quick = pool.begin().await.unwrap();
    checkout(
        &mut quick,
        &expired,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("snel@example.org"),
        later_slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();

    let payer_clock = Arc::new(FakeClock::new(ts("2026-10-28T12:14:59Z")));
    let mut payer = pool.acquire().await.unwrap();
    let pid = backend_pid(&mut payer).await;
    let task = tokio::spawn({
        let clock = Arc::clone(&payer_clock);
        let payment = paid("tr_1", &sloth);
        async move { mark_paid(&mut payer, clock.as_ref(), sloth.id, &payment).await }
    });
    wait_until_blocked(&pool, pid).await;
    payer_clock.set(ts("2026-10-28T12:15:00Z"));
    quick.commit().await.unwrap();
    let outcome = task.await.unwrap().unwrap();
    assert_eq!(refund_reason(&outcome), Some(RefundReason::HoldExpired));
}
