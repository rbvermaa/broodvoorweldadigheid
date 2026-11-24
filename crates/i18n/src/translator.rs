use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use unic_langid::LanguageIdentifier;

use crate::I18nError;

pub const LOCALE: &str = "nl-NL";

const DUTCH: &[(&str, &str)] = &[("common.ftl", include_str!("../../../locales/nl/common.ftl"))];

/// Names of the `locales/nl/*.ftl` files compiled into the binary.
pub fn resource_files() -> impl Iterator<Item = &'static str> {
    DUTCH.iter().map(|(file, _)| *file)
}

pub struct Translator {
    bundle: FluentBundle<FluentResource>,
    ids: Vec<String>,
}

impl Translator {
    /// Builds the nl-NL bundle from the Fluent files compiled into the binary.
    ///
    /// # Errors
    /// Fails when a Fluent file has a syntax error or repeats a message id.
    pub fn dutch() -> Result<Self, I18nError> {
        Self::from_sources(LOCALE, DUTCH)
    }

    fn from_sources(locale: &str, sources: &[(&str, &str)]) -> Result<Self, I18nError> {
        let langid: LanguageIdentifier = locale
            .parse()
            .map_err(|_| I18nError::Locale(locale.to_owned()))?;
        let mut bundle = FluentBundle::new_concurrent(vec![langid]);
        // Isolation marks (U+2068/U+2069) only help when mixing text directions; the UI is
        // Dutch only and the marks would leak invisibly into HTML, emails and labels.
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
        Ok(Self { bundle, ids })
    }

    /// Formats a message. Any Fluent error, such as a missing argument, is an error so
    /// tests catch it instead of rendering a half-filled sentence.
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
}

fn join<E: std::fmt::Display>(errors: &[E]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Fluent only starts a message at column 0 with `identifier =`; continuation lines are
/// indented. The caller filters the result through the bundle, so only real messages remain.
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

    #[test]
    fn scanner_finds_messages_but_not_terms_comments_or_continuations() {
        let source = "# comment\n-brand = Brood\nhello = Hoi\nlong =\n    regel een\n    other = nee\nwith-attr = Tekst\n    .title = Titel\nsnake_case=x\n";
        let ids: Vec<String> = message_ids_in(source).collect();
        assert_eq!(ids, ["hello", "long", "with-attr", "snake_case"]);
    }

    #[test]
    fn attribute_only_message_has_no_value() {
        let t = Translator::from_sources(LOCALE, &[("x.ftl", "button =\n    .title = Opslaan\n")])
            .unwrap();
        assert_eq!(t.message_ids(), ["button"]);
        assert_eq!(
            t.t("button", None),
            Err(I18nError::NoValue("button".into()))
        );
    }

    #[test]
    fn syntax_error_names_the_file() {
        let Err(err) = Translator::from_sources(LOCALE, &[("bad.ftl", "= geen id\n")]) else {
            panic!("expected a parse error");
        };
        assert!(
            matches!(&err, I18nError::Parse { file, .. } if file == "bad.ftl"),
            "{err:?}"
        );
    }

    #[test]
    fn duplicate_message_across_files_is_an_error() {
        let Err(err) =
            Translator::from_sources(LOCALE, &[("a.ftl", "x = een\n"), ("b.ftl", "x = twee\n")])
        else {
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
}
