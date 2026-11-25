//! Sales per BTW rate, the B2B invoice overview, charity payouts, donation reports and
//! the bakery's contribution, with CSV export (F11, F11a, F11b, F11c, F11e), and the
//! public donation totals (F11d). See ADR 0017 and 0040.
#![forbid(unsafe_code)]

pub mod contribution;
mod csv_out;
pub mod donations;
mod error;
mod facts;
pub mod invoices;
pub mod payouts;
mod period;
pub mod public;
mod refunds;
pub mod sales;

pub use error::Error;
pub use period::{Granularity, Period};
pub use refunds::refund_lines;
