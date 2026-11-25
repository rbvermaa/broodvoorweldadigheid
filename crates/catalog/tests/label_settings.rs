#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use catalog::label_settings::{self, LabelSettings};
use catalog::{AllergenInfo, Error, ProductInput, company, products};
use domain::{Money, ProductId, VatRate};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;

fn cookies() -> ProductInput {
    ProductInput {
        name_nl: "Havermoutkoekjes".to_owned(),
        description_nl: String::new(),
        photo_path: None,
        price_incl: Money::from_cents(450),
        vat_rate: VatRate::Nine,
        pay_what_you_want: None,
        unit_weight_g: 200,
        dough_g_per_piece: 210,
        freezer_days: None,
        lead_time_hours: None,
        allergen_info: AllergenInfo {
            allergens: BTreeSet::new(),
            ingredient_text: "havermout".to_owned(),
        },
        packaged: false,
        sold_online: false,
        estimated_cost: None,
        active: true,
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn label_settings_round_trip(mut conn: PoolConnection<Postgres>) {
    let id = products::create(&mut conn, &cookies()).await.unwrap().id;
    assert_eq!(
        label_settings::get(&mut conn, id).await.unwrap(),
        LabelSettings::default()
    );
    let settings = LabelSettings {
        packaged: true,
        shelf_life_days: Some(42),
        freezer_days: Some(90),
        storage: "Droog en koel bewaren.".to_owned(),
    };
    label_settings::set(
        &mut conn,
        id,
        &LabelSettings {
            storage: "  Droog en koel bewaren. ".to_owned(),
            ..settings.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(label_settings::get(&mut conn, id).await.unwrap(), settings);
    assert_eq!(
        products::get(&mut conn, id).await.unwrap().freezer_days,
        Some(90)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn invalid_label_settings_are_refused(mut conn: PoolConnection<Postgres>) {
    let id = products::create(&mut conn, &cookies()).await.unwrap().id;
    for (settings, field) in [
        (
            LabelSettings {
                shelf_life_days: Some(0),
                ..LabelSettings::default()
            },
            "shelf life",
        ),
        (
            LabelSettings {
                freezer_days: Some(label_settings::MAX_DAYS + 1),
                ..LabelSettings::default()
            },
            "freezer period",
        ),
        (
            LabelSettings {
                storage: "x".repeat(label_settings::MAX_STORAGE_CHARS + 1),
                ..LabelSettings::default()
            },
            "storage instruction",
        ),
    ] {
        assert!(
            matches!(
                label_settings::set(&mut conn, id, &settings).await,
                Err(Error::Invalid(f)) if f == field
            ),
            "{field}"
        );
    }
    let at_limits = LabelSettings {
        packaged: false,
        shelf_life_days: Some(1),
        freezer_days: Some(label_settings::MAX_DAYS),
        storage: "x".repeat(label_settings::MAX_STORAGE_CHARS),
    };
    label_settings::set(&mut conn, id, &at_limits)
        .await
        .unwrap();
    assert_eq!(label_settings::get(&mut conn, id).await.unwrap(), at_limits);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn unknown_products_have_no_label_settings(mut conn: PoolConnection<Postgres>) {
    let missing = ProductId::new(999_999);
    assert!(matches!(
        label_settings::get(&mut conn, missing).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        label_settings::set(&mut conn, missing, &LabelSettings::default()).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_company_address_is_set_and_cleared(mut conn: PoolConnection<Postgres>) {
    assert_eq!(company::contact(&mut conn).await.unwrap().address, None);
    company::set_address(&mut conn, " Voorbeeldlaan 1, 1234 AB Voorbeeld ")
        .await
        .unwrap();
    assert_eq!(
        company::contact(&mut conn)
            .await
            .unwrap()
            .address
            .as_deref(),
        Some("Voorbeeldlaan 1, 1234 AB Voorbeeld")
    );
    company::set_address(&mut conn, "  ").await.unwrap();
    assert_eq!(company::contact(&mut conn).await.unwrap().address, None);
    assert!(matches!(
        company::set_address(&mut conn, &"x".repeat(company::MAX_ADDRESS_CHARS + 1)).await,
        Err(Error::Invalid("address"))
    ));
}
