use domain::RecipeVersionId;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("the name is already in use")]
    Duplicate,
    /// A frozen version uses it, and the change would alter that version's figures.
    #[error("a frozen recipe version uses it")]
    InUse,
    /// The recipe already has a draft; edit that one.
    #[error("recipe already has draft version {0:?}")]
    DraftExists(RecipeVersionId),
    /// Sub-recipes nest deeper than [`crate::MAX_NESTING`], or a recipe uses itself.
    #[error("sub-recipes nest too deep or use the recipe itself")]
    Nesting,
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

impl From<crate::formula::UnknownCode> for Error {
    fn from(err: crate::formula::UnknownCode) -> Self {
        Self::Corrupt(err.to_string())
    }
}

/// A unique violation becomes [`Error::Duplicate`].
pub(crate) fn unique(err: sqlx::Error) -> Error {
    match &err {
        sqlx::Error::Database(db) if db.is_unique_violation() => Error::Duplicate,
        _ => Error::Db(err),
    }
}
