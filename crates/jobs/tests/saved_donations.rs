//! Saved donations (F4d): the confirmation links to the choice, and the tick moves those
//! nobody chose for to the next drop's charity with an email.
#![forbid(unsafe_code)]

use std::sync::Arc;

use bakedays::{NewBakeDay, Schedule, days};
use domain::{BakeDayId, CharityId, OrderId, PickupLocationId, ProductId};
use i18n::Translator;
use jiff::{SignedDuration, Timestamp};
use jobs::OrderEmails;
use jobs::order_emails::{donation_moved_key, enqueue_confirmation};
use jobs::saved_donations::tick;
use orders::donations::{DonationChoice, SavedDonation, choose_at_checkout};
use orders::{Cart, CustomerInput, OrderedBy, PaidOutcome, PaymentReceived};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn emails() -> OrderEmails {
    OrderEmails::new(
        Arc::new(Translator::dutch().unwrap()),
        "https://shop.example.org",
    )
}

fn saturday(date: &str, charity: Option<CharityId>) -> NewBakeDay {
    NewBakeDay {
        date: date.parse().unwrap(),
        location_id: PickupLocationId::new(1),
        online_ordering: true,
        charity_id: charity,
    }
}

async fn charity(conn: &mut PgConnection, name: &str) -> CharityId {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO charity (name, url) VALUES ($1, 'https://example.org') RETURNING id",
    )
    .bind(name)
    .fetch_one(conn)
    .await
    .unwrap();
    CharityId::new(id)
}

async fn bread(conn: &mut PgConnection) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role) VALUES ('test-baker', 'Testbakker', 'baker') RETURNING id)
         INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online, allergens_approved_by, allergens_approved_at)
         VALUES ('Desem', 450, 9, 800, 900, true, (SELECT id FROM baker), '2026-10-01T10:00:00Z') RETURNING id",
    )
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

/// A drop open for orders at the clock's time.
async fn open_drop(conn: &mut PgConnection, clock: &FakeClock, input: &NewBakeDay) -> BakeDayId {
    let day = days::create(conn, input).await.unwrap();
    days::update_schedule(
        conn,
        day.id,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(ts("2026-10-01T00:00:00Z")),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    days::open(conn, clock, day.id).await.unwrap();
    day.id
}

/// A paid order on a charity drop whose donation the customer saved, with its
/// confirmation queued as the webhook does.
async fn saved_order(conn: &mut PgConnection, clock: &FakeClock) -> (OrderId, SavedDonation) {
    sqlx::query("UPDATE company SET address = 'Teststraat 1, 1234 AB Testdorp', kvk = '12345678'")
        .execute(&mut *conn)
        .await
        .unwrap();
    let voedselbank = charity(conn, "Voedselbank").await;
    let day = open_drop(conn, clock, &saturday("2026-10-31", Some(voedselbank))).await;
    let slot = days::list_slots(conn, day).await.unwrap()[0].id;
    let product = bread(conn).await;
    bakedays::range::set_product(
        conn,
        day,
        &bakedays::BakeDayProduct {
            product_id: product,
            max_qty: None,
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await
    .unwrap();
    let mut cart = Cart::new(day);
    cart.set(product, 2, None).unwrap();
    let order = orders::checkout(
        conn,
        clock,
        &cart,
        &CustomerInput {
            email: "klant@example.org".to_owned(),
            name: "Test Klant".to_owned(),
            phone: None,
            marketing_opt_in: false,
        },
        slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();
    choose_at_checkout(conn, order.id, DonationChoice::Save)
        .await
        .unwrap();
    let outcome = orders::mark_paid(
        conn,
        clock,
        order.id,
        &PaymentReceived {
            reference: "tr_1".to_owned(),
            amount: order.total,
        },
    )
    .await
    .unwrap();
    let PaidOutcome::Paid {
        saved: Some(saved), ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    enqueue_confirmation(conn, clock, &emails(), order.id, Some(&saved))
        .await
        .unwrap()
        .expect("queued");
    (order.id, saved)
}

#[derive(Debug, sqlx::FromRow)]
struct Queued {
    kind: String,
    dedupe_key: Option<String>,
    text_body: String,
    expires_us: Option<i64>,
}

async fn outbox(conn: &mut PgConnection) -> Vec<Queued> {
    sqlx::query_as(
        "SELECT kind, dedupe_key, text_body,
            (extract(epoch FROM expires_at) * 1000000)::bigint AS expires_us
        FROM email_outbox ORDER BY id",
    )
    .fetch_all(conn)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_confirmation_of_a_saved_donation_links_to_the_choice_and_expires(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let (_, saved) = saved_order(&mut conn, &clock).await;
    let queued = outbox(&mut conn).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, "order_confirmation");
    assert!(
        queued[0].text_body.contains(&format!(
            "https://shop.example.org/donatie/kies/{}",
            saved.token.as_str()
        )),
        "{}",
        queued[0].text_body
    );
    assert!(
        queued[0].text_body.contains("donderdag 28 januari 2027"),
        "{}",
        queued[0].text_body
    );
    assert_eq!(queued[0].expires_us, Some(saved.choose_by.as_microsecond()));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_tick_moves_an_unchosen_donation_once_and_mails_the_customer(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let (order, saved) = saved_order(&mut conn, &clock).await;
    let hospice = charity(&mut conn, "Hospice").await;
    open_drop(&mut conn, &clock, &saturday("2027-02-06", Some(hospice))).await;

    clock.set(saved.choose_by - SignedDuration::from_secs(1));
    let report = tick(&mut conn, &clock, &emails()).await.unwrap();
    assert!(report.moved.is_empty());

    clock.set(saved.choose_by);
    let report = tick(&mut conn, &clock, &emails()).await.unwrap();
    assert_eq!(report.moved, [saved.allocation]);
    assert!(!report.waiting);
    let queued = outbox(&mut conn).await;
    let moved = queued.last().unwrap();
    assert_eq!(moved.kind, "donation_moved");
    assert_eq!(
        moved.dedupe_key.as_deref(),
        Some(donation_moved_key(saved.allocation).as_str())
    );
    assert!(moved.text_body.contains("Hospice"), "{}", moved.text_body);
    assert!(
        moved.text_body.contains("zaterdag 6 februari 2027"),
        "{}",
        moved.text_body
    );
    let charity_now: Option<i64> =
        sqlx::query_scalar("SELECT charity_id FROM donation_allocation WHERE order_id = $1")
            .bind(order.get())
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(charity_now, Some(hospice.get()));

    let again = tick(&mut conn, &clock, &emails()).await.unwrap();
    assert!(again.moved.is_empty());
    assert_eq!(outbox(&mut conn).await.len(), queued.len());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_a_coming_charity_drop_the_tick_waits(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let (_, saved) = saved_order(&mut conn, &clock).await;
    clock.set(saved.choose_by + SignedDuration::from_hours(1));
    let report = tick(&mut conn, &clock, &emails()).await.unwrap();
    assert!(report.moved.is_empty());
    assert!(report.waiting);
    assert_eq!(outbox(&mut conn).await.len(), 1);
}
