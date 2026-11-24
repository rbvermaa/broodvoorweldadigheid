use domain::{Clock, Money, StaffUserId};
use orders::donations::{self, Chooser, DonationChoice, Expired};
use orders::{OrderedBy, PaidOutcome, PaymentReceived};
use reports::Period;
use reports::payouts;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{BREAD, charity, date, drop_on, product, sell, ts};

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_saved_donation_counts_for_the_charity_it_ends_up_with_minus_refunds(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let hospice = charity(&mut conn, "Hospice").await;
    let october = drop_on(&mut conn, &clock, "2026-10-31", Some(food_bank)).await;
    let bread = product(&mut conn, &BREAD).await;
    sell(&mut conn, &october, &[bread]).await;

    let mut saved = Vec::new();
    for n in 0..2 {
        let mut cart = orders::Cart::new(october.day);
        cart.set(bread, 2, None).unwrap();
        let order = orders::checkout(
            &mut conn,
            &clock,
            &cart,
            &orders::CustomerInput {
                email: format!("bewaar{n}@example.org"),
                name: "Test Klant".to_owned(),
                phone: None,
                marketing_opt_in: false,
            },
            october.slot,
            OrderedBy::Guest,
        )
        .await
        .unwrap();
        donations::choose_at_checkout(&mut conn, order.id, DonationChoice::Save)
            .await
            .unwrap();
        let outcome = orders::mark_paid(
            &mut conn,
            &clock,
            order.id,
            &PaymentReceived {
                reference: format!("tr_saved{n}"),
                amount: order.total,
            },
        )
        .await
        .unwrap();
        let PaidOutcome::Paid {
            saved: Some(link), ..
        } = outcome
        else {
            panic!("not saved: {outcome:?}");
        };
        saved.push((order, link));
    }

    let oct = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let before = payouts::payout(&mut conn, food_bank, oct).await.unwrap();
    assert_eq!(before.saved.cents(), 2 * 1090);
    assert_eq!(before.to_transfer, Money::ZERO);

    let (chosen, link) = &saved[0];
    donations::choose(
        &mut conn,
        Chooser::Link(link.token.as_str()),
        hospice,
        clock.now(),
    )
    .await
    .unwrap()
    .expect("chosen");
    orders::cancel_paid(&mut conn, &clock, chosen.id, StaffUserId::new(1))
        .await
        .unwrap();
    let hospice_oct = payouts::payout(&mut conn, hospice, oct).await.unwrap();
    assert_eq!(hospice_oct.received.cents(), 1090);
    assert_eq!(
        hospice_oct.refunds.cents(),
        1090,
        "the refund counts as soon as it is requested"
    );
    assert_eq!(hospice_oct.to_transfer, Money::ZERO);

    drop_on(&mut conn, &clock, "2027-02-06", Some(food_bank)).await;
    let (moved_order, link) = &saved[1];
    let expired = donations::move_next_expired(&mut conn, link.choose_by)
        .await
        .unwrap();
    assert!(
        matches!(&expired, Expired::Moved(moved) if moved.order == moved_order.id && moved.charity == food_bank),
        "{expired:?}"
    );
    let after = payouts::payout(&mut conn, food_bank, oct).await.unwrap();
    assert_eq!(after.saved, Money::ZERO);
    assert_eq!(
        after.received.cents(),
        1090,
        "dated by the order's own drop"
    );
    assert_eq!(after.to_transfer.cents(), 1090);

    let report = reports::donations::drops(&mut conn, oct).await.unwrap();
    let drop = &report[0];
    assert_eq!(drop.to_drop_charity.cents(), 1090);
    assert_eq!(
        drop.to_other_charities,
        Money::ZERO,
        "the hospice donation was refunded"
    );
    assert_eq!(drop.saved, Money::ZERO);
}
