#![forbid(unsafe_code)]

use std::process::ExitCode;

use auth::Role;
use cli::{Command, Error, StaffCommand, backup, execute};
use sqlx::PgPool;

fn database_url(pool: &PgPool) -> String {
    let admin = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let database = pool
        .connect_options()
        .get_database()
        .expect("test database name")
        .to_owned();
    backup::with_database(&admin, &database).expect("url")
}

fn add(pool: &PgPool, sub: &str, role: Role) -> Command {
    Command::Staff(StaffCommand::Add {
        database_url: database_url(pool),
        sub: sub.to_string(),
        name: "Bakker Bas".to_string(),
        email: Some("bas@example.org".to_string()),
        role,
    })
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn staff_add_stores_the_person_with_the_role(pool: PgPool) {
    let code = execute(
        add(&pool, "uuid-bas", Role::Baker),
        jiff::Timestamp::UNIX_EPOCH,
    )
    .await
    .expect("add");
    assert_eq!(code, ExitCode::SUCCESS);
    let staff = auth::staff::find_by_sub(&pool, "uuid-bas")
        .await
        .expect("find")
        .expect("added");
    assert_eq!(staff.role, Role::Baker);
    assert_eq!(staff.email.as_deref(), Some("bas@example.org"));

    let error = execute(
        add(&pool, "uuid-bas", Role::Owner),
        jiff::Timestamp::UNIX_EPOCH,
    )
    .await
    .expect_err("duplicate");
    assert!(
        matches!(error, Error::Staff(auth::staff::Error::DuplicateSub)),
        "{error}"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_empty_email_from_make_staff_add_means_none(pool: PgPool) {
    for (sub, email) in [
        ("0d2f1f9e-1c2b-4e3a-9f00-000000000001", ""),
        ("0d2f1f9e-1c2b-4e3a-9f00-000000000002", "  "),
    ] {
        let Command::Staff(StaffCommand::Add {
            database_url,
            name,
            role,
            ..
        }) = add(&pool, sub, Role::Counter)
        else {
            unreachable!("add builds a staff add command");
        };
        let command = Command::Staff(StaffCommand::Add {
            database_url,
            sub: sub.to_string(),
            name,
            email: Some(email.to_string()),
            role,
        });
        let code = execute(command, jiff::Timestamp::UNIX_EPOCH)
            .await
            .expect("add");
        assert_eq!(code, ExitCode::SUCCESS);
        let staff = auth::staff::find_by_sub(&pool, sub)
            .await
            .expect("find")
            .expect("added");
        assert_eq!(staff.email, None, "{email:?}");
    }
}
