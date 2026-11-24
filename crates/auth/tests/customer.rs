#![forbid(unsafe_code)]

use auth::customer::{
    self, ABSOLUTE_TIMEOUT, Customer, IDLE_TIMEOUT, LINK_VALID, LINKS_PER_WINDOW, LinkRequest,
    LoginLink,
};
use db::PgPool;
use domain::CustomerId;
use jiff::{SignedDuration, Timestamp};
use sha2::{Digest, Sha256};

fn start() -> Timestamp {
    "2026-11-24T09:00:00Z".parse().expect("timestamp")
}

async fn customer(pool: &PgPool, email: &str) -> CustomerId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO customer (email, name, created_at) VALUES ($1, 'Klant', now()) RETURNING id",
    )
    .bind(email)
    .fetch_one(pool)
    .await
    .expect("customer");
    CustomerId::new(id)
}

async fn request(pool: &PgPool, email: &str, now: Timestamp) -> LinkRequest {
    let mut conn = pool.acquire().await.expect("conn");
    customer::request_link(&mut conn, email, now)
        .await
        .expect("request")
}

async fn issue(pool: &PgPool, email: &str, now: Timestamp) -> LoginLink {
    match request(pool, email, now).await {
        LinkRequest::Issued(link) => link,
        other => panic!("expected a link, got {other:?}"),
    }
}

async fn redeem(pool: &PgPool, token: &str, now: Timestamp) -> Option<CustomerId> {
    let mut conn = pool.acquire().await.expect("conn");
    customer::redeem_link(&mut conn, token, now)
        .await
        .expect("redeem")
}

async fn valid(pool: &PgPool, token: &str, now: Timestamp) -> bool {
    let mut conn = pool.acquire().await.expect("conn");
    customer::link_is_valid(&mut conn, token, now)
        .await
        .expect("peek")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_link_is_only_issued_for_a_known_customer(pool: PgPool) {
    let id = customer(&pool, "klant@example.nl").await;
    let link = issue(&pool, "  Klant@Example.NL ", start()).await;
    assert_eq!(link.customer, id);
    assert_eq!(link.email, "klant@example.nl");
    assert!(!format!("{link:?}").contains(link.token.as_str()));

    assert!(matches!(
        request(&pool, "onbekend@example.nl", start()).await,
        LinkRequest::Unknown
    ));
    assert!(matches!(
        request(&pool, "geen adres", start()).await,
        LinkRequest::Unknown
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_the_hash_of_the_token_is_stored(pool: PgPool) {
    customer(&pool, "klant@example.nl").await;
    let link = issue(&pool, "klant@example.nl", start()).await;
    let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT token_hash FROM customer_login_link")
        .fetch_all(&pool)
        .await
        .expect("rows");
    assert_eq!(
        stored,
        vec![Sha256::digest(link.token.as_str().as_bytes()).to_vec()]
    );
    let found: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM customer_login_link
         WHERE position(convert_to($1, 'UTF8') IN token_hash) > 0",
    )
    .bind(link.token.as_str())
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(found, 0);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_link_works_once(pool: PgPool) {
    let id = customer(&pool, "klant@example.nl").await;
    let link = issue(&pool, "klant@example.nl", start()).await;
    assert!(valid(&pool, link.token.as_str(), start()).await);
    assert!(
        valid(&pool, link.token.as_str(), start()).await,
        "looking does not use it"
    );
    assert_eq!(redeem(&pool, link.token.as_str(), start()).await, Some(id));
    assert_eq!(redeem(&pool, link.token.as_str(), start()).await, None);
    assert!(!valid(&pool, link.token.as_str(), start()).await);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_link_expires(pool: PgPool) {
    let id = customer(&pool, "klant@example.nl").await;
    let first = issue(&pool, "klant@example.nl", start()).await;
    let second = issue(&pool, "klant@example.nl", start()).await;
    let almost = start() + LINK_VALID - SignedDuration::from_secs(1);
    assert_eq!(redeem(&pool, first.token.as_str(), almost).await, Some(id));
    let expired = start() + LINK_VALID;
    assert!(!valid(&pool, second.token.as_str(), expired).await);
    assert_eq!(redeem(&pool, second.token.as_str(), expired).await, None);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn junk_and_unknown_tokens_sign_nobody_in(pool: PgPool) {
    customer(&pool, "klant@example.nl").await;
    issue(&pool, "klant@example.nl", start()).await;
    for token in ["", "abc", &"a".repeat(43), &"a".repeat(500), "../../etc"] {
        assert_eq!(redeem(&pool, token, start()).await, None, "{token}");
        assert!(!valid(&pool, token, start()).await, "{token}");
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn links_per_customer_are_limited(pool: PgPool) {
    customer(&pool, "klant@example.nl").await;
    for _ in 0..LINKS_PER_WINDOW {
        issue(&pool, "klant@example.nl", start()).await;
    }
    assert!(matches!(
        request(&pool, "klant@example.nl", start()).await,
        LinkRequest::Throttled
    ));
    let later = start() + customer::LINK_WINDOW;
    issue(&pool, "klant@example.nl", later).await;
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_login_names_its_customer_until_it_ends(pool: PgPool) {
    let id = customer(&pool, "klant@example.nl").await;
    let login = customer::start(&pool, id, start()).await.expect("start");
    let other = customer::start(&pool, id, start()).await.expect("start");
    assert_ne!(login, other);
    assert_eq!(
        customer::current(&pool, &login, start())
            .await
            .expect("current"),
        Some(Customer {
            id,
            email: "klant@example.nl".to_owned(),
            name: "Klant".to_owned(),
        })
    );
    customer::end(&pool, &login).await.expect("end");
    assert_eq!(
        customer::current(&pool, &login, start())
            .await
            .expect("current"),
        None
    );
    assert!(
        customer::current(&pool, &other, start())
            .await
            .expect("current")
            .is_some()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_login_ends_when_idle_or_old(pool: PgPool) {
    let id = customer(&pool, "klant@example.nl").await;
    let idle = customer::start(&pool, id, start()).await.expect("start");
    let later = start() + IDLE_TIMEOUT;
    assert_eq!(
        customer::current(&pool, &idle, later)
            .await
            .expect("current"),
        None
    );

    let busy = customer::start(&pool, id, start()).await.expect("start");
    let mut now = start();
    while now < start() + ABSOLUTE_TIMEOUT - IDLE_TIMEOUT {
        now += IDLE_TIMEOUT - SignedDuration::from_mins(1);
        assert!(
            customer::current(&pool, &busy, now)
                .await
                .expect("current")
                .is_some()
        );
    }
    assert_eq!(
        customer::current(&pool, &busy, start() + ABSOLUTE_TIMEOUT)
            .await
            .expect("current"),
        None
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn concurrent_requests_stay_within_the_limit(pool: PgPool) {
    customer(&pool, "klant@example.nl").await;
    let tries = usize::try_from(LINKS_PER_WINDOW).expect("small") + 3;
    let requests = (0..tries).map(|_| {
        let pool = pool.clone();
        tokio::spawn(async move { request(&pool, "klant@example.nl", start()).await })
    });
    let mut issued = 0;
    for request in requests.collect::<Vec<_>>() {
        if matches!(request.await.expect("task"), LinkRequest::Issued(_)) {
            issued += 1;
        }
    }
    assert_eq!(issued, LINKS_PER_WINDOW);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn two_clicks_at_once_sign_in_once(pool: PgPool) {
    customer(&pool, "klant@example.nl").await;
    let link = issue(&pool, "klant@example.nl", start()).await;
    let clicks = (0..2).map(|_| {
        let pool = pool.clone();
        let token = link.token.as_str().to_owned();
        tokio::spawn(async move { redeem(&pool, &token, start()).await })
    });
    let mut signed_in = 0;
    for click in clicks.collect::<Vec<_>>() {
        if click.await.expect("task").is_some() {
            signed_in += 1;
        }
    }
    assert_eq!(signed_in, 1);
}

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(pool)
        .await
        .expect("count")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_cleanup_deletes_expired_links_and_logins_only(pool: PgPool) {
    let id = customer(&pool, "klant@example.nl").await;
    let old = issue(&pool, "klant@example.nl", start()).await;
    let mut conn = pool.acquire().await.expect("conn");
    customer::redeem_link(&mut conn, old.token.as_str(), start())
        .await
        .expect("redeem");
    customer::start(&pool, id, start()).await.expect("login");
    let other = customer(&pool, "ander@example.nl").await;
    let later = start() + SignedDuration::from_hours(24 * 8);
    let fresh = issue(&pool, "ander@example.nl", later).await;
    sqlx::query(
        "INSERT INTO customer_login (customer_id, created_at, last_seen_at) VALUES ($1, $2::timestamptz, $2::timestamptz)",
    )
    .bind(other.get())
    .bind(later.to_string())
    .execute(&pool)
    .await
    .expect("a login that start's own cleanup does not touch");

    let cleaned =
        customer::delete_expired(&mut conn, later + LINK_VALID - SignedDuration::from_secs(1))
            .await
            .expect("cleanup");
    assert_eq!(
        (cleaned.links, cleaned.logins),
        (1, 1),
        "the first customer's"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM customer_login_link").await,
        1
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM customer_login").await, 1);
    let valid = customer::link_is_valid(&mut conn, fresh.token.as_str(), later)
        .await
        .expect("valid");
    assert!(valid);

    let cleaned = customer::delete_expired(&mut conn, later + ABSOLUTE_TIMEOUT)
        .await
        .expect("cleanup");
    assert_eq!((cleaned.links, cleaned.logins), (1, 1));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_erased_customer_cannot_use_a_link_or_a_login(pool: PgPool) {
    let id = customer(&pool, "klant@example.nl").await;
    let link = issue(&pool, "klant@example.nl", start()).await;
    let login = customer::start(&pool, id, start()).await.expect("login");
    sqlx::query("UPDATE customer SET erased_at = now() WHERE id = $1")
        .bind(id.get())
        .execute(&pool)
        .await
        .expect("erase");

    let mut conn = pool.acquire().await.expect("conn");
    let redeemed = customer::redeem_link(&mut conn, link.token.as_str(), start())
        .await
        .expect("redeem");
    assert_eq!(redeemed, None);
    let current = customer::current(&pool, &login, start())
        .await
        .expect("current");
    assert_eq!(current, None);
    assert_eq!(count(&pool, "SELECT count(*) FROM customer_login").await, 0);
}
