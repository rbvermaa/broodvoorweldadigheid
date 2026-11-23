use crate::model::RootFile;

const ATTRIBUTE: &str = "#![forbid(unsafe_code)]";

pub fn check(roots: &[RootFile]) -> Vec<String> {
    roots
        .iter()
        .filter(|root| !root.contents.lines().any(|line| line.trim() == ATTRIBUTE))
        .map(|root| {
            format!(
                "{} root file {} lacks {ATTRIBUTE}",
                root.crate_name, root.path
            )
        })
        .collect()
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
}
