#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use catalog::company::KitchenMayContain;
use catalog::{
    AllergenInfo, Approval, ApprovalCleared, Error, Pricing, ProductInput, PublishedAllergens,
    products,
};
use domain::{Allergen, InvalidPriceRange, Money, ProductId, StaffUserId, VatRate};
use jiff::{SignedDuration, Timestamp};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

const BAKER: StaffUserId = StaffUserId::new(7);

fn eur(cents: i64) -> Money {
    Money::from_cents(cents)
}

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn allergen_info() -> AllergenInfo {
    AllergenInfo {
        allergens: BTreeSet::from([Allergen::CerealsContainingGluten]),
        ingredient_text: "TARWEbloem, water, zout".to_owned(),
    }
}

fn kitchen(approved: bool) -> KitchenMayContain {
    KitchenMayContain {
        allergens: BTreeSet::from([Allergen::SesameSeeds, Allergen::CerealsContainingGluten]),
        approval: approved.then(|| Approval {
            by: BAKER,
            at: ts("2026-11-01T10:00:00Z"),
        }),
    }
}

fn sourdough() -> ProductInput {
    ProductInput {
        name_nl: "Desembrood wit".to_owned(),
        description_nl: "Licht zuur, krokante korst".to_owned(),
        photo_path: None,
        price_incl: eur(650),
        vat_rate: VatRate::Nine,
        pay_what_you_want: None,
        unit_weight_g: 800,
        dough_g_per_piece: 900,
        freezer_days: Some(90),
        allergen_info: allergen_info(),
        packaged: false,
        sold_online: true,
        estimated_cost: Some(eur(140)),
        active: true,
    }
}

/// Approvals name a staff user, so `BAKER` has to exist.
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

async fn approved(conn: &mut PgConnection, clock: &FakeClock) -> ProductId {
    add_baker(conn).await;
    let product = products::create(conn, &sourdough()).await.unwrap();
    products::approve_allergens(
        conn,
        product.id,
        &product.allergen_info,
        product.allergen_revision,
        BAKER,
        clock,
    )
    .await
    .unwrap();
    product.id
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn create_then_get_round_trips(mut conn: PoolConnection<Postgres>) {
    let created = products::create(&mut conn, &sourdough()).await.unwrap();
    let fetched = products::get(&mut conn, created.id).await.unwrap();

    assert_eq!(created, fetched);
    assert_eq!(fetched.name_nl, "Desembrood wit");
    assert_eq!(fetched.pricing, Pricing::Fixed(eur(650)));
    assert_eq!(fetched.pricing.price_incl(), eur(650));
    assert_eq!(fetched.vat_rate, VatRate::Nine);
    assert_eq!(fetched.allergen_info, allergen_info());
    assert_eq!(fetched.estimated_cost, Some(eur(140)));
    assert_eq!(fetched.freezer_days, Some(90));
    assert_eq!(fetched.approval, None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn new_products_publish_no_allergen_data(mut conn: PoolConnection<Postgres>) {
    let product = products::create(&mut conn, &sourdough()).await.unwrap();
    assert_eq!(product.published_allergens(&kitchen(true)), None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn pay_what_you_want_round_trips(mut conn: PoolConnection<Postgres>) {
    let input = ProductInput {
        price_incl: eur(600),
        pay_what_you_want: Some((eur(300), eur(1500))),
        vat_rate: VatRate::TwentyOne,
        ..sourdough()
    };
    let product = products::create(&mut conn, &input).await.unwrap();

    let Pricing::PayWhatYouWant(pwyw) = product.pricing else {
        panic!("expected pay what you want, got {:?}", product.pricing);
    };
    assert_eq!(
        (pwyw.suggested(), pwyw.min(), pwyw.max()),
        (eur(600), eur(300), eur(1500))
    );
    assert_eq!(product.pricing.price_incl(), eur(600));
    assert_eq!(product.vat_rate, VatRate::TwentyOne);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn invalid_pay_what_you_want_bounds_are_refused_before_writing(
    mut conn: PoolConnection<Postgres>,
) {
    let below_min = ProductInput {
        price_incl: eur(200),
        pay_what_you_want: Some((eur(300), eur(1500))),
        ..sourdough()
    };
    let zero_min = ProductInput {
        pay_what_you_want: Some((eur(0), eur(1500))),
        ..sourdough()
    };
    let negative_min = ProductInput {
        pay_what_you_want: Some((eur(-1), eur(1500))),
        ..sourdough()
    };
    assert!(matches!(
        products::create(&mut conn, &below_min).await,
        Err(Error::Price(InvalidPriceRange::SuggestedOutsideRange))
    ));
    for input in [&zero_min, &negative_min] {
        assert!(matches!(
            products::create(&mut conn, input).await,
            Err(Error::Price(InvalidPriceRange::MinimumNotPositive))
        ));
    }

    let product = products::create(&mut conn, &sourdough()).await.unwrap();
    assert!(matches!(
        products::update(&mut conn, product.id, &below_min).await,
        Err(Error::Price(_))
    ));
    assert_eq!(products::list_active(&mut conn).await.unwrap().len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn invalid_fields_are_refused(mut conn: PoolConnection<Postgres>) {
    let cases = [
        (
            ProductInput {
                name_nl: "  ".to_owned(),
                ..sourdough()
            },
            "name",
        ),
        (
            ProductInput {
                price_incl: eur(-1),
                ..sourdough()
            },
            "price",
        ),
        (
            ProductInput {
                unit_weight_g: 0,
                ..sourdough()
            },
            "unit weight",
        ),
        (
            ProductInput {
                dough_g_per_piece: -1,
                ..sourdough()
            },
            "dough weight",
        ),
        (
            ProductInput {
                freezer_days: Some(0),
                ..sourdough()
            },
            "freezer days",
        ),
        (
            ProductInput {
                estimated_cost: Some(eur(-1)),
                ..sourdough()
            },
            "estimated cost",
        ),
    ];
    for (input, field) in cases {
        match products::create(&mut conn, &input).await {
            Err(Error::Invalid(got)) => assert_eq!(got, field),
            other => panic!("{field}: expected Invalid, got {other:?}"),
        }
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn update_replaces_fields(mut conn: PoolConnection<Postgres>) {
    let product = products::create(&mut conn, &sourdough()).await.unwrap();
    let input = ProductInput {
        name_nl: "Desembrood volkoren".to_owned(),
        price_incl: eur(700),
        sold_online: false,
        estimated_cost: None,
        ..sourdough()
    };
    let updated = products::update(&mut conn, product.id, &input)
        .await
        .unwrap();

    assert_eq!(updated.id, product.id);
    assert_eq!(updated.name_nl, "Desembrood volkoren");
    assert_eq!(updated.pricing, Pricing::Fixed(eur(700)));
    assert!(!updated.sold_online);
    assert_eq!(updated.estimated_cost, None);
    assert_eq!(products::get(&mut conn, product.id).await.unwrap(), updated);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn missing_products_are_not_found(mut conn: PoolConnection<Postgres>) {
    let missing = ProductId::new(999);
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    assert!(matches!(
        products::get(&mut conn, missing).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        products::update(&mut conn, missing, &sourdough()).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        products::approve_allergens(&mut conn, missing, &allergen_info(), 0, BAKER, &clock).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn approval_records_who_and_when_and_publishes(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-03-29T01:30:00.250Z"));
    let id = approved(&mut conn, &clock).await;

    let product = products::get(&mut conn, id).await.unwrap();
    let approval = product.approval.expect("approved");
    assert_eq!(approval.by, BAKER);
    assert_eq!(approval.at, ts("2026-03-29T01:30:00.250Z"));
    assert_eq!(
        product.published_allergens(&kitchen(true)),
        Some(PublishedAllergens {
            contains: BTreeSet::from([Allergen::CerealsContainingGluten]),
            may_contain: BTreeSet::from([Allergen::SesameSeeds]),
            ingredient_text: "TARWEbloem, water, zout".to_owned(),
        })
    );
    assert_eq!(product.published_allergens(&kitchen(false)), None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn approving_stale_allergen_data_is_refused(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    let product = products::create(&mut conn, &sourdough()).await.unwrap();
    let mut reviewed = product.allergen_info.clone();
    reviewed.allergens.insert(Allergen::Milk);

    assert!(matches!(
        products::approve_allergens(
            &mut conn,
            product.id,
            &reviewed,
            product.allergen_revision,
            BAKER,
            &clock
        )
        .await,
        Err(Error::AllergensChanged)
    ));
    assert_eq!(
        products::get(&mut conn, product.id).await.unwrap().approval,
        None
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn changing_allergen_data_clears_approval(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    let mut milk = allergen_info();
    milk.allergens.insert(Allergen::Milk);
    let mut new_text = allergen_info();
    new_text.ingredient_text = "TARWEbloem, water, zout, MELK".to_owned();

    for changed in [milk, new_text] {
        let id = approved(&mut conn, &clock).await;
        let input = ProductInput {
            allergen_info: changed.clone(),
            ..sourdough()
        };
        let updated = products::update(&mut conn, id, &input).await.unwrap();
        assert_eq!(updated.approval, None, "{changed:?}");
        assert_eq!(updated.published_allergens(&kitchen(true)), None);
        assert_eq!(updated.allergen_info, changed);
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn other_changes_keep_approval(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    let id = approved(&mut conn, &clock).await;
    clock.advance(SignedDuration::from_hours(1));

    let input = ProductInput {
        price_incl: eur(675),
        description_nl: "Nieuwe tekst".to_owned(),
        ..sourdough()
    };
    let updated = products::update(&mut conn, id, &input).await.unwrap();
    let approval = updated.approval.expect("still approved");
    assert_eq!(approval.at, ts("2026-11-24T10:00:00Z"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn list_active_skips_inactive_products(mut conn: PoolConnection<Postgres>) {
    let a = products::create(
        &mut conn,
        &ProductInput {
            name_nl: "B-brood".to_owned(),
            ..sourdough()
        },
    )
    .await
    .unwrap();
    let b = products::create(
        &mut conn,
        &ProductInput {
            name_nl: "A-brood".to_owned(),
            sold_online: false,
            ..sourdough()
        },
    )
    .await
    .unwrap();
    products::create(
        &mut conn,
        &ProductInput {
            active: false,
            ..sourdough()
        },
    )
    .await
    .unwrap();

    let ids: Vec<_> = products::list_active(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, [b.id, a.id]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn list_all_includes_inactive_products_by_name(mut conn: PoolConnection<Postgres>) {
    let active = products::create(&mut conn, &sourdough()).await.unwrap();
    let inactive = products::create(
        &mut conn,
        &ProductInput {
            name_nl: "Appeltaart".to_owned(),
            active: false,
            ..sourdough()
        },
    )
    .await
    .unwrap();

    let ids: Vec<_> = products::list_all(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, [inactive.id, active.id]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn products_not_sold_online_never_reach_the_webshop_list(mut conn: PoolConnection<Postgres>) {
    let online = products::create(&mut conn, &sourdough()).await.unwrap();
    for input in [
        ProductInput {
            name_nl: "Granola".to_owned(),
            sold_online: false,
            packaged: true,
            ..sourdough()
        },
        ProductInput {
            name_nl: "Oud brood".to_owned(),
            active: false,
            ..sourdough()
        },
    ] {
        products::create(&mut conn, &input).await.unwrap();
    }

    let listed = products::list_sold_online(&mut conn).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, online.id);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn stored_rows_that_break_domain_rules_are_reported(mut conn: PoolConnection<Postgres>) {
    let product = products::create(&mut conn, &sourdough()).await.unwrap();
    sqlx::query("ALTER TABLE product DROP CONSTRAINT product_pwyw_bounds")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE product SET pay_what_you_want = true WHERE id = $1")
        .bind(product.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(matches!(
        products::get(&mut conn, product.id).await,
        Err(Error::Corrupt(_))
    ));

    sqlx::query("UPDATE product SET min_price_cents = 900, max_price_cents = 1000 WHERE id = $1")
        .bind(product.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(matches!(
        products::get(&mut conn, product.id).await,
        Err(Error::Corrupt(_))
    ));

    sqlx::query("ALTER TABLE product DROP CONSTRAINT product_vat_rate_check")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE product SET pay_what_you_want = false, min_price_cents = NULL, max_price_cents = NULL, vat_rate = 6 WHERE id = $1")
        .bind(product.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(matches!(
        products::get(&mut conn, product.id).await,
        Err(Error::Corrupt(_))
    ));
}

async fn insert_raw(conn: &mut PgConnection, columns: &str, values: &str) -> sqlx::Error {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece{columns})
         VALUES ('Brood', 500, 9, 800, 900{values})"
    )))
    .execute(conn)
    .await
    .expect_err("constraint should refuse the row")
}

fn constraint(err: &sqlx::Error) -> Option<&str> {
    err.as_database_error().and_then(|e| e.constraint())
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn database_refuses_bad_vat_rates(mut conn: PoolConnection<Postgres>) {
    for rate in ["0", "6", "19", "-9"] {
        let err = sqlx::query(
            "INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g, dough_g_per_piece)
             VALUES ('Brood', 500, $1::smallint, 800, 900)",
        )
        .bind(rate)
        .execute(&mut *conn)
        .await
        .expect_err(rate);
        assert_eq!(constraint(&err), Some("product_vat_rate_check"), "{rate}");
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn database_refuses_unknown_allergen_codes(mut conn: PoolConnection<Postgres>) {
    let err = insert_raw(&mut conn, ", allergens", ", '{gluten}'").await;
    assert_eq!(constraint(&err), Some("product_allergens_check"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn database_refuses_inconsistent_prices_and_approvals(mut conn: PoolConnection<Postgres>) {
    let cases = [
        (", pay_what_you_want", ", true"),
        (
            ", pay_what_you_want, min_price_cents, max_price_cents",
            ", true, 600, 1000",
        ),
        (
            ", pay_what_you_want, min_price_cents, max_price_cents",
            ", true, 0, 1000",
        ),
        (", min_price_cents, max_price_cents", ", 100, 1000"),
    ];
    for (columns, values) in cases {
        let err = insert_raw(&mut conn, columns, values).await;
        assert_eq!(constraint(&err), Some("product_pwyw_bounds"), "{values}");
    }
    let err = insert_raw(&mut conn, ", allergens_approved_by", ", 1").await;
    assert_eq!(constraint(&err), Some("product_approval_complete"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn any_writer_changing_allergen_data_clears_approval(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    for change in ["allergens = '{milk}'", "ingredient_text = 'iets anders'"] {
        let id = approved(&mut conn, &clock).await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE product SET {change} WHERE id = $1"
        )))
        .bind(id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            products::get(&mut conn, id).await.unwrap().approval,
            None,
            "{change}"
        );
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn every_allergen_code_is_accepted_and_approvable(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    let all = BTreeSet::from(Allergen::ALL);
    for info in [
        AllergenInfo {
            allergens: all.clone(),
            ingredient_text: "alles".to_owned(),
        },
        AllergenInfo::default(),
    ] {
        let input = ProductInput {
            allergen_info: info.clone(),
            ..sourdough()
        };
        let product = products::create(&mut conn, &input).await.unwrap();
        assert_eq!(product.allergen_info, info);
        add_baker(&mut conn).await;
        products::approve_allergens(
            &mut conn,
            product.id,
            &info,
            product.allergen_revision,
            BAKER,
            &clock,
        )
        .await
        .unwrap();
        let renamed = ProductInput {
            name_nl: "Hernoemd".to_owned(),
            ..input
        };
        let updated = products::update(&mut conn, product.id, &renamed)
            .await
            .unwrap();
        assert!(updated.approval.is_some(), "{info:?}");
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn approval_ignores_the_stored_order_of_allergens(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    let product = products::create(&mut conn, &sourdough()).await.unwrap();
    sqlx::query(
        "UPDATE product SET allergens = '{sesame_seeds,cereals_containing_gluten}' WHERE id = $1",
    )
    .bind(product.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    add_baker(&mut conn).await;
    let mut reviewed = allergen_info();
    reviewed.allergens.insert(Allergen::SesameSeeds);
    let approved = products::approve_allergens(&mut conn, product.id, &reviewed, 0, BAKER, &clock)
        .await
        .unwrap();
    assert!(approved.approval.is_some());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_recipe_change_says_why_until_the_next_approval(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    let id = approved(&mut conn, &clock).await;
    sqlx::query(
        "WITH r AS (INSERT INTO recipe (name, kind) VALUES ('Wit', 'dough') RETURNING id)
        UPDATE product SET recipe_id = (SELECT id FROM r) WHERE id = $1",
    )
    .bind(id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    let cleared = products::get(&mut conn, id).await.unwrap();
    assert_eq!(cleared.approval, None);
    assert_eq!(cleared.approval_cleared, Some(ApprovalCleared::Recipe));

    let computed = catalog::ComputedAllergens {
        allergens: cleared.allergen_info.allergens.clone(),
        ingredients: catalog::IngredientList(vec![catalog::ListedIngredient {
            name: "tarwebloem".to_owned(),
            emphasised: true,
        }]),
    };
    let again = products::approve_computed(
        &mut conn,
        id,
        &computed,
        cleared.allergen_revision,
        BAKER,
        &clock,
    )
    .await
    .unwrap();
    assert!(again.approval.is_some());
    assert_eq!(again.approval_cleared, None);
    assert_eq!(products::get(&mut conn, id).await.unwrap(), again);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_product_with_a_recipe_takes_only_computed_values(mut conn: PoolConnection<Postgres>) {
    add_baker(&mut conn).await;
    let clock = FakeClock::new(ts("2026-11-01T10:00:00Z"));
    let product = products::create(&mut conn, &sourdough()).await.unwrap();
    assert!(!product.has_recipe);
    sqlx::query(
        "WITH r AS (INSERT INTO recipe (name, kind) VALUES ('Desem', 'dough') RETURNING id)
         UPDATE product SET recipe_id = (SELECT id FROM r) WHERE id = $1",
    )
    .bind(product.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    let linked = products::get(&mut conn, product.id).await.unwrap();
    assert!(linked.has_recipe);

    let hand = products::approve_allergens(
        &mut conn,
        product.id,
        &linked.allergen_info,
        linked.allergen_revision,
        BAKER,
        &clock,
    )
    .await;
    assert!(matches!(hand, Err(Error::ComputedOnly)), "{hand:?}");
    let stale = products::approve_allergens(
        &mut conn,
        product.id,
        &linked.allergen_info,
        linked.allergen_revision - 1,
        BAKER,
        &clock,
    )
    .await;
    assert!(matches!(stale, Err(Error::ComputedOnly)), "{stale:?}");

    let computed = catalog::ComputedAllergens {
        allergens: BTreeSet::from([Allergen::CerealsContainingGluten]),
        ingredients: catalog::IngredientList(vec![catalog::ListedIngredient {
            name: "tarwebloem".to_owned(),
            emphasised: true,
        }]),
    };
    let approved = products::approve_computed(
        &mut conn,
        product.id,
        &computed,
        linked.allergen_revision,
        BAKER,
        &clock,
    )
    .await
    .unwrap();
    assert!(approved.approval.is_some());
}
