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

async fn flour_bread(conn: &mut sqlx::PgConnection, bread: domain::ProductId) {
    use recipes::recipes as store;
    let flour = recipes::ingredients::create(
        conn,
        &recipes::IngredientInput {
            name: "Tarwebloem".into(),
            label_name: String::new(),
            role: recipes::IngredientRole::Flour,
            supplier_name: String::new(),
            allergens: std::collections::BTreeSet::new(),
        },
    )
    .await
    .unwrap()
    .id;
    let (recipe, version) = store::create(
        conn,
        &recipes::RecipeInput {
            name: "Alleen bloem".into(),
            kind: recipes::RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    store::save_draft(
        conn,
        version.id,
        &recipes::DraftInput {
            lines: vec![recipes::LineInput {
                pct: recipes::Pct::HUNDRED,
                part: recipes::LinePart::Ingredient(flour),
            }],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    store::link_product(conn, recipe.id, bread).await.unwrap();
    recipes::prices::add(
        conn,
        &FakeClock::new(ts("2026-10-01T10:00:00Z")),
        &recipes::prices::NewPrice {
            ingredient_id: flour,
            per_kg: Money::from_cents(100),
            valid_from: date("2026-10-01"),
            by: None,
        },
    )
    .await
    .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn ingredients_use_the_batch_cost_else_the_standard_cost(mut conn: PoolConnection<Postgres>) {
    use orders::audit::Actor;
    use production::batches;
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let drop = drop_on(&mut conn, &clock, "2026-10-31", Some(food_bank)).await;
    let bread = product(&mut conn, &BREAD).await;
    sell(&mut conn, &drop, &[bread]).await;
    paid_order(&mut conn, &clock, &drop, &[(bread, 2, None)]).await;
    let oct = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let ingredients = |report: reports::contribution::ContributionReport| report.total.ingredients;

    let estimate = contribution::report(&mut conn, oct).await.unwrap();
    assert_eq!(
        ingredients(estimate).cents(),
        240,
        "no recipe: the estimate"
    );

    flour_bread(&mut conn, bread).await;
    let standard = contribution::report(&mut conn, oct).await.unwrap();
    // 900 g of flour at €1,00 per kg, twice.
    assert_eq!(ingredients(standard).cents(), 180);

    let baker = Actor::Staff(domain::StaffUserId::new(1));
    let batch = batches::start_manual(&mut conn, drop.day, bread, 10, baker, &clock)
        .await
        .unwrap();
    batches::mark_mixed(&mut conn, batch, baker, &clock)
        .await
        .unwrap();
    batches::record_actuals(
        &mut conn,
        batch,
        &[batches::Actual {
            product_id: bread,
            pieces: 10,
            waste: 1,
        }],
        baker,
        &clock,
    )
    .await
    .unwrap();
    let actual = contribution::report(&mut conn, oct).await.unwrap();
    // €9,00 of flour over 9 good pieces.
    assert_eq!(ingredients(actual.clone()).cents(), 200);
    assert!(actual.total.missing_estimates.is_empty());
}
