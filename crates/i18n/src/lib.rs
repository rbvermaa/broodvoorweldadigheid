#![forbid(unsafe_code)]

mod format;
mod translator;

pub use fluent_bundle::{FluentArgs, FluentValue};
pub use format::{Amsterdam, TIME_ZONE, long_date, money, short_date, time, time_range};
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
    #[error("cannot load time zone {zone}: {details}")]
    TimeZone { zone: String, details: String },
}
