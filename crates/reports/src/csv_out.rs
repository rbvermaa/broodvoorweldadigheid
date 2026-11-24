//! CSV for the accountant and Dutch Excel: semicolons, decimal commas, no thousands
//! separator, dates as dd-mm-jjjj, UTF-8 with a byte order mark (ADR 0017).

use domain::{Money, VatRate};
use i18n::Translator;
use jiff::civil::Date;

use crate::Error;

pub(crate) struct Csv {
    writer: csv::Writer<Vec<u8>>,
}

impl Csv {
    pub fn new(t: &Translator, header_keys: &[&str]) -> Result<Self, Error> {
        let mut csv = Self {
            writer: csv::WriterBuilder::new()
                .delimiter(b';')
                .terminator(csv::Terminator::CRLF)
                .from_writer(Vec::new()),
        };
        let header = header_keys
            .iter()
            .map(|key| t.t(key, None))
            .collect::<Result<Vec<_>, _>>()?;
        csv.row(&header)?;
        Ok(csv)
    }

    pub fn row<S: AsRef<[u8]>>(&mut self, fields: &[S]) -> Result<(), Error> {
        self.writer
            .write_record(fields)
            .map_err(|e| Error::Csv(e.to_string()))
    }

    pub fn finish(self) -> Result<String, Error> {
        let bytes = self
            .writer
            .into_inner()
            .map_err(|e| Error::Csv(e.to_string()))?;
        let text = String::from_utf8(bytes).map_err(|e| Error::Csv(e.to_string()))?;
        Ok(format!("\u{feff}{text}"))
    }
}

/// `1234,56`, `-4,50`
pub(crate) fn amount(money: Money) -> String {
    let cents = money.cents();
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    format!("{sign}{},{:02}", abs / 100, abs % 100)
}

pub(crate) fn date(date: Date) -> String {
    i18n::numeric_date(date)
}

pub(crate) fn rate(rate: VatRate) -> String {
    format!("{}%", rate.percent())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_use_a_decimal_comma_without_thousands() {
        assert_eq!(amount(Money::from_cents(123_456)), "1234,56");
        assert_eq!(amount(Money::from_cents(-450)), "-4,50");
        assert_eq!(amount(Money::from_cents(5)), "0,05");
        assert_eq!(amount(Money::ZERO), "0,00");
        assert_eq!(amount(Money::from_cents(i64::MIN)), "-92233720368547758,08");
    }

    #[test]
    fn rates_and_dates() {
        assert_eq!(rate(VatRate::Nine), "9%");
        assert_eq!(rate(VatRate::TwentyOne), "21%");
        assert_eq!(date(jiff::civil::date(2026, 3, 5)), "05-03-2026");
    }

    #[test]
    fn fields_with_semicolons_are_quoted() {
        let t = Translator::dutch().unwrap();
        let mut csv = Csv::new(&t, &["report-col-product"]).unwrap();
        csv.row(&["Brood; groot"]).unwrap();
        let text = csv.finish().unwrap();
        assert!(text.starts_with('\u{feff}'));
        assert!(text.ends_with("\"Brood; groot\"\r\n"));
    }
}
