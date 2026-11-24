use std::future::Future;
use std::pin::Pin;

use jiff::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EmailKind {
    OrderConfirmation,
    PickupReminder,
    OrderCancelled,
    SickDayCancelled,
    DropAlert,
    SubscriptionConfirmation,
    ContactMessage,
}

impl EmailKind {
    pub const ALL: [Self; 7] = [
        Self::OrderConfirmation,
        Self::PickupReminder,
        Self::OrderCancelled,
        Self::SickDayCancelled,
        Self::DropAlert,
        Self::SubscriptionConfirmation,
        Self::ContactMessage,
    ];

    /// The value stored in `email_outbox.kind`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OrderConfirmation => "order_confirmation",
            Self::PickupReminder => "pickup_reminder",
            Self::OrderCancelled => "order_cancelled",
            Self::SickDayCancelled => "sick_day_cancelled",
            Self::DropAlert => "drop_alert",
            Self::SubscriptionConfirmation => "subscription_confirmation",
            Self::ContactMessage => "contact_message",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// A composed email, ready to queue. The From address is the sender's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    pub kind: EmailKind,
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
    /// The https URL for `List-Unsubscribe` (RFC 8058 one-click), on drop alerts.
    pub list_unsubscribe: Option<String>,
    /// Where replies go when that is not the From address, such as a contact form sender.
    pub reply_to: Option<String>,
}

/// An email taken from the outbox to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingEmail {
    /// The local part of the Message-ID; the same on every retry.
    pub id: String,
    pub date: Timestamp,
    pub email: Email,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// Worth retrying later: a timeout, a dropped connection or an SMTP 4xx reply.
    #[error("temporary failure: {0}")]
    Transient(String),
    /// Retrying will not help: a rejected recipient or an SMTP 5xx reply.
    #[error("permanent failure: {0}")]
    Permanent(String),
}

pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<(), SendError>> + Send + 'a>>;

/// Hands an email to the mail relay. Object-safe, so the server can hold a
/// `Box<dyn EmailSender>` and tests a fake.
pub trait EmailSender: Send + Sync {
    fn send<'a>(&'a self, email: &'a OutgoingEmail) -> SendFuture<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_through_their_stored_names() {
        for kind in EmailKind::ALL {
            assert_eq!(EmailKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EmailKind::parse("newsletter"), None);
    }
}
