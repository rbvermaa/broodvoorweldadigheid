//! The B2B invoice overview on paper (F11a): the data its Typst template reads, every text
//! already translated and formatted. The `production` crate lays it out with the other
//! PDFs (ADR 0034).

use domain::{Money, VatRate};
use i18n::{FluentArgs, Translator, money, numeric_date};
use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::Error;
use crate::invoices::InvoiceOverview;

/// The bakery's details from the company settings; `None` until the owner fills them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seller {
    pub name: String,
    pub address: Option<String>,
    pub kvk: Option<String>,
    pub btw_id: Option<String>,
}

/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn seller(conn: &mut PgConnection) -> Result<Seller, Error> {
    let row = sqlx::query!("SELECT name, address, kvk, btw_id FROM company")
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| Error::Corrupt("no company row".to_owned()))?;
    let filled = |value: Option<String>| value.filter(|v| !v.trim().is_empty());
    Ok(Seller {
        name: row.name,
        address: filled(row.address),
        kvk: filled(row.kvk),
        btw_id: filled(row.btw_id),
    })
}

fn fact(t: &Translator, label: &str, value: &str) -> Result<String, Error> {
    let mut args = FluentArgs::new();
    args.set("label", t.t(label, None)?);
    args.set("value", value);
    Ok(t.t("b2b-invoice-pdf-fact", Some(&args))?)
}

fn rate(rate: VatRate) -> String {
    format!("{}%", rate.percent())
}

fn amount(value: Money) -> String {
    money(value.cents())
}

/// # Errors
/// [`Error::I18n`] for a missing text, [`Error::Money`] when a total does not fit.
pub fn paper(overview: &InvoiceOverview, seller: &Seller, t: &Translator) -> Result<Value, Error> {
    let account = &overview.account;
    let mut from = vec![seller.name.clone()];
    from.extend(seller.address.clone());
    if let Some(kvk) = &seller.kvk {
        from.push(fact(t, "b2b-kvk", kvk)?);
    }
    if let Some(btw_id) = &seller.btw_id {
        from.push(fact(t, "b2b-btw-id", btw_id)?);
    }
    let mut to = vec![
        account.company_name.clone(),
        account.invoice_address.clone(),
        fact(t, "b2b-kvk", &account.kvk)?,
    ];
    if let Some(btw_id) = &account.btw_id {
        to.push(fact(t, "b2b-btw-id", btw_id)?);
    }
    to.push(account.invoice_email.clone());

    let mut heading = FluentArgs::new();
    heading.set("company", account.company_name.as_str());
    let mut period = FluentArgs::new();
    period.set("from", numeric_date(overview.period.from()));
    period.set("to", numeric_date(overview.period.to()));

    let lines: Vec<Vec<String>> = overview
        .orders
        .iter()
        .flat_map(|order| {
            order.lines.iter().map(|line| {
                vec![
                    order.number.to_string(),
                    numeric_date(order.drop_date),
                    line.product_name.clone(),
                    line.qty.to_string(),
                    amount(line.unit_price_excl),
                    rate(line.vat_rate),
                    amount(line.net),
                    amount(line.vat),
                    amount(line.gross),
                ]
            })
        })
        .collect();
    let mut net = Money::from_cents(0);
    let mut vat = Money::from_cents(0);
    let mut totals = Vec::new();
    for vat_rate in VatRate::ALL {
        let sum = overview.totals.rate(vat_rate);
        net = net.checked_add(sum.net())?;
        vat = vat.checked_add(sum.vat())?;
        totals.push(vec![
            rate(vat_rate),
            amount(sum.net()),
            amount(sum.vat()),
            amount(sum.gross()),
        ]);
    }
    let column = |id: &str| t.t(id, None);
    Ok(json!({
        "title": t.t("b2b-invoice-heading", Some(&heading))?,
        "period": fact(t, "b2b-col-period", &t.t("b2b-period", Some(&period))?)?,
        "intro": t.t("b2b-invoice-intro", None)?,
        "seller": { "heading": t.t("b2b-invoice-pdf-from", None)?, "lines": from },
        "customer": { "heading": t.t("b2b-invoice-pdf-to", None)?, "lines": to },
        "labels": {
            "orders": t.t("b2b-invoice-orders", None)?,
            "totals": t.t("b2b-invoice-totals", None)?,
            "empty": t.t("b2b-invoice-none", None)?,
            "columns": [
                column("report-col-order")?,
                column("report-col-drop-date")?,
                column("report-col-product")?,
                column("report-col-pieces")?,
                column("report-col-unit-price-excl")?,
                column("report-col-vat-rate")?,
                column("report-col-net")?,
                column("report-col-vat")?,
                column("report-col-gross")?,
            ],
            "total_columns": [
                column("report-col-vat-rate")?,
                column("report-col-net")?,
                column("report-col-vat")?,
                column("report-col-gross")?,
            ],
        },
        "lines": lines,
        "totals": totals,
        "total": [
            t.t("report-total", None)?,
            amount(net),
            amount(vat),
            amount(overview.totals.gross()?),
        ],
    }))
}

/// The overview as an A4 PDF; the same overview gives the same bytes.
///
/// # Errors
/// See [`paper`]; [`Error::Pdf`] when Typst fails.
pub fn pdf(overview: &InvoiceOverview, seller: &Seller, t: &Translator) -> Result<Vec<u8>, Error> {
    let data = paper(overview, seller, t)?;
    let ident = format!(
        "invoice-overview-{}-{}-{}",
        overview.account.id.get(),
        overview.period.from(),
        overview.period.to()
    );
    Ok(production::invoice_overview_pdf(&data, ident)?)
}

#[cfg(test)]
mod tests {
    use domain::{BusinessAccountId, CustomerId, Line, OrderId, VatTotals};
    use orders::OrderNumber;

    use super::*;
    use crate::Period;
    use crate::invoices::{BusinessAccount, InvoiceLine, InvoiceOrder};

    fn line(product: &str, qty: u32, cents: i64, vat_rate: VatRate) -> (InvoiceLine, Line) {
        let unit_price = Money::from_cents(cents);
        let line = Line {
            unit_price,
            qty,
            vat_rate,
        };
        let (net, vat) = line.split().unwrap();
        let shown = InvoiceLine {
            product_name: product.to_owned(),
            qty,
            unit_price_excl: vat_rate.net_part(unit_price),
            vat_rate,
            net,
            vat,
            gross: line.total().unwrap(),
        };
        (shown, line)
    }

    fn order(seq: i32, drop: &str, lines: Vec<(InvoiceLine, Line)>) -> InvoiceOrder {
        let (shown, raw): (Vec<_>, Vec<_>) = lines.into_iter().unzip();
        InvoiceOrder {
            id: OrderId::new(i64::from(seq)),
            number: OrderNumber { year: 2026, seq },
            drop_date: drop.parse().unwrap(),
            lines: shown,
            totals: VatTotals::from_lines(&raw).unwrap(),
        }
    }

    fn overview() -> InvoiceOverview {
        let orders = vec![
            order(
                4,
                "2026-12-05",
                vec![
                    line("Desem", 10, 450, VatRate::Nine),
                    line("Koekjes", 2, 300, VatRate::Nine),
                ],
            ),
            order(
                9,
                "2026-12-12",
                vec![line("Bezorging", 1, 500, VatRate::TwentyOne)],
            ),
        ];
        let totals = orders
            .iter()
            .try_fold(VatTotals::default(), |sum, o| sum.checked_add(&o.totals))
            .unwrap();
        InvoiceOverview {
            account: BusinessAccount {
                id: BusinessAccountId::new(3),
                customer_id: CustomerId::new(7),
                contact_email: "inkoop@example.nl".to_owned(),
                company_name: "Café Test BV".to_owned(),
                kvk: "87654321".to_owned(),
                btw_id: Some("NL000000000B01".to_owned()),
                invoice_address: "Teststraat 1, 1234 AB Testdorp".to_owned(),
                invoice_email: "factuur@example.nl".to_owned(),
                on_account_allowed: true,
            },
            period: Period::new("2026-12-01".parse().unwrap(), "2026-12-31".parse().unwrap())
                .unwrap(),
            orders,
            totals,
        }
    }

    fn seller() -> Seller {
        Seller {
            name: "Brood voor Weldadigheid".to_owned(),
            address: Some("Majoor van Swietenlaan 15, 8382 CE Frederiksoord".to_owned()),
            kvk: Some("12345678".to_owned()),
            btw_id: Some("NL000000000B02".to_owned()),
        }
    }

    fn dutch() -> Translator {
        Translator::dutch().unwrap()
    }

    #[test]
    fn invoice_overview_input() {
        let data = paper(&overview(), &seller(), &dutch()).unwrap();
        insta::assert_snapshot!(serde_json::to_string_pretty(&data).unwrap());
    }

    #[test]
    fn invoice_overview_on_paper() {
        let data = paper(&overview(), &seller(), &dutch()).unwrap();
        let (text, warnings) = production::invoice_overview_text(&data).unwrap();
        assert_eq!(warnings, Vec::<String>::new());
        assert!(
            text.contains("Bestelling | Bakdag | Product | Aantal | Prijs per stuk excl. BTW"),
            "every column, product included, fits on one line: {text}"
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn an_empty_overview_says_so() {
        let mut empty = overview();
        empty.orders.clear();
        empty.totals = VatTotals::default();
        let data = paper(&empty, &seller(), &dutch()).unwrap();
        let (text, _) = production::invoice_overview_text(&data).unwrap();
        assert!(text.contains(&dutch().t("b2b-invoice-none", None).unwrap()));
        assert!(!text.contains("Desem"), "{text}");
    }

    #[test]
    fn company_details_not_filled_in_are_left_out() {
        let bare = Seller {
            address: None,
            kvk: None,
            btw_id: None,
            ..seller()
        };
        let mut account = overview();
        account.account.btw_id = None;
        let data = paper(&account, &bare, &dutch()).unwrap();
        assert_eq!(data["seller"]["lines"], json!(["Brood voor Weldadigheid"]));
        let customer = data["customer"]["lines"].to_string();
        assert!(customer.contains("KvK-nummer: 87654321"), "{customer}");
        assert!(!customer.contains("BTW-id"), "{customer}");
    }

    #[test]
    fn the_same_overview_gives_the_same_pdf() {
        let t = dutch();
        let first = pdf(&overview(), &seller(), &t).unwrap();
        assert!(first.starts_with(b"%PDF-"));
        assert_eq!(first, pdf(&overview(), &seller(), &t).unwrap());
    }
}
