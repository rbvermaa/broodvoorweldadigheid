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
    /// The bake day is cancelled or done.
    #[error("the bake day is cancelled or done")]
    DayFinal,
    /// The batch is mixed; its lines and planned pieces are locked.
    #[error("the batch is mixed")]
    Locked,
    #[error("the batch is not mixed yet")]
    NotMixed,
    #[error("the batch has no actual pieces yet")]
    NotBaked,
    /// The batch's product may not get a label (F17).
    #[error("the label is blocked: {0:?}")]
    LabelBlocked(crate::label_check::LabelBlock),
    /// The batch's computed allergen data changed since it was reviewed.
    #[error("allergen data changed since it was reviewed")]
    AllergensChanged,
    /// Something a label needs besides the allergen check is missing (F18, F18a).
    #[error("the label is incomplete: {0:?}")]
    LabelGap(crate::label_files::LabelGap),
    #[error(transparent)]
    Label(#[from] labels::Error),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error("the CSV could not be made: {0}")]
    Csv(String),
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
