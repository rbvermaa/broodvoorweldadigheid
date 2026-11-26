//! Turns design/icons.toml and the Lucide release into `icons.rs`: the inner SVG markup of
//! every icon we use. Shared by build.rs and the unit tests.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

/// The domain allergen codes (EU 1169/2011), in `domain::Allergen::ALL` order.
pub const ALLERGEN_CODES: [&str; 14] = [
    "cereals_containing_gluten",
    "crustaceans",
    "eggs",
    "fish",
    "peanuts",
    "soybeans",
    "milk",
    "nuts",
    "celery",
    "mustard",
    "sesame_seeds",
    "sulphur_dioxide_and_sulphites",
    "lupin",
    "molluscs",
];

const SHAPES: [&str; 7] = [
    "path", "circle", "rect", "line", "polyline", "polygon", "ellipse",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IconsFile {
    lucide: Vec<String>,
    allergens: BTreeMap<String, String>,
    /// Icons Lucide does not have, as the shapes inside `<svg>`, checked like Lucide's.
    #[serde(default)]
    own: BTreeMap<String, String>,
}

/// `read_svg(name)` returns Lucide's `icons/NAME.svg`.
pub fn generate(
    source: &str,
    read_svg: impl Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    let file: IconsFile = toml::from_str(source).map_err(|error| error.to_string())?;
    let mut problems = Vec::new();

    let codes: BTreeSet<&str> = file.allergens.keys().map(String::as_str).collect();
    for code in ALLERGEN_CODES {
        if !codes.contains(code) {
            problems.push(format!("allergen {code} has no icon"));
        }
    }
    for code in &codes {
        if !ALLERGEN_CODES.contains(code) {
            problems.push(format!("{code} is not an allergen code"));
        }
    }
    let mut seen = BTreeMap::new();
    for (code, icon) in &file.allergens {
        if let Some(other) = seen.insert(icon.as_str(), code.as_str()) {
            problems.push(format!(
                "allergens {other} and {code} share the icon {icon}"
            ));
        }
    }

    let names: BTreeSet<&str> = file
        .lucide
        .iter()
        .chain(file.allergens.values())
        .map(String::as_str)
        .collect();
    let mut icons = Vec::new();
    for &name in &names {
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            problems.push(format!("{name:?} is not a Lucide icon name"));
            continue;
        }
        match read_svg(name).and_then(|svg| inner_svg(&svg)) {
            Ok(inner) => icons.push((name, inner)),
            Err(error) => problems.push(format!("icon {name}: {error}")),
        }
    }
    for (name, shapes) in &file.own {
        if names.contains(name.as_str()) {
            problems.push(format!("own icon {name} is also a Lucide icon"));
            continue;
        }
        match inner_svg(&format!("<svg>{shapes}</svg>")) {
            Ok(inner) => icons.push((name.as_str(), inner)),
            Err(error) => problems.push(format!("own icon {name}: {error}")),
        }
    }
    icons.sort_unstable();
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }

    let mut out = String::from("pub const ICONS: &[(&str, &str)] = &[\n");
    for (name, inner) in icons {
        out.push_str(&format!("    ({name:?}, {inner:?}),\n"));
    }
    out.push_str("];\n\npub const ALLERGEN_ICONS: &[(&str, &str)] = &[\n");
    for (code, icon) in &file.allergens {
        out.push_str(&format!("    ({code:?}, {icon:?}),\n"));
    }
    out.push_str("];\n");
    Ok(out)
}

const ATTRIBUTES: [&str; 14] = [
    "d", "cx", "cy", "r", "rx", "ry", "x", "y", "x1", "x2", "y1", "y2", "width", "height",
];

/// The shapes inside Lucide's `<svg>`, on one line. Only plain shapes with geometry
/// attributes (and `points`) are accepted.
fn inner_svg(svg: &str) -> Result<String, String> {
    let start = svg.find("<svg").ok_or("no <svg>")?;
    let open_end = svg[start..].find('>').ok_or("unclosed <svg>")? + start + 1;
    let close = svg.rfind("</svg>").ok_or("no </svg>")?;
    let inner = svg
        .get(open_end..close)
        .ok_or("</svg> before <svg>")?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("> <", "><");
    if inner.is_empty() {
        return Err("empty icon".to_string());
    }
    for element in inner.split('<').skip(1) {
        let element = element.trim_end();
        let body = element
            .strip_suffix("/>")
            .ok_or_else(|| format!("<{element} is not a self-closing shape"))?;
        let (tag, mut attributes) = body.split_once(' ').unwrap_or((body, ""));
        if !SHAPES.contains(&tag) {
            return Err(format!("unexpected element <{tag}>"));
        }
        loop {
            attributes = attributes.trim_start();
            if attributes.is_empty() {
                break;
            }
            let (name, rest) = attributes
                .split_once("=\"")
                .ok_or_else(|| format!("<{tag}>: cannot read {attributes:?}"))?;
            if !(ATTRIBUTES.contains(&name) || name == "points") {
                return Err(format!("<{tag}> has a {name} attribute"));
            }
            let (value, rest) = rest
                .split_once('"')
                .ok_or_else(|| format!("<{tag}>: unclosed {name}"))?;
            if !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || " .,-".contains(c))
            {
                return Err(format!("<{tag}> {name} has {value:?}"));
            }
            attributes = rest;
        }
    }
    Ok(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHEAT: &str = "<!-- @license lucide-static - ISC -->\n<svg\n  class=\"lucide lucide-x\"\n  viewBox=\"0 0 24 24\"\n>\n  <path d=\"M2 22 16 8\" />\n  <circle cx=\"1\" cy=\"2\" r=\"3\" />\n</svg>\n";

    fn icons_toml(lucide: &str, allergens: &[(&str, &str)]) -> String {
        let mut toml = format!("lucide = [{lucide}]\n[allergens]\n");
        for (code, icon) in allergens {
            toml.push_str(&format!("{code} = \"{icon}\"\n"));
        }
        toml
    }

    fn all_allergens() -> Vec<(&'static str, String)> {
        ALLERGEN_CODES
            .iter()
            .enumerate()
            .map(|(i, code)| (*code, format!("a{i}")))
            .collect()
    }

    fn toml_with_all(lucide: &str) -> String {
        let allergens = all_allergens();
        let pairs: Vec<(&str, &str)> = allergens
            .iter()
            .map(|(code, icon)| (*code, icon.as_str()))
            .collect();
        icons_toml(lucide, &pairs)
    }

    #[test]
    fn keeps_only_the_shapes_on_one_line() {
        assert_eq!(
            inner_svg(WHEAT).expect("valid"),
            "<path d=\"M2 22 16 8\" /><circle cx=\"1\" cy=\"2\" r=\"3\" />"
        );
    }

    #[test]
    fn generates_icons_and_allergen_map() {
        let out = generate(&toml_with_all("\"check\""), |_| Ok(WHEAT.to_string())).expect("ok");
        assert!(
            out.contains("(\"check\", \"<path d=\\\"M2 22 16 8\\\" />"),
            "{out}"
        );
        assert!(out.contains("(\"milk\", \"a6\")"), "{out}");
        assert_eq!(out.matches("<circle").count(), 15, "{out}");
    }

    #[test]
    fn refuses_scripts_styles_classes_and_handlers() {
        for bad in [
            "<svg><script>alert(1)</script></svg>",
            "<svg><path d=\"M\" style=\"x\" /></svg>",
            "<svg><path class=\"x\" d=\"M\" /></svg>",
            "<svg><path onload=\"x\" d=\"M\" /></svg>",
            "<svg><use href=\"#x\" /></svg>",
            "<svg><path d=\"M\"></path></svg>",
            "<svg></svg>",
            "<path />",
            "<svg><path d=\"M\"\nonload=\"x\" /></svg>",
            "<svg><path d=\"M\"\tonload=\"x\" /></svg>",
            "<svg><path d=\"url(#x)\" /></svg>",
            "<svg><path d=M /></svg>",
        ] {
            assert!(inner_svg(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_allergen_needs_its_own_known_icon() {
        let missing = icons_toml("", &[("milk", "milk")]);
        let error = generate(&missing, |_| Ok(WHEAT.to_string())).expect_err("incomplete");
        assert!(error.contains("allergen eggs has no icon"), "{error}");

        let mut allergens = all_allergens();
        allergens[1].1 = allergens[0].1.clone();
        allergens.push(("gluten", "x".to_string()));
        let pairs: Vec<(&str, &str)> = allergens.iter().map(|(c, i)| (*c, i.as_str())).collect();
        let error =
            generate(&icons_toml("", &pairs), |_| Ok(WHEAT.to_string())).expect_err("shared icon");
        assert!(error.contains("share the icon a0"), "{error}");
        assert!(error.contains("gluten is not an allergen code"), "{error}");
    }

    #[test]
    fn unknown_icons_and_bad_names_fail() {
        let error = generate(&toml_with_all("\"nope\", \"../x\""), |name| {
            if name == "nope" {
                Err("not found".to_string())
            } else {
                Ok(WHEAT.to_string())
            }
        })
        .expect_err("unknown");
        assert!(error.contains("icon nope: not found"), "{error}");
        assert!(
            error.contains("\"../x\" is not a Lucide icon name"),
            "{error}"
        );
    }

    #[test]
    fn own_icons_are_checked_like_lucide_ones() {
        let mut toml = toml_with_all("\"check\"");
        toml.push_str("[own]\nring = '<circle cx=\"12\" cy=\"12\" r=\"4\" />'\n");
        let out = generate(&toml, |_| Ok(WHEAT.to_string())).expect("ok");
        assert!(
            out.contains("(\"ring\", \"<circle cx=\\\"12\\\" cy=\\\"12\\\" r=\\\"4\\\" />\")"),
            "{out}"
        );

        let mut toml = toml_with_all("\"check\"");
        toml.push_str("[own]\ncheck = '<circle r=\"1\" />'\nbad = '<script>alert(1)</script>'\n");
        let error = generate(&toml, |_| Ok(WHEAT.to_string())).expect_err("invalid");
        assert!(
            error.contains("own icon check is also a Lucide icon"),
            "{error}"
        );
        assert!(error.contains("own icon bad:"), "{error}");
    }
}
