use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Deserialize;

const AA_NORMAL_TEXT: f64 = 4.5;
const AA_LARGE_TEXT: f64 = 3.0;

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

const LENGTH_UNITS: &[&str] = &["rem", "em", "px"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Tokens {
    color: BTreeMap<String, String>,
    font: BTreeMap<String, Vec<String>>,
    font_size: BTreeMap<String, String>,
    font_weight: BTreeMap<String, u16>,
    line_height: BTreeMap<String, String>,
    space: BTreeMap<String, String>,
    radius: BTreeMap<String, String>,
    #[serde(default)]
    border: BTreeMap<String, String>,
    #[serde(default)]
    size: BTreeMap<String, String>,
    shadow: BTreeMap<String, String>,
    breakpoint: BTreeMap<String, String>,
    #[serde(default)]
    pair: Vec<Pair>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Pair {
    foreground: String,
    background: String,
    #[serde(default)]
    large_text: bool,
}

#[derive(Clone, Copy)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

/// Turns the contents of `design/tokens.toml` into a `:root` block of CSS variables.
/// On failure the error holds one problem per line.
pub fn generate(source: &str) -> Result<String, String> {
    let tokens: Tokens = toml::from_str(source).map_err(|error| error.to_string())?;
    let mut sheet = Sheet::default();

    let mut colours = BTreeMap::new();
    for (name, value) in &tokens.color {
        let parsed = parse_hex(value).ok_or_else(|| format!("{value:?} is not a #rrggbb colour"));
        if let Ok(rgb) = parsed {
            colours.insert(name.as_str(), rgb);
        }
        sheet.add("color", name, parsed.map(|_| value.to_ascii_lowercase()));
    }
    for (name, stack) in &tokens.font {
        sheet.add("font", name, font_stack(stack));
    }
    sheet.add_all("font-size", &tokens.font_size, length);
    for (name, weight) in &tokens.font_weight {
        let value = if (1..=1000).contains(weight) {
            Ok(weight.to_string())
        } else {
            Err(format!("{weight} is not a font weight from 1 to 1000"))
        };
        sheet.add("font-weight", name, value);
    }
    sheet.add_all("line-height", &tokens.line_height, |value| {
        if is_number(value) {
            Ok(value.to_string())
        } else {
            Err(format!("{value:?} is not a unitless number such as 1.5"))
        }
    });
    sheet.add_all("space", &tokens.space, length);
    sheet.add_all("radius", &tokens.radius, length);
    sheet.add_all("border", &tokens.border, length);
    sheet.add_all("size", &tokens.size, length);
    sheet.add_all("shadow", &tokens.shadow, shadow);
    sheet.add_all("breakpoint", &tokens.breakpoint, length);

    check_pairs(&tokens.pair, &colours, &mut sheet.errors);

    if sheet.errors.is_empty() {
        Ok(sheet.render())
    } else {
        Err(sheet.errors.join("\n"))
    }
}

#[derive(Default)]
struct Sheet {
    variables: Vec<(String, String)>,
    seen: BTreeSet<String>,
    errors: Vec<String>,
}

impl Sheet {
    fn add(&mut self, section: &str, name: &str, value: Result<String, String>) {
        if !is_name(name) {
            self.errors.push(format!(
                "[{section}] {name:?}: names use lowercase letters, digits and single hyphens"
            ));
            return;
        }
        let variable = format!("--{section}-{name}");
        match value {
            Err(problem) => self.errors.push(format!("[{section}] {name}: {problem}")),
            Ok(_) if !self.seen.insert(variable.clone()) => {
                self.errors.push(format!("{variable} is defined twice"));
            }
            Ok(value) => self.variables.push((variable, value)),
        }
    }

    fn add_all(
        &mut self,
        section: &str,
        values: &BTreeMap<String, String>,
        check: impl Fn(&str) -> Result<String, String>,
    ) {
        for (name, value) in values {
            self.add(section, name, check(value));
        }
    }

    fn render(&self) -> String {
        let mut css =
            String::from("/* Generated from design/tokens.toml. Do not edit. */\n:root {\n");
        for (variable, value) in &self.variables {
            let _ = writeln!(css, "  {variable}: {value};");
        }
        css.push_str("}\n");
        css
    }
}

fn check_pairs(pairs: &[Pair], colours: &BTreeMap<&str, Rgb>, errors: &mut Vec<String>) {
    if pairs.is_empty() {
        errors.push("declare at least one [[pair]] of colours used together".to_string());
    }
    let mut seen = BTreeSet::new();
    for pair in pairs {
        let label = format!("pair {} on {}", pair.foreground, pair.background);
        if !seen.insert((pair.foreground.as_str(), pair.background.as_str())) {
            errors.push(format!(
                "{} on {} is declared twice",
                pair.foreground, pair.background
            ));
            continue;
        }
        let lookup = |name: &str| {
            colours
                .get(name)
                .copied()
                .ok_or_else(|| format!("{label}: unknown colour {name:?}"))
        };
        let (foreground, background) = match (lookup(&pair.foreground), lookup(&pair.background)) {
            (Ok(foreground), Ok(background)) => (foreground, background),
            (foreground, background) => {
                errors.extend(foreground.err());
                errors.extend(background.err());
                continue;
            }
        };
        let (minimum, kind) = if pair.large_text {
            (AA_LARGE_TEXT, "large text")
        } else {
            (AA_NORMAL_TEXT, "normal text")
        };
        let ratio = contrast_ratio(foreground, background);
        if ratio < minimum {
            let shown = (ratio * 100.0).floor() / 100.0;
            errors.push(format!(
                "{label}: contrast {shown:.2}:1 is below {minimum}:1 (WCAG 2.2 AA, {kind})"
            ));
        }
    }
}

fn parse_hex(value: &str) -> Option<Rgb> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(Rgb {
        r: channel(0)?,
        g: channel(2)?,
        b: channel(4)?,
    })
}

/// WCAG 2.2 relative luminance of an sRGB colour, from 0 (black) to 1 (white).
fn relative_luminance(colour: Rgb) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(colour.r) + 0.7152 * linear(colour.g) + 0.0722 * linear(colour.b)
}

fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (relative_luminance(a), relative_luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn is_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_number(value: &str) -> bool {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits(whole) && digits(fraction)
}

fn length(value: &str) -> Result<String, String> {
    let valid = value == "0"
        || LENGTH_UNITS
            .iter()
            .any(|unit| value.strip_suffix(unit).is_some_and(is_number));
    if valid {
        Ok(value.to_string())
    } else {
        Err(format!(
            "{value:?} is not a length such as 0, 1rem, 0.5em or 2px"
        ))
    }
}

fn font_stack(stack: &[String]) -> Result<String, String> {
    if !stack
        .last()
        .is_some_and(|family| GENERIC_FAMILIES.contains(&family.as_str()))
    {
        return Err("the font stack must end with a generic family such as sans-serif".to_string());
    }
    let mut families = Vec::with_capacity(stack.len());
    for family in stack {
        let plain = family
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b' ' || byte == b'-');
        if family.trim().is_empty() || !plain {
            return Err(format!("{family:?} is not a valid font family name"));
        }
        if GENERIC_FAMILIES.contains(&family.as_str()) {
            families.push(family.clone());
        } else {
            families.push(format!("\"{family}\""));
        }
    }
    Ok(families.join(", "))
}

fn shadow(value: &str) -> Result<String, String> {
    if is_plain_shadow(value) {
        Ok(value.to_string())
    } else {
        Err(format!(
            "{value:?} is not a plain box-shadow value (lengths, #rrggbb or rgb(...), commas)"
        ))
    }
}

fn is_plain_shadow(value: &str) -> bool {
    let allowed = |byte: u8| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || b" .%/,#-()".contains(&byte)
    };
    if value.trim().is_empty() || !value.bytes().all(allowed) {
        return false;
    }
    let mut inside_rgb = false;
    for (at, byte) in value.bytes().enumerate() {
        match byte {
            b'(' if !inside_rgb && value[..at].ends_with("rgb") => inside_rgb = true,
            b')' if inside_rgb => inside_rgb = false,
            b'(' | b')' => return false,
            _ => {}
        }
    }
    !inside_rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r##"
[color]
black = "#000000"
white = "#FFFFFF"

[font]
body = ["Inter", "Helvetica Neue", "sans-serif"]

[font-size]
md = "1rem"

[font-weight]
bold = 700

[line-height]
normal = "1.5"

[space]
0 = "0"
1 = "0.25rem"

[radius]
full = "9999px"

[shadow]
sm = "0 1px 2px rgb(0 0 0 / 0.08)"

[breakpoint]
md = "48rem"

[[pair]]
foreground = "black"
background = "white"
"##;

    fn with(extra: &str) -> String {
        format!("{MINIMAL}\n{extra}")
    }

    fn replaced(from: &str, to: &str) -> String {
        assert!(MINIMAL.contains(from), "{from:?} not in fixture");
        MINIMAL.replacen(from, to, 1)
    }

    fn error_of(source: &str) -> String {
        generate(source).expect_err("tokens should be rejected")
    }

    fn rgb(hex: &str) -> Rgb {
        parse_hex(hex).expect("valid colour")
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.01,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn black_on_white_is_21_to_1() {
        assert_close(contrast_ratio(rgb("#000000"), rgb("#ffffff")), 21.0);
        assert_close(contrast_ratio(rgb("#ffffff"), rgb("#000000")), 21.0);
    }

    #[test]
    fn same_colour_is_1_to_1() {
        assert_close(contrast_ratio(rgb("#767676"), rgb("#767676")), 1.0);
    }

    #[test]
    fn contrast_matches_known_values() {
        assert_close(contrast_ratio(rgb("#767676"), rgb("#ffffff")), 4.54);
        assert_close(contrast_ratio(rgb("#777777"), rgb("#ffffff")), 4.48);
        assert_close(contrast_ratio(rgb("#ff0000"), rgb("#ffffff")), 4.0);
        assert_close(contrast_ratio(rgb("#0000ff"), rgb("#ffffff")), 8.59);
    }

    #[test]
    fn hex_colours_only_in_rrggbb_form() {
        assert!(parse_hex("#a0B1c2").is_some());
        for bad in [
            "#fff", "000000", "#00000g", "#0000000", "red", "#", "", "#ff00ff ",
        ] {
            assert!(parse_hex(bad).is_none(), "{bad:?} accepted");
        }
    }

    #[test]
    fn renders_root_block_with_css_variables() {
        let css = generate(MINIMAL).expect("minimal tokens are valid");
        assert!(
            css.starts_with("/* Generated from design/tokens.toml. Do not edit. */\n:root {\n")
        );
        assert!(css.ends_with("}\n"));
        for line in [
            "  --color-black: #000000;\n",
            "  --color-white: #ffffff;\n",
            "  --font-body: \"Inter\", \"Helvetica Neue\", sans-serif;\n",
            "  --font-size-md: 1rem;\n",
            "  --font-weight-bold: 700;\n",
            "  --line-height-normal: 1.5;\n",
            "  --space-0: 0;\n",
            "  --space-1: 0.25rem;\n",
            "  --radius-full: 9999px;\n",
            "  --shadow-sm: 0 1px 2px rgb(0 0 0 / 0.08);\n",
            "  --breakpoint-md: 48rem;\n",
        ] {
            assert!(css.contains(line), "missing {line:?} in\n{css}");
        }
    }

    #[test]
    fn unknown_section_is_rejected() {
        let error = error_of(&with("[colour]\nred = \"#ff0000\"\n"));
        assert!(error.contains("colour"), "{error}");
    }

    #[test]
    fn missing_section_is_rejected() {
        let source = replaced("[radius]\nfull = \"9999px\"\n", "");
        let error = error_of(&source);
        assert!(error.contains("radius"), "{error}");
    }

    #[test]
    fn unknown_pair_field_is_rejected() {
        let error = error_of(&with(
            "[[pair]]\nforeground = \"black\"\nbackground = \"white\"\nbold = true\n",
        ));
        assert!(error.contains("bold"), "{error}");
    }

    #[test]
    fn short_hex_colour_is_rejected() {
        let error = error_of(&replaced("\"#FFFFFF\"", "\"#fff\""));
        assert!(error.contains("[color] white"), "{error}");
        assert!(error.contains("#rrggbb"), "{error}");
    }

    #[test]
    fn duplicate_key_in_a_section_is_rejected() {
        let error = error_of(&replaced("white = ", "black = \"#000000\"\nwhite = "));
        assert!(error.contains("duplicate"), "{error}");
    }

    #[test]
    fn duplicate_css_variable_across_sections_is_rejected() {
        let source = replaced(
            "body = [",
            "size-md = [\"Inter\", \"sans-serif\"]\nbody = [",
        );
        let error = error_of(&source);
        assert!(error.contains("--font-size-md is defined twice"), "{error}");
    }

    #[test]
    fn duplicate_pair_is_rejected() {
        let error = error_of(&with(
            "[[pair]]\nforeground = \"black\"\nbackground = \"white\"\n",
        ));
        assert!(
            error.contains("black on white is declared twice"),
            "{error}"
        );
    }

    #[test]
    fn bad_names_are_rejected() {
        for name in ["Black", "-x", "x-", "a--b", "a_b", "\"\""] {
            let source = replaced("black = ", &format!("{name} = \"#000000\"\nblack = "));
            let error = error_of(&source);
            assert!(error.contains("[color]"), "{name}: {error}");
            assert!(error.contains("lowercase"), "{name}: {error}");
        }
    }

    #[test]
    fn pair_below_aa_contrast_is_rejected() {
        let error = error_of(
            &with("[[pair]]\nforeground = \"grey\"\nbackground = \"white\"\n")
                .replace("black = ", "grey = \"#777777\"\nblack = "),
        );
        assert!(
            error.contains("pair grey on white: contrast 4.47:1 is below 4.5:1"),
            "{error}"
        );
    }

    #[test]
    fn large_text_pair_needs_3_to_1() {
        let colours = replaced(
            "black = ",
            "red = \"#ff0000\"\npale = \"#aaaaaa\"\nblack = ",
        );
        let passing = format!(
            "{colours}\n[[pair]]\nforeground = \"red\"\nbackground = \"white\"\nlarge-text = true\n"
        );
        assert!(generate(&passing).is_ok(), "{passing}");
        let failing = format!(
            "{colours}\n[[pair]]\nforeground = \"pale\"\nbackground = \"white\"\nlarge-text = true\n"
        );
        let error = error_of(&failing);
        assert!(error.contains("pair pale on white"), "{error}");
        assert!(error.contains("below 3:1"), "{error}");
        assert!(error.contains("large text"), "{error}");
    }

    #[test]
    fn pair_with_unknown_colour_is_rejected() {
        let error = error_of(&with(
            "[[pair]]\nforeground = \"ink\"\nbackground = \"white\"\n",
        ));
        assert!(
            error.contains("pair ink on white: unknown colour \"ink\""),
            "{error}"
        );
    }

    #[test]
    fn at_least_one_pair_is_required() {
        let source = replaced(
            "[[pair]]\nforeground = \"black\"\nbackground = \"white\"\n",
            "",
        );
        let error = error_of(&source);
        assert!(error.contains("pair"), "{error}");
    }

    #[test]
    fn bad_lengths_are_rejected() {
        for (from, section) in [
            ("md = \"1rem\"", "[font-size] md"),
            ("1 = \"0.25rem\"", "[space] 1"),
            ("full = \"9999px\"", "[radius] full"),
            ("md = \"48rem\"", "[breakpoint] md"),
        ] {
            for bad in [
                "",
                "1",
                "1.rem",
                "1 rem",
                "-1rem",
                "1vw",
                "calc(1rem)",
                "1rem;",
            ] {
                let (key, _) = from.split_once(" = ").expect("fixture line");
                let source = replaced(from, &format!("{key} = {bad:?}"));
                let error = error_of(&source);
                assert!(error.contains(section), "{bad:?}: {error}");
            }
        }
    }

    #[test]
    fn good_lengths_are_accepted() {
        for good in ["0", "2px", "0.5rem", "1.25em", "10rem"] {
            let source = replaced("md = \"1rem\"", &format!("md = {good:?}"));
            assert!(generate(&source).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn bad_line_height_is_rejected() {
        for bad in ["", "1.5rem", "abc", "1.", ".5"] {
            let source = replaced("normal = \"1.5\"", &format!("normal = {bad:?}"));
            let error = error_of(&source);
            assert!(error.contains("[line-height] normal"), "{bad:?}: {error}");
        }
    }

    #[test]
    fn bad_font_weight_is_rejected() {
        for bad in ["0", "1001"] {
            let error = error_of(&replaced("bold = 700", &format!("bold = {bad}")));
            assert!(error.contains("[font-weight] bold"), "{bad}: {error}");
        }
    }

    #[test]
    fn font_stack_must_end_with_generic_family() {
        let error = error_of(&replaced(", \"sans-serif\"]", "]"));
        assert!(error.contains("[font] body"), "{error}");
        assert!(error.contains("generic"), "{error}");
    }

    #[test]
    fn font_names_cannot_break_out_of_quotes() {
        for bad in [
            r#""In\"ter""#,
            r#""a;b""#,
            r#""a{b""#,
            r#""a,b""#,
            r#""""#,
            r#"" ""#,
            r#""a</style>""#,
            r#""a\fb""#,
            r#""a\tb""#,
        ] {
            let source = replaced("\"Inter\"", bad);
            let error = error_of(&source);
            assert!(error.contains("[font] body"), "{bad:?}: {error}");
        }
    }

    #[test]
    fn real_shadow_values_are_accepted() {
        for good in [
            "none",
            "0 0 0 2px #000000",
            "inset 0 1px 2px rgb(0 0 0 / 8%), 0 1px 0 rgb(255 255 255)",
        ] {
            let source = replaced(
                "sm = \"0 1px 2px rgb(0 0 0 / 0.08)\"",
                &format!("sm = {good:?}"),
            );
            assert!(generate(&source).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn shadow_cannot_break_out_of_declaration() {
        for bad in [
            "",
            "0 0 1px red; color: red",
            "0 }",
            "0 {",
            "a\\b",
            "a\nb",
            "a</style>",
            "0 4px 12px rgb(0 0 0 / 0.12",
            "0 4px 12px rgb(0 0 0 / 0.12))",
            ")0 1px 2px rgb(0 0 0 / 0.1)(",
            "0 1px 2px /* x",
            "0 1px 2px url(x)",
            "0 1px 2px red !important",
            "@import x",
        ] {
            let source = replaced(
                "sm = \"0 1px 2px rgb(0 0 0 / 0.08)\"",
                &format!("sm = {bad:?}"),
            );
            let error = error_of(&source);
            assert!(error.contains("[shadow] sm"), "{bad:?}: {error}");
        }
    }

    #[test]
    fn all_problems_are_reported_together() {
        let source = replaced("\"#FFFFFF\"", "\"#fff\"").replace("\"1rem\"", "\"1\"");
        let error = error_of(&source);
        assert!(error.contains("[color] white"), "{error}");
        assert!(error.contains("[font-size] md"), "{error}");
    }
}
