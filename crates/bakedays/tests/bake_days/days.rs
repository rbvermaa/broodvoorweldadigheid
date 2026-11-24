use std::collections::BTreeMap;

use bakedays::schedule::LeadTimeWarning;
use bakedays::status::TransitionRefused;
use bakedays::{
    BakeDayProduct, BakeDayStatus, Error, ExtraInput, NewBakeDay, PickupSlotInput,
    PickupWindowInput, PublicStatus, Purpose, Schedule, Transition, days, extras, pickup, range,
};
use domain::{BakeDayId, Clock, PickupLocationId};
use jiff::civil::{Weekday, time};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{DE_PROEF, charity, create_day, date, product, saturday, ts};

fn schedule_of(day: &bakedays::BakeDay) -> Schedule {
    Schedule {
        online_ordering: day.online_ordering,
        menu_publish_at: day.menu_publish_at,
        opens_at: day.opens_at,
        cutoff_at: day.cutoff_at,
        bake_starts_at: day.bake_starts_at,
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn new_day_takes_pickup_and_deadline_from_the_window(mut conn: PoolConnection<Postgres>) {
    let day = create_day(&mut conn, &saturday()).await;
    assert_eq!(day.date, date("2026-10-31"));
    assert_eq!(day.status, BakeDayStatus::Draft);
    assert!(day.online_ordering);
    assert_eq!(day.cutoff_at, Some(ts("2026-10-29T19:00:00Z")));
    assert_eq!(day.bake_starts_at, ts("2026-10-31T03:00:00Z"));
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
            ..saturday()
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

    let normal_sale = days::set_charity(&mut conn, day.id, None, 0).await.unwrap();
    assert_eq!(normal_sale.charity_id, None);
    assert!(matches!(
        days::set_charity(&mut conn, day.id, Some(food_bank), 1).await,
        Err(Error::OrdersExist)
    ));
    assert!(matches!(
        days::set_charity(&mut conn, day.id, Some(archived), 0).await,
        Err(Error::CharityArchived)
    ));
    assert!(matches!(
        days::set_charity(&mut conn, day.id, Some(domain::CharityId::new(999)), 0).await,
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
async fn copy_moves_times_by_local_days_and_brings_the_range(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-20T12:00:00Z"));
    let archived_later = charity(&mut conn, "Later gearchiveerd", false).await;
    let source = create_day(
        &mut conn,
        &NewBakeDay {
            date: date("2026-10-24"),
            bake_starts_at: ts("2026-10-24T02:00:00Z"),
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
    let bread = product(&mut conn, "Desem", None).await;
    let rolls = product(&mut conn, "Bolletjes", None).await;
    let oven = range::create_pool(&mut conn, source.id, "Oven", 40)
        .await
        .unwrap();
    for (product_id, pool) in [(bread, Some(oven.id)), (rolls, None)] {
        range::set_product(
            &mut conn,
            source.id,
            &BakeDayProduct {
                product_id,
                max_qty: Some(12),
                capacity_pool_id: pool,
                units_per_item: 2,
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
    extras::add(&mut conn, source.id, bread, &extra, &BTreeMap::new())
        .await
        .unwrap();
    sqlx::query("UPDATE charity SET archived_at = now() WHERE id = $1")
        .bind(archived_later.get())
        .execute(&mut *conn)
        .await
        .unwrap();

    let copy = days::copy_to(&mut conn, source.id, date("2026-10-31"))
        .await
        .unwrap();
    assert_ne!(copy.id, source.id);
    assert_eq!(copy.date, date("2026-10-31"));
    assert_eq!(copy.status, BakeDayStatus::Draft);
    assert_eq!(copy.charity_id, None);
    // Same local times, one hour later in UTC after the change to winter time.
    assert_eq!(copy.menu_publish_at, Some(ts("2026-10-28T07:00:00Z")));
    assert_eq!(copy.opens_at, Some(ts("2026-10-28T07:00:00Z")));
    assert_eq!(copy.cutoff_at, Some(ts("2026-10-29T19:00:00Z")));
    assert_eq!(copy.bake_starts_at, ts("2026-10-31T03:00:00Z"));
    let slots = days::list_slots(&mut conn, copy.id).await.unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].starts_at, ts("2026-10-31T10:00:00Z"));

    let pools = range::list_pools(&mut conn, copy.id).await.unwrap();
    assert_eq!(pools.len(), 1);
    assert_ne!(pools[0].id, oven.id);
    assert_eq!((pools[0].name.as_str(), pools[0].max_units), ("Oven", 40));
    let products = range::list_products(&mut conn, copy.id).await.unwrap();
    assert_eq!(
        products,
        [
            BakeDayProduct {
                product_id: bread,
                max_qty: Some(12),
                capacity_pool_id: Some(pools[0].id),
                units_per_item: 2,
            },
            BakeDayProduct {
                product_id: rolls,
                max_qty: Some(12),
                capacity_pool_id: None,
                units_per_item: 2,
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
    for weeks in [0, 53] {
        assert!(matches!(
            days::repeat_weekly(&mut conn, source.id, weeks).await,
            Err(Error::Invalid(_))
        ));
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn lead_time_warning_uses_the_longest_lead_time_on_the_drop(
    mut conn: PoolConnection<Postgres>,
) {
    let day = create_day(&mut conn, &saturday()).await;
    assert_eq!(
        days::lead_time_warning(&mut conn, day.id).await.unwrap(),
        None
    );

    let rolls = product(&mut conn, "Bolletjes", Some(12)).await;
    let bread = product(&mut conn, "Desem", None).await;
    let add = |product_id| BakeDayProduct {
        product_id,
        max_qty: None,
        capacity_pool_id: None,
        units_per_item: 1,
    };
    range::set_product(&mut conn, day.id, &add(rolls))
        .await
        .unwrap();
    assert_eq!(
        days::lead_time_warning(&mut conn, day.id).await.unwrap(),
        None
    );

    // Default 36 h before Saturday 04:00 local is Thursday 16:00, before the 20:00 deadline.
    range::set_product(&mut conn, day.id, &add(bread))
        .await
        .unwrap();
    assert_eq!(
        days::lead_time_warning(&mut conn, day.id).await.unwrap(),
        Some(LeadTimeWarning {
            product_id: bread,
            lead_time_hours: 36,
            cutoff_at: ts("2026-10-29T19:00:00Z"),
            latest_cutoff: ts("2026-10-29T15:00:00Z"),
        })
    );
    assert!(matches!(
        days::lead_time_warning(&mut conn, BakeDayId::new(999)).await,
        Err(Error::NotFound)
    ));
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
    ];
    for (sql, constraint) in cases {
        let err = insert(sql).execute(&mut *conn).await.unwrap_err();
        assert_eq!(
            err.as_database_error().and_then(|e| e.constraint()),
            Some(constraint)
        );
    }
}
