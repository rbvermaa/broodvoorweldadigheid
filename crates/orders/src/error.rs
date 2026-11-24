use domain::{Money, MoneyError, ProductId};

use crate::cart::CartError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error(transparent)]
    Cart(#[from] CartError),
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("the bake day has no online ordering")]
    ProductionOnly,
    #[error("the bake day is not open for orders")]
    NotOpen,
    #[error("the order deadline has passed")]
    PastDeadline,
    #[error("the bake day is done or cancelled")]
    DayFinal,
    #[error("the pickup slot is not on this bake day")]
    SlotNotOnDay,
    #[error("the pickup slot is full")]
    SlotFull,
    #[error("only {remaining} left of product {product_id:?}")]
    CapacityExceeded {
        product_id: ProductId,
        remaining: u32,
    },
    #[error("paid {paid:?} but the order total is {expected:?}")]
    AmountMismatch { expected: Money, paid: Money },
    #[error("amount out of range")]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Catalog(#[from] catalog::Error),
    #[error(transparent)]
    BakeDays(bakedays::Error),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

impl From<bakedays::Error> for Error {
    fn from(err: bakedays::Error) -> Self {
        match err {
            bakedays::Error::CapacityExceeded {
                product_id,
                remaining,
            } => Self::CapacityExceeded {
                product_id,
                remaining,
            },
            bakedays::Error::Db(err) => Self::Db(err),
            other => Self::BakeDays(other),
        }
    }
}

impl From<db::time::OutOfRange> for Error {
    fn from(err: db::time::OutOfRange) -> Self {
        Self::Corrupt(err.to_string())
    }
}
