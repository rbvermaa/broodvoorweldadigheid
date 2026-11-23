#![forbid(unsafe_code)]

use std::net::TcpListener;
use std::time::Duration;

use db::PgPool;

fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    format!("postgres://postgres@127.0.0.1:{port}/postgres")
}

#[sqlx::test(migrations = false)]
async fn migrate_creates_schema_info(pool: PgPool) {
    db::migrate(&pool).await.expect("migrate");

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM schema_info")
        .fetch_one(&pool)
        .await
        .expect("schema_info exists");
    assert_eq!(rows, 1);
}

#[sqlx::test(migrations = false)]
async fn migrate_twice_is_a_no_op(pool: PgPool) {
    db::migrate(&pool).await.expect("first migrate");
    db::migrate(&pool).await.expect("second migrate");
}

#[sqlx::test(migrations = false)]
async fn ping_succeeds_on_a_live_database(pool: PgPool) {
    db::ping(&pool).await.expect("ping");
}

#[tokio::test]
async fn ping_fails_when_the_database_is_unreachable() {
    let pool = db::connect_lazy(&closed_port_url(), Duration::from_millis(300)).expect("lazy pool");
    assert!(db::ping(&pool).await.is_err());
}

#[tokio::test]
async fn connect_fails_when_the_database_is_unreachable() {
    assert!(db::connect(&closed_port_url()).await.is_err());
}
