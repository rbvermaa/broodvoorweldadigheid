#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Orders(#[from] orders::Error),
    #[error(transparent)]
    BakeDays(#[from] bakedays::Error),
    #[error(transparent)]
    Catalog(#[from] catalog::Error),
    #[error(transparent)]
    I18n(#[from] i18n::I18nError),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error("the PDF could not be made: {0}")]
    Pdf(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}
