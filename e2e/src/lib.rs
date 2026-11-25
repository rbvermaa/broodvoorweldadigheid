//! Browser tests for the webshop and back office (thirtyfour). The flows in [`flows`]
//! drive a running server through a WebDriver, pay in the mock Mollie of [`mollie`] and
//! read emails from the outbox; [`seed`] adds the data each flow needs. See the README
//! for running them outside the VM test (`make e2e-local`).
#![forbid(unsafe_code)]

pub mod browser;
pub mod flows;
pub mod mollie;
pub mod seed;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
