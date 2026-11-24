use domain::VatRate;
use i18n::Translator;
use reports::{Granularity, Period, sales};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{
    BREAD, COOKIES, DONATION_LOAF, charity, customer, date, drop_on, on_account_order, paid_order,
    product, refund, sell, ts,
};

fn year() -> Period {
    Period::new(date("2026-01-01"), date("2026-12-31")).unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sales_per_rate_per_amsterdam_day_and_quarter(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-09-30T21:30:00Z"));
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let charity_drop = drop_on(&mut conn, &clock, "2026-10-03", Some(food_bank)).await;
    let normal_sale = drop_on(&mut conn, &clock, "2026-10-31", None).await;
    let bread = product(&mut conn, &BREAD).await;
    let cookies = product(&mut conn, &COOKIES).await;
    let loaf = product(&mut conn, &DONATION_LOAF).await;
    sell(&mut conn, &charity_drop, &[bread, cookies, loaf]).await;
    sell(&mut conn, &normal_sale, &[bread]).await;

    // 23:30 on 30 September in Amsterdam: Q3.
    paid_order(&mut conn, &clock, &charity_drop, &[(bread, 2, None)]).await;
    // 00:30 on 1 October in Amsterdam: Q4.
    clock.set(ts("2026-09-30T22:30:00Z"));
    let mixed = paid_order(
        &mut conn,
        &clock,
        &charity_drop,
        &[(cookies, 1, None), (loaf, 2, Some(900))],
    )
    .await;
    // A drop without charity counts as any other sale.
    clock.set(ts("2026-10-25T23:30:00Z"));
    let refunded = paid_order(&mut conn, &clock, &normal_sale, &[(bread, 1, None)]).await;
    refund(&mut conn, refunded.id, 545).await;
    // A partial refund of the mixed order is split by estimate.
    refund(&mut conn, mixed.id, 100).await;
    // On account: counted when created.
    let business = customer(&mut conn, "zaak@example.org").await;
    on_account_order(
        &mut conn,
        business,
        &normal_sale,
        &[(bread, 4, 545, 9)],
        "2026-10-26T09:00:00Z",
    )
    .await;
    // A checkout never paid does not count.
    clock.set(ts("2026-10-27T10:00:00Z"));
    orders::checkout(
        &mut conn,
        &clock,
        &{
            let mut cart = orders::Cart::new(normal_sale.day);
            cart.set(bread, 1, None).unwrap();
            cart
        },
        &orders::CustomerInput {
            email: "open@example.org".into(),
            name: "Open".into(),
            phone: None,
            marketing_opt_in: false,
        },
        normal_sale.slot,
        orders::OrderedBy::Guest,
    )
    .await
    .unwrap();

    let quarters = sales::report(&mut conn, year(), Granularity::Quarter)
        .await
        .unwrap();
    assert_eq!(quarters.rows.len(), 2);
    let q3 = quarters.rows[0].totals;
    assert_eq!(quarters.rows[0].period.from(), date("2026-07-01"));
    assert_eq!(q3.rate(VatRate::Nine).gross().cents(), 1090);
    assert_eq!(q3.rate(VatRate::Nine).vat().cents(), 90);
    let q4 = quarters.rows[1].totals;
    // 1800 + 2180 (on account) - 100 * 1800 / 2163 rounded = 83
    assert_eq!(q4.rate(VatRate::Nine).gross().cents(), 1800 + 2180 - 83);
    assert_eq!(q4.rate(VatRate::TwentyOne).gross().cents(), 363 - 17);
    assert_eq!(quarters.estimated_refunds, vec![mixed.number]);
    assert_eq!(quarters.pay_what_you_want.len(), 1);
    assert_eq!(quarters.pay_what_you_want[0].average_paid().cents(), 900);
    assert_eq!(
        quarters.pay_what_you_want[0].average_suggested().cents(),
        600
    );

    let days = sales::report(&mut conn, year(), Granularity::Day)
        .await
        .unwrap();
    let froms: Vec<_> = days.rows.iter().map(|r| r.period.from()).collect();
    assert_eq!(
        froms,
        [date("2026-09-30"), date("2026-10-01"), date("2026-10-26")]
    );
    assert_eq!(days.total, quarters.total);

    let october = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let month = sales::report(&mut conn, october, Granularity::Month)
        .await
        .unwrap();
    assert_eq!(month.total, q4);

    let t = Translator::dutch().unwrap();
    let text = sales::csv(&month, &t).unwrap();
    assert!(
        text.contains("01-10-2026;31-10-2026;21%;3,46;0,60;2,86\r\n"),
        "{text}"
    );
}
