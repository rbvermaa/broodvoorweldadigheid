use bakedays::{NewBakeDay, days};
use domain::{CharityId, Clock, CustomerId};
use jiff::SignedDuration;
use orders::donations::{
    Chooser, DonationChoice, Expired, SavedDonation, choose, choose_at_checkout, move_next_expired,
    saved_by_link, saved_of_customer,
};
use orders::{Error, Order, OrderStatus, PaidOutcome, PaymentReceived, allocations, mark_paid};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{
    BREAD, OpenDay, charity, on_range, open_day, order_as_guest, product, saturday, ts, wednesday,
};

struct Drop {
    shop: OpenDay,
    bread: domain::ProductId,
    charity: CharityId,
}

async fn charity_drop(conn: &mut PgConnection, clock: &FakeClock) -> Drop {
    let voedselbank = charity(conn, "Voedselbank").await;
    let shop = open_day(
        conn,
        clock,
        &NewBakeDay {
            charity_id: Some(voedselbank),
            ..saturday()
        },
    )
    .await;
    let bread = product(conn, &BREAD).await;
    on_range(conn, &shop, bread, None, None).await;
    Drop {
        shop,
        bread,
        charity: voedselbank,
    }
}

async fn place(conn: &mut PgConnection, clock: &FakeClock, drop: &Drop) -> Order {
    order_as_guest(conn, clock, &drop.shop, &[(drop.bread, 2, None)])
        .await
        .unwrap()
}

async fn pay(conn: &mut PgConnection, clock: &FakeClock, order: &Order) -> PaidOutcome {
    mark_paid(
        conn,
        clock,
        order.id,
        &PaymentReceived {
            reference: format!("tr_{}", order.id.get()),
            amount: order.total,
        },
    )
    .await
    .unwrap()
}

/// A paid order on the drop whose donation is saved, and its choose link.
async fn saved(conn: &mut PgConnection, clock: &FakeClock, drop: &Drop) -> (Order, SavedDonation) {
    let order = place(conn, clock, drop).await;
    choose_at_checkout(conn, order.id, DonationChoice::Save)
        .await
        .unwrap();
    match pay(conn, clock, &order).await {
        PaidOutcome::Paid {
            allocation: None,
            saved: Some(saved),
            ..
        } => (order, saved),
        other => panic!("{other:?}"),
    }
}

async fn customer_of(conn: &mut PgConnection, order: &Order) -> CustomerId {
    let id: i64 = sqlx::query_scalar(r#"SELECT customer_id FROM "order" WHERE id = $1"#)
        .bind(order.id.get())
        .fetch_one(conn)
        .await
        .unwrap();
    CustomerId::new(id)
}

async fn allocation_row(conn: &mut PgConnection, order: &Order) -> (String, Option<i64>) {
    sqlx::query_as("SELECT status, charity_id FROM donation_allocation WHERE order_id = $1")
        .bind(order.id.get())
        .fetch_one(conn)
        .await
        .unwrap()
}

async fn audit_actions(conn: &mut PgConnection, allocation: i64) -> Vec<String> {
    orders::audit::history(conn, "donation_allocation", allocation)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.action)
        .collect()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn another_charity_chosen_at_checkout_gets_the_donation(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let hospice = charity(&mut conn, "Hospice").await;
    let order = place(&mut conn, &clock, &drop).await;
    choose_at_checkout(&mut conn, order.id, DonationChoice::Charity(hospice))
        .await
        .unwrap();
    let PaidOutcome::Paid {
        allocation, saved, ..
    } = pay(&mut conn, &clock, &order).await
    else {
        panic!("not paid");
    };
    assert_eq!(saved, None);
    let allocation = allocation.expect("an allocation");
    assert_eq!(allocation.charity_id, hospice);
    assert_eq!(allocation.amount, order.total);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn choosing_the_drops_own_charity_is_the_default(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let order = place(&mut conn, &clock, &drop).await;
    choose_at_checkout(&mut conn, order.id, DonationChoice::Save)
        .await
        .unwrap();
    choose_at_checkout(&mut conn, order.id, DonationChoice::Charity(drop.charity))
        .await
        .unwrap();
    pay(&mut conn, &clock, &order).await;
    let found = allocations(&mut conn, order.id).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].charity_id, drop.charity);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn checkout_choices_are_checked(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let archived = charity(&mut conn, "Oud").await;
    sqlx::query("UPDATE charity SET archived_at = now() WHERE id = $1")
        .bind(archived.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let order = place(&mut conn, &clock, &drop).await;
    let refused = choose_at_checkout(&mut conn, order.id, DonationChoice::Charity(archived)).await;
    assert!(
        matches!(refused, Err(Error::Invalid("charity"))),
        "{refused:?}"
    );
    let unknown = choose_at_checkout(
        &mut conn,
        order.id,
        DonationChoice::Charity(CharityId::new(999_999)),
    )
    .await;
    assert!(
        matches!(unknown, Err(Error::Invalid("charity"))),
        "{unknown:?}"
    );

    pay(&mut conn, &clock, &order).await;
    let late = choose_at_checkout(&mut conn, order.id, DonationChoice::Save).await;
    assert!(
        matches!(late, Err(Error::WrongStatus(OrderStatus::Paid))),
        "{late:?}"
    );

    let normal = open_day(
        &mut conn,
        &clock,
        &NewBakeDay {
            date: "2026-11-07".parse().unwrap(),
            ..saturday()
        },
    )
    .await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &normal, bread, None, None).await;
    let sale = order_as_guest(&mut conn, &clock, &normal, &[(bread, 1, None)])
        .await
        .unwrap();
    let no_charity = choose_at_checkout(&mut conn, sale.id, DonationChoice::Save).await;
    assert!(
        matches!(no_charity, Err(Error::Invalid("donation"))),
        "{no_charity:?}"
    );
    choose_at_checkout(&mut conn, sale.id, DonationChoice::DropCharity)
        .await
        .unwrap();
    let PaidOutcome::Paid {
        allocation, saved, ..
    } = pay(&mut conn, &clock, &sale).await
    else {
        panic!("not paid");
    };
    assert_eq!(
        (allocation, saved),
        (None, None),
        "a normal sale has no donation"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_saved_donation_stores_only_the_hash_of_its_link(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let (order, saved) = saved(&mut conn, &clock, &drop).await;
    assert_eq!(
        saved.choose_by,
        ts("2027-01-27T23:00:00Z"),
        "3 months, local midnight"
    );
    let (plain, hash, choose_by): (Option<String>, Vec<u8>, i64) = sqlx::query_as(
        "SELECT choose_token, choose_token_hash,
            (extract(epoch FROM choose_by) * 1000000)::bigint
        FROM donation_allocation WHERE id = $1",
    )
    .bind(saved.allocation.get())
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(plain, None);
    assert_eq!(
        hash,
        Sha256::digest(saved.token.as_str().as_bytes()).to_vec()
    );
    assert_eq!(choose_by, saved.choose_by.as_microsecond());
    assert_eq!(
        allocation_row(&mut conn, &order).await,
        ("saved".to_owned(), None)
    );
    assert_eq!(
        audit_actions(&mut conn, saved.allocation.get()).await,
        ["saved"]
    );
    assert!(allocations(&mut conn, order.id).await.unwrap().is_empty());

    let found = saved_by_link(&mut conn, saved.token.as_str(), clock.now())
        .await
        .unwrap()
        .expect("the link works");
    assert_eq!(found.order, order.id);
    assert_eq!(found.amount, order.total);
    assert_eq!(found.drop_date, "2026-10-31".parse().unwrap());
    assert_eq!(
        saved_by_link(&mut conn, &"A".repeat(43), clock.now())
            .await
            .unwrap(),
        None
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_link_chooses_once_and_then_stops_working(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let hospice = charity(&mut conn, "Hospice").await;
    let (order, saved) = saved(&mut conn, &clock, &drop).await;
    let link = Chooser::Link(saved.token.as_str());

    let chosen = choose(&mut conn, link, hospice, clock.now()).await.unwrap();
    assert_eq!(chosen, Some(saved.allocation));
    assert_eq!(
        allocation_row(&mut conn, &order).await,
        ("earmarked".to_owned(), Some(hospice.get()))
    );
    assert_eq!(
        audit_actions(&mut conn, saved.allocation.get()).await,
        ["saved", "chosen"]
    );
    let cleared: bool = sqlx::query_scalar(
        "SELECT choose_token_hash IS NULL AND choose_by IS NULL FROM donation_allocation
        WHERE id = $1",
    )
    .bind(saved.allocation.get())
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert!(cleared, "the link is gone");

    let again = choose(&mut conn, link, drop.charity, clock.now())
        .await
        .unwrap();
    assert_eq!(again, None);
    assert_eq!(
        saved_by_link(&mut conn, saved.token.as_str(), clock.now())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        allocations(&mut conn, order.id).await.unwrap()[0].charity_id,
        hospice
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_archived_charity_cannot_be_chosen(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let archived = charity(&mut conn, "Oud").await;
    sqlx::query("UPDATE charity SET archived_at = now() WHERE id = $1")
        .bind(archived.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let (order, saved) = saved(&mut conn, &clock, &drop).await;
    let refused = choose(
        &mut conn,
        Chooser::Link(saved.token.as_str()),
        archived,
        clock.now(),
    )
    .await;
    assert!(
        matches!(refused, Err(Error::Invalid("charity"))),
        "{refused:?}"
    );
    assert_eq!(
        allocation_row(&mut conn, &order).await,
        ("saved".to_owned(), None)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn after_its_date_the_link_no_longer_chooses(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let (order, saved) = saved(&mut conn, &clock, &drop).await;
    clock.set(saved.choose_by);
    assert_eq!(
        saved_by_link(&mut conn, saved.token.as_str(), clock.now())
            .await
            .unwrap(),
        None
    );
    let late = choose(
        &mut conn,
        Chooser::Link(saved.token.as_str()),
        drop.charity,
        clock.now(),
    )
    .await
    .unwrap();
    assert_eq!(late, None);
    assert_eq!(
        allocation_row(&mut conn, &order).await,
        ("saved".to_owned(), None)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_signed_in_customer_chooses_only_their_own_donations(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let hospice = charity(&mut conn, "Hospice").await;
    let (order, saved) = saved(&mut conn, &clock, &drop).await;
    let owner = customer_of(&mut conn, &order).await;
    let stranger: i64 = sqlx::query_scalar(
        "INSERT INTO customer (email, name, created_at) VALUES ('ander@example.org', 'Ander', now())
         RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let stranger = CustomerId::new(stranger);

    assert!(
        saved_of_customer(&mut conn, stranger, clock.now())
            .await
            .unwrap()
            .is_empty()
    );
    let listed = saved_of_customer(&mut conn, owner, clock.now())
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].allocation, saved.allocation);

    let theirs = choose(
        &mut conn,
        Chooser::Customer(stranger, saved.allocation),
        hospice,
        clock.now(),
    )
    .await
    .unwrap();
    assert_eq!(theirs, None);
    let mine = choose(
        &mut conn,
        Chooser::Customer(owner, saved.allocation),
        hospice,
        clock.now(),
    )
    .await
    .unwrap();
    assert_eq!(mine, Some(saved.allocation));
    assert!(
        saved_of_customer(&mut conn, owner, clock.now())
            .await
            .unwrap()
            .is_empty()
    );
}

/// A published drop, open for orders.
async fn drop_on(conn: &mut PgConnection, date: &str, charity: Option<CharityId>) {
    open_day(
        conn,
        &wednesday(),
        &NewBakeDay {
            date: date.parse().unwrap(),
            charity_id: charity,
            ..saturday()
        },
    )
    .await;
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_unchosen_donation_moves_to_the_next_drops_charity(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let (order, saved) = saved(&mut conn, &clock, &drop).await;
    let hospice = charity(&mut conn, "Hospice").await;
    let dierenasiel = charity(&mut conn, "Dierenasiel").await;
    drop_on(&mut conn, "2027-01-23", Some(dierenasiel)).await;
    drop_on(&mut conn, "2027-01-30", None).await;
    days::create(
        &mut conn,
        &NewBakeDay {
            date: "2027-02-06".parse().unwrap(),
            charity_id: Some(dierenasiel),
            ..saturday()
        },
    )
    .await
    .unwrap();
    drop_on(&mut conn, "2027-02-13", Some(hospice)).await;
    drop_on(&mut conn, "2027-02-20", Some(dierenasiel)).await;

    clock.set(saved.choose_by - SignedDuration::from_secs(1));
    assert_eq!(
        move_next_expired(&mut conn, clock.now()).await.unwrap(),
        Expired::Nothing
    );

    clock.set(saved.choose_by + SignedDuration::from_hours(25));
    let Expired::Moved(moved) = move_next_expired(&mut conn, clock.now()).await.unwrap() else {
        panic!("not moved");
    };
    assert_eq!(moved.allocation, saved.allocation);
    assert_eq!(moved.order, order.id);
    assert_eq!(
        moved.charity, hospice,
        "the next drop with a charity from today"
    );
    assert_eq!(moved.charity_name, "Hospice");
    assert_eq!(moved.drop_date, "2027-02-13".parse().unwrap());
    assert_eq!(moved.amount, order.total);
    assert!(moved.notify);
    assert_eq!(
        allocation_row(&mut conn, &order).await,
        ("earmarked".to_owned(), Some(hospice.get()))
    );
    assert_eq!(
        audit_actions(&mut conn, saved.allocation.get()).await,
        ["saved", "moved"]
    );
    assert_eq!(
        move_next_expired(&mut conn, clock.now()).await.unwrap(),
        Expired::Nothing
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn without_a_coming_charity_drop_the_donation_waits(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let (order, saved) = saved(&mut conn, &clock, &drop).await;
    let archived = charity(&mut conn, "Oud").await;
    drop_on(&mut conn, "2027-02-06", Some(archived)).await;
    sqlx::query("UPDATE charity SET archived_at = now() WHERE id = $1")
        .bind(archived.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    clock.set(saved.choose_by);
    assert_eq!(
        move_next_expired(&mut conn, clock.now()).await.unwrap(),
        Expired::NoDrop
    );
    assert_eq!(
        allocation_row(&mut conn, &order).await,
        ("saved".to_owned(), None)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_cancelled_orders_saved_donation_moves_without_a_mail(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let drop = charity_drop(&mut conn, &clock).await;
    let (order, saved) = saved(&mut conn, &clock, &drop).await;
    let staff: i64 = sqlx::query_scalar("SELECT id FROM staff_user LIMIT 1")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    orders::cancel_paid(&mut conn, &clock, order.id, domain::StaffUserId::new(staff))
        .await
        .unwrap();
    assert_eq!(
        saved_by_link(&mut conn, saved.token.as_str(), clock.now())
            .await
            .unwrap(),
        None
    );
    drop_on(&mut conn, "2027-02-06", Some(drop.charity)).await;
    clock.set(saved.choose_by);
    let Expired::Moved(moved) = move_next_expired(&mut conn, clock.now()).await.unwrap() else {
        panic!("not moved");
    };
    assert!(!moved.notify);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_malformed_link_chooses_nothing(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let hospice = charity(&mut conn, "Hospice").await;
    for token in ["", "te-kort", &"!".repeat(43)] {
        assert_eq!(
            choose(&mut conn, Chooser::Link(token), hospice, clock.now())
                .await
                .unwrap(),
            None,
            "{token:?}"
        );
    }
}
