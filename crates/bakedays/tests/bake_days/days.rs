use bakedays::schedule::LeadTimeWarning;
use bakedays::status::TransitionRefused;
use bakedays::{
    BakeDayProduct, BakeDayStatus, Error, ExtraInput, NewBakeDay, NoOrders, PickupSlotInput,
    PickupWindowInput, PublicStatus, Purpose, Schedule, Transition, days, extras, pickup, range,
};
use domain::{BakeDayId, Clock, PickupLocationId};
use jiff::civil::{Weekday, time};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

use crate::common::{
    DE_PROEF, FixedOrders, backend_pid, charity, create_day, date, next_saturday, product,
    saturday, ts, wait_until_blocked,
};

fn schedule_of(day: &bakedays::BakeDay) -> Schedule {
    Schedule {
        online_ordering: day.online_ordering,
        menu_publish_at: day.menu_publish_at,
        opens_at: day.opens_at,
        cutoff_at: day.cutoff_at,
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn new_day_takes_pickup_and_deadline_from_the_window(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    assert_eq!(day.date, date("2026-10-31"));
    assert_eq!(day.status, BakeDayStatus::Draft);
    assert!(day.online_ordering);
    assert_eq!(day.cutoff_at, Some(ts("2026-10-29T19:00:00Z")));
    // Baking starts at 05:00 CET.
    assert_eq!(day.bake_starts_at, ts("2026-10-31T04:00:00Z"));
    assert_eq!(
        (day.menu_publish_at, day.opens_at, day.charity_id),
        (None, None, None)
    );
    assert_eq!(days::get(&mut conn, day.id).await.unwrap(), day);

    let slots = days::list_slots(&mut conn, day.id).await.unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].bake_day_id, day.id);
    assert_eq!(slots[0].location_id, DE_PROEF);
    assert_eq!(
        (slots[0].starts_at, slots[0].ends_at, slots[0].max_orders),
        (ts("2026-10-31T10:00:00Z"), ts("2026-10-31T11:00:00Z"), None)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn summer_time_saturday_gets_summer_time_times(mut conn: PoolConnection<Postgres>) {
    let day = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-10-24"),
            ..saturday()
        },
    )
    .await;
    assert_eq!(day.cutoff_at, Some(ts("2026-10-22T18:00:00Z")));
    let slots = days::list_slots(&mut conn, day.id).await.unwrap();
    assert_eq!(slots[0].starts_at, ts("2026-10-24T09:00:00Z"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn every_active_window_gets_a_slot_and_the_earliest_deadline_wins(
    mut conn: PoolConnection<Postgres>,
) {
    let window = |starts, ends, days_before, active| PickupWindowInput {
        weekday: Weekday::Saturday,
        starts: time(starts, 0, 0, 0),
        ends: time(ends, 0, 0, 0),
        default_deadline_days_before: days_before,
        default_deadline_time: time(12, 0, 0, 0),
        active,
    };
    pickup::create_window(&mut conn, DE_PROEF, &window(14, 15, 1, true))
        .await
        .unwrap();
    pickup::create_window(&mut conn, DE_PROEF, &window(16, 17, 5, false))
        .await
        .unwrap();
    let day = create_day(&mut conn, &saturday()).await;
    let starts: Vec<_> = days::list_slots(&mut conn, day.id)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.starts_at)
        .collect();
    assert_eq!(
        starts,
        [ts("2026-10-31T10:00:00Z"), ts("2026-10-31T13:00:00Z")]
    );
    assert_eq!(day.cutoff_at, Some(ts("2026-10-29T19:00:00Z")));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn days_without_a_window_or_location_are_refused(mut conn: PoolConnection<Postgres>) {
    let friday = NewBakeDay {
        date: date("2026-10-30"),
        ..saturday()
    };
    assert!(matches!(
        days::create(&mut conn, &friday).await,
        Err(Error::NoPickupWindow(Weekday::Friday))
    ));
    let nowhere = NewBakeDay {
        location_id: PickupLocationId::new(999),
        ..saturday()
    };
    assert!(matches!(
        days::create(&mut conn, &nowhere).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        days::get(&mut conn, BakeDayId::new(999)).await,
        Err(Error::NotFound)
    ));
    assert!(
        days::list_between(&mut conn, date("2026-01-01"), date("2026-12-31"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn schedule_changes_per_drop(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    let changed = Schedule {
        menu_publish_at: Some(ts("2026-10-28T07:00:00Z")),
        opens_at: Some(ts("2026-10-28T07:00:00Z")),
        cutoff_at: Some(ts("2026-10-29T11:00:00Z")),
        ..schedule_of(&day)
    };
    let updated = days::update_schedule(&mut conn, day.id, &changed)
        .await
        .unwrap();
    assert_eq!(schedule_of(&updated), changed);

    let invalid = [
        Schedule {
            cutoff_at: None,
            opens_at: None,
            ..changed
        },
        Schedule {
            opens_at: Some(ts("2026-10-29T11:00:00Z")),
            ..changed
        },
        Schedule {
            menu_publish_at: Some(ts("2026-10-29T11:00:01Z")),
            ..changed
        },
        Schedule {
            cutoff_at: Some(ts("2026-10-31T04:00:01Z")),
            ..changed
        },
        Schedule {
            online_ordering: false,
            ..changed
        },
    ];
    for schedule in invalid {
        assert!(
            matches!(
                days::update_schedule(&mut conn, day.id, &schedule).await,
                Err(Error::Invalid(_))
            ),
            "{schedule:?}"
        );
    }
    assert!(matches!(
        days::update_schedule(&mut conn, BakeDayId::new(999), &changed).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn production_only_days_never_open(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-26T12:00:00Z"));
    let day = create_day(
        &mut conn,
        &NewBakeDay {
            online_ordering: false,
            ..next_saturday()
        },
    )
    .await;
    assert!(!day.online_ordering);
    assert_eq!(day.public_status(clock.now(), false), None);
    assert!(matches!(
        days::open(&mut conn, &clock, day.id).await,
        Err(Error::Transition(TransitionRefused::ProductionOnly))
    ));
    let closed = days::close(&mut conn, &clock, day.id).await.unwrap();
    assert_eq!(closed.status, BakeDayStatus::Closed);

    let online = create_day(&mut conn, &saturday()).await;
    days::open(&mut conn, &clock, online.id).await.unwrap();
    let production_only = Schedule {
        online_ordering: false,
        ..schedule_of(&online)
    };
    assert!(matches!(
        days::update_schedule(&mut conn, online.id, &production_only).await,
        Err(Error::Invalid(_))
    ));

    let err = sqlx::query("UPDATE bake_day SET online_ordering = false WHERE id = $1")
        .bind(online.id.get())
        .execute(&mut *conn)
        .await
        .unwrap_err();
    assert_eq!(
        err.as_database_error().and_then(|e| e.constraint()),
        Some("bake_day_production_only_never_opens")
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_day_without_pickup_slots_never_sells_online(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-26T12:00:00Z"));
    let friday = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-10-30"),
            online_ordering: false,
            ..saturday()
        },
    )
    .await;
    assert!(
        days::list_slots(&mut conn, friday.id)
            .await
            .unwrap()
            .is_empty()
    );
    let online = Schedule {
        online_ordering: true,
        cutoff_at: Some(ts("2026-10-28T19:00:00Z")),
        ..schedule_of(&friday)
    };
    assert!(matches!(
        days::update_schedule(&mut conn, friday.id, &online).await,
        Err(Error::NoPickupSlot)
    ));
    assert!(
        !days::get(&mut conn, friday.id)
            .await
            .unwrap()
            .online_ordering
    );

    sqlx::query(
        "UPDATE bake_day SET online_ordering = true, cutoff_at = '2026-10-28T19:00:00Z' WHERE id = $1",
    )
    .bind(friday.id.get())
    .execute(&mut *conn)
    .await
    .unwrap();
    assert!(matches!(
        days::open(&mut conn, &clock, friday.id).await,
        Err(Error::NoPickupSlot)
    ));
    assert_eq!(
        days::get(&mut conn, friday.id).await.unwrap().status,
        BakeDayStatus::Draft
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn status_moves_through_the_state_machine(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-29T18:59:59Z"));
    let day = create_day(&mut conn, &saturday()).await;
    assert_eq!(
        day.public_status(clock.now(), false),
        Some(PublicStatus::Planned)
    );
    let open = days::open(&mut conn, &clock, day.id).await.unwrap();
    assert_eq!(open.status, BakeDayStatus::Open);
    assert_eq!(
        open.public_status(clock.now(), true),
        Some(PublicStatus::SoldOut)
    );
    assert!(matches!(
        days::open(&mut conn, &clock, day.id).await,
        Err(Error::Transition(TransitionRefused::NotAllowed {
            from: BakeDayStatus::Open,
            transition: Transition::Open
        }))
    ));
    clock.set(ts("2026-10-29T19:00:00Z"));
    assert_eq!(
        days::close(&mut conn, &clock, day.id).await.unwrap().status,
        BakeDayStatus::Closed
    );
    clock.set(ts("2026-10-31T02:00:00Z"));
    assert!(matches!(
        days::finish(&mut conn, &clock, day.id).await,
        Err(Error::Transition(TransitionRefused::NotBaked))
    ));
    clock.set(ts("2026-10-31T12:00:00Z"));
    let done = days::finish(&mut conn, &clock, day.id).await.unwrap();
    assert_eq!(done.status, BakeDayStatus::Done);
    assert!(matches!(
        days::cancel(&mut conn, &clock, day.id, "Ziek").await,
        Err(Error::Transition(_))
    ));
    assert!(matches!(
        days::update_schedule(&mut conn, day.id, &schedule_of(&done)).await,
        Err(Error::Final)
    ));
    assert!(matches!(
        days::open(&mut conn, &clock, BakeDayId::new(999)).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn opening_after_the_deadline_is_refused(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-29T19:00:00Z"));
    let day = create_day(&mut conn, &saturday()).await;
    assert!(matches!(
        days::open(&mut conn, &clock, day.id).await,
        Err(Error::Transition(TransitionRefused::DeadlinePassed))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn cancelling_keeps_the_message(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let day = create_day(&mut conn, &saturday()).await;
    assert!(matches!(
        days::cancel(&mut conn, &clock, day.id, "  ").await,
        Err(Error::Invalid(_))
    ));
    let cancelled = days::cancel(&mut conn, &clock, day.id, " De oven is kapot. ")
        .await
        .unwrap();
    assert_eq!(cancelled.status, BakeDayStatus::Cancelled);
    assert_eq!(
        cancelled.cancel_message.as_deref(),
        Some("De oven is kapot.")
    );
    assert_eq!(
        cancelled.public_status(ts("2026-10-28T12:00:00Z"), false),
        Some(PublicStatus::Cancelled)
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn charity_changes_only_until_the_first_order(mut conn: PoolConnection<Postgres>) {
    let food_bank = charity(&mut conn, "Voedselbank", false).await;
    let archived = charity(&mut conn, "Oud", true).await;
    let day = create_day(
        &mut conn,
        &NewBakeDay {
            charity_id: Some(food_bank),
            ..saturday()
        },
    )
    .await;
    assert_eq!(day.charity_id, Some(food_bank));

    let normal_sale = days::set_charity(&mut conn, day.id, None, &NoOrders)
        .await
        .unwrap();
    assert_eq!(normal_sale.charity_id, None);
    assert!(matches!(
        days::set_charity(&mut conn, day.id, Some(food_bank), &FixedOrders::count(1)).await,
        Err(Error::OrdersExist)
    ));
    assert!(matches!(
        days::set_charity(&mut conn, day.id, Some(archived), &NoOrders).await,
        Err(Error::CharityArchived)
    ));
    assert!(matches!(
        days::set_charity(
            &mut conn,
            day.id,
            Some(domain::CharityId::new(999)),
            &NoOrders
        )
        .await,
        Err(Error::Invalid("charity"))
    ));
    assert!(matches!(
        days::create(
            &mut conn,
            &NewBakeDay {
                charity_id: Some(archived),
                ..saturday()
            }
        )
        .await,
        Err(Error::CharityArchived)
    ));
    assert!(days::ensure_charity_changeable(0).is_ok());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn slots_change_per_drop(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let day = create_day(&mut conn, &saturday()).await;
    let slot = days::list_slots(&mut conn, day.id).await.unwrap()[0];
    let later = PickupSlotInput {
        starts_at: ts("2026-10-31T11:00:00Z"),
        ends_at: ts("2026-10-31T12:30:00Z"),
        max_orders: Some(20),
    };
    days::update_slot(&mut conn, slot.id, &later).await.unwrap();
    let extra = days::add_slot(&mut conn, day.id, DE_PROEF, &later)
        .await
        .unwrap();
    let slots = days::list_slots(&mut conn, day.id).await.unwrap();
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[1].id, extra);
    for s in &slots {
        assert_eq!(
            (s.starts_at, s.ends_at, s.max_orders),
            (later.starts_at, later.ends_at, later.max_orders)
        );
    }

    let backwards = PickupSlotInput {
        ends_at: later.starts_at,
        ..later
    };
    let no_orders = PickupSlotInput {
        max_orders: Some(0),
        ..later
    };
    for input in [backwards, no_orders] {
        assert!(matches!(
            days::update_slot(&mut conn, slot.id, &input).await,
            Err(Error::Invalid(_))
        ));
    }
    assert!(matches!(
        days::update_slot(&mut conn, domain::PickupSlotId::new(999), &later).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        days::add_slot(&mut conn, day.id, PickupLocationId::new(999), &later).await,
        Err(Error::NotFound)
    ));
    days::cancel(&mut conn, &clock, day.id, "Ziek")
        .await
        .unwrap();
    assert!(matches!(
        days::update_slot(&mut conn, slot.id, &later).await,
        Err(Error::Final)
    ));
    assert!(matches!(
        days::add_slot(&mut conn, day.id, DE_PROEF, &later).await,
        Err(Error::Final)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_slot_moves_to_another_date_but_never_before_baking_starts(
    mut conn: PoolConnection<Postgres>,
) {
    let day = create_day(&mut conn, &saturday()).await;
    let slot = days::list_slots(&mut conn, day.id).await.unwrap()[0];
    let sunday = PickupSlotInput {
        starts_at: ts("2026-11-01T10:00:00Z"),
        ends_at: ts("2026-11-01T11:00:00Z"),
        max_orders: None,
    };
    let before = days::update_slot(&mut conn, slot.id, &sunday)
        .await
        .unwrap();
    assert_eq!(before, slot);
    let moved = days::list_slots(&mut conn, day.id).await.unwrap()[0];
    assert_eq!(
        (moved.starts_at, moved.ends_at),
        (sunday.starts_at, sunday.ends_at)
    );

    // Baking starts at 05:00 CET, 04:00 UTC.
    let at_bake_start = PickupSlotInput {
        starts_at: ts("2026-10-31T04:00:00Z"),
        ends_at: ts("2026-10-31T05:00:00Z"),
        max_orders: None,
    };
    days::update_slot(&mut conn, slot.id, &at_bake_start)
        .await
        .unwrap();
    let too_early = PickupSlotInput {
        starts_at: ts("2026-10-31T03:59:00Z"),
        ..at_bake_start
    };
    assert!(matches!(
        days::update_slot(&mut conn, slot.id, &too_early).await,
        Err(Error::Invalid("pickup before bake start"))
    ));
    assert!(matches!(
        days::add_slot(&mut conn, day.id, DE_PROEF, &too_early).await,
        Err(Error::Invalid("pickup before bake start"))
    ));
}

async fn order_in_slot(conn: &mut PgConnection, slot: &bakedays::PickupSlot) {
    sqlx::query(
        "WITH c AS (INSERT INTO customer (email, name, created_at)
            VALUES ('klant@example.org', 'Test Klant', now()) RETURNING id),
        n AS (INSERT INTO order_number (year, last) VALUES (2026, 1) RETURNING year)
        INSERT INTO \"order\" (number_year, number_seq, customer_id, customer_name,
            bake_day_id, pickup_slot_id, channel, payment_mode, status, cancel_reason,
            total_cents, created_at)
        SELECT n.year, 1, c.id, 'Test Klant', $1, $2, 'web', 'online', 'cancelled',
            'hold_expired', 650, now()
        FROM c, n",
    )
    .bind(slot.bake_day_id.get())
    .bind(slot.id.get())
    .execute(conn)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_slot_is_removed_only_without_orders_and_an_online_day_keeps_one(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let day = create_day(&mut conn, &saturday()).await;
    let first = days::list_slots(&mut conn, day.id).await.unwrap()[0];
    let input = PickupSlotInput {
        starts_at: ts("2026-10-31T12:00:00Z"),
        ends_at: ts("2026-10-31T13:00:00Z"),
        max_orders: None,
    };
    let second = days::add_slot(&mut conn, day.id, DE_PROEF, &input)
        .await
        .unwrap();
    days::remove_slot(&mut conn, second).await.unwrap();
    assert_eq!(days::list_slots(&mut conn, day.id).await.unwrap(), [first]);
    assert!(matches!(
        days::remove_slot(&mut conn, first.id).await,
        Err(Error::NoPickupSlot)
    ));
    assert!(matches!(
        days::remove_slot(&mut conn, second).await,
        Err(Error::NotFound)
    ));

    let third = days::add_slot(&mut conn, day.id, DE_PROEF, &input)
        .await
        .unwrap();
    let third = days::list_slots(&mut conn, day.id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == third)
        .unwrap();
    order_in_slot(&mut conn, &third).await;
    assert!(matches!(
        days::remove_slot(&mut conn, third.id).await,
        Err(Error::SlotHasOrders)
    ));
    assert_eq!(days::list_slots(&mut conn, day.id).await.unwrap().len(), 2);

    let production = create_day(
        &mut conn,
        &NewBakeDay {
            online_ordering: false,
            ..next_saturday()
        },
    )
    .await;
    let markt: i64 = sqlx::query_scalar(
        "INSERT INTO pickup_location (name, address) VALUES ('Markt', 'Brink 1') RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let slotless = create_day(
        &mut conn,
        &NewBakeDay {
            location_id: PickupLocationId::new(markt),
            online_ordering: false,
            ..next_saturday()
        },
    )
    .await;
    let only = days::list_slots(&mut conn, production.id).await.unwrap()[0];
    assert!(matches!(
        days::remove_slot(&mut conn, only.id).await,
        Err(Error::DateTaken(_))
    ));
    days::cancel(&mut conn, &clock, slotless.id, "Geen markt")
        .await
        .unwrap();
    days::remove_slot(&mut conn, only.id).await.unwrap();
    assert!(
        days::list_slots(&mut conn, production.id)
            .await
            .unwrap()
            .is_empty()
    );

    days::cancel(&mut conn, &clock, day.id, "Ziek")
        .await
        .unwrap();
    assert!(matches!(
        days::remove_slot(&mut conn, first.id).await,
        Err(Error::Final)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn copy_moves_times_by_local_days_and_brings_the_range(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-20T12:00:00Z"));
    let archived_later = charity(&mut conn, "Later gearchiveerd", false).await;
    let source = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-10-24"),
            charity_id: Some(archived_later),
            ..saturday()
        },
    )
    .await;
    let source = days::update_schedule(
        &mut conn,
        source.id,
        &Schedule {
            menu_publish_at: Some(ts("2026-10-21T06:00:00Z")),
            opens_at: Some(ts("2026-10-21T06:00:00Z")),
            ..schedule_of(&source)
        },
    )
    .await
    .unwrap();
    days::open(&mut conn, &clock, source.id).await.unwrap();
    let bread = product(&mut conn, "Desem").await;
    let rolls = product(&mut conn, "Bolletjes").await;
    for (product_id, max_qty) in [(bread, Some(12)), (rolls, None)] {
        range::set_product(
            &mut conn,
            source.id,
            &BakeDayProduct {
                product_id,
                max_qty,
            },
        )
        .await
        .unwrap();
    }
    let extra = ExtraInput {
        qty: 2,
        purpose: Purpose::Freezer,
        note: None,
    };
    extras::add(&mut conn, source.id, bread, &extra, &NoOrders)
        .await
        .unwrap();
    sqlx::query("UPDATE charity SET archived_at = now() WHERE id = $1")
        .bind(archived_later.get())
        .execute(&mut *conn)
        .await
        .unwrap();

    let copied = days::copy_to(&mut conn, source.id, date("2026-10-31"))
        .await
        .unwrap();
    assert!(copied.charity_dropped);
    let copy = copied.day;
    assert!(matches!(
        days::copy_to(&mut conn, source.id, date("2026-10-31")).await,
        Err(Error::DateTaken(d)) if d == date("2026-10-31")
    ));
    assert!(matches!(
        days::copy_to(&mut conn, source.id, source.date).await,
        Err(Error::DateTaken(_))
    ));
    assert_ne!(copy.id, source.id);
    assert_eq!(copy.date, date("2026-10-31"));
    assert_eq!(copy.status, BakeDayStatus::Draft);
    assert_eq!(copy.charity_id, None);
    // Same local times, one hour later in UTC after the change to winter time.
    assert_eq!(copy.menu_publish_at, Some(ts("2026-10-28T07:00:00Z")));
    assert_eq!(copy.opens_at, Some(ts("2026-10-28T07:00:00Z")));
    assert_eq!(copy.cutoff_at, Some(ts("2026-10-29T19:00:00Z")));
    assert_eq!(copy.bake_starts_at, ts("2026-10-31T04:00:00Z"));
    let slots = days::list_slots(&mut conn, copy.id).await.unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].starts_at, ts("2026-10-31T10:00:00Z"));

    let products = range::list_products(&mut conn, copy.id).await.unwrap();
    assert_eq!(
        products,
        [
            BakeDayProduct {
                product_id: bread,
                max_qty: Some(12),
            },
            BakeDayProduct {
                product_id: rolls,
                max_qty: None,
            },
        ]
    );
    assert!(extras::list(&mut conn, copy.id).await.unwrap().is_empty());
    assert!(matches!(
        days::copy_to(&mut conn, BakeDayId::new(999), date("2026-10-31")).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn repeat_weekly_copies_to_the_next_weeks(mut conn: PoolConnection<Postgres>) {
    let food_bank = charity(&mut conn, "Voedselbank", false).await;
    let source = create_day(
        &mut conn,
        &NewBakeDay {
            charity_id: Some(food_bank),
            ..saturday()
        },
    )
    .await;
    let copies = days::repeat_weekly(&mut conn, source.id, 3).await.unwrap();
    assert!(copies.iter().all(|c| !c.charity_dropped));
    let copies: Vec<_> = copies.into_iter().map(|c| c.day).collect();
    let dates: Vec<_> = copies.iter().map(|d| d.date).collect();
    assert_eq!(
        dates,
        [date("2026-11-07"), date("2026-11-14"), date("2026-11-21")]
    );
    assert!(copies.iter().all(|d| d.charity_id == Some(food_bank)));
    assert_eq!(copies[2].cutoff_at, Some(ts("2026-11-19T19:00:00Z")));
    let listed = days::list_between(&mut conn, date("2026-10-31"), date("2026-11-14"))
        .await
        .unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0], source);
    assert!(matches!(
        days::repeat_weekly(&mut conn, source.id, 1).await,
        Err(Error::DateTaken(_))
    ));
    for weeks in [0, 53] {
        assert!(matches!(
            days::repeat_weekly(&mut conn, source.id, weeks).await,
            Err(Error::Invalid(_))
        ));
    }
}

async fn set_cutoff(conn: &mut PgConnection, day: &bakedays::BakeDay, cutoff: &str) {
    let schedule = Schedule {
        cutoff_at: Some(ts(cutoff)),
        ..schedule_of(day)
    };
    days::update_schedule(conn, day.id, &schedule)
        .await
        .unwrap();
}

fn sourdough_warning(cutoff: &str, latest: &str) -> Option<LeadTimeWarning> {
    Some(LeadTimeWarning {
        cutoff_at: ts(cutoff),
        latest_cutoff: ts(latest),
    })
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn deadline_after_thursday_8pm_warns_for_a_saturday_drop(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    assert_eq!(day.cutoff_at, Some(ts("2026-10-29T19:00:00Z")));
    assert_eq!(
        days::lead_time_warning(&mut conn, day.id).await.unwrap(),
        None
    );

    for cutoff in ["2026-10-29T19:01:00Z", "2026-10-30T19:00:00Z"] {
        set_cutoff(&mut conn, &day, cutoff).await;
        assert_eq!(
            days::lead_time_warning(&mut conn, day.id).await.unwrap(),
            sourdough_warning(cutoff, "2026-10-29T19:00:00Z")
        );
    }
    assert!(matches!(
        days::lead_time_warning(&mut conn, BakeDayId::new(999)).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn summer_time_saturday_after_the_march_change(mut conn: PoolConnection<Postgres>) {
    let day = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-04-04"),
            ..saturday()
        },
    )
    .await;
    assert_eq!(day.cutoff_at, Some(ts("2026-04-02T18:00:00Z")));
    assert_eq!(
        days::lead_time_warning(&mut conn, day.id).await.unwrap(),
        None
    );
    set_cutoff(&mut conn, &day, "2026-04-02T19:00:00Z").await;
    assert_eq!(
        days::lead_time_warning(&mut conn, day.id).await.unwrap(),
        sourdough_warning("2026-04-02T19:00:00Z", "2026-04-02T18:00:00Z")
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_rule_comes_from_the_company_settings(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    sqlx::query("UPDATE company SET sourdough_deadline_days_before = 3")
        .execute(&mut *conn)
        .await
        .unwrap();
    let warning = days::lead_time_warning(&mut conn, day.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(warning.latest_cutoff, ts("2026-10-28T19:00:00Z"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn baking_starts_at_five_on_the_bake_date(mut conn: PoolConnection<Postgres>) {
    // The clocks jump forward on Sunday 29 March: 05:00 is summer time.
    let spring = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-03-29"),
            online_ordering: false,
            ..saturday()
        },
    )
    .await;
    assert_eq!(spring.bake_starts_at, ts("2026-03-29T03:00:00Z"));
    // They fall back on Sunday 25 October: 05:00 is winter time.
    let autumn = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-10-25"),
            online_ordering: false,
            ..saturday()
        },
    )
    .await;
    assert_eq!(autumn.bake_starts_at, ts("2026-10-25T04:00:00Z"));

    let day = create_day(&mut conn, &saturday()).await;
    sqlx::query("UPDATE bake_day SET bake_starts_at = $2::timestamptz WHERE id = $1")
        .bind(day.id.get())
        .bind("2026-10-31T02:00:00Z")
        .execute(&mut *conn)
        .await
        .unwrap();
    let saved = days::update_schedule(&mut conn, day.id, &schedule_of(&day))
        .await
        .unwrap();
    assert_eq!(saved.bake_starts_at, ts("2026-10-31T04:00:00Z"));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn database_refuses_inconsistent_days(mut conn: PoolConnection<Postgres>) {
    let insert = |sql: &'static str| sqlx::query(sql);
    let cases = [
        (
            "INSERT INTO bake_day (date, bake_starts_at) VALUES ('2026-10-31', now())",
            "bake_day_online_needs_cutoff",
        ),
        (
            "INSERT INTO bake_day (date, bake_starts_at, cutoff_at, opens_at)
             VALUES ('2026-10-31', now(), now(), now())",
            "bake_day_opens_before_cutoff",
        ),
        (
            "INSERT INTO bake_day (date, bake_starts_at, cutoff_at, status)
             VALUES ('2026-10-31', now(), now(), 'cancelled')",
            "bake_day_cancel_message",
        ),
        (
            "INSERT INTO bake_day (date, bake_starts_at, cutoff_at, menu_publish_at)
             VALUES ('2026-10-31', now(), now(), now() + interval '1 hour')",
            "bake_day_menu_before_cutoff",
        ),
        (
            "INSERT INTO bake_day (date, bake_starts_at, cutoff_at)
             VALUES ('2026-10-31', now(), now() + interval '1 hour')",
            "bake_day_cutoff_before_bake",
        ),
    ];
    for (sql, constraint) in cases {
        let err = insert(sql).execute(&mut *conn).await.unwrap_err();
        assert_eq!(
            err.as_database_error().and_then(|e| e.constraint()),
            Some(constraint)
        );
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn production_only_days_need_no_pickup_window(mut conn: PoolConnection<Postgres>) {
    let friday = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-10-30"),
            online_ordering: false,
            ..saturday()
        },
    )
    .await;
    assert_eq!(friday.cutoff_at, None);
    assert!(
        days::list_slots(&mut conn, friday.id)
            .await
            .unwrap()
            .is_empty()
    );

    let bread = product(&mut conn, "Desem").await;
    range::set_product(
        &mut conn,
        friday.id,
        &BakeDayProduct {
            product_id: bread,
            max_qty: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        days::lead_time_warning(&mut conn, friday.id).await.unwrap(),
        None
    );

    let saturday_production = create_day(
        &mut conn,
        &NewBakeDay {
            online_ordering: false,
            ..saturday()
        },
    )
    .await;
    assert_eq!(
        saturday_production.cutoff_at,
        Some(ts("2026-10-29T19:00:00Z"))
    );
    assert!(matches!(
        days::copy_to(&mut conn, friday.id, date("2026-11-06")).await,
        Ok(copy) if copy.day.cutoff_at.is_none()
    ));
    let other_friday = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-11-13"),
            online_ordering: false,
            ..saturday()
        },
    )
    .await;
    assert!(matches!(
        days::copy_to(&mut conn, friday.id, other_friday.date).await,
        Err(Error::DateTaken(_))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_cancelled_day_does_not_block_a_copy(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-20T12:00:00Z"));
    let source = create_day(&mut conn, &saturday()).await;
    let later = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-11-07"),
            ..saturday()
        },
    )
    .await;
    days::cancel(&mut conn, &clock, later.id, "Ziek")
        .await
        .unwrap();
    days::copy_to(&mut conn, source.id, later.date)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn one_location_gets_one_bake_day_per_date(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    assert!(matches!(
        days::create(&mut conn, &saturday()).await,
        Err(Error::DateTaken(d)) if d == day.date
    ));
    let market = pickup::create_location(
        &mut conn,
        &bakedays::PickupLocationInput {
            name: "Markt".to_owned(),
            address: "Brink 1, 8380 AA Dorp".to_owned(),
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    let slot = PickupSlotInput {
        starts_at: ts("2026-10-31T13:00:00Z"),
        ends_at: ts("2026-10-31T14:00:00Z"),
        max_orders: None,
    };
    days::add_slot(&mut conn, day.id, market.id, &slot)
        .await
        .unwrap();
    days::add_slot(&mut conn, day.id, DE_PROEF, &slot)
        .await
        .unwrap();
    let other = create_day(
        &mut conn,
        &NewBakeDay {
            online_ordering: false,
            date: date("2026-10-30"),
            ..saturday()
        },
    )
    .await;
    assert!(matches!(
        days::copy_to(&mut conn, other.id, date("2026-10-30")).await,
        Err(Error::DateTaken(_))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn concurrent_copies_to_one_date_make_one_day(pool: sqlx::PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let source = create_day(&mut conn, &next_saturday()).await;
    drop(conn);

    let mut first = pool.begin().await.unwrap();
    days::create(&mut first, &saturday()).await.unwrap();

    let mut second = pool.acquire().await.unwrap();
    let pid = backend_pid(&mut second).await;
    let task =
        tokio::spawn(
            async move { days::copy_to(&mut second, source.id, date("2026-10-31")).await },
        );
    wait_until_blocked(&pool, pid).await;
    first.commit().await.unwrap();
    let copied = task.await.unwrap();
    assert!(matches!(copied, Err(Error::DateTaken(_))), "{copied:?}");
}
