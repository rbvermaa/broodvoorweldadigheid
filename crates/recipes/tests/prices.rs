#![forbid(unsafe_code)]
//! Ingredient prices with a valid-from date and the standard cost of products (F16).

use std::collections::BTreeSet;

use domain::{IngredientId, Money, ProductId};
use jiff::civil::{Date, date};
use recipes::cost::UnitPrice;
use recipes::prices::{self, NewPrice, NoCost};
use recipes::{
    DraftInput, Error, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput,
    RecipeKind, ingredients, recipes as store,
};
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

fn clock() -> FakeClock {
    FakeClock::new("2026-12-01T10:00:00Z".parse().unwrap())
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

async fn price(conn: &mut PgConnection, id: IngredientId, cents: i64, from: Date) -> i64 {
    prices::add(
        conn,
        &clock(),
        &NewPrice {
            ingredient_id: id,
            price: UnitPrice::per_kg(Money::from_cents(cents)),
            valid_from: from,
            by: None,
        },
    )
    .await
    .unwrap()
    .id
}

async fn on(conn: &mut PgConnection, id: IngredientId, day: Date) -> Option<i64> {
    prices::prices_on(conn, day)
        .await
        .unwrap()
        .get(&id)
        .map(|p| p.per_kg_cents().cents())
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_price_valid_on_a_date_is_the_latest_from_before_it(
    mut conn: PoolConnection<Postgres>,
) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let salt = ingredient(&mut conn, "Zeezout", IngredientRole::Other).await;
    price(&mut conn, flour, 95, date(2026, 12, 1)).await;
    price(&mut conn, flour, 110, date(2026, 12, 10)).await;
    price(&mut conn, salt, 120, date(2026, 12, 5)).await;

    assert_eq!(on(&mut conn, flour, date(2026, 11, 30)).await, None);
    assert_eq!(on(&mut conn, flour, date(2026, 12, 1)).await, Some(95));
    assert_eq!(on(&mut conn, flour, date(2026, 12, 9)).await, Some(95));
    assert_eq!(on(&mut conn, flour, date(2026, 12, 10)).await, Some(110));
    assert_eq!(on(&mut conn, flour, date(2027, 6, 1)).await, Some(110));
    assert_eq!(on(&mut conn, salt, date(2026, 12, 4)).await, None);
    let valid = prices::valid_on(&mut conn, date(2026, 12, 5))
        .await
        .unwrap();
    assert_eq!(valid.len(), 2);
    assert_eq!(valid[&salt].valid_from, date(2026, 12, 5));
    assert_eq!(valid[&flour].valid_from, date(2026, 12, 1));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_second_price_on_the_same_day_counts_and_both_stay(mut conn: PoolConnection<Postgres>) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let first = price(&mut conn, flour, 95, date(2026, 12, 1)).await;
    let typo_fixed = price(&mut conn, flour, 59, date(2026, 12, 1)).await;
    let older = price(&mut conn, flour, 80, date(2026, 11, 1)).await;
    assert_eq!(on(&mut conn, flour, date(2026, 12, 1)).await, Some(59));

    let history = prices::history(&mut conn, flour).await.unwrap();
    let ids: Vec<i64> = history.iter().map(|p| p.id).collect();
    assert_eq!(ids, [typo_fixed, first, older]);
    assert_eq!(history[0].price, UnitPrice::per_kg(Money::from_cents(59)));
    assert_eq!(
        history[0].created_at,
        "2026-12-01T10:00:00Z".parse().unwrap()
    );
    assert!(
        prices::history(&mut conn, IngredientId::new(999))
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn prices_are_never_changed_or_removed(mut conn: PoolConnection<Postgres>) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let id = price(&mut conn, flour, 95, date(2026, 12, 1)).await;
    for statement in [
        "UPDATE ingredient_price SET price_per_kg_cents = 1 WHERE id = $1",
        "DELETE FROM ingredient_price WHERE id = $1",
    ] {
        let refused = sqlx::query(statement).bind(id).execute(&mut *conn).await;
        assert!(refused.is_err(), "{statement}");
    }
    assert_eq!(on(&mut conn, flour, date(2026, 12, 1)).await, Some(95));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_price_must_be_sensible(mut conn: PoolConnection<Postgres>) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let new = |id, cents, weight_g| NewPrice {
        ingredient_id: id,
        price: UnitPrice::new(Money::from_cents(cents), weight_g).unwrap(),
        valid_from: date(2026, 12, 1),
        by: None,
    };
    for (cents, weight, why) in [
        (-1, 1000, "price"),
        (prices::MAX_PRICE_CENTS + 1, 1000, "price"),
        (100, prices::MAX_WEIGHT_G + 1, "weight"),
        // €1.000 for 1 g is €1.000.000 per kilo.
        (100_000, 1, "per_kg"),
    ] {
        let refused = prices::add(&mut conn, &clock(), &new(flour, cents, weight)).await;
        assert!(
            matches!(refused, Err(Error::Invalid(w)) if w == why),
            "{cents} {weight}"
        );
    }
    let unknown = prices::add(&mut conn, &clock(), &new(IngredientId::new(999), 95, 1000)).await;
    assert!(matches!(unknown, Err(Error::NotFound)));
    let water = prices::add(&mut conn, &clock(), &new(flour, 0, 1))
        .await
        .unwrap();
    assert_eq!(water.price.price(), Money::ZERO);
    let most = prices::add(
        &mut conn,
        &clock(),
        &new(flour, prices::MAX_PRICE_CENTS, prices::MAX_WEIGHT_G),
    )
    .await
    .unwrap();
    assert_eq!(most.price.weight_g(), prices::MAX_WEIGHT_G);
    // €100 for 1 g is exactly the most per kilo.
    prices::add(&mut conn, &clock(), &new(flour, 10_000, 1))
        .await
        .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_price_is_kept_as_paid_for_its_weight(mut conn: PoolConnection<Postgres>) {
    let flour = ingredient(&mut conn, "Roggemeel", IngredientRole::Flour).await;
    let sack = UnitPrice::new(Money::from_cents(2137), 25_000).unwrap();
    let added = prices::add(
        &mut conn,
        &clock(),
        &NewPrice {
            ingredient_id: flour,
            price: sack,
            valid_from: date(2026, 12, 1),
            by: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(added.price, sack);
    assert_eq!(
        prices::history(&mut conn, flour).await.unwrap()[0].price,
        sack
    );
    let on_day = prices::prices_on(&mut conn, date(2026, 12, 1))
        .await
        .unwrap();
    assert_eq!(on_day[&flour], sack);
    // The rounded price per kilo is stored too, for readers that only know that column.
    let (per_kg, cents, weight): (i64, Option<i64>, Option<i32>) = sqlx::query_as(
        "SELECT price_per_kg_cents, price_cents, weight_g FROM ingredient_price WHERE id = $1",
    )
    .bind(added.id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!((per_kg, cents, weight), (85, Some(2137), Some(25_000)));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_price_row_is_old_style_or_new_style(mut conn: PoolConnection<Postgres>) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let insert = "INSERT INTO ingredient_price
        (ingredient_id, price_per_kg_cents, price_cents, weight_g, valid_from, created_at)
        VALUES ($1, $2, $3, $4, date '2026-12-01', now())";
    for (per_kg, cents, weight) in [
        (85, Some(2137), None),
        (85, None, Some(25_000)),
        (86, Some(2137), Some(25_000)),
        (0, Some(0), Some(0)),
        (532, Some(399), Some(-750)),
    ] {
        let refused = sqlx::query(insert)
            .bind(flour.get())
            .bind(per_kg)
            .bind(cents)
            .bind(weight)
            .execute(&mut *conn)
            .await;
        assert!(refused.is_err(), "{per_kg} {cents:?} {weight:?}");
    }
    for (per_kg, cents, weight) in [
        (95, None, None),
        (85, Some(2137), Some(25_000)),
        (532, Some(399), Some(750)),
        (33_333, Some(100), Some(3)),
    ] {
        sqlx::query(insert)
            .bind(flour.get())
            .bind(per_kg)
            .bind(cents)
            .bind(weight)
            .execute(&mut *conn)
            .await
            .unwrap_or_else(|e| panic!("{per_kg} {cents:?} {weight:?}: {e}"));
    }
}

/// The migration that added prices per package.
const PACKAGE_MIGRATION: i64 = 20_261_207_001_100;

#[sqlx::test(migrations = false)]
async fn prices_entered_per_kilo_keep_their_cost(pool: sqlx::PgPool) {
    let before = db::MIGRATOR
        .iter()
        .map(|m| m.version)
        .filter(|v| *v < PACKAGE_MIGRATION)
        .max()
        .unwrap();
    db::MIGRATOR.run_to(before, &pool).await.unwrap();
    let flour: i64 = sqlx::query_scalar(
        "INSERT INTO ingredient (name, role) VALUES ('Tarwebloem', 'flour') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ingredient_price (ingredient_id, price_per_kg_cents, valid_from, created_at)
        VALUES ($1, 95, date '2026-12-01', now()), ($1, 0, date '2026-11-01', now())",
    )
    .bind(flour)
    .execute(&pool)
    .await
    .unwrap();

    db::MIGRATOR.run(&pool).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let flour = IngredientId::new(flour);
    let history = prices::history(&mut conn, flour).await.unwrap();
    let kept: Vec<UnitPrice> = history.iter().map(|p| p.price).collect();
    assert_eq!(
        kept,
        [
            UnitPrice::per_kg(Money::from_cents(95)),
            UnitPrice::per_kg(Money::ZERO)
        ]
    );
    let price = prices::prices_on(&mut conn, date(2026, 12, 1))
        .await
        .unwrap()[&flour];
    assert_eq!(price.cost(10_453).get(), 10_453 * 95);
    assert_eq!(price.per_kg_cents(), Money::from_cents(95));
    let updated =
        sqlx::query("UPDATE ingredient_price SET price_per_kg_cents = price_per_kg_cents")
            .execute(&mut *conn)
            .await
            .expect_err("prices stay history");
    let code = updated.as_database_error().and_then(|e| e.code());
    assert_eq!(code.as_deref(), Some("23001"), "{updated}");
}

#[test]
fn today_is_the_date_in_amsterdam() {
    let at = |utc: &str| prices::today(&FakeClock::new(utc.parse().unwrap()));
    // Winter time, UTC+1.
    assert_eq!(at("2026-12-01T22:59:59Z"), date(2026, 12, 1));
    assert_eq!(at("2026-12-01T23:00:00Z"), date(2026, 12, 2));
    // Summer time, UTC+2.
    assert_eq!(at("2026-06-30T21:59:59Z"), date(2026, 6, 30));
    assert_eq!(at("2026-06-30T22:00:00Z"), date(2026, 7, 1));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_price_from_tomorrow_counts_after_midnight_in_amsterdam(
    mut conn: PoolConnection<Postgres>,
) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    price(&mut conn, flour, 95, date(2026, 12, 1)).await;
    price(&mut conn, flour, 110, date(2026, 12, 2)).await;
    let clock = FakeClock::new("2026-12-01T22:30:00Z".parse().unwrap());
    let today = prices::today(&clock);
    assert_eq!(on(&mut conn, flour, today).await, Some(95));
    clock.set("2026-12-01T23:30:00Z".parse().unwrap());
    let today = prices::today(&clock);
    assert_eq!(on(&mut conn, flour, today).await, Some(110));
}

async fn product(conn: &mut PgConnection, name: &str, dough_g: i32) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
        VALUES ($1, 650, 9, 800, $2) RETURNING id",
    )
    .bind(name)
    .bind(dough_g)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn standard_cost_is_the_version_in_use_at_the_prices_of_a_date(
    mut conn: PoolConnection<Postgres>,
) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let water = ingredient(&mut conn, "Water", IngredientRole::Water).await;
    let (_, levain_v1) = store::create(
        &mut conn,
        &RecipeInput {
            name: "Levain".to_owned(),
            kind: RecipeKind::Preferment,
        },
    )
    .await
    .unwrap();
    let line = |thousandths, part| LineInput {
        pct: Pct::from_thousandths(thousandths),
        part,
    };
    let save = |lines| DraftInput {
        lines,
        lead_time_hours: None,
        notes: String::new(),
    };
    store::save_draft(
        &mut conn,
        levain_v1.id,
        &save(vec![
            line(100_000, LinePart::Ingredient(flour)),
            line(100_000, LinePart::Ingredient(water)),
        ]),
    )
    .await
    .unwrap();
    let (dough, dough_v1) = store::create(
        &mut conn,
        &RecipeInput {
            name: "Desembrood".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    store::save_draft(
        &mut conn,
        dough_v1.id,
        &save(vec![
            line(80_000, LinePart::Ingredient(flour)),
            line(60_000, LinePart::Ingredient(water)),
            line(40_000, LinePart::Sub(levain_v1.id)),
        ]),
    )
    .await
    .unwrap();
    let bread = product(&mut conn, "Desem wit", 900).await;
    store::link_product(&mut conn, dough.id, bread)
        .await
        .unwrap();
    let no_dough = product(&mut conn, "Zonder deeg", 0).await;
    store::link_product(&mut conn, dough.id, no_dough)
        .await
        .unwrap();
    let no_recipe = product(&mut conn, "Koekjes", 50).await;
    let empty = product(&mut conn, "Leeg", 500).await;
    let (bare, _) = store::create(
        &mut conn,
        &RecipeInput {
            name: "Nog niets".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    store::link_product(&mut conn, bare.id, empty)
        .await
        .unwrap();
    let unused = product(&mut conn, "Geen versie", 500).await;
    let (none, _) = store::create(
        &mut conn,
        &RecipeInput {
            name: "Zonder versie".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    store::link_product(&mut conn, none.id, unused)
        .await
        .unwrap();
    sqlx::query("UPDATE recipe SET current_version_id = NULL WHERE id = $1")
        .bind(none.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    price(&mut conn, flour, 100, date(2026, 12, 1)).await;

    let all = [
        bread,
        no_dough,
        no_recipe,
        empty,
        unused,
        ProductId::new(999),
    ];
    let costs = prices::standard_costs(&mut conn, &all, date(2026, 12, 1))
        .await
        .unwrap();
    assert_eq!(costs.len(), 5, "an unknown product is left out");
    let of = |id: ProductId| costs.iter().find(|c| c.product.product_id == id).unwrap();
    assert_eq!(of(bread).per_piece(), None, "water has no price yet");
    let missing: Vec<IngredientId> = of(bread)
        .cost
        .as_ref()
        .unwrap()
        .dough
        .missing()
        .map(|i| i.id)
        .collect();
    assert_eq!(missing, [water]);
    assert_eq!(of(no_dough).cost, Err(NoCost::NoDough));
    assert_eq!(of(no_recipe).cost, Err(NoCost::NoRecipe));
    assert_eq!(of(empty).cost, Err(NoCost::NoLines));
    assert_eq!(of(unused).cost, Err(NoCost::NoVersion));
    assert_eq!(of(no_recipe).per_piece(), None);

    price(&mut conn, water, 0, date(2026, 12, 1)).await;
    let costs = prices::standard_costs(&mut conn, &[bread], date(2026, 12, 1))
        .await
        .unwrap();
    // 900 g of dough is 100 parts flour in 180: 500 g at €1,00 per kg.
    assert_eq!(costs[0].per_piece(), Some(Money::from_cents(50)));
    let before = prices::standard_costs(&mut conn, &[bread], date(2026, 11, 30))
        .await
        .unwrap();
    assert_eq!(before[0].per_piece(), None);
}
