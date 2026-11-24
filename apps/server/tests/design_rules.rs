//! Design-system checks from "Testing and quality": no inline styles or scripts in
//! templates, no literal colours or sizes in app.css, no missing Dutch texts.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use i18n::Translator;
use server::pages::error_messages;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn templates() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = std::fs::read_dir(repo().join("templates"))
        .expect("templates/")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "html"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read_to_string(&path).expect("read template"))
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no templates found");
    files
}

#[test]
fn templates_have_no_inline_styles_or_scripts() {
    for (name, source) in templates() {
        let lower = source.to_ascii_lowercase();
        assert!(!lower.contains("style="), "{name}: style attribute");
        assert!(!lower.contains("<style"), "{name}: <style> element");
        for tag in lower.split("<script").skip(1) {
            let open = tag.split('>').next().unwrap_or_default();
            assert!(open.contains(" src="), "{name}: inline <script>");
        }
        for word in lower.split(|c: char| c.is_whitespace() || c == '<') {
            let handler = word.starts_with("on")
                && word.find('=').is_some_and(|eq| {
                    eq > 2 && word[2..eq].chars().all(|c| c.is_ascii_alphabetic())
                });
            assert!(!handler, "{name}: inline event handler {word}");
        }
    }
}

fn message_ids_in_template(source: &str) -> Vec<String> {
    source
        .split(".t(\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_text_in_templates_and_error_pages_exists_in_fluent() {
    let translator = Translator::dutch().expect("Dutch texts");
    let known = translator.message_ids();
    let mut used: Vec<String> = templates()
        .iter()
        .flat_map(|(_, source)| message_ids_in_template(source))
        .collect();
    for status in [
        StatusCode::FORBIDDEN,
        StatusCode::NOT_FOUND,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let (title, text) = error_messages(status);
        used.extend([title.to_owned(), text.to_owned()]);
    }
    assert!(
        used.iter().any(|id| id == "home-intro"),
        "scanner finds ids"
    );
    for id in used {
        assert!(known.contains(&id), "missing Fluent message {id}");
    }
}

/// Every declaration value is made of `var(--token)`s; the only literal numbers allowed
/// are 0, 1 and 100%.
#[test]
fn app_css_uses_only_tokens() {
    let css = std::fs::read_to_string(repo().join("design/app.css")).expect("design/app.css");
    let mut without_comments = String::new();
    for (index, part) in css.split("/*").enumerate() {
        if index == 0 {
            without_comments.push_str(part);
        } else if let Some((_, rest)) = part.split_once("*/") {
            without_comments.push_str(rest);
        }
    }
    let mut checked = 0;
    for line in without_comments.lines() {
        let Some((_, value)) = line.trim().split_once(':') else {
            continue;
        };
        let Some(value) = value.strip_suffix(';') else {
            continue;
        };
        checked += 1;
        let mut rest = value.to_owned();
        while let Some(start) = rest.find("var(") {
            let end = rest[start..].find(')').expect("closed var(") + start;
            rest.replace_range(start..=end, " ");
        }
        assert!(!rest.contains('#'), "literal colour: {line}");
        for function in ["rgb", "hsl", "oklch", "color("] {
            assert!(!rest.contains(function), "literal colour: {line}");
        }
        for word in rest.split(|c: char| !(c.is_ascii_alphanumeric() || "%.-".contains(c))) {
            if word.chars().any(|c| c.is_ascii_digit()) {
                assert!(
                    ["0", "1", "100%", "-100%"].contains(&word),
                    "literal size {word}: {line}"
                );
            }
        }
    }
    assert!(checked > 10, "found only {checked} declarations");
}

#[test]
fn template_scanner_finds_literal_message_ids() {
    assert_eq!(
        message_ids_in_template(
            "{{ layout.t(\"a-b\")? }} {{ layout.t(x)? }} {{ layout.asset(\"x.css\") }} {{ l.t(\"c\")? }}"
        ),
        ["a-b", "c"]
    );
}
