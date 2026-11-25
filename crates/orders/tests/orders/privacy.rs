use domain::{Clock, CustomerId};
use orders::audit::{self, Actor};
use orders::donations::{self, DonationChoice};
use orders::privacy::{self, ERASED_NAME, Erase};
use orders::{OrderedBy, PaidOutcome, PaymentReceived, checkout, mark_paid};
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};
use testkit::FakeClock;

use crate::common::{
    BREAD, backend_pid, cart, charity, guest, on_range, open_day, product, saturday, ts,
    wait_until_blocked, wednesday,
};

async fn pay(conn: &mut sqlx::PgConnection, clock: &testkit::FakeClock, order: &orders::Order) {
    let outcome = mark_paid(
        conn,
        clock,
        order.id,
        &PaymentReceived {
            reference: format!("tr_{}", order.id.get()),
            amount: order.total,
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, PaidOutcome::Paid { .. }), "{outcome:?}");
}

async fn picked_up(conn: &mut sqlx::PgConnection, order: &orders::Order) {
    sqlx::query("UPDATE \"order\" SET status = 'picked_up' WHERE id = $1")
        .bind(order.id.get())
        .execute(conn)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_export_holds_profile_orders_lines_payments_donations_and_logins(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let mut input = saturday();
    input.charity_id = Some(food_bank);
    let shop = open_day(&mut conn, &clock, &input).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let mut me = guest("sanne@example.org");
    me.phone = Some("06 1234 5678".to_owned());
    let order = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 2, None)]),
        &me,
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    pay(&mut conn, &clock, &order).await;
    sqlx::query(
        "INSERT INTO customer_login (customer_id, created_at, last_seen_at)
        VALUES ($1, '2026-10-28T12:00:00Z', '2026-10-28T12:05:00Z')",
    )
    .bind(order.customer_id.get())
    .execute(&mut *conn)
    .await
    .unwrap();

    let data = privacy::export(&mut conn, order.customer_id).await.unwrap();
    assert_eq!(data.customer.email, "sanne@example.org");
    assert_eq!(data.customer.name, "Test Klant");
    assert_eq!(data.customer.phone.as_deref(), Some("06 1234 5678"));
    assert_eq!(data.orders.len(), 1);
    let exported = &data.orders[0];
    assert_eq!(exported.number, order.number.to_string());
    assert_eq!(exported.bake_day, "2026-10-31");
    assert_eq!(exported.status, "paid");
    assert_eq!(exported.total_cents, 900);
    assert_eq!(exported.lines.len(), 1);
    assert_eq!(exported.lines[0].product, "Desem");
    assert_eq!(exported.lines[0].qty, 2);
    assert_eq!(exported.payments.len(), 1);
    assert_eq!(exported.payments[0].amount_cents, 900);
    assert_eq!(exported.donations.len(), 1);
    assert_eq!(
        exported.donations[0].charity.as_deref(),
        Some("Voedselbank")
    );
    assert_eq!(exported.donations[0].amount_cents, 900);
    assert_eq!(data.logins.len(), 1);
    assert_eq!(data.logins[0].started_at, "2026-10-28T12:00:00Z");
    assert!(data.drop_alerts.is_none());
    assert!(data.votes.is_empty());
    assert!(data.business.is_none());
    let json = data.to_json().unwrap();
    assert!(json.contains("\"email\": \"sanne@example.org\""), "{json}");

    assert!(matches!(
        privacy::export(&mut conn, CustomerId::new(9999)).await,
        Err(orders::Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn erasing_keeps_the_orders_and_donations_but_nothing_that_identifies(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let mut input = saturday();
    input.charity_id = Some(food_bank);
    let shop = open_day(&mut conn, &clock, &input).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let mut me = guest("sanne@example.org");
    me.phone = Some("06 1234 5678".to_owned());
    me.marketing_opt_in = true;
    let kept = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &me,
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    pay(&mut conn, &clock, &kept).await;
    let saved = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 2, None)]),
        &me,
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    donations::choose_at_checkout(&mut conn, saved.id, DonationChoice::Save)
        .await
        .unwrap();
    pay(&mut conn, &clock, &saved).await;
    picked_up(&mut conn, &kept).await;
    picked_up(&mut conn, &saved).await;
    let customer = kept.customer_id;
    let token_before: String =
        sqlx::query_scalar("SELECT status_token::text FROM \"order\" WHERE id = $1")
            .bind(kept.id.get())
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO customer_login (customer_id, created_at, last_seen_at)
        VALUES ($1, now(), now())",
    )
    .bind(customer.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO customer_login_link (token_hash, customer_id, created_at, expires_at)
        VALUES (sha256('x'), $1, now(), now() + interval '30 minutes')",
    )
    .bind(customer.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    let amounts_before: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
        "SELECT amount_cents, status, charity_id FROM donation_allocation ORDER BY id",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();

    let erased = privacy::erase(&mut conn, customer, Actor::Customer(customer), clock.now())
        .await
        .unwrap();
    assert_eq!(
        erased,
        Erase::Erased {
            email: "sanne@example.org".to_owned()
        }
    );

    let (email, name, phone, consent): (String, String, Option<String>, bool) =
        sqlx::query_as("SELECT email, name, phone, marketing_consent FROM customer WHERE id = $1")
            .bind(customer.get())
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert!(!email.contains("sanne"), "{email}");
    assert_eq!(name, ERASED_NAME);
    assert_eq!((phone, consent), (None, false));
    let orders: Vec<(String, Option<String>, bool, i64, String)> = sqlx::query_as(
        "SELECT customer_name, customer_phone, marketing_opt_in, total_cents, status_token::text
        FROM \"order\" WHERE customer_id = $1 ORDER BY id",
    )
    .bind(customer.get())
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(orders.len(), 2);
    for (name, phone, opt_in, _, _) in &orders {
        assert_eq!((name.as_str(), phone, opt_in), (ERASED_NAME, &None, &false));
    }
    assert_eq!(orders[0].3, 450);
    assert_eq!(orders[1].3, 900);
    assert_ne!(orders[0].4, token_before);
    let amounts_after: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
        "SELECT amount_cents, status, charity_id FROM donation_allocation ORDER BY id",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(amounts_after, amounts_before);
    for (table, sql) in [
        (
            "customer_login",
            "SELECT count(*) FROM customer_login WHERE customer_id = $1",
        ),
        (
            "customer_login_link",
            "SELECT count(*) FROM customer_login_link WHERE customer_id = $1",
        ),
    ] {
        let left: i64 = sqlx::query_scalar(sql)
            .bind(customer.get())
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(left, 0, "{table}");
    }
    let history = audit::history(&mut conn, "customer", customer.get())
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].action, "erased");
    assert_eq!(history[0].before, None);
    assert_eq!(history[0].after, None);

    assert!(matches!(
        privacy::erase(&mut conn, customer, Actor::Customer(customer), clock.now()).await,
        Err(orders::Error::NotFound)
    ));
    assert!(matches!(
        privacy::export(&mut conn, customer).await,
        Err(orders::Error::NotFound)
    ));
    assert_eq!(
        privacy::find_by_email(&mut conn, "sanne@example.org")
            .await
            .unwrap(),
        None
    );
    let checkout_again = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("sanne@example.org"),
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    assert_ne!(checkout_again.customer_id, customer);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_order_still_to_pay_or_pick_up_blocks_erasing(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let order = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("sanne@example.org"),
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    let customer = order.customer_id;
    assert_eq!(
        erase_at(&mut conn, customer, "2026-10-28T12:00:00Z").await,
        Erase::OpenOrders
    );
    pay(&mut conn, &clock, &order).await;
    assert_eq!(
        erase_at(&mut conn, customer, "2026-10-31T10:59:00Z").await,
        Erase::OpenOrders,
        "the pickup window ends at 12:00 in Amsterdam"
    );
    assert!(matches!(
        erase_at(&mut conn, customer, "2026-10-31T11:00:00Z").await,
        Erase::Erased { .. }
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_business_customer_is_not_erased_here(mut conn: PoolConnection<Postgres>) {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO customer (email, name, created_at) VALUES ('zaak@example.org', 'Zaak', now())
        RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO business_account (customer_id, company_name, kvk, invoice_address,
            invoice_email)
        VALUES ($1, 'Zaak BV', '12345678', 'Straat 1', 'factuur@example.org')",
    )
    .bind(id)
    .execute(&mut *conn)
    .await
    .unwrap();
    let customer = CustomerId::new(id);
    assert_eq!(
        privacy::erase(
            &mut conn,
            customer,
            Actor::Customer(customer),
            ts("2026-10-28T12:00:00Z")
        )
        .await
        .unwrap(),
        Erase::Business
    );
    let data = privacy::export(&mut conn, customer).await.unwrap();
    assert_eq!(
        data.business.map(|b| (b.company_name, b.invoice_email)),
        Some(("Zaak BV".to_owned(), "factuur@example.org".to_owned()))
    );
    let name: String = sqlx::query_scalar("SELECT name FROM customer WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(name, "Zaak");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn consent_is_stored_and_the_address_returned(mut conn: PoolConnection<Postgres>) {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO customer (email, name, created_at) VALUES ('sanne@example.org', 'Sanne', now())
        RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let customer = CustomerId::new(id);
    assert_eq!(
        privacy::set_marketing_consent(&mut conn, customer, true)
            .await
            .unwrap(),
        "sanne@example.org"
    );
    let stored: bool = sqlx::query_scalar("SELECT marketing_consent FROM customer WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert!(stored);
    let found = privacy::find_by_email(&mut conn, " Sanne@Example.org ")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((found.id, found.orders), (customer, 0));
    assert!(matches!(
        privacy::set_marketing_consent(&mut conn, CustomerId::new(9999), true).await,
        Err(orders::Error::NotFound)
    ));
}

async fn erase_at(conn: &mut sqlx::PgConnection, customer: CustomerId, at: &str) -> Erase {
    privacy::erase(conn, customer, Actor::Customer(customer), ts(at))
        .await
        .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_erased_customers_saved_donation_still_moves_but_without_an_email(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let food_bank = charity(&mut conn, "Voedselbank").await;
    let mut input = saturday();
    input.charity_id = Some(food_bank);
    let shop = open_day(&mut conn, &clock, &input).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let order = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("sanne@example.org"),
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    donations::choose_at_checkout(&mut conn, order.id, DonationChoice::Save)
        .await
        .unwrap();
    pay(&mut conn, &clock, &order).await;
    picked_up(&mut conn, &order).await;
    let customer = order.customer_id;
    assert!(matches!(
        erase_at(&mut conn, customer, "2026-11-01T12:00:00Z").await,
        Erase::Erased { .. }
    ));

    let later = ts("2027-01-30T12:00:00Z");
    let next = FakeClock::new(later);
    let mut day = saturday();
    day.date = "2027-02-06".parse().unwrap();
    day.charity_id = Some(food_bank);
    open_day(&mut conn, &next, &day).await;
    match donations::move_next_expired(&mut conn, later)
        .await
        .unwrap()
    {
        donations::Expired::Moved(moved) => {
            assert_eq!(moved.charity, food_bank);
            assert_eq!(moved.amount.cents(), 450);
            assert!(!moved.notify);
        }
        other => panic!("not moved: {other:?}"),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_erase_waits_for_a_checkout_of_the_same_customer_and_sees_its_order(pool: PgPool) {
    let clock = wednesday();
    let mut conn = pool.acquire().await.unwrap();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None).await;
    let first = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("sanne@example.org"),
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE \"order\" SET status = 'cancelled', cancel_reason = 'hold_expired', hold_expires_at = NULL")
        .execute(&mut *conn)
        .await
        .unwrap();
    let customer = first.customer_id;

    let mut ordering = pool.begin().await.unwrap();
    checkout(
        &mut ordering,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("sanne@example.org"),
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    let mut erasing = pool.acquire().await.unwrap();
    let pid = backend_pid(&mut erasing).await;
    let task = tokio::spawn(async move {
        privacy::erase(
            &mut erasing,
            customer,
            Actor::Customer(customer),
            ts("2026-10-28T12:00:00Z"),
        )
        .await
    });
    wait_until_blocked(&pool, pid).await;
    ordering.commit().await.unwrap();
    assert_eq!(task.await.unwrap().unwrap(), Erase::OpenOrders);
}
