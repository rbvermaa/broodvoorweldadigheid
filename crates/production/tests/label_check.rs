//! A batch's labels against the approved allergen data (F17, ADR 0046).
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use bakedays::{NewBakeDay, days};
use catalog::{ComputedAllergens, IngredientList, ListedIngredient, company, products};
use domain::{
    Allergen, BakeDayId, BatchId, IngredientId, PickupLocationId, ProductId, StaffUserId,
};
use orders::audit::Actor;
use production::Error;
use production::batches::{self, Adjustment};
use production::label_check::{self, LabelBlock, LabelSource};
use recipes::recipes::{self as store};
use recipes::{
    DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput, RecipeKind,
    ingredients, rollup,
};
use sqlx::{PgConnection, PgPool};
use testkit::FakeClock;

const BAKER_ID: StaffUserId = StaffUserId::new(3);
const BAKER: Actor = Actor::Staff(BAKER_ID);
const GLUTEN: Allergen = Allergen::CerealsContainingGluten;

fn clock() -> FakeClock {
    FakeClock::new("2026-12-01T10:00:00Z".parse().unwrap())
}

fn pct(percent: u32) -> Pct {
    Pct::from_thousandths(percent * 1000)
}

fn listed(items: &[(&str, bool)]) -> IngredientList {
    IngredientList(
        items
            .iter()
            .map(|&(name, emphasised)| ListedIngredient::plain(name.to_owned(), emphasised))
            .collect(),
    )
}

struct Kitchen {
    day: BakeDayId,
    bread: ProductId,
    spelt: IngredientId,
    sesame: IngredientId,
}

async fn ingredient(
    conn: &mut PgConnection,
    name: &str,
    role: IngredientRole,
    allergens: &[Allergen],
) -> IngredientId {
    ingredients::create(
        conn,
        &IngredientInput {
            name: name.to_owned(),
            label_name: name.to_lowercase(),
            role,
            hydration: None,
            supplier_name: String::new(),
            allergens: allergens.iter().copied().collect(),
        },
    )
    .await
    .unwrap()
    .id
}

/// A bread from 100 wheat flour, 75 water, 2 salt; the kitchen list approved.
async fn kitchen(conn: &mut PgConnection) -> Kitchen {
    sqlx::query(
        "INSERT INTO staff_user (id, oidc_sub, name, role) OVERRIDING SYSTEM VALUE
         VALUES ($1, 'baker-fixture', 'Bakker', 'baker')",
    )
    .bind(BAKER_ID.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    company::approve_kitchen_may_contain(conn, &BTreeSet::new(), BAKER_ID, &clock())
        .await
        .unwrap();
    let day = days::create(
        conn,
        &NewBakeDay {
            date: jiff::civil::date(2026, 12, 5),
            location_id: PickupLocationId::new(1),
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .unwrap()
    .id;
    let flour = ingredient(conn, "Tarwebloem", IngredientRole::Flour, &[GLUTEN]).await;
    let water = ingredient(conn, "Water", IngredientRole::Water, &[]).await;
    let salt = ingredient(conn, "Zout", IngredientRole::Other, &[]).await;
    let spelt = ingredient(conn, "Speltbloem", IngredientRole::Flour, &[GLUTEN]).await;
    let sesame = ingredient(
        conn,
        "Sesamzaad",
        IngredientRole::Other,
        &[Allergen::SesameSeeds],
    )
    .await;
    let (recipe, version) = store::create(
        conn,
        &RecipeInput {
            name: "Desembrood".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    let line = |percent, id| LineInput {
        pct: pct(percent),
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
    let bread: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online)
         VALUES ('Desem wit', 450, 9, 800, 900, true) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let bread = ProductId::new(bread);
    store::link_product(conn, recipe.id, bread).await.unwrap();
    Kitchen {
        day,
        bread,
        spelt,
        sesame,
    }
}

fn recipe_values() -> ComputedAllergens {
    ComputedAllergens {
        allergens: BTreeSet::from([GLUTEN]),
        ingredients: listed(&[("tarwebloem", true), ("water", false), ("zout", false)]),
    }
}

async fn approve_product(conn: &mut PgConnection, product: ProductId) {
    let revision = products::get(conn, product)
        .await
        .unwrap()
        .allergen_revision;
    rollup::approve_product(
        conn,
        product,
        &recipe_values(),
        revision,
        BAKER_ID,
        &clock(),
    )
    .await
    .unwrap();
}

async fn batch(conn: &mut PgConnection, k: &Kitchen) -> BatchId {
    batches::start_manual(conn, k.day, k.bread, 10, BAKER, &clock())
        .await
        .unwrap()
}

async fn set_pct(conn: &mut PgConnection, id: BatchId, line: usize, percent: u32) {
    let b = batches::get(conn, id).await.unwrap();
    batches::adjust(
        conn,
        id,
        &Adjustment {
            lines: vec![(b.lines[line].id, pct(percent))],
            pieces: vec![],
            reason: None,
        },
        BAKER,
        &clock(),
    )
    .await
    .unwrap();
}

async fn blocked(conn: &mut PgConnection, id: BatchId, product: ProductId) -> LabelBlock {
    match label_check::label_allowed(conn, id, product).await {
        Err(Error::LabelBlocked(block)) => block,
        other => panic!("expected a block, got {other:?}"),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_batch_as_in_the_recipe_uses_the_product_approval(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = batch(&mut conn, &k).await;
    assert_eq!(
        blocked(&mut conn, id, k.bread).await,
        LabelBlock::ProductNotApproved
    );

    approve_product(&mut conn, k.bread).await;
    let content = label_check::label_allowed(&mut conn, id, k.bread)
        .await
        .unwrap();
    assert_eq!(content.source, LabelSource::Product);
    assert_eq!(content.allergens, BTreeSet::from([GLUTEN]));
    assert_eq!(content.ingredients, recipe_values().ingredients);
    assert!(!content.order_changed);

    // A change of quantity that keeps the order by weight needs nothing.
    set_pct(&mut conn, id, 1, 70).await;
    assert_eq!(
        label_check::label_allowed(&mut conn, id, k.bread)
            .await
            .unwrap()
            .source,
        LabelSource::Product
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn hand_entered_product_data_does_not_cover_a_batch(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = batch(&mut conn, &k).await;
    sqlx::query(
        "UPDATE product SET allergens = '{cereals_containing_gluten}',
            ingredient_text = 'TARWEbloem, water, zout' WHERE id = $1",
    )
    .bind(k.bread.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE product SET allergens_approved_by = $2, allergens_approved_at = now()
         WHERE id = $1",
    )
    .bind(k.bread.get())
    .bind(BAKER_ID.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    match blocked(&mut conn, id, k.bread).await {
        LabelBlock::Differs { approved, .. } => assert_eq!(approved, None),
        other => panic!("{other:?}"),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_swapped_flour_blocks_labels_until_the_batch_is_approved(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    approve_product(&mut conn, k.bread).await;
    let id = batch(&mut conn, &k).await;
    set_pct(&mut conn, id, 0, 0).await;
    batches::add_line(
        &mut conn,
        id,
        k.spelt,
        pct(100),
        Some("molen dicht"),
        BAKER,
        &clock(),
    )
    .await
    .unwrap();

    let swapped = ComputedAllergens {
        allergens: BTreeSet::from([GLUTEN]),
        ingredients: listed(&[("speltbloem", true), ("water", false), ("zout", false)]),
    };
    match blocked(&mut conn, id, k.bread).await {
        LabelBlock::Differs {
            computed,
            approved,
            difference,
        } => {
            assert_eq!(computed, swapped);
            assert_eq!(approved, Some(recipe_values()));
            assert!(!difference.allergens);
            assert!(difference.ingredients);
            assert!(!difference.order);
        }
        other => panic!("{other:?}"),
    }

    let refused =
        label_check::approve_batch(&mut conn, id, &recipe_values(), BAKER_ID, &clock()).await;
    assert!(
        matches!(refused, Err(Error::AllergensChanged)),
        "{refused:?}"
    );

    label_check::approve_batch(&mut conn, id, &swapped, BAKER_ID, &clock())
        .await
        .unwrap();
    let content = label_check::label_allowed(&mut conn, id, k.bread)
        .await
        .unwrap();
    assert_eq!(content.source, LabelSource::Batch);
    assert_eq!(content.ingredients, swapped.ingredients);
    let check = label_check::check(&mut conn, id, k.bread).await.unwrap();
    assert_eq!(check.batch_approval.map(|a| a.by), Some(BAKER_ID));

    // Another change after the approval blocks again.
    batches::add_line(&mut conn, id, k.sesame, pct(5), None, BAKER, &clock())
        .await
        .unwrap();
    match blocked(&mut conn, id, k.bread).await {
        LabelBlock::Differs { difference, .. } => assert!(difference.allergens),
        other => panic!("{other:?}"),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_change_in_the_order_by_weight_blocks_and_warns(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    approve_product(&mut conn, k.bread).await;
    let id = batch(&mut conn, &k).await;
    // Water 120% now outweighs flour.
    set_pct(&mut conn, id, 1, 120).await;
    let reordered = ComputedAllergens {
        allergens: BTreeSet::from([GLUTEN]),
        ingredients: listed(&[("water", false), ("tarwebloem", true), ("zout", false)]),
    };
    match blocked(&mut conn, id, k.bread).await {
        LabelBlock::Differs {
            computed,
            difference,
            ..
        } => {
            assert_eq!(computed, reordered);
            assert!(difference.order);
        }
        other => panic!("{other:?}"),
    }
    label_check::approve_batch(&mut conn, id, &reordered, BAKER_ID, &clock())
        .await
        .unwrap();
    let content = label_check::label_allowed(&mut conn, id, k.bread)
        .await
        .unwrap();
    assert!(content.order_changed, "F18 still warns before printing");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn labels_need_the_kitchen_list_and_a_product_of_the_batch(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    approve_product(&mut conn, k.bread).await;
    let id = batch(&mut conn, &k).await;
    let other: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece) VALUES ('Koek', 250, 9, 100, 0) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert!(matches!(
        label_check::label_allowed(&mut conn, id, ProductId::new(other)).await,
        Err(Error::NotFound)
    ));

    company::set_kitchen_may_contain(&mut conn, &BTreeSet::from([Allergen::Nuts]))
        .await
        .unwrap();
    assert_eq!(
        blocked(&mut conn, id, k.bread).await,
        LabelBlock::KitchenNotApproved
    );
    company::approve_kitchen_may_contain(
        &mut conn,
        &BTreeSet::from([Allergen::Nuts]),
        BAKER_ID,
        &clock(),
    )
    .await
    .unwrap();
    let content = label_check::label_allowed(&mut conn, id, k.bread)
        .await
        .unwrap();
    assert_eq!(content.may_contain, BTreeSet::from([Allergen::Nuts]));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_mixed_batch_can_still_be_approved(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    approve_product(&mut conn, k.bread).await;
    let id = batch(&mut conn, &k).await;
    batches::add_line(&mut conn, id, k.sesame, pct(5), None, BAKER, &clock())
        .await
        .unwrap();
    batches::mark_mixed(&mut conn, id, BAKER, &clock())
        .await
        .unwrap();
    let computed = label_check::check(&mut conn, id, k.bread)
        .await
        .unwrap()
        .computed
        .unwrap();
    assert!(computed.allergens.contains(&Allergen::SesameSeeds));
    label_check::approve_batch(&mut conn, id, &computed, BAKER_ID, &clock())
        .await
        .unwrap();
    assert_eq!(
        label_check::label_allowed(&mut conn, id, k.bread)
            .await
            .unwrap()
            .allergens,
        computed.allergens
    );
}
