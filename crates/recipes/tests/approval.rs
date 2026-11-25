//! A product's allergen approval is cleared when its recipe or an ingredient in it changes
//! (F10, F17, ADR 0037).
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use domain::{Allergen, IngredientId, ProductId, RecipeId, RecipeVersionId};
use jiff::Timestamp;
use recipes::{
    DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput, RecipeKind,
    ingredients, recipes as store,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn input(name: &str, role: IngredientRole) -> IngredientInput {
    IngredientInput {
        name: name.to_owned(),
        label_name: String::new(),
        role,
        supplier_name: String::new(),
        allergens: BTreeSet::new(),
    }
}

async fn ingredient(conn: &mut PgConnection, name: &str, role: IngredientRole) -> IngredientId {
    ingredients::create(conn, &input(name, role))
        .await
        .unwrap()
        .id
}

async fn recipe(
    conn: &mut PgConnection,
    name: &str,
    kind: RecipeKind,
) -> (RecipeId, RecipeVersionId) {
    let (recipe, version) = store::create(
        conn,
        &RecipeInput {
            name: name.to_owned(),
            kind,
        },
    )
    .await
    .unwrap();
    (recipe.id, version.id)
}

fn draft(lines: &[(LinePart, u32)]) -> DraftInput {
    DraftInput {
        lines: lines
            .iter()
            .map(|(part, percent)| LineInput {
                pct: Pct::from_thousandths(percent * 1000),
                part: *part,
            })
            .collect(),
        lead_time_hours: None,
        notes: String::new(),
    }
}

async fn save(conn: &mut PgConnection, version: RecipeVersionId, lines: &[(LinePart, u32)]) {
    store::save_draft(conn, version, &draft(lines))
        .await
        .unwrap();
}

async fn product(conn: &mut PgConnection, name: &str) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
        VALUES ($1, 650, 9, 800, 900) RETURNING id",
    )
    .bind(name)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

async fn approve(conn: &mut PgConnection, product: ProductId) {
    sqlx::query(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role)
            VALUES ('test-baker', 'Testbakker', 'baker')
            ON CONFLICT (oidc_sub) DO UPDATE SET name = EXCLUDED.name RETURNING id)
        UPDATE product SET allergens_approved_by = (SELECT id FROM baker),
            allergens_approved_at = '2026-11-01T10:00:00Z'
        WHERE id = $1",
    )
    .bind(product.get())
    .execute(conn)
    .await
    .unwrap();
}

/// Whether the product is approved, and why its approval was last cleared.
async fn state(conn: &mut PgConnection, product: ProductId) -> (bool, Option<String>) {
    sqlx::query_as(
        "SELECT allergens_approved_by IS NOT NULL, allergens_cleared_reason
        FROM product WHERE id = $1",
    )
    .bind(product.get())
    .fetch_one(conn)
    .await
    .unwrap()
}

const APPROVED: (bool, Option<String>) = (true, None);

fn cleared(reason: &str) -> (bool, Option<String>) {
    (false, Some(reason.to_owned()))
}

async fn link(conn: &mut PgConnection, recipe: RecipeId, product: ProductId) {
    store::link_product(conn, recipe, product).await.unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn linking_relinking_and_unlinking_a_recipe_clears_the_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let (white, _) = recipe(&mut conn, "Wit", RecipeKind::Dough).await;
    let (spelt, _) = recipe(&mut conn, "Spelt", RecipeKind::Dough).await;
    let bread = product(&mut conn, "Brood").await;
    approve(&mut conn, bread).await;
    assert_eq!(state(&mut conn, bread).await, APPROVED);

    link(&mut conn, white, bread).await;
    assert_eq!(state(&mut conn, bread).await, cleared("recipe"), "linked");

    approve(&mut conn, bread).await;
    assert_eq!(
        state(&mut conn, bread).await,
        APPROVED,
        "approval resets the reason"
    );
    link(&mut conn, white, bread).await;
    assert_eq!(state(&mut conn, bread).await, APPROVED, "same recipe again");

    link(&mut conn, spelt, bread).await;
    assert_eq!(
        state(&mut conn, bread).await,
        cleared("recipe"),
        "other recipe"
    );

    approve(&mut conn, bread).await;
    store::unlink_product(&mut conn, spelt, bread)
        .await
        .unwrap();
    assert_eq!(state(&mut conn, bread).await, cleared("recipe"), "unlinked");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn taking_another_version_into_use_clears_the_approval(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let (dough, v1) = recipe(&mut conn, "Wit", RecipeKind::Dough).await;
    save(&mut conn, v1, &[(LinePart::Ingredient(flour), 100)]).await;
    store::freeze(&mut conn, dough, v1, &clock).await.unwrap();
    let bread = product(&mut conn, "Brood").await;
    let other = product(&mut conn, "Ander brood").await;
    link(&mut conn, dough, bread).await;
    approve(&mut conn, bread).await;
    approve(&mut conn, other).await;

    let v2 = store::new_version(&mut conn, dough).await.unwrap().id;
    assert_eq!(
        state(&mut conn, bread).await,
        APPROVED,
        "a new draft is not in use"
    );
    store::set_current(&mut conn, dough, v1).await.unwrap();
    assert_eq!(state(&mut conn, bread).await, APPROVED, "same version");

    store::set_current(&mut conn, dough, v2).await.unwrap();
    assert_eq!(state(&mut conn, bread).await, cleared("recipe"));
    assert_eq!(state(&mut conn, other).await, APPROVED, "not made from it");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn editing_the_lines_of_a_draft_in_use_clears_the_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let sesame = ingredient(&mut conn, "Sesam", IngredientRole::Other).await;
    let (dough, v1) = recipe(&mut conn, "Wit", RecipeKind::Dough).await;
    save(&mut conn, v1, &[(LinePart::Ingredient(flour), 100)]).await;
    let bread = product(&mut conn, "Brood").await;
    link(&mut conn, dough, bread).await;
    approve(&mut conn, bread).await;

    let same_lines = DraftInput {
        lead_time_hours: Some(12),
        notes: "Langer rijzen".to_owned(),
        ..draft(&[(LinePart::Ingredient(flour), 100)])
    };
    store::save_draft(&mut conn, v1, &same_lines).await.unwrap();
    assert_eq!(
        state(&mut conn, bread).await,
        APPROVED,
        "notes and lead time only"
    );

    save(
        &mut conn,
        v1,
        &[
            (LinePart::Ingredient(flour), 100),
            (LinePart::Ingredient(sesame), 5),
        ],
    )
    .await;
    assert_eq!(state(&mut conn, bread).await, cleared("recipe"));
}

/// Dough v1 uses levain v1, which uses starter v1, which holds rye flour.
struct Chain {
    rye: IngredientId,
    water: IngredientId,
    dough: RecipeId,
    dough_v1: RecipeVersionId,
    starter: RecipeId,
    starter_v1: RecipeVersionId,
    bread: ProductId,
}

async fn chain(conn: &mut PgConnection) -> Chain {
    let rye = ingredient(conn, "Roggebloem", IngredientRole::Flour).await;
    let water = ingredient(conn, "Water", IngredientRole::Water).await;
    let flour = ingredient(conn, "Tarwebloem", IngredientRole::Flour).await;
    let (starter, starter_v1) = recipe(conn, "Moederdesem", RecipeKind::Preferment).await;
    save(
        conn,
        starter_v1,
        &[
            (LinePart::Ingredient(rye), 100),
            (LinePart::Ingredient(water), 100),
        ],
    )
    .await;
    let (_, levain_v1) = recipe(conn, "Desem", RecipeKind::Preferment).await;
    save(
        conn,
        levain_v1,
        &[
            (LinePart::Sub(starter_v1), 20),
            (LinePart::Ingredient(flour), 100),
        ],
    )
    .await;
    let (dough, dough_v1) = recipe(conn, "Desembrood", RecipeKind::Dough).await;
    save(
        conn,
        dough_v1,
        &[
            (LinePart::Sub(levain_v1), 20),
            (LinePart::Ingredient(flour), 100),
        ],
    )
    .await;
    let bread = product(conn, "Desembrood").await;
    link(conn, dough, bread).await;
    approve(conn, bread).await;
    Chain {
        rye,
        water,
        dough,
        dough_v1,
        starter,
        starter_v1,
        bread,
    }
}

async fn set_allergens(conn: &mut PgConnection, id: IngredientId, allergens: &[Allergen]) {
    update(conn, id, |i| {
        i.allergens = allergens.iter().copied().collect()
    })
    .await;
}

async fn update(
    conn: &mut PgConnection,
    id: IngredientId,
    change: impl FnOnce(&mut IngredientInput),
) {
    let current = ingredients::get(conn, id).await.unwrap();
    let mut changed = IngredientInput {
        name: current.name,
        label_name: current.label_name,
        role: current.role,
        supplier_name: current.supplier_name,
        allergens: current.allergens,
    };
    change(&mut changed);
    ingredients::update(conn, id, &changed).await.unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_ingredient_two_sub_recipes_deep_clears_the_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let c = chain(&mut conn).await;
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    store::freeze(&mut conn, c.dough, c.dough_v1, &clock)
        .await
        .unwrap();
    let frozen_lines = store::lines(&mut conn, c.starter_v1).await.unwrap();
    assert_eq!(
        state(&mut conn, c.bread).await,
        APPROVED,
        "freezing changes nothing"
    );

    set_allergens(&mut conn, c.rye, &[Allergen::CerealsContainingGluten]).await;
    assert_eq!(state(&mut conn, c.bread).await, cleared("ingredient"));
    assert_eq!(
        store::lines(&mut conn, c.starter_v1).await.unwrap(),
        frozen_lines,
        "the frozen versions stay as they are"
    );

    approve(&mut conn, c.bread).await;
    set_allergens(&mut conn, c.water, &[]).await;
    assert_eq!(state(&mut conn, c.bread).await, APPROVED, "same allergens");
    set_allergens(&mut conn, c.water, &[Allergen::SulphurDioxideAndSulphites]).await;
    assert_eq!(state(&mut conn, c.bread).await, cleared("ingredient"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn editing_a_draft_sub_recipe_two_levels_down_clears_the_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let c = chain(&mut conn).await;
    save(
        &mut conn,
        c.starter_v1,
        &[
            (LinePart::Ingredient(c.rye), 100),
            (LinePart::Ingredient(c.water), 90),
        ],
    )
    .await;
    assert_eq!(state(&mut conn, c.bread).await, cleared("recipe"));

    approve(&mut conn, c.bread).await;
    store::set_current(&mut conn, c.starter, c.starter_v1)
        .await
        .unwrap();
    assert_eq!(
        state(&mut conn, c.bread).await,
        APPROVED,
        "the dough uses a version of the starter, not the starter's version in use"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_label_name_counts_but_other_ingredient_fields_do_not(
    mut conn: PoolConnection<Postgres>,
) {
    let c = chain(&mut conn).await;

    update(&mut conn, c.rye, |i| {
        i.supplier_name = "Molen De Hoop".to_owned()
    })
    .await;
    update(&mut conn, c.rye, |i| i.role = IngredientRole::Other).await;
    assert_eq!(
        state(&mut conn, c.bread).await,
        APPROVED,
        "supplier and role"
    );

    update(&mut conn, c.rye, |i| i.name = "Rogge".to_owned()).await;
    assert_eq!(
        state(&mut conn, c.bread).await,
        cleared("ingredient"),
        "an empty label name is the name"
    );

    approve(&mut conn, c.bread).await;
    update(&mut conn, c.rye, |i| i.label_name = "Rogge".to_owned()).await;
    assert_eq!(state(&mut conn, c.bread).await, APPROVED, "same label");
    update(&mut conn, c.rye, |i| i.name = "Roggemeel".to_owned()).await;
    assert_eq!(
        state(&mut conn, c.bread).await,
        APPROVED,
        "label name is set"
    );
    update(&mut conn, c.rye, |i| i.label_name = "ROGGEmeel".to_owned()).await;
    assert_eq!(state(&mut conn, c.bread).await, cleared("ingredient"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn versions_not_in_use_and_other_edits_leave_the_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let nuts = ingredient(&mut conn, "Walnoot", IngredientRole::Other).await;
    let (dough, v1) = recipe(&mut conn, "Notenbrood", RecipeKind::Dough).await;
    save(
        &mut conn,
        v1,
        &[
            (LinePart::Ingredient(flour), 100),
            (LinePart::Ingredient(nuts), 10),
        ],
    )
    .await;
    store::freeze(&mut conn, dough, v1, &clock).await.unwrap();
    let v2 = store::new_version(&mut conn, dough).await.unwrap().id;
    save(&mut conn, v2, &[(LinePart::Ingredient(flour), 100)]).await;
    store::set_current(&mut conn, dough, v2).await.unwrap();
    let bread = product(&mut conn, "Notenbrood").await;
    link(&mut conn, dough, bread).await;
    approve(&mut conn, bread).await;

    set_allergens(&mut conn, nuts, &[Allergen::Nuts]).await;
    assert_eq!(
        state(&mut conn, bread).await,
        APPROVED,
        "walnut is only in the frozen v1, which is not in use"
    );

    store::update(
        &mut conn,
        dough,
        &RecipeInput {
            name: "Walnotenbrood".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    store::freeze(&mut conn, dough, v2, &clock).await.unwrap();
    store::new_version(&mut conn, dough).await.unwrap();
    assert_eq!(state(&mut conn, bread).await, APPROVED);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn any_writer_changing_a_line_in_use_clears_the_approval(mut conn: PoolConnection<Postgres>) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let (dough, v1) = recipe(&mut conn, "Wit", RecipeKind::Dough).await;
    save(&mut conn, v1, &[(LinePart::Ingredient(flour), 100)]).await;
    let bread = product(&mut conn, "Brood").await;
    link(&mut conn, dough, bread).await;
    approve(&mut conn, bread).await;

    sqlx::query("UPDATE recipe_line SET bakers_pct_x1000 = 90000 WHERE recipe_version_id = $1")
        .bind(v1.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(state(&mut conn, bread).await, cleared("recipe"));

    approve(&mut conn, bread).await;
    sqlx::query("DELETE FROM recipe_line WHERE recipe_version_id = $1")
        .bind(v1.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(state(&mut conn, bread).await, cleared("recipe"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_sub_recipe_used_only_by_a_version_not_in_use_leaves_the_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let water = ingredient(&mut conn, "Water", IngredientRole::Water).await;
    let (_, levain_v1) = recipe(&mut conn, "Desem", RecipeKind::Preferment).await;
    save(&mut conn, levain_v1, &[(LinePart::Ingredient(flour), 100)]).await;
    let (dough, v1) = recipe(&mut conn, "Wit", RecipeKind::Dough).await;
    save(&mut conn, v1, &[(LinePart::Ingredient(flour), 100)]).await;
    store::freeze(&mut conn, dough, v1, &clock).await.unwrap();
    let v2 = store::new_version(&mut conn, dough).await.unwrap().id;
    save(
        &mut conn,
        v2,
        &[
            (LinePart::Ingredient(flour), 100),
            (LinePart::Sub(levain_v1), 20),
        ],
    )
    .await;
    let bread = product(&mut conn, "Brood").await;
    link(&mut conn, dough, bread).await;
    approve(&mut conn, bread).await;

    save(
        &mut conn,
        levain_v1,
        &[
            (LinePart::Ingredient(flour), 100),
            (LinePart::Ingredient(water), 100),
        ],
    )
    .await;
    assert_eq!(
        state(&mut conn, bread).await,
        APPROVED,
        "only the draft v2 uses the levain"
    );
}

async fn revision(conn: &mut PgConnection, product: ProductId) -> i64 {
    sqlx::query_scalar("SELECT allergens_recipe_rev FROM product WHERE id = $1")
        .bind(product.get())
        .fetch_one(conn)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn every_recipe_change_counts_even_without_an_approval(mut conn: PoolConnection<Postgres>) {
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let (dough, v1) = recipe(&mut conn, "Wit", RecipeKind::Dough).await;
    let bread = product(&mut conn, "Brood").await;
    assert_eq!(revision(&mut conn, bread).await, 0);
    link(&mut conn, dough, bread).await;
    assert_eq!(revision(&mut conn, bread).await, 1);
    save(&mut conn, v1, &[(LinePart::Ingredient(flour), 100)]).await;
    assert_eq!(revision(&mut conn, bread).await, 2);
    update(&mut conn, flour, |i| {
        i.allergens = [Allergen::CerealsContainingGluten].into();
    })
    .await;
    assert_eq!(revision(&mut conn, bread).await, 3);
    assert_eq!(
        state(&mut conn, bread).await,
        (false, None),
        "never approved, so no reason to show"
    );
}
