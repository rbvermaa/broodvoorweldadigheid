//! Lucide icons and the allergen icon set from design/icons.toml, rendered as inline SVG.

use std::fmt;

include!(concat!(env!("OUT_DIR"), "/icons.rs"));

/// An inline `<svg>`. Without a label it is hidden from screen readers, so use one only
/// next to visible text that says the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    shapes: &'static str,
    label: Option<String>,
}

/// The SVG is built from our own icon set and the label is escaped in `Display`.
impl askama::filters::HtmlSafe for Icon {}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown icon {0}; add it to design/icons.toml")]
pub struct UnknownIcon(pub String);

impl Icon {
    pub fn named(name: &str) -> Result<Self, UnknownIcon> {
        ICONS
            .iter()
            .find(|(icon, _)| *icon == name)
            .map(|(_, shapes)| Self {
                shapes,
                label: None,
            })
            .ok_or_else(|| UnknownIcon(name.to_owned()))
    }

    pub fn allergen(code: &str) -> Result<Self, UnknownIcon> {
        ALLERGEN_ICONS
            .iter()
            .find(|(allergen, _)| *allergen == code)
            .ok_or_else(|| UnknownIcon(code.to_owned()))
            .and_then(|(_, icon)| Self::named(icon))
    }

    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl fmt::Display for Icon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "<svg class=\"icon\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" \
             stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" focusable=\"false\"",
        )?;
        match &self.label {
            Some(label) => {
                f.write_str(" role=\"img\" aria-label=\"")?;
                for c in label.chars() {
                    match c {
                        '&' => f.write_str("&amp;")?,
                        '<' => f.write_str("&lt;")?,
                        '>' => f.write_str("&gt;")?,
                        '"' => f.write_str("&quot;")?,
                        '\'' => f.write_str("&#39;")?,
                        c => fmt::Write::write_char(f, c)?,
                    }
                }
                f.write_str("\">")?;
            }
            None => f.write_str(" aria-hidden=\"true\">")?,
        }
        f.write_str(self.shapes)?;
        f.write_str("</svg>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icon_set::ALLERGEN_CODES;

    #[test]
    fn every_allergen_has_an_icon() {
        for code in ALLERGEN_CODES {
            assert!(Icon::allergen(code).is_ok(), "{code}");
        }
        assert_eq!(ALLERGEN_ICONS.len(), 14);
    }

    #[test]
    fn decorative_icons_are_hidden_and_labels_are_escaped() {
        let plain = Icon::named("check").expect("check").to_string();
        assert!(plain.contains("aria-hidden=\"true\""), "{plain}");
        assert!(!plain.contains("aria-label"), "{plain}");
        let labelled = Icon::named("check")
            .expect("check")
            .labelled("a\"<b>&'")
            .to_string();
        assert!(
            labelled.contains("role=\"img\" aria-label=\"a&quot;&lt;b&gt;&amp;&#39;\">"),
            "{labelled}"
        );
    }

    #[test]
    fn unknown_icons_are_an_error() {
        assert_eq!(Icon::named("nope"), Err(UnknownIcon("nope".to_string())));
        assert!(Icon::allergen("gluten").is_err());
    }
}
