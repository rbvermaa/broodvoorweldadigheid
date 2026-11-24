use std::fmt;
use std::future::Future;
use std::pin::Pin;

use crate::{Money, OrderId};

/// Boxed so the trait stays dyn-compatible and the server can hold an
/// `Arc<dyn PaymentProvider>` (ADR 0013).
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PaymentError>> + Send + 'a>>;

/// Every call to a payment provider (Mollie) goes through this trait.
///
/// A webhook only names a payment; call [`PaymentProvider::get_payment`] and act on
/// what it returns, never on the webhook itself.
pub trait PaymentProvider: Send + Sync {
    fn create_payment<'a>(&'a self, request: &'a NewPayment) -> ProviderFuture<'a, CreatedPayment>;

    fn get_payment<'a>(&'a self, id: &'a ProviderPaymentId) -> ProviderFuture<'a, PaymentSnapshot>;

    fn refund<'a>(&'a self, request: &'a RefundRequest) -> ProviderFuture<'a, CreatedRefund>;

    /// One page of fees the provider took per payment, newest first. Pass the
    /// returned `next` cursor to get the following page.
    fn list_payment_fees<'a>(
        &'a self,
        cursor: Option<&'a FeeCursor>,
    ) -> ProviderFuture<'a, FeePage>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaymentError {
    #[error("the request was not sent: {0}")]
    InvalidRequest(String),
    #[error("the payment provider refused the credentials")]
    Unauthorized,
    #[error("the payment provider does not know this payment")]
    NotFound,
    #[error("the payment provider rejected the request: {detail}")]
    Rejected {
        field: Option<String>,
        detail: String,
    },
    #[error("the same request is still being processed")]
    Conflict,
    #[error("too many requests to the payment provider")]
    RateLimited,
    #[error("the payment provider is unavailable: {0}")]
    Unavailable(String),
    #[error("unexpected answer from the payment provider: {0}")]
    InvalidResponse(String),
    #[error("not configured: {0}")]
    NotConfigured(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind}: expected 1 to {max} characters from A-Z, a-z, 0-9, _ and -")]
pub struct InvalidToken {
    kind: &'static str,
    max: usize,
}

fn check_token(value: &str, kind: &'static str, max: usize) -> Result<(), InvalidToken> {
    let ok = !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(InvalidToken { kind, max })
    }
}

macro_rules! token_type {
    ($(#[$doc:meta])* $name:ident, $kind:literal, $max:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// # Errors
            /// [`InvalidToken`] when the value is empty, too long or has other characters.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidToken> {
                let value = value.into();
                check_token(&value, $kind, $max)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

token_type!(
    /// The provider's id for a payment, such as Mollie's `tr_…`.
    ProviderPaymentId,
    "payment id",
    64
);
token_type!(
    /// The provider's id for a refund, such as Mollie's `re_…`.
    ProviderRefundId,
    "refund id",
    64
);
token_type!(
    /// Sent so that a retried create or refund is not carried out twice.
    IdempotencyKey,
    "idempotency key",
    128
);
token_type!(
    /// Where the next page of [`PaymentProvider::list_payment_fees`] starts.
    FeeCursor,
    "fee cursor",
    128
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaymentMethod {
    Ideal,
    Wero,
    CreditCard,
    Bancontact,
}

impl PaymentMethod {
    pub const ALL: [Self; 4] = [Self::Ideal, Self::Wero, Self::CreditCard, Self::Bancontact];

    /// The method name Mollie uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ideal => "ideal",
            Self::Wero => "wero",
            Self::CreditCard => "creditcard",
            Self::Bancontact => "bancontact",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPayment {
    pub amount: Money,
    pub description: String,
    pub redirect_url: String,
    pub webhook_url: String,
    pub order: OrderId,
    /// Empty lets the customer choose from every method enabled for the account.
    pub methods: Vec<PaymentMethod>,
    /// `None` makes the provider client pick a fresh key.
    pub idempotency_key: Option<IdempotencyKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedPayment {
    pub id: ProviderPaymentId,
    pub checkout_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaymentStatus {
    Open,
    Pending,
    Authorized,
    Paid,
    Failed,
    Canceled,
    Expired,
}

impl PaymentStatus {
    /// No later webhook can change a payment in this status (refunds aside).
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(
            self,
            Self::Paid | Self::Failed | Self::Canceled | Self::Expired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentSnapshot {
    pub id: ProviderPaymentId,
    pub status: PaymentStatus,
    pub amount: Money,
    pub amount_refunded: Money,
    /// The provider's method name, such as `ideal`; `None` until the customer picks one.
    pub method: Option<String>,
    /// The order the payment was created for, read back from its metadata.
    pub order: Option<OrderId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundRequest {
    pub payment: ProviderPaymentId,
    pub amount: Money,
    pub description: String,
    /// Required: a refund retried after a lost answer must not pay out twice. Derive
    /// it from what the refund is for, such as `refund-<order>-<n>`.
    pub idempotency_key: IdempotencyKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefundStatus {
    Queued,
    Pending,
    Processing,
    Refunded,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedRefund {
    pub id: ProviderRefundId,
    pub status: RefundStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeeKind {
    /// Deducted from a payment or capture.
    Payment,
    /// Deducted from a refund; `gross` and `net` are negative.
    Refund,
    /// Deducted from a chargeback; `gross` and `net` are negative.
    Chargeback,
    /// Booked on its own, such as a fee for a failed payment; `gross` is the charge.
    Separate,
}

/// The total fee for a payment is the sum of `fee` over all its entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentFee {
    pub payment: ProviderPaymentId,
    pub kind: FeeKind,
    /// The amount moved before deductions: what the customer paid, or minus what was
    /// refunded.
    pub gross: Money,
    /// What the provider charged; zero or positive.
    pub fee: Money,
    /// What the balance changed by. Can differ from `gross - fee` when the provider
    /// holds back a reserve.
    pub net: Money,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeePage {
    pub fees: Vec<PaymentFee>,
    pub next: Option<FeeCursor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_accept_provider_ids() {
        let id = ProviderPaymentId::new("tr_7UhSN1zuXS").unwrap();
        assert_eq!(id.as_str(), "tr_7UhSN1zuXS");
        assert_eq!(id.to_string(), "tr_7UhSN1zuXS");
        assert!(ProviderRefundId::new("re_4qqhO89gsT").is_ok());
        assert!(IdempotencyKey::new("refund-42-1").is_ok());
        assert!(FeeCursor::new("baltr_QM24QwzUWR4ev4Xfgyt29A").is_ok());
    }

    #[test]
    fn tokens_refuse_bad_values() {
        for bad in ["", "tr 1", "tr_1/../x", "tr_é", "id=tr_1", &"a".repeat(65)] {
            assert!(ProviderPaymentId::new(bad).is_err(), "{bad:?}");
        }
        assert!(IdempotencyKey::new("a".repeat(128)).is_ok());
        let err = IdempotencyKey::new("a".repeat(129)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid idempotency key: expected 1 to 128 characters from A-Z, a-z, 0-9, _ and -"
        );
    }

    #[test]
    fn method_names_match_mollie() {
        let names: Vec<_> = PaymentMethod::ALL.iter().map(|m| m.as_str()).collect();
        assert_eq!(names, ["ideal", "wero", "creditcard", "bancontact"]);
    }

    #[test]
    fn final_statuses() {
        use PaymentStatus::*;
        for status in [Paid, Failed, Canceled, Expired] {
            assert!(status.is_final(), "{status:?}");
        }
        for status in [Open, Pending, Authorized] {
            assert!(!status.is_final(), "{status:?}");
        }
    }

    #[test]
    fn errors_read_well() {
        let err = PaymentError::Rejected {
            field: Some("amount".into()),
            detail: "The amount is lower than the minimum".into(),
        };
        assert_eq!(
            err.to_string(),
            "the payment provider rejected the request: The amount is lower than the minimum"
        );
    }
}
