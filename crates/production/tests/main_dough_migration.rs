//! The main-dough migration (F14, ADR 0053): old-way versions and their batches keep their
//! grams, new-way drafts keep their numbers, and migrating again does not run it twice.
#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::collections::BTreeSet;

use bakedays::{NewBakeDay, days};
use domain::{BatchId, IngredientId, PickupLocationId, ProductId, RecipeVersionId, StaffUserId};
use orders::audit::Actor;
use production::batches::{self, Adjustment};
use recipes::recipes::{self as store};
use recipes::{
    Base, DraftInput, Formula, IngredientInput, IngredientRole, LineInput, LinePart, Part, Pct,
    RecipeInput, RecipeKind, Scaled, ingredients,
};
use sqlx::migrate::Migrator;
use sqlx::{PgConnection, PgPool};
use testkit::FakeClock;

const MIGRATION: i64 = 20_261_208_000_400;
const BAKER: Actor = Actor::Staff(StaffUserId::new(3));
const WEIGHTS: [u64; 4] = [1, 777, 9_000, 123_457];

fn clock() -> FakeClock {
    FakeClock::new("2026-12-01T10:00:00Z".parse().unwrap())
}

fn pct(percent: u32) -> Pct {
    Pct::from_thousandths(percent * 1000)
}

async fn ingredient(
    conn: &mut PgConnection,
    name: &str,
    role: IngredientRole,
    hydration: Option<Pct>,
) -> IngredientId {
    ingredients::create(
        conn,
        &IngredientInput {
            name: name.to_owned(),
            label_name: String::new(),
            role,
            hydration,
            supplier_name: String::new(),
            allergens: BTreeSet::new(),
        },
    )
    .await
    .unwrap()
    .id
}

async fn recipe(
    conn: &mut PgConnection,
    name: &str,
    kind: RecipeKind,
    lines: Vec<(LinePart, u32)>,
) -> (domain::RecipeId, RecipeVersionId) {
    let lines = lines
        .into_iter()
        .map(|(part, percent)| (part, percent * 1000))
        .collect();
    recipe_x1000(conn, name, kind, lines).await
}

/// Like [`recipe`], with the percentages in thousandths.
async fn recipe_x1000(
    conn: &mut PgConnection,
    name: &str,
    kind: RecipeKind,
    lines: Vec<(LinePart, u32)>,
) -> (domain::RecipeId, RecipeVersionId) {
    let (recipe, version) = store::create(
        conn,
        &RecipeInput {
            name: name.to_owned(),
            kind,
        },
    )
    .await
    .unwrap();
    store::save_draft(
        conn,
        version.id,
        &DraftInput {
            lines: lines
                .into_iter()
                .map(|(part, thousandths)| LineInput {
                    pct: Pct::from_thousandths(thousandths),
                    part,
                })
                .collect(),
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    (recipe.id, version.id)
}

async fn product(conn: &mut PgConnection, name: &str) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online)
         VALUES ($1, 450, 9, 800, 900, true) RETURNING id",
    )
    .bind(name)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

/// Every leaf's grams in order, at each of [`WEIGHTS`].
fn grams(formula: &Formula) -> Vec<Vec<u64>> {
    fn leaves(scaled: &[Scaled<'_>], out: &mut Vec<u64>) {
        for line in scaled {
            match &line.line.part {
                Part::Ingredient(_) => out.push(line.grams),
                Part::Sub(_) => leaves(&line.lines, out),
            }
        }
    }
    WEIGHTS
        .iter()
        .map(|&weight| {
            let mut out = Vec::new();
            leaves(&formula.scale(weight).unwrap(), &mut out);
            out
        })
        .collect()
}

fn thousandths(formula: &Formula) -> Vec<u32> {
    formula.lines.iter().map(|l| l.pct.thousandths()).collect()
}

/// Leaf grams before and after differ by at most `slack` grams.
fn assert_close(before: &[Vec<u64>], after: &[Vec<u64>], slack: u64, what: &str) {
    for ((weight, old), new) in WEIGHTS.iter().zip(before).zip(after) {
        assert_eq!(old.len(), new.len());
        for (a, b) in old.iter().zip(new) {
            assert!(
                a.abs_diff(*b) <= slack,
                "{what} at {weight} g: {old:?} became {new:?}"
            );
        }
    }
}

struct Snapshot {
    versions: Vec<Vec<u32>>,
    version_grams: Vec<Vec<Vec<u64>>>,
    batch_lines: Vec<Vec<(Option<Pct>, Pct)>>,
    batch_grams: Vec<Vec<Vec<u64>>>,
    batch_dough: Vec<u64>,
    allergen_revs: Vec<i64>,
}

async fn snapshot(
    conn: &mut PgConnection,
    versions: &[RecipeVersionId],
    batch_ids: &[BatchId],
) -> Snapshot {
    let mut s = Snapshot {
        versions: Vec::new(),
        version_grams: Vec::new(),
        batch_lines: Vec::new(),
        batch_grams: Vec::new(),
        batch_dough: Vec::new(),
        allergen_revs: Vec::new(),
    };
    for &id in versions {
        let formula = store::formula(conn, id).await.unwrap();
        s.versions.push(thousandths(&formula));
        s.version_grams.push(grams(&formula));
    }
    for &id in batch_ids {
        let batch = batches::get(conn, id).await.unwrap();
        s.batch_lines.push(
            batch
                .lines
                .iter()
                .map(|l| (l.base_pct, l.actual_pct))
                .collect(),
        );
        let formula = batches::formula(conn, &batch).await.unwrap();
        s.batch_grams.push(grams(&formula));
        s.batch_dough.push(batch.dough_g());
    }
    s.allergen_revs = sqlx::query_scalar("SELECT allergens_recipe_rev FROM product ORDER BY id")
        .fetch_all(conn)
        .await
        .unwrap();
    s
}

#[sqlx::test(migrations = false)]
async fn old_way_versions_and_batches_keep_their_grams_and_new_way_drafts_their_numbers(
    pool: PgPool,
) {
    let before_migration = Migrator {
        migrations: Cow::Owned(
            db::MIGRATOR
                .iter()
                .filter(|m| m.version < MIGRATION)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    before_migration.run(&pool).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let clock = clock();

    let wheat = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour, None).await;
    let spelt = ingredient(&mut conn, "Speltmeel", IngredientRole::Flour, None).await;
    let rye = ingredient(&mut conn, "Roggemeel", IngredientRole::Flour, None).await;
    let water = ingredient(&mut conn, "Water", IngredientRole::Water, None).await;
    let salt = ingredient(&mut conn, "Zeezout", IngredientRole::Other, None).await;
    let seeds = ingredient(&mut conn, "Lijnzaad", IngredientRole::Other, None).await;
    let desem = ingredient(&mut conn, "Desem", IngredientRole::Starter, Some(pct(100))).await;
    let stiff = ingredient(
        &mut conn,
        "Desem stijf",
        IngredientRole::Starter,
        Some(pct(50)),
    )
    .await;
    let i = LinePart::Ingredient;

    let (levain_recipe, levain) = recipe(
        &mut conn,
        "Levain",
        RecipeKind::Preferment,
        vec![(i(wheat), 100), (i(water), 100)],
    )
    .await;
    store::freeze(&mut conn, levain_recipe, levain, &clock)
        .await
        .unwrap();
    // Written against all flour: own 80% + 10% from the starter + 10% from the levain.
    let (bread_recipe, bread) = recipe(
        &mut conn,
        "Desembrood",
        RecipeKind::Dough,
        vec![
            (i(wheat), 80),
            (i(water), 65),
            (i(salt), 2),
            (i(desem), 20),
            (LinePart::Sub(levain), 20),
        ],
    )
    .await;
    let loaf = product(&mut conn, "Desem wit").await;
    store::link_product(&mut conn, bread_recipe, loaf)
        .await
        .unwrap();
    let (straight_recipe, straight) = recipe(
        &mut conn,
        "Wit",
        RecipeKind::Dough,
        vec![(i(wheat), 100), (i(water), 75), (i(salt), 2)],
    )
    .await;
    store::freeze(&mut conn, straight_recipe, straight, &clock)
        .await
        .unwrap();
    let (levain_only_recipe, levain_only) = recipe(
        &mut conn,
        "Levaindeeg",
        RecipeKind::Dough,
        vec![(LinePart::Sub(levain), 200), (i(water), 50), (i(salt), 2)],
    )
    .await;
    store::freeze(&mut conn, levain_only_recipe, levain_only, &clock)
        .await
        .unwrap();
    // The owner's draft, typed the new way: own flours 100% plus 20% stiff starter.
    let (_, landbrood) = recipe(
        &mut conn,
        "Landbrood",
        RecipeKind::Dough,
        vec![
            (i(wheat), 70),
            (i(spelt), 15),
            (i(rye), 15),
            (i(water), 70),
            (i(salt), 2),
            (i(stiff), 20),
        ],
    )
    .await;
    // 85% own flour and 22.5% of a stiff starter (15% flour): a factor of 100/85.
    let (stiff_recipe, stiff_bread) = recipe_x1000(
        &mut conn,
        "Stijf",
        RecipeKind::Dough,
        vec![
            (i(wheat), 85_000),
            (i(water), 70_000),
            (i(salt), 2_000),
            (i(stiff), 22_500),
        ],
    )
    .await;
    store::freeze(&mut conn, stiff_recipe, stiff_bread, &clock)
        .await
        .unwrap();
    // A preferment that is migrated itself (80% own flour), inside a dough that is too.
    let (pre_recipe, pre) = recipe(
        &mut conn,
        "Voordeeg",
        RecipeKind::Preferment,
        vec![(i(wheat), 80), (LinePart::Sub(levain), 40)],
    )
    .await;
    let (nested_recipe, nested) = recipe(
        &mut conn,
        "Genest",
        RecipeKind::Dough,
        vec![(i(wheat), 90), (i(water), 60), (LinePart::Sub(pre), 12)],
    )
    .await;
    store::freeze(&mut conn, nested_recipe, nested, &clock)
        .await
        .unwrap();
    let pre_frozen = store::version(&mut conn, pre_recipe, pre).await.unwrap();
    assert!(pre_frozen.is_frozen());
    // 0.001% own flour and 10% starter: the factor 5001 would take the starter above
    // 1000%, so the version keeps its numbers.
    let (capped_recipe, capped) = recipe_x1000(
        &mut conn,
        "Bijna geen bloem",
        RecipeKind::Dough,
        vec![(i(wheat), 1), (i(water), 50_000), (i(desem), 10_000)],
    )
    .await;
    store::freeze(&mut conn, capped_recipe, capped, &clock)
        .await
        .unwrap();
    // Flour and water weigh the same: 90% + 10% from a 100:50 levain, 95% + 5%. Rounding
    // makes water heavier, so the ingredient list changes order and the approval goes.
    let (stiff_levain_recipe, stiff_levain) = recipe(
        &mut conn,
        "Levain stijf",
        RecipeKind::Preferment,
        vec![(i(wheat), 100), (i(water), 50)],
    )
    .await;
    store::freeze(&mut conn, stiff_levain_recipe, stiff_levain, &clock)
        .await
        .unwrap();
    let (tie_recipe, tie) = recipe(
        &mut conn,
        "Gelijk",
        RecipeKind::Dough,
        vec![
            (i(wheat), 90),
            (i(water), 95),
            (LinePart::Sub(stiff_levain), 15),
        ],
    )
    .await;
    let tie_loaf = product(&mut conn, "Gelijk brood").await;
    store::link_product(&mut conn, tie_recipe, tie_loaf)
        .await
        .unwrap();
    store::freeze(&mut conn, tie_recipe, tie, &clock)
        .await
        .unwrap();

    let day = days::create(
        &mut conn,
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
    let mixed = batches::start_manual(&mut conn, day, loaf, 11, BAKER, &clock)
        .await
        .unwrap();
    let lines = batches::get(&mut conn, mixed).await.unwrap().lines;
    batches::adjust(
        &mut conn,
        mixed,
        &Adjustment {
            lines: vec![
                (lines[1].id, Pct::from_thousandths(67_500)),
                (lines[2].id, Pct::default()),
            ],
            pieces: Vec::new(),
            reason: Some("natter".to_owned()),
        },
        BAKER,
        &clock,
    )
    .await
    .unwrap();
    batches::add_line(&mut conn, mixed, seeds, pct(5), None, BAKER, &clock)
        .await
        .unwrap();
    batches::mark_mixed(&mut conn, mixed, BAKER, &clock)
        .await
        .unwrap();
    let planned = batches::start_manual(&mut conn, day, loaf, 7, BAKER, &clock)
        .await
        .unwrap();

    // A draft copied from the frozen bread holds its numbers the old way.
    let copied = store::new_version(&mut conn, bread_recipe)
        .await
        .unwrap()
        .id;

    let versions = [
        bread,
        levain,
        straight,
        levain_only,
        landbrood,
        stiff_bread,
        pre,
        nested,
        capped,
        copied,
        tie,
    ];
    let batch_ids = [mixed, planned];
    let staff: i64 = sqlx::query_scalar(
        "INSERT INTO staff_user (oidc_sub, name, role) VALUES ('bakker', 'Bakker', 'baker')
         RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    for update in [
        "UPDATE product SET ingredient_names = ARRAY['x'], ingredient_emphasis = ARRAY[false]",
        "UPDATE product SET allergens_approved_by = $1, allergens_approved_at = now()",
    ] {
        sqlx::query(update)
            .bind(staff)
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    let old = snapshot(&mut conn, &versions, &batch_ids).await;
    assert_eq!(
        store::formula(&mut conn, bread)
            .await
            .unwrap()
            .analysis()
            .unwrap()
            .total_flour,
        pct(100)
    );

    db::MIGRATOR.run(&pool).await.unwrap();
    let new = snapshot(&mut conn, &versions, &batch_ids).await;

    // The bread: ×100/80 on every line.
    assert_eq!(new.versions[0], [100_000, 81_250, 2_500, 25_000, 25_000]);
    let analysis = store::formula(&mut conn, bread)
        .await
        .unwrap()
        .analysis()
        .unwrap();
    assert_eq!(analysis.base, Base::MainFlour);
    assert_eq!(analysis.base_flour(), Some(pct(100)));
    assert_eq!(analysis.total_flour, pct(125));
    assert_eq!(analysis.hydration, Some(pct(85)));
    assert_close(&old.version_grams[0], &new.version_grams[0], 1, "bread");
    // Nothing to do for the levain, the straight dough, the dough without own flour, the
    // draft typed the new way and the version that would go above 1000%.
    for index in [1, 2, 3, 4, 8] {
        assert_eq!(old.versions[index], new.versions[index], "version {index}");
        assert_eq!(old.version_grams[index], new.version_grams[index]);
    }
    for index in [0, 5, 6, 7, 9, 10] {
        assert_ne!(old.versions[index], new.versions[index], "version {index}");
        assert_close(
            &old.version_grams[index],
            &new.version_grams[index],
            1,
            &format!("version {index}"),
        );
        let analysis = store::formula(&mut conn, versions[index])
            .await
            .unwrap()
            .analysis()
            .unwrap();
        assert_eq!(
            analysis.main_flour.tenths(),
            1000,
            "version {index}: {analysis:?}"
        );
    }
    assert_eq!(
        new.versions[9], new.versions[0],
        "the copied draft follows the bread"
    );
    assert_eq!(
        new.versions[4],
        [70_000, 15_000, 15_000, 70_000, 2_000, 20_000]
    );
    // The batches follow their version, and their base still matches it.
    assert_eq!(
        new.batch_lines[0],
        [
            (Some(pct(100)), pct(100)),
            (
                Some(Pct::from_thousandths(81_250)),
                Pct::from_thousandths(84_375)
            ),
            (Some(Pct::from_thousandths(2_500)), Pct::default()),
            (Some(pct(25)), pct(25)),
            (Some(pct(25)), pct(25)),
            (None, Pct::from_thousandths(6_250)),
        ]
    );
    for (index, lines) in new.batch_lines.iter().enumerate() {
        let base: Vec<u32> = lines
            .iter()
            .filter_map(|(base, _)| base.map(Pct::thousandths))
            .collect();
        assert_eq!(base, new.versions[0], "batch {index}");
    }
    assert_eq!(old.batch_dough, new.batch_dough);
    for (index, (a, b)) in old.batch_grams.iter().zip(&new.batch_grams).enumerate() {
        assert_close(a, b, 1, &format!("batch {index}"));
    }
    assert_eq!(
        new.allergen_revs,
        [old.allergen_revs[0], old.allergen_revs[1] + 1],
        "only the product whose ingredient order changed loses its approval"
    );
    let approved: Vec<bool> =
        sqlx::query_scalar("SELECT allergens_approved_by IS NOT NULL FROM product ORDER BY id")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
    assert_eq!(approved, [true, false]);
    let frozen: bool =
        sqlx::query_scalar("SELECT frozen_at IS NOT NULL FROM recipe_version WHERE id = $1")
            .bind(bread.get())
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert!(frozen);
    let refused =
        sqlx::query("UPDATE recipe_line SET bakers_pct_x1000 = 1 WHERE recipe_version_id = $1")
            .bind(bread.get())
            .execute(&mut *conn)
            .await;
    assert!(refused.is_err(), "the frozen-line trigger is back on");
    let refused = sqlx::query("UPDATE batch_line SET actual_pct_x1000 = 1 WHERE batch_id = $1")
        .bind(mixed.get())
        .execute(&mut *conn)
        .await;
    assert!(refused.is_err(), "the mixed-batch trigger is back on");

    db::MIGRATOR.run(&pool).await.unwrap();
    let again = snapshot(&mut conn, &versions, &batch_ids).await;
    assert_eq!(again.versions, new.versions, "sqlx runs the migration once");
    assert_eq!(again.batch_lines, new.batch_lines);
}
