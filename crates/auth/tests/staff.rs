#![forbid(unsafe_code)]

use auth::Identity;
use auth::staff::{self, Error};
use auth::{BootstrapOwner, NewStaff, Role};
use db::PgPool;

fn identity(sub: &str, name: &str) -> Identity {
    Identity {
        sub: sub.to_string(),
        name: Some(name.to_string()),
        preferred_username: None,
        email: None,
        email_verified: false,
    }
}

fn new_staff(sub: &str, role: Role) -> NewStaff {
    NewStaff {
        oidc_sub: sub.to_string(),
        name: "Toonbank Tineke".to_string(),
        email: Some("tineke@example.org".to_string()),
        role,
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn added_staff_can_be_found_by_id_and_subject(pool: PgPool) {
    let added = staff::add(&pool, &new_staff("sub-1", Role::Counter))
        .await
        .expect("add");
    assert_eq!(added.role, Role::Counter);
    assert!(added.active);
    assert_eq!(
        staff::find(&pool, added.id).await.expect("find"),
        Some(added.clone())
    );
    assert_eq!(
        staff::find_by_sub(&pool, "sub-1").await.expect("find"),
        Some(added)
    );
    assert_eq!(
        staff::find_by_sub(&pool, "nobody").await.expect("find"),
        None
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_subject_is_unique(pool: PgPool) {
    staff::add(&pool, &new_staff("sub-1", Role::Baker))
        .await
        .expect("add");
    let error = staff::add(&pool, &new_staff("sub-1", Role::Owner))
        .await
        .expect_err("duplicate");
    assert!(matches!(error, Error::DuplicateSub), "{error}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_database_rejects_unknown_roles_and_empty_subjects(pool: PgPool) {
    let bad_role =
        sqlx::query("INSERT INTO staff_user (oidc_sub, name, role) VALUES ('x', 'X', 'admin')")
            .execute(&pool)
            .await;
    assert!(bad_role.is_err());
    let empty_sub =
        sqlx::query("INSERT INTO staff_user (oidc_sub, name, role) VALUES ('', 'X', 'owner')")
            .execute(&pool)
            .await;
    assert!(empty_sub.is_err());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn known_staff_sign_in_and_get_their_name_refreshed(pool: PgPool) {
    let added = staff::add(&pool, &new_staff("sub-1", Role::Baker))
        .await
        .expect("add");
    let signed_in = staff::sign_in(
        &pool,
        &identity("sub-1", "Tineke de Bakker"),
        &BootstrapOwner::default(),
    )
    .await
    .expect("sign in");
    assert_eq!(signed_in.id, added.id);
    assert_eq!(signed_in.name, "Tineke de Bakker");
    assert_eq!(
        signed_in.email.as_deref(),
        Some("tineke@example.org"),
        "no email claim keeps the stored one"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn inactive_and_unknown_people_are_refused(pool: PgPool) {
    let added = staff::add(&pool, &new_staff("sub-1", Role::Baker))
        .await
        .expect("add");
    sqlx::query("UPDATE staff_user SET active = false WHERE id = $1")
        .bind(added.id.get())
        .execute(&pool)
        .await
        .expect("deactivate");
    let error = staff::sign_in(
        &pool,
        &identity("sub-1", "Tineke"),
        &BootstrapOwner::default(),
    )
    .await
    .expect_err("inactive");
    assert!(matches!(error, Error::Inactive(_)), "{error}");

    let error = staff::sign_in(
        &pool,
        &identity("stranger", "Onbekend"),
        &BootstrapOwner::default(),
    )
    .await
    .expect_err("unknown");
    assert!(matches!(error, Error::Unknown), "{error}");
    assert_eq!(
        staff::find_by_sub(&pool, "stranger").await.expect("find"),
        None
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_bootstrap_owner_gets_in_once_while_there_is_no_owner(pool: PgPool) {
    let bootstrap = BootstrapOwner {
        sub: Some("first".to_string()),
        email: Some("eigenaar@example.org".to_string()),
    };
    let owner = staff::sign_in(&pool, &identity("first", "Eigenaar"), &bootstrap)
        .await
        .expect("bootstrap");
    assert_eq!(owner.role, Role::Owner);
    assert_eq!(owner.name, "Eigenaar");

    let mut by_email = identity("second", "Tweede");
    by_email.email = Some("eigenaar@example.org".to_string());
    by_email.email_verified = true;
    let error = staff::sign_in(&pool, &by_email, &bootstrap)
        .await
        .expect_err("an owner exists now");
    assert!(matches!(error, Error::Unknown), "{error}");

    let again = staff::sign_in(&pool, &identity("first", "Eigenaar"), &bootstrap)
        .await
        .expect("the owner signs in as a known user");
    assert_eq!(again.id, owner.id);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn bootstrap_by_verified_email(pool: PgPool) {
    let bootstrap = BootstrapOwner {
        sub: None,
        email: Some("eigenaar@example.org".to_string()),
    };
    let mut person = identity("uuid-9", "Eigenaar");
    person.email = Some("eigenaar@example.org".to_string());
    let error = staff::sign_in(&pool, &person, &bootstrap)
        .await
        .expect_err("unverified email");
    assert!(matches!(error, Error::Unknown), "{error}");
    person.email_verified = true;
    let owner = staff::sign_in(&pool, &person, &bootstrap)
        .await
        .expect("verified email");
    assert_eq!(owner.role, Role::Owner);
    assert_eq!(owner.email.as_deref(), Some("eigenaar@example.org"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn concurrent_bootstrap_logins_make_one_owner(pool: PgPool) {
    let bootstrap = BootstrapOwner {
        sub: Some("by-sub".to_string()),
        email: Some("eigenaar@example.org".to_string()),
    };
    let by_sub = identity("by-sub", "Een");
    let mut by_email = identity("by-email", "Twee");
    by_email.email = Some("eigenaar@example.org".to_string());
    by_email.email_verified = true;
    let (first, second) = tokio::join!(
        staff::sign_in(&pool, &by_sub, &bootstrap),
        staff::sign_in(&pool, &by_email, &bootstrap),
    );
    assert_eq!(
        [first.is_ok(), second.is_ok()]
            .iter()
            .filter(|ok| **ok)
            .count(),
        1,
        "{first:?} {second:?}"
    );
    let owners: i64 = sqlx::query_scalar("SELECT count(*) FROM staff_user WHERE role = 'owner'")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(owners, 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_last_active_owner_cannot_be_demoted_deactivated_or_deleted(pool: PgPool) {
    let owner = staff::add(&pool, &new_staff("owner-1", Role::Owner))
        .await
        .expect("add");
    let error = staff::set_role(&pool, owner.id, Role::Baker)
        .await
        .expect_err("demote");
    assert!(matches!(error, Error::LastOwner), "{error}");
    let error = staff::deactivate(&pool, owner.id)
        .await
        .expect_err("deactivate");
    assert!(matches!(error, Error::LastOwner), "{error}");
    let deleted = sqlx::query("DELETE FROM staff_user WHERE id = $1")
        .bind(owner.id.get())
        .execute(&pool)
        .await;
    assert!(deleted.is_err(), "delete");

    let second = staff::add(&pool, &new_staff("owner-2", Role::Owner))
        .await
        .expect("add");
    staff::set_role(&pool, owner.id, Role::Baker)
        .await
        .expect("another owner is left");
    let error = staff::deactivate(&pool, second.id)
        .await
        .expect_err("now the last one");
    assert!(matches!(error, Error::LastOwner), "{error}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_product_approval_must_name_a_staff_user(pool: PgPool) {
    let constraint: Option<String> = sqlx::query_scalar(
        "SELECT conname::text FROM pg_constraint
         WHERE conname = 'product_allergens_approved_by_fkey'
           AND confrelid = 'staff_user'::regclass",
    )
    .fetch_optional(&pool)
    .await
    .expect("constraint");
    assert!(constraint.is_some());
}
