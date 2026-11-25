use domain::{Money, StaffUserId};
use orders::audit::Actor;
use reports::payouts::{self, NewTransfer};
use reports::{Error, Period};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{BREAD, charity, date, drop_on, paid_order, product, refund, sell, ts};

const STAFF: Actor = Actor::Staff(StaffUserId::new(1));

fn transfer(charity: domain::CharityId, up_to: &str, cents: i64, reference: &str) -> NewTransfer {
    NewTransfer {
        charity,
        up_to: date(up_to),
        amount: Money::from_cents(cents),
        transferred_on: date("2026-11-10"),
        bank_reference: reference.into(),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn transfers_pay_paid_minus_refunds_and_correct_later_refunds(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let shelter = charity(&mut conn, "Dierenasiel").await;
    let october = drop_on(&mut conn, &clock, "2026-10-31", Some(food_bank)).await;
    let november = drop_on(&mut conn, &clock, "2026-11-07", Some(food_bank)).await;
    let other = drop_on(&mut conn, &clock, "2026-10-24", Some(shelter)).await;
    let bread = product(&mut conn, &BREAD).await;
    for shop in [&october, &november, &other] {
        sell(&mut conn, shop, &[bread]).await;
    }
    let first = paid_order(&mut conn, &clock, &october, &[(bread, 2, None)]).await;
    let second = paid_order(&mut conn, &clock, &october, &[(bread, 2, None)]).await;
    paid_order(&mut conn, &clock, &october, &[(bread, 2, None)]).await;
    paid_order(&mut conn, &clock, &other, &[(bread, 1, None)]).await;
    refund(&mut conn, second.id, 545).await;

    let oct = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let payout = payouts::payout(&mut conn, food_bank, oct).await.unwrap();
    assert_eq!(payout.received.cents(), 3270);
    assert_eq!(payout.refunds.cents(), 545);
    assert_eq!(payout.net().unwrap().cents(), 2725);
    assert_eq!(payout.to_transfer.cents(), 2725);
    assert_eq!(payout.transferred, Money::ZERO);

    assert!(matches!(
        payouts::record_transfer(&mut conn, &clock, STAFF, &transfer(food_bank, "2026-10-31", 2700, "NL-1")).await,
        Err(Error::AmountMismatch { due }) if due.cents() == 2725
    ));
    assert!(matches!(
        payouts::record_transfer(
            &mut conn,
            &clock,
            STAFF,
            &transfer(food_bank, "2026-10-31", 2725, " ")
        )
        .await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        payouts::record_transfer(
            &mut conn,
            &clock,
            STAFF,
            &transfer(domain::CharityId::new(999), "2026-10-31", 1, "x")
        )
        .await,
        Err(Error::NotFound)
    ));
    let done = payouts::record_transfer(
        &mut conn,
        &clock,
        STAFF,
        &transfer(food_bank, "2026-10-31", 2725, "NL-1"),
    )
    .await
    .unwrap();
    assert_eq!(done.amount.cents(), 2725);
    assert!(matches!(
        payouts::record_transfer(&mut conn, &clock, STAFF, &transfer(food_bank, "2026-10-31", 0, "NL-2")).await,
        Err(Error::NothingToTransfer { due }) if due == Money::ZERO
    ));
    let after = payouts::payout(&mut conn, food_bank, oct).await.unwrap();
    assert_eq!(after.transferred.cents(), 2725);
    assert_eq!(after.to_transfer, Money::ZERO);
    let statuses: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM donation_allocation WHERE charity_id = $1 ORDER BY id",
    )
    .bind(food_bank.get())
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(statuses, ["transferred"; 3]);
    let locked = sqlx::query("UPDATE donation_allocation SET amount_cents = 1 WHERE order_id = $1")
        .bind(first.id.get())
        .execute(&mut *conn)
        .await;
    assert!(locked.is_err());

    // A refund after the transfer is taken off the next one.
    refund(&mut conn, first.id, 200).await;
    clock.set(ts("2026-11-02T10:00:00Z"));
    paid_order(&mut conn, &clock, &november, &[(bread, 2, None)]).await;
    let saved = paid_order(&mut conn, &clock, &november, &[(bread, 1, None)]).await;
    sqlx::query(
        "UPDATE donation_allocation SET status = 'saved', charity_id = NULL,
            choose_token_hash = sha256('tok'),
            choose_by = '2027-02-01T00:00:00Z' WHERE order_id = $1",
    )
    .bind(saved.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    let nov = Period::new(date("2026-11-01"), date("2026-11-30")).unwrap();
    let next = payouts::payout(&mut conn, food_bank, nov).await.unwrap();
    assert_eq!(next.received.cents(), 1090);
    assert_eq!(next.saved.cents(), 545);
    assert_eq!(next.corrections.cents(), 200);
    assert_eq!(next.to_transfer.cents(), 890);
    payouts::record_transfer(
        &mut conn,
        &clock,
        STAFF,
        &transfer(food_bank, "2026-11-30", 890, "NL-2"),
    )
    .await
    .unwrap();
    let transfers = payouts::transfers(&mut conn, food_bank).await.unwrap();
    let amounts: Vec<i64> = transfers.iter().map(|t| t.amount.cents()).collect();
    assert_eq!(amounts, [2725, 890]);
    assert_eq!(transfers[0].bank_reference, "NL-1");

    // The other charity was never touched.
    let shelter_payout = payouts::payout(&mut conn, shelter, oct).await.unwrap();
    assert_eq!(shelter_payout.to_transfer.cents(), 545);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn cancelled_orders_count_as_refunded_as_soon_as_the_refund_is_requested(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let october = drop_on(&mut conn, &clock, "2026-10-31", Some(food_bank)).await;
    let bread = product(&mut conn, &BREAD).await;
    sell(&mut conn, &october, &[bread]).await;
    let requested = paid_order(&mut conn, &clock, &october, &[(bread, 2, None)]).await;
    let failed = paid_order(&mut conn, &clock, &october, &[(bread, 1, None)]).await;
    let reported = paid_order(&mut conn, &clock, &october, &[(bread, 1, None)]).await;
    paid_order(&mut conn, &clock, &october, &[(bread, 2, None)]).await;
    let baker = StaffUserId::new(1);
    for order in [&requested, &failed, &reported] {
        orders::cancel_paid(&mut conn, &clock, order.id, baker)
            .await
            .unwrap();
    }
    sqlx::query(
        "UPDATE refund SET status = 'failed', attempts = 1, last_error = 'refused'
        WHERE payment_id IN (SELECT id FROM payment WHERE order_id = $1)",
    )
    .bind(failed.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    // Mollie reports the same refund on the payment: it is not counted twice.
    refund(&mut conn, reported.id, 545).await;

    let oct = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let payout = payouts::payout(&mut conn, food_bank, oct).await.unwrap();
    assert_eq!(payout.received.cents(), 6 * 545);
    assert_eq!(payout.refunds.cents(), 4 * 545);
    assert_eq!(payout.to_transfer.cents(), 2 * 545);

    let sold = reports::sales::report(&mut conn, oct, reports::Granularity::Month)
        .await
        .unwrap();
    assert_eq!(
        sold.total.rate(domain::VatRate::Nine).gross().cents(),
        2 * 545,
        "a cancelled paid order is a sale with its refund"
    );
    assert_eq!(sold.total.rate(domain::VatRate::Nine).vat().cents(), 90);
}
