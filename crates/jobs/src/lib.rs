//! Scheduled work (F1): background jobs on apalis with PostgreSQL. See ADR 0011.
#![forbid(unsafe_code)]

pub mod bake_days;
mod error;
pub mod scheduler;

pub use bake_days::TickReport;
pub use error::Error;
pub use scheduler::{BakeDayTick, run, schedule_tick, setup};
