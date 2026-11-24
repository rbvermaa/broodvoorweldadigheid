//! The customer emails about an order (F9): queued in the outbox in the caller's
//! transaction, sent later by `notify::send_pending`. See ADR 0022.

use std::sync::Arc;

use domain::{Clock, Money, OrderId, bakery_time_zone};
use i18n::Translator;
use jiff::civil::{Time, time};
use jiff::{SignedDuration, Timestamp};
use notify::{
    Company, ComposeError, Composer, Donation, Email, Item, OrderCancelled, OrderConfirmation,
    OutboxError, OutboxId, Pickup, PickupReminder, Recipient,
};
use orders::{OrderDetails, Payment};
use sqlx::{Acquire, PgConnection};

/// The local time on the day before pickup when the reminder goes out, unless configured.
pub const DEFAULT_REMINDER_AT: Time = time(18, 0, 0, 0);

/// Reminders are looked for among pickups that start at most this long from now.
const REMINDER_HORIZON: SignedDuration = SignedDuration::from_hours(72);

/// What the order emails need besides the order itself.
#[derive(Clone)]
pub struct OrderEmails {
    i18n: Arc<Translator>,
    public_url: String,
    reminder_at: Time,
}

impl OrderEmails {
    /// `public_url` is where customers reach the shop, without a trailing slash.
    #[must_use]
    pub fn new(i18n: Arc<Translator>, public_url: impl Into<String>) -> Self {
        Self {
            i18n,
            public_url: public_url.into(),
            reminder_at: DEFAULT_REMINDER_AT,
        }
    }

    /// The local time on the day before pickup when reminders go out.
    #[must_use]
    pub fn with_reminder_at(mut self, at: Time) -> Self {
        self.reminder_at = at;
        self
    }

    #[must_use]
    pub fn reminder_at(&self) -> Time {
        self.reminder_at
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error(transparent)]
    Orders(#[from] orders::Error),
    #[error(transparent)]
    Catalog(#[from] catalog::Error),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    #[error("cannot compose the email: {0}")]
    Compose(#[from] ComposeError),
    #[error("the company address and KvK number must be filled in before emails can go out")]
    CompanyIncomplete,
}

/// `order-confirmation:42`
#[must_use]
pub fn confirmation_key(order: OrderId) -> String {
    format!("order-confirmation:{}", order.get())
}

/// `pickup-reminder:42`
#[must_use]
pub fn reminder_key(order: OrderId) -> String {
    format!("pickup-reminder:{}", order.get())
}

/// `order-cancelled:42`
#[must_use]
pub fn cancellation_key(order: OrderId) -> String {
    format!("order-cancelled:{}", order.get())
}

/// `refund-due:7`, per payment: an order can have more than one refused payment.
#[must_use]
pub fn refund_key(payment: &Payment) -> String {
    format!("refund-due:{}", payment.id.get())
}

/// When the pickup reminder for a pickup starting at `pickup` is due: `at` local time on
/// the day before the pickup's local date.
#[must_use]
pub fn reminder_due(pickup: Timestamp, at: Time) -> Timestamp {
    let zone = bakery_time_zone();
    let day = pickup.to_zoned(zone.clone()).date();
    day.yesterday()
        .ok()
        .and_then(|before| before.to_datetime(at).to_zoned(zone).ok())
        .map_or(pickup, |due| due.timestamp())
}

async fn company(conn: &mut PgConnection) -> Result<Company, EmailError> {
    let contact = catalog::company::contact(conn).await?;
    match (contact.address, contact.kvk) {
        (Some(address), Some(kvk)) => Ok(Company {
            name: contact.name,
            address,
            kvk,
        }),
        _ => Err(EmailError::CompanyIncomplete),
    }
}

fn recipient(details: &OrderDetails) -> Recipient {
    Recipient {
        email: details.email.clone(),
        name: Some(details.order.customer_name.clone()),
    }
}

fn pickup(details: &OrderDetails) -> Pickup {
    Pickup {
        starts: details.pickup.starts,
        ends: details.pickup.ends,
        location: details.pickup.location.clone(),
        address: details.pickup.address.clone(),
    }
}

async fn compose(
    conn: &mut PgConnection,
    emails: &OrderEmails,
    order: OrderId,
    build: impl FnOnce(&Composer<'_>, &OrderDetails) -> Result<Email, ComposeError>,
) -> Result<Email, EmailError> {
    let company = company(conn).await?;
    let details = orders::details(conn, order).await?;
    let composer = Composer::new(&emails.i18n, &company);
    Ok(build(&composer, &details)?)
}

/// Queues the order confirmation: lines with BTW per rate, the charity or normal sale,
/// the ophaalmoment and a link to the status page. Call it in the transaction that marks
/// the order paid. Queued at most once per order.
///
/// # Errors
/// [`EmailError`].
pub async fn enqueue_confirmation(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    emails: &OrderEmails,
    order: OrderId,
) -> Result<Option<OutboxId>, EmailError> {
    let email = compose(conn, emails, order, |composer, details| {
        composer.order_confirmation(&OrderConfirmation {
            to: recipient(details),
            order_number: details.order.number.to_string(),
            drop_date: details.drop_date,
            pickup: pickup(details),
            lines: details
                .order
                .lines
                .iter()
                .zip(&details.products)
                .map(|(line, product)| notify::OrderLine {
                    product: product.clone(),
                    qty: line.qty,
                    unit_price: line.unit_price,
                    vat_rate: line.vat_rate,
                    pay_what_you_want: line.suggested_price.is_some(),
                })
                .collect(),
            donation: details
                .charity
                .clone()
                .map_or(Donation::NormalSale, |name| Donation::Charity { name }),
            status_url: format!("{}/bestelling/{}", emails.public_url, details.status_token),
        })
    })
    .await?;
    Ok(notify::enqueue(conn, clock, &email, Some(&confirmation_key(order))).await?)
}

async fn enqueue_cancelled(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    emails: &OrderEmails,
    order: OrderId,
    refund: Option<Money>,
    key: &str,
) -> Result<Option<OutboxId>, EmailError> {
    let email = compose(conn, emails, order, |composer, details| {
        composer.order_cancelled(&OrderCancelled {
            to: recipient(details),
            order_number: details.order.number.to_string(),
            drop_date: details.drop_date,
            refund,
        })
    })
    .await?;
    Ok(notify::enqueue(conn, clock, &email, Some(key)).await?)
}

/// Queues the cancellation email for an order staff cancel (F6), in the same transaction
/// as the cancel. `refund` is what goes back, `None` when nothing was paid. Queued at most
/// once per order.
///
/// # Errors
/// [`EmailError`].
pub async fn enqueue_cancellation(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    emails: &OrderEmails,
    order: OrderId,
    refund: Option<Money>,
) -> Result<Option<OutboxId>, EmailError> {
    let key = cancellation_key(order);
    enqueue_cancelled(conn, clock, emails, order, refund, &key).await
}

/// Queues the cancellation email for a payment that came in for a cancelled order, such
/// as one whose hold expired: the order is off and the payment goes back. Queued at most
/// once per payment.
///
/// # Errors
/// [`EmailError`].
pub async fn enqueue_refund_notice(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    emails: &OrderEmails,
    payment: &Payment,
) -> Result<Option<OutboxId>, EmailError> {
    let key = refund_key(payment);
    enqueue_cancelled(
        conn,
        clock,
        emails,
        payment.order_id,
        Some(payment.amount),
        &key,
    )
    .await
}

/// Queues the pickup reminder of every paid or confirmed order whose reminder is due
/// ([`reminder_due`]) and whose pickup has not started, once per order. Each order is
/// checked again under its bake day lock in the transaction that queues its reminder.
/// Returns the orders queued now. An order whose email cannot be composed is logged and
/// skipped.
///
/// # Errors
/// [`EmailError`] when the database fails, or [`EmailError::CompanyIncomplete`].
pub async fn enqueue_due_reminders(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    emails: &OrderEmails,
) -> Result<Vec<OrderId>, EmailError> {
    let now = clock.now();
    let until = now.checked_add(REMINDER_HORIZON).unwrap_or(now);
    let candidates: Vec<OrderId> = orders::awaiting_pickup(conn, now, until)
        .await?
        .into_iter()
        .filter(|(_, starts)| now >= reminder_due(*starts, emails.reminder_at))
        .map(|(order, _)| order)
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<String> = candidates.iter().copied().map(reminder_key).collect();
    let queued = notify::queued_keys(conn, &keys).await?;
    let company = company(conn).await?;
    let composer = Composer::new(&emails.i18n, &company);
    let mut done = Vec::new();
    for (order, key) in candidates.into_iter().zip(keys) {
        if queued.contains(&key) {
            continue;
        }
        let mut tx = conn.begin().await.map_err(orders::Error::from)?;
        let starts = orders::lock_awaiting_pickup(&mut tx, order).await?;
        let now = clock.now();
        let still_due = starts
            .is_some_and(|starts| now < starts && now >= reminder_due(starts, emails.reminder_at));
        if !still_due {
            continue;
        }
        let details = match orders::details(&mut tx, order).await {
            Ok(details) => details,
            Err(orders::Error::Db(error)) => return Err(orders::Error::Db(error).into()),
            Err(error) => {
                tracing::error!(order = order.get(), %error, "cannot read the order for its reminder");
                continue;
            }
        };
        let email = composer.pickup_reminder(&PickupReminder {
            to: recipient(&details),
            order_number: details.order.number.to_string(),
            pickup: pickup(&details),
            items: details
                .order
                .lines
                .iter()
                .zip(&details.products)
                .map(|(line, product)| Item {
                    qty: line.qty,
                    product: product.clone(),
                })
                .collect(),
        });
        match email {
            Ok(email) => {
                let queued = notify::enqueue(&mut tx, clock, &email, Some(&key)).await?;
                tx.commit().await.map_err(orders::Error::from)?;
                if queued.is_some() {
                    done.push(order);
                }
            }
            Err(error) => {
                tracing::error!(order = order.get(), %error, "cannot compose the pickup reminder");
            }
        }
    }
    Ok(done)
}
