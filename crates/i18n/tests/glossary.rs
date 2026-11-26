#![forbid(unsafe_code)]

use std::path::Path;

/// (file, message id, reason). Every entry needs a reason a reviewer can check.
const ALLOWED: &[(&str, &str, &str)] = &[];

/// The text of each value line, without its `{ ... }` placeables.
fn values(source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut id = String::new();
    for line in source.lines() {
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            out.push((id.clone(), text(line)));
        } else if let Some((key, value)) = line.split_once('=') {
            key.trim().clone_into(&mut id);
            out.push((id.clone(), text(value)));
        }
    }
    out
}

fn text(line: &str) -> String {
    let mut out = String::new();
    let mut depth = 0_u32;
    for c in line.trim().chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Standalone "drop" or "drops" in any case, unless it follows "Bread ".
fn bare_drop(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(at) = lower[from..].find("drop").map(|i| i + from) {
        let end = at + 4;
        let end = if bytes.get(end) == Some(&b's') {
            end + 1
        } else {
            end
        };
        let before_ok = at == 0
            || !lower[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
        let after_ok = !lower[end..]
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric);
        let named = text[..at].ends_with("Bread ") && text[at..].starts_with("Drop");
        if before_ok && after_ok && !named {
            return true;
        }
        from = end;
    }
    false
}

#[test]
fn bare_drop_spots_the_words_the_glossary_replaced() {
    assert!(bare_drop("De drop van morgen"));
    assert!(bare_drop("Drops"));
    assert!(bare_drop("Drop-alerts"));
    assert!(!bare_drop("De Bread Drop van morgen"));
    assert!(!bare_drop("Alle Bread Drops"));
    assert!(!bare_drop("Bread Drop-alerts"));
    assert!(!bare_drop("dropdown en airdrop"));
    assert!(bare_drop("bread drop"));
    assert!(bare_drop("Bread drop"));
}

#[test]
fn dutch_texts_say_bread_drop_not_bakdag_or_drop() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales/nl");
    let mut wrong = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("locales/nl exists") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "ftl") {
            continue;
        }
        let file = path.file_name().unwrap().to_str().unwrap().to_owned();
        let source = std::fs::read_to_string(&path).unwrap();
        for (id, value) in values(&source) {
            let allowed = ALLOWED.iter().any(|(f, i, _)| *f == file && *i == id);
            if !allowed && (value.to_lowercase().contains("bakdag") || bare_drop(&value)) {
                wrong.push(format!("{file} {id}: {value}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "use \"Bread Drop\" (or \"bakdatum\" for the date):\n{}",
        wrong.join("\n")
    );
}
