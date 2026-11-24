//! Email templates and sending (F2, F9, F9a; ADR 0014).
#![forbid(unsafe_code)]

mod company;
mod compose;
mod content;
mod email;
pub mod outbox;
mod smtp;
pub mod style;
pub mod subscribers;

pub use company::load_company;
pub use compose::{
    Company, ComposeError, Composer, Donation, DropAlert, DropAlertKind, Item, OrderCancelled,
    OrderConfirmation, OrderLine, Pickup, PickupReminder, Recipient, SickDayCancelled,
    SubscriptionConfirmation,
};
pub use content::RenderError;
pub use email::{Email, EmailKind, EmailSender, OutgoingEmail, SendError, SendFuture};
pub use outbox::{OutboxError, OutboxId, SendReport, enqueue, send_pending};
pub use smtp::{Secret, SmtpConfig, SmtpConfigError, SmtpSender, TlsMode, message};
