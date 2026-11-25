use domain::{Money, MoneyError, OrderId};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("order {0:?} is already invoiced")]
    AlreadyInvoiced(OrderId),
    #[error("order {0:?} is not an uninvoiced on-account order of this customer")]
    NotInvoiceable(OrderId),
    #[error("invoice number is already used")]
    DuplicateInvoiceNumber,
    #[error("the amount to transfer is {due:?}")]
    AmountMismatch { due: Money },
    #[error("nothing to transfer: the amount due is {due:?}")]
    NothingToTransfer { due: Money },
    #[error("amount out of range")]
    Money(#[from] MoneyError),
    #[error("cannot write CSV: {0}")]
    Csv(String),
    #[error(transparent)]
    I18n(#[from] i18n::I18nError),
    #[error(transparent)]
    Orders(#[from] orders::Error),
    #[error(transparent)]
    Pdf(#[from] production::Error),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

impl From<db::time::OutOfRange> for Error {
    fn from(err: db::time::OutOfRange) -> Self {
        Self::Corrupt(err.to_string())
    }
}

impl From<domain::UnknownVatRate> for Error {
    fn from(err: domain::UnknownVatRate) -> Self {
        Self::Corrupt(err.to_string())
    }
}
