use domain::{StaffUserId, VatRate};
use i18n::Translator;
use orders::audit::{self, Actor};
use reports::invoices::{self, AccountDetails, Contact, NewBusinessAccount, NewInvoice};
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

fn details() -> AccountDetails {
    AccountDetails {
        company_name: "Café De Hoek".into(),
        kvk: "12345678".into(),
        btw_id: None,
        invoice_address: "Dorpsstraat 1, 8382 AA Frederiksoord".into(),
        invoice_email: "factuur@dehoek.example".into(),
        on_account_allowed: true,
    }
}

fn contact(email: &str) -> Contact {
    Contact {
        email: email.into(),
        name: "Inkoop De Hoek".into(),
    }
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn accounts_are_opened_listed_and_changed_with_an_audit_entry(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let opened = invoices::open_account(
        &mut conn,
        &clock,
        STAFF,
        &contact(" Inkoop@DeHoek.example "),
        &details(),
    )
    .await
    .unwrap();
    assert_eq!(opened.contact_email, "inkoop@dehoek.example");
    let (email, name): (String, String) =
        sqlx::query_as("SELECT email, name FROM customer WHERE id = $1")
            .bind(opened.customer_id.get())
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(
        (email.as_str(), name.as_str()),
        ("inkoop@dehoek.example", "Inkoop De Hoek")
    );
    assert!(matches!(
        invoices::open_account(
            &mut conn,
            &clock,
            STAFF,
            &contact("inkoop@dehoek.example"),
            &details()
        )
        .await,
        Err(Error::Invalid(_))
    ));
    for bad in [
        contact("geen-mail"),
        Contact {
            name: " ".into(),
            ..contact("a@b.example")
        },
    ] {
        assert!(matches!(
            invoices::open_account(&mut conn, &clock, STAFF, &bad, &details()).await,
            Err(Error::Invalid(_))
        ));
    }

    let existing = customer(&mut conn, "bestaand@example.org").await;
    let linked = invoices::open_account(
        &mut conn,
        &clock,
        STAFF,
        &contact("bestaand@example.org"),
        &AccountDetails {
            company_name: "Atelier Aan Zee".into(),
            ..details()
        },
    )
    .await
    .unwrap();
    assert_eq!(linked.customer_id, existing);
    let kept: String = sqlx::query_scalar("SELECT name FROM customer WHERE id = $1")
        .bind(existing.get())
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(kept, "Zaak");

    let names: Vec<_> = invoices::list_accounts(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.company_name)
        .collect();
    assert_eq!(names, ["Atelier Aan Zee", "Café De Hoek"]);

    assert!(matches!(
        invoices::update_account(
            &mut conn,
            &clock,
            STAFF,
            opened.id,
            &AccountDetails {
                kvk: "123".into(),
                ..details()
            }
        )
        .await,
        Err(Error::Invalid(_))
    ));
    let changed = invoices::update_account(
        &mut conn,
        &clock,
        STAFF,
        opened.id,
        &AccountDetails {
            btw_id: Some("NL123456789B01".into()),
            on_account_allowed: false,
            ..details()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        invoices::account_by_id(&mut conn, opened.id).await.unwrap(),
        changed
    );
    assert!(!changed.on_account_allowed);
    let history = audit::history(&mut conn, "business_account", opened.id.get())
        .await
        .unwrap();
    let actions: Vec<_> = history.iter().map(|h| h.action.as_str()).collect();
    assert_eq!(actions, ["created", "changed"]);
    assert_eq!(history[1].actor, "staff:1");
    assert!(matches!(
        invoices::account_by_id(&mut conn, domain::BusinessAccountId::new(999)).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn totals_per_rate_add_up_the_lines_of_drops_in_the_period(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-20T10:00:00Z"));
    let first = drop_on(&mut conn, &clock, "2026-10-24", None).await;
    let last = drop_on(&mut conn, &clock, "2026-10-31", None).await;
    let next = drop_on(&mut conn, &clock, "2026-11-07", None).await;
    let bread = product(&mut conn, &BREAD).await;
    let cookies = product(&mut conn, &COOKIES).await;
    for shop in [&first, &last, &next] {
        sell(&mut conn, shop, &[bread, cookies]).await;
    }
    let c = customer(&mut conn, "zaak@example.org").await;
    invoices::create_account(&mut conn, &account(c))
        .await
        .unwrap();
    let lines = [
        [(bread, 1, 333, 9), (cookies, 7, 199, 21)],
        [(bread, 2, 101, 9), (cookies, 1, 1, 21)],
        [(bread, 5, 457, 9), (cookies, 3, 289, 21)],
    ];
    let a = on_account_order(&mut conn, c, &first, &lines[0], "2026-09-30T08:00:00Z").await;
    // Placed on 1 November in Amsterdam, but delivered on the 31 October drop.
    let b = on_account_order(&mut conn, c, &last, &lines[1], "2026-10-31T23:30:00Z").await;
    on_account_order(&mut conn, c, &next, &lines[2], "2026-10-21T08:00:00Z").await;

    let october = Period::new(date("2026-10-01"), date("2026-10-31")).unwrap();
    let overview = invoices::uninvoiced(&mut conn, c, october).await.unwrap();
    let ids: Vec<_> = overview.orders.iter().map(|o| o.id).collect();
    assert_eq!(ids, [a, b]);

    let all: Vec<domain::Line> = lines[..2]
        .iter()
        .flatten()
        .map(|&(_, qty, cents, vat)| domain::Line {
            unit_price: domain::Money::from_cents(cents),
            qty,
            vat_rate: VatRate::try_from(vat).unwrap(),
        })
        .collect();
    assert_eq!(
        overview.totals,
        domain::VatTotals::from_lines(&all).unwrap()
    );
    for rate in VatRate::ALL {
        let shown = overview.totals.rate(rate);
        let (mut net, mut vat) = (0, 0);
        for order in &overview.orders {
            for line in order.lines.iter().filter(|l| l.vat_rate == rate) {
                net += line.net.cents();
                vat += line.vat.cents();
                assert_eq!(line.net.cents() + line.vat.cents(), line.gross.cents());
            }
        }
        assert_eq!((shown.net().cents(), shown.vat().cents()), (net, vat));
    }
    assert_eq!(
        overview.totals.gross().unwrap().cents(),
        333 + 7 * 199 + 202 + 1
    );

    let invoice = NewInvoice {
        customer_id: c,
        orders: ids,
        invoice_number: "2026-010".into(),
        invoice_date: date("2026-11-02"),
        period: october,
    };
    invoices::mark_invoiced(&mut conn, &clock, STAFF, &invoice)
        .await
        .unwrap();
    let made = invoices::invoices_of(&mut conn, c).await.unwrap();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].invoice_number, "2026-010");
    assert_eq!(made[0].total, overview.totals.gross().unwrap());
    assert_eq!(made[0].orders, 2);
}
