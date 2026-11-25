#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use catalog::company::KitchenMayContain;
use catalog::{Approval, Error, SourdoughDeadline, company};
use domain::{Allergen, Clock, StaffUserId};
use jiff::civil::time;
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

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

const BAKER: StaffUserId = StaffUserId::new(7);

async fn add_baker(conn: &mut PgConnection) {
    sqlx::query(
        "INSERT INTO staff_user (id, oidc_sub, name, role) OVERRIDING SYSTEM VALUE
         VALUES ($1, 'baker-fixture', 'Bakker', 'baker') ON CONFLICT (id) DO NOTHING",
    )
    .bind(BAKER.get())
    .execute(conn)
    .await
    .unwrap();
}

fn traces() -> BTreeSet<Allergen> {
    BTreeSet::from([Allergen::SesameSeeds, Allergen::Nuts])
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn kitchen_may_contain_starts_empty_and_unapproved(mut conn: PoolConnection<Postgres>) {
    assert_eq!(
        company::kitchen_may_contain(&mut conn).await.unwrap(),
        KitchenMayContain::default()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn approving_the_kitchen_list_records_who_and_when(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new("2026-03-29T01:30:00.250Z".parse().unwrap());
    add_baker(&mut conn).await;
    let saved = company::set_kitchen_may_contain(&mut conn, &traces())
        .await
        .unwrap();
    assert_eq!(saved.allergens, traces());
    assert_eq!(saved.approval, None);

    let approved = company::approve_kitchen_may_contain(&mut conn, &traces(), BAKER, &clock)
        .await
        .unwrap();
    assert_eq!(
        approved.approval,
        Some(Approval {
            by: BAKER,
            at: clock.now(),
        })
    );
    assert_eq!(
        company::kitchen_may_contain(&mut conn).await.unwrap(),
        approved
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_empty_kitchen_list_can_be_approved(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new("2026-11-25T10:00:00Z".parse().unwrap());
    add_baker(&mut conn).await;
    let approved = company::approve_kitchen_may_contain(&mut conn, &BTreeSet::new(), BAKER, &clock)
        .await
        .unwrap();
    assert!(approved.approval.is_some());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn approving_a_stale_kitchen_list_is_refused(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new("2026-11-25T10:00:00Z".parse().unwrap());
    add_baker(&mut conn).await;
    company::set_kitchen_may_contain(&mut conn, &traces())
        .await
        .unwrap();
    let mut reviewed = traces();
    reviewed.remove(&Allergen::Nuts);
    assert!(matches!(
        company::approve_kitchen_may_contain(&mut conn, &reviewed, BAKER, &clock).await,
        Err(Error::AllergensChanged)
    ));
    assert_eq!(
        company::kitchen_may_contain(&mut conn)
            .await
            .unwrap()
            .approval,
        None
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn changing_the_kitchen_list_clears_its_approval(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new("2026-11-25T10:00:00Z".parse().unwrap());
    add_baker(&mut conn).await;
    company::approve_kitchen_may_contain(&mut conn, &BTreeSet::new(), BAKER, &clock)
        .await
        .unwrap();
    let changed = company::set_kitchen_may_contain(&mut conn, &traces())
        .await
        .unwrap();
    assert_eq!(changed.approval, None);

    company::approve_kitchen_may_contain(&mut conn, &traces(), BAKER, &clock)
        .await
        .unwrap();
    let same = company::set_kitchen_may_contain(&mut conn, &traces())
        .await
        .unwrap();
    assert!(
        same.approval.is_some(),
        "saving the same list keeps the approval"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn any_writer_changing_the_kitchen_list_clears_its_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new("2026-11-25T10:00:00Z".parse().unwrap());
    add_baker(&mut conn).await;
    company::approve_kitchen_may_contain(&mut conn, &BTreeSet::new(), BAKER, &clock)
        .await
        .unwrap();
    sqlx::query("UPDATE company SET may_contain = '{milk}'")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        company::kitchen_may_contain(&mut conn)
            .await
            .unwrap()
            .approval,
        None
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_same_kitchen_set_in_another_order_keeps_its_approval(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new("2026-11-25T10:00:00Z".parse().unwrap());
    add_baker(&mut conn).await;
    company::set_kitchen_may_contain(&mut conn, &traces())
        .await
        .unwrap();
    company::approve_kitchen_may_contain(&mut conn, &traces(), BAKER, &clock)
        .await
        .unwrap();
    sqlx::query("UPDATE company SET may_contain = '{sesame_seeds,nuts}'")
        .execute(&mut *conn)
        .await
        .unwrap();
    let kept = company::kitchen_may_contain(&mut conn).await.unwrap();
    assert_eq!(kept.allergens, traces());
    assert!(kept.approval.is_some(), "a reordered list is no change");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn other_company_changes_keep_the_kitchen_approval(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new("2026-11-25T10:00:00Z".parse().unwrap());
    add_baker(&mut conn).await;
    company::approve_kitchen_may_contain(&mut conn, &BTreeSet::new(), BAKER, &clock)
        .await
        .unwrap();
    sqlx::query("UPDATE company SET name = 'Andere naam'")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(
        company::kitchen_may_contain(&mut conn)
            .await
            .unwrap()
            .approval
            .is_some()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn database_refuses_a_bad_kitchen_list(mut conn: PoolConnection<Postgres>) {
    let constraint = |err: sqlx::Error| {
        err.as_database_error()
            .and_then(|e| e.constraint())
            .map(str::to_owned)
    };
    let err = sqlx::query("UPDATE company SET may_contain = '{gluten}'")
        .execute(&mut *conn)
        .await
        .unwrap_err();
    assert_eq!(
        constraint(err).as_deref(),
        Some("company_may_contain_check")
    );
    let err = sqlx::query("UPDATE company SET may_contain = '{milk,NULL}'")
        .execute(&mut *conn)
        .await
        .unwrap_err();
    assert_eq!(
        constraint(err).as_deref(),
        Some("company_may_contain_check")
    );
    let err = sqlx::query("UPDATE company SET may_contain_approved_at = now()")
        .execute(&mut *conn)
        .await
        .unwrap_err();
    assert_eq!(
        constraint(err).as_deref(),
        Some("company_may_contain_approval_complete")
    );
    let err = sqlx::query(
        "UPDATE company SET may_contain_approved_by = 999, may_contain_approved_at = now()",
    )
    .execute(&mut *conn)
    .await
    .unwrap_err();
    assert_eq!(
        constraint(err).as_deref(),
        Some("company_may_contain_approved_by_fkey")
    );
}
