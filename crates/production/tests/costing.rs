//! Actual batch cost, variance and unit costs against the database (F16).
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use bakedays::{NewBakeDay, days};
use domain::{BakeDayId, BatchId, IngredientId, Money, PickupLocationId, ProductId, StaffUserId};
use jiff::civil::{Date, date};
use orders::audit::Actor;
use production::batches::{self, Actual, Adjustment};
use production::costing::{self, Basis, UnitCost};
use recipes::cost::CostError;
use recipes::prices::{self, NewPrice};
use recipes::recipes::{self as store};
use recipes::{
    DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput, RecipeKind,
    ingredients,
};
use sqlx::{PgConnection, PgPool};
use testkit::FakeClock;

const DE_PROEF: PickupLocationId = PickupLocationId::new(1);
const BAKER: Actor = Actor::Staff(StaffUserId::new(3));

fn clock() -> FakeClock {
    FakeClock::new("2026-12-01T10:00:00Z".parse().unwrap())
}

fn cents(c: i64) -> Money {
    Money::from_cents(c)
}

struct Kitchen {
    day: BakeDayId,
    bread: ProductId,
    rolls: ProductId,
    cookies: ProductId,
    flour: IngredientId,
    salt: IngredientId,
}

async fn ingredient(conn: &mut PgConnection, name: &str, role: IngredientRole) -> IngredientId {
    ingredients::create(
        conn,
        &IngredientInput {
            name: name.to_owned(),
            label_name: String::new(),
            role,
            supplier_name: String::new(),
            allergens: BTreeSet::new(),
        },
    )
    .await
    .unwrap()
    .id
}

async fn product(conn: &mut PgConnection, name: &str, dough_g: i32) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online)
         VALUES ($1, 450, 9, 800, $2, true) RETURNING id",
    )
    .bind(name)
    .bind(dough_g)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

async fn price(conn: &mut PgConnection, id: IngredientId, c: i64, from: Date) {
    prices::add(
        conn,
        &clock(),
        &NewPrice {
            ingredient_id: id,
            price: recipes::cost::UnitPrice::per_kg(cents(c)),
            valid_from: from,
            by: None,
        },
    )
    .await
    .unwrap();
}

/// Saturday 5 December 2026; bread (900 g) and rolls (90 g) from a 100/75/2 recipe;
/// cookies without a recipe. Flour costs €0,90 from 1 December and €1,00 from 5 December.
async fn kitchen(conn: &mut PgConnection) -> Kitchen {
    let day = days::create(
        conn,
        &NewBakeDay {
            date: date(2026, 12, 5),
            location_id: DE_PROEF,
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .unwrap()
    .id;
    let flour = ingredient(conn, "Tarwebloem", IngredientRole::Flour).await;
    let water = ingredient(conn, "Water", IngredientRole::Water).await;
    let salt = ingredient(conn, "Zeezout", IngredientRole::Other).await;
    let (recipe, version) = store::create(
        conn,
        &RecipeInput {
            name: "Desembrood".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    let line = |percent: u32, id| LineInput {
        pct: Pct::from_thousandths(percent * 1000),
        part: LinePart::Ingredient(id),
    };
    store::save_draft(
        conn,
        version.id,
        &DraftInput {
            lines: vec![line(100, flour), line(75, water), line(2, salt)],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    let bread = product(conn, "Desem wit", 900).await;
    let rolls = product(conn, "Bolletjes", 90).await;
    let cookies = product(conn, "Koekjes", 50).await;
    store::link_product(conn, recipe.id, bread).await.unwrap();
    store::link_product(conn, recipe.id, rolls).await.unwrap();
    price(conn, flour, 90, date(2026, 12, 1)).await;
    price(conn, flour, 100, date(2026, 12, 5)).await;
    price(conn, water, 0, date(2026, 12, 1)).await;
    price(conn, salt, 120, date(2026, 12, 1)).await;
    Kitchen {
        day,
        bread,
        rolls,
        cookies,
        flour,
        salt,
    }
}

/// 20 breads with water at 70% and salt at 2,2%, mixed.
async fn adjusted_batch(conn: &mut PgConnection, k: &Kitchen) -> BatchId {
    let id = batches::start_manual(conn, k.day, k.bread, 20, BAKER, &clock())
        .await
        .unwrap();
    let batch = batches::get(conn, id).await.unwrap();
    batches::adjust(
        conn,
        id,
        &Adjustment {
            lines: vec![
                (batch.lines[1].id, Pct::from_thousandths(70_000)),
                (batch.lines[2].id, Pct::from_thousandths(2_200)),
            ],
            pieces: Vec::new(),
            reason: None,
        },
        BAKER,
        &clock(),
    )
    .await
    .unwrap();
    batches::mark_mixed(conn, id, BAKER, &clock())
        .await
        .unwrap();
    id
}

async fn bake(conn: &mut PgConnection, id: BatchId, product: ProductId, pieces: u32, waste: u32) {
    batches::record_actuals(
        conn,
        id,
        &[Actual {
            product_id: product,
            pieces,
            waste,
        }],
        BAKER,
        &clock(),
    )
    .await
    .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_batch_costs_its_lines_at_the_bake_date_prices(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = adjusted_batch(&mut conn, &k).await;

    let before = costing::batch_cost(&mut conn, id, date(2026, 12, 1))
        .await
        .unwrap();
    assert_eq!(before.bake_date, date(2026, 12, 5));
    // 10 453 g flour at €1,00 and 230 g salt at €1,20.
    assert_eq!(before.actual(), Some(cents(1073)));
    assert_eq!(before.good_g, None);
    assert_eq!(before.variance, None);
    assert_eq!(before.per_piece, [(k.bread, None)]);

    bake(&mut conn, id, k.bread, 19, 1).await;
    let after = costing::batch_cost(&mut conn, id, date(2026, 12, 1))
        .await
        .unwrap();
    assert_eq!(after.good_g, Some(16_200));
    // €10,729 over 18 good pieces.
    assert_eq!(after.per_piece, [(k.bread, Some(cents(60)))]);
    let v = after.variance.unwrap().unwrap();
    assert_eq!(v.standard, cents(846));
    assert_eq!(v.yield_loss, cents(94));
    assert_eq!(v.recipe, cents(28));
    assert_eq!(v.price, cents(105));
    assert_eq!(v.actual, cents(1073));
    assert_eq!(v.total(), cents(227));

    // At the bake date's prices as standard, nothing is left for price.
    let same = costing::batch_cost(&mut conn, id, date(2026, 12, 5))
        .await
        .unwrap();
    assert_eq!(same.variance.unwrap().unwrap().price, Money::ZERO);

    // A later price does not change an old batch.
    price(&mut conn, k.flour, 150, date(2026, 12, 20)).await;
    let later = costing::batch_cost(&mut conn, id, date(2026, 12, 1))
        .await
        .unwrap();
    assert_eq!(later.actual(), Some(cents(1073)));

    let baked = costing::baked_since(&mut conn, date(2026, 12, 1))
        .await
        .unwrap();
    assert_eq!(baked.len(), 1);
    assert_eq!(baked[0].id, id);
    assert_eq!(baked[0].code.to_string(), "261205-1");
    assert_eq!(baked[0].bake_date, date(2026, 12, 5));
    assert_eq!(baked[0].recipe_name, "Desembrood");
    assert_eq!(baked[0].version, 1);
    assert!(
        costing::baked_since(&mut conn, date(2026, 12, 6))
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_missing_price_stops_the_variance(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = adjusted_batch(&mut conn, &k).await;
    bake(&mut conn, id, k.bread, 20, 0).await;
    // Salt has no price yet on 30 November, the standard date.
    let cost = costing::batch_cost(&mut conn, id, date(2026, 11, 30))
        .await
        .unwrap();
    assert_eq!(cost.actual(), Some(cents(1073)));
    let Some(Err(CostError::MissingPrices(missing))) = cost.variance else {
        panic!("expected missing prices, got {:?}", cost.variance);
    };
    let ids: Vec<IngredientId> = missing.iter().map(|i| i.id).collect();
    assert!(ids.contains(&k.flour) && ids.contains(&k.salt), "{ids:?}");
    assert!(matches!(
        costing::batch_cost(&mut conn, BatchId::new(999), date(2026, 12, 1)).await,
        Err(production::Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn unit_costs_come_from_batches_else_the_standard(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let all = [k.bread, k.rolls, k.cookies];

    let standard = costing::unit_costs(&mut conn, k.day, &all).await.unwrap();
    // 900 g: 508,5 g flour at €1,00 and 10,2 g salt at €1,20 on the bake date.
    assert_eq!(
        standard.get(&k.bread),
        Some(&UnitCost {
            cost: Some(cents(52)),
            basis: Basis::Standard,
        })
    );
    assert_eq!(standard[&k.rolls].basis, Basis::Standard);
    assert_eq!(standard.get(&k.cookies), None, "no recipe, no cost");

    let first = adjusted_batch(&mut conn, &k).await;
    let unbaked = costing::unit_costs(&mut conn, k.day, &all).await.unwrap();
    assert_eq!(unbaked[&k.bread].basis, Basis::Standard);

    bake(&mut conn, first, k.bread, 19, 1).await;
    // A second batch, all waste, still counts: €10,729 + €1,041 over 18 good pieces.
    let second = batches::start_manual(&mut conn, k.day, k.bread, 2, BAKER, &clock())
        .await
        .unwrap();
    batches::mark_mixed(&mut conn, second, BAKER, &clock())
        .await
        .unwrap();
    bake(&mut conn, second, k.bread, 2, 2).await;
    let baked = costing::unit_costs(&mut conn, k.day, &all).await.unwrap();
    assert_eq!(
        baked.get(&k.bread),
        Some(&UnitCost {
            cost: Some(cents(65)),
            basis: Basis::Batches,
        })
    );
    assert_eq!(baked[&k.rolls].basis, Basis::Standard);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_batch_without_prices_leaves_the_cost_unknown(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let seeds = ingredient(&mut conn, "Zaden", IngredientRole::Other).await;
    let id = batches::start_manual(&mut conn, k.day, k.bread, 10, BAKER, &clock())
        .await
        .unwrap();
    batches::add_line(
        &mut conn,
        id,
        seeds,
        Pct::from_thousandths(5000),
        None,
        BAKER,
        &clock(),
    )
    .await
    .unwrap();
    batches::mark_mixed(&mut conn, id, BAKER, &clock())
        .await
        .unwrap();
    bake(&mut conn, id, k.bread, 10, 0).await;
    let costs = costing::unit_costs(&mut conn, k.day, &[k.bread])
        .await
        .unwrap();
    assert_eq!(
        costs[&k.bread],
        UnitCost {
            cost: None,
            basis: Basis::Batches,
        },
        "seeds have no price"
    );
    let cost = costing::batch_cost(&mut conn, id, date(2026, 12, 5))
        .await
        .unwrap();
    assert_eq!(cost.actual(), None);
    assert_eq!(cost.per_piece, [(k.bread, None)]);
}
