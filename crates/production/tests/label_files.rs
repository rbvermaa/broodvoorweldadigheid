//! Product and freezer labels of a batch (F18, F18a, ADR 0047): the F17 check first, then
//! the label settings, the freezer pieces and the dates.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use bakedays::{NewBakeDay, days};
use catalog::label_settings::{self, LabelSettings};
use catalog::{ComputedAllergens, IngredientList, ListedIngredient, company, products};
use domain::{
    Allergen, BakeDayId, BatchId, IngredientId, PickupLocationId, ProductId, StaffUserId,
};
use i18n::Translator;
use jiff::civil::date;
use orders::audit::Actor;
use production::Error;
use production::batches::{self, Actual, Adjustment};
use production::label_check::{self, LabelBlock};
use production::label_files::{self, Frozen, LabelGap, LabelKind};
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

/// Saturday 5 December 2026, 23:30 in Amsterdam (22:30 UTC).
fn late_on_bake_day() -> FakeClock {
    FakeClock::new("2026-12-05T22:30:00Z".parse().unwrap())
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
            supplier_name: String::new(),
            allergens: allergens.iter().copied().collect(),
        },
    )
    .await
    .unwrap()
    .id
}

/// A bread from 100 wheat flour, 75 water, 2 salt on 5 December 2026, its computed
/// allergens approved, milk on the approved kitchen list, an address and full label
/// settings.
async fn kitchen(conn: &mut PgConnection) -> Kitchen {
    let clock = late_on_bake_day();
    sqlx::query(
        "INSERT INTO staff_user (id, oidc_sub, name, role) OVERRIDING SYSTEM VALUE
         VALUES ($1, 'baker-fixture', 'Bakker', 'baker')",
    )
    .bind(BAKER_ID.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    company::set_kitchen_may_contain(conn, &BTreeSet::from([Allergen::Milk]))
        .await
        .unwrap();
    company::approve_kitchen_may_contain(conn, &BTreeSet::from([Allergen::Milk]), BAKER_ID, &clock)
        .await
        .unwrap();
    company::set_address(conn, "Voorbeeldlaan 1, 1234 AB Voorbeeld")
        .await
        .unwrap();
    let day = days::create(
        conn,
        &NewBakeDay {
            date: date(2026, 12, 5),
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
         VALUES ('Desem wit', 450, 9, 800, 900, false) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let bread = ProductId::new(bread);
    store::link_product(conn, recipe.id, bread).await.unwrap();
    let revision = products::get(conn, bread).await.unwrap().allergen_revision;
    rollup::approve_product(
        conn,
        bread,
        &ComputedAllergens {
            allergens: BTreeSet::from([GLUTEN]),
            ingredients: listed(&[("tarwebloem", true), ("water", false), ("zout", false)]),
        },
        revision,
        BAKER_ID,
        &clock,
    )
    .await
    .unwrap();
    label_settings::set(conn, bread, &settings()).await.unwrap();
    Kitchen { day, bread, spelt }
}

fn settings() -> LabelSettings {
    LabelSettings {
        packaged: true,
        shelf_life_days: Some(4),
        freezer_days: Some(90),
        storage: "Koel en droog bewaren.".to_owned(),
    }
}

async fn baked(conn: &mut PgConnection, k: &Kitchen, pieces: u32, waste: u32) -> BatchId {
    let clock = late_on_bake_day();
    let id = batches::start_manual(conn, k.day, k.bread, 10, BAKER, &clock)
        .await
        .unwrap();
    batches::mark_mixed(conn, id, BAKER, &clock).await.unwrap();
    batches::record_actuals(
        conn,
        id,
        &[Actual {
            product_id: k.bread,
            pieces,
            waste,
        }],
        BAKER,
        &clock,
    )
    .await
    .unwrap();
    id
}

async fn gap(conn: &mut PgConnection, id: BatchId, k: &Kitchen, kind: LabelKind) -> LabelGap {
    match label_files::label(conn, id, k.bread, kind).await {
        Err(Error::LabelGap(gap)) => gap,
        other => panic!("expected a gap, got {other:?}"),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_product_label_carries_the_checked_data(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = baked(&mut conn, &k, 10, 2).await;
    let label = label_files::label(&mut conn, id, k.bread, LabelKind::Product)
        .await
        .unwrap();
    assert_eq!(label.copies(), 8, "one per good piece");
    assert!(!label.order_changed());
    let l = label.label();
    assert_eq!(
        l.kind,
        labels::Kind::Product {
            storage: "Koel en droog bewaren.".to_owned()
        }
    );
    assert_eq!(l.product, "Desem wit");
    assert_eq!(l.best_before, date(2026, 12, 9), "bake day plus 4 days");
    assert_eq!(l.batch_code, "261205-1");
    assert_eq!(l.net_weight_g, 800);
    assert_eq!(l.may_contain, BTreeSet::from([Allergen::Milk]));
    assert_eq!(
        l.ingredients
            .iter()
            .map(|i| (i.name.as_str(), i.bold))
            .collect::<Vec<_>>(),
        [("tarwebloem", true), ("water", false), ("zout", false)]
    );
    assert_eq!(l.business.address, "Voorbeeldlaan 1, 1234 AB Voorbeeld");

    let t = Translator::dutch().unwrap();
    let files = label_files::files(&label, &t).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "261205-1-product-1.png");
    assert_eq!(files[0].png.size.pixels(), (591, 354));
    let csv = label_files::csv(&label, &files, &t).unwrap();
    assert!(
        csv.contains("261205-1-product-1.png;Productetiket;Desem wit;TARWEBLOEM, water, zout;melk;800 g;09-12-2026;;Koel en droog bewaren.;261205-1;"),
        "{csv}"
    );
    assert!(csv.ends_with(";8\r\n"), "{csv}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_blocked_batch_gets_no_label_of_either_kind(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let clock = late_on_bake_day();
    let id = batches::start_manual(&mut conn, k.day, k.bread, 10, BAKER, &clock)
        .await
        .unwrap();
    batches::add_line(&mut conn, id, k.spelt, pct(10), None, BAKER, &clock)
        .await
        .unwrap();
    batches::mark_mixed(&mut conn, id, BAKER, &clock)
        .await
        .unwrap();
    batches::record_actuals(
        &mut conn,
        id,
        &[Actual {
            product_id: k.bread,
            pieces: 10,
            waste: 0,
        }],
        BAKER,
        &clock,
    )
    .await
    .unwrap();
    label_files::record_frozen(&mut conn, id, k.bread, 4, BAKER, &clock)
        .await
        .unwrap();
    for kind in LabelKind::ALL {
        assert!(
            matches!(
                label_files::label(&mut conn, id, k.bread, kind).await,
                Err(Error::LabelBlocked(LabelBlock::Differs { .. }))
            ),
            "{kind:?}"
        );
    }

    // The allergen check comes before anything else that is missing.
    company::set_address(&mut conn, "").await.unwrap();
    assert!(matches!(
        label_files::label(&mut conn, id, k.bread, LabelKind::Product).await,
        Err(Error::LabelBlocked(_))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_changed_order_by_weight_is_passed_on_as_a_warning(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let clock = late_on_bake_day();
    let id = batches::start_manual(&mut conn, k.day, k.bread, 10, BAKER, &clock)
        .await
        .unwrap();
    let water = batches::get(&mut conn, id).await.unwrap().lines[1].id;
    batches::adjust(
        &mut conn,
        id,
        &Adjustment {
            lines: vec![(water, pct(120))],
            pieces: vec![],
            reason: None,
        },
        BAKER,
        &clock,
    )
    .await
    .unwrap();
    let reordered = ComputedAllergens {
        allergens: BTreeSet::from([GLUTEN]),
        ingredients: listed(&[("water", false), ("tarwebloem", true), ("zout", false)]),
    };
    label_check::approve_batch(&mut conn, id, &reordered, BAKER_ID, &clock)
        .await
        .unwrap();
    batches::mark_mixed(&mut conn, id, BAKER, &clock)
        .await
        .unwrap();
    batches::record_actuals(
        &mut conn,
        id,
        &[Actual {
            product_id: k.bread,
            pieces: 10,
            waste: 0,
        }],
        BAKER,
        &clock,
    )
    .await
    .unwrap();
    let label = label_files::label(&mut conn, id, k.bread, LabelKind::Product)
        .await
        .unwrap();
    assert!(label.order_changed());
    assert_eq!(label.label().ingredients[0].name, "water");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn what_a_product_label_misses_is_named(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let clock = late_on_bake_day();
    let unbaked = batches::start_manual(&mut conn, k.day, k.bread, 10, BAKER, &clock)
        .await
        .unwrap();
    assert_eq!(
        gap(&mut conn, unbaked, &k, LabelKind::Product).await,
        LabelGap::NotBaked
    );
    let all_waste = baked(&mut conn, &k, 10, 10).await;
    assert_eq!(
        gap(&mut conn, all_waste, &k, LabelKind::Product).await,
        LabelGap::NoPieces
    );

    let id = baked(&mut conn, &k, 10, 0).await;
    for (settings, missing) in [
        (
            LabelSettings {
                packaged: false,
                ..settings()
            },
            LabelGap::NotPackaged,
        ),
        (
            LabelSettings {
                shelf_life_days: None,
                ..settings()
            },
            LabelGap::NoShelfLife,
        ),
        (
            LabelSettings {
                storage: String::new(),
                ..settings()
            },
            LabelGap::NoStorage,
        ),
    ] {
        label_settings::set(&mut conn, k.bread, &settings)
            .await
            .unwrap();
        assert_eq!(gap(&mut conn, id, &k, LabelKind::Product).await, missing);
    }
    label_settings::set(&mut conn, k.bread, &settings())
        .await
        .unwrap();
    company::set_address(&mut conn, "").await.unwrap();
    assert_eq!(
        gap(&mut conn, id, &k, LabelKind::Product).await,
        LabelGap::NoAddress
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn freezer_labels_count_from_the_freeze_date_in_amsterdam(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = baked(&mut conn, &k, 10, 1).await;
    assert_eq!(
        gap(&mut conn, id, &k, LabelKind::Freezer).await,
        LabelGap::NoFrozenPieces
    );

    // 23:30 on 5 December in Amsterdam is 22:30 UTC; the freeze date is the 5th. An hour
    // later it is the 6th in Amsterdam while still the 5th in UTC.
    let clock = late_on_bake_day();
    clock.advance(jiff::SignedDuration::from_hours(1));
    label_files::record_frozen(&mut conn, id, k.bread, 6, BAKER, &clock)
        .await
        .unwrap();
    let on = date(2026, 12, 6);
    assert_eq!(
        label_files::frozen(&mut conn, id, k.bread).await.unwrap(),
        Some(Frozen { pieces: 6, on })
    );
    let label = label_files::label(&mut conn, id, k.bread, LabelKind::Freezer)
        .await
        .unwrap();
    assert_eq!(label.copies(), 6, "one per frozen piece");
    let product = label_files::label(&mut conn, id, k.bread, LabelKind::Product)
        .await
        .unwrap();
    assert_eq!(
        product.copies(),
        3,
        "frozen pieces get only a freezer label"
    );
    assert_eq!(label.label().kind, labels::Kind::Freezer { frozen_on: on });
    assert_eq!(
        label.label().may_contain,
        BTreeSet::from([Allergen::Milk]),
        "the kitchen list goes on freezer labels too"
    );
    assert_eq!(
        label.label().best_before,
        date(2027, 3, 6),
        "freeze date plus 90 days"
    );

    // A correction keeps the first freeze date; 0 clears it.
    clock.advance(jiff::SignedDuration::from_hours(48));
    label_files::record_frozen(&mut conn, id, k.bread, 9, BAKER, &clock)
        .await
        .unwrap();
    assert_eq!(
        label_files::frozen(&mut conn, id, k.bread).await.unwrap(),
        Some(Frozen { pieces: 9, on })
    );
    label_files::record_frozen(&mut conn, id, k.bread, 0, BAKER, &clock)
        .await
        .unwrap();
    assert_eq!(
        label_files::frozen(&mut conn, id, k.bread).await.unwrap(),
        None
    );

    label_settings::set(
        &mut conn,
        k.bread,
        &LabelSettings {
            freezer_days: None,
            ..settings()
        },
    )
    .await
    .unwrap();
    label_files::record_frozen(&mut conn, id, k.bread, 2, BAKER, &clock)
        .await
        .unwrap();
    assert_eq!(
        gap(&mut conn, id, &k, LabelKind::Freezer).await,
        LabelGap::NoFreezerPeriod
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_freeze_date_follows_summer_time(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let id = baked(&mut conn, &k, 10, 0).await;
    // Summer time starts on 28 March 2027; 22:30 UTC on the 28th is 00:30 on the 29th.
    let clock = FakeClock::new("2027-03-28T22:30:00Z".parse().unwrap());
    label_files::record_frozen(&mut conn, id, k.bread, 1, BAKER, &clock)
        .await
        .unwrap();
    assert_eq!(
        label_files::frozen(&mut conn, id, k.bread).await.unwrap(),
        Some(Frozen {
            pieces: 1,
            on: date(2027, 3, 29)
        })
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn frozen_pieces_are_good_pieces_of_a_baked_batch(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let k = kitchen(&mut conn).await;
    let clock = late_on_bake_day();
    let unbaked = batches::start_manual(&mut conn, k.day, k.bread, 10, BAKER, &clock)
        .await
        .unwrap();
    assert!(matches!(
        label_files::record_frozen(&mut conn, unbaked, k.bread, 1, BAKER, &clock).await,
        Err(Error::NotBaked)
    ));
    let id = baked(&mut conn, &k, 10, 3).await;
    assert!(matches!(
        label_files::record_frozen(&mut conn, id, k.bread, 8, BAKER, &clock).await,
        Err(Error::Invalid("frozen pieces"))
    ));
    label_files::record_frozen(&mut conn, id, k.bread, 7, BAKER, &clock)
        .await
        .unwrap();
    assert!(matches!(
        label_files::record_frozen(&mut conn, id, ProductId::new(999_999), 1, BAKER, &clock).await,
        Err(Error::NotFound)
    ));

    // Fewer good pieces than are in the freezer is refused.
    let fewer = batches::record_actuals(
        &mut conn,
        id,
        &[Actual {
            product_id: k.bread,
            pieces: 10,
            waste: 4,
        }],
        BAKER,
        &clock,
    )
    .await;
    assert!(matches!(fewer, Err(Error::Invalid("frozen pieces"))));
}
