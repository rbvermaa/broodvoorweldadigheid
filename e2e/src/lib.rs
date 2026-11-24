//! Browser tests for the webshop and back office (thirtyfour). The flows drive a running
//! server through a WebDriver; see the README for running them outside the VM test.
#![forbid(unsafe_code)]

pub mod browser;
pub mod flows;
pub mod mollie;
pub mod seed;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
