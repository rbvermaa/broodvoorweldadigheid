use domain::{BakeDayId, Money};
use reports::Period;
use reports::contribution::{self, DropCostKind, NewDropCost};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{
    BREAD, DONATION_LOAF, charity, date, drop_on, fee, paid_order, product, sell, ts,
};

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_bakery_covers_btw_fees_ingredients_and_costs(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let drop = drop_on(&mut conn, &clock, "2026-10-31", Some(food_bank)).await;
    let normal = drop_on(&mut conn, &clock, "2026-10-24", None).await;
    let bread = product(&mut conn, &BREAD).await;
    let loaf = product(&mut conn, &DONATION_LOAF).await;
    sell(&mut conn, &drop, &[bread, loaf]).await;
    sell(&mut conn, &normal, &[bread]).await;
    let a = paid_order(&mut conn, &clock, &drop, &[(bread, 2, None)]).await;
    paid_order(&mut conn, &clock, &drop, &[(loaf, 1, Some(1090))]).await;
    let n = paid_order(&mut conn, &clock, &normal, &[(bread, 1, None)]).await;
    fee(&mut conn, a.id, 39).await;
    fee(&mut conn, n.id, 39).await;
    for (kind, cents) in [(DropCostKind::Packaging, 150), (DropCostKind::Energy, 300)] {
        contribution::add_drop_cost(
            &mut conn,
            &clock,
            &NewDropCost {
                bake_day_id: drop.day,
                kind,
                amount: Money::from_cents(cents),
                note: Some(" zakken ".into()),
            },
        )
        .await
        .unwrap();
    }
    let other = NewDropCost {
        bake_day_id: normal.day,
        kind: DropCostKind::Other,
        amount: Money::from_cents(999),
        note: None,
    };
    contribution::add_drop_cost(&mut conn, &clock, &other)
        .await
        .unwrap();
    assert!(matches!(
        contribution::add_drop_cost(
            &mut conn,
            &clock,
            &NewDropCost {
                amount: Money::ZERO,
                ..other.clone()
            }
        )
        .await,
        Err(reports::Error::Invalid(_))
    ));
    assert!(matches!(
        contribution::add_drop_cost(
            &mut conn,
            &clock,
            &NewDropCost {
                bake_day_id: BakeDayId::new(999),
                ..other
            }
        )
        .await,
        Err(reports::Error::NotFound)
    ));

    let oct = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let report = contribution::report(&mut conn, oct).await.unwrap();
    assert_eq!(report.drops.len(), 1, "the normal sale is skipped");
    let c = &report.drops[0].contribution;
    assert_eq!(c.received.cents(), 2180);
    assert_eq!(c.btw.cents(), 180);
    assert_eq!(c.fees.cents(), 39);
    assert_eq!(c.unknown_fees, 1);
    assert_eq!(c.ingredients.cents(), 240);
    assert_eq!(
        c.missing_estimates.iter().collect::<Vec<_>>(),
        ["Donatiebrood"]
    );
    assert_eq!(c.drop_costs.cents(), 450);
    assert_eq!(c.covered().unwrap().cents(), 909);
    assert_eq!(c.per_euro_cents().unwrap(), Some(42));
    assert!(!c.complete());
    assert_eq!(report.charities.len(), 1);
    assert_eq!(report.charities[0].charity, Some(food_bank));
    assert_eq!(report.charities[0].contribution, *c);
    assert_eq!(report.total, *c);
}
