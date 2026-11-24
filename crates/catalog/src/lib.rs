#![forbid(unsafe_code)]

pub mod charities;
pub mod company;
mod error;
pub mod products;

pub use charities::{Charity, CharityInput};
pub use company::SourdoughDeadline;
pub use error::Error;
pub use products::{AllergenInfo, Approval, Pricing, Product, ProductInput};
