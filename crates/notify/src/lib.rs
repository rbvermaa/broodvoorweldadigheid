//! Email templates and sending (F2, F9, F9a; ADR 0014).
#![forbid(unsafe_code)]

mod compose;
mod content;
mod email;
pub mod outbox;
pub mod polls;
mod smtp;
pub mod style;
pub mod subscribers;
mod token;

pub use compose::{
    Company, ComposeError, Composer, ContactMessage, Donation, DonationMoved, DropAlert,
    DropAlertKind, Item, LoginLink, OnAccountConfirmation, OrderCancelled, OrderConfirmation,
    OrderLine, Pickup, PickupMoved, PickupReminder, Recipient, SickDayCancelled,
    SubscriptionConfirmation, VoteConfirmation, is_address,
};
pub use content::RenderError;
pub use email::{Email, EmailKind, EmailSender, OutgoingEmail, SendError, SendFuture};
pub use outbox::{
    OutboxError, OutboxId, SendReport, delete_finished, delete_retired, enqueue, enqueue_expiring,
    forget_address, queued_keys, send_pending, withdraw_unsent, withdraw_unsent_prefixed,
};
pub use smtp::{Secret, SmtpConfig, SmtpConfigError, SmtpSender, TlsMode, message};
