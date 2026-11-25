//! CSV rows of the label text for the Niimbot app's data import (ADR 0047).

use i18n::{Translator, numeric_date};

use crate::data::{allergen_names, business, net_weight, storage};
use crate::{Error, Kind, Label};

/// One label design and how many to print of it.
#[derive(Debug, Clone, Copy)]
pub struct CsvRow<'a> {
    pub label: &'a Label,
    pub copies: u32,
    /// The PNG with this design; its front when it is split.
    pub file: &'a str,
}

const COLUMNS: [&str; 12] = [
    "labels-csv-file",
    "labels-csv-kind",
    "labels-csv-product",
    "labels-csv-ingredients",
    "labels-csv-may-contain",
    "labels-csv-net-weight",
    "labels-csv-best-before",
    "labels-csv-frozen-on",
    "labels-csv-storage",
    "labels-csv-batch",
    "labels-csv-business",
    "labels-csv-copies",
];

/// A text field cannot be bold, so ingredients with an allergen are in capitals, which
/// EU 1169/2011 also accepts as emphasis.
fn ingredients(label: &Label) -> String {
    label
        .ingredients
        .iter()
        .map(|i| {
            if i.bold {
                i.name.to_uppercase()
            } else {
                i.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The rows as CSV in the shape of the other exports (ADR 0017): semicolons, CRLF, UTF-8
/// with a byte order mark, a header row of Dutch column names.
///
/// # Errors
/// [`Error::I18n`] or [`Error::Csv`].
pub fn csv(rows: &[CsvRow<'_>], t: &Translator) -> Result<String, Error> {
    let csv_error = |e: csv::Error| Error::Csv(e.to_string());
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b';')
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    let header = COLUMNS
        .iter()
        .map(|id| t.t(id, None))
        .collect::<Result<Vec<_>, _>>()?;
    writer.write_record(&header).map_err(csv_error)?;
    for row in rows {
        let label = row.label;
        let (kind, frozen_on) = match label.kind {
            Kind::Product { .. } => (t.t("labels-kind-product", None)?, String::new()),
            Kind::Freezer { frozen_on } => {
                (t.t("labels-kind-freezer", None)?, numeric_date(frozen_on))
            }
        };
        writer
            .write_record([
                row.file.to_owned(),
                kind,
                label.product.clone(),
                ingredients(label),
                allergen_names(label, t)?.join(", "),
                net_weight(label, t)?,
                numeric_date(label.best_before),
                frozen_on,
                storage(label, t)?,
                label.batch_code.clone(),
                business(label, t)?,
                row.copies.to_string(),
            ])
            .map_err(csv_error)?;
    }
    let bytes = writer.into_inner().map_err(|e| Error::Csv(e.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|e| Error::Csv(e.to_string()))?;
    Ok(format!("\u{feff}{text}"))
}
