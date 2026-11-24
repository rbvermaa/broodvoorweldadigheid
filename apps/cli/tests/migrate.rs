#![forbid(unsafe_code)]

use std::process::ExitCode;

use cli::{Command, backup, execute};
use sqlx::PgPool;

fn admin_url() -> String {
    std::env::var("DATABASE_URL").expect(
        "DATABASE_URL must point at a Postgres superuser connection; `nix flake check` starts one",
    )
}

#[sqlx::test(migrations = false)]
async fn migrate_applies_every_migration_and_is_repeatable(pool: PgPool) {
    let database = pool
        .connect_options()
        .get_database()
        .expect("test database name")
        .to_owned();
    let database_url = backup::with_database(&admin_url(), &database).expect("url");

    for _ in 0..2 {
        let code = execute(
            Command::Migrate {
                database_url: database_url.clone(),
            },
            jiff::Timestamp::UNIX_EPOCH,
        )
        .await
        .expect("migrate");
        assert_eq!(code, ExitCode::SUCCESS);
    }

    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&pool)
        .await
        .expect("migrations table");
    assert_eq!(
        applied,
        i64::try_from(db::MIGRATOR.iter().count()).expect("count")
    );
    sqlx::query("SELECT count(*) FROM http_session")
        .execute(&pool)
        .await
        .expect("session table exists");
}

#[tokio::test]
async fn migrate_fails_when_the_database_is_unreachable() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let result = execute(
        Command::Migrate {
            database_url: format!("postgres://postgres@127.0.0.1:{port}/postgres"),
        },
        jiff::Timestamp::UNIX_EPOCH,
    )
    .await;
    assert!(matches!(result, Err(cli::Error::Db(_))), "{result:?}");
}
