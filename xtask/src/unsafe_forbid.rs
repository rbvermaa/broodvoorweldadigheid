use crate::model::{Manifest, RootFile};

const ATTRIBUTE: &str = "#![forbid(unsafe_code)]";

pub fn check(roots: &[RootFile]) -> Vec<String> {
    roots
        .iter()
        .filter(|root| !has_leading_attribute(&root.contents))
        .map(|root| {
            format!(
                "{} root file {} lacks {ATTRIBUTE}",
                root.crate_name, root.path
            )
        })
        .collect()
}

fn has_leading_attribute(contents: &str) -> bool {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .take_while(|line| line.starts_with("#!["))
        .any(|line| line == ATTRIBUTE)
}

pub fn check_manifests(manifests: &[Manifest]) -> Vec<String> {
    manifests
        .iter()
        .filter(|manifest| !inherits_workspace_lints(&manifest.contents))
        .map(|manifest| {
            format!(
                "{} manifest {} lacks [lints] workspace = true",
                manifest.crate_name, manifest.path
            )
        })
        .collect()
}

fn inherits_workspace_lints(contents: &str) -> bool {
    contents
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "[lints]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .any(|line| line == "workspace = true")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(crate_name: &str, contents: &str) -> RootFile {
        RootFile {
            crate_name: crate_name.to_string(),
            path: format!("crates/{crate_name}/src/lib.rs"),
            contents: contents.to_string(),
        }
    }

    #[test]
    fn root_with_forbid_passes() {
        let roots = [root("domain", "#![forbid(unsafe_code)]\n\npub fn f() {}\n")];
        assert_eq!(check(&roots), Vec::<String>::new());
    }

    #[test]
    fn root_without_forbid_fails() {
        let roots = [
            root("domain", "#![forbid(unsafe_code)]\n"),
            root(
                "orders",
                "// #![forbid(unsafe_code)]\n#![deny(unsafe_code)]\n",
            ),
        ];
        let violations = check(&roots);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("orders"), "{violations:?}");
        assert!(
            violations[0].contains("crates/orders/src/lib.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn attribute_after_doc_comments_passes() {
        let roots = [root(
            "domain",
            "//! Domain types.\n\n#![forbid(unsafe_code)]\n#![warn(missing_docs)]\n",
        )];
        assert_eq!(check(&roots), Vec::<String>::new());
    }

    #[test]
    fn attribute_inside_block_comment_fails() {
        let roots = [root(
            "domain",
            "/*\n#![forbid(unsafe_code)]\n*/\npub fn f() {}\n",
        )];
        assert_eq!(check(&roots).len(), 1);
    }

    #[test]
    fn attribute_inside_string_fails() {
        let roots = [root(
            "domain",
            "pub const S: &str = r\"\n#![forbid(unsafe_code)]\n\";\n",
        )];
        assert_eq!(check(&roots).len(), 1);
    }

    #[test]
    fn attribute_inside_inline_module_fails() {
        let roots = [root("domain", "mod m {\n    #![forbid(unsafe_code)]\n}\n")];
        assert_eq!(check(&roots).len(), 1);
    }

    fn manifest(crate_name: &str, contents: &str) -> Manifest {
        Manifest {
            crate_name: crate_name.to_string(),
            path: format!("crates/{crate_name}/Cargo.toml"),
            contents: contents.to_string(),
        }
    }

    #[test]
    fn manifest_inheriting_workspace_lints_passes() {
        let manifests = [manifest(
            "domain",
            "[package]\nname = \"domain\"\n\n[lints]\nworkspace = true\n",
        )];
        assert_eq!(check_manifests(&manifests), Vec::<String>::new());
    }

    #[test]
    fn manifest_without_workspace_lints_fails() {
        let manifests = [
            manifest("domain", "[package]\nname = \"domain\"\n"),
            manifest("orders", "[lints]\n\n[features]\nworkspace = true\n"),
        ];
        let violations = check_manifests(&manifests);
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(violations[0].contains("domain"), "{violations:?}");
        assert!(violations[1].contains("orders"), "{violations:?}");
    }
}
