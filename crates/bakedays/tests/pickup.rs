#![forbid(unsafe_code)]

use bakedays::{Error, PickupLocationInput, PickupWindowInput, pickup};
use domain::{PickupLocationId, PickupWindowId};
use jiff::civil::{Weekday, time};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;

fn location(name: &str) -> PickupLocationInput {
    PickupLocationInput {
        name: name.to_owned(),
        address: "Dorpsstraat 1, 8380 AA Dorp".to_owned(),
        notes: "Achterom".to_owned(),
    }
}

fn friday_evening() -> PickupWindowInput {
    PickupWindowInput {
        weekday: Weekday::Friday,
        starts: time(17, 0, 0, 0),
        ends: time(18, 30, 0, 0),
        default_deadline_days_before: 1,
        default_deadline_time: time(12, 0, 0, 0),
        active: true,
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn de_proef_is_seeded_with_a_saturday_window(mut conn: PoolConnection<Postgres>) {
    let locations = pickup::list_locations(&mut conn).await.unwrap();
    assert_eq!(locations.len(), 1);
    let de_proef = &locations[0];
    assert_eq!(de_proef.name, "De Proef");
    assert_eq!(
        de_proef.address,
        "Majoor van Swietenlaan 15, 8382 CE Frederiksoord"
    );

    let windows = pickup::list_windows(&mut conn, de_proef.id).await.unwrap();
    assert_eq!(windows.len(), 1);
    let window = windows[0];
    assert_eq!(window.location_id, de_proef.id);
    assert_eq!(window.weekday, Weekday::Saturday);
    assert_eq!(
        (window.starts, window.ends),
        (time(11, 0, 0, 0), time(12, 0, 0, 0))
    );
    assert_eq!(window.default_deadline_days_before, 2);
    assert_eq!(window.default_deadline_time, time(20, 0, 0, 0));
    assert!(window.active);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn locations_create_update_get_list(mut conn: PoolConnection<Postgres>) {
    let created = pickup::create_location(&mut conn, &location("Markt"))
        .await
        .unwrap();
    assert_eq!(
        pickup::get_location(&mut conn, created.id).await.unwrap(),
        created
    );

    let updated = pickup::update_location(
        &mut conn,
        created.id,
        &PickupLocationInput {
            notes: String::new(),
            ..location("Aardige markt")
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.name, "Aardige markt");
    assert_eq!(updated.notes, "");

    let names: Vec<_> = pickup::list_locations(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(names, ["Aardige markt", "De Proef"]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn invalid_locations_are_refused(mut conn: PoolConnection<Postgres>) {
    assert!(matches!(
        pickup::create_location(&mut conn, &location(" ")).await,
        Err(Error::Invalid("name"))
    ));
    let no_address = PickupLocationInput {
        address: String::new(),
        ..location("Markt")
    };
    assert!(matches!(
        pickup::create_location(&mut conn, &no_address).await,
        Err(Error::Invalid("address"))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn windows_create_update_list(mut conn: PoolConnection<Postgres>) {
    let markt = pickup::create_location(&mut conn, &location("Markt"))
        .await
        .unwrap();
    let friday = pickup::create_window(&mut conn, markt.id, &friday_evening())
        .await
        .unwrap();
    assert_eq!(friday.location_id, markt.id);
    assert_eq!(friday.weekday, Weekday::Friday);
    assert_eq!(friday.ends, time(18, 30, 0, 0));

    let wednesday = pickup::create_window(
        &mut conn,
        markt.id,
        &PickupWindowInput {
            weekday: Weekday::Wednesday,
            ..friday_evening()
        },
    )
    .await
    .unwrap();

    let moved = pickup::update_window(
        &mut conn,
        friday.id,
        &PickupWindowInput {
            weekday: Weekday::Sunday,
            starts: time(9, 15, 30, 0),
            active: false,
            ..friday_evening()
        },
    )
    .await
    .unwrap();
    assert_eq!(moved.weekday, Weekday::Sunday);
    assert_eq!(moved.starts, time(9, 15, 30, 0));
    assert!(!moved.active);

    let windows = pickup::list_windows(&mut conn, markt.id).await.unwrap();
    assert_eq!(windows, [wednesday, moved]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn invalid_windows_are_refused(mut conn: PoolConnection<Postgres>) {
    let id = pickup::list_locations(&mut conn).await.unwrap()[0].id;
    let cases = [
        PickupWindowInput {
            ends: time(17, 0, 0, 0),
            ..friday_evening()
        },
        PickupWindowInput {
            starts: time(19, 0, 0, 0),
            ..friday_evening()
        },
    ];
    for input in cases {
        assert!(matches!(
            pickup::create_window(&mut conn, id, &input).await,
            Err(Error::Invalid("pickup window"))
        ));
    }
    for days in [-1, 29] {
        let input = PickupWindowInput {
            default_deadline_days_before: days,
            ..friday_evening()
        };
        assert!(matches!(
            pickup::create_window(&mut conn, id, &input).await,
            Err(Error::Invalid("deadline days before"))
        ));
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn missing_rows_are_not_found(mut conn: PoolConnection<Postgres>) {
    let missing = PickupLocationId::new(999);
    assert!(matches!(
        pickup::get_location(&mut conn, missing).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        pickup::update_location(&mut conn, missing, &location("x")).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        pickup::create_window(&mut conn, missing, &friday_evening()).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        pickup::update_window(&mut conn, PickupWindowId::new(999), &friday_evening()).await,
        Err(Error::NotFound)
    ));
    assert!(
        pickup::list_windows(&mut conn, missing)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn database_refuses_bad_windows(mut conn: PoolConnection<Postgres>) {
    let id = pickup::list_locations(&mut conn).await.unwrap()[0].id;
    let insert = |weekday: i16, starts: &'static str, ends: &'static str| {
        sqlx::query(
            "INSERT INTO pickup_window (location_id, weekday, starts, ends,
                default_deadline_days_before, default_deadline_time)
             VALUES ($1, $2, $3::time, $4::time, 2, '20:00')",
        )
        .bind(id.get())
        .bind(weekday)
        .bind(starts)
        .bind(ends)
    };
    for (weekday, starts, ends) in [
        (0, "11:00", "12:00"),
        (8, "11:00", "12:00"),
        (6, "12:00", "11:00"),
    ] {
        assert!(
            insert(weekday, starts, ends)
                .execute(&mut *conn)
                .await
                .is_err(),
            "{weekday} {starts}-{ends}"
        );
    }
    insert(7, "11:00", "12:00")
        .execute(&mut *conn)
        .await
        .unwrap();
}
