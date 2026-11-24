//! Email templates and sending (F2, F9, F9a; ADR 0014).
#![forbid(unsafe_code)]

mod compose;
mod content;
mod email;
pub mod outbox;
mod smtp;
pub mod style;
pub mod subscribers;

pub use compose::{
    Company, ComposeError, Composer, ContactMessage, Donation, DropAlert, DropAlertKind, Item,
    LoginLink, OrderCancelled, OrderConfirmation, OrderLine, Pickup, PickupMoved, PickupReminder,
    Recipient, SickDayCancelled, SubscriptionConfirmation, is_address,
};
pub use content::RenderError;
pub use email::{Email, EmailKind, EmailSender, OutgoingEmail, SendError, SendFuture};
pub use outbox::{
    OutboxError, OutboxId, SendReport, enqueue, enqueue_expiring, queued_keys, send_pending,
    withdraw_unsent,
};
pub use smtp::{Secret, SmtpConfig, SmtpConfigError, SmtpSender, TlsMode, message};
