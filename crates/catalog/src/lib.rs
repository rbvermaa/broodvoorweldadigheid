#![forbid(unsafe_code)]

mod error;
pub mod products;

pub use error::Error;
pub use products::{AllergenInfo, Approval, Pricing, Product, ProductInput};
