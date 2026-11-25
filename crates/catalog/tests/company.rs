#![forbid(unsafe_code)]

use catalog::{SourdoughDeadline, company};
use jiff::civil::time;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sourdough_deadline_defaults_to_20_00_two_days_before(mut conn: PoolConnection<Postgres>) {
    assert_eq!(
        company::sourdough_deadline(&mut conn).await.unwrap(),
        SourdoughDeadline {
            days_before: 2,
            at: time(20, 0, 0, 0),
        }
    );
    sqlx::query(
        "UPDATE company SET sourdough_deadline_days_before = 1, sourdough_deadline_time = '17:30'",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    assert_eq!(
        company::sourdough_deadline(&mut conn).await.unwrap(),
        SourdoughDeadline {
            days_before: 1,
            at: time(17, 30, 0, 0),
        }
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sourdough_deadline_days_are_limited(mut conn: PoolConnection<Postgres>) {
    for days in [-1_i16, 15] {
        let err = sqlx::query("UPDATE company SET sourdough_deadline_days_before = $1")
            .bind(days)
            .execute(&mut *conn)
            .await
            .unwrap_err();
        assert_eq!(
            err.as_database_error().and_then(|e| e.constraint()),
            Some("company_sourdough_deadline_days_before")
        );
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_public_donation_page_is_off_until_switched_on(mut conn: PoolConnection<Postgres>) {
    assert!(!company::public_donations(&mut conn).await.unwrap());
    company::set_public_donations(&mut conn, true)
        .await
        .unwrap();
    assert!(company::public_donations(&mut conn).await.unwrap());
    company::set_public_donations(&mut conn, false)
        .await
        .unwrap();
    assert!(!company::public_donations(&mut conn).await.unwrap());
}
