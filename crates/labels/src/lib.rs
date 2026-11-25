//! Product labels (F18) and freezer labels (F18a) as files for the official Niimbot app
//! (ADR 0024, ADR 0047): a 300-dpi PNG per label design at its exact size, and CSV rows of
//! the label text for the app's data import.
//!
//! This crate only lays out what it is given. Whether a batch may get a label is decided
//! in `production::label_files`, which calls the F17 label check before anything reaches here.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use domain::Allergen;
use jiff::ToSpan;
use jiff::civil::Date;

mod csv;
mod data;
mod png;
mod world;

pub use crate::csv::{CsvRow, csv};
pub use crate::data::data;
pub use crate::png::{Png, Side, render, text};

/// The NIIMBOT B21 Pro prints 300 dots per inch.
pub const DPI: u32 = 300;

/// Every label is 50 mm wide: the B21 Pro's whole print head (591 dots).
pub const WIDTH_MM: u32 = 50;

/// Label lengths tried in order, shortest first (ADR 0047). A label takes the first one
/// its text fits on; text that fits none is split over a front and a back label.
pub const LENGTHS_MM: [u32; 5] = [30, 40, 50, 70, 100];

/// A label size in millimetres, width across the print head first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width_mm: u32,
    pub length_mm: u32,
}

impl Size {
    /// Pixels at 300 dpi, rounded to the nearest dot.
    #[must_use]
    pub const fn pixels(self) -> (u32, u32) {
        (dots(self.width_mm), dots(self.length_mm))
    }
}

const fn dots(mm: u32) -> u32 {
    (mm * DPI * 10 + 127) / 254
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingredient {
    pub name: String,
    /// Holds one of the 14 allergens: printed in bold.
    pub bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A packaged product (F18), with its own storage instruction.
    Product { storage: String },
    /// A piece from the freezer (F18a); the storage instruction is the freezer's.
    Freezer { frozen_on: Date },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Business {
    pub name: String,
    pub address: String,
}

/// Everything one label carries, already checked by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub kind: Kind,
    pub product: String,
    pub ingredients: Vec<Ingredient>,
    /// The kitchen-wide may-contain list without what the product contains.
    pub may_contain: BTreeSet<Allergen>,
    pub net_weight_g: u32,
    pub best_before: Date,
    /// `YYMMDD-N` (ADR 0042).
    pub batch_code: String,
    pub business: Business,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    I18n(#[from] i18n::I18nError),
    #[error("the label could not be laid out: {0}")]
    Typst(String),
    #[error("the label could not be drawn: {0}")]
    Draw(String),
    #[error("the label text does not fit on the longest label, also not split")]
    TooLong,
    #[error("a word on the label is too long for its width")]
    TooWide,
    #[error("the best-before date is out of range")]
    DateOutOfRange,
    #[error("the CSV could not be made: {0}")]
    Csv(String),
}

/// `start` plus `days`: the bake date plus the shelf life for a product label, the freeze
/// date plus the freezer period for a freezer label. Calendar days, so a change between
/// summer and winter time does not move the date.
///
/// # Errors
/// [`Error::DateOutOfRange`] past the year 9999.
pub fn best_before(start: Date, days: u16) -> Result<Date, Error> {
    start
        .checked_add(i64::from(days).days())
        .map_err(|_| Error::DateOutOfRange)
}

#[cfg(test)]
mod tests;
