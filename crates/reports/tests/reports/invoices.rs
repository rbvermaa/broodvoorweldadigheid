use domain::{StaffUserId, VatRate};
use i18n::Translator;
use orders::audit::{self, Actor};
use reports::invoices::{self, NewBusinessAccount, NewInvoice};
use reports::{Error, Period};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

use crate::common::{BREAD, COOKIES, customer, date, drop_on, on_account_order, product, sell, ts};

const STAFF: Actor = Actor::Staff(StaffUserId::new(1));

fn account(customer: domain::CustomerId) -> NewBusinessAccount {
    NewBusinessAccount {
        customer_id: customer,
        company_name: " Café De Hoek ".into(),
        kvk: "12345678".into(),
        btw_id: Some("NL123456789B01".into()),
        invoice_address: "Dorpsstraat 1, 8382 AA Frederiksoord".into(),
        invoice_email: "Factuur@Example.org".into(),
        on_account_allowed: true,
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn business_accounts_are_checked(mut conn: PoolConnection<Postgres>) {
    let c = customer(&mut conn, "zaak@example.org").await;
    for bad in [
        NewBusinessAccount {
            kvk: "1234".into(),
            ..account(c)
        },
        NewBusinessAccount {
            company_name: " ".into(),
            ..account(c)
        },
        NewBusinessAccount {
            btw_id: Some(" ".into()),
            ..account(c)
        },
        NewBusinessAccount {
            invoice_email: "geen-mail".into(),
            ..account(c)
        },
    ] {
        assert!(matches!(
            invoices::create_account(&mut conn, &bad).await,
            Err(Error::Invalid(_))
        ));
    }
    let created = invoices::create_account(&mut conn, &account(c))
        .await
        .unwrap();
    assert_eq!(created.company_name, "Café De Hoek");
    assert_eq!(created.invoice_email, "factuur@example.org");
    assert_eq!(invoices::account(&mut conn, c).await.unwrap(), created);
    assert!(matches!(
        invoices::create_account(&mut conn, &account(c)).await,
        Err(Error::Invalid(_))
    ));
    let other = customer(&mut conn, "ander@example.org").await;
    assert!(matches!(
        invoices::account(&mut conn, other).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn uninvoiced_orders_are_listed_then_locked_by_the_invoice(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let first = drop_on(&mut conn, &clock, "2026-10-31", None).await;
    let second = drop_on(&mut conn, &clock, "2026-11-07", None).await;
    let bread = product(&mut conn, &BREAD).await;
    let cookies = product(&mut conn, &COOKIES).await;
    sell(&mut conn, &first, &[bread, cookies]).await;
    let c = customer(&mut conn, "zaak@example.org").await;
    invoices::create_account(&mut conn, &account(c))
        .await
        .unwrap();
    let a = on_account_order(
        &mut conn,
        c,
        &first,
        &[(bread, 3, 545, 9), (cookies, 2, 363, 21)],
        "2026-10-21T09:00:00Z",
    )
    .await;
    let b = on_account_order(
        &mut conn,
        c,
        &second,
        &[(bread, 1, 545, 9)],
        "2026-10-22T09:00:00Z",
    )
    .await;
    let other = customer(&mut conn, "ander@example.org").await;
    let foreign = on_account_order(
        &mut conn,
        other,
        &first,
        &[(bread, 1, 545, 9)],
        "2026-10-21T09:00:00Z",
    )
    .await;

    let october = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let overview = invoices::uninvoiced(&mut conn, c, october).await.unwrap();
    assert_eq!(overview.orders.len(), 1);
    let order = &overview.orders[0];
    assert_eq!(order.id, a);
    assert_eq!(order.drop_date, date("2026-10-31"));
    let bread_line = &order.lines[0];
    assert_eq!(bread_line.unit_price_excl.cents(), 500);
    assert_eq!(
        (
            bread_line.net.cents(),
            bread_line.vat.cents(),
            bread_line.gross.cents()
        ),
        (1500, 135, 1635)
    );
    let cookie_line = &order.lines[1];
    assert_eq!(
        (cookie_line.net.cents(), cookie_line.vat.cents()),
        (600, 126)
    );
    assert_eq!(overview.totals.rate(VatRate::Nine).net().cents(), 1500);
    assert_eq!(overview.totals.rate(VatRate::TwentyOne).vat().cents(), 126);

    let t = Translator::dutch().unwrap();
    let text = invoices::csv(&overview, &t).unwrap();
    assert!(
        text.contains("Desem;3;5,00;9%;15,00;1,35;16,35\r\n"),
        "{text}"
    );
    assert!(
        text.ends_with("Totaal;;;;;21%;6,00;1,26;7,26\r\n"),
        "{text}"
    );

    let invoice = |orders: Vec<domain::OrderId>, number: &str| NewInvoice {
        customer_id: c,
        orders,
        invoice_number: number.into(),
        invoice_date: date("2026-11-02"),
        period: october,
    };
    for (orders, number) in [(vec![b], "2026-001"), (vec![foreign], "2026-001")] {
        assert!(matches!(
            invoices::mark_invoiced(&mut conn, &clock, STAFF, &invoice(orders, number)).await,
            Err(Error::NotInvoiceable(_))
        ));
    }
    assert!(matches!(
        invoices::mark_invoiced(&mut conn, &clock, STAFF, &invoice(vec![], "2026-001")).await,
        Err(Error::Invalid(_))
    ));
    let made = invoices::mark_invoiced(&mut conn, &clock, STAFF, &invoice(vec![a, a], "2026-001"))
        .await
        .unwrap();
    assert_eq!(made.total.cents(), 1635 + 726);
    assert!(matches!(
        invoices::mark_invoiced(&mut conn, &clock, STAFF, &invoice(vec![a], "2026-002")).await,
        Err(Error::AlreadyInvoiced(id)) if id == a
    ));
    let november = Period::new(date("2026-11-01"), date("2026-11-30")).unwrap();
    let next = NewInvoice {
        period: november,
        ..invoice(vec![b], "2026-001")
    };
    assert!(matches!(
        invoices::mark_invoiced(&mut conn, &clock, STAFF, &next).await,
        Err(Error::DuplicateInvoiceNumber)
    ));
    assert!(
        invoices::uninvoiced(&mut conn, c, october)
            .await
            .unwrap()
            .orders
            .is_empty()
    );
    let history = audit::history(&mut conn, "order", a.get()).await.unwrap();
    assert_eq!(history.last().unwrap().action, "invoiced");
    assert_eq!(history.last().unwrap().actor, "staff:1");

    // Locked: lines and order details cannot change; picking up still can.
    let line_change = sqlx::query("UPDATE order_line SET qty = 1 WHERE order_id = $1")
        .bind(a.get())
        .execute(&mut *conn)
        .await;
    assert!(line_change.is_err());
    let new_line = sqlx::query(
        "INSERT INTO order_line (order_id, product_id, qty, unit_price_cents, vat_rate) VALUES ($1, $2, 1, 1, 9)",
    )
    .bind(a.get())
    .bind(bread.get())
    .execute(&mut *conn)
    .await;
    assert!(new_line.is_err());
    let total_change = sqlx::query("UPDATE \"order\" SET total_cents = 1 WHERE id = $1")
        .bind(a.get())
        .execute(&mut *conn)
        .await;
    assert!(total_change.is_err());
    let cancel = sqlx::query(
        "UPDATE \"order\" SET status = 'cancelled', cancel_reason = 'staff' WHERE id = $1",
    )
    .bind(a.get())
    .execute(&mut *conn)
    .await;
    assert!(cancel.is_err());
    sqlx::query("UPDATE \"order\" SET status = 'picked_up' WHERE id = $1")
        .bind(a.get())
        .execute(&mut *conn)
        .await
        .unwrap();
    let delete = sqlx::query("DELETE FROM \"order\" WHERE id = $1")
        .bind(a.get())
        .execute(&mut *conn)
        .await;
    assert!(delete.is_err());
    let no_account = NewInvoice {
        customer_id: other,
        ..invoice(vec![foreign], "2026-009")
    };
    assert!(matches!(
        invoices::mark_invoiced(&mut conn, &clock, STAFF, &no_account).await,
        Err(Error::NotFound)
    ));
    let rewrite = sqlx::query("UPDATE manual_invoice SET total_cents = 0")
        .execute(&mut *conn)
        .await;
    assert!(rewrite.is_err());
    // Uninvoiced orders still change freely.
    sqlx::query("UPDATE order_line SET qty = 2 WHERE order_id = $1")
        .bind(b.get())
        .execute(&mut *conn)
        .await
        .unwrap();
}
