use domain::CharityId;
use i18n::Translator;
use reports::donations;
use reports::{Granularity, Period};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{BREAD, charity, date, drop_on, paid_order, product, refund, sell, ts};

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn drops_and_charities_report_the_same_money(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let shelter = charity(&mut conn, "Dierenasiel").await;
    let drop = drop_on(&mut conn, &clock, "2026-10-31", Some(food_bank)).await;
    let normal = drop_on(&mut conn, &clock, "2026-10-24", None).await;
    let bread = product(&mut conn, &BREAD).await;
    sell(&mut conn, &drop, &[bread]).await;
    sell(&mut conn, &normal, &[bread]).await;
    paid_order(&mut conn, &clock, &drop, &[(bread, 2, None)]).await;
    let moved = paid_order(&mut conn, &clock, &drop, &[(bread, 1, None)]).await;
    let refunded = paid_order(&mut conn, &clock, &drop, &[(bread, 1, None)]).await;
    paid_order(&mut conn, &clock, &normal, &[(bread, 3, None)]).await;
    refund(&mut conn, refunded.id, 545).await;
    sqlx::query("UPDATE donation_allocation SET charity_id = $2 WHERE order_id = $1")
        .bind(moved.id.get())
        .bind(shelter.get())
        .execute(&mut *conn)
        .await
        .unwrap();

    let oct = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let drops = donations::drops(&mut conn, oct).await.unwrap();
    assert_eq!(drops.len(), 1, "the normal sale is skipped");
    let d = &drops[0];
    assert_eq!(d.orders, 3);
    assert_eq!(d.received.cents(), 2180);
    assert_eq!(d.refunds.cents(), 545);
    assert_eq!(d.to_drop_charity.cents(), 1090);
    assert_eq!(d.to_other_charities.cents(), 545);
    assert_eq!(d.saved.cents(), 0);

    let charities = donations::charities(&mut conn, oct, Granularity::Month)
        .await
        .unwrap();
    let totals: Vec<(CharityId, i64)> = charities
        .iter()
        .map(|c| (c.charity, c.in_period.cents()))
        .collect();
    assert_eq!(totals, [(food_bank, 1090), (shelter, 545)]);
    assert_eq!(charities[0].all_time.cents(), 1090);
    assert_eq!(charities[0].to_transfer.cents(), 1090);
    assert_eq!(charities[0].series.len(), 1);
    let sum: i64 = charities.iter().map(|c| c.in_period.cents()).sum();
    assert_eq!(sum, d.net().unwrap().cents());

    let t = Translator::dutch().unwrap();
    let names = |c: CharityId| {
        if c == food_bank {
            "Voedselbank".to_owned()
        } else {
            "Dierenasiel".to_owned()
        }
    };
    let text = donations::drops_csv(&drops, &names, &t).unwrap();
    assert!(
        text.ends_with("31-10-2026;Voedselbank;3;21,80;5,45;16,35;10,90;5,45;0,00\r\n"),
        "{text}"
    );
    let text = donations::charities_csv(&charities, &names, &t).unwrap();
    assert!(
        text.contains("Dierenasiel;5,45;5,45;0,00;5,45\r\n"),
        "{text}"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_public_totals_are_the_owner_reports_minus_requested_refunds(
    mut conn: PoolConnection<Postgres>,
) {
    use domain::StaffUserId;
    use reports::public::{self, Amount};

    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let drop = drop_on(&mut conn, &clock, "2026-10-31", Some(food_bank)).await;
    let bread = product(&mut conn, &BREAD).await;
    sell(&mut conn, &drop, &[bread]).await;
    let mut paid = Vec::new();
    for _ in 0..6 {
        paid.push(paid_order(&mut conn, &clock, &drop, &[(bread, 1, None)]).await);
    }
    orders::cancel_paid(&mut conn, &clock, paid[0].id, StaffUserId::new(1))
        .await
        .unwrap();

    let oct = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let owner = donations::drops(&mut conn, oct).await.unwrap();
    let net = owner[0].net().unwrap();
    assert_eq!(net.cents(), 5 * 545, "the requested refund already counts");
    let charities = donations::charities(&mut conn, oct, Granularity::Month)
        .await
        .unwrap();

    let page = public::donations(&mut conn).await.unwrap();
    assert!(
        page.drops.is_empty(),
        "a drop still taking orders is left out"
    );
    assert_eq!(page.total, Amount::Hidden);
    bakedays::days::close(&mut conn, &clock, drop.day)
        .await
        .unwrap();

    let page = public::donations(&mut conn).await.unwrap();
    assert_eq!(page.total, Amount::Shown(net));
    assert_eq!(page.drops.len(), 1);
    assert_eq!(page.drops[0].charity, food_bank);
    assert_eq!(page.drops[0].amount, Amount::Shown(net));
    assert_eq!(page.charities.len(), 1);
    assert_eq!(
        page.charities[0].amount,
        Amount::Shown(charities[0].all_time)
    );
    assert_eq!(page.unassigned, None);

    orders::cancel_paid(&mut conn, &clock, paid[1].id, StaffUserId::new(1))
        .await
        .unwrap();
    let page = public::donations(&mut conn).await.unwrap();
    assert_eq!(
        page.total,
        Amount::Hidden,
        "four orders still give something"
    );
    assert_eq!(page.drops[0].amount, Amount::Hidden);
    assert_eq!(page.charities[0].amount, Amount::Hidden);
}
