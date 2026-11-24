#![forbid(unsafe_code)]

use catalog::{CharityInput, Error, charities};
use domain::CharityId;
use jiff::{SignedDuration, Timestamp};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn charity(name: &str) -> CharityInput {
    CharityInput {
        name: name.to_owned(),
        description: "Helpt gezinnen in de regio".to_owned(),
        url: Some("https://example.org".to_owned()),
        logo_path: None,
        iban: Some("NL91ABNA0417164300".to_owned()),
        anbi_rsin: Some("123456789".to_owned()),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn create_then_get_round_trips(mut conn: PoolConnection<Postgres>) {
    let created = charities::create(&mut conn, &charity("Voedselbank"))
        .await
        .unwrap();
    let fetched = charities::get(&mut conn, created.id).await.unwrap();

    assert_eq!(created, fetched);
    assert_eq!(fetched.name, "Voedselbank");
    assert_eq!(fetched.iban.as_deref(), Some("NL91ABNA0417164300"));
    assert_eq!(fetched.archived_at, None);
    assert!(fetched.is_active());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn empty_names_are_refused(mut conn: PoolConnection<Postgres>) {
    let created = charities::create(&mut conn, &charity("Voedselbank"))
        .await
        .unwrap();
    assert!(matches!(
        charities::create(&mut conn, &charity(" ")).await,
        Err(Error::Invalid("name"))
    ));
    assert!(matches!(
        charities::update(&mut conn, created.id, &charity("")).await,
        Err(Error::Invalid("name"))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn update_replaces_fields(mut conn: PoolConnection<Postgres>) {
    let created = charities::create(&mut conn, &charity("Voedselbank"))
        .await
        .unwrap();
    let input = CharityInput {
        name: "Voedselbank Westerveld".to_owned(),
        logo_path: Some("documents/logo.png".to_owned()),
        iban: None,
        ..charity("")
    };
    let updated = charities::update(&mut conn, created.id, &input)
        .await
        .unwrap();

    assert_eq!(updated.name, "Voedselbank Westerveld");
    assert_eq!(updated.logo_path.as_deref(), Some("documents/logo.png"));
    assert_eq!(updated.iban, None);
    assert_eq!(
        charities::get(&mut conn, created.id).await.unwrap(),
        updated
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn missing_charities_are_not_found(mut conn: PoolConnection<Postgres>) {
    let missing = CharityId::new(999);
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    assert!(matches!(
        charities::get(&mut conn, missing).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        charities::update(&mut conn, missing, &charity("x")).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        charities::archive(&mut conn, missing, &clock).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn archive_keeps_the_row_and_hides_it_from_customers(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    let kept = charities::create(&mut conn, &charity("Voedselbank"))
        .await
        .unwrap();
    let archived = charities::create(&mut conn, &charity("Dierenasiel"))
        .await
        .unwrap();

    let result = charities::archive(&mut conn, archived.id, &clock)
        .await
        .unwrap();
    assert_eq!(result.archived_at, Some(ts("2026-11-24T10:00:00Z")));
    assert!(!result.is_active());

    let active: Vec<_> = charities::list_active(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(active, [kept.id]);

    let all: Vec<_> = charities::list_all(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(all, [archived.id, kept.id]);
    assert_eq!(
        charities::get(&mut conn, archived.id).await.unwrap(),
        result
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn archiving_twice_keeps_the_first_time(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    let created = charities::create(&mut conn, &charity("Voedselbank"))
        .await
        .unwrap();
    charities::archive(&mut conn, created.id, &clock)
        .await
        .unwrap();
    clock.advance(SignedDuration::from_hours(24));
    let again = charities::archive(&mut conn, created.id, &clock)
        .await
        .unwrap();
    assert_eq!(again.archived_at, Some(ts("2026-11-24T10:00:00Z")));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn updating_an_archived_charity_keeps_it_archived(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-11-24T10:00:00Z"));
    let created = charities::create(&mut conn, &charity("Voedselbank"))
        .await
        .unwrap();
    charities::archive(&mut conn, created.id, &clock)
        .await
        .unwrap();
    let updated = charities::update(&mut conn, created.id, &charity("Voedselbank NL"))
        .await
        .unwrap();
    assert!(!updated.is_active());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_database_refuses_to_delete_a_charity(mut conn: PoolConnection<Postgres>) {
    let created = charities::create(&mut conn, &charity("Voedselbank"))
        .await
        .unwrap();
    let err = sqlx::query("DELETE FROM charity WHERE id = $1")
        .bind(created.id.get())
        .execute(&mut *conn)
        .await
        .expect_err("delete must be refused");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23001")
    );
    assert!(charities::get(&mut conn, created.id).await.is_ok());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn later_tables_can_reference_charities(mut conn: PoolConnection<Postgres>) {
    let created = charities::create(&mut conn, &charity("Voedselbank"))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE allocation_probe (charity_id bigint NOT NULL REFERENCES charity (id))",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("INSERT INTO allocation_probe VALUES ($1)")
        .bind(created.id.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(
        sqlx::query("INSERT INTO allocation_probe VALUES (999)")
            .execute(&mut *conn)
            .await
            .is_err()
    );
}
