use bakedays::{OrderSource, days};
use domain::{OrderId, PickupSlotId, ProductId};
use jiff::SignedDuration;
use orders::audit;
use orders::{
    CancelReason, CustomerInput, Error, HOLD, OrderStatus, OrderedBy, Orders, PaidOutcome,
    PaymentReceived, RefundReason, cancel_pending, checkout_replacing, mark_paid,
    release_expired_holds, same_as_order,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};

use crate::common::{
    BREAD, DONATION_LOAF, cart, guest, on_range, open_day, order_as_guest, product, saturday,
    wednesday,
};

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_expired_hold_frees_the_loaf_and_the_job_cancels_the_order(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(1)).await;
    let held = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)]).await,
        Err(Error::CapacityExceeded { remaining: 0, .. })
    ));

    clock.advance(HOLD - SignedDuration::from_secs(1));
    assert!(
        release_expired_holds(&mut conn, &clock)
            .await
            .unwrap()
            .is_empty()
    );
    let source = Orders::new(&clock);
    assert_eq!(
        source.ordered(&mut conn, shop.day.id).await.unwrap()[&bread],
        1
    );

    clock.advance(SignedDuration::from_secs(1));
    assert!(
        source
            .ordered(&mut conn, shop.day.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(source.count(&mut conn, shop.day.id).await.unwrap(), 1);
    assert_eq!(
        release_expired_holds(&mut conn, &clock).await.unwrap(),
        [held.id]
    );
    assert!(
        release_expired_holds(&mut conn, &clock)
            .await
            .unwrap()
            .is_empty()
    );
    let released = orders::order::get(&mut conn, held.id).await.unwrap();
    assert_eq!(released.status, OrderStatus::Cancelled);
    assert_eq!(released.cancel_reason, Some(CancelReason::HoldExpired));
    assert_eq!(source.count(&mut conn, shop.day.id).await.unwrap(), 0);

    let history = audit::history(&mut conn, "order", held.id.get())
        .await
        .unwrap();
    let actions: Vec<_> = history.iter().map(|h| h.action.as_str()).collect();
    assert_eq!(actions, ["created", "hold_expired"]);
    assert_eq!(history[1].actor, "system:hold-expiry");
    assert_eq!(
        history[1].before.as_ref().unwrap()["status"],
        "pending_payment"
    );
    assert_eq!(
        history[1].after.as_ref().unwrap()["cancel_reason"],
        "hold_expired"
    );
    assert_eq!(history[1].at, domain::Clock::now(&clock));

    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_charity_is_fixed_once_an_order_is_held(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let source = Orders::new(&clock);
    assert!(matches!(
        days::set_charity(&mut conn, shop.day.id, None, &source).await,
        Err(bakedays::Error::OrdersExist)
    ));
    clock.advance(HOLD);
    release_expired_holds(&mut conn, &clock).await.unwrap();
    days::set_charity(&mut conn, shop.day.id, None, &source)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_audit_log_is_append_only(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    for sql in [
        "UPDATE audit_log SET actor = 'iemand'",
        "DELETE FROM audit_log",
        "TRUNCATE audit_log",
    ] {
        let err = sqlx::query(sql).execute(&mut *conn).await.unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_replaced_order_frees_its_hold_and_a_late_payment_is_refunded(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(1)).await;
    let held = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    orders::payment_started(&mut conn, &clock, held.id, "tr_replaced", held.total)
        .await
        .unwrap();

    assert!(
        cancel_pending(&mut conn, &clock, held.id, CancelReason::ReplacedByCustomer)
            .await
            .unwrap()
    );
    let cancelled = orders::order::get(&mut conn, held.id).await.unwrap();
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert_eq!(
        cancelled.cancel_reason,
        Some(CancelReason::ReplacedByCustomer)
    );
    let history = audit::history(&mut conn, "order", held.id.get())
        .await
        .unwrap();
    assert_eq!(history.last().unwrap().action, "cancelled");
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .expect("the hold gave its loaf back");
    assert!(
        !cancel_pending(&mut conn, &clock, held.id, CancelReason::ReplacedByCustomer)
            .await
            .unwrap(),
        "only a pending order is cancelled"
    );

    let late = PaymentReceived {
        reference: "tr_replaced".to_owned(),
        amount: held.total,
    };
    assert!(matches!(
        mark_paid(&mut conn, &clock, held.id, &late).await.unwrap(),
        PaidOutcome::RefundDue {
            reason: RefundReason::OrderCancelled,
            ..
        }
    ));
    assert_eq!(
        orders::order::get(&mut conn, held.id).await.unwrap().status,
        OrderStatus::Cancelled
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_paid_or_unknown_order_is_not_cancelled_as_pending(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let order = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let payment = PaymentReceived {
        reference: "tr_paid".to_owned(),
        amount: order.total,
    };
    mark_paid(&mut conn, &clock, order.id, &payment)
        .await
        .unwrap();
    assert!(
        !cancel_pending(
            &mut conn,
            &clock,
            order.id,
            CancelReason::ReplacedByCustomer
        )
        .await
        .unwrap()
    );
    assert_eq!(
        orders::order::get(&mut conn, order.id)
            .await
            .unwrap()
            .status,
        OrderStatus::Paid
    );
    assert!(matches!(
        cancel_pending(
            &mut conn,
            &clock,
            OrderId::new(999_999),
            CancelReason::ReplacedByCustomer
        )
        .await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_same_checkout_matches_its_order_and_any_change_does_not(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    let loaf = product(&mut conn, &DONATION_LOAF).await;
    on_range(&mut conn, &shop, bread, None).await;
    on_range(&mut conn, &shop, loaf, None).await;
    let lines = [(bread, 1, None), (loaf, 1, Some(800))];
    let order = order_as_guest(&mut conn, &clock, &shop, &lines)
        .await
        .unwrap();
    let customer = guest("klant@example.org");

    let same = |cart_lines: &[(ProductId, u32, Option<i64>)], input: &CustomerInput| {
        (cart(&shop, cart_lines), input.clone())
    };
    let cases: Vec<(&str, (orders::Cart, CustomerInput), bool)> = vec![
        ("unchanged", same(&lines, &customer), true),
        (
            "email in other case",
            same(&lines, &guest(" Klant@Example.org ")),
            true,
        ),
        (
            "qty",
            same(&[(bread, 2, None), (loaf, 1, Some(800))], &customer),
            false,
        ),
        ("line removed", same(&[(bread, 1, None)], &customer), false),
        (
            "chosen price",
            same(&[(bread, 1, None), (loaf, 1, Some(900))], &customer),
            false,
        ),
        ("email", same(&lines, &guest("ander@example.org")), false),
        ("invalid email", same(&lines, &guest("geen-mail")), false),
        (
            "name",
            same(
                &lines,
                &CustomerInput {
                    name: "Andere Klant".to_owned(),
                    ..customer.clone()
                },
            ),
            false,
        ),
        (
            "phone",
            same(
                &lines,
                &CustomerInput {
                    phone: Some("0612345678".to_owned()),
                    ..customer.clone()
                },
            ),
            false,
        ),
        (
            "marketing",
            same(
                &lines,
                &CustomerInput {
                    marketing_opt_in: true,
                    ..customer.clone()
                },
            ),
            false,
        ),
    ];
    for (case, (cart, input), expected) in cases {
        assert_eq!(
            same_as_order(&mut conn, &order, &cart, &input, shop.slot)
                .await
                .unwrap(),
            expected,
            "{case}"
        );
    }
    let other_slot = PickupSlotId::new(shop.slot.get() + 1000);
    assert!(
        !same_as_order(
            &mut conn,
            &order,
            &cart(&shop, &lines),
            &customer,
            other_slot
        )
        .await
        .unwrap()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_replacement_is_all_or_nothing(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(2)).await;
    let held = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let customer = guest("klant@example.org");

    let too_many = checkout_replacing(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 3, None)]),
        &customer,
        shop.slot,
        OrderedBy::Guest,
        held.id,
    )
    .await;
    assert!(matches!(
        too_many,
        Err(Error::CapacityExceeded { remaining: 2, .. })
    ));
    assert_eq!(
        orders::order::get(&mut conn, held.id).await.unwrap().status,
        OrderStatus::PendingPayment,
        "a refused replacement leaves the held order"
    );

    let new = checkout_replacing(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 2, None)]),
        &customer,
        shop.slot,
        OrderedBy::Guest,
        held.id,
    )
    .await
    .expect("the held loaf counts as free");
    assert_eq!(new.lines[0].qty, 2);
    let old = orders::order::get(&mut conn, held.id).await.unwrap();
    assert_eq!(old.cancel_reason, Some(CancelReason::ReplacedByCustomer));

    assert!(matches!(
        checkout_replacing(
            &mut conn,
            &clock,
            &cart(&shop, &[(bread, 1, None)]),
            &customer,
            shop.slot,
            OrderedBy::Guest,
            held.id,
        )
        .await,
        Err(Error::NotReplaceable)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_replacement_on_another_day_is_refused(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let next_week = open_day(
        &mut conn,
        &clock,
        &bakedays::NewBakeDay {
            date: "2026-11-07".parse().unwrap(),
            ..saturday()
        },
    )
    .await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    on_range(&mut conn, &next_week, bread, None).await;
    let held = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();

    assert!(matches!(
        checkout_replacing(
            &mut conn,
            &clock,
            &cart(&next_week, &[(bread, 1, None)]),
            &guest("klant@example.org"),
            next_week.slot,
            OrderedBy::Guest,
            held.id,
        )
        .await,
        Err(Error::NotReplaceable)
    ));
    assert_eq!(
        orders::order::get(&mut conn, held.id).await.unwrap().status,
        OrderStatus::PendingPayment
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_staff_replacement_names_the_staff_member_in_the_audit_log(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let held = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO staff_user (id, oidc_sub, name, role) OVERRIDING SYSTEM VALUE
         VALUES (42, 'staff-42', 'Bas', 'baker')",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    checkout_replacing(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 2, None)]),
        &guest("klant@example.org"),
        shop.slot,
        OrderedBy::Staff(domain::StaffUserId::new(42)),
        held.id,
    )
    .await
    .unwrap();
    let history = audit::history(&mut conn, "order", held.id.get())
        .await
        .unwrap();
    let last = history.last().unwrap();
    assert_eq!(last.actor, "staff:42");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_order_is_no_longer_the_same_once_the_kitchen_list_lost_its_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let lines = [(bread, 1, None)];
    let customer = guest("klant@example.org");
    let order = order_as_guest(&mut conn, &clock, &shop, &lines)
        .await
        .unwrap();
    assert!(
        same_as_order(
            &mut conn,
            &order,
            &cart(&shop, &lines),
            &customer,
            shop.slot
        )
        .await
        .unwrap()
    );

    sqlx::query("UPDATE company SET may_contain = '{nuts}'")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(
        !same_as_order(
            &mut conn,
            &order,
            &cart(&shop, &lines),
            &customer,
            shop.slot
        )
        .await
        .unwrap(),
        "the cart can no longer be priced, so it is not the same order"
    );
}
