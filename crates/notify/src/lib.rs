//! Email templates and sending (F2, F9, F9a; ADR 0014).
#![forbid(unsafe_code)]

mod compose;
mod content;
mod email;
pub mod outbox;
mod smtp;
pub mod style;

pub use compose::{
    Company, ComposeError, Composer, Donation, DropAlert, DropAlertKind, Item, OrderCancelled,
    OrderConfirmation, OrderLine, Pickup, PickupReminder, Recipient, SickDayCancelled,
    SubscriptionConfirmation,
};
pub use content::RenderError;
pub use email::{Email, EmailKind, EmailSender, OutgoingEmail, SendError, SendFuture};
pub use outbox::{OutboxError, OutboxId, SendReport, enqueue, queued_keys, send_pending};
pub use smtp::{Secret, SmtpConfig, SmtpConfigError, SmtpSender, TlsMode, message};
