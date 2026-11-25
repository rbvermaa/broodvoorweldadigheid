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
    /// The version is frozen; only a draft can change.
    #[error("the recipe version is frozen")]
    Frozen,
    /// The recipe already has a draft; edit that one.
    #[error("recipe already has draft version {0:?}")]
    DraftExists(RecipeVersionId),
    /// Sub-recipes nest deeper than [`crate::MAX_NESTING`], or a recipe uses itself.
    #[error("sub-recipes nest too deep or use the recipe itself")]
    Nesting,
    /// A formula or one of its sub-recipes has no lines, so nothing can be weighed.
    #[error("recipe has no lines to weigh")]
    Unweighable,
    /// The computed allergen data or the recipe changed since it was reviewed.
    #[error("allergen data changed since it was reviewed")]
    AllergensChanged,
    #[error(transparent)]
    Catalog(catalog::Error),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_well_in_the_log() {
        let cases = [
            (Error::NotFound, "not found"),
            (Error::Invalid("name"), "invalid name"),
            (Error::Duplicate, "the name is already in use"),
            (Error::InUse, "a frozen recipe version uses it"),
            (Error::Frozen, "the recipe version is frozen"),
            (
                Error::DraftExists(RecipeVersionId::new(7)),
                "recipe already has draft version",
            ),
            (
                Error::Nesting,
                "sub-recipes nest too deep or use the recipe itself",
            ),
            (Error::Unweighable, "recipe has no lines to weigh"),
            (
                Error::AllergensChanged,
                "allergen data changed since it was reviewed",
            ),
            (Error::Catalog(catalog::Error::NotFound), "not found"),
            (Error::Db(sqlx::Error::RowNotFound), "database error: "),
        ];
        for (error, text) in cases {
            assert!(error.to_string().starts_with(text), "{error}");
        }
    }

    #[test]
    fn bad_stored_values_are_corrupt() {
        let time = Error::from(db::time::OutOfRange("timestamp"));
        assert!(matches!(&time, Error::Corrupt(_)), "{time:?}");
        assert!(time.to_string().starts_with("stored data is invalid: "));
        let code = Error::from(crate::formula::UnknownCode("meel".to_owned()));
        assert!(
            matches!(&code, Error::Corrupt(m) if m.contains("meel")),
            "{code:?}"
        );
    }
}
