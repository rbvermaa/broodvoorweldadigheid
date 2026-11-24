//! Turns the classes in a rendered email into style attributes, from `design/email.css`
//! with every `var(--token)` replaced by its value from `design/tokens.toml` (ADR 0014).

use std::collections::BTreeMap;
use std::sync::LazyLock;

const TOKENS_TOML: &str = include_str!("../../../design/tokens.toml");
const EMAIL_CSS: &str = include_str!("../../../design/email.css");

/// Email clients size `rem` inconsistently; the tokens assume a 16px root.
const PX_PER_REM: f64 = 16.0;

/// The generic font families that `apps/server/src/tokens.rs` leaves unquoted.
const GENERIC_FAMILIES: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "emoji",
    "math",
    "fangsong",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StyleError {
    #[error("design/tokens.toml: {0}")]
    Tokens(String),
    #[error("design/email.css: {0}")]
    Css(String),
    #[error("class {0} is not in design/email.css")]
    UnknownClass(String),
}

/// Every token as the CSS variable `apps/server` generates: `--color-black` → `#000000`.
///
/// # Errors
/// [`StyleError::Tokens`] when the file does not parse.
pub fn token_values() -> Result<BTreeMap<String, String>, StyleError> {
    let table: toml::Table =
        toml::from_str(TOKENS_TOML).map_err(|error| StyleError::Tokens(error.to_string()))?;
    let mut values = BTreeMap::new();
    for (section, entries) in &table {
        let Some(entries) = entries.as_table() else {
            continue;
        };
        for (name, value) in entries {
            let css = match value {
                toml::Value::String(text) => text.clone(),
                toml::Value::Integer(number) => number.to_string(),
                toml::Value::Array(families) => families
                    .iter()
                    .map(|family| {
                        let family = family
                            .as_str()
                            .ok_or_else(|| StyleError::Tokens(format!("[{section}] {name}")))?;
                        Ok(if GENERIC_FAMILIES.contains(&family) {
                            family.to_owned()
                        } else {
                            format!("\"{family}\"")
                        })
                    })
                    .collect::<Result<Vec<_>, StyleError>>()?
                    .join(", "),
                _ => return Err(StyleError::Tokens(format!("[{section}] {name}"))),
            };
            values.insert(format!("--{section}-{name}"), css);
        }
    }
    Ok(values)
}

/// Class name → the declarations for its style attribute.
type Rules = BTreeMap<String, String>;

static RULES: LazyLock<Result<Rules, StyleError>> = LazyLock::new(|| {
    let tokens = token_values()?;
    parse_rules(EMAIL_CSS, &tokens)
});

fn parse_rules(css: &str, tokens: &BTreeMap<String, String>) -> Result<Rules, StyleError> {
    let mut text = String::new();
    for (index, part) in css.split("/*").enumerate() {
        if index == 0 {
            text.push_str(part);
        } else {
            let (_, rest) = part
                .split_once("*/")
                .ok_or_else(|| StyleError::Css("unclosed comment".to_owned()))?;
            text.push_str(rest);
        }
    }
    let mut rules = Rules::new();
    let mut rest = text.as_str();
    while let Some(open) = rest.find('{') {
        let selector = rest[..open].trim();
        let close = rest[open..]
            .find('}')
            .map(|close| open + close)
            .ok_or_else(|| StyleError::Css(format!("{selector}: no closing brace")))?;
        let mut style = String::new();
        for declaration in rest[open + 1..close].split(';') {
            let declaration = declaration.trim();
            if declaration.is_empty() {
                continue;
            }
            let (property, value) = declaration
                .split_once(':')
                .ok_or_else(|| StyleError::Css(format!("not a declaration: {declaration}")))?;
            let value = resolve(value.trim(), tokens)?;
            style.push_str(&format!("{}: {value}; ", property.trim()));
        }
        for class in selector.split(',') {
            let class = class.trim();
            let name = class
                .strip_prefix('.')
                .filter(|name| {
                    !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                })
                .ok_or_else(|| StyleError::Css(format!("not a single class: {class}")))?;
            rules.entry(name.to_owned()).or_default().push_str(&style);
        }
        rest = &rest[close + 1..];
    }
    if !rest.trim().is_empty() {
        return Err(StyleError::Css(format!(
            "text outside a rule: {}",
            rest.trim()
        )));
    }
    Ok(rules
        .into_iter()
        .map(|(class, style)| (class, style.trim_end().to_owned()))
        .collect())
}

/// Replaces each `var(--name)` with the token value, `rem` with `px`, and double quotes
/// with single ones so the value fits in a double-quoted attribute.
fn resolve(value: &str, tokens: &BTreeMap<String, String>) -> Result<String, StyleError> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("var(") {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find(')')
            .map(|end| start + end)
            .ok_or_else(|| StyleError::Css(format!("unclosed var(): {value}")))?;
        let name = rest[start + 4..end].trim();
        let token = tokens
            .get(name)
            .ok_or_else(|| StyleError::Css(format!("{name} is not a token")))?;
        out.push_str(token);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    let words: Vec<String> = out
        .split(' ')
        .map(|word| {
            word.strip_suffix("rem")
                .and_then(|number| number.parse::<f64>().ok())
                .map_or_else(|| word.to_owned(), |rem| format!("{}px", rem * PX_PER_REM))
        })
        .collect();
    Ok(words.join(" ").replace('"', "'"))
}

/// Swaps every `class="…"` in rendered HTML for the matching `style="…"`.
///
/// Askama escapes `"` in all text it inserts, so `class="` only occurs where a template
/// wrote it.
pub(crate) fn inline(html: &str) -> Result<String, StyleError> {
    let rules = RULES.as_ref().map_err(Clone::clone)?;
    inline_with(html, rules)
}

fn inline_with(html: &str, rules: &Rules) -> Result<String, StyleError> {
    const ATTRIBUTE: &str = " class=\"";
    let mut out = String::with_capacity(html.len() * 2);
    let mut rest = html;
    while let Some(start) = rest.find(ATTRIBUTE) {
        out.push_str(&rest[..start]);
        let value_start = start + ATTRIBUTE.len();
        let end = rest[value_start..]
            .find('"')
            .map(|end| value_start + end)
            .ok_or_else(|| StyleError::UnknownClass(rest[value_start..].to_owned()))?;
        let styles = rest[value_start..end]
            .split_whitespace()
            .map(|class| {
                rules
                    .get(class)
                    .map(String::as_str)
                    .ok_or_else(|| StyleError::UnknownClass(class.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        out.push_str(" style=\"");
        out.push_str(&styles.join(" "));
        out.push('"');
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> BTreeMap<String, String> {
        token_values().expect("tokens")
    }

    #[test]
    fn tokens_read_like_the_generated_css_variables() {
        let tokens = tokens();
        assert_eq!(tokens["--color-black"], "#000000");
        assert_eq!(tokens["--space-4"], "1rem");
        assert_eq!(tokens["--font-weight-bold"], "700");
        assert!(tokens["--font-body"].starts_with("\"Inter\", system-ui,"));
        assert!(!tokens.keys().any(|name| name.starts_with("--pair")));
    }

    #[test]
    fn email_css_parses_and_resolves_every_token() {
        let rules = RULES.as_ref().expect("rules");
        let body = &rules["email-body"];
        assert!(body.contains("background: #f5f5f5;"), "{body}");
        assert!(body.contains("font-size: 16px;"), "{body}");
        assert!(body.contains("font-family: 'Inter', system-ui"), "{body}");
        for style in rules.values() {
            assert!(!style.contains("var("), "{style}");
            assert!(!style.contains("rem"), "{style}");
            assert!(!style.contains('"'), "{style}");
        }
    }

    #[test]
    fn rem_becomes_px_and_quotes_become_single() {
        let tokens = tokens();
        assert_eq!(
            resolve("var(--space-2) var(--space-5) 0", &tokens),
            Ok("8px 24px 0".to_owned())
        );
        assert_eq!(
            resolve("var(--font-size-sm)", &tokens),
            Ok("14px".to_owned())
        );
        assert_eq!(
            resolve("var(--nope)", &tokens),
            Err(StyleError::Css("--nope is not a token".to_owned()))
        );
        assert!(resolve("var(--space-2", &tokens).is_err());
    }

    #[test]
    fn only_single_class_rules_are_accepted() {
        let tokens = tokens();
        for bad in [
            "p { margin: 0; }",
            ".a .b { margin: 0; }",
            ".a:hover { margin: 0; }",
            ".a { margin }",
            ".a { margin: 0;",
            "/* open",
            ".a { margin: 0; } stray",
            ". { margin: 0; }",
        ] {
            assert!(parse_rules(bad, &tokens).is_err(), "{bad}");
        }
        let rules = parse_rules(
            "/* x */ .a, .b { margin: 0; } .a { padding: var(--space-1); }",
            &tokens,
        )
        .expect("rules");
        assert_eq!(rules["a"], "margin: 0; padding: 4px;");
        assert_eq!(rules["b"], "margin: 0;");
    }

    #[test]
    fn classes_become_style_attributes() {
        let rules: Rules = [("a", "margin: 0;"), ("b", "color: #000000;")]
            .into_iter()
            .map(|(class, style)| (class.to_owned(), style.to_owned()))
            .collect();
        assert_eq!(
            inline_with("<p class=\"a b\">x</p><i>class=\"\"</i>", &rules),
            Ok("<p style=\"margin: 0; color: #000000;\">x</p><i>class=\"\"</i>".to_owned())
        );
        assert_eq!(
            inline_with("<p class=\"c\">", &rules),
            Err(StyleError::UnknownClass("c".to_owned()))
        );
        assert!(inline_with("<p class=\"a", &rules).is_err());
    }
}
