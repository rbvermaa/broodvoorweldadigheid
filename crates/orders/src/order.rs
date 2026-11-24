use std::fmt;

use db::time::from_micros;
use domain::{
    BakeDayId, CustomerId, Money, MoneyError, OrderId, OrderLineId, PickupSlotId, ProductId,
    VatRate, VatTotals,
};
use jiff::Timestamp;
use sqlx::PgConnection;

use crate::Error;

macro_rules! text_enum {
    ($(#[$doc:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &[Self] = &[$(Self::$variant),+];

            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            pub(crate) fn parse(text: &str) -> Result<Self, Error> {
                Self::ALL
                    .iter()
                    .copied()
                    .find(|v| v.as_str() == text)
                    .ok_or_else(|| Error::Corrupt(format!("{} {text:?}", stringify!($name))))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(OrderStatus {
    PendingPayment => "pending_payment",
    Paid => "paid",
    Confirmed => "confirmed",
    PickedUp => "picked_up",
    Cancelled => "cancelled",
    Refunded => "refunded",
});

text_enum!(
    /// Web shop or entered by staff (F6).
    Channel {
        Web => "web",
        Manual => "manual",
    }
);

text_enum!(PaymentMode {
    Online => "online",
    OnAccount => "on_account",
});

text_enum!(CancelReason {
    HoldExpired => "hold_expired",
    Staff => "staff",
    Customer => "customer",
    DayCancelled => "day_cancelled",
});

/// `2026-0042`: the 42nd order of 2026, by the order's creation date in the bakery's
/// time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderNumber {
    pub year: i16,
    pub seq: i32,
}

impl fmt::Display for OrderNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{:04}", self.year, self.seq)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderLine {
    pub id: OrderLineId,
    pub product_id: ProductId,
    pub qty: u32,
    pub unit_price: Money,
    pub suggested_price: Option<Money>,
    pub vat_rate: VatRate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub id: OrderId,
    pub number: OrderNumber,
    pub customer_id: CustomerId,
    pub bake_day_id: BakeDayId,
    pub pickup_slot_id: PickupSlotId,
    pub channel: Channel,
    pub payment_mode: PaymentMode,
    pub status: OrderStatus,
    pub cancel_reason: Option<CancelReason>,
    pub total: Money,
    pub hold_expires_at: Option<Timestamp>,
    pub payment_ref: Option<String>,
    pub paid_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub lines: Vec<OrderLine>,
}

impl Order {
    /// # Errors
    /// [`MoneyError::Overflow`].
    pub fn totals(&self) -> Result<VatTotals, MoneyError> {
        let lines: Vec<domain::Line> = self
            .lines
            .iter()
            .map(|l| domain::Line {
                unit_price: l.unit_price,
                qty: l.qty,
                vat_rate: l.vat_rate,
            })
            .collect();
        VatTotals::from_lines(&lines)
    }

    /// Money came in for an order that does not count (ADR 0012); staff or the payments
    /// crate must refund it.
    #[must_use]
    pub fn refund_due(&self) -> bool {
        self.status == OrderStatus::Cancelled && self.payment_ref.is_some()
    }
}

pub(crate) const fn vat_to_db(rate: VatRate) -> i16 {
    match rate {
        VatRate::Nine => 9,
        VatRate::TwentyOne => 21,
    }
}

struct Row {
    id: i64,
    number_year: i16,
    number_seq: i32,
    customer_id: i64,
    bake_day_id: i64,
    pickup_slot_id: i64,
    channel: String,
    payment_mode: String,
    status: String,
    cancel_reason: Option<String>,
    total_cents: i64,
    hold_expires_at_us: Option<i64>,
    payment_ref: Option<String>,
    paid_at_us: Option<i64>,
    created_at_us: i64,
}

struct LineRow {
    id: i64,
    product_id: i64,
    qty: i32,
    unit_price_cents: i64,
    suggested_price_cents: Option<i64>,
    vat_rate: i16,
}

fn line_from_row(row: LineRow) -> Result<OrderLine, Error> {
    Ok(OrderLine {
        id: OrderLineId::new(row.id),
        product_id: ProductId::new(row.product_id),
        qty: u32::try_from(row.qty).map_err(|_| Error::Corrupt(format!("qty {}", row.qty)))?,
        unit_price: Money::from_cents(row.unit_price_cents),
        suggested_price: row.suggested_price_cents.map(Money::from_cents),
        vat_rate: VatRate::try_from(row.vat_rate).map_err(|e| Error::Corrupt(e.to_string()))?,
    })
}

fn order_from_row(row: Row, lines: Vec<OrderLine>) -> Result<Order, Error> {
    Ok(Order {
        id: OrderId::new(row.id),
        number: OrderNumber {
            year: row.number_year,
            seq: row.number_seq,
        },
        customer_id: CustomerId::new(row.customer_id),
        bake_day_id: BakeDayId::new(row.bake_day_id),
        pickup_slot_id: PickupSlotId::new(row.pickup_slot_id),
        channel: Channel::parse(&row.channel)?,
        payment_mode: PaymentMode::parse(&row.payment_mode)?,
        status: OrderStatus::parse(&row.status)?,
        cancel_reason: row
            .cancel_reason
            .as_deref()
            .map(CancelReason::parse)
            .transpose()?,
        total: Money::from_cents(row.total_cents),
        hold_expires_at: row.hold_expires_at_us.map(from_micros).transpose()?,
        payment_ref: row.payment_ref,
        paid_at: row.paid_at_us.map(from_micros).transpose()?,
        created_at: from_micros(row.created_at_us)?,
        lines,
    })
}

/// An order with its lines, by line id.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Db`] or [`Error::Corrupt`].
pub async fn get(conn: &mut PgConnection, id: OrderId) -> Result<Order, Error> {
    let row = sqlx::query_as!(
        Row,
        r#"SELECT id, number_year, number_seq, customer_id, bake_day_id, pickup_slot_id,
            channel, payment_mode, status, cancel_reason, total_cents,
            (extract(epoch FROM hold_expires_at) * 1000000)::bigint AS hold_expires_at_us,
            payment_ref,
            (extract(epoch FROM paid_at) * 1000000)::bigint AS paid_at_us,
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_at_us!"
        FROM "order" WHERE id = $1"#,
        id.get(),
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::NotFound)?;
    let lines = sqlx::query_as!(
        LineRow,
        "SELECT id, product_id, qty, unit_price_cents, suggested_price_cents, vat_rate
        FROM order_line WHERE order_id = $1 ORDER BY id",
        id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(line_from_row)
    .collect::<Result<_, _>>()?;
    order_from_row(row, lines)
}

/// The fields an audit entry keeps of an order's state.
pub(crate) fn snapshot(order: &Order) -> serde_json::Value {
    serde_json::json!({
        "status": order.status.as_str(),
        "cancel_reason": order.cancel_reason.map(CancelReason::as_str),
        "total_cents": order.total.cents(),
        "hold_expires_at": order.hold_expires_at.map(|t| t.to_string()),
        "payment_ref": order.payment_ref,
        "paid_at": order.paid_at.map(|t| t.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_through_text() {
        for status in OrderStatus::ALL {
            assert_eq!(OrderStatus::parse(status.as_str()).unwrap(), *status);
            assert_eq!(status.to_string(), status.as_str());
        }
        for channel in Channel::ALL {
            assert_eq!(Channel::parse(channel.as_str()).unwrap(), *channel);
        }
        for mode in PaymentMode::ALL {
            assert_eq!(PaymentMode::parse(mode.as_str()).unwrap(), *mode);
        }
        for reason in CancelReason::ALL {
            assert_eq!(CancelReason::parse(reason.as_str()).unwrap(), *reason);
        }
        assert!(matches!(OrderStatus::parse("lost"), Err(Error::Corrupt(_))));
    }

    #[test]
    fn order_numbers_are_padded() {
        assert_eq!(
            OrderNumber {
                year: 2026,
                seq: 42
            }
            .to_string(),
            "2026-0042"
        );
        assert_eq!(
            OrderNumber {
                year: 2026,
                seq: 12_345
            }
            .to_string(),
            "2026-12345"
        );
    }

    #[test]
    fn corrupt_lines_are_reported() {
        let row = |qty, vat_rate| LineRow {
            id: 1,
            product_id: 1,
            qty,
            unit_price_cents: 450,
            suggested_price_cents: None,
            vat_rate,
        };
        assert!(matches!(line_from_row(row(-1, 9)), Err(Error::Corrupt(_))));
        assert!(matches!(line_from_row(row(1, 6)), Err(Error::Corrupt(_))));
        assert_eq!(
            line_from_row(row(2, 21)).unwrap().vat_rate,
            VatRate::TwentyOne
        );
    }
}
