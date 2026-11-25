#![forbid(unsafe_code)]

use db::PgPool;

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn unused_columns_and_tables_are_dropped(pool: PgPool) {
    let left: Vec<String> = sqlx::query_scalar(
        "SELECT table_name || '.' || column_name FROM information_schema.columns
        WHERE table_schema = 'public' AND (table_name, column_name) IN (
            ('bake_day_product', 'capacity_pool_id'),
            ('bake_day_product', 'units_per_item'),
            ('company', 'default_lead_time_hours'),
            ('donation_allocation', 'choose_token'),
            ('product', 'lead_time_hours'),
            ('product', 'may_contain'))
        UNION ALL
        SELECT table_name FROM information_schema.tables
        WHERE table_schema = 'public' AND table_name = 'capacity_pool'
        UNION ALL
        SELECT conname FROM pg_constraint WHERE conname = 'email_outbox_kind_check'
        ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .expect("schema query");
    assert_eq!(left, Vec::<String>::new());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn email_outbox_refuses_an_unknown_or_empty_kind(pool: PgPool) {
    for kind in ["no_such_kind", ""] {
        let err = sqlx::query(
            "INSERT INTO email_outbox
                (kind, to_address, subject, text_body, html_body, next_attempt_at, created_at)
            VALUES ($1, 'a@example.org', 's', 't', 'h', now(), now())",
        )
        .bind(kind)
        .execute(&pool)
        .await
        .expect_err(kind);
        let code = err.as_database_error().and_then(|e| e.code());
        assert_eq!(code.as_deref(), Some("23503"), "{kind:?}: {err}");
    }
}
