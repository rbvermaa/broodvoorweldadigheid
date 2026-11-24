use orders::order::{self, CustomerOrder};
use orders::{OrderStatus, OrderedBy, checkout};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;

use crate::common::{
    BREAD, cart, guest, on_range, open_day, order_as_guest, product, saturday, wednesday,
};

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_customer_sees_only_their_own_orders_newest_first(mut conn: PoolConnection<Postgres>) {
    let clock = wednesday();
    let shop = open_day(&mut conn, &clock, &saturday()).await;
    let bread = product(&mut conn, &BREAD).await;
    on_range(&mut conn, &shop, bread, None, None).await;
    let first = order_as_guest(&mut conn, &clock, &shop, &[(bread, 1, None)])
        .await
        .unwrap();
    let second = order_as_guest(&mut conn, &clock, &shop, &[(bread, 2, None)])
        .await
        .unwrap();
    checkout(
        &mut conn,
        &clock,
        &cart(&shop, &[(bread, 3, None)]),
        &guest("iemand-anders@example.org"),
        shop.slot,
        OrderedBy::Guest,
    )
    .await
    .unwrap();

    let mine = order::for_customer(&mut conn, first.customer_id)
        .await
        .unwrap();
    let token = order::status_token(&mut conn, second.id).await.unwrap();
    assert_eq!(
        mine.iter().map(|o| o.id).collect::<Vec<_>>(),
        vec![second.id, first.id]
    );
    assert_eq!(
        mine[0],
        CustomerOrder {
            id: second.id,
            number: second.number,
            date: shop.day.date,
            status: OrderStatus::PendingPayment,
            total: second.total,
            status_token: token,
        }
    );
}
