#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Orders(#[from] orders::Error),
    #[error(transparent)]
    BakeDays(#[from] bakedays::Error),
    #[error(transparent)]
    Catalog(#[from] catalog::Error),
    #[error(transparent)]
    Recipes(#[from] recipes::Error),
    #[error(transparent)]
    I18n(#[from] i18n::I18nError),
    #[error("not found")]
    NotFound,
    #[error("invalid {0}")]
    Invalid(&'static str),
    /// The mixer capacity is not set, so batches can only be started by hand.
    #[error("the mixer capacity is not set")]
    NoCapacity,
    /// The bake day is cancelled or done.
    #[error("the bake day is cancelled or done")]
    DayFinal,
    /// The batch is mixed; its lines and planned pieces are locked.
    #[error("the batch is mixed")]
    Locked,
    #[error("the batch is not mixed yet")]
    NotMixed,
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error("the PDF could not be made: {0}")]
    Pdf(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

impl From<db::time::OutOfRange> for Error {
    fn from(err: db::time::OutOfRange) -> Self {
        Self::Corrupt(err.to_string())
    }
}
