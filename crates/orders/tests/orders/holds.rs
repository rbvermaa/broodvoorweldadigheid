use bakedays::{OrderSource, days};
use jiff::SignedDuration;
use orders::audit;
use orders::{CancelReason, Error, HOLD, OrderStatus, Orders, release_expired_holds};
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};

use crate::common::{BREAD, on_range, open_day, order_as_guest, product, saturday, wednesday};

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_expired_hold_frees_the_loaf_and_the_job_cancels_the_order(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, Some(1), None).await;
    let held = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    assert!(matches!(
        order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)]).await,
        Err(Error::CapacityExceeded { remaining: 0, .. })
    ));

    clock.advance(HOLD - SignedDuration::from_secs(1));
    assert!(
        release_expired_holds(&mut conn, &clock)
            .await
            .unwrap()
            .is_empty()
    );
    let source = Orders::new(&clock);
    assert_eq!(
        source.ordered(&mut conn, shop.day.id).await.unwrap()[&bread],
        1
    );

    clock.advance(SignedDuration::from_secs(1));
    assert!(
        source
            .ordered(&mut conn, shop.day.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(source.count(&mut conn, shop.day.id).await.unwrap(), 1);
    assert_eq!(
        release_expired_holds(&mut conn, &clock).await.unwrap(),
        [held.id]
    );
    assert!(
        release_expired_holds(&mut conn, &clock)
            .await
            .unwrap()
            .is_empty()
    );
    let released = orders::order::get(&mut conn, held.id).await.unwrap();
    assert_eq!(released.status, OrderStatus::Cancelled);
    assert_eq!(released.cancel_reason, Some(CancelReason::HoldExpired));
    assert_eq!(source.count(&mut conn, shop.day.id).await.unwrap(), 0);

    let history = audit::history(&mut conn, "order", held.id.get())
        .await
        .unwrap();
    let actions: Vec<_> = history.iter().map(|h| h.action.as_str()).collect();
    assert_eq!(actions, ["created", "hold_expired"]);
    assert_eq!(history[1].actor, "system:hold-expiry");
    assert_eq!(
        history[1].before.as_ref().unwrap()["status"],
        "pending_payment"
    );
    assert_eq!(
        history[1].after.as_ref().unwrap()["cancel_reason"],
        "hold_expired"
    );
    assert_eq!(history[1].at, domain::Clock::now(&clock));

    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_charity_is_fixed_once_an_order_is_held(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let source = Orders::new(&clock);
    assert!(matches!(
        days::set_charity(&mut conn, shop.day.id, None, &source).await,
        Err(bakedays::Error::OrdersExist)
    ));
    clock.advance(HOLD);
    release_expired_holds(&mut conn, &clock).await.unwrap();
    days::set_charity(&mut conn, shop.day.id, None, &source)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_audit_log_is_append_only(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    for sql in [
        "UPDATE audit_log SET actor = 'iemand'",
        "DELETE FROM audit_log",
        "TRUNCATE audit_log",
    ] {
        let err = sqlx::query(sql).execute(&mut *conn).await.unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
}
