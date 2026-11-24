#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("invalid {0}")]
    Invalid(&'static str),
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
