#![forbid(unsafe_code)]

use auth::login::{self, ABSOLUTE_TIMEOUT, IDLE_TIMEOUT};
use auth::staff;
use auth::{NewStaff, Role, StaffUser};
use db::PgPool;
use jiff::{SignedDuration, Timestamp};

fn start() -> Timestamp {
    "2026-11-24T09:00:00Z".parse().expect("timestamp")
}

async fn baker(pool: &PgPool) -> StaffUser {
    staff::add(
        pool,
        &NewStaff {
            oidc_sub: "uuid-baker".to_string(),
            name: "Bakker Bas".to_string(),
            email: None,
            role: Role::Baker,
        },
    )
    .await
    .expect("add")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_login_names_its_staff_member_until_it_ends(pool: PgPool) {
    let baker = baker(&pool).await;
    let login = login::start(&pool, baker.id, start()).await.expect("start");
    assert!(login.as_str().len() >= 32);
    assert!(!format!("{login:?}").contains(login.as_str()));
    let other = login::start(&pool, baker.id, start()).await.expect("start");
    assert_ne!(login, other);

    let current = login::current(&pool, &login, start())
        .await
        .expect("current");
    assert_eq!(current, Some(baker));
    login::end(&pool, &login).await.expect("end");
    assert_eq!(
        login::current(&pool, &login, start())
            .await
            .expect("current"),
        None
    );
    assert!(
        login::current(&pool, &other, start())
            .await
            .expect("current")
            .is_some(),
        "the other browser stays signed in"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_idle_login_ends(pool: PgPool) {
    let baker = baker(&pool).await;
    let login = login::start(&pool, baker.id, start()).await.expect("start");
    let almost = start() + IDLE_TIMEOUT - SignedDuration::from_mins(1);
    assert!(
        login::current(&pool, &login, almost)
            .await
            .expect("current")
            .is_some()
    );
    let later = almost + IDLE_TIMEOUT;
    assert_eq!(
        login::current(&pool, &login, later).await.expect("current"),
        None
    );
    assert_eq!(
        login::current(&pool, &login, almost)
            .await
            .expect("current"),
        None,
        "an ended login stays ended"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_busy_login_still_ends_after_a_week(pool: PgPool) {
    let baker = baker(&pool).await;
    let login = login::start(&pool, baker.id, start()).await.expect("start");
    let mut now = start();
    while now + SignedDuration::from_hours(10) < start() + ABSOLUTE_TIMEOUT {
        now += SignedDuration::from_hours(10);
        assert!(
            login::current(&pool, &login, now)
                .await
                .expect("current")
                .is_some()
        );
    }
    let after = start() + ABSOLUTE_TIMEOUT;
    assert_eq!(
        login::current(&pool, &login, after).await.expect("current"),
        None
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn deactivating_ends_every_login(pool: PgPool) {
    let baker = baker(&pool).await;
    let login = login::start(&pool, baker.id, start()).await.expect("start");
    staff::deactivate(&pool, baker.id)
        .await
        .expect("deactivate");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM staff_login")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(left, 0);
    assert_eq!(
        login::current(&pool, &login, start())
            .await
            .expect("current"),
        None
    );
}
