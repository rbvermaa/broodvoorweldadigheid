#![forbid(unsafe_code)]

use db::PgPool;

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn company_settings_are_seeded(pool: PgPool) {
    let name: String = sqlx::query_scalar("SELECT name FROM company")
        .fetch_one(&pool)
        .await
        .expect("one company row");
    assert_eq!(name, "Brood voor Weldadigheid");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn company_is_a_single_row(pool: PgPool) {
    let second = sqlx::query("INSERT INTO company (id, name) VALUES (2, 'x')")
        .execute(&pool)
        .await;
    assert!(second.is_err());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn de_proef_saturday_window_is_seeded(pool: PgPool) {
    let row: (String, String, i16, String, String, i16, String) = sqlx::query_as(
        "SELECT l.name, l.address, w.weekday, w.starts::text, w.ends::text,
            w.default_deadline_days_before, w.default_deadline_time::text
         FROM pickup_location l JOIN pickup_window w ON w.location_id = l.id",
    )
    .fetch_one(&pool)
    .await
    .expect("one seeded window");
    assert_eq!(
        row,
        (
            "De Proef".to_owned(),
            "Majoor van Swietenlaan 15, 8382 CE Frederiksoord".to_owned(),
            6,
            "11:00:00".to_owned(),
            "12:00:00".to_owned(),
            2,
            "20:00:00".to_owned(),
        )
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn company_row_cannot_be_removed(pool: PgPool) {
    for sql in ["DELETE FROM company", "TRUNCATE company"] {
        assert!(sqlx::query(sql).execute(&pool).await.is_err(), "{sql}");
    }
}
