//! Business accounts and the overview of on-account orders to invoice by hand (F11a).

use db::time::{from_epoch_days, to_epoch_days, to_micros};
use domain::{BusinessAccountId, Clock, CustomerId, Line, Money, OrderId, VatRate, VatTotals};
use i18n::Translator;
use jiff::civil::Date;
use orders::OrderNumber;
use orders::audit::{self, Actor, Entry};
use sqlx::{Acquire, PgConnection};

use crate::csv_out::{self, Csv};
use crate::{Error, Period};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBusinessAccount {
    pub customer_id: CustomerId,
    pub company_name: String,
    /// Eight digits.
    pub kvk: String,
    pub btw_id: Option<String>,
    pub invoice_address: String,
    pub invoice_email: String,
    pub on_account_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessAccount {
    pub id: BusinessAccountId,
    pub customer_id: CustomerId,
    /// The customer's email, which orders on account are placed under.
    pub contact_email: String,
    pub company_name: String,
    pub kvk: String,
    pub btw_id: Option<String>,
    pub invoice_address: String,
    pub invoice_email: String,
    pub on_account_allowed: bool,
}

fn required(text: &str, what: &'static str) -> Result<String, Error> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Invalid(what));
    }
    Ok(text.to_owned())
}

/// Creates the business account of a customer; a customer has at most one.
///
/// # Errors
/// [`Error::Invalid`] for an empty field, a KvK number that is not eight digits, or a
/// customer that already has an account; [`Error::Db`].
pub async fn create_account(
    conn: &mut PgConnection,
    input: &NewBusinessAccount,
) -> Result<BusinessAccount, Error> {
    let details = AccountDetails {
        company_name: input.company_name.clone(),
        kvk: input.kvk.clone(),
        btw_id: input.btw_id.clone(),
        invoice_address: input.invoice_address.clone(),
        invoice_email: input.invoice_email.clone(),
        on_account_allowed: input.on_account_allowed,
    }
    .checked()?;
    sqlx::query_scalar!(
        "INSERT INTO business_account (customer_id, company_name, kvk, btw_id, invoice_address,
            invoice_email, on_account_allowed)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (customer_id) DO NOTHING RETURNING id",
        input.customer_id.get(),
        details.company_name,
        details.kvk,
        details.btw_id,
        details.invoice_address,
        details.invoice_email,
        details.on_account_allowed,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::Invalid("customer already has a business account"))?;
    account(conn, input.customer_id).await
}

/// # Errors
/// [`Error::NotFound`] when the customer has no business account; [`Error::Db`].
pub async fn account(
    conn: &mut PgConnection,
    customer: CustomerId,
) -> Result<BusinessAccount, Error> {
    let row = sqlx::query!(
        "SELECT a.id, a.customer_id, c.email, a.company_name, a.kvk, a.btw_id,
            a.invoice_address, a.invoice_email, a.on_account_allowed
        FROM business_account a JOIN customer c ON c.id = a.customer_id
        WHERE a.customer_id = $1",
        customer.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(BusinessAccount {
        id: BusinessAccountId::new(row.id),
        customer_id: CustomerId::new(row.customer_id),
        contact_email: row.email,
        company_name: row.company_name,
        kvk: row.kvk,
        btw_id: row.btw_id,
        invoice_address: row.invoice_address,
        invoice_email: row.invoice_email,
        on_account_allowed: row.on_account_allowed,
    })
}

/// What the owner fills in for a business account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDetails {
    pub company_name: String,
    /// Eight digits.
    pub kvk: String,
    pub btw_id: Option<String>,
    pub invoice_address: String,
    pub invoice_email: String,
    pub on_account_allowed: bool,
}

/// The person who orders for the business; they become the account's customer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub email: String,
    pub name: String,
}

impl AccountDetails {
    fn checked(&self) -> Result<Self, Error> {
        let kvk = required(&self.kvk, "KvK number")?;
        if kvk.len() != 8 || !kvk.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Invalid("KvK number"));
        }
        Ok(Self {
            company_name: required(&self.company_name, "company name")?,
            kvk,
            btw_id: self
                .btw_id
                .as_deref()
                .map(|id| required(id, "BTW-id"))
                .transpose()?,
            invoice_address: required(&self.invoice_address, "invoice address")?,
            invoice_email: email(&self.invoice_email, "invoice email")?,
            on_account_allowed: self.on_account_allowed,
        })
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "company_name": self.company_name,
            "kvk": self.kvk,
            "btw_id": self.btw_id,
            "invoice_address": self.invoice_address,
            "invoice_email": self.invoice_email,
            "on_account_allowed": self.on_account_allowed,
        })
    }
}

impl BusinessAccount {
    fn details(&self) -> AccountDetails {
        AccountDetails {
            company_name: self.company_name.clone(),
            kvk: self.kvk.clone(),
            btw_id: self.btw_id.clone(),
            invoice_address: self.invoice_address.clone(),
            invoice_email: self.invoice_email.clone(),
            on_account_allowed: self.on_account_allowed,
        }
    }
}

fn email(text: &str, what: &'static str) -> Result<String, Error> {
    let email = required(text, what)?.to_lowercase();
    if !email.contains('@') || email.chars().any(char::is_whitespace) {
        return Err(Error::Invalid(what));
    }
    Ok(email)
}

/// Opens a business account for the contact's customer, created when the email is new.
/// An existing customer is linked as they are, never changed. Writes an audit entry.
///
/// # Errors
/// [`Error::Invalid`] for a wrong field or a customer that already has an account;
/// [`Error::Db`].
pub async fn open_account(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    actor: Actor,
    contact: &Contact,
    details: &AccountDetails,
) -> Result<BusinessAccount, Error> {
    let contact_email = email(&contact.email, "contact email")?;
    let contact_name = required(&contact.name, "contact name")?;
    let details = details.checked()?;
    let now = clock.now();
    let mut tx = conn.begin().await?;
    let created = sqlx::query_scalar!(
        "INSERT INTO customer (email, name, created_at)
        VALUES ($1, $2, timestamptz 'epoch' + $3::bigint * interval '1 microsecond')
        ON CONFLICT (email) DO NOTHING RETURNING id",
        contact_email,
        contact_name,
        to_micros(now),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let customer_id = match created {
        Some(id) => id,
        None => {
            sqlx::query_scalar!("SELECT id FROM customer WHERE email = $1", contact_email)
                .fetch_one(&mut *tx)
                .await?
        }
    };
    let account = create_account(
        &mut tx,
        &NewBusinessAccount {
            customer_id: CustomerId::new(customer_id),
            company_name: details.company_name.clone(),
            kvk: details.kvk.clone(),
            btw_id: details.btw_id.clone(),
            invoice_address: details.invoice_address.clone(),
            invoice_email: details.invoice_email.clone(),
            on_account_allowed: details.on_account_allowed,
        },
    )
    .await?;
    let mut after = details.snapshot();
    after["customer_id"] = customer_id.into();
    audit::record(
        &mut tx,
        &Entry {
            entity: "business_account",
            entity_id: account.id.get(),
            action: "created",
            before: None,
            after: Some(after),
            actor,
            at: now,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(account)
}

/// Changes an account's details, with an audit entry of before and after. Orders already
/// placed keep their name; invoices already made are not affected.
///
/// # Errors
/// [`Error::Invalid`] for a wrong field; [`Error::NotFound`]; [`Error::Db`].
pub async fn update_account(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    actor: Actor,
    id: BusinessAccountId,
    details: &AccountDetails,
) -> Result<BusinessAccount, Error> {
    let details = details.checked()?;
    let mut tx = conn.begin().await?;
    let before = sqlx::query_scalar!(
        "SELECT customer_id FROM business_account WHERE id = $1 FOR UPDATE",
        id.get(),
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    let before = account(&mut tx, CustomerId::new(before)).await?;
    sqlx::query!(
        "UPDATE business_account SET company_name = $2, kvk = $3, btw_id = $4,
            invoice_address = $5, invoice_email = $6, on_account_allowed = $7
        WHERE id = $1",
        id.get(),
        details.company_name,
        details.kvk,
        details.btw_id,
        details.invoice_address,
        details.invoice_email,
        details.on_account_allowed,
    )
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        &Entry {
            entity: "business_account",
            entity_id: id.get(),
            action: "changed",
            before: Some(before.details().snapshot()),
            after: Some(details.snapshot()),
            actor,
            at: clock.now(),
        },
    )
    .await?;
    let changed = account(&mut tx, before.customer_id).await?;
    tx.commit().await?;
    Ok(changed)
}

/// # Errors
/// [`Error::NotFound`]; [`Error::Db`].
pub async fn account_by_id(
    conn: &mut PgConnection,
    id: BusinessAccountId,
) -> Result<BusinessAccount, Error> {
    let customer = sqlx::query_scalar!(
        "SELECT customer_id FROM business_account WHERE id = $1",
        id.get(),
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::NotFound)?;
    account(conn, CustomerId::new(customer)).await
}

/// Every business account, by company name.
///
/// # Errors
/// [`Error::Db`].
pub async fn list_accounts(conn: &mut PgConnection) -> Result<Vec<BusinessAccount>, Error> {
    Ok(sqlx::query!(
        "SELECT a.id, a.customer_id, c.email, a.company_name, a.kvk, a.btw_id,
            a.invoice_address, a.invoice_email, a.on_account_allowed
        FROM business_account a JOIN customer c ON c.id = a.customer_id
        ORDER BY lower(a.company_name), a.id",
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| BusinessAccount {
        id: BusinessAccountId::new(row.id),
        customer_id: CustomerId::new(row.customer_id),
        contact_email: row.email,
        company_name: row.company_name,
        kvk: row.kvk,
        btw_id: row.btw_id,
        invoice_address: row.invoice_address,
        invoice_email: row.invoice_email,
        on_account_allowed: row.on_account_allowed,
    })
    .collect())
}

/// An invoice recorded for a customer, with the number of orders it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedInvoice {
    pub invoice_number: String,
    pub invoice_date: Date,
    pub period: Period,
    pub total: Money,
    pub orders: u32,
}

/// The customer's recorded invoices, newest first.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn invoices_of(
    conn: &mut PgConnection,
    customer: CustomerId,
) -> Result<Vec<RecordedInvoice>, Error> {
    sqlx::query!(
        r#"SELECT i.invoice_number, (i.invoice_date - date 'epoch') AS "invoice_day!",
            (i.period_from - date 'epoch') AS "from_day!",
            (i.period_to - date 'epoch') AS "to_day!", i.total_cents,
            (SELECT count(*) FROM "order" o WHERE o.manual_invoice_id = i.id) AS "orders!"
        FROM manual_invoice i WHERE i.customer_id = $1
        ORDER BY i.invoice_date DESC, i.id DESC"#,
        customer.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(RecordedInvoice {
            invoice_number: row.invoice_number,
            invoice_date: from_epoch_days(row.invoice_day)?,
            period: Period::new(from_epoch_days(row.from_day)?, from_epoch_days(row.to_day)?)?,
            total: Money::from_cents(row.total_cents),
            orders: u32::try_from(row.orders)
                .map_err(|_| Error::Corrupt("invoice order count".into()))?,
        })
    })
    .collect()
}

/// One order line with its price excluding BTW. `net` and `vat` split the line total
/// (ADR 0005); `unit_price_excl` is the unit price split on its own, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceLine {
    pub product_name: String,
    pub qty: u32,
    pub unit_price_excl: Money,
    pub vat_rate: VatRate,
    pub net: Money,
    pub vat: Money,
    pub gross: Money,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceOrder {
    pub id: OrderId,
    pub number: OrderNumber,
    /// The drop the order was picked up from.
    pub drop_date: Date,
    pub lines: Vec<InvoiceLine>,
    pub totals: VatTotals,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceOverview {
    pub account: BusinessAccount,
    pub period: Period,
    pub orders: Vec<InvoiceOrder>,
    pub totals: VatTotals,
}

/// Every uninvoiced on-account order of the customer on drops dated in `period`
/// (confirmed or picked up), with prices excluding BTW and totals per rate.
///
/// # Errors
/// [`Error::NotFound`] when the customer has no business account; [`Error::Db`],
/// [`Error::Corrupt`] or [`Error::Money`].
pub async fn uninvoiced(
    conn: &mut PgConnection,
    customer: CustomerId,
    period: Period,
) -> Result<InvoiceOverview, Error> {
    let account = account(conn, customer).await?;
    let rows = sqlx::query!(
        r#"SELECT o.id, o.number_year, o.number_seq, (bd.date - date 'epoch') AS "drop_day!",
            p.name_nl, ol.qty, ol.unit_price_cents, ol.vat_rate
        FROM "order" o
        JOIN bake_day bd ON bd.id = o.bake_day_id
        JOIN order_line ol ON ol.order_id = o.id
        JOIN product p ON p.id = ol.product_id
        WHERE o.customer_id = $1 AND o.payment_mode = 'on_account'
            AND o.manual_invoice_id IS NULL AND o.status IN ('confirmed', 'picked_up')
            AND bd.date BETWEEN date 'epoch' + $2::integer AND date 'epoch' + $3::integer
        ORDER BY bd.date, o.number_year, o.number_seq, ol.id"#,
        customer.get(),
        to_epoch_days(period.from()),
        to_epoch_days(period.to()),
    )
    .fetch_all(conn)
    .await?;
    let mut orders: Vec<(InvoiceOrder, Vec<Line>)> = Vec::new();
    for row in rows {
        let vat_rate = VatRate::try_from(row.vat_rate)?;
        let qty = u32::try_from(row.qty).map_err(|_| Error::Corrupt("line quantity".into()))?;
        let unit_price = Money::from_cents(row.unit_price_cents);
        let line = Line {
            unit_price,
            qty,
            vat_rate,
        };
        let (net, vat) = line.split()?;
        let invoice_line = InvoiceLine {
            product_name: row.name_nl,
            qty,
            unit_price_excl: vat_rate.net_part(unit_price),
            vat_rate,
            net,
            vat,
            gross: line.total()?,
        };
        match orders.last_mut() {
            Some((order, lines)) if order.id.get() == row.id => {
                order.lines.push(invoice_line);
                lines.push(line);
            }
            _ => orders.push((
                InvoiceOrder {
                    id: OrderId::new(row.id),
                    number: OrderNumber {
                        year: row.number_year,
                        seq: row.number_seq,
                    },
                    drop_date: from_epoch_days(row.drop_day)?,
                    lines: vec![invoice_line],
                    totals: VatTotals::default(),
                },
                vec![line],
            )),
        }
    }
    let mut totals = VatTotals::default();
    let orders = orders
        .into_iter()
        .map(|(mut order, lines)| {
            order.totals = VatTotals::from_lines(&lines)?;
            totals = totals.checked_add(&order.totals)?;
            Ok(order)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(InvoiceOverview {
        account,
        period,
        orders,
        totals,
    })
}

/// The hand-made invoice that orders are marked with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewInvoice {
    pub customer_id: CustomerId,
    pub orders: Vec<OrderId>,
    pub invoice_number: String,
    pub invoice_date: Date,
    pub period: Period,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualInvoice {
    pub id: i64,
    pub customer_id: CustomerId,
    pub invoice_number: String,
    pub invoice_date: Date,
    pub period: Period,
    pub total: Money,
}

/// Links the orders to a hand-made invoice, which locks them: an invoiced order never
/// changes again, except being picked up (ADR 0017). All orders or none.
///
/// # Errors
/// [`Error::Invalid`] for no orders or an empty number; [`Error::DuplicateInvoiceNumber`];
/// [`Error::AlreadyInvoiced`]; [`Error::NotInvoiceable`] for an order that is not a
/// confirmed or picked-up on-account order of this customer on a drop in the period;
/// [`Error::Db`].
pub async fn mark_invoiced(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    actor: Actor,
    invoice: &NewInvoice,
) -> Result<ManualInvoice, Error> {
    let number = required(&invoice.invoice_number, "invoice number")?;
    let mut ids: Vec<i64> = invoice.orders.iter().map(|o| o.get()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Err(Error::Invalid("orders"));
    }
    let mut tx = conn.begin().await?;
    account(&mut tx, invoice.customer_id).await?;
    let rows = sqlx::query!(
        r#"SELECT o.id, o.customer_id, o.payment_mode, o.status, o.manual_invoice_id,
            o.total_cents, (bd.date - date 'epoch') AS "drop_day!"
        FROM "order" o JOIN bake_day bd ON bd.id = o.bake_day_id
        WHERE o.id = ANY($1) ORDER BY o.id FOR UPDATE OF o"#,
        &ids,
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in &ids {
        let id = OrderId::new(*id);
        let row = rows
            .iter()
            .find(|r| r.id == id.get())
            .ok_or(Error::NotInvoiceable(id))?;
        if row.manual_invoice_id.is_some() {
            return Err(Error::AlreadyInvoiced(id));
        }
        let drop_date = from_epoch_days(row.drop_day)?;
        if row.customer_id != invoice.customer_id.get()
            || row.payment_mode != "on_account"
            || !matches!(row.status.as_str(), "confirmed" | "picked_up")
            || !invoice.period.contains(drop_date)
        {
            return Err(Error::NotInvoiceable(id));
        }
    }
    let total = rows.iter().try_fold(Money::ZERO, |sum, r| {
        sum.checked_add(Money::from_cents(r.total_cents))
    })?;
    let now = clock.now();
    let id = sqlx::query_scalar!(
        "INSERT INTO manual_invoice (customer_id, invoice_number, invoice_date, period_from,
            period_to, total_cents, recorded_at, recorded_by)
        VALUES ($1, $2, date 'epoch' + $3::integer, date 'epoch' + $4::integer,
            date 'epoch' + $5::integer, $6,
            timestamptz 'epoch' + $7::bigint * interval '1 microsecond', $8)
        RETURNING id",
        invoice.customer_id.get(),
        number,
        to_epoch_days(invoice.invoice_date),
        to_epoch_days(invoice.period.from()),
        to_epoch_days(invoice.period.to()),
        total.cents(),
        to_micros(now),
        actor.to_db(),
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| match &err {
        sqlx::Error::Database(db)
            if db.constraint() == Some("manual_invoice_invoice_number_key") =>
        {
            Error::DuplicateInvoiceNumber
        }
        _ => Error::Db(err),
    })?;
    sqlx::query!(
        r#"UPDATE "order" SET manual_invoice_id = $1 WHERE id = ANY($2)"#,
        id,
        &ids,
    )
    .execute(&mut *tx)
    .await?;
    for order in &ids {
        audit::record(
            &mut tx,
            &Entry {
                entity: "order",
                entity_id: *order,
                action: "invoiced",
                before: None,
                after: Some(serde_json::json!({
                    "manual_invoice_id": id,
                    "invoice_number": number,
                })),
                actor,
                at: now,
            },
        )
        .await?;
    }
    audit::record(
        &mut tx,
        &Entry {
            entity: "manual_invoice",
            entity_id: id,
            action: "recorded",
            before: None,
            after: Some(serde_json::json!({
                "customer_id": invoice.customer_id.get(),
                "invoice_number": number,
                "invoice_date": invoice.invoice_date.to_string(),
                "orders": ids,
                "total_cents": total.cents(),
            })),
            actor,
            at: now,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(ManualInvoice {
        id,
        customer_id: invoice.customer_id,
        invoice_number: number,
        invoice_date: invoice.invoice_date,
        period: invoice.period,
        total,
    })
}

/// One row per order line, then the totals per BTW rate.
///
/// # Errors
/// [`Error::I18n`] for a missing column name, [`Error::Csv`].
pub fn csv(overview: &InvoiceOverview, t: &Translator) -> Result<String, Error> {
    let mut out = Csv::new(
        t,
        &[
            "report-col-order",
            "report-col-drop-date",
            "report-col-product",
            "report-col-pieces",
            "report-col-unit-price-excl",
            "report-col-vat-rate",
            "report-col-net",
            "report-col-vat",
            "report-col-gross",
        ],
    )?;
    for order in &overview.orders {
        for line in &order.lines {
            out.row(&[
                order.number.to_string(),
                csv_out::date(order.drop_date),
                line.product_name.clone(),
                line.qty.to_string(),
                csv_out::amount(line.unit_price_excl),
                csv_out::rate(line.vat_rate),
                csv_out::amount(line.net),
                csv_out::amount(line.vat),
                csv_out::amount(line.gross),
            ])?;
        }
    }
    let total = t.t("report-total", None)?;
    for rate in VatRate::ALL {
        let sum = overview.totals.rate(rate);
        out.row(&[
            total.clone(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            csv_out::rate(rate),
            csv_out::amount(sum.net()),
            csv_out::amount(sum.vat()),
            csv_out::amount(sum.gross()),
        ])?;
    }
    out.finish()
}
