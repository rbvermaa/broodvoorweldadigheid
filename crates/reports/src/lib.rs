//! Sales per BTW rate, the B2B invoice overview, charity payouts, donation reports and
//! the bakery's contribution, with CSV export (F11, F11a, F11b, F11c, F11e). See
//! ADR 0017.
#![forbid(unsafe_code)]

pub mod contribution;
mod csv_out;
pub mod donations;
mod error;
mod facts;
pub mod invoices;
pub mod payouts;
mod period;
mod refunds;
pub mod sales;

pub use error::Error;
pub use period::{Granularity, Period};
pub use refunds::refund_lines;
