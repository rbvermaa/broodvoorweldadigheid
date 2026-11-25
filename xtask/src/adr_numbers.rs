use std::collections::BTreeMap;
use std::path::Path;

pub fn check(dir: &Path) -> Result<Vec<String>, String> {
    let read_error = |error| format!("cannot read {}: {error}", dir.display());
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(read_error)? {
        let entry = entry.map_err(read_error)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let contents = std::fs::read_to_string(entry.path())
            .map_err(|error| format!("cannot read {}: {error}", entry.path().display()))?;
        files.push((name, contents));
    }
    files.sort();
    Ok(check_files(&files))
}

fn check_files(files: &[(String, String)]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut by_number: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, contents) in files {
        let Some(number) = number(name) else {
            violations.push(format!("{name} is not named NNNN-title.md"));
            continue;
        };
        by_number.entry(number).or_default().push(name);
        if !has_title(contents, number) {
            violations.push(format!("{name} does not start with \"# {number} <title>\""));
        }
    }
    for (number, names) in by_number {
        if names.len() > 1 {
            violations.push(format!("ADR {number} is used by {}", names.join(" and ")));
        }
    }
    violations
}

fn number(name: &str) -> Option<&str> {
    let (number, title) = name.strip_suffix(".md")?.split_once('-')?;
    (number.len() == 4 && number.bytes().all(|b| b.is_ascii_digit()) && !title.is_empty())
        .then_some(number)
}

fn has_title(contents: &str, number: &str) -> bool {
    contents
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("# "))
        .and_then(|heading| heading.strip_prefix(number))
        .and_then(|rest| rest.strip_prefix(' '))
        .is_some_and(|title| !title.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("xtask-adr-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create fixture dir");
            for (file, contents) in files {
                std::fs::write(dir.join(file), contents).expect("write fixture file");
            }
            Self(dir)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn distinct_numbers_with_titles_pass() {
        let fixture = Fixture::new(
            "ok",
            &[
                ("0001-one.md", "# 0001 One\n\nStatus: accepted\n"),
                ("0002-two.md", "# 0002 Two\n"),
            ],
        );
        assert_eq!(check(&fixture.0), Ok(Vec::new()));
    }

    #[test]
    fn duplicate_number_names_both_files() {
        let fixture = Fixture::new(
            "duplicate",
            &[
                ("0001-one.md", "# 0001 One\n"),
                ("0002-two.md", "# 0002 Two\n"),
                ("0002-other.md", "# 0002 Other\n"),
            ],
        );
        let violations = check(&fixture.0).expect("readable");
        assert_eq!(
            violations,
            ["ADR 0002 is used by 0002-other.md and 0002-two.md"]
        );
    }

    #[test]
    fn number_without_title_fails() {
        let fixture = Fixture::new(
            "untitled",
            &[
                ("0001.md", "# 0001 One\n"),
                ("0002-.md", "# 0002 Two\n"),
                ("0003-three.md", "# 0003\n"),
                ("0004-four.md", "# 0005 Four\n"),
                ("12-short.md", "# 12 Short\n"),
            ],
        );
        let violations = check(&fixture.0).expect("readable");
        assert_eq!(
            violations,
            [
                "0001.md is not named NNNN-title.md",
                "0002-.md is not named NNNN-title.md",
                "0003-three.md does not start with \"# 0003 <title>\"",
                "0004-four.md does not start with \"# 0004 <title>\"",
                "12-short.md is not named NNNN-title.md",
            ]
        );
    }

    #[test]
    fn missing_dir_is_an_error() {
        let dir = std::env::temp_dir().join(format!("xtask-adr-{}-missing", std::process::id()));
        assert!(check(&dir).is_err());
    }
}
