#![forbid(unsafe_code)]

pub mod charities;
mod error;
pub mod products;

pub use charities::{Charity, CharityInput};
pub use error::Error;
pub use products::{AllergenInfo, Approval, Pricing, Product, ProductInput};
