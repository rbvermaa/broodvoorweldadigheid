//! Scheduled work (F1, F9a): background jobs on apalis with PostgreSQL. See ADRs 0011
//! and 0021.
#![forbid(unsafe_code)]

pub mod bake_days;
pub mod drop_alerts;
mod error;
pub mod scheduler;

pub use bake_days::TickReport;
pub use drop_alerts::AlertReport;
pub use error::Error;
pub use scheduler::{BakeDayTick, Mail, TickOutcome, run, schedule_tick, setup, work};
