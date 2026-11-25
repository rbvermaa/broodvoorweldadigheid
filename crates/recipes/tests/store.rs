#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use domain::{Allergen, IngredientId, ProductId, RecipeId, RecipeVersionId};
use jiff::{SignedDuration, Timestamp};
use recipes::{
    DraftInput, Error, IngredientInput, IngredientRole, LineInput, LinePart, Part, Pct,
    RecipeInput, RecipeKind, Version, ingredients, recipes as store,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn pct(percent: u32) -> Pct {
    Pct::from_thousandths(percent * 1000)
}

fn ingredient_input(name: &str, role: IngredientRole) -> IngredientInput {
    IngredientInput {
        name: name.to_owned(),
        label_name: String::new(),
        role,
        supplier_name: String::new(),
        allergens: BTreeSet::new(),
    }
}

async fn ingredient(conn: &mut PgConnection, name: &str, role: IngredientRole) -> IngredientId {
    ingredients::create(conn, &ingredient_input(name, role))
        .await
        .unwrap()
        .id
}

async fn recipe(conn: &mut PgConnection, name: &str, kind: RecipeKind) -> (RecipeId, Version) {
    let (recipe, version) = store::create(
        conn,
        &RecipeInput {
            name: name.to_owned(),
            kind,
        },
    )
    .await
    .unwrap();
    (recipe.id, version)
}

fn line(part: LinePart, percent: u32) -> LineInput {
    LineInput {
        pct: pct(percent),
        part,
    }
}

fn draft(lines: Vec<LineInput>) -> DraftInput {
    DraftInput {
        lines,
        lead_time_hours: None,
        notes: String::new(),
    }
}

struct Kitchen {
    flour: IngredientId,
    water: IngredientId,
    salt: IngredientId,
}

async fn kitchen(conn: &mut PgConnection) -> Kitchen {
    Kitchen {
        flour: ingredient(conn, "Tarwebloem", IngredientRole::Flour).await,
        water: ingredient(conn, "Water", IngredientRole::Water).await,
        salt: ingredient(conn, "Zeezout", IngredientRole::Other).await,
    }
}

/// A levain recipe whose draft v1 is flour 100%, water 100%.
async fn levain(conn: &mut PgConnection, k: &Kitchen) -> (RecipeId, Version) {
    let (id, v1) = recipe(conn, "Desem", RecipeKind::Preferment).await;
    store::save_draft(
        conn,
        v1.id,
        &draft(vec![
            line(LinePart::Ingredient(k.flour), 100),
            line(LinePart::Ingredient(k.water), 100),
        ]),
    )
    .await
    .unwrap();
    (id, v1)
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn ingredients_round_trip_and_names_are_unique(mut conn: PoolConnection<Postgres>) {
    let input = IngredientInput {
        name: " Speltbloem ".to_owned(),
        label_name: "SPELTbloem".to_owned(),
        role: IngredientRole::Flour,
        supplier_name: "Molen De Hoop".to_owned(),
        allergens: BTreeSet::from([Allergen::CerealsContainingGluten]),
    };
    let created = ingredients::create(&mut conn, &input).await.unwrap();
    assert_eq!(created.name, "Speltbloem");
    assert_eq!(created.label(), "SPELTbloem");
    assert_eq!(
        ingredients::get(&mut conn, created.id).await.unwrap(),
        created
    );

    let duplicate = ingredients::create(
        &mut conn,
        &ingredient_input("speltbloem", IngredientRole::Flour),
    )
    .await;
    assert!(matches!(duplicate, Err(Error::Duplicate)), "{duplicate:?}");
    let empty =
        ingredients::create(&mut conn, &ingredient_input("  ", IngredientRole::Other)).await;
    assert!(matches!(empty, Err(Error::Invalid("name"))), "{empty:?}");

    let water = ingredient(&mut conn, "Water", IngredientRole::Water).await;
    let mut changed = input.clone();
    changed.label_name = String::new();
    changed.allergens.clear();
    changed.role = IngredientRole::Other;
    let updated = ingredients::update(&mut conn, created.id, &changed)
        .await
        .unwrap();
    assert_eq!(updated.label(), "Speltbloem");
    assert!(updated.allergens.is_empty());
    assert_eq!(updated.role, IngredientRole::Other);

    let names: Vec<String> = ingredients::list(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.name)
        .collect();
    assert_eq!(names, vec!["Speltbloem", "Water"]);

    let taken = ingredients::update(&mut conn, water, &changed).await;
    assert!(matches!(taken, Err(Error::Duplicate)), "{taken:?}");
    let missing = ingredients::update(&mut conn, IngredientId::new(999), &changed).await;
    assert!(matches!(missing, Err(Error::NotFound)), "{missing:?}");
    let blank = ingredients::update(
        &mut conn,
        water,
        &ingredient_input("", IngredientRole::Water),
    )
    .await;
    assert!(matches!(blank, Err(Error::Invalid("name"))), "{blank:?}");
    let gone = ingredients::get(&mut conn, IngredientId::new(999)).await;
    assert!(matches!(gone, Err(Error::NotFound)), "{gone:?}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_new_recipe_has_an_empty_current_draft(mut conn: PoolConnection<Postgres>) {
    let (id, v1) = recipe(&mut conn, "Desembrood wit", RecipeKind::Dough).await;
    assert_eq!(v1.version, 1);
    assert!(!v1.is_frozen());
    let fetched = store::get(&mut conn, id).await.unwrap();
    assert_eq!(fetched.current_version_id, Some(v1.id));
    assert_eq!(fetched.kind, RecipeKind::Dough);
    assert!(store::lines(&mut conn, v1.id).await.unwrap().is_empty());

    let duplicate = store::create(
        &mut conn,
        &RecipeInput {
            name: "desembrood WIT".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await;
    assert!(matches!(duplicate, Err(Error::Duplicate)), "{duplicate:?}");
    let empty = store::create(
        &mut conn,
        &RecipeInput {
            name: " ".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await;
    assert!(matches!(empty, Err(Error::Invalid("name"))), "{empty:?}");

    let renamed = store::update(
        &mut conn,
        id,
        &RecipeInput {
            name: "Wit desem".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    assert_eq!(renamed.name, "Wit desem");
    let missing = store::update(
        &mut conn,
        RecipeId::new(999),
        &RecipeInput {
            name: "X".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await;
    assert!(matches!(missing, Err(Error::NotFound)), "{missing:?}");
    assert!(matches!(
        store::get(&mut conn, RecipeId::new(999)).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_draft_saves_lines_and_resolves_its_sub_recipes(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let (_, levain_v1) = levain(&mut conn, &k).await;
    let (dough, v1) = recipe(&mut conn, "Desembrood", RecipeKind::Dough).await;
    let lines = vec![
        line(LinePart::Ingredient(k.flour), 90),
        line(LinePart::Ingredient(k.water), 65),
        line(LinePart::Ingredient(k.salt), 2),
        line(LinePart::Sub(levain_v1.id), 20),
    ];
    let saved = store::save_draft(
        &mut conn,
        v1.id,
        &DraftInput {
            lines: lines.clone(),
            lead_time_hours: Some(36),
            notes: "Desem 's avonds aanzetten".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(saved.id, v1.id);
    assert_eq!(saved.lead_time_hours, Some(36));
    assert_eq!(saved.notes, "Desem 's avonds aanzetten");
    assert_eq!(store::lines(&mut conn, v1.id).await.unwrap(), lines);

    let formula = store::formula(&mut conn, v1.id).await.unwrap();
    assert_eq!(formula.lines.len(), 4);
    let Part::Sub(sub) = &formula.lines[3].part else {
        panic!("sub-recipe line");
    };
    assert_eq!(sub.name, "Desem");
    assert_eq!(sub.kind, RecipeKind::Preferment);
    assert_eq!(sub.formula.lines.len(), 2);
    let analysis = formula.analysis().unwrap();
    assert_eq!(analysis.hydration, Some(pct(75)));
    assert_eq!(analysis.total_flour, pct(100));

    let fewer = vec![line(LinePart::Ingredient(k.flour), 100)];
    store::save_draft(&mut conn, v1.id, &draft(fewer.clone()))
        .await
        .unwrap();
    assert_eq!(store::lines(&mut conn, v1.id).await.unwrap(), fewer);
    assert_eq!(store::versions(&mut conn, dough).await.unwrap().len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn freezing_freezes_sub_recipes_and_keeps_the_first_time(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let k = kitchen(&mut conn).await;
    let (levain_id, levain_v1) = levain(&mut conn, &k).await;
    let (dough, v1) = recipe(&mut conn, "Desembrood", RecipeKind::Dough).await;
    store::save_draft(
        &mut conn,
        v1.id,
        &draft(vec![
            line(LinePart::Ingredient(k.flour), 90),
            line(LinePart::Sub(levain_v1.id), 20),
        ]),
    )
    .await
    .unwrap();

    let frozen = store::freeze(&mut conn, dough, v1.id, &clock)
        .await
        .unwrap();
    assert_eq!(frozen.frozen_at, Some(ts("2026-12-01T09:00:00Z")));
    let levain_frozen = store::version(&mut conn, levain_id, levain_v1.id)
        .await
        .unwrap();
    assert_eq!(levain_frozen.frozen_at, Some(ts("2026-12-01T09:00:00Z")));

    clock.advance(SignedDuration::from_hours(1));
    let again = store::freeze(&mut conn, dough, v1.id, &clock)
        .await
        .unwrap();
    assert_eq!(again.frozen_at, Some(ts("2026-12-01T09:00:00Z")));

    let wrong = store::freeze(&mut conn, levain_id, v1.id, &clock).await;
    assert!(matches!(wrong, Err(Error::NotFound)), "{wrong:?}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn editing_a_frozen_version_creates_the_next_one(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let k = kitchen(&mut conn).await;
    let (dough, v1) = recipe(&mut conn, "Desembrood", RecipeKind::Dough).await;
    let original = vec![
        line(LinePart::Ingredient(k.flour), 100),
        line(LinePart::Ingredient(k.water), 78),
    ];
    store::save_draft(&mut conn, v1.id, &draft(original.clone()))
        .await
        .unwrap();
    store::freeze(&mut conn, dough, v1.id, &clock)
        .await
        .unwrap();

    let changed = vec![
        line(LinePart::Ingredient(k.flour), 100),
        line(LinePart::Ingredient(k.water), 74),
    ];
    let v2 = store::save_draft(&mut conn, v1.id, &draft(changed.clone()))
        .await
        .unwrap();
    assert_ne!(v2.id, v1.id);
    assert_eq!(v2.version, 2);
    assert!(!v2.is_frozen());
    assert_eq!(store::lines(&mut conn, v2.id).await.unwrap(), changed);
    assert_eq!(store::lines(&mut conn, v1.id).await.unwrap(), original);
    assert_eq!(
        store::get(&mut conn, dough)
            .await
            .unwrap()
            .current_version_id,
        Some(v1.id),
        "the new version is not in use until staff say so"
    );

    let second = store::save_draft(&mut conn, v1.id, &draft(changed)).await;
    assert!(
        matches!(second, Err(Error::DraftExists(id)) if id == v2.id),
        "{second:?}"
    );

    let versions: Vec<i32> = store::versions(&mut conn, dough)
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.version)
        .collect();
    assert_eq!(versions, vec![2, 1]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn new_version_copies_the_newest_or_returns_the_draft(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let k = kitchen(&mut conn).await;
    let (dough, v1) = recipe(&mut conn, "Desembrood", RecipeKind::Dough).await;
    let lines = vec![
        line(LinePart::Ingredient(k.flour), 100),
        line(LinePart::Ingredient(k.salt), 2),
    ];
    store::save_draft(
        &mut conn,
        v1.id,
        &DraftInput {
            lines: lines.clone(),
            lead_time_hours: Some(24),
            notes: "Koel rijzen".to_owned(),
        },
    )
    .await
    .unwrap();

    let same = store::new_version(&mut conn, dough).await.unwrap();
    assert_eq!(same.id, v1.id, "a draft exists, so that one is returned");

    store::freeze(&mut conn, dough, v1.id, &clock)
        .await
        .unwrap();
    let v2 = store::new_version(&mut conn, dough).await.unwrap();
    assert_eq!(v2.version, 2);
    assert_eq!(v2.lead_time_hours, Some(24));
    assert_eq!(v2.notes, "Koel rijzen");
    assert_eq!(store::lines(&mut conn, v2.id).await.unwrap(), lines);

    let missing = store::new_version(&mut conn, RecipeId::new(999)).await;
    assert!(matches!(missing, Err(Error::NotFound)), "{missing:?}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_database_refuses_changes_to_frozen_versions(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let k = kitchen(&mut conn).await;
    let (dough, v1) = recipe(&mut conn, "Desembrood", RecipeKind::Dough).await;
    store::save_draft(
        &mut conn,
        v1.id,
        &draft(vec![line(LinePart::Ingredient(k.flour), 100)]),
    )
    .await
    .unwrap();
    store::freeze(&mut conn, dough, v1.id, &clock)
        .await
        .unwrap();

    for sql in [
        "UPDATE recipe_line SET bakers_pct_x1000 = 1 WHERE recipe_version_id = $1",
        "DELETE FROM recipe_line WHERE recipe_version_id = $1",
        "INSERT INTO recipe_line (recipe_version_id, position, ingredient_id, bakers_pct_x1000)
            SELECT $1, 9, id, 1000 FROM ingredient LIMIT 1",
        "UPDATE recipe_version SET notes = 'stiekem' WHERE id = $1",
        "UPDATE recipe_version SET frozen_at = NULL WHERE id = $1",
    ] {
        let refused = sqlx::query(sql).bind(v1.id.get()).execute(&mut *conn).await;
        assert!(refused.is_err(), "{sql}");
    }
    let deleted = sqlx::query("DELETE FROM recipe_version WHERE id = $1")
        .bind(v1.id.get())
        .execute(&mut *conn)
        .await
        .unwrap_err();
    assert!(
        deleted.to_string().contains("is frozen"),
        "the trigger refuses before any foreign key does: {deleted}"
    );

    let (_, other) = recipe(&mut conn, "Spelt", RecipeKind::Dough).await;
    let moved =
        sqlx::query("UPDATE recipe_line SET recipe_version_id = $2 WHERE recipe_version_id = $1")
            .bind(v1.id.get())
            .bind(other.id.get())
            .execute(&mut *conn)
            .await;
    assert!(moved.is_err(), "a line cannot leave a frozen version");

    let (concept, draft_v1) = recipe(&mut conn, "Concept", RecipeKind::Dough).await;
    sqlx::query("UPDATE recipe SET current_version_id = NULL WHERE id = $1")
        .bind(concept.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let deleted = sqlx::query("DELETE FROM recipe_version WHERE id = $1")
        .bind(draft_v1.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(deleted.rows_affected(), 1, "a draft version can go");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_database_refuses_a_frozen_version_with_a_draft_sub(
    mut conn: PoolConnection<Postgres>,
) {
    let k = kitchen(&mut conn).await;
    let (levain_id, levain_v1) = levain(&mut conn, &k).await;
    let (_, v1) = recipe(&mut conn, "Desembrood", RecipeKind::Dough).await;
    store::save_draft(
        &mut conn,
        v1.id,
        &draft(vec![
            line(LinePart::Ingredient(k.flour), 90),
            line(LinePart::Sub(levain_v1.id), 20),
        ]),
    )
    .await
    .unwrap();

    let mut tx = sqlx::Connection::begin(&mut *conn).await.unwrap();
    sqlx::query("UPDATE recipe_version SET frozen_at = now() WHERE id = $1")
        .bind(v1.id.get())
        .execute(&mut *tx)
        .await
        .expect("the check waits for the commit");
    let refused = tx.commit().await.unwrap_err();
    assert!(
        refused.to_string().contains("draft sub-recipe"),
        "the sub-recipe is still a draft: {refused}"
    );
    assert!(
        store::version(&mut conn, levain_id, levain_v1.id)
            .await
            .unwrap()
            .frozen_at
            .is_none()
    );

    let mut tx = sqlx::Connection::begin(&mut *conn).await.unwrap();
    for id in [v1.id, levain_v1.id] {
        sqlx::query("UPDATE recipe_version SET frozen_at = now() WHERE id = $1")
            .bind(id.get())
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx.commit()
        .await
        .expect("freezing both in one transaction is fine");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn what_a_frozen_version_counts_on_cannot_change(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let k = kitchen(&mut conn).await;
    let (levain_id, levain_v1) = levain(&mut conn, &k).await;
    let rye = ingredient(&mut conn, "Roggemeel", IngredientRole::Flour).await;
    let (_, rye_v1) = recipe(&mut conn, "Rogge", RecipeKind::Dough).await;
    store::save_draft(
        &mut conn,
        rye_v1.id,
        &draft(vec![line(LinePart::Ingredient(rye), 100)]),
    )
    .await
    .unwrap();
    let draft_use = ingredients::update(
        &mut conn,
        rye,
        &ingredient_input("Roggemeel", IngredientRole::Other),
    )
    .await;
    assert!(draft_use.is_ok(), "only a draft uses it: {draft_use:?}");

    let as_dough = RecipeInput {
        name: "Desem".to_owned(),
        kind: RecipeKind::Dough,
    };
    let as_preferment = RecipeInput {
        name: "Desem".to_owned(),
        kind: RecipeKind::Preferment,
    };
    store::update(&mut conn, levain_id, &as_dough)
        .await
        .expect("nothing frozen uses the levain yet");
    store::update(&mut conn, levain_id, &as_preferment)
        .await
        .unwrap();

    let (dough, v1) = recipe(&mut conn, "Desembrood", RecipeKind::Dough).await;
    store::save_draft(
        &mut conn,
        v1.id,
        &draft(vec![
            line(LinePart::Ingredient(k.flour), 90),
            line(LinePart::Ingredient(k.water), 60),
            line(LinePart::Sub(levain_v1.id), 20),
        ]),
    )
    .await
    .unwrap();
    store::freeze(&mut conn, dough, v1.id, &clock)
        .await
        .unwrap();

    let role = ingredients::update(
        &mut conn,
        k.water,
        &ingredient_input("Water", IngredientRole::Other),
    )
    .await;
    assert!(matches!(role, Err(Error::InUse)), "{role:?}");
    let renamed = ingredients::update(
        &mut conn,
        k.water,
        &ingredient_input("Kraanwater", IngredientRole::Water),
    )
    .await
    .unwrap();
    assert_eq!(renamed.name, "Kraanwater", "a new name keeps the figures");

    let kind = store::update(&mut conn, levain_id, &as_dough).await;
    assert!(matches!(kind, Err(Error::InUse)), "{kind:?}");
    let renamed = store::update(
        &mut conn,
        levain_id,
        &RecipeInput {
            name: "Desem 1:1".to_owned(),
            kind: RecipeKind::Preferment,
        },
    )
    .await
    .unwrap();
    assert_eq!(renamed.name, "Desem 1:1");
    let top_kind = store::update(
        &mut conn,
        dough,
        &RecipeInput {
            name: "Desembrood".to_owned(),
            kind: RecipeKind::Filling,
        },
    )
    .await;
    assert!(
        top_kind.is_ok(),
        "no frozen version uses the dough as a sub-recipe: {top_kind:?}"
    );

    let missing = store::update(&mut conn, RecipeId::new(999), &as_dough).await;
    assert!(matches!(missing, Err(Error::NotFound)), "{missing:?}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn drafts_refuse_bad_input(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let (_, v1) = recipe(&mut conn, "Desembrood", RecipeKind::Dough).await;
    let flour = |percent| line(LinePart::Ingredient(k.flour), percent);

    let late = DraftInput {
        lead_time_hours: Some(169),
        ..draft(vec![flour(100)])
    };
    assert!(matches!(
        store::save_draft(&mut conn, v1.id, &late).await,
        Err(Error::Invalid("lead_time_hours"))
    ));
    let negative = DraftInput {
        lead_time_hours: Some(-1),
        ..draft(vec![])
    };
    assert!(matches!(
        store::save_draft(&mut conn, v1.id, &negative).await,
        Err(Error::Invalid("lead_time_hours"))
    ));
    assert!(matches!(
        store::save_draft(&mut conn, v1.id, &draft(vec![flour(0)])).await,
        Err(Error::Invalid("pct"))
    ));
    assert!(matches!(
        store::save_draft(&mut conn, v1.id, &draft(vec![flour(1001)])).await,
        Err(Error::Invalid("pct"))
    ));
    assert!(matches!(
        store::save_draft(&mut conn, v1.id, &draft(vec![flour(1); 61])).await,
        Err(Error::Invalid("lines"))
    ));
    let unknown = draft(vec![line(LinePart::Ingredient(IngredientId::new(999)), 10)]);
    assert!(matches!(
        store::save_draft(&mut conn, v1.id, &unknown).await,
        Err(Error::Invalid("line"))
    ));
    let unknown_sub = draft(vec![line(LinePart::Sub(RecipeVersionId::new(999)), 10)]);
    assert!(matches!(
        store::save_draft(&mut conn, v1.id, &unknown_sub).await,
        Err(Error::Invalid("line"))
    ));
    assert!(matches!(
        store::save_draft(&mut conn, RecipeVersionId::new(999), &draft(vec![])).await,
        Err(Error::NotFound)
    ));
    assert!(store::lines(&mut conn, v1.id).await.unwrap().is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sub_recipes_cannot_loop_or_nest_too_deep(mut conn: PoolConnection<Postgres>) {
    let k = kitchen(&mut conn).await;
    let flour = line(LinePart::Ingredient(k.flour), 100);
    let (a, a1) = recipe(&mut conn, "A", RecipeKind::Dough).await;
    let (_, b1) = recipe(&mut conn, "B", RecipeKind::Preferment).await;

    let own = draft(vec![flour, line(LinePart::Sub(a1.id), 10)]);
    assert!(matches!(
        store::save_draft(&mut conn, a1.id, &own).await,
        Err(Error::Nesting)
    ));
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    store::save_draft(&mut conn, a1.id, &draft(vec![flour]))
        .await
        .unwrap();
    store::freeze(&mut conn, a, a1.id, &clock).await.unwrap();
    let a2 = store::new_version(&mut conn, a).await.unwrap();
    let older = draft(vec![flour, line(LinePart::Sub(a1.id), 10)]);
    assert!(
        matches!(
            store::save_draft(&mut conn, a2.id, &older).await,
            Err(Error::Nesting)
        ),
        "not even an older version of the same recipe"
    );

    store::save_draft(
        &mut conn,
        a2.id,
        &draft(vec![flour, line(LinePart::Sub(b1.id), 20)]),
    )
    .await
    .unwrap();
    let back = draft(vec![flour, line(LinePart::Sub(a2.id), 20)]);
    assert!(
        matches!(
            store::save_draft(&mut conn, b1.id, &back).await,
            Err(Error::Nesting)
        ),
        "B inside A, so A cannot go inside B"
    );

    let (_, c1) = recipe(&mut conn, "C", RecipeKind::Preferment).await;
    let (_, d1) = recipe(&mut conn, "D", RecipeKind::Preferment).await;
    let (_, e1) = recipe(&mut conn, "E", RecipeKind::Preferment).await;
    store::save_draft(&mut conn, e1.id, &draft(vec![flour]))
        .await
        .unwrap();
    store::save_draft(
        &mut conn,
        d1.id,
        &draft(vec![flour, line(LinePart::Sub(e1.id), 10)]),
    )
    .await
    .unwrap();
    store::save_draft(
        &mut conn,
        c1.id,
        &draft(vec![flour, line(LinePart::Sub(d1.id), 10)]),
    )
    .await
    .unwrap();
    store::save_draft(
        &mut conn,
        b1.id,
        &draft(vec![flour, line(LinePart::Sub(c1.id), 10)]),
    )
    .await
    .unwrap_err();
    assert!(
        store::formula(&mut conn, a2.id).await.is_ok(),
        "A > B > C > D > E would be four levels; the save above was refused"
    );
    store::save_draft(
        &mut conn,
        b1.id,
        &draft(vec![flour, line(LinePart::Sub(d1.id), 10)]),
    )
    .await
    .unwrap();
    let depth3 = store::formula(&mut conn, a2.id).await.unwrap();
    assert_eq!(depth3.scale(1000).unwrap().len(), 2);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn current_version_products_and_sub_choices(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let k = kitchen(&mut conn).await;
    let (levain_id, levain_v1) = levain(&mut conn, &k).await;
    let (dough, v1) = recipe(&mut conn, "Desembrood", RecipeKind::Dough).await;
    store::freeze(&mut conn, dough, v1.id, &clock)
        .await
        .unwrap();
    let v2 = store::new_version(&mut conn, dough).await.unwrap();

    let current = store::set_current(&mut conn, dough, v2.id).await.unwrap();
    assert_eq!(current.current_version_id, Some(v2.id));
    let wrong = store::set_current(&mut conn, dough, levain_v1.id).await;
    assert!(matches!(wrong, Err(Error::NotFound)), "{wrong:?}");

    let product: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
        VALUES ('Desem wit', 650, 9, 800, 900) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let product = ProductId::new(product);
    store::link_product(&mut conn, dough, product)
        .await
        .unwrap();
    let linked = store::products(&mut conn, dough).await.unwrap();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].name, "Desem wit");

    let summaries = store::list(&mut conn).await.unwrap();
    let names: Vec<&str> = summaries.iter().map(|s| s.recipe.name.as_str()).collect();
    assert_eq!(names, vec!["Desem", "Desembrood"]);
    assert_eq!(summaries[1].current_version, Some(2));
    assert_eq!(summaries[1].products, vec!["Desem wit"]);
    assert!(summaries[0].products.is_empty());

    store::unlink_product(&mut conn, levain_id, product)
        .await
        .unwrap();
    assert_eq!(
        store::products(&mut conn, dough).await.unwrap().len(),
        1,
        "linked to another recipe"
    );
    store::unlink_product(&mut conn, dough, product)
        .await
        .unwrap();
    assert!(store::products(&mut conn, dough).await.unwrap().is_empty());

    let missing_product = store::link_product(&mut conn, dough, ProductId::new(999)).await;
    assert!(
        matches!(missing_product, Err(Error::NotFound)),
        "{missing_product:?}"
    );
    let missing_recipe = store::link_product(&mut conn, RecipeId::new(999), product).await;
    assert!(
        matches!(missing_recipe, Err(Error::NotFound)),
        "{missing_recipe:?}"
    );

    let choices = store::sub_choices(&mut conn, dough).await.unwrap();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].version_id, levain_v1.id);
    assert_eq!(choices[0].recipe_name, "Desem");
    assert_eq!(choices[0].kind, RecipeKind::Preferment);
    assert!(!choices[0].frozen);
    let for_levain = store::sub_choices(&mut conn, levain_id).await.unwrap();
    let versions: Vec<i32> = for_levain.iter().map(|c| c.version).collect();
    assert_eq!(versions, vec![2, 1]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_role_change_waits_for_a_freeze_in_progress(pool: sqlx::PgPool) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let mut setup = pool.acquire().await.unwrap();
    let k = kitchen(&mut setup).await;
    let (dough, v1) = recipe(&mut setup, "Desembrood", RecipeKind::Dough).await;
    store::save_draft(
        &mut setup,
        v1.id,
        &draft(vec![
            line(LinePart::Ingredient(k.flour), 100),
            line(LinePart::Ingredient(k.water), 70),
        ]),
    )
    .await
    .unwrap();
    drop(setup);

    let mut freezer = pool.acquire().await.unwrap();
    let mut tx = sqlx::Connection::begin(&mut *freezer).await.unwrap();
    store::freeze(&mut tx, dough, v1.id, &clock).await.unwrap();

    let other = pool.clone();
    let water = k.water;
    let change = tokio::spawn(async move {
        let mut conn = other.acquire().await.unwrap();
        ingredients::update(
            &mut conn,
            water,
            &ingredient_input("Water", IngredientRole::Other),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !change.is_finished(),
        "the role change waits for the freeze"
    );
    tx.commit().await.unwrap();

    let result = change.await.unwrap();
    assert!(matches!(result, Err(Error::InUse)), "{result:?}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_line_written_during_a_freeze_is_refused(pool: sqlx::PgPool) {
    let mut setup = pool.acquire().await.unwrap();
    let k = kitchen(&mut setup).await;
    let (_, v1) = recipe(&mut setup, "Desembrood", RecipeKind::Dough).await;
    drop(setup);

    let mut freezer = pool.acquire().await.unwrap();
    let mut tx = sqlx::Connection::begin(&mut *freezer).await.unwrap();
    sqlx::query("UPDATE recipe_version SET frozen_at = now() WHERE id = $1")
        .bind(v1.id.get())
        .execute(&mut *tx)
        .await
        .unwrap();

    let other = pool.clone();
    let (version, flour) = (v1.id.get(), k.flour.get());
    let insert = tokio::spawn(async move {
        sqlx::query(
            "INSERT INTO recipe_line (recipe_version_id, position, ingredient_id, bakers_pct_x1000)
            VALUES ($1, 0, $2, 100000)",
        )
        .bind(version)
        .bind(flour)
        .execute(&other)
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!insert.is_finished(), "the line waits for the freeze");
    tx.commit().await.unwrap();

    let refused = insert.await.unwrap().unwrap_err();
    assert!(refused.to_string().contains("is frozen"), "{refused}");
}
