use serde_json::Value;

use crate::{Error, pdf};

/// The B2B invoice overview (F11a) as an A4 PDF. `reports` owns the overview and hands
/// over `data` with every text translated and formatted (ADR 0034); `ident` names the
/// document.
///
/// # Errors
/// [`Error::Pdf`].
pub fn invoice_overview_pdf(data: &Value, ident: String) -> Result<Vec<u8>, Error> {
    pdf::render(&pdf::INVOICE_OVERVIEW, data, ident)
}

/// The text of [`invoice_overview_pdf`], line by line, and the layout warnings, for
/// snapshot tests in `reports`.
///
/// # Errors
/// [`Error::Pdf`].
pub fn invoice_overview_text(data: &Value) -> Result<(String, Vec<String>), Error> {
    let (document, warnings) = pdf::layout(&pdf::INVOICE_OVERVIEW, data)?;
    Ok((pdf::text(&document), warnings))
}
