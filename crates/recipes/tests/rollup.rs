//! Allergens and ingredient list computed from a recipe (F17, ADR 0046).
#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap};

use catalog::{ComputedAllergens, IngredientList, ListedIngredient, products};
use domain::{Allergen, IngredientId, ProductId, RecipeId, RecipeVersionId, StaffUserId};
use proptest::prelude::*;
use recipes::{
    DraftInput, Error, Formula, Ingredient, IngredientInput, IngredientRef, IngredientRole, Line,
    LineInput, LinePart, Part, Pct, RecipeInput, RecipeKind, SubRecipe, ingredients,
    recipes as store, rollup,
};
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

const GLUTEN: Allergen = Allergen::CerealsContainingGluten;

fn pct(percent: u32) -> Pct {
    Pct::from_thousandths(percent * 1000)
}

fn leaf(id: i64, percent: u32) -> Line {
    Line {
        pct: pct(percent),
        part: Part::Ingredient(IngredientRef {
            id: IngredientId::new(id),
            name: format!("ingredient {id}"),
            role: IngredientRole::Other,
        }),
    }
}

fn sub(version: i64, percent: u32, lines: Vec<Line>) -> Line {
    Line {
        pct: pct(percent),
        part: Part::Sub(SubRecipe {
            version_id: RecipeVersionId::new(version),
            recipe_id: RecipeId::new(version),
            name: format!("Recept {version}"),
            version: 1,
            kind: RecipeKind::Preferment,
            formula: Formula { lines },
        }),
    }
}

fn known(id: i64, name: &str, label: &str, allergens: &[Allergen]) -> Ingredient {
    Ingredient {
        id: IngredientId::new(id),
        name: name.to_owned(),
        label_name: label.to_owned(),
        role: IngredientRole::Other,
        supplier_name: String::new(),
        allergens: allergens.iter().copied().collect(),
    }
}

fn book(items: Vec<Ingredient>) -> HashMap<IngredientId, Ingredient> {
    items.into_iter().map(|i| (i.id, i)).collect()
}

fn listed(items: &[(&str, bool)]) -> IngredientList {
    IngredientList(
        items
            .iter()
            .map(|&(name, emphasised)| ListedIngredient {
                name: name.to_owned(),
                emphasised,
            })
            .collect(),
    )
}

const FLOUR: i64 = 1;
const WATER: i64 = 2;
const SALT: i64 = 3;
const SESAME: i64 = 4;
const SPELT: i64 = 5;
const FLOUR_Y: i64 = 6;

fn bakery() -> HashMap<IngredientId, Ingredient> {
    book(vec![
        known(FLOUR, "Tarwebloem molen X", "tarwebloem", &[GLUTEN]),
        known(WATER, "Water", "", &[]),
        known(SALT, "Zeezout", "zout", &[]),
        known(SESAME, "Sesamzaad", "sesamzaad", &[Allergen::SesameSeeds]),
        known(SPELT, "Speltbloem", "speltbloem", &[GLUTEN]),
        known(FLOUR_Y, "Tarwebloem molen Y", "tarwebloem", &[GLUTEN]),
    ])
}

#[test]
fn a_levain_merges_into_the_dough_by_weight() {
    // 80 flour + 20 levain (10 flour, 10 water) + 70 water + 2 salt: flour 90, water 80.
    let formula = Formula {
        lines: vec![
            leaf(FLOUR, 80),
            sub(10, 20, vec![leaf(FLOUR, 100), leaf(WATER, 100)]),
            leaf(WATER, 70),
            leaf(SALT, 2),
        ],
    };
    let computed = rollup::compute(&formula, &bakery()).unwrap();
    assert_eq!(computed.allergens, BTreeSet::from([GLUTEN]));
    assert_eq!(
        computed.ingredients,
        listed(&[("tarwebloem", true), ("Water", false), ("zout", false)])
    );
}

#[test]
fn heavier_sub_recipe_ingredients_move_ahead() {
    // Water 40 on top, plus 60 levain of which 50 is water: water 90 beats flour 60.
    let formula = Formula {
        lines: vec![
            leaf(FLOUR, 50),
            leaf(WATER, 40),
            sub(10, 60, vec![leaf(FLOUR, 20), leaf(WATER, 100)]),
        ],
    };
    let computed = rollup::compute(&formula, &bakery()).unwrap();
    assert_eq!(
        computed.ingredients,
        listed(&[("Water", false), ("tarwebloem", true)])
    );
}

#[test]
fn allergens_come_from_any_depth() {
    let formula = Formula {
        lines: vec![
            leaf(WATER, 100),
            sub(
                10,
                10,
                vec![
                    leaf(WATER, 100),
                    sub(
                        11,
                        10,
                        vec![leaf(WATER, 100), sub(12, 10, vec![leaf(SESAME, 100)])],
                    ),
                ],
            ),
        ],
    };
    let computed = rollup::compute(&formula, &bakery()).unwrap();
    assert_eq!(computed.allergens, BTreeSet::from([Allergen::SesameSeeds]));
    assert_eq!(
        computed.ingredients,
        listed(&[("Water", false), ("sesamzaad", true)])
    );
}

#[test]
fn ingredients_with_the_same_label_name_are_one_entry() {
    // Two mills of wheat flour at 30 each outweigh water at 50 together.
    let formula = Formula {
        lines: vec![leaf(FLOUR, 30), leaf(WATER, 50), leaf(FLOUR_Y, 30)],
    };
    let computed = rollup::compute(&formula, &bakery()).unwrap();
    assert_eq!(
        computed.ingredients,
        listed(&[("tarwebloem", true), ("Water", false)])
    );
}

#[test]
fn a_line_at_zero_percent_is_left_out_with_its_allergens() {
    let formula = Formula {
        lines: vec![
            leaf(FLOUR, 0),
            leaf(SPELT, 100),
            leaf(WATER, 70),
            sub(10, 0, vec![leaf(SESAME, 100)]),
        ],
    };
    let computed = rollup::compute(&formula, &bakery()).unwrap();
    assert_eq!(computed.allergens, BTreeSet::from([GLUTEN]));
    assert_eq!(
        computed.ingredients,
        listed(&[("speltbloem", true), ("Water", false)])
    );
}

#[test]
fn ties_keep_the_order_of_the_recipe() {
    let formula = Formula {
        lines: vec![leaf(SALT, 10), leaf(WATER, 10), leaf(FLOUR, 10)],
    };
    let computed = rollup::compute(&formula, &bakery()).unwrap();
    assert_eq!(
        computed.ingredients,
        listed(&[("zout", false), ("Water", false), ("tarwebloem", true)])
    );
}

#[test]
fn an_unknown_ingredient_or_an_empty_sub_recipe_is_an_error() {
    let unknown = Formula {
        lines: vec![leaf(99, 100)],
    };
    assert!(matches!(
        rollup::compute(&unknown, &bakery()),
        Err(Error::Corrupt(_))
    ));
    let empty = Formula {
        lines: vec![leaf(FLOUR, 100), sub(10, 20, vec![])],
    };
    assert!(matches!(
        rollup::compute(&empty, &bakery()),
        Err(Error::Unweighable)
    ));
}

const IDS: i64 = 6;

fn allergen_strategy() -> impl Strategy<Value = BTreeSet<Allergen>> {
    proptest::collection::btree_set(proptest::sample::select(Allergen::ALL.to_vec()), 0..3)
}

/// Ingredients 1..=6 with random allergens; a few share a label name.
fn ingredients_strategy() -> impl Strategy<Value = HashMap<IngredientId, Ingredient>> {
    proptest::collection::vec(allergen_strategy(), IDS as usize).prop_map(|sets| {
        book(
            sets.into_iter()
                .zip(1..)
                .map(|(allergens, id)| Ingredient {
                    id: IngredientId::new(id),
                    name: format!("Ingrediënt {id}"),
                    label_name: format!("naam {}", id % 4),
                    role: IngredientRole::Other,
                    supplier_name: String::new(),
                    allergens,
                })
                .collect(),
        )
    })
}

/// Formulas up to three sub-recipe levels deep; every level has at least one line above 0%.
fn formula_strategy() -> impl Strategy<Value = Formula> {
    let leaf_line = (1..=IDS, 0u32..300).prop_map(|(id, percent)| leaf(id, percent));
    let tree = leaf_line.prop_recursive(3, 24, 4, |inner| {
        (
            proptest::collection::vec(inner, 1..4),
            1u32..100,
            10i64..1000,
        )
            .prop_map(|(mut lines, percent, version)| {
                lines.push(leaf(1, 1));
                sub(version, percent, lines)
            })
    });
    proptest::collection::vec(tree, 1..6).prop_map(|mut lines| {
        lines.push(leaf(2, 1));
        Formula { lines }
    })
}

/// Every ingredient on a path of lines above 0%.
fn used(lines: &[Line], out: &mut Vec<IngredientId>) {
    for line in lines.iter().filter(|l| l.pct.thousandths() > 0) {
        match &line.part {
            Part::Ingredient(i) => out.push(i.id),
            Part::Sub(s) => used(&s.formula.lines, out),
        }
    }
}

proptest! {
    #[test]
    fn computed_allergens_are_the_union_of_every_used_ingredient(
        formula in formula_strategy(),
        book in ingredients_strategy(),
    ) {
        let computed = rollup::compute(&formula, &book).unwrap();
        let mut ids = Vec::new();
        used(&formula.lines, &mut ids);
        let mut union = BTreeSet::new();
        for id in &ids {
            let allergens = &book[id].allergens;
            prop_assert!(computed.allergens.is_superset(allergens));
            union.extend(allergens.iter().copied());
        }
        prop_assert_eq!(&computed.allergens, &union);
        let labels: BTreeSet<&str> = ids.iter().map(|id| book[id].label()).collect();
        let names: Vec<&str> = computed.ingredients.0.iter().map(|i| i.name.as_str()).collect();
        prop_assert_eq!(names.len(), labels.len());
        prop_assert_eq!(names.iter().copied().collect::<BTreeSet<_>>(), labels);
        for entry in &computed.ingredients.0 {
            let any = ids
                .iter()
                .filter(|id| book[*id].label() == entry.name)
                .any(|id| !book[id].allergens.is_empty());
            prop_assert_eq!(entry.emphasised, any);
        }
    }

    #[test]
    fn equal_weights_keep_the_order_of_the_lines(order in Just((1..=IDS).collect::<Vec<_>>()).prop_shuffle(), percent in 1u32..500) {
        let distinct = book(
            (1..=IDS)
                .map(|id| known(id, &format!("I{id}"), &format!("i{id}"), &[]))
                .collect(),
        );
        let formula = Formula { lines: order.iter().map(|&id| leaf(id, percent)).collect() };
        let computed = rollup::compute(&formula, &distinct).unwrap();
        let names: Vec<String> = computed.ingredients.0.iter().map(|i| i.name.clone()).collect();
        let expected: Vec<String> = order.iter().map(|id| format!("i{id}")).collect();
        prop_assert_eq!(names, expected);
    }

    #[test]
    fn top_level_weights_come_out_in_descending_order(percents in proptest::collection::vec(1u32..1000, 6)) {
        let distinct = book(
            (1..=IDS)
                .map(|id| known(id, &format!("I{id}"), &format!("i{id}"), &[]))
                .collect(),
        );
        let formula = Formula {
            lines: percents.iter().zip(1..).map(|(&p, id)| leaf(id, p)).collect(),
        };
        let computed = rollup::compute(&formula, &distinct).unwrap();
        let mut expected: Vec<(u32, i64)> = percents.iter().copied().zip(1..).collect();
        expected.sort_by_key(|e| std::cmp::Reverse(e.0));
        let expected: Vec<String> = expected.iter().map(|(_, id)| format!("i{id}")).collect();
        let names: Vec<String> = computed.ingredients.0.iter().map(|i| i.name.clone()).collect();
        prop_assert_eq!(names, expected);
    }
}

// Against the database.

const BAKER: StaffUserId = StaffUserId::new(7);

async fn add_baker(conn: &mut PgConnection) {
    sqlx::query(
        "INSERT INTO staff_user (id, oidc_sub, name, role) OVERRIDING SYSTEM VALUE
         VALUES ($1, 'baker-fixture', 'Bakker', 'baker') ON CONFLICT (id) DO NOTHING",
    )
    .bind(BAKER.get())
    .execute(conn)
    .await
    .unwrap();
}

async fn ingredient(
    conn: &mut PgConnection,
    name: &str,
    label: &str,
    allergens: &[Allergen],
) -> IngredientId {
    ingredients::create(
        conn,
        &IngredientInput {
            name: name.to_owned(),
            label_name: label.to_owned(),
            role: IngredientRole::Other,
            supplier_name: String::new(),
            allergens: allergens.iter().copied().collect(),
        },
    )
    .await
    .unwrap()
    .id
}

fn lines(items: &[(LinePart, u32)]) -> DraftInput {
    DraftInput {
        lines: items
            .iter()
            .map(|&(part, percent)| LineInput {
                pct: pct(percent),
                part,
            })
            .collect(),
        lead_time_hours: None,
        notes: String::new(),
    }
}

struct Kitchen {
    bread: ProductId,
    levain: RecipeVersionId,
    dough: RecipeVersionId,
    sesame: IngredientId,
}

/// A bread from 80 flour, 20 levain (1:1), 70 water and 2 salt.
async fn kitchen(conn: &mut PgConnection) -> Kitchen {
    add_baker(conn).await;
    let flour = ingredient(conn, "Tarwebloem", "tarwebloem", &[GLUTEN]).await;
    let water = ingredient(conn, "Water", "", &[]).await;
    let salt = ingredient(conn, "Zeezout", "zout", &[]).await;
    let sesame = ingredient(conn, "Sesam", "sesamzaad", &[Allergen::SesameSeeds]).await;
    let (_, levain) = store::create(
        conn,
        &RecipeInput {
            name: "Desem".to_owned(),
            kind: RecipeKind::Preferment,
        },
    )
    .await
    .unwrap();
    store::save_draft(
        conn,
        levain.id,
        &lines(&[
            (LinePart::Ingredient(flour), 100),
            (LinePart::Ingredient(water), 100),
        ]),
    )
    .await
    .unwrap();
    let (recipe, dough) = store::create(
        conn,
        &RecipeInput {
            name: "Desembrood".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    store::save_draft(
        conn,
        dough.id,
        &lines(&[
            (LinePart::Ingredient(flour), 80),
            (LinePart::Sub(levain.id), 20),
            (LinePart::Ingredient(water), 70),
            (LinePart::Ingredient(salt), 2),
        ]),
    )
    .await
    .unwrap();
    let bread: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, ingredient_text)
         VALUES ('Desembrood', 650, 9, 800, 900, 'met de hand') RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let bread = ProductId::new(bread);
    store::link_product(conn, recipe.id, bread).await.unwrap();
    Kitchen {
        bread,
        levain: levain.id,
        dough: dough.id,
        sesame,
    }
}

fn expected() -> ComputedAllergens {
    ComputedAllergens {
        allergens: BTreeSet::from([GLUTEN]),
        ingredients: listed(&[("tarwebloem", true), ("Water", false), ("zout", false)]),
    }
}

fn clock() -> FakeClock {
    FakeClock::new("2026-12-01T10:00:00Z".parse().unwrap())
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_product_shows_what_its_recipe_in_use_computes(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let rolled = rollup::for_product(&mut conn, k.bread)
        .await
        .unwrap()
        .expect("has a recipe");
    assert_eq!(rolled.version_id, k.dough);
    assert_eq!(rolled.computed, Some(expected()));
    assert_eq!(
        rollup::for_version(&mut conn, k.dough).await.unwrap(),
        expected()
    );

    let plain: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
         VALUES ('Koek', 250, 9, 100, 0) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(
        rollup::for_product(&mut conn, ProductId::new(plain))
            .await
            .unwrap(),
        None,
        "a product without a recipe keeps its hand-entered values"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn approving_computed_values_stores_them_on_the_product(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let before = products::get(&mut conn, k.bread).await.unwrap();
    assert_eq!(before.approval, None);
    let approved = rollup::approve_product(
        &mut conn,
        k.bread,
        &expected(),
        before.allergen_revision,
        BAKER,
        &clock(),
    )
    .await
    .unwrap();
    assert_eq!(approved.approval.map(|a| a.by), Some(BAKER));
    assert_eq!(approved.allergen_info.allergens, BTreeSet::from([GLUTEN]));
    assert_eq!(
        approved.allergen_info.ingredient_text,
        "tarwebloem, Water, zout"
    );
    assert_eq!(approved.ingredient_list, Some(expected().ingredients));
    assert_eq!(products::get(&mut conn, k.bread).await.unwrap(), approved);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_approval_for_an_old_revision_or_other_values_is_refused(
    mut conn: PoolConnection<Postgres>,
) {
    let k = kitchen(&mut conn).await;
    let revision = products::get(&mut conn, k.bread)
        .await
        .unwrap()
        .allergen_revision;

    let mut other = expected();
    other.allergens.insert(Allergen::Milk);
    let refused =
        rollup::approve_product(&mut conn, k.bread, &other, revision, BAKER, &clock()).await;
    assert!(
        matches!(refused, Err(Error::AllergensChanged)),
        "{refused:?}"
    );

    // A sesame topping in the levain: the recipe changed after the page was opened.
    sqlx::query(
        "UPDATE recipe_line SET ingredient_id = $1 WHERE recipe_version_id = $2 AND position = 1",
    )
    .bind(k.sesame.get())
    .bind(k.levain.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    let stale =
        rollup::approve_product(&mut conn, k.bread, &expected(), revision, BAKER, &clock()).await;
    assert!(matches!(stale, Err(Error::AllergensChanged)), "{stale:?}");

    let fresh = products::get(&mut conn, k.bread).await.unwrap();
    assert_eq!(fresh.approval, None);
    let now = rollup::for_product(&mut conn, k.bread)
        .await
        .unwrap()
        .unwrap()
        .computed
        .unwrap();
    assert!(now.allergens.contains(&Allergen::SesameSeeds));
    rollup::approve_product(
        &mut conn,
        k.bread,
        &now,
        fresh.allergen_revision,
        BAKER,
        &clock(),
    )
    .await
    .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_hand_edit_of_computed_values_drops_the_list(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let revision = products::get(&mut conn, k.bread)
        .await
        .unwrap()
        .allergen_revision;
    rollup::approve_product(&mut conn, k.bread, &expected(), revision, BAKER, &clock())
        .await
        .unwrap();
    sqlx::query("UPDATE product SET name_nl = 'Desem' WHERE id = $1")
        .bind(k.bread.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let kept = products::get(&mut conn, k.bread).await.unwrap();
    assert!(kept.approval.is_some());
    assert_eq!(kept.ingredient_list, Some(expected().ingredients));

    let mut input = catalog::ProductInput {
        name_nl: kept.name_nl.clone(),
        description_nl: kept.description_nl.clone(),
        photo_path: None,
        price_incl: kept.pricing.price_incl(),
        vat_rate: kept.vat_rate,
        pay_what_you_want: None,
        unit_weight_g: kept.unit_weight_g,
        dough_g_per_piece: kept.dough_g_per_piece,
        freezer_days: None,
        lead_time_hours: None,
        allergen_info: kept.allergen_info.clone(),
        packaged: false,
        sold_online: true,
        estimated_cost: None,
        active: true,
    };
    let same = products::update(&mut conn, k.bread, &input).await.unwrap();
    assert!(same.approval.is_some(), "saving the same values keeps all");
    assert_eq!(same.ingredient_list, Some(expected().ingredients));

    input.allergen_info.ingredient_text = "TARWEbloem, water, zout".to_owned();
    let edited = products::update(&mut conn, k.bread, &input).await.unwrap();
    assert_eq!(edited.approval, None);
    assert_eq!(edited.ingredient_list, None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn changing_the_stored_list_clears_the_approval(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let revision = products::get(&mut conn, k.bread)
        .await
        .unwrap()
        .allergen_revision;
    rollup::approve_product(&mut conn, k.bread, &expected(), revision, BAKER, &clock())
        .await
        .unwrap();
    sqlx::query("UPDATE product SET ingredient_emphasis = '{f,f,f}' WHERE id = $1")
        .bind(k.bread.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        products::get(&mut conn, k.bread).await.unwrap().approval,
        None
    );
}
