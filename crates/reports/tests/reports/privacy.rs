use orders::audit::Actor;
use orders::privacy::{self, Erase};
use reports::{Granularity, Period, contribution, donations, payouts, sales};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

use crate::common::{
    BREAD, COOKIES, DONATION_LOAF, charity, date, drop_on, fee, paid_order, product, refund, sell,
    ts,
};

#[derive(Debug, PartialEq, Eq)]
struct Reports {
    sales: sales::SalesReport,
    payout: payouts::Payout,
    drops: Vec<donations::DropDonations>,
    charities: Vec<donations::CharityDonations>,
    contribution: contribution::ContributionReport,
}

async fn reports(conn: &mut PgConnection, charity: domain::CharityId) -> Reports {
    let october = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    Reports {
        sales: sales::report(conn, october, Granularity::Day)
            .await
            .unwrap(),
        payout: payouts::payout(conn, charity, october).await.unwrap(),
        drops: donations::drops(conn, october).await.unwrap(),
        charities: donations::charities(conn, october, Granularity::Month)
            .await
            .unwrap(),
        contribution: contribution::report(conn, october).await.unwrap(),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn erasing_a_customer_changes_no_report(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let shop = drop_on(&mut conn, &clock, "2026-10-31", Some(food_bank)).await;
    let bread = product(&mut conn, &BREAD).await;
    let loaf = product(&mut conn, &DONATION_LOAF).await;
    let cookies = product(&mut conn, &COOKIES).await;
    sell(&mut conn, &shop, &[bread, loaf, cookies]).await;
    let erased = paid_order(
        &mut conn,
        &clock,
        &shop,
        &[(bread, 2, None), (loaf, 1, Some(1200)), (cookies, 1, None)],
    )
    .await;
    let other = paid_order(&mut conn, &clock, &shop, &[(bread, 1, None)]).await;
    refund(&mut conn, erased.id, 545).await;
    fee(&mut conn, erased.id, 32).await;
    fee(&mut conn, other.id, 29).await;
    sqlx::query("UPDATE \"order\" SET status = 'picked_up'")
        .execute(&mut *conn)
        .await
        .unwrap();
    let before = reports(&mut conn, food_bank).await;
    assert!(before.payout.received.cents() > 0);

    let customer = erased.customer_id;
    assert!(matches!(
        privacy::erase(
            &mut conn,
            customer,
            Actor::Customer(customer),
            ts("2026-11-01T10:00:00Z")
        )
        .await
        .unwrap(),
        Erase::Erased { .. }
    ));
    assert_eq!(reports(&mut conn, food_bank).await, before);
}
