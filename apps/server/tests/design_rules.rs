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

/// Lowercase, without whitespace around `=`, so `style = "…"` reads as `style="…"`.
fn compact(source: &str) -> String {
    tight(&source.to_ascii_lowercase())
}

/// Blanks out askama tags of the given kinds.
fn without_askama(source: &str, kinds: &[(&str, &str)]) -> String {
    let mut text = source.to_owned();
    for (open, close) in kinds {
        while let Some(start) = text.find(open) {
            let end = text[start..]
                .find(close)
                .map_or(text.len(), |end| start + end + close.len());
            text.replace_range(start..end, " ");
        }
    }
    text
}

/// Collapses whitespace and drops it around `=`.
fn tight(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            while chars.peek().is_some_and(|next| next.is_whitespace()) {
                chars.next();
            }
            if chars.peek() == Some(&'=') || out.ends_with('=') {
                continue;
            }
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

fn template_problems(source: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let text = compact(source);
    for (needle, problem) in [
        ("style=", "style attribute"),
        ("<style", "<style> element"),
        ("hx-on", "inline htmx handler"),
        ("javascript:", "javascript: URL"),
        (" class='", "class must use double quotes"),
    ] {
        if text.contains(needle) {
            problems.push(problem.to_owned());
        }
    }
    let dense: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    for (needle, problem) in [
        ("|safe", "|safe filter"),
        ("filtersafe", "safe filter block"),
        ("|escape(\"none\")", "escaping turned off"),
        ("|e(\"none\")", "escaping turned off"),
    ] {
        if dense.contains(needle) {
            problems.push(problem.to_owned());
        }
    }
    for (at, _) in text.match_indices(" class=") {
        if !text[at + " class=".len()..].starts_with(['"', '\'']) {
            problems.push("unquoted class".to_owned());
        }
    }
    for tag in text.split("<script").skip(1) {
        let open = tag.split('>').next().unwrap_or_default();
        if !open.contains(" src=") {
            problems.push("inline <script>".to_owned());
        }
    }
    let markup = compact(&without_askama(
        source,
        &[("{#", "#}"), ("{%", "%}"), ("{{", "}}")],
    ));
    for word in markup.split(|c: char| c.is_whitespace() || c == '<' || c == '"' || c == '\'') {
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

/// What stylesheets may refer to: the breakpoint values and the token variables.
struct Tokens {
    breakpoints: Vec<String>,
    variables: BTreeSet<String>,
}

fn tokens() -> Tokens {
    let variables = server::assets::TOKENS_CSS
        .lines()
        .filter_map(|line| line.trim().split_once(':'))
        .map(|(name, _)| name.to_owned())
        .filter(|name| name.starts_with("--"))
        .collect();
    Tokens {
        breakpoints: breakpoints(),
        variables,
    }
}

/// Splits CSS into `(selector or at-rule, declarations)`, without comments. Statements
/// outside any rule land under the selector `""`.
fn css_rules(css: &str) -> Vec<(String, Vec<String>)> {
    let mut text = String::new();
    for (index, part) in css.split("/*").enumerate() {
        if index == 0 {
            text.push_str(part);
        } else if let Some((_, rest)) = part.split_once("*/") {
            text.push_str(rest);
        }
    }
    let mut rules: Vec<(String, Vec<String>)> = vec![(String::new(), Vec::new())];
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
                if !declaration.is_empty() {
                    rules[open.last().copied().unwrap_or(0)].1.push(declaration);
                }
                current.clear();
                if c == '}' {
                    open.pop();
                }
            }
            c => current.push(c),
        }
    }
    let rest = current.trim().to_owned();
    if !rest.is_empty() {
        rules[0].1.push(rest);
    }
    rules
}

/// Replaces each `var(--name)` with a space. A name that is not a token, or a fallback
/// value, is a problem.
fn strip_vars(declaration: &str, value: &str, tokens: &Tokens) -> (String, Vec<String>) {
    let mut problems = Vec::new();
    let mut rest = value.to_owned();
    while let Some(start) = rest.find("var(") {
        let end = rest[start..]
            .find(')')
            .map_or(rest.len() - 1, |end| start + end);
        let inner = rest[start + 4..end].trim();
        if inner.contains(',') || inner.contains('(') {
            problems.push(format!("var() with a fallback: {declaration}"));
        } else if !tokens.variables.contains(inner) {
            problems.push(format!("{inner} is not a token: {declaration}"));
        }
        rest.replace_range(start..=end, " ");
    }
    (rest, problems)
}

/// CSS system colours (CSS Color 4) and absolute or relative size keywords.
const KEYWORDS: &[&str] = &[
    "accentcolor",
    "accentcolortext",
    "activetext",
    "buttonborder",
    "buttonface",
    "buttontext",
    "canvas",
    "canvastext",
    "field",
    "fieldtext",
    "graytext",
    "highlight",
    "highlighttext",
    "linktext",
    "mark",
    "marktext",
    "selecteditem",
    "selecteditemtext",
    "visitedtext",
    "xx-small",
    "x-small",
    "small",
    "medium",
    "large",
    "x-large",
    "xx-large",
    "xxx-large",
    "smaller",
    "larger",
    "thin",
    "thick",
];

fn value_problems(
    declaration: &str,
    property: &str,
    value: &str,
    in_font_face: bool,
    tokens: &Tokens,
) -> Vec<String> {
    let (rest, mut problems) = strip_vars(declaration, value, tokens);
    let rest = rest.to_ascii_lowercase();
    if rest.contains('#') {
        problems.push(format!("literal colour: {declaration}"));
    }
    for function in [
        "rgb",
        "hsl",
        "hwb",
        "lab(",
        "lch(",
        "oklab",
        "oklch",
        "color(",
        "color-mix",
        "light-dark",
    ] {
        if rest.contains(function) {
            problems.push(format!("literal colour: {declaration}"));
        }
    }
    if rest.contains("url(") {
        problems.push(format!("url() outside @font-face: {declaration}"));
    }
    if property == "font-family" && !in_font_face {
        if !rest.trim().is_empty() {
            problems.push(format!("font outside the font tokens: {declaration}"));
        }
    } else if (rest.contains('"') || rest.contains('\'')) && !in_font_face {
        problems.push(format!("text in CSS belongs in Fluent: {declaration}"));
    }
    let weight = property == "font-weight";
    for word in rest.split(|c: char| !(c.is_ascii_alphanumeric() || "%.-".contains(c))) {
        if NAMED_COLOURS.contains(&word) {
            problems.push(format!("named colour {word}: {declaration}"));
        }
        if KEYWORDS.contains(&word) && !(weight && word == "medium") {
            problems.push(format!("keyword {word} outside tokens: {declaration}"));
        }
        if word.chars().any(|c| c.is_ascii_digit()) && !ALLOWED_NUMBERS.contains(&word) {
            problems.push(format!("literal size {word}: {declaration}"));
        }
    }
    problems
}

fn css_problems(css: &str, tokens: &Tokens) -> Vec<String> {
    let mut problems = Vec::new();
    for (selector, declarations) in css_rules(css) {
        if selector.is_empty() {
            problems.extend(
                declarations
                    .iter()
                    .map(|statement| format!("statement outside a rule: {statement}")),
            );
            continue;
        }
        if let Some(query) = selector.strip_prefix("@media") {
            for word in query.split(|c: char| c.is_whitespace() || "():,".contains(c)) {
                let digits = word.chars().any(|c| c.is_ascii_digit());
                if digits && !tokens.breakpoints.iter().any(|b| b == word) {
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
                problems.extend(value_problems(
                    &declaration,
                    property,
                    value,
                    selector == "@font-face",
                    tokens,
                ));
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
    let text = tight(&without_askama(source, &[("{#", "#}"), ("{%", "%}")]));
    let mut classes = Vec::new();
    for (at, _) in text.match_indices("class=\"") {
        if !text[..at].ends_with(char::is_whitespace) {
            continue;
        }
        let rest = &text[at + "class=\"".len()..];
        let value = rest.split('"').next().unwrap_or_default();
        if value.contains("{{") {
            return Err(format!("class built from an expression: {value}"));
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
    let tokens = tokens();
    let mut declarations = 0;
    for (name, css) in stylesheets() {
        declarations += css_rules(&css).iter().map(|(_, d)| d.len()).sum::<usize>();
        let problems = css_problems(&css, &tokens);
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
        "<p style = \"x\">",
        "<a href=\"x\"onclick=\"y\">",
        "<a onclick = \"y\">",
        "<button hx-on:click=\"y\">",
        "<a href=\"javascript:x()\">",
        "{{ text|escape(\"none\") }}",
        "{{ text | e(\"none\") }}",
        "{{ text | escape ( \"none\" ) }}",
        "{{ text | safe }}",
        "{% filter safe %}x{% endfilter %}",
        "<p class='a'>",
        "<p class=a>",
    ] {
        assert!(!template_problems(bad).is_empty(), "{bad}");
    }
    let good = "<script src=\"/x.js\" defer></script><p class=\"a\">on</p>{% let one = 1 %}{% if online == x %}{% endif %}{{ cache(\"none\") }}";
    assert!(template_problems(good).is_empty());
}

#[test]
fn css_checker_catches_planted_violations() {
    let tokens = Tokens {
        breakpoints: vec!["36rem".to_owned()],
        variables: [
            "--color-text",
            "--color-black",
            "--space-1",
            "--space-2",
            "--size-tile",
        ]
        .map(str::to_owned)
        .into(),
    };
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
        ".a { color: var(--color-text, red); }",
        ".a { padding: var(--space-9); }",
        "@import url(x.css);",
        ".a { color: color-mix(in srgb, var(--color-text), var(--color-black)); }",
        ".a { color: CanvasText; }",
        ".a { font-size: larger; }",
        ".a { border: thin solid var(--color-text); }",
        ".a::after { content: \"Verplicht\"; }",
        ".a { background: url(x.png); }",
        ".a { font-family: \"Comic Sans MS\"; }",
        ".a { font-family: Arial, var(--color-text); }",
    ] {
        assert!(!css_problems(bad, &tokens).is_empty(), "{bad}");
    }
    let good = "/* #fff 12px red */ .a { color: var(--color-text); flex: 1 1 var(--size-tile); \
        clip-path: inset(50%); border-left: var(--space-1) solid currentColor; }\n\
        @media (min-width: 36rem) { .a:hover { margin: 0 var(--space-2); } }\n\
        @font-face { font-family: \"Inter\"; src: url(\"fonts/inter.woff2\") format(\"woff2\"); \
        font-weight: 100 900; }";
    assert_eq!(css_problems(good, &tokens), Vec::<String>::new());
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
    assert_eq!(
        template_classes(
            "<b class=\"x {% if v == \"s\" %}x-s{% else if v == \"d\" %}x-d{% endif %}\" {# class=\"no\" #}>"
        )
        .expect("classes"),
        ["x", "x-s", "x-d"]
    );
    assert!(template_classes("<p class=\"button-{{ variant }}\"></p>").is_err());
    assert_eq!(
        template_classes("<p\n  class = \"nope\">").expect("classes"),
        ["nope"]
    );
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
