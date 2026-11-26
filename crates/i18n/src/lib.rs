//! Dutch UI text and formats.
//!
//! Numbers, money, dates and times shown to people are formatted here and passed into
//! Fluent messages as preformatted strings. Pass a Fluent number only when a message
//! selects on it, such as a plural.
#![forbid(unsafe_code)]

mod format;
mod search;
mod translator;

pub use fluent_bundle::{FluentArgs, FluentValue};
pub use format::{Amsterdam, decimal, money, numeric_date, time, time_range};
pub use search::search_key;
pub use translator::{LOCALE, Translator, resource_files};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum I18nError {
    #[error("invalid locale {0}")]
    Locale(String),
    #[error("{file}: Fluent syntax errors: {details}")]
    Parse { file: String, details: String },
    #[error("{file}: cannot add to the Fluent bundle: {details}")]
    Bundle { file: String, details: String },
    #[error("missing Fluent message {0}")]
    MissingMessage(String),
    #[error("Fluent message {0} has no value")]
    NoValue(String),
    #[error("cannot format Fluent message {id}: {details}")]
    Format { id: String, details: String },
    #[error("Fluent message {id} gives the same name for different values")]
    DuplicateName { id: String },
    #[error("time range ends before it starts or spans days: {start} to {end}")]
    TimeRange { start: String, end: String },
}
