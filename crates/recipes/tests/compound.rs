//! Fillings and toppings as compound ingredients on the ingredient list (F17, ADR 0046).
#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap};

use catalog::{ComputedAllergens, IngredientList, ListedIngredient, products};
use domain::{Allergen, IngredientId, ProductId, RecipeId, RecipeVersionId, StaffUserId};
use proptest::prelude::*;
use recipes::rollup::{self, CompoundNames};
use recipes::{
    DraftInput, Formula, Ingredient, IngredientInput, IngredientRef, IngredientRole, LabelSetting,
    Line, LineInput, LinePart, Part, Pct, RecipeInput, RecipeKind, SubRecipe, ingredients,
    recipes as store,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

const GLUTEN: Allergen = Allergen::CerealsContainingGluten;

fn leaf(id: i64, thousandths: u32) -> Line {
    Line {
        pct: Pct::from_thousandths(thousandths),
        part: Part::Ingredient(IngredientRef {
            id: IngredientId::new(id),
            name: format!("ingredient {id}"),
            role: IngredientRole::Other,
        }),
    }
}

fn sub(recipe: i64, kind: RecipeKind, thousandths: u32, lines: Vec<Line>) -> Line {
    Line {
        pct: Pct::from_thousandths(thousandths),
        part: Part::Sub(SubRecipe {
            version_id: RecipeVersionId::new(recipe),
            recipe_id: RecipeId::new(recipe),
            name: format!("Recept {recipe}"),
            version: 1,
            kind,
            formula: Formula { lines },
        }),
    }
}

fn known(id: i64, label: &str, allergens: &[Allergen]) -> Ingredient {
    Ingredient {
        id: IngredientId::new(id),
        name: format!("Ingrediënt {id}"),
        label_name: label.to_owned(),
        role: IngredientRole::Other,
        supplier_name: String::new(),
        allergens: allergens.iter().copied().collect(),
    }
}

const FLOUR: i64 = 1;
const WATER: i64 = 2;
const SALT: i64 = 3;
const SUGAR: i64 = 4;
const CINNAMON: i64 = 5;
const BUTTER: i64 = 6;
const HAZELNUT: i64 = 7;

const LEVAIN: i64 = 10;
const FILLING: i64 = 20;
const TOPPING: i64 = 30;

fn bakery() -> HashMap<IngredientId, Ingredient> {
    [
        known(FLOUR, "tarwebloem", &[GLUTEN]),
        known(WATER, "water", &[]),
        known(SALT, "zout", &[]),
        known(SUGAR, "suiker", &[]),
        known(CINNAMON, "kaneel", &[]),
        known(BUTTER, "boter", &[Allergen::Milk]),
        known(HAZELNUT, "hazelnoot", &[Allergen::Nuts]),
    ]
    .into_iter()
    .map(|i| (i.id, i))
    .collect()
}

fn names(pairs: &[(i64, &str)]) -> CompoundNames {
    pairs
        .iter()
        .map(|&(id, name)| (RecipeId::new(id), name.to_owned()))
        .collect()
}

fn plain(items: &[(&str, bool)]) -> Vec<ListedIngredient> {
    items
        .iter()
        .map(|&(name, emphasised)| ListedIngredient::plain(name, emphasised))
        .collect()
}

fn pct(percent: u32) -> u32 {
    percent * 1000
}

/// 100 flour, 20 levain (1:1), 60 water, 40 filling (50 sugar, 10 cinnamon, 40 butter),
/// 2 salt.
fn bun() -> Formula {
    Formula {
        lines: vec![
            leaf(FLOUR, pct(100)),
            sub(
                LEVAIN,
                RecipeKind::Preferment,
                pct(20),
                vec![leaf(FLOUR, pct(100)), leaf(WATER, pct(100))],
            ),
            leaf(WATER, pct(60)),
            sub(
                FILLING,
                RecipeKind::Filling,
                pct(40),
                vec![
                    leaf(SUGAR, pct(50)),
                    leaf(CINNAMON, pct(10)),
                    leaf(BUTTER, pct(40)),
                ],
            ),
            leaf(SALT, pct(2)),
        ],
    }
}

#[test]
fn a_filling_is_one_entry_by_its_weight_with_its_own_ingredients_inside() {
    let computed = rollup::compute(&bun(), &bakery(), &names(&[(FILLING, "vulling")])).unwrap();
    assert_eq!(
        computed.ingredients,
        IngredientList(vec![
            ListedIngredient::plain("tarwebloem", true),
            ListedIngredient::plain("water", false),
            ListedIngredient::compound(
                "vulling",
                plain(&[("suiker", false), ("boter", true), ("kaneel", false)]),
            ),
            ListedIngredient::plain("zout", false),
        ])
    );
    assert_eq!(
        computed.ingredients.text(),
        "tarwebloem, water, vulling (suiker, boter, kaneel), zout"
    );
    assert_eq!(computed.allergens, BTreeSet::from([GLUTEN, Allergen::Milk]));
}

#[test]
fn without_the_setting_a_filling_merges_like_a_levain() {
    let computed = rollup::compute(&bun(), &bakery(), &CompoundNames::new()).unwrap();
    assert_eq!(
        computed.ingredients,
        IngredientList(plain(&[
            ("tarwebloem", true),
            ("water", false),
            ("suiker", false),
            ("boter", true),
            ("kaneel", false),
            ("zout", false),
        ]))
    );
}

#[test]
fn a_compound_outweighing_the_flour_goes_first() {
    let mut heavy = bun();
    heavy.lines[3].pct = Pct::from_thousandths(pct(150));
    let computed = rollup::compute(&heavy, &bakery(), &names(&[(FILLING, "vulling")])).unwrap();
    let top: Vec<&str> = computed.ingredients.0.iter().map(|i| &*i.name).collect();
    assert_eq!(top, ["vulling", "tarwebloem", "water", "zout"]);
}

#[test]
fn an_ingredient_in_the_dough_and_the_filling_is_listed_in_both() {
    let mut buttered = bun();
    buttered.lines.push(leaf(BUTTER, pct(5)));
    let computed = rollup::compute(&buttered, &bakery(), &names(&[(FILLING, "vulling")])).unwrap();
    assert_eq!(
        computed.ingredients.text(),
        "tarwebloem, water, vulling (suiker, boter, kaneel), boter, zout"
    );
}

#[test]
fn a_compound_inside_a_compound_merges_into_the_outer_brackets() {
    // A filling with a nut topping in it and a levain-like soaker: one level of brackets.
    let formula = Formula {
        lines: vec![
            leaf(FLOUR, pct(100)),
            sub(
                FILLING,
                RecipeKind::Filling,
                pct(30),
                vec![
                    leaf(SUGAR, pct(40)),
                    sub(
                        TOPPING,
                        RecipeKind::Topping,
                        pct(60),
                        vec![leaf(HAZELNUT, pct(50)), leaf(SUGAR, pct(50))],
                    ),
                ],
            ),
        ],
    };
    let compounds = names(&[(FILLING, "vulling"), (TOPPING, "crumble")]);
    let computed = rollup::compute(&formula, &bakery(), &compounds).unwrap();
    assert_eq!(
        computed.ingredients,
        IngredientList(vec![
            ListedIngredient::plain("tarwebloem", true),
            ListedIngredient::compound("vulling", plain(&[("suiker", false), ("hazelnoot", true)]),),
        ])
    );
    assert!(computed.allergens.contains(&Allergen::Nuts));
}

#[test]
fn a_compound_inside_a_merged_sub_recipe_still_shows_as_compound() {
    let formula = Formula {
        lines: vec![
            leaf(FLOUR, pct(100)),
            sub(
                LEVAIN,
                RecipeKind::Soaker,
                pct(30),
                vec![
                    leaf(WATER, pct(100)),
                    sub(
                        TOPPING,
                        RecipeKind::Topping,
                        pct(20),
                        vec![leaf(HAZELNUT, pct(100))],
                    ),
                ],
            ),
        ],
    };
    let computed = rollup::compute(&formula, &bakery(), &names(&[(TOPPING, "topping")])).unwrap();
    assert_eq!(
        computed.ingredients.text(),
        "tarwebloem, water, topping (hazelnoot)"
    );
}

#[test]
fn compounds_with_the_same_name_are_one_entry() {
    let formula = Formula {
        lines: vec![
            leaf(FLOUR, pct(100)),
            sub(
                FILLING,
                RecipeKind::Filling,
                pct(10),
                vec![leaf(SUGAR, pct(100))],
            ),
            sub(
                TOPPING,
                RecipeKind::Topping,
                pct(10),
                vec![leaf(SUGAR, pct(50)), leaf(CINNAMON, pct(50))],
            ),
        ],
    };
    let compounds = names(&[(FILLING, "Vulling"), (TOPPING, "vulling")]);
    let computed = rollup::compute(&formula, &bakery(), &compounds).unwrap();
    assert_eq!(
        computed.ingredients.text(),
        "tarwebloem, Vulling (suiker, kaneel)"
    );
}

#[test]
fn a_filling_at_zero_percent_is_left_out() {
    let mut empty = bun();
    empty.lines[3].pct = Pct::from_thousandths(0);
    let computed = rollup::compute(&empty, &bakery(), &names(&[(FILLING, "vulling")])).unwrap();
    assert_eq!(computed.ingredients.text(), "tarwebloem, water, zout");
    assert_eq!(computed.allergens, BTreeSet::from([GLUTEN]));
}

fn distinct() -> HashMap<IngredientId, Ingredient> {
    (1..=6)
        .map(|id| known(id, &format!("i{id}"), &[]))
        .map(|i| (i.id, i))
        .collect()
}

fn flatten(list: &IngredientList) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for item in &list.0 {
        if item.parts.is_empty() {
            out.insert(item.name.clone());
        }
        for part in &item.parts {
            out.insert(part.name.clone());
        }
    }
    out
}

/// Formulas up to three sub-recipe levels deep over recipes 10..14; every level has a
/// line above 0%.
fn formula_strategy() -> impl Strategy<Value = Formula> {
    let leaf_line = (1i64..=6, 0u32..300_000).prop_map(|(id, t)| leaf(id, t));
    let tree = leaf_line.prop_recursive(3, 24, 4, |inner| {
        (
            proptest::collection::vec(inner, 1..4),
            1u32..100_000,
            10i64..14,
        )
            .prop_map(|(mut lines, t, recipe)| {
                lines.push(leaf(1, 1000));
                sub(recipe, RecipeKind::Filling, t, lines)
            })
    });
    proptest::collection::vec(tree, 1..6).prop_map(|mut lines| {
        lines.push(leaf(2, 1000));
        Formula { lines }
    })
}

fn allergen_book() -> impl Strategy<Value = HashMap<IngredientId, Ingredient>> {
    let set =
        proptest::collection::btree_set(proptest::sample::select(Allergen::ALL.to_vec()), 0..3);
    proptest::collection::vec(set, 6).prop_map(|sets| {
        sets.into_iter()
            .zip(1..)
            .map(|(allergens, id)| Ingredient {
                allergens,
                ..known(id, &format!("naam {}", id % 4), &[])
            })
            .map(|i| (i.id, i))
            .collect()
    })
}

proptest! {
    #[test]
    fn compounds_keep_every_ingredient_and_allergen(
        formula in formula_strategy(),
        book in allergen_book(),
        chosen in proptest::collection::btree_set(10i64..14, 0..4),
    ) {
        let compounds: CompoundNames = chosen
            .iter()
            .map(|&id| (RecipeId::new(id), format!("samengesteld {}", id % 2)))
            .collect();
        let merged = rollup::compute(&formula, &book, &CompoundNames::new()).unwrap();
        let computed = rollup::compute(&formula, &book, &compounds).unwrap();
        prop_assert_eq!(&computed.allergens, &merged.allergens);
        prop_assert_eq!(flatten(&computed.ingredients), flatten(&merged.ingredients));
        let mut seen = BTreeSet::new();
        for item in &computed.ingredients.0 {
            prop_assert!(seen.insert((item.name.to_lowercase(), item.parts.is_empty())));
            if !item.parts.is_empty() {
                prop_assert!(!item.emphasised);
                prop_assert!(compounds.values().any(|n| *n == item.name));
                let mut inner = BTreeSet::new();
                for part in &item.parts {
                    prop_assert!(part.parts.is_empty(), "brackets nest one level");
                    prop_assert!(inner.insert(part.name.to_lowercase()));
                }
            }
        }
        let (names, emphasis, depth) = computed.ingredients.to_db();
        prop_assert_eq!(
            IngredientList::from_db(Some(names), Some(emphasis), Some(depth)).unwrap(),
            Some(computed.ingredients.clone())
        );
    }

    /// The filling's own list, computed on its own, is what goes in the brackets; the
    /// filling takes its place among the dough's ingredients by its line's weight.
    #[test]
    fn a_compound_holds_its_own_list_and_sits_by_its_weight(
        top in proptest::collection::vec(1u32..200_000, 3),
        inner in proptest::collection::vec(1u32..200_000, 3),
        filling in 1u32..200_000,
        at in 0usize..4,
    ) {
        let filling_lines: Vec<Line> = inner.iter().zip(4..).map(|(&t, id)| leaf(id, t)).collect();
        let mut lines: Vec<Line> = top.iter().zip(1..).map(|(&t, id)| leaf(id, t)).collect();
        lines.insert(at, sub(FILLING, RecipeKind::Filling, filling, filling_lines.clone()));
        let compounds = names(&[(FILLING, "vulling")]);
        let computed = rollup::compute(&Formula { lines }, &distinct(), &compounds).unwrap();

        let alone = rollup::compute(&Formula { lines: filling_lines }, &distinct(), &compounds)
            .unwrap();
        let compound = computed.ingredients.0.iter().find(|i| i.name == "vulling").unwrap();
        prop_assert_eq!(&compound.parts, &alone.ingredients.0);

        let mut expected: Vec<(u32, String)> =
            top.iter().zip(1..).map(|(&t, id)| (t, format!("i{id}"))).collect();
        expected.insert(at, (filling, "vulling".to_owned()));
        expected.sort_by_key(|e| std::cmp::Reverse(e.0));
        let order: Vec<&str> = computed.ingredients.0.iter().map(|i| &*i.name).collect();
        let expected: Vec<&str> = expected.iter().map(|(_, n)| n.as_str()).collect();
        prop_assert_eq!(order, expected);
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

async fn ingredient(conn: &mut PgConnection, label: &str, allergens: &[Allergen]) -> IngredientId {
    ingredients::create(
        conn,
        &IngredientInput {
            name: label.to_owned(),
            label_name: String::new(),
            role: IngredientRole::Other,
            supplier_name: String::new(),
            allergens: allergens.iter().copied().collect(),
        },
    )
    .await
    .unwrap()
    .id
}

fn draft(items: &[(LinePart, u32)]) -> DraftInput {
    DraftInput {
        lines: items
            .iter()
            .map(|&(part, percent)| LineInput {
                pct: Pct::from_thousandths(percent * 1000),
                part,
            })
            .collect(),
        lead_time_hours: None,
        notes: String::new(),
    }
}

struct Kitchen {
    bun: ProductId,
    filling: RecipeId,
    filling_product: ProductId,
    dough: RecipeId,
}

async fn product(conn: &mut PgConnection, name: &str) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
         VALUES ($1, 300, 9, 100, 100) RETURNING id",
    )
    .bind(name)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    ProductId::new(id)
}

/// A cinnamon bun: 100 flour, 60 water, 40 filling (60 sugar, 40 butter). The filling is
/// also sold on its own, as a jar.
async fn kitchen(conn: &mut PgConnection) -> Kitchen {
    add_baker(conn).await;
    let flour = ingredient(conn, "tarwebloem", &[GLUTEN]).await;
    let water = ingredient(conn, "water", &[]).await;
    let sugar = ingredient(conn, "suiker", &[]).await;
    let butter = ingredient(conn, "boter", &[Allergen::Milk]).await;
    let recipe = |name: &str, kind| RecipeInput {
        name: name.to_owned(),
        kind,
    };
    let (filling, filling_v1) = store::create(conn, &recipe("Kaneelvulling", RecipeKind::Filling))
        .await
        .unwrap();
    store::save_draft(
        conn,
        filling_v1.id,
        &draft(&[
            (LinePart::Ingredient(sugar), 60),
            (LinePart::Ingredient(butter), 40),
        ]),
    )
    .await
    .unwrap();
    let (dough, dough_v1) = store::create(conn, &recipe("Kaneelbol", RecipeKind::Dough))
        .await
        .unwrap();
    store::save_draft(
        conn,
        dough_v1.id,
        &draft(&[
            (LinePart::Ingredient(flour), 100),
            (LinePart::Ingredient(water), 60),
            (LinePart::Sub(filling_v1.id), 40),
        ]),
    )
    .await
    .unwrap();
    let bun = product(conn, "Kaneelbol").await;
    store::link_product(conn, dough.id, bun).await.unwrap();
    let filling_product = product(conn, "Kaneelvulling in pot").await;
    store::link_product(conn, filling.id, filling_product)
        .await
        .unwrap();
    Kitchen {
        bun,
        filling: filling.id,
        filling_product,
        dough: dough.id,
    }
}

fn clock() -> FakeClock {
    FakeClock::new("2026-12-01T10:00:00Z".parse().unwrap())
}

async fn approve(conn: &mut PgConnection, product: ProductId) -> ComputedAllergens {
    let computed = rollup::for_product(conn, product)
        .await
        .unwrap()
        .unwrap()
        .computed
        .unwrap();
    let revision = products::get(conn, product)
        .await
        .unwrap()
        .allergen_revision;
    rollup::approve_product(conn, product, &computed, revision, BAKER, &clock())
        .await
        .unwrap();
    computed
}

async fn approved(conn: &mut PgConnection, product: ProductId) -> bool {
    products::get(conn, product)
        .await
        .unwrap()
        .approval
        .is_some()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_filling_recipe_shows_as_compound_by_default(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let computed = approve(&mut conn, k.bun).await;
    assert_eq!(
        computed.ingredients.text(),
        "tarwebloem, water, Kaneelvulling (suiker, boter)"
    );
    let stored = products::get(&mut conn, k.bun).await.unwrap();
    assert_eq!(stored.ingredient_list, Some(computed.ingredients));
    assert_eq!(
        stored.allergen_info.ingredient_text,
        "tarwebloem, water, Kaneelvulling (suiker, boter)"
    );
    let jar = approve(&mut conn, k.filling_product).await;
    assert_eq!(
        jar.ingredients.text(),
        "suiker, boter",
        "at the top the setting does not count"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn changing_the_compound_setting_clears_the_approval_of_products_using_it(
    mut conn: PoolConnection<Postgres>,
) {
    let k = kitchen(&mut conn).await;
    approve(&mut conn, k.bun).await;
    approve(&mut conn, k.filling_product).await;

    let named = store::set_label(
        &mut conn,
        k.filling,
        &LabelSetting {
            compound: None,
            name: " vulling ".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(named.compound_name(), Some("vulling"));
    assert!(!approved(&mut conn, k.bun).await);
    assert!(
        approved(&mut conn, k.filling_product).await,
        "its own products keep their approval"
    );
    let bun = products::get(&mut conn, k.bun).await.unwrap();
    assert_eq!(bun.ingredient_list, None);
    assert_eq!(bun.approval_cleared, Some(catalog::ApprovalCleared::Recipe));
    let computed = approve(&mut conn, k.bun).await;
    assert_eq!(
        computed.ingredients.text(),
        "tarwebloem, water, vulling (suiker, boter)"
    );

    store::set_label(
        &mut conn,
        k.filling,
        &LabelSetting {
            compound: Some(false),
            name: "vulling".to_owned(),
        },
    )
    .await
    .unwrap();
    let cleared = products::get(&mut conn, k.bun).await.unwrap();
    assert_eq!(cleared.approval, None);
    assert_eq!(
        cleared.approval_cleared,
        Some(catalog::ApprovalCleared::Recipe)
    );
    let merged = approve(&mut conn, k.bun).await;
    assert_eq!(
        merged.ingredients.text(),
        "tarwebloem, water, suiker, boter"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_a_change_in_what_the_list_shows_clears_the_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let k = kitchen(&mut conn).await;
    approve(&mut conn, k.bun).await;
    let filling = store::get(&mut conn, k.filling).await.unwrap();

    // The same effective setting, stated explicitly.
    store::set_label(
        &mut conn,
        k.filling,
        &LabelSetting {
            compound: Some(true),
            name: String::new(),
        },
    )
    .await
    .unwrap();
    assert!(approved(&mut conn, k.bun).await);

    // The name feeds the list while the label name is empty.
    store::update(
        &mut conn,
        k.filling,
        &RecipeInput {
            name: "Kaneelvulling 2".to_owned(),
            kind: filling.kind,
        },
    )
    .await
    .unwrap();
    assert!(!approved(&mut conn, k.bun).await);
    approve(&mut conn, k.bun).await;

    store::set_label(
        &mut conn,
        k.filling,
        &LabelSetting {
            compound: Some(true),
            name: "vulling".to_owned(),
        },
    )
    .await
    .unwrap();
    approve(&mut conn, k.bun).await;
    store::update(
        &mut conn,
        k.filling,
        &RecipeInput {
            name: "Kaneelvulling 3".to_owned(),
            kind: filling.kind,
        },
    )
    .await
    .unwrap();
    assert!(
        approved(&mut conn, k.bun).await,
        "a rename does not change the label name"
    );

    // Changing the dough recipe's own setting changes nothing for its products.
    store::set_label(
        &mut conn,
        k.dough,
        &LabelSetting {
            compound: Some(true),
            name: "deeg".to_owned(),
        },
    )
    .await
    .unwrap();
    assert!(approved(&mut conn, k.bun).await);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_kind_sets_the_default_and_counts_only_through_it(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let input = |kind| RecipeInput {
        name: "Kaneelvulling".to_owned(),
        kind,
    };
    store::update(&mut conn, k.filling, &input(RecipeKind::Topping))
        .await
        .unwrap();
    approve(&mut conn, k.bun).await;
    store::update(&mut conn, k.filling, &input(RecipeKind::Filling))
        .await
        .unwrap();
    assert!(approved(&mut conn, k.bun).await, "still compound");
    store::update(&mut conn, k.filling, &input(RecipeKind::Soaker))
        .await
        .unwrap();
    assert!(!approved(&mut conn, k.bun).await, "now merged");
    let merged = approve(&mut conn, k.bun).await;
    assert_eq!(
        merged.ingredients.text(),
        "tarwebloem, water, suiker, boter"
    );
    store::update(&mut conn, k.filling, &input(RecipeKind::Preferment))
        .await
        .unwrap();
    assert!(approved(&mut conn, k.bun).await, "merged either way");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_label_name_is_at_most_80_characters(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let long = LabelSetting {
        compound: Some(true),
        name: "v".repeat(81),
    };
    assert!(matches!(
        store::set_label(&mut conn, k.filling, &long).await,
        Err(recipes::Error::Invalid("label_name"))
    ));
    let fits = LabelSetting {
        compound: Some(true),
        name: "ü".repeat(80),
    };
    store::set_label(&mut conn, k.filling, &fits).await.unwrap();
    let missing = store::set_label(&mut conn, RecipeId::new(999_999), &fits).await;
    assert!(matches!(missing, Err(recipes::Error::NotFound)));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_stored_list_keeps_its_structure(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    approve(&mut conn, k.bun).await;
    let (names, emphasis, depth): (Vec<String>, Vec<bool>, Vec<i16>) = sqlx::query_as(
        "SELECT ingredient_names, ingredient_emphasis, ingredient_depth FROM product WHERE id = $1",
    )
    .bind(k.bun.get())
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(
        names,
        ["tarwebloem", "water", "Kaneelvulling", "suiker", "boter"]
    );
    assert_eq!(emphasis, [true, false, false, false, true]);
    assert_eq!(depth, [0, 0, 0, 1, 1]);
    let refused = sqlx::query("UPDATE product SET ingredient_depth = '{0,0}' WHERE id = $1")
        .bind(k.bun.get())
        .execute(&mut *conn)
        .await;
    assert!(refused.is_err(), "one depth per entry");
    sqlx::query("UPDATE product SET ingredient_depth = '{0,0,0,0,0}' WHERE id = $1")
        .bind(k.bun.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(
        !approved(&mut conn, k.bun).await,
        "another structure clears the approval"
    );
}
