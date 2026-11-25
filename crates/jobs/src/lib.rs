//! Scheduled work (F1, F4d, F9a, F12, F14a): background jobs on apalis with PostgreSQL. See ADRs
//! 0011, 0021 and 0031.
#![forbid(unsafe_code)]

pub mod bake_days;
pub mod batches;
pub mod cleanup;
pub mod drop_alerts;
mod error;
pub mod order_emails;
pub mod saved_donations;
pub mod scheduler;

pub use bake_days::TickReport;
pub use batches::SplitReport;
pub use cleanup::CleanupReport;
pub use drop_alerts::AlertReport;
pub use error::Error;
pub use order_emails::{EmailError, OrderEmails};
pub use saved_donations::MoveReport;
pub use scheduler::{BakeDayTick, Mail, TickOutcome, run, schedule_tick, setup, work};
