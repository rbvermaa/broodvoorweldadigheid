use domain::{CustomerId, Money, StaffUserId, VatRate};
use orders::audit;
use orders::{
    Channel, CustomerInput, Error, OrderStatus, PaymentMode, allocations, checkout_on_account,
};
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;

use crate::common::{
    BREAD, DONATION_LOAF, cart, charity, on_range, open_day, order_as_guest, product, saturday,
    ts, wednesday,
};

const BAKER: StaffUserId = StaffUserId::new(3);

fn company() -> CustomerInput {
    CustomerInput {
        email: "inkoop@dehoek.example".to_owned(),
        name: "Café De Hoek".to_owned(),
        phone: None,
        marketing_opt_in: false,
    }
}

async fn business(conn: &mut PgConnection, email: &str, on_account_allowed: bool) -> CustomerId {
    let id: i64 = sqlx::query_scalar(
        "WITH c AS (INSERT INTO customer (email, name, created_at)
            VALUES ($1, 'Inkoop', '2026-10-01T10:00:00Z') RETURNING id)
        INSERT INTO business_account (customer_id, company_name, kvk, invoice_address,
            invoice_email, on_account_allowed)
        SELECT id, 'Café De Hoek', '12345678', 'Dorpsstraat 1', 'factuur@dehoek.example', $2
        FROM c RETURNING customer_id",
    )
    .bind(email)
    .bind(on_account_allowed)
    .fetch_one(conn)
    .await
    .unwrap();
    CustomerId::new(id)
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn staff_order_on_account_is_confirmed_at_the_suggested_price(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let mut input = saturday();
    input.charity_id = Some(charity(&mut conn, "Voedselbank").await);
    let shop = open_day(&mut conn, &clock, &input).await;
    let bread = product(&mut conn, &BREAD).await;
    let loaf = product(&mut conn, &DONATION_LOAF).await;
    for id in [bread, loaf] {
        on_range(&mut conn, &shop, id, Some(5), None).await;
    }
    let customer = business(&mut conn, "inkoop@dehoek.example", true).await;

    clock.set(ts("2026-10-30T09:00:00Z"));
    let order = checkout_on_account(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 3, None), (loaf, 1, Some(1500))]),
        &company(),
        shop.slot,
        BAKER,
    )
    .await
    .unwrap();

    assert_eq!(order.customer_id, customer);
    assert_eq!(order.status, OrderStatus::Confirmed);
    assert_eq!(order.payment_mode, PaymentMode::OnAccount);
    assert_eq!(order.channel, Channel::Manual);
    assert_eq!((order.hold_expires_at, order.paid_at), (None, None));
    assert_eq!(order.customer_name, "Café De Hoek");
    let lines: Vec<_> = order
        .lines
        .iter()
        .map(|l| (l.product_id, l.qty, l.unit_price, l.vat_rate))
        .collect();
    assert_eq!(
        lines,
        [
            (bread, 3, Money::from_cents(450), VatRate::Nine),
            (loaf, 1, Money::from_cents(600), VatRate::Nine),
        ]
    );
    assert_eq!(order.total, Money::from_cents(1950));
    assert!(allocations(&mut conn, order.id).await.unwrap().is_empty());
    let history = audit::history(&mut conn, "order", order.id.get())
        .await
        .unwrap();
    assert_eq!(
        (history[0].action.as_str(), history[0].actor.as_str()),
        ("created", "staff:3")
    );

    clock.set(ts("2026-10-28T12:00:00Z"));
    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(bread, 3, None)]).await,
        Err(Error::CapacityExceeded { remaining: 2, .. })
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_business_accounts_allowed_on_account_can_order_on_account(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(5), None).await;
    business(&mut conn, "dicht@dehoek.example", false).await;

    for email in ["dicht@dehoek.example", "nieuw@example.org"] {
        let customer = CustomerInput {
            email: email.to_owned(),
            ..company()
        };
        assert!(matches!(
            checkout_on_account(
                &mut conn,
                &clock,
                &cart(&shop, &[(bread, 1, None)]),
                &customer,
                shop.slot,
                BAKER,
            )
            .await,
            Err(Error::NotOnAccount)
        ));
    }
    let customers: i64 = sqlx::query_scalar("SELECT count(*) FROM customer")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(customers, 1);
    let orders: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "order""#)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(orders, 0);
}
