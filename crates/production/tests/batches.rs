//! Batches against the database (F14a): split, copy and freeze, adjustments, lock, actuals.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use bakedays::{BakeDayProduct, ExtraInput, NewBakeDay, Purpose, days, extras, range};
use domain::{BakeDayId, IngredientId, PickupLocationId, ProductId, RecipeId, StaffUserId};
use orders::Orders;
use orders::audit::{self, Actor};
use production::Error;
use production::batches::{self, Actual, Adjustment, LineRef, Origin, SkipReason, Status};
use recipes::recipes::{self as store};
use recipes::{
    DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, Pct, RecipeInput, RecipeKind,
    ingredient_totals, ingredients,
};
use sqlx::{PgConnection, PgPool};
use testkit::FakeClock;

const DE_PROEF: PickupLocationId = PickupLocationId::new(1);
const BAKER: Actor = Actor::Staff(StaffUserId::new(3));

fn clock() -> FakeClock {
    FakeClock::new("2026-12-01T10:00:00Z".parse().unwrap())
}

fn pct(percent: u32) -> Pct {
    Pct::from_thousandths(percent * 1000)
}

struct Kitchen {
    day: BakeDayId,
    bread: ProductId,
    recipe: RecipeId,
    version: domain::RecipeVersionId,
    flour: IngredientId,
    water: IngredientId,
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

/// Saturday 5 December 2026 with a bread (900 g dough) from a 100/75/2 recipe.
async fn kitchen(conn: &mut PgConnection) -> Kitchen {
    let day = days::create(
        conn,
        &NewBakeDay {
            date: jiff::civil::date(2026, 12, 5),
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
    store::save_draft(
        conn,
        version.id,
        &DraftInput {
            lines: vec![
                LineInput {
                    pct: pct(100),
                    part: LinePart::Ingredient(flour),
                },
                LineInput {
                    pct: pct(75),
                    part: LinePart::Ingredient(water),
                },
                LineInput {
                    pct: pct(2),
                    part: LinePart::Ingredient(salt),
                },
            ],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    let bread = product(conn, "Desem wit", 900).await;
    store::link_product(conn, recipe.id, bread).await.unwrap();
    Kitchen {
        day,
        bread,
        recipe: recipe.id,
        version: version.id,
        flour,
        water,
    }
}

async fn extra(conn: &mut PgConnection, day: BakeDayId, product: ProductId, qty: u32) {
    range::set_product(
        conn,
        day,
        &BakeDayProduct {
            product_id: product,
            max_qty: None,
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await
    .unwrap();
    extras::add(
        conn,
        day,
        product,
        &ExtraInput {
            qty,
            purpose: Purpose::Freezer,
            note: None,
        },
        &Orders::new(&clock()),
    )
    .await
    .unwrap();
}

async fn set_capacity(conn: &mut PgConnection, grams: Option<u32>) {
    batches::set_capacity(conn, grams, BAKER, &clock())
        .await
        .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_a_mixer_capacity_only_manual_batches_work(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    extra(&mut conn, k.day, k.bread, 10).await;
    assert_eq!(batches::capacity(&mut conn).await.unwrap(), None);
    let refused = batches::make_batches(&mut conn, k.day, BAKER, &clock()).await;
    assert!(matches!(refused, Err(Error::NoCapacity)), "{refused:?}");
    let refused = batches::split_at_deadline(&mut conn, k.day, &clock()).await;
    assert!(matches!(refused, Err(Error::NoCapacity)), "{refused:?}");
    let split_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM batch_split")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(split_rows, 0, "a refused split is tried again later");

    let id = batches::start_manual(&mut conn, k.day, k.bread, 40, BAKER, &clock())
        .await
        .unwrap();
    let batch = batches::get(&mut conn, id).await.unwrap();
    assert_eq!(batch.origin, Origin::Manual);
    assert_eq!(
        batch.dough_g(),
        36_000,
        "a manual batch may exceed any capacity"
    );
    assert_eq!(batch.code.to_string(), "261205-1");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_split_copies_the_version_and_freezes_it(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    extra(&mut conn, k.day, k.bread, 25).await;
    set_capacity(&mut conn, Some(10_000)).await;
    let report = batches::make_batches(&mut conn, k.day, BAKER, &clock())
        .await
        .unwrap();
    assert_eq!(report.created.len(), 3);
    assert!(report.skipped.is_empty());

    let version = store::version(&mut conn, k.recipe, k.version)
        .await
        .unwrap();
    assert!(version.is_frozen(), "the first batch freezes the version");

    let list = batches::list(&mut conn, k.day).await.unwrap();
    let codes: Vec<String> = list.iter().map(|b| b.code.to_string()).collect();
    assert_eq!(codes, ["261205-1", "261205-2", "261205-3"]);
    let pieces: Vec<u32> = list.iter().map(|b| b.outputs[0].planned_pieces).collect();
    assert_eq!(pieces, [9, 8, 8]);
    for batch in &list {
        assert_eq!(batch.status, Status::Planned);
        assert_eq!(batch.origin, Origin::Cutoff);
        assert_eq!(batch.recipe_version_id, k.version);
        let pcts: Vec<(Option<Pct>, Pct)> = batch
            .lines
            .iter()
            .map(|l| (l.base_pct, l.actual_pct))
            .collect();
        assert_eq!(
            pcts,
            [
                (Some(pct(100)), pct(100)),
                (Some(pct(75)), pct(75)),
                (Some(pct(2)), pct(2))
            ]
        );
        assert!(batch.lines.iter().all(|l| !l.is_changed()));
        assert!(batch.dough_g() <= 10_000);
    }

    let formulas = batches::formulas_for_day(&mut conn, k.day).await.unwrap();
    assert_eq!(formulas.len(), 3);
    assert!(
        formulas
            .iter()
            .all(|f| f.recipe_name == "Desembrood" && f.outputs[0].product_id == k.bread)
    );
    let total: u64 = formulas
        .iter()
        .map(|f| {
            let scaled = f.formula.scale(f.dough_g).unwrap();
            ingredient_totals(&scaled)
                .iter()
                .map(|(_, g)| g)
                .sum::<u64>()
        })
        .sum();
    assert_eq!(total, 25 * 900, "batch lines add up to the dough to make");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn making_batches_again_only_adds_what_is_new(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    extra(&mut conn, k.day, k.bread, 10).await;
    set_capacity(&mut conn, Some(10_000)).await;
    let first = batches::make_batches(&mut conn, k.day, BAKER, &clock())
        .await
        .unwrap();
    assert_eq!(first.created.len(), 1);
    let again = batches::make_batches(&mut conn, k.day, BAKER, &clock())
        .await
        .unwrap();
    assert!(again.created.is_empty());
    extra(&mut conn, k.day, k.bread, 4).await;
    let more = batches::make_batches(&mut conn, k.day, BAKER, &clock())
        .await
        .unwrap();
    assert_eq!(more.created.len(), 1);
    let added = batches::get(&mut conn, more.created[0]).await.unwrap();
    assert_eq!(added.outputs[0].planned_pieces, 4);
    assert_eq!(added.code.mix_no, 2);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_deadline_split_runs_once(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    extra(&mut conn, k.day, k.bread, 10).await;
    set_capacity(&mut conn, Some(10_000)).await;
    let first = batches::split_at_deadline(&mut conn, k.day, &clock())
        .await
        .unwrap();
    assert_eq!(first.created.len(), 1);
    assert!(!first.already_split);
    let second = batches::split_at_deadline(&mut conn, k.day, &clock())
        .await
        .unwrap();
    assert!(second.already_split);
    assert!(second.created.is_empty());
    let history = audit::history(&mut conn, "batch", first.created[0].get())
        .await
        .unwrap();
    assert_eq!(history[0].action, "create");
    assert_eq!(history[0].actor, "system:batch-split");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn products_that_cannot_be_split_are_reported(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let granola = product(&mut conn, "Granola", 0).await;
    let cookie = product(&mut conn, "Koek", 0).await;
    store::link_product(&mut conn, k.recipe, cookie)
        .await
        .unwrap();
    let (empty, _) = store::create(
        &mut conn,
        &RecipeInput {
            name: "Leeg".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    let rye = product(&mut conn, "Rogge", 900).await;
    store::link_product(&mut conn, empty.id, rye).await.unwrap();
    for p in [k.bread, granola, cookie, rye] {
        extra(&mut conn, k.day, p, 2).await;
    }
    set_capacity(&mut conn, Some(800)).await;
    let report = batches::make_batches(&mut conn, k.day, BAKER, &clock())
        .await
        .unwrap();
    assert!(report.created.is_empty());
    let mut reasons: Vec<(String, SkipReason)> = report
        .skipped
        .into_iter()
        .map(|s| (s.name, s.reason))
        .collect();
    reasons.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        reasons,
        [
            ("Desem wit".to_owned(), SkipReason::TooHeavy),
            ("Granola".to_owned(), SkipReason::NoRecipe),
            ("Koek".to_owned(), SkipReason::NoDoughWeight),
            ("Rogge".to_owned(), SkipReason::EmptyRecipe),
        ]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn adjustments_are_audited_with_their_reason(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = batches::start_manual(&mut conn, k.day, k.bread, 10, BAKER, &clock())
        .await
        .unwrap();
    let batch = batches::get(&mut conn, id).await.unwrap();
    let water = batch.lines[1].id;
    let flour = batch.lines[0].id;
    batches::adjust(
        &mut conn,
        id,
        &Adjustment {
            lines: vec![(flour, pct(100)), (water, pct(72))],
            pieces: vec![(k.bread, 12)],
            reason: Some(" bloem neemt minder op ".to_owned()),
        },
        BAKER,
        &clock(),
    )
    .await
    .unwrap();
    let batch = batches::get(&mut conn, id).await.unwrap();
    assert_eq!(batch.status, Status::Adjusted);
    assert_eq!(batch.lines[1].actual_pct, pct(72));
    assert_eq!(batch.lines[1].base_pct, Some(pct(75)));
    assert!(batch.lines[1].is_changed());
    assert_eq!(
        batch.lines[1].reason.as_deref(),
        Some("bloem neemt minder op")
    );
    assert!(!batch.lines[0].is_changed());
    assert_eq!(
        batch.lines[0].reason, None,
        "an unchanged line keeps no reason"
    );
    assert_eq!(batch.dough_g(), 12 * 900);

    let history = audit::history(&mut conn, "batch", id.get()).await.unwrap();
    let actions: Vec<&str> = history.iter().map(|h| h.action.as_str()).collect();
    assert_eq!(actions, ["create", "line", "pieces"]);
    let line = &history[1];
    assert_eq!(line.actor, "staff:3");
    assert_eq!(line.before.as_ref().unwrap()["pct_x1000"], 75_000);
    assert_eq!(line.after.as_ref().unwrap()["pct_x1000"], 72_000);
    assert_eq!(
        line.after.as_ref().unwrap()["reason"],
        "bloem neemt minder op"
    );

    batches::add_line(&mut conn, id, k.flour, pct(5), None, BAKER, &clock())
        .await
        .unwrap();
    let batch = batches::get(&mut conn, id).await.unwrap();
    let added = batch.lines.last().unwrap();
    assert_eq!(added.base_pct, None);
    assert!(added.is_changed());
    assert!(matches!(&added.part, LineRef::Ingredient(i) if i.id == k.flour));
    let refused = batches::add_line(&mut conn, id, k.water, pct(0), None, BAKER, &clock()).await;
    assert!(matches!(refused, Err(Error::Invalid("pct"))), "{refused:?}");
    let refused = batches::adjust(
        &mut conn,
        id,
        &Adjustment {
            pieces: vec![(ProductId::new(999), 1)],
            ..Adjustment::default()
        },
        BAKER,
        &clock(),
    )
    .await;
    assert!(
        matches!(refused, Err(Error::Invalid("product"))),
        "{refused:?}"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_mixed_batch_refuses_every_edit(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = batches::start_manual(&mut conn, k.day, k.bread, 10, BAKER, &clock())
        .await
        .unwrap();
    batches::mark_mixed(&mut conn, id, BAKER, &clock())
        .await
        .unwrap();
    let batch = batches::get(&mut conn, id).await.unwrap();
    assert_eq!(batch.status, Status::Mixed);
    assert!(batch.mixed_at.is_some());
    let line = batch.lines[1].id;

    let refused = batches::adjust(
        &mut conn,
        id,
        &Adjustment {
            lines: vec![(line, pct(70))],
            ..Adjustment::default()
        },
        BAKER,
        &clock(),
    )
    .await;
    assert!(matches!(refused, Err(Error::Locked)), "{refused:?}");
    let refused = batches::add_line(&mut conn, id, k.flour, pct(5), None, BAKER, &clock()).await;
    assert!(matches!(refused, Err(Error::Locked)), "{refused:?}");
    let refused = batches::mark_mixed(&mut conn, id, BAKER, &clock()).await;
    assert!(matches!(refused, Err(Error::Locked)), "{refused:?}");

    for statement in [
        "UPDATE batch_line SET actual_pct_x1000 = 1 WHERE batch_id = $1",
        "DELETE FROM batch_line WHERE batch_id = $1",
        "INSERT INTO batch_line (batch_id, position, ingredient_id, actual_pct_x1000)
            SELECT $1, 99, ingredient_id, 1 FROM batch_line WHERE batch_id = $1 LIMIT 1",
        "UPDATE batch_output SET planned_pieces = 1 WHERE batch_id = $1",
        "UPDATE batch SET status = 'planned', mixed_at = NULL WHERE id = $1",
        "UPDATE batch SET recipe_version_id = recipe_version_id + 0, mix_no = 9 WHERE id = $1",
        "DELETE FROM batch WHERE id = $1",
    ] {
        let result = sqlx::query(statement)
            .bind(id.get())
            .execute(&mut *conn)
            .await;
        assert!(result.is_err(), "{statement}");
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn actual_pieces_and_waste_come_after_mixing(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = batches::start_manual(&mut conn, k.day, k.bread, 10, BAKER, &clock())
        .await
        .unwrap();
    let actual = |pieces, waste| {
        [Actual {
            product_id: k.bread,
            pieces,
            waste,
        }]
    };
    let refused = batches::record_actuals(&mut conn, id, &actual(10, 0), BAKER, &clock()).await;
    assert!(matches!(refused, Err(Error::NotMixed)), "{refused:?}");
    let early = sqlx::query(
        "UPDATE batch_output SET actual_pieces = 1, waste_pieces = 0 WHERE batch_id = $1",
    )
    .bind(id.get())
    .execute(&mut *conn)
    .await;
    assert!(early.is_err(), "the database refuses actuals before mixing");

    batches::mark_mixed(&mut conn, id, BAKER, &clock())
        .await
        .unwrap();
    let refused = batches::record_actuals(&mut conn, id, &actual(9, 10), BAKER, &clock()).await;
    assert!(
        matches!(refused, Err(Error::Invalid("waste"))),
        "{refused:?}"
    );
    let refused = batches::record_actuals(&mut conn, id, &[], BAKER, &clock()).await;
    assert!(
        matches!(refused, Err(Error::Invalid("product"))),
        "{refused:?}"
    );

    batches::record_actuals(&mut conn, id, &actual(10, 1), BAKER, &clock())
        .await
        .unwrap();
    let batch = batches::get(&mut conn, id).await.unwrap();
    assert_eq!(batch.status, Status::Baked);
    let baked_at = batch.baked_at.expect("baked");
    assert_eq!(batch.outputs[0].actual_pieces, Some(10));
    assert_eq!(batch.outputs[0].waste_pieces, Some(1));

    let later = FakeClock::new("2026-12-05T12:00:00Z".parse().unwrap());
    batches::record_actuals(&mut conn, id, &actual(9, 2), BAKER, &later)
        .await
        .unwrap();
    let batch = batches::get(&mut conn, id).await.unwrap();
    assert_eq!(batch.outputs[0].actual_pieces, Some(9));
    assert_eq!(batch.baked_at, Some(baked_at), "baked keeps its first time");
    let history = audit::history(&mut conn, "batch", id.get()).await.unwrap();
    let actions: Vec<&str> = history.iter().map(|h| h.action.as_str()).collect();
    assert_eq!(actions, ["create", "mixed", "actuals", "actuals"]);
    assert_eq!(history[3].before.as_ref().unwrap()[0]["pieces"], 10);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn manual_batches_need_a_recipe_and_pieces(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let granola = product(&mut conn, "Granola", 500).await;
    let refused = batches::start_manual(&mut conn, k.day, granola, 5, BAKER, &clock()).await;
    assert!(
        matches!(refused, Err(Error::Invalid("recipe"))),
        "{refused:?}"
    );
    let refused = batches::start_manual(&mut conn, k.day, k.bread, 0, BAKER, &clock()).await;
    assert!(
        matches!(refused, Err(Error::Invalid("pieces"))),
        "{refused:?}"
    );
    let refused =
        batches::start_manual(&mut conn, k.day, ProductId::new(999), 5, BAKER, &clock()).await;
    assert!(matches!(refused, Err(Error::NotFound)), "{refused:?}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_capacity_setting_is_checked_and_audited(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let refused = batches::set_capacity(&mut conn, Some(0), BAKER, &clock()).await;
    assert!(
        matches!(refused, Err(Error::Invalid("capacity"))),
        "{refused:?}"
    );
    set_capacity(&mut conn, Some(12_500)).await;
    assert_eq!(batches::capacity(&mut conn).await.unwrap(), Some(12_500));
    set_capacity(&mut conn, None).await;
    assert_eq!(batches::capacity(&mut conn).await.unwrap(), None);
    let history = audit::history(&mut conn, "company", 1).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0].after.as_ref().unwrap()["mixer_capacity_g"],
        12_500
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn paid_orders_count_and_pending_ones_do_not(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let clock = clock();
    let day = days::get(&mut conn, k.day).await.unwrap();
    days::update_schedule(
        &mut conn,
        k.day,
        &bakedays::Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some("2026-11-20T00:00:00Z".parse().unwrap()),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role)
            VALUES ('test-baker', 'Testbakker', 'baker') RETURNING id)
        UPDATE product SET allergens_approved_by = (SELECT id FROM baker),
            allergens_approved_at = '2026-11-01T10:00:00Z' WHERE id = $1",
    )
    .bind(k.bread.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    extra(&mut conn, k.day, k.bread, 1).await;
    days::open(&mut conn, &clock, k.day).await.unwrap();
    let slot = days::list_slots(&mut conn, k.day).await.unwrap()[0].id;
    let mut placed = Vec::new();
    for (name, qty) in [("Sanne", 3), ("Joris", 5)] {
        let mut cart = orders::Cart::new(k.day);
        cart.set(k.bread, qty, None).unwrap();
        let customer = orders::CustomerInput {
            email: format!("{}@example.org", name.to_lowercase()),
            name: name.to_owned(),
            phone: None,
            marketing_opt_in: false,
        };
        placed.push(
            orders::checkout(
                &mut conn,
                &clock,
                &cart,
                &customer,
                slot,
                orders::OrderedBy::Guest,
            )
            .await
            .unwrap(),
        );
    }
    let paid = &placed[0];
    orders::mark_paid(
        &mut conn,
        &clock,
        paid.id,
        &orders::PaymentReceived {
            reference: "tr_1".to_owned(),
            amount: paid.total,
        },
    )
    .await
    .unwrap();
    set_capacity(&mut conn, Some(50_000)).await;
    let report = batches::make_batches(&mut conn, k.day, BAKER, &clock)
        .await
        .unwrap();
    let batch = batches::get(&mut conn, report.created[0]).await.unwrap();
    assert_eq!(batch.outputs[0].planned_pieces, 4, "3 paid plus 1 extra");
}
