use domain::ProductId;
use jiff::civil::{Date, Weekday};

use crate::capacity::Exceeded;
use crate::status::TransitionRefused;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("the location has no active pickup window on {0:?}")]
    NoPickupWindow(Weekday),
    #[error("a bake day already uses this pickup location on {0}")]
    DateTaken(Date),
    #[error("the charity is archived")]
    CharityArchived,
    #[error("the charity cannot change after the first order")]
    OrdersExist,
    #[error("the bake day is done or cancelled")]
    Final,
    #[error(transparent)]
    Transition(#[from] TransitionRefused),
    #[error("only {remaining} left of product {product_id:?}")]
    CapacityExceeded {
        product_id: ProductId,
        remaining: u32,
    },
    #[error(transparent)]
    Catalog(#[from] catalog::Error),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

impl From<Exceeded> for Error {
    fn from(err: Exceeded) -> Self {
        Self::CapacityExceeded {
            product_id: err.product_id,
            remaining: err.remaining,
        }
    }
}

impl From<db::time::OutOfRange> for Error {
    fn from(err: db::time::OutOfRange) -> Self {
        Self::Corrupt(err.to_string())
    }
}
