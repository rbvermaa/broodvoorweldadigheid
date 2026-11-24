use std::path::Path;

const FORBIDDEN: &[&str] = &["TimeZone::system", "Zoned::now", "TimeZone::get("];
const ALLOWED_PREFIX: &str = "crates/domain/";
const SELF_PATH: &str = "xtask/src/time_zone.rs";
const SKIPPED_DIRS: &[&str] = &["target", "result"];

pub struct SourceFile {
    pub path: String,
    pub contents: String,
}

pub fn check(files: &[SourceFile]) -> Vec<String> {
    let mut violations = Vec::new();
    for file in files.iter().filter(|f| !f.path.starts_with(ALLOWED_PREFIX)) {
        for (number, line) in file.contents.lines().enumerate() {
            for pattern in FORBIDDEN.iter().filter(|p| line.contains(*p)) {
                violations.push(format!(
                    "{}:{}: {pattern}; get the zone from domain::bakery_time_zone() \
                     and the time from a domain::Clock",
                    file.path,
                    number + 1
                ));
            }
        }
    }
    violations
}

/// Every `.rs` file under `root`, with `/`-separated paths relative to it.
pub fn workspace_files(root: &Path) -> Result<Vec<SourceFile>, String> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.retain(|f| f.path != SELF_PATH);
    Ok(files)
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<SourceFile>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("cannot read {}: {error}", dir.display()))?
            .path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name) {
                collect(root, &path, files)?;
            }
        } else if name.ends_with(".rs") {
            let contents = std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let parts: Vec<_> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            files.push(SourceFile {
                path: parts.join("/"),
                contents,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, contents: &str) -> SourceFile {
        SourceFile {
            path: path.to_string(),
            contents: contents.to_string(),
        }
    }

    #[test]
    fn clean_files_pass() {
        let files = [file(
            "crates/orders/src/lib.rs",
            "let now = clock.now_in_bakery();\n",
        )];
        assert!(check(&files).is_empty());
    }

    #[test]
    fn each_forbidden_call_outside_domain_fails() {
        let files = [
            file("apps/server/src/lib.rs", "let tz = TimeZone::system();\n"),
            file("crates/jobs/src/lib.rs", "x\nlet now = Zoned::now();\n"),
            file("apps/cli/src/lib.rs", "let tz = TimeZone::get(\"UTC\")?;\n"),
        ];
        let violations = check(&files);
        assert_eq!(violations.len(), 3, "{violations:?}");
        assert!(
            violations[0].starts_with("apps/server/src/lib.rs:1"),
            "{violations:?}"
        );
        assert!(
            violations[1].starts_with("crates/jobs/src/lib.rs:2"),
            "{violations:?}"
        );
        assert!(
            violations[1].contains("domain::bakery_time_zone()"),
            "{violations:?}"
        );
    }

    #[test]
    fn domain_may_use_them() {
        let files = [file("crates/domain/src/clock.rs", "TimeZone::get(\"x\")\n")];
        assert!(check(&files).is_empty());
    }

    #[test]
    fn workspace_gets_its_zone_through_domain() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let files = workspace_files(&root).unwrap();
        assert!(files.iter().any(|f| f.path == "crates/domain/src/clock.rs"));
        assert!(!files.iter().any(|f| f.path == SELF_PATH));
        let violations = check(&files);
        assert!(violations.is_empty(), "{violations:#?}");
    }
}
