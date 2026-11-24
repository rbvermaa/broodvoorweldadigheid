use bakedays::{NewBakeDay, days, range};
use domain::{Money, PickupSlotId, PriceRefused, StaffUserId, VatRate};
use orders::audit::{self, Actor};
use orders::{
    Cart, CartError, Channel, Error, HOLD, OrderNumber, OrderStatus, OrderedBy, PaymentMode,
    checkout,
};
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};
use testkit::FakeClock;

use crate::common::{
    BREAD, COOKIES, DONATION_LOAF, ProductSpec, backend_pid, cart, guest, on_range, open_day,
    order_as_guest, product, saturday, ts, wait_until_blocked, wednesday,
};

fn eur(cents: i64) -> Money {
    Money::from_cents(cents)
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn checkout_copies_prices_and_holds_capacity(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    let loaf = product(&mut conn, &DONATION_LOAF).await;
    let cookies = product(&mut conn, &COOKIES).await;
    for id in [bread, loaf, cookies] {
        on_range(&mut conn, &shop, id, Some(10), None).await;
    }

    let order = order_as_guest(
        &mut conn,
        &clock,
        &shop,
        &[(bread, 2, None), (loaf, 1, Some(1000)), (cookies, 1, None)],
    )
    .await
    .unwrap();

    assert_eq!(order.number, OrderNumber { year: 2026, seq: 1 });
    assert_eq!(order.number.to_string(), "2026-0001");
    assert_eq!(order.status, OrderStatus::PendingPayment);
    assert_eq!(order.channel, Channel::Web);
    assert_eq!(order.payment_mode, PaymentMode::Online);
    assert_eq!(order.bake_day_id, shop.day.id);
    assert_eq!(order.pickup_slot_id, shop.slot);
    assert_eq!(order.created_at, domain::Clock::now(&clock));
    assert_eq!(order.hold_expires_at, Some(ts("2026-10-28T12:15:00Z")));
    assert_eq!(order.paid_at, None);
    assert_eq!(
        (
            order.customer_name.as_str(),
            order.customer_phone.as_deref()
        ),
        ("Test Klant", None)
    );
    assert!(!order.marketing_opt_in);
    assert_eq!(order.total, eur(2295));
    let lines: Vec<_> = order
        .lines
        .iter()
        .map(|l| {
            (
                l.product_id,
                l.qty,
                l.unit_price,
                l.suggested_price,
                l.vat_rate,
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            (bread, 2, eur(450), None, VatRate::Nine),
            (loaf, 1, eur(1000), Some(eur(600)), VatRate::Nine),
            (cookies, 1, eur(395), None, VatRate::TwentyOne),
        ]
    );
    let totals = order.totals().unwrap();
    assert_eq!(totals.gross(), Ok(order.total));
    assert_eq!(totals.rate(VatRate::Nine).gross(), eur(1900));
    assert_eq!(totals.rate(VatRate::TwentyOne).vat(), eur(69));

    let history = audit::history(&mut conn, "order", order.id.get())
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].action, "created");
    assert_eq!(history[0].actor, Actor::Customer(order.customer_id).to_db());
    assert_eq!(
        history[0].after.as_ref().unwrap()["status"],
        "pending_payment"
    );

    sqlx::query("UPDATE product SET price_incl_cents = 999, vat_rate = 21 WHERE id = $1")
        .bind(bread.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        orders::order::get(&mut conn, order.id).await.unwrap(),
        order
    );

    let left = range::availability(&mut conn, shop.day.id, &orders::Orders::new(&clock))
        .await
        .unwrap();
    let left: Vec<_> = left.iter().map(|a| a.remaining).collect();
    use bakedays::capacity::Remaining::Pieces;
    assert_eq!(left, [Pieces(8), Pieces(9), Pieces(9)]);

    let second = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    assert_eq!(second.number.to_string(), "2026-0002");
    assert_eq!(second.customer_id, order.customer_id);
    assert_eq!(second.lines[0].unit_price, eur(999));
    assert_eq!(second.lines[0].vat_rate, VatRate::TwentyOne);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_guest_checkout_never_changes_an_existing_customer(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let mut first = guest("Klant@Example.org");
    first.marketing_opt_in = true;
    first.phone = Some("0612345678".to_owned());
    let a = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &first,
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    let mut again = guest(" klant@example.org ");
    again.name = "Iemand Anders".to_owned();
    again.phone = Some("0698765432".to_owned());
    let b = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &again,
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    assert_eq!(a.customer_id, b.customer_id);
    assert_eq!(
        (
            a.customer_name.as_str(),
            a.customer_phone.as_deref(),
            a.marketing_opt_in
        ),
        ("Test Klant", Some("0612345678"), true)
    );
    assert_eq!(
        (
            b.customer_name.as_str(),
            b.customer_phone.as_deref(),
            b.marketing_opt_in
        ),
        ("Iemand Anders", Some("0698765432"), false)
    );
    let row: (String, String, Option<String>, bool) =
        sqlx::query_as("SELECT email, name, phone, marketing_consent FROM customer WHERE id = $1")
            .bind(a.customer_id.get())
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(
        row,
        (
            "klant@example.org".to_owned(),
            "Test Klant".to_owned(),
            Some("0612345678".to_owned()),
            false
        )
    );
    let bad = guest("geen-email");
    assert!(matches!(
        checkout(
            &mut conn,
            &clock,
            &cart(&shop, &[(bread, 1, None)]),
            &bad,
            shop.slot,
            OrderedBy::Guest
        )
        .await,
        Err(Error::Invalid("email"))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn web_orders_stop_at_the_deadline_but_staff_can_still_order(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(3), None).await;
    // Thursday 20:00 CET, four days after summer time ended.
    assert_eq!(shop.day.cutoff_at, Some(ts("2026-10-29T19:00:00Z")));

    clock.set(ts("2026-10-29T18:59:59Z"));
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();

    clock.set(ts("2026-10-29T19:00:00Z"));
    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)]).await,
        Err(Error::PastDeadline)
    ));

    let staff = OrderedBy::Staff(StaffUserId::new(4));
    let manual = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 2, None)]),
        &guest("telefoon@example.org"),
        shop.slot,
        staff,
    )
    .await
    .unwrap();
    assert_eq!(manual.channel, Channel::Manual);
    let history = audit::history(&mut conn, "order", manual.id.get())
        .await
        .unwrap();
    assert_eq!(history[0].actor, "staff:4");

    days::close(&mut conn, &clock, shop.day.id).await.unwrap();
    assert!(matches!(
        checkout(
            &mut conn,
            &clock,
            &cart(&shop, &[(bread, 1, None)]),
            &guest("laat@example.org"),
            shop.slot,
            staff
        )
        .await,
        Err(Error::CapacityExceeded { remaining: 0, .. })
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_deadline_follows_local_time_in_summer(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-20T12:00:00Z"));
    let summer = NewBakeDay {
        date: "2026-10-24".parse().unwrap(),
        ..saturday()
    };
    let shop = open_day(&mut conn, &clock, &summer).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    // Thursday 20:00 CEST is 18:00 UTC.
    assert_eq!(shop.day.cutoff_at, Some(ts("2026-10-22T18:00:00Z")));
    clock.set(ts("2026-10-22T17:59:59Z"));
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    clock.set(ts("2026-10-22T18:00:00Z"));
    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)]).await,
        Err(Error::PastDeadline)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn refusals(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    let loaf = product(&mut conn, &DONATION_LOAF).await;
    let frozen = product(
        &mut conn,
        &ProductSpec {
            name: "Diepvriesbrood",
            sold_online: false,
            ..BREAD
        },
    )
    .await;
    let off_range = product(&mut conn, &COOKIES).await;
    for id in [bread, loaf, frozen] {
        on_range(&mut conn, &shop, id, None, None).await;
    }
    let refused = async |conn: &mut sqlx::PgConnection, lines: &[_]| {
        order_as_guest(conn, &clock, &shop, lines)
            .await
            .unwrap_err()
    };

    assert!(matches!(
        refused(&mut conn, &[]).await,
        Error::Cart(CartError::Empty)
    ));
    assert!(matches!(
        refused(&mut conn, &[(frozen, 1, None)]).await,
        Error::Cart(CartError::Unavailable(id)) if id == frozen
    ));
    assert!(matches!(
        refused(&mut conn, &[(off_range, 1, None)]).await,
        Error::Cart(CartError::Unavailable(id)) if id == off_range
    ));
    assert!(matches!(
        refused(&mut conn, &[(domain::ProductId::new(999), 1, None)]).await,
        Error::Cart(CartError::Unavailable(_))
    ));
    assert!(matches!(
        refused(&mut conn, &[(loaf, 1, Some(1501))]).await,
        Error::Cart(CartError::Price {
            refused: PriceRefused::AboveMaximum { .. },
            ..
        })
    ));
    assert!(matches!(
        refused(&mut conn, &[(bread, 1, Some(100))]).await,
        Error::Cart(CartError::PriceNotChoosable(_))
    ));
    sqlx::query("UPDATE product SET ingredient_text = 'tarwe' WHERE id = $1")
        .bind(bread.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(matches!(
        refused(&mut conn, &[(bread, 1, None)]).await,
        Error::Cart(CartError::AllergensNotApproved(_))
    ));

    let other = open_day(
        &mut conn,
        &clock,
        &NewBakeDay {
            date: "2026-11-07".parse().unwrap(),
            ..saturday()
        },
    )
    .await;
    let wrong_slot = checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(loaf, 1, None)]),
        &guest("a@example.org"),
        other.slot,
        OrderedBy::Guest,
    )
    .await;
    assert!(
        matches!(wrong_slot, Err(Error::SlotNotOnDay)),
        "{wrong_slot:?}"
    );
    let no_day = checkout(
        &mut conn,
        &clock,
        &Cart::new(domain::BakeDayId::new(999)).with(loaf),
        &guest("a@example.org"),
        PickupSlotId::new(1),
        OrderedBy::Guest,
    )
    .await;
    assert!(matches!(no_day, Err(Error::NotFound)), "{no_day:?}");

    let draft = days::create(
        &mut conn,
        &NewBakeDay {
            date: "2026-11-14".parse().unwrap(),
            ..saturday()
        },
    )
    .await
    .unwrap();
    let draft_slot = days::list_slots(&mut conn, draft.id).await.unwrap()[0].id;
    let not_open = checkout(
        &mut conn,
        &clock,
        &Cart::new(draft.id).with(loaf),
        &guest("a@example.org"),
        draft_slot,
        OrderedBy::Guest,
    )
    .await;
    assert!(matches!(not_open, Err(Error::NotOpen)), "{not_open:?}");

    let production = days::create(
        &mut conn,
        &NewBakeDay {
            date: "2026-11-15".parse().unwrap(),
            online_ordering: false,
            ..saturday()
        },
    )
    .await
    .unwrap();
    let staff = OrderedBy::Staff(StaffUserId::new(1));
    let production_only = checkout(
        &mut conn,
        &clock,
        &Cart::new(production.id).with(loaf),
        &guest("a@example.org"),
        draft_slot,
        staff,
    )
    .await;
    assert!(
        matches!(production_only, Err(Error::ProductionOnly)),
        "{production_only:?}"
    );

    days::cancel(&mut conn, &clock, other.day.id, "Ziek")
        .await
        .unwrap();
    let cancelled = checkout(
        &mut conn,
        &clock,
        &Cart::new(other.day.id).with(loaf),
        &guest("a@example.org"),
        other.slot,
        staff,
    )
    .await;
    assert!(matches!(cancelled, Err(Error::DayFinal)), "{cancelled:?}");

    let count: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "order""#)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

trait With {
    fn with(self, product: domain::ProductId) -> Self;
}

impl With for Cart {
    fn with(mut self, product: domain::ProductId) -> Self {
        self.set(product, 1, None).unwrap();
        self
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn sold_out_products_and_shared_pools_are_refused(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    let loaf = product(&mut conn, &DONATION_LOAF).await;
    let oven = range::create_pool(&mut conn, shop.day.id, "Oven", 5)
        .await
        .unwrap();
    on_range(&mut conn, &shop, bread, Some(4), Some(oven.id)).await;
    on_range(&mut conn, &shop, loaf, None, Some(oven.id)).await;

    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(bread, 5, None)]).await,
        Err(Error::CapacityExceeded { remaining: 4, product_id }) if product_id == bread
    ));
    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(bread, 3, None), (loaf, 3, None)]).await,
        Err(Error::CapacityExceeded { remaining: 2, product_id }) if product_id == loaf
    ));
    order_as_guest(
        &mut conn,
        &clock,
        &shop,
        &[(bread, 3, None), (loaf, 2, None)],
    )
    .await
    .unwrap();
    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(loaf, 1, None)]).await,
        Err(Error::CapacityExceeded { remaining: 0, .. })
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_full_pickup_slot_takes_no_more_orders(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    sqlx::query("UPDATE pickup_slot SET max_orders = 1 WHERE id = $1")
        .bind(shop.slot.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)]).await,
        Err(Error::SlotFull)
    ));
    clock.advance(HOLD);
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_second_checkout_for_the_last_loaf_waits_and_then_loses(pool: PgPool) {
    let clock = wednesday();
    let mut conn = pool.acquire().await.unwrap();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(1), None).await;
    let day = shop.day.id;
    let slot = shop.slot;
    drop(conn);

    let mut first = pool.begin().await.unwrap();
    let won = checkout(
        &mut first,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("eerst@example.org"),
        slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();

    let mut second = pool.acquire().await.unwrap();
    let pid = backend_pid(&mut second).await;
    let task = tokio::spawn(async move {
        let clock = wednesday();
        let mut cart = Cart::new(day);
        cart.set(bread, 1, None).unwrap();
        checkout(
            &mut second,
            &clock,
            &cart,
            &guest("tweede@example.org"),
            slot,
            OrderedBy::Guest,
        )
        .await
    });
    wait_until_blocked(&pool, pid).await;
    first.commit().await.unwrap();
    let lost = task.await.unwrap();
    assert!(
        matches!(lost, Err(Error::CapacityExceeded { remaining: 0, .. })),
        "{lost:?}"
    );
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        orders::order::get(&mut conn, won.id).await.unwrap().status,
        OrderStatus::PendingPayment
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn concurrent_checkouts_never_both_get_the_last_loaf(pool: PgPool) {
    let clock = wednesday();
    let mut conn = pool.acquire().await.unwrap();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(1), None).await;
    drop(conn);
    for round in 0..5 {
        let take = |email: &'static str| {
            let pool = pool.clone();
            let cart = cart(&shop, &[(bread, 1, None)]);
            let clock = &clock;
            let slot = shop.slot;
            async move {
                let mut conn = pool.acquire().await.unwrap();
                checkout(
                    &mut conn,
                    clock,
                    &cart,
                    &guest(email),
                    slot,
                    OrderedBy::Guest,
                )
                .await
            }
        };
        let (a, b) = tokio::join!(take("a@example.org"), take("b@example.org"));
        let (won, lost) = if a.is_ok() { (a, b) } else { (b, a) };
        assert!(
            won.is_ok(),
            "round {round}: nobody got the last loaf: {won:?}"
        );
        assert!(
            matches!(lost, Err(Error::CapacityExceeded { remaining: 0, .. })),
            "{lost:?}"
        );
        clock.advance(HOLD);
    }
    let rows: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "order""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 5);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_full_slot_is_checked_under_its_lock_across_products(pool: PgPool) {
    let clock = wednesday();
    let mut conn = pool.acquire().await.unwrap();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    let cookies = product(&mut conn, &COOKIES).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    on_range(&mut conn, &shop, cookies, None, None).await;
    sqlx::query("UPDATE pickup_slot SET max_orders = 1 WHERE id = $1")
        .bind(shop.slot.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);

    let mut first = pool.begin().await.unwrap();
    checkout(
        &mut first,
        &clock,
        &cart(&shop, &[(bread, 1, None)]),
        &guest("eerst@example.org"),
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();

    let mut second = pool.acquire().await.unwrap();
    let pid = backend_pid(&mut second).await;
    let other = cart(&shop, &[(cookies, 1, None)]);
    let slot = shop.slot;
    let task = tokio::spawn(async move {
        checkout(
            &mut second,
            &wednesday(),
            &other,
            &guest("tweede@example.org"),
            slot,
            OrderedBy::Guest,
        )
        .await
    });
    wait_until_blocked(&pool, pid).await;
    first.commit().await.unwrap();
    let lost = task.await.unwrap();
    assert!(matches!(lost, Err(Error::SlotFull)), "{lost:?}");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn order_numbers_start_again_at_new_year_in_local_time(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-12-28T12:00:00Z"));
    let shop = open_day(
        &mut conn,
        &clock,
        &NewBakeDay {
            date: "2027-01-02".parse().unwrap(),
            ..saturday()
        },
    )
    .await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let staff = OrderedBy::Staff(StaffUserId::new(1));
    let mut number_at = async |now: &str| {
        clock.set(ts(now));
        checkout(
            &mut conn,
            &clock,
            &cart(&shop, &[(bread, 1, None)]),
            &guest("oud@example.org"),
            shop.slot,
            staff,
        )
        .await
        .unwrap()
        .number
        .to_string()
    };
    assert_eq!(number_at("2026-12-31T22:59:00Z").await, "2026-0001");
    assert_eq!(number_at("2026-12-31T23:30:00Z").await, "2027-0001");
    assert_eq!(number_at("2027-01-01T08:00:00Z").await, "2027-0002");
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_cart_that_costs_nothing_is_refused_without_a_hold(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let sample = product(
        &mut conn,
        &ProductSpec {
            name: "Proefstukje",
            price: 0,
            ..BREAD
        },
    )
    .await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, sample, Some(1), None).await;
    on_range(&mut conn, &shop, bread, None, None).await;

    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(sample, 1, None)]).await,
        Err(Error::FreeOrder)
    ));
    let orders: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "order""#)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(orders, 0);

    let order = order_as_guest(
        &mut conn,
        &clock,
        &shop,
        &[(sample, 1, None), (bread, 1, None)],
    )
    .await
    .unwrap();
    assert_eq!(
        order.total,
        eur(450),
        "the sample still had its one place left"
    );
}
