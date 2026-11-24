//! Design-system checks from "Testing and quality": no inline styles or scripts in
//! templates; no colours, sizes or classes outside the tokens and components; no missing
//! or unused Dutch texts.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use i18n::Translator;
use server::icons::ALLERGEN_ICONS;
use server::pages::error_messages;

/// Fluent ids that no code names literally. `allergen-`: `Layout::allergen` builds
/// `allergen-{code}`; checked against design/icons.toml below.
const DYNAMIC_PREFIXES: [&str; 1] = ["allergen-"];
/// Messages kept for the checkout slice (F4); the i18n tests use them already.
const RESERVED: [&str; 3] = ["order-thanks", "order-deadline", "pickup-moment"];

/// Unitless numbers and percentages that are not sizes.
const ALLOWED_NUMBERS: [&str; 5] = ["0", "1", "50%", "100%", "-100%"];

/// The CSS named colours (CSS Color 4). `currentColor` and `transparent` are allowed.
const NAMED_COLOURS: &[&str] = &[
    "aliceblue",
    "antiquewhite",
    "aqua",
    "aquamarine",
    "azure",
    "beige",
    "bisque",
    "black",
    "blanchedalmond",
    "blue",
    "blueviolet",
    "brown",
    "burlywood",
    "cadetblue",
    "chartreuse",
    "chocolate",
    "coral",
    "cornflowerblue",
    "cornsilk",
    "crimson",
    "cyan",
    "darkblue",
    "darkcyan",
    "darkgoldenrod",
    "darkgray",
    "darkgreen",
    "darkgrey",
    "darkkhaki",
    "darkmagenta",
    "darkolivegreen",
    "darkorange",
    "darkorchid",
    "darkred",
    "darksalmon",
    "darkseagreen",
    "darkslateblue",
    "darkslategray",
    "darkslategrey",
    "darkturquoise",
    "darkviolet",
    "deeppink",
    "deepskyblue",
    "dimgray",
    "dimgrey",
    "dodgerblue",
    "firebrick",
    "floralwhite",
    "forestgreen",
    "fuchsia",
    "gainsboro",
    "ghostwhite",
    "gold",
    "goldenrod",
    "gray",
    "green",
    "greenyellow",
    "grey",
    "honeydew",
    "hotpink",
    "indianred",
    "indigo",
    "ivory",
    "khaki",
    "lavender",
    "lavenderblush",
    "lawngreen",
    "lemonchiffon",
    "lightblue",
    "lightcoral",
    "lightcyan",
    "lightgoldenrodyellow",
    "lightgray",
    "lightgreen",
    "lightgrey",
    "lightpink",
    "lightsalmon",
    "lightseagreen",
    "lightskyblue",
    "lightslategray",
    "lightslategrey",
    "lightsteelblue",
    "lightyellow",
    "lime",
    "limegreen",
    "linen",
    "magenta",
    "maroon",
    "mediumaquamarine",
    "mediumblue",
    "mediumorchid",
    "mediumpurple",
    "mediumseagreen",
    "mediumslateblue",
    "mediumspringgreen",
    "mediumturquoise",
    "mediumvioletred",
    "midnightblue",
    "mintcream",
    "mistyrose",
    "moccasin",
    "navajowhite",
    "navy",
    "oldlace",
    "olive",
    "olivedrab",
    "orange",
    "orangered",
    "orchid",
    "palegoldenrod",
    "palegreen",
    "paleturquoise",
    "palevioletred",
    "papayawhip",
    "peachpuff",
    "peru",
    "pink",
    "plum",
    "powderblue",
    "purple",
    "rebeccapurple",
    "red",
    "rosybrown",
    "royalblue",
    "saddlebrown",
    "salmon",
    "sandybrown",
    "seagreen",
    "seashell",
    "sienna",
    "silver",
    "skyblue",
    "slateblue",
    "slategray",
    "slategrey",
    "snow",
    "springgreen",
    "steelblue",
    "tan",
    "teal",
    "thistle",
    "tomato",
    "turquoise",
    "violet",
    "wheat",
    "white",
    "whitesmoke",
    "yellow",
    "yellowgreen",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files_under(dir: &Path, ext: &str, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.expect("entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().expect("name").to_string_lossy();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                files_under(&path, ext, out);
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            let relative = path
                .strip_prefix(repo())
                .unwrap_or(&path)
                .display()
                .to_string();
            out.push((relative, std::fs::read_to_string(&path).expect("read")));
        }
    }
}

fn templates() -> Vec<(String, String)> {
    let mut files = Vec::new();
    files_under(&repo().join("templates"), "html", &mut files);
    assert!(files.len() > 5, "templates not found");
    files
}

fn stylesheets() -> Vec<(String, String)> {
    let mut files = Vec::new();
    files_under(&repo().join("design"), "css", &mut files);
    assert!(files.len() >= 3, "stylesheets not found");
    files
}

/// The `src` directories of every crate and app: code that ships.
fn shipped_rust() -> Vec<(String, String)> {
    let mut files = Vec::new();
    for group in ["apps", "crates"] {
        for entry in std::fs::read_dir(repo().join(group)).expect("dir") {
            let src = entry.expect("entry").path().join("src");
            if src.is_dir() {
                files_under(&src, "rs", &mut files);
            }
        }
    }
    files
}

/// The server names its Fluent ids itself; crates/i18n tests use their own fixtures.
fn server_rust() -> Vec<(String, String)> {
    let mut files = Vec::new();
    files_under(&repo().join("apps/server/src"), "rs", &mut files);
    files
}

fn breakpoints() -> Vec<String> {
    let source = std::fs::read_to_string(repo().join("design/tokens.toml")).expect("read");
    let tokens: toml::Table = toml::from_str(&source).expect("tokens.toml");
    tokens["breakpoint"]
        .as_table()
        .expect("[breakpoint]")
        .values()
        .map(|value| value.as_str().expect("string").to_owned())
        .collect()
}

fn template_problems(source: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let lower = source.to_ascii_lowercase();
    if lower.contains("style=") {
        problems.push("style attribute".to_owned());
    }
    if lower.contains("<style") {
        problems.push("<style> element".to_owned());
    }
    if lower.contains("|safe") || lower.contains("| safe") {
        problems.push("|safe filter".to_owned());
    }
    for tag in lower.split("<script").skip(1) {
        let open = tag.split('>').next().unwrap_or_default();
        if !open.contains(" src=") {
            problems.push("inline <script>".to_owned());
        }
    }
    for word in lower.split(|c: char| c.is_whitespace() || c == '<') {
        let handler = word.starts_with("on")
            && word
                .find('=')
                .is_some_and(|eq| eq > 2 && word[2..eq].chars().all(|c| c.is_ascii_alphabetic()));
        if handler {
            problems.push(format!("inline event handler {word}"));
        }
    }
    problems
}

/// Splits CSS into `(selector or at-rule, declarations)`, without comments.
fn css_rules(css: &str) -> Vec<(String, Vec<String>)> {
    let mut text = String::new();
    for (index, part) in css.split("/*").enumerate() {
        if index == 0 {
            text.push_str(part);
        } else if let Some((_, rest)) = part.split_once("*/") {
            text.push_str(rest);
        }
    }
    let mut rules: Vec<(String, Vec<String>)> = Vec::new();
    let mut current = String::new();
    let mut open: Vec<usize> = Vec::new();
    for c in text.chars() {
        match c {
            '{' => {
                rules.push((current.trim().to_owned(), Vec::new()));
                open.push(rules.len() - 1);
                current.clear();
            }
            ';' | '}' => {
                let declaration = current.trim().to_owned();
                if let (false, Some(&rule)) = (declaration.is_empty(), open.last()) {
                    rules[rule].1.push(declaration);
                }
                current.clear();
                if c == '}' {
                    open.pop();
                }
            }
            c => current.push(c),
        }
    }
    rules
}

fn strip_vars(value: &str) -> String {
    let mut rest = value.to_owned();
    while let Some(start) = rest.find("var(") {
        let mut depth = 0;
        let mut end = rest.len() - 1;
        for (offset, c) in rest[start..].char_indices() {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
                if depth == 0 {
                    end = start + offset;
                    break;
                }
            }
        }
        rest.replace_range(start..=end, " ");
    }
    rest
}

fn value_problems(declaration: &str, value: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let rest = strip_vars(value).to_ascii_lowercase();
    if rest.contains('#') {
        problems.push(format!("literal colour: {declaration}"));
    }
    for function in [
        "rgb", "hsl", "hwb", "lab(", "lch(", "oklab", "oklch", "color(",
    ] {
        if rest.contains(function) {
            problems.push(format!("literal colour: {declaration}"));
        }
    }
    for word in rest.split(|c: char| !(c.is_ascii_alphanumeric() || "%.-".contains(c))) {
        if NAMED_COLOURS.contains(&word) {
            problems.push(format!("named colour {word}: {declaration}"));
        }
        if word.chars().any(|c| c.is_ascii_digit()) && !ALLOWED_NUMBERS.contains(&word) {
            problems.push(format!("literal size {word}: {declaration}"));
        }
    }
    problems
}

fn css_problems(css: &str, breakpoints: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    for (selector, declarations) in css_rules(css) {
        if let Some(query) = selector.strip_prefix("@media") {
            for word in query.split(|c: char| c.is_whitespace() || "():,".contains(c)) {
                let digits = word.chars().any(|c| c.is_ascii_digit());
                if digits && !breakpoints.iter().any(|b| b == word) {
                    problems.push(format!("{word} in {selector:?} is not a breakpoint token"));
                }
            }
        } else if selector.starts_with('@') && selector != "@font-face" {
            problems.push(format!("unexpected at-rule {selector}"));
        }
        for declaration in declarations {
            let Some((property, value)) = declaration.split_once(':') else {
                problems.push(format!("not a declaration: {declaration}"));
                continue;
            };
            let property = property.trim();
            if property.starts_with("--") {
                problems.push(format!("custom property outside tokens: {declaration}"));
            } else if !(selector == "@font-face" && ["src", "font-weight"].contains(&property)) {
                problems.extend(value_problems(&declaration, value));
            }
        }
    }
    problems
}

fn css_classes(css: &str) -> BTreeSet<String> {
    let mut classes = BTreeSet::new();
    for (selector, _) in css_rules(css) {
        if selector.starts_with('@') {
            continue;
        }
        for part in selector.split('.').skip(1) {
            let name: String = part
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !name.is_empty() {
                classes.insert(name);
            }
        }
    }
    classes
}

/// Class names in `class="…"` attributes. Askama `{% %}` tags are dropped, so the literal
/// names in every branch count; a `{{ }}` expression in a class is an error.
fn template_classes(source: &str) -> Result<Vec<String>, String> {
    let mut classes = Vec::new();
    for part in source.split(" class=\"").skip(1) {
        let mut value = part.split('"').next().unwrap_or_default().to_owned();
        if value.contains("{{") {
            return Err(format!("class built from an expression: {value}"));
        }
        while let Some(start) = value.find("{%") {
            let end = value[start..]
                .find("%}")
                .map_or(value.len(), |end| start + end + 2);
            value.replace_range(start..end, " ");
        }
        classes.extend(value.split_whitespace().map(str::to_owned));
    }
    Ok(classes)
}

/// Ids passed literally to `t("…")` or `t_count("…")`.
fn message_ids_used(source: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for call in ["t(\"", "t_count(\""] {
        for (at, _) in source.match_indices(call) {
            let before = source[..at].chars().next_back();
            if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            if let Some(id) = source[at + call.len()..].split('"').next() {
                ids.push(id.to_owned());
            }
        }
    }
    ids
}

fn string_literals(source: &str) -> BTreeSet<&str> {
    source.split('"').skip(1).step_by(2).collect()
}

#[test]
fn templates_have_no_inline_styles_or_scripts() {
    for (name, source) in templates() {
        let problems = template_problems(&source);
        assert!(problems.is_empty(), "{name}: {problems:?}");
    }
}

#[test]
fn stylesheets_use_only_tokens() {
    let breakpoints = breakpoints();
    let mut declarations = 0;
    for (name, css) in stylesheets() {
        declarations += css_rules(&css).iter().map(|(_, d)| d.len()).sum::<usize>();
        let problems = css_problems(&css, &breakpoints);
        assert!(problems.is_empty(), "{name}:\n{}", problems.join("\n"));
    }
    assert!(declarations > 100, "found only {declarations} declarations");
}

#[test]
fn every_class_in_templates_is_defined_in_our_css() {
    let defined: BTreeSet<String> = stylesheets()
        .iter()
        .flat_map(|(_, css)| css_classes(css))
        .collect();
    assert!(defined.contains("button-primary"), "scanner finds classes");
    assert!(defined.contains("icon"), "Icon renders class=\"icon\"");
    for (name, source) in templates() {
        let classes = template_classes(&source).unwrap_or_else(|error| panic!("{name}: {error}"));
        for class in classes {
            assert!(
                defined.contains(&class),
                "{name}: class {class} is not in design/*.css"
            );
        }
    }
}

#[test]
fn every_text_in_templates_and_code_exists_in_fluent() {
    let translator = Translator::dutch().expect("Dutch texts");
    let known = translator.message_ids();
    let mut used: Vec<(String, String)> = Vec::new();
    for (name, source) in templates().into_iter().chain(server_rust()) {
        used.extend(
            message_ids_used(&source)
                .into_iter()
                .map(|id| (name.clone(), id)),
        );
    }
    for status in [
        StatusCode::FORBIDDEN,
        StatusCode::NOT_FOUND,
        StatusCode::METHOD_NOT_ALLOWED,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let (title, text) = error_messages(status);
        used.push(("error_messages".to_owned(), title.to_owned()));
        used.push(("error_messages".to_owned(), text.to_owned()));
    }
    assert!(
        used.iter().any(|(_, id)| id == "home-intro"),
        "scanner reads templates"
    );
    assert!(
        used.iter().any(|(_, id)| id == "gallery-long-word"),
        "scanner reads Rust"
    );
    for (name, id) in used {
        assert!(known.contains(&id), "{name}: missing Fluent message {id}");
    }
    for (code, _) in ALLERGEN_ICONS {
        let id = format!("allergen-{code}");
        assert!(known.contains(&id), "missing Fluent message {id}");
    }
}

#[test]
fn every_fluent_message_is_used() {
    let translator = Translator::dutch().expect("Dutch texts");
    let sources: Vec<(String, String)> = templates().into_iter().chain(shipped_rust()).collect();
    let literals: BTreeSet<&str> = sources
        .iter()
        .flat_map(|(_, source)| string_literals(source))
        .collect();
    for id in translator.message_ids() {
        let dynamic = DYNAMIC_PREFIXES.iter().any(|prefix| id.starts_with(prefix));
        let used = literals.contains(id.as_str()) || RESERVED.contains(&id.as_str());
        assert!(dynamic || used, "Fluent message {id} is never used");
    }
    for id in RESERVED {
        assert!(
            translator.message_ids().iter().any(|known| known == id),
            "{id}"
        );
    }
}

#[test]
fn template_checker_catches_planted_violations() {
    for bad in [
        "<p style=\"color: red\">x</p>",
        "<STYLE>p{}</STYLE>",
        "<script>alert(1)</script>",
        "<a href=\"#\" onclick=\"x()\">x</a>",
        "{{ text|safe }}",
    ] {
        assert!(!template_problems(bad).is_empty(), "{bad}");
    }
    let good = "<script src=\"/x.js\" defer></script><p class=\"a\">on</p>";
    assert!(template_problems(good).is_empty());
}

#[test]
fn css_checker_catches_planted_violations() {
    let breakpoints = vec!["36rem".to_owned()];
    for bad in [
        ".a { color: #fff; }",
        ".a { color: rgb(0 0 0); }",
        ".a { color: red; }",
        ".a { background: var(--color-black) papayawhip; }",
        ".a { padding: 12px; }",
        ".a { margin: 0 1.5rem; }",
        ".a { width: calc(var(--space-1) * 3); }",
        ".a { --my-colour: var(--color-black); }",
        "@media (min-width: 40rem) { .a { color: var(--color-text); } }",
        "@keyframes spin { to { transform: rotate(1turn); } }",
        "/* fine */ .a { color: var(--color-text) } .b { font-size: 2em }",
        "@font-face { font-family: \"X\"; font-display: 2s; }",
    ] {
        assert!(!css_problems(bad, &breakpoints).is_empty(), "{bad}");
    }
    let good = "/* #fff 12px red */ .a { color: var(--color-text); flex: 1 1 var(--size-tile); \
        clip-path: inset(50%); border-left: var(--space-1) solid currentColor; }\n\
        @media (min-width: 36rem) { .a:hover { margin: 0 var(--space-2); } }\n\
        @font-face { font-family: \"Inter\"; src: url(\"fonts/inter.woff2\") format(\"woff2\"); \
        font-weight: 100 900; }";
    assert_eq!(css_problems(good, &breakpoints), Vec::<String>::new());
}

#[test]
fn class_checker_reads_branches_and_refuses_expressions() {
    assert_eq!(
        template_classes(
            "<p class=\"a {% if x %}b{% else %}c{% endif %}\"></p><b class=\"d\"></b>"
        )
        .expect("classes"),
        ["a", "b", "c", "d"]
    );
    assert!(template_classes("<p class=\"button-{{ variant }}\"></p>").is_err());
    let classes =
        css_classes(".a, .b:hover > .c-d { x: y; } @media (min-width: 1rem) { .e { z: w; } }");
    assert_eq!(
        classes.into_iter().collect::<Vec<_>>(),
        ["a", "b", "c-d", "e"]
    );
}

#[test]
fn message_scanner_finds_literal_ids() {
    assert_eq!(
        message_ids_used(
            "{{ layout.t(\"a-b\")? }} {{ layout.t(x)? }} {{ layout.asset(\"x.css\") }} \
             i18n.t(\"c\", None) t_count(\"d\", n) format!(\"e\") assert(\"f\")"
        ),
        ["a-b", "c", "d"]
    );
}
