//! Instructions per recipe version: free text or steps (F14, ADR 0049).
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use domain::{IngredientId, ProductId, RecipeId, RecipeVersionId};
use jiff::Timestamp;
use recipes::method::{self, MAX_STEPS, MAX_TEXT, stamp};
use recipes::{
    DraftInput, Error, IngredientInput, IngredientRole, LineInput, LinePart, Method, MethodKind,
    Pct, RecipeInput, RecipeKind, Step, StepEdit, ingredients, recipes as store,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

async fn flour(conn: &mut PgConnection) -> IngredientId {
    ingredients::create(
        conn,
        &IngredientInput {
            name: "Tarwebloem".to_owned(),
            label_name: String::new(),
            role: IngredientRole::Flour,
            supplier_name: String::new(),
            allergens: BTreeSet::new(),
        },
    )
    .await
    .unwrap()
    .id
}

async fn recipe(conn: &mut PgConnection) -> (RecipeId, RecipeVersionId) {
    let (recipe, version) = store::create(
        conn,
        &RecipeInput {
            name: "Desembrood".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .unwrap();
    (recipe.id, version.id)
}

fn step(name: &str, minutes: Option<u32>) -> Step {
    Step {
        name: name.to_owned(),
        duration_min: minutes,
        temperature_c: None,
        notes: String::new(),
    }
}

async fn current(conn: &mut PgConnection, id: RecipeVersionId) -> Method {
    method::method(conn, id).await.unwrap()
}

async fn seen(conn: &mut PgConnection, id: RecipeVersionId) -> String {
    stamp(&current(conn, id).await)
}

async fn to_steps(conn: &mut PgConnection, id: RecipeVersionId, steps: &[Step]) {
    let s = seen(conn, id).await;
    method::set_kind(conn, id, &s, MethodKind::Steps)
        .await
        .unwrap();
    for one in steps {
        let s = seen(conn, id).await;
        method::edit_step(conn, id, &s, StepEdit::Add(one.clone()))
            .await
            .unwrap();
    }
}

async fn freeze(conn: &mut PgConnection, recipe: RecipeId, id: RecipeVersionId) {
    let clock = FakeClock::new("2026-12-01T10:00:00Z".parse::<Timestamp>().unwrap());
    store::freeze(conn, recipe, id, &clock).await.unwrap();
}

fn names(method: &Method) -> Vec<String> {
    match method {
        Method::Steps(steps) => steps.iter().map(|s| s.name.clone()).collect(),
        other => panic!("not steps: {other:?}"),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_new_version_has_no_instructions(mut conn: PoolConnection<Postgres>) {
    let (_, v1) = recipe(&mut conn).await;
    assert_eq!(current(&mut conn, v1).await, Method::None);
    assert!(matches!(
        method::method(&mut conn, RecipeVersionId::new(999_999)).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn steps_are_stored_in_order_and_edited_one_at_a_time(mut conn: PoolConnection<Postgres>) {
    let (_, v1) = recipe(&mut conn).await;
    let bulk = Step {
        name: "Bulk".to_owned(),
        duration_min: Some(240),
        temperature_c: Some(24),
        notes: "4 keer vouwen,\nelk half uur".to_owned(),
    };
    to_steps(
        &mut conn,
        v1,
        &[
            step("Autolyse", Some(30)),
            bulk.clone(),
            step("Bakken", Some(45)),
        ],
    )
    .await;
    let stored = current(&mut conn, v1).await;
    assert_eq!(names(&stored), ["Autolyse", "Bulk", "Bakken"]);
    let Method::Steps(steps) = &stored else {
        unreachable!()
    };
    assert_eq!(steps[1], bulk);

    let edits = [
        StepEdit::Set {
            position: 2,
            step: Step {
                temperature_c: Some(250),
                ..step("Bakken", Some(40))
            },
        },
        StepEdit::MoveUp { position: 2 },
        StepEdit::MoveDown { position: 0 },
        StepEdit::Remove { position: 2 },
    ];
    for edit in edits {
        let s = seen(&mut conn, v1).await;
        method::edit_step(&mut conn, v1, &s, edit).await.unwrap();
    }
    let after = current(&mut conn, v1).await;
    assert_eq!(names(&after), ["Bakken", "Autolyse"]);
    let Method::Steps(steps) = &after else {
        unreachable!()
    };
    assert_eq!(steps[0].temperature_c, Some(250));
    assert_eq!(steps[0].duration_min, Some(40));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn out_of_date_or_bad_step_edits_change_nothing(mut conn: PoolConnection<Postgres>) {
    let (_, v1) = recipe(&mut conn).await;
    let old = seen(&mut conn, v1).await;
    assert!(matches!(
        method::edit_step(&mut conn, v1, &old, StepEdit::Add(step("Bulk", None))).await,
        Err(Error::Stale)
    ));
    to_steps(&mut conn, v1, &[step("Bulk", None)]).await;
    let before = current(&mut conn, v1).await;
    let refused = [
        (old.clone(), StepEdit::Add(step("Vormen", None)), "stale"),
        (
            stamp(&before),
            StepEdit::Remove { position: 1 },
            "no such step",
        ),
        (stamp(&before), StepEdit::Add(step("", None)), "empty name"),
        (
            stamp(&before),
            StepEdit::Set {
                position: 0,
                step: step("Bulk", Some(0)),
            },
            "zero minutes",
        ),
    ];
    for (s, edit, why) in refused {
        let result = method::edit_step(&mut conn, v1, &s, edit).await;
        assert!(
            matches!(result, Err(Error::Stale | Error::Invalid(_))),
            "{why}: {result:?}"
        );
        assert_eq!(current(&mut conn, v1).await, before, "{why}");
    }
    assert!(matches!(
        method::edit_step(
            &mut conn,
            RecipeVersionId::new(999_999),
            &old,
            StepEdit::Add(step("Bulk", None))
        )
        .await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_version_holds_at_most_forty_steps(mut conn: PoolConnection<Postgres>) {
    let (_, v1) = recipe(&mut conn).await;
    let many: Vec<Step> = (0..MAX_STEPS)
        .map(|i| step(&format!("Stap {i}"), Some(5)))
        .collect();
    to_steps(&mut conn, v1, &many).await;
    let s = seen(&mut conn, v1).await;
    let result = method::edit_step(&mut conn, v1, &s, StepEdit::Add(step("Nog een", None))).await;
    assert!(matches!(result, Err(Error::Invalid("steps"))), "{result:?}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn free_text_and_steps_never_both_hold_content(mut conn: PoolConnection<Postgres>) {
    let (_, v1) = recipe(&mut conn).await;
    assert!(
        matches!(
            method::save_text(&mut conn, v1, "Meng alles").await,
            Err(Error::Stale)
        ),
        "text needs the text kind first"
    );
    let s = seen(&mut conn, v1).await;
    method::set_kind(&mut conn, v1, &s, MethodKind::Text)
        .await
        .unwrap();
    let saved = method::save_text(&mut conn, v1, "  Meng alles.\r\n\r\nKneed 10 minuten. \n")
        .await
        .unwrap();
    assert_eq!(
        saved,
        Method::Text("Meng alles.\n\nKneed 10 minuten.".to_owned())
    );
    assert!(matches!(
        method::save_text(&mut conn, v1, &"x".repeat(MAX_TEXT + 1)).await,
        Err(Error::Invalid("method_text"))
    ));

    let text_stamp = seen(&mut conn, v1).await;
    let switched = method::set_kind(&mut conn, v1, &text_stamp, MethodKind::Steps)
        .await
        .unwrap();
    assert_eq!(switched, Method::Steps(Vec::new()), "the text goes");
    assert!(matches!(
        method::save_text(&mut conn, v1, "Meng").await,
        Err(Error::Stale)
    ));
    assert!(
        matches!(
            method::set_kind(&mut conn, v1, &text_stamp, MethodKind::None).await,
            Err(Error::Stale)
        ),
        "a switch from an out-of-date page"
    );

    to_steps(&mut conn, v1, &[step("Bulk", Some(60))]).await;
    let s = seen(&mut conn, v1).await;
    let same = method::set_kind(&mut conn, v1, &s, MethodKind::Steps)
        .await
        .unwrap();
    assert_eq!(names(&same), ["Bulk"], "the same kind keeps the steps");
    let none = method::set_kind(&mut conn, v1, &s, MethodKind::None)
        .await
        .unwrap();
    assert_eq!(none, Method::None, "the steps go");

    let writers = [
        "UPDATE recipe_version SET method_text = 'x' WHERE id = $1",
        "INSERT INTO recipe_step (recipe_version_id, position, name) VALUES ($1, 0, 'Bulk')",
    ];
    for sql in writers {
        let result = sqlx::query(sql).bind(v1.get()).execute(&mut *conn).await;
        assert!(result.is_err(), "{sql}");
    }
    to_steps(&mut conn, v1, &[step("Bulk", Some(60))]).await;
    let result = sqlx::query("UPDATE recipe_version SET method_kind = 'text' WHERE id = $1")
        .bind(v1.get())
        .execute(&mut *conn)
        .await;
    assert!(result.is_err(), "a kind change while steps exist");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn frozen_instructions_refuse_every_change(mut conn: PoolConnection<Postgres>) {
    let (recipe_id, v1) = recipe(&mut conn).await;
    to_steps(&mut conn, v1, &[step("Bulk", Some(60))]).await;
    freeze(&mut conn, recipe_id, v1).await;
    let s = seen(&mut conn, v1).await;
    let results = [
        method::edit_step(&mut conn, v1, &s, StepEdit::Add(step("Vormen", None)))
            .await
            .err(),
        method::set_kind(&mut conn, v1, &s, MethodKind::Text)
            .await
            .err(),
        method::save_text(&mut conn, v1, "Meng").await.err(),
    ];
    for result in results {
        assert!(matches!(result, Some(Error::Frozen)), "{result:?}");
    }
    let writers = [
        "UPDATE recipe_step SET name = 'Rijzen' WHERE recipe_version_id = $1",
        "DELETE FROM recipe_step WHERE recipe_version_id = $1",
        "INSERT INTO recipe_step (recipe_version_id, position, name) VALUES ($1, 1, 'Vormen')",
        "UPDATE recipe_version SET method_kind = 'none' WHERE id = $1",
    ];
    for sql in writers {
        let result = sqlx::query(sql).bind(v1.get()).execute(&mut *conn).await;
        assert!(result.is_err(), "{sql}");
    }
    assert_eq!(names(&current(&mut conn, v1).await), ["Bulk"]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_next_draft_copies_the_instructions(mut conn: PoolConnection<Postgres>) {
    let (recipe_id, v1) = recipe(&mut conn).await;
    to_steps(
        &mut conn,
        v1,
        &[step("Autolyse", Some(30)), step("Bulk", Some(240))],
    )
    .await;
    freeze(&mut conn, recipe_id, v1).await;
    let v2 = store::new_version(&mut conn, recipe_id).await.unwrap().id;
    assert_eq!(current(&mut conn, v2).await, current(&mut conn, v1).await);

    let s = seen(&mut conn, v2).await;
    method::edit_step(&mut conn, v2, &s, StepEdit::Remove { position: 0 })
        .await
        .unwrap();
    assert_eq!(names(&current(&mut conn, v1).await), ["Autolyse", "Bulk"]);
    assert_eq!(names(&current(&mut conn, v2).await), ["Bulk"]);

    let s = seen(&mut conn, v2).await;
    method::set_kind(&mut conn, v2, &s, MethodKind::Text)
        .await
        .unwrap();
    method::save_text(&mut conn, v2, "Kneden, rijzen, bakken.")
        .await
        .unwrap();
    freeze(&mut conn, recipe_id, v2).await;
    let saved = store::save_draft(
        &mut conn,
        v2,
        &DraftInput {
            lines: Vec::new(),
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(saved.version, 3);
    assert_eq!(
        current(&mut conn, saved.id).await,
        Method::Text("Kneden, rijzen, bakken.".to_owned()),
        "saving a frozen version keeps its instructions"
    );
}

async fn product(conn: &mut PgConnection, recipe: RecipeId) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
        VALUES ('Desembrood', 650, 9, 800, 900) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let product = ProductId::new(id);
    store::link_product(conn, recipe, product).await.unwrap();
    sqlx::query(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role)
            VALUES ('test-baker', 'Testbakker', 'baker') RETURNING id)
        UPDATE product SET allergens_approved_by = (SELECT id FROM baker),
            allergens_approved_at = '2026-11-01T10:00:00Z'
        WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await
    .unwrap();
    product
}

async fn approval(conn: &mut PgConnection, product: ProductId) -> (bool, i64) {
    sqlx::query_as(
        "SELECT allergens_approved_by IS NOT NULL, allergens_recipe_rev::bigint
        FROM product WHERE id = $1",
    )
    .bind(product.get())
    .fetch_one(conn)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn instructions_never_clear_the_allergen_approval(mut conn: PoolConnection<Postgres>) {
    let (recipe_id, v1) = recipe(&mut conn).await;
    let flour = flour(&mut conn).await;
    store::save_draft(
        &mut conn,
        v1,
        &DraftInput {
            lines: vec![LineInput {
                pct: Pct::HUNDRED,
                part: LinePart::Ingredient(flour),
            }],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    let bread = product(&mut conn, recipe_id).await;
    let before = approval(&mut conn, bread).await;
    assert!(before.0);

    to_steps(
        &mut conn,
        v1,
        &[step("Bulk", Some(60)), step("Bakken", None)],
    )
    .await;
    let s = seen(&mut conn, v1).await;
    method::edit_step(&mut conn, v1, &s, StepEdit::MoveUp { position: 1 })
        .await
        .unwrap();
    let s = seen(&mut conn, v1).await;
    method::set_kind(&mut conn, v1, &s, MethodKind::Text)
        .await
        .unwrap();
    method::save_text(&mut conn, v1, "Meng").await.unwrap();
    assert_eq!(approval(&mut conn, bread).await, before);
}
