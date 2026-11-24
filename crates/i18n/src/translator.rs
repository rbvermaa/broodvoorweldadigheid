use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use jiff::civil::Date;
use unic_langid::LanguageIdentifier;

use crate::I18nError;

pub const LOCALE: &str = "nl-NL";

const COMMON: (&str, &str) = ("common.ftl", include_str!("../../../locales/nl/common.ftl"));
const DATES: (&str, &str) = ("dates.ftl", include_str!("../../../locales/nl/dates.ftl"));
const CALENDAR: (&str, &str) = (
    "calendar.ftl",
    include_str!("../../../locales/nl/calendar.ftl"),
);
const REPORTS: (&str, &str) = (
    "reports.ftl",
    include_str!("../../../locales/nl/reports.ftl"),
);
const COMPONENTS: (&str, &str) = (
    "components.ftl",
    include_str!("../../../locales/nl/components.ftl"),
);
const GALLERY: (&str, &str) = (
    "gallery.ftl",
    include_str!("../../../locales/nl/gallery.ftl"),
);
const EMAIL: (&str, &str) = ("email.ftl", include_str!("../../../locales/nl/email.ftl"));
const DUTCH: &[(&str, &str)] = &[COMMON, DATES, CALENDAR, COMPONENTS, GALLERY, EMAIL, REPORTS];

/// Names of the `locales/nl/*.ftl` files compiled into the binary.
pub fn resource_files() -> impl Iterator<Item = &'static str> {
    DUTCH.iter().map(|(file, _)| *file)
}

#[derive(Default)]
struct DateNames {
    weekday: Vec<String>,
    weekday_short: Vec<String>,
    month: Vec<String>,
    month_short: Vec<String>,
}

pub struct Translator {
    bundle: FluentBundle<FluentResource>,
    ids: Vec<String>,
    dates: DateNames,
}

impl Translator {
    /// Builds the nl-NL bundle from the Fluent files compiled into the binary.
    ///
    /// # Errors
    /// Fails when a Fluent file has a syntax error or repeats a message id, or when a
    /// weekday, month or date message is missing or broken.
    pub fn dutch() -> Result<Self, I18nError> {
        Self::from_sources(LOCALE, DUTCH)
    }

    fn from_sources(locale: &str, sources: &[(&str, &str)]) -> Result<Self, I18nError> {
        let langid: LanguageIdentifier = locale
            .parse()
            .map_err(|_| I18nError::Locale(locale.to_owned()))?;
        let mut bundle = FluentBundle::new_concurrent(vec![langid]);
        // Dutch is left-to-right only; isolation marks would end up in HTML and emails.
        bundle.set_use_isolating(false);
        let mut ids = Vec::new();
        for (file, source) in sources {
            let resource = FluentResource::try_new((*source).to_owned()).map_err(|(_, errs)| {
                I18nError::Parse {
                    file: (*file).to_owned(),
                    details: join(&errs),
                }
            })?;
            bundle
                .add_resource(resource)
                .map_err(|errs| I18nError::Bundle {
                    file: (*file).to_owned(),
                    details: join(&errs),
                })?;
            ids.extend(message_ids_in(source));
        }
        ids.retain(|id| bundle.has_message(id));
        ids.sort();
        ids.dedup();
        let mut translator = Self {
            bundle,
            ids,
            dates: DateNames::default(),
        };
        translator.dates = DateNames {
            weekday: translator.names("weekday", 7)?,
            weekday_short: translator.names("weekday-short", 7)?,
            month: translator.names("month", 12)?,
            month_short: translator.names("month-short", 12)?,
        };
        let sample = Date::constant(2026, 12, 5);
        translator.t("date-long", Some(&translator.date_args(sample, true)))?;
        translator.t("date-short", Some(&translator.date_args(sample, false)))?;
        Ok(translator)
    }

    fn names(&self, id: &str, count: u8) -> Result<Vec<String>, I18nError> {
        let mut names = Vec::with_capacity(usize::from(count));
        for n in 1..=count {
            let mut args = FluentArgs::new();
            args.set("n", n);
            let name = self.t(id, Some(&args))?;
            if names.contains(&name) {
                return Err(I18nError::DuplicateName { id: id.to_owned() });
            }
            names.push(name);
        }
        Ok(names)
    }

    /// Formats a message. Any Fluent error, such as a missing argument, is returned.
    ///
    /// # Errors
    /// Fails when the message is missing, has no value, or cannot be formatted.
    pub fn t(&self, id: &str, args: Option<&FluentArgs<'_>>) -> Result<String, I18nError> {
        let message = self
            .bundle
            .get_message(id)
            .ok_or_else(|| I18nError::MissingMessage(id.to_owned()))?;
        let pattern = message
            .value()
            .ok_or_else(|| I18nError::NoValue(id.to_owned()))?;
        let mut errors = Vec::new();
        let text = self.bundle.format_pattern(pattern, args, &mut errors);
        if errors.is_empty() {
            Ok(text.into_owned())
        } else {
            Err(I18nError::Format {
                id: id.to_owned(),
                details: join(&errors),
            })
        }
    }

    /// Every message id in the bundle, sorted. Terms (`-name`) are not messages.
    #[must_use]
    pub fn message_ids(&self) -> &[String] {
        &self.ids
    }

    /// `zaterdag 5 december 2026`
    #[must_use]
    pub fn long_date(&self, date: Date) -> String {
        self.checked("date-long", &self.date_args(date, true))
    }

    /// `za 5 dec`
    #[must_use]
    pub fn short_date(&self, date: Date) -> String {
        self.checked("date-short", &self.date_args(date, false))
    }

    fn date_args(&self, date: Date, long: bool) -> FluentArgs<'_> {
        let weekday = usize::from(date.weekday().to_monday_one_offset().unsigned_abs()) - 1;
        let month = usize::from(date.month().unsigned_abs()) - 1;
        let (weekdays, months) = if long {
            (&self.dates.weekday, &self.dates.month)
        } else {
            (&self.dates.weekday_short, &self.dates.month_short)
        };
        let mut args = FluentArgs::new();
        args.set("weekday", weekdays[weekday].as_str());
        args.set("day", date.day().to_string());
        args.set("month", months[month].as_str());
        args.set("year", date.year().to_string());
        args
    }

    /// For messages `from_sources` already formatted with the same argument names.
    fn checked(&self, id: &str, args: &FluentArgs<'_>) -> String {
        self.t(id, Some(args))
            .unwrap_or_else(|err| format!("[{err}]"))
    }
}

fn join<E: std::fmt::Display>(errors: &[E]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Fluent starts a message only at column 0 with `identifier =`; other lines are indented
/// or start with `#`, `-`, `}` or `[`. The caller keeps only ids the bundle knows.
fn message_ids_in(source: &str) -> impl Iterator<Item = String> + '_ {
    source.lines().filter_map(|line| {
        if !line.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return None;
        }
        let end = line
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(line.len());
        line[end..]
            .trim_start()
            .starts_with('=')
            .then(|| line[..end].to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_dates(file: &'static str, source: &'static str) -> Result<Translator, I18nError> {
        Translator::from_sources(LOCALE, &[DATES, (file, source)])
    }

    #[test]
    fn scanner_finds_messages_but_not_terms_comments_or_continuations() {
        let source = "# comment\n-brand = Brood\nhello = Hoi\nlong =\n    regel een\n    other = nee\nwith-attr = Tekst\n    .title = Titel\nsnake_case=x\n";
        let ids: Vec<String> = message_ids_in(source).collect();
        assert_eq!(ids, ["hello", "long", "with-attr", "snake_case"]);
    }

    #[test]
    fn scanner_handles_crlf_and_multiline_selects() {
        let source =
            "a = Een\r\npick = { $n ->\r\n    [one] een\r\n   *[other] veel\r\n}\r\nb = Twee\r\n";
        let ids: Vec<String> = message_ids_in(source).collect();
        assert_eq!(ids, ["a", "pick", "b"]);
        let t = with_dates(
            "x.ftl",
            "a = Een\r\npick = { $n ->\r\n    [one] een\r\n   *[other] veel\r\n}\r\nb = Twee\r\n",
        )
        .unwrap();
        assert_eq!(t.t("b", None).unwrap(), "Twee");
        for id in ["a", "b", "pick"] {
            assert!(t.message_ids().iter().any(|i| i == id), "{id}");
        }
    }

    #[test]
    fn attribute_only_message_has_no_value() {
        let t = with_dates("x.ftl", "button =\n    .title = Opslaan\n").unwrap();
        assert!(t.message_ids().iter().any(|i| i == "button"));
        assert_eq!(
            t.t("button", None),
            Err(I18nError::NoValue("button".into()))
        );
    }

    #[test]
    fn syntax_error_names_the_file() {
        let Err(err) = with_dates("bad.ftl", "= geen id\n") else {
            panic!("expected a parse error");
        };
        assert!(
            matches!(&err, I18nError::Parse { file, .. } if file == "bad.ftl"),
            "{err:?}"
        );
    }

    #[test]
    fn duplicate_message_across_files_is_an_error() {
        let Err(err) = with_dates("b.ftl", "weekday = twee\n") else {
            panic!("expected a duplicate error");
        };
        assert!(
            matches!(&err, I18nError::Bundle { file, .. } if file == "b.ftl"),
            "{err:?}"
        );
    }

    #[test]
    fn invalid_locale_is_an_error() {
        assert!(matches!(
            Translator::from_sources("not a locale!", &[]),
            Err(I18nError::Locale(_))
        ));
    }

    #[test]
    fn missing_date_messages_fail_at_construction() {
        assert_eq!(
            Translator::from_sources(LOCALE, &[COMMON]).err(),
            Some(I18nError::MissingMessage("weekday".into()))
        );
        let without_long = DATES.1.replace("date-long =", "date-lang =");
        let sources = [("dates.ftl", without_long.as_str())];
        assert_eq!(
            Translator::from_sources(LOCALE, &sources).err(),
            Some(I18nError::MissingMessage("date-long".into()))
        );
    }

    #[test]
    fn select_falling_back_to_default_is_caught() {
        let broken = DATES.1.replace("    [3] mrt\n", "");
        let sources = [("dates.ftl", broken.as_str())];
        assert_eq!(
            Translator::from_sources(LOCALE, &sources).err(),
            Some(I18nError::DuplicateName {
                id: "month-short".into()
            })
        );
    }

    #[test]
    fn broken_date_message_fails_at_construction() {
        let broken = DATES.1.replace("{ $year }", "{ $jaar }");
        let sources = [("dates.ftl", broken.as_str())];
        assert!(matches!(
            Translator::from_sources(LOCALE, &sources),
            Err(I18nError::Format { id, .. }) if id == "date-long"
        ));
    }
}
