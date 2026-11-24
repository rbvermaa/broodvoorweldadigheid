use std::sync::Arc;

use domain::{
    Money, NewPayment, OrderId, PaymentProvider, ProviderPaymentId, RefundId, StaffUserId,
};
use orders::audit;
use orders::{
    CancelReason, Error, Order, OrderStatus, PaidOutcome, PaymentReceived, RefundState, cancel_day,
    cancel_paid, day_preview, issue, mark_paid, of_order, open_of_day,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, PgPool, Postgres};
use testkit::{FakeClock, FakePaymentProvider};

use crate::common::{
    BREAD, OpenDay, on_range, open_day, order_as_guest, product, saturday, wednesday,
};

const BAS: StaffUserId = StaffUserId::new(7);

/// A paid order of `qty` loaves, paid through the fake provider.
async fn paid(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &OpenDay,
    bread: domain::ProductId,
    mollie: &FakePaymentProvider,
    qty: u32,
) -> (Order, ProviderPaymentId) {
    let order = order_as_guest(conn, clock, shop, &[(bread, qty, None)])
        .await
        .unwrap();
    let created = mollie
        .create_payment(&NewPayment {
            amount: order.total,
            description: format!("Bestelling {}", order.number),
            redirect_url: "https://shop.test/bestelling".into(),
            webhook_url: "https://shop.test/webhooks/mollie".into(),
            order: order.id,
            methods: Vec::new(),
            idempotency_key: None,
        })
        .await
        .unwrap();
    mollie.pay(&created.id);
    let outcome = mark_paid(
        conn,
        clock,
        order.id,
        &PaymentReceived {
            reference: created.id.to_string(),
            amount: order.total,
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, PaidOutcome::Paid { .. }), "{outcome:?}");
    (
        orders::order::get(conn, order.id).await.unwrap(),
        created.id,
    )
}

async fn actions(conn: &mut PgConnection, id: OrderId) -> Vec<String> {
    audit::history(conn, "order", id.get())
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect()
}

async fn send(
    conn: &mut PgConnection,
    mollie: &FakePaymentProvider,
    clock: &FakeClock,
    id: RefundId,
) -> orders::Refund {
    issue(conn, mollie, clock, id, "Terugbetaling")
        .await
        .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn cancelling_a_paid_order_requests_one_full_refund(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let mollie = FakePaymentProvider::new();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let (order, payment) = paid(&mut conn, &clock, &shop, bread, &mollie, 2).await;

    let cancelled = cancel_paid(&mut conn, &clock, order.id, BAS).await.unwrap();
    assert_eq!(cancelled.order.status, OrderStatus::Cancelled);
    assert_eq!(cancelled.order.cancel_reason, Some(CancelReason::Staff));
    assert_eq!(cancelled.refunded(), Some(Money::from_cents(900)));
    assert_eq!(cancelled.refunds.len(), 1);
    let refund = &cancelled.refunds[0];
    assert_eq!(refund.state, RefundState::Requested);
    assert_eq!(refund.attempts, 0);
    assert_eq!(refund.requested_by, "staff:7");
    assert_eq!(refund.provider_payment_id, payment.to_string());
    assert!(
        mollie.refunds(&payment).is_empty(),
        "nothing is sent before the commit"
    );
    assert_eq!(
        actions(&mut conn, order.id).await.last().unwrap(),
        "cancelled"
    );

    assert!(matches!(
        cancel_paid(&mut conn, &clock, order.id, BAS).await,
        Err(Error::WrongStatus(OrderStatus::Cancelled))
    ));
    assert_eq!(of_order(&mut conn, order.id).await.unwrap().len(), 1);

    let done = send(&mut conn, &mollie, &clock, refund.id).await;
    assert_eq!(done.state, RefundState::Done);
    assert_eq!(done.provider_refund_id.as_deref(), Some("re_fake1"));
    assert_eq!(done.attempts, 1);
    assert_eq!(mollie.refunds(&payment), [Money::from_cents(900)]);
    let again = send(&mut conn, &mollie, &clock, refund.id).await;
    assert_eq!(again, done);
    assert_eq!(mollie.refunds(&payment).len(), 1);
    assert_eq!(
        actions(&mut conn, order.id).await.last().unwrap(),
        "refund_done"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_failed_refund_is_retried_in_place(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let mollie = FakePaymentProvider::new();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let (order, payment) = paid(&mut conn, &clock, &shop, bread, &mollie, 1).await;
    let refund = cancel_paid(&mut conn, &clock, order.id, BAS)
        .await
        .unwrap()
        .refunds[0]
        .id;

    mollie.refuse_refunds(true);
    let failed = send(&mut conn, &mollie, &clock, refund).await;
    assert_eq!(failed.state, RefundState::Failed);
    assert!(failed.last_error.as_deref().unwrap().contains("refused"));
    mollie.set_down(true);
    let still = send(&mut conn, &mollie, &clock, refund).await;
    assert_eq!(still.state, RefundState::Failed);
    assert_eq!(still.attempts, 2);
    assert_eq!(open_of_day(&mut conn, shop.day.id).await.unwrap().len(), 1);

    mollie.set_down(false);
    mollie.refuse_refunds(false);
    let done = send(&mut conn, &mollie, &clock, refund).await;
    assert_eq!(done.state, RefundState::Done);
    assert_eq!(done.attempts, 3);
    assert_eq!(done.last_error, None);
    assert_eq!(mollie.refunds(&payment), [Money::from_cents(450)]);
    assert!(
        open_of_day(&mut conn, shop.day.id)
            .await
            .unwrap()
            .is_empty()
    );
    let log = actions(&mut conn, order.id).await;
    assert_eq!(log.iter().filter(|a| *a == "refund_failed").count(), 2);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_refund_whose_answer_was_lost_is_not_paid_twice(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let mollie = FakePaymentProvider::new();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let (order, payment) = paid(&mut conn, &clock, &shop, bread, &mollie, 1).await;
    let refund = cancel_paid(&mut conn, &clock, order.id, BAS)
        .await
        .unwrap()
        .refunds[0]
        .id;

    mollie.lose_next_refund_answer();
    let lost = send(&mut conn, &mollie, &clock, refund).await;
    assert_eq!(lost.state, RefundState::Failed);
    assert_eq!(mollie.refunds(&payment).len(), 1);

    let found = send(&mut conn, &mollie, &clock, refund).await;
    assert_eq!(found.state, RefundState::Done);
    assert_eq!(mollie.refunds(&payment), [Money::from_cents(450)]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn two_sends_at_once_refund_once(pool: PgPool) {
    let clock = Arc::new(wednesday());
    let mollie = Arc::new(FakePaymentProvider::new());
    let mut conn = pool.acquire().await.unwrap();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let (order, payment) = paid(&mut conn, &clock, &shop, bread, &mollie, 1).await;
    let refund = cancel_paid(&mut conn, clock.as_ref(), order.id, BAS)
        .await
        .unwrap()
        .refunds[0]
        .id;
    let run = |pool: PgPool, mollie: Arc<FakePaymentProvider>, clock: Arc<FakeClock>| async move {
        let mut conn = pool.acquire().await.unwrap();
        issue(
            &mut conn,
            mollie.as_ref(),
            clock.as_ref(),
            refund,
            "Terugbetaling",
        )
        .await
        .unwrap()
    };
    let (a, b) = tokio::join!(
        run(pool.clone(), mollie.clone(), clock.clone()),
        run(pool.clone(), mollie.clone(), clock.clone())
    );
    assert_eq!(a.state, RefundState::Done);
    assert_eq!(b.state, RefundState::Done);
    assert_eq!(mollie.refunds(&payment).len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_paid_or_confirmed_orders_on_open_days_are_cancelled_with_a_refund(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let mollie = FakePaymentProvider::new();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let pending = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    assert!(matches!(
        cancel_paid(&mut conn, &clock, pending.id, BAS).await,
        Err(Error::WrongStatus(OrderStatus::PendingPayment))
    ));
    assert!(matches!(
        cancel_paid(&mut conn, &clock, OrderId::new(9999), BAS).await,
        Err(Error::NotFound)
    ));

    let on_account = on_account_order(&mut conn, &clock, &shop, bread, false).await;
    let invoiced = on_account_order(&mut conn, &clock, &shop, bread, true).await;
    assert!(matches!(
        cancel_paid(&mut conn, &clock, invoiced, BAS).await,
        Err(Error::Invoiced)
    ));
    let cancelled = cancel_paid(&mut conn, &clock, on_account, BAS)
        .await
        .unwrap();
    assert_eq!(cancelled.order.status, OrderStatus::Cancelled);
    assert!(cancelled.refunds.is_empty());
    assert_eq!(cancelled.refunded(), None);

    let (late, _) = paid(&mut conn, &clock, &shop, bread, &mollie, 1).await;
    sqlx::query("UPDATE bake_day SET status = 'done' WHERE id = $1")
        .bind(shop.day.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(matches!(
        cancel_paid(&mut conn, &clock, late.id, BAS).await,
        Err(Error::DayFinal)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn refunds_never_exceed_an_accepted_payment(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let mollie = FakePaymentProvider::new();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let (order, _) = paid(&mut conn, &clock, &shop, bread, &mollie, 1).await;
    let payment: i64 = sqlx::query_scalar("SELECT id FROM payment WHERE order_id = $1")
        .bind(order.id.get())
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    let insert = |cents: i64| {
        sqlx::query(
            "INSERT INTO refund (payment_id, amount_cents, status, requested_by, requested_at,
                updated_at)
            VALUES ($1, $2, 'requested', 'staff:7', now(), now())",
        )
        .bind(payment)
        .bind(cents)
    };
    assert!(insert(451).execute(&mut *conn).await.is_err());
    insert(400).execute(&mut *conn).await.unwrap();
    assert!(insert(51).execute(&mut *conn).await.is_err());
    insert(50).execute(&mut *conn).await.unwrap();
    assert!(
        sqlx::query("DELETE FROM refund")
            .execute(&mut *conn)
            .await
            .is_err()
    );
    sqlx::query("UPDATE payment SET refund_due = true, refund_reason = 'paid_twice'")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(insert(1).execute(&mut *conn).await.is_err());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_sick_day_cancels_every_order_and_is_safe_to_run_again(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let mollie = FakePaymentProvider::new();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let (first, first_payment) = paid(&mut conn, &clock, &shop, bread, &mollie, 1).await;
    let (second, second_payment) = paid(&mut conn, &clock, &shop, bread, &mollie, 2).await;
    let pending = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let (picked, _) = paid(&mut conn, &clock, &shop, bread, &mollie, 1).await;
    orders::manage::mark_picked_up(&mut conn, &clock, picked.id, BAS)
        .await
        .unwrap();
    let invoiced = on_account_order(&mut conn, &clock, &shop, bread, true).await;

    let preview = day_preview(&mut conn, shop.day.id).await.unwrap();
    assert_eq!((preview.orders, preview.refunds), (3, 2));
    assert_eq!(preview.refund_total, Money::from_cents(1350));
    assert_eq!(preview.invoiced.len(), 1);
    assert!(matches!(
        cancel_day(&mut conn, &clock, shop.day.id, "  ", BAS).await,
        Err(Error::Invalid(_))
    ));

    let result = cancel_day(&mut conn, &clock, shop.day.id, "Ik ben ziek.", BAS)
        .await
        .unwrap();
    assert_eq!(result.message, "Ik ben ziek.");
    assert_eq!(
        result.orders,
        [
            (first.id, Some(Money::from_cents(450))),
            (second.id, Some(Money::from_cents(900))),
            (pending.id, None),
        ]
    );
    assert_eq!(result.open_refunds.len(), 2);
    let invoiced = orders::order::get(&mut conn, invoiced).await.unwrap();
    assert_eq!(result.invoiced, [invoiced.number]);
    assert_eq!(invoiced.status, OrderStatus::Confirmed);
    for id in [first.id, second.id, pending.id] {
        let order = orders::order::get(&mut conn, id).await.unwrap();
        assert_eq!(order.status, OrderStatus::Cancelled);
        assert_eq!(order.cancel_reason, Some(CancelReason::DayCancelled));
    }
    assert_eq!(
        orders::order::get(&mut conn, picked.id)
            .await
            .unwrap()
            .status,
        OrderStatus::PickedUp
    );
    let day = bakedays::days::get(&mut conn, shop.day.id).await.unwrap();
    assert_eq!(day.status, bakedays::BakeDayStatus::Cancelled);

    mollie.refuse_refunds(true);
    let failed = send(&mut conn, &mollie, &clock, result.open_refunds[0].id).await;
    assert_eq!(failed.state, RefundState::Failed);
    mollie.refuse_refunds(false);
    send(&mut conn, &mollie, &clock, result.open_refunds[1].id).await;

    let again = cancel_day(&mut conn, &clock, shop.day.id, "Ander bericht", BAS)
        .await
        .unwrap();
    assert_eq!(again.message, "Ik ben ziek.");
    assert_eq!(again.orders, result.orders);
    assert_eq!(again.open_refunds.len(), 1);
    assert_eq!(again.open_refunds[0].id, failed.id);
    for refund in &again.open_refunds {
        send(&mut conn, &mollie, &clock, refund.id).await;
    }
    assert_eq!(mollie.refunds(&first_payment), [Money::from_cents(450)]);
    assert_eq!(mollie.refunds(&second_payment), [Money::from_cents(900)]);
    assert!(
        open_of_day(&mut conn, shop.day.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(of_order(&mut conn, first.id).await.unwrap().len(), 1);
    let log = actions(&mut conn, first.id).await;
    assert_eq!(log.iter().filter(|a| *a == "cancelled").count(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_done_day_cannot_be_cancelled(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    sqlx::query("UPDATE bake_day SET status = 'done' WHERE id = $1")
        .bind(shop.day.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(matches!(
        cancel_day(&mut conn, &clock, shop.day.id, "Ziek", BAS).await,
        Err(Error::DayFinal)
    ));
    assert!(matches!(
        cancel_day(&mut conn, &clock, domain::BakeDayId::new(9999), "Ziek", BAS).await,
        Err(Error::NotFound)
    ));
}

/// A confirmed on-account order, invoiced when `invoiced`.
async fn on_account_order(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &OpenDay,
    bread: domain::ProductId,
    invoiced: bool,
) -> OrderId {
    let order = order_as_guest(conn, clock, shop, &[(bread, 1, None)])
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE "order" SET status = 'confirmed', payment_mode = 'on_account' WHERE id = $1"#,
    )
    .bind(order.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    if invoiced {
        sqlx::query(
            r#"WITH invoice AS (
                INSERT INTO manual_invoice (customer_id, invoice_number, invoice_date,
                    period_from, period_to, total_cents, recorded_at, recorded_by)
                VALUES ($2, 'F-' || $1, '2026-10-28', '2026-10-01', '2026-10-31', 450, now(),
                    'staff:1')
                RETURNING id)
            UPDATE "order" SET manual_invoice_id = (SELECT id FROM invoice) WHERE id = $1"#,
        )
        .bind(order.id.get())
        .bind(order.customer_id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    order.id
}
