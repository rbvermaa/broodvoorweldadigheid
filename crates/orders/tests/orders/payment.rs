use bakedays::{NewBakeDay, days};
use domain::{Money, OrderId, StaffUserId};
use jiff::SignedDuration;
use orders::audit;
use orders::{
    CancelReason, Error, HOLD, Order, OrderStatus, OrderedBy, PaidOutcome, PaymentReceived,
    RefundReason, allocations, checkout, mark_paid, release_expired_holds,
};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{
    BREAD, DONATION_LOAF, OpenDay, cart, charity, guest, on_range, open_day, order_as_guest,
    product, saturday, ts, wednesday,
};

fn paid(reference: &str, order: &Order) -> PaymentReceived {
    PaymentReceived {
        reference: reference.to_owned(),
        amount: order.total,
    }
}

async fn status(conn: &mut sqlx::PgConnection, id: OrderId) -> Order {
    orders::order::get(conn, id).await.unwrap()
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
    on_range(&mut conn, &shop, loaf, None, None).await;
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
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(paid_order.status, OrderStatus::Paid);
    assert_eq!(paid_order.payment_ref.as_deref(), Some("tr_1"));
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
    assert_eq!(
        twice,
        PaidOutcome::RefundDue {
            order: paid_order,
            reason: RefundReason::PaidTwice
        }
    );
    assert_eq!(allocations(&mut conn, order.id).await.unwrap().len(), 1);

    let history = audit::history(&mut conn, "order", order.id.get())
        .await
        .unwrap();
    let actions: Vec<_> = history.iter().map(|h| h.action.as_str()).collect();
    assert_eq!(actions, ["created", "paid", "second_payment"]);
    assert_eq!(history[1].actor, "system:payment");
    assert_eq!(
        history[1].after.as_ref().unwrap()["payment"]["reference"],
        "tr_1"
    );
    assert_eq!(
        history[2].after.as_ref().unwrap()["payment"]["reference"],
        "tr_2"
    );
    let earmarked = audit::history(&mut conn, "donation_allocation", allocation.id.get())
        .await
        .unwrap();
    assert_eq!(earmarked[0].after.as_ref().unwrap()["amount_cents"], 2500);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_normal_sale_earmarks_nothing(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let wrong = PaymentReceived {
        reference: "tr_1".to_owned(),
        amount: Money::from_cents(1),
    };
    assert!(matches!(
        mark_paid(&mut conn, &clock, order.id, &wrong).await,
        Err(Error::AmountMismatch { .. })
    ));
    assert_eq!(
        status(&mut conn, order.id).await.status,
        OrderStatus::PendingPayment
    );
    let outcome = mark_paid(&mut conn, &clock, order.id, &paid("tr_1", &order))
        .await
        .unwrap();
    assert!(
        matches!(&outcome, PaidOutcome::Paid { allocation: None, order } if order.status == OrderStatus::Paid),
        "{outcome:?}"
    );
    assert!(allocations(&mut conn, order.id).await.unwrap().is_empty());
    assert!(matches!(
        mark_paid(&mut conn, &clock, OrderId::new(999), &paid("tr_9", &order)).await,
        Err(Error::NotFound)
    ));
}

async fn one_loaf_day(
    conn: &mut sqlx::PgConnection,
    clock: &FakeClock,
) -> (OpenDay, domain::ProductId) {
    let shop = open_day(conn, clock, &saturday()).await;
    let bread = product(conn, &BREAD).await;
    on_range(conn, &shop, bread, Some(1), None).await;
    (shop, bread)
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_late_payment_counts_while_the_loaf_is_still_there(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let (shop, bread) = one_loaf_day(&mut conn, &clock).await;
    let late = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    clock.advance(HOLD);
    let outcome = mark_paid(&mut conn, &clock, late.id, &paid("tr_1", &late))
        .await
        .unwrap();
    assert!(matches!(outcome, PaidOutcome::Paid { .. }), "{outcome:?}");

    let released = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)]).await;
    assert!(
        matches!(released, Err(Error::CapacityExceeded { .. })),
        "{released:?}"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_payment_after_release_revives_the_order_if_it_fits(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let (shop, bread) = one_loaf_day(&mut conn, &clock).await;
    let late = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    clock.advance(HOLD);
    release_expired_holds(&mut conn, &clock).await.unwrap();
    let outcome = mark_paid(&mut conn, &clock, late.id, &paid("tr_1", &late))
        .await
        .unwrap();
    let PaidOutcome::Paid { order, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        (order.status, order.cancel_reason),
        (OrderStatus::Paid, None)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_late_payment_for_a_loaf_sold_since_is_refunded(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let (shop, bread) = one_loaf_day(&mut conn, &clock).await;
    let sloth = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    clock.advance(HOLD);
    let quick = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("snel@example.org"),
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();

    let outcome = mark_paid(&mut conn, &clock, sloth.id, &paid("tr_late", &sloth))
        .await
        .unwrap();
    let PaidOutcome::RefundDue { order, reason } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(reason, RefundReason::SoldOut);
    assert_eq!(order.status, OrderStatus::Cancelled);
    assert_eq!(order.cancel_reason, Some(CancelReason::HoldExpired));
    assert_eq!(order.payment_ref.as_deref(), Some("tr_late"));
    assert!(order.refund_due());
    assert_eq!(order.paid_at, None);

    let replay = mark_paid(&mut conn, &clock, sloth.id, &paid("tr_late", &sloth))
        .await
        .unwrap();
    assert_eq!(
        replay,
        PaidOutcome::RefundDue {
            order: order.clone(),
            reason: RefundReason::OrderCancelled
        }
    );
    let another = mark_paid(&mut conn, &clock, sloth.id, &paid("tr_again", &sloth))
        .await
        .unwrap();
    assert!(
        matches!(another, PaidOutcome::RefundDue { reason: RefundReason::OrderCancelled, ref order } if order.payment_ref.as_deref() == Some("tr_late")),
        "{another:?}"
    );

    let quick_paid = mark_paid(&mut conn, &clock, quick.id, &paid("tr_quick", &quick))
        .await
        .unwrap();
    assert!(matches!(quick_paid, PaidOutcome::Paid { .. }));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_full_slot_also_refuses_a_late_payment(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    sqlx::query("UPDATE pickup_slot SET max_orders = 1 WHERE id = $1")
        .bind(shop.slot.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let sloth = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    clock.advance(HOLD);
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let outcome = mark_paid(&mut conn, &clock, sloth.id, &paid("tr_1", &sloth))
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            PaidOutcome::RefundDue {
                reason: RefundReason::SoldOut,
                ..
            }
        ),
        "{outcome:?}"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_late_payment_after_the_deadline_is_refunded_unless_staff_took_the_order(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-29T18:50:00Z"));
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let web = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let staff = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("balie@example.org"),
        shop.slot,
        OrderedBy::Staff(StaffUserId::new(2)),
    )
    .await
    .unwrap();
    let within = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();

    clock.set(ts("2026-10-29T19:04:59Z"));
    let in_time = mark_paid(&mut conn, &clock, within.id, &paid("tr_in_time", &within))
        .await
        .unwrap();
    assert!(matches!(in_time, PaidOutcome::Paid { .. }), "{in_time:?}");

    clock.set(ts("2026-10-29T19:05:00Z"));
    let outcome = mark_paid(&mut conn, &clock, web.id, &paid("tr_web", &web))
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            PaidOutcome::RefundDue {
                reason: RefundReason::PastDeadline,
                ..
            }
        ),
        "{outcome:?}"
    );
    let outcome = mark_paid(&mut conn, &clock, staff.id, &paid("tr_staff", &staff))
        .await
        .unwrap();
    assert!(matches!(outcome, PaidOutcome::Paid { .. }), "{outcome:?}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_payment_for_a_cancelled_day_is_refunded(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    days::cancel(&mut conn, &clock, shop.day.id, "Ziek")
        .await
        .unwrap();
    let outcome = mark_paid(&mut conn, &clock, order.id, &paid("tr_1", &order))
        .await
        .unwrap();
    let PaidOutcome::RefundDue { order, reason } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(reason, RefundReason::DayFinal);
    assert_eq!(order.cancel_reason, Some(CancelReason::DayCancelled));
    assert!(order.refund_due());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_payment_for_an_order_cancelled_by_staff_is_refunded(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE "order" SET status = 'cancelled', cancel_reason = 'staff' WHERE id = $1"#,
    )
    .bind(order.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    let outcome = mark_paid(&mut conn, &clock, order.id, &paid("tr_1", &order))
        .await
        .unwrap();
    let PaidOutcome::RefundDue {
        order: cancelled,
        reason,
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(reason, RefundReason::OrderCancelled);
    assert_eq!(cancelled.cancel_reason, Some(CancelReason::Staff));
    assert!(cancelled.refund_due());

    let confirmed = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "order" SET status = 'confirmed' WHERE id = $1"#)
        .bind(confirmed.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let outcome = mark_paid(&mut conn, &clock, confirmed.id, &paid("tr_2", &confirmed))
        .await
        .unwrap();
    assert!(
        matches!(&outcome, PaidOutcome::RefundDue { reason: RefundReason::OrderCancelled, order } if order.status == OrderStatus::Confirmed && order.payment_ref.is_none()),
        "{outcome:?}"
    );
}
