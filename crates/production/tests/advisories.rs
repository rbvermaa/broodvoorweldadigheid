#![forbid(unsafe_code)]

use std::path::Path;

const IGNORED: [&str; 2] = ["RUSTSEC-2026-0194", "RUSTSEC-2026-0195"];

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Versions of `name` in a Cargo.lock, as (major, minor).
fn locked_versions(lock: &str, name: &str) -> Vec<(u64, u64)> {
    let needle = format!("name = \"{name}\"");
    let mut versions = Vec::new();
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() != needle {
            continue;
        }
        let version = lines
            .next()
            .and_then(|l| l.trim().strip_prefix("version = \""))
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or_else(|| panic!("{name} has no version line"));
        let mut parts = version.split('.').map(|p| p.parse::<u64>().unwrap());
        versions.push((parts.next().unwrap(), parts.next().unwrap()));
    }
    versions
}

fn ignore_entries(audit: &str) -> Vec<&'static str> {
    let active: String = audit
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    IGNORED
        .into_iter()
        .filter(|id| active.contains(id))
        .collect()
}

#[test]
fn quick_xml_ignores_go_once_the_old_quick_xml_is_gone() {
    let audit = read(".cargo/audit.toml");
    let ignored = ignore_entries(&audit);
    let lock = read("Cargo.lock");
    let vulnerable = locked_versions(&lock, "quick-xml")
        .into_iter()
        .any(|version| version < (0, 41));
    assert!(
        vulnerable || ignored.is_empty(),
        "no quick-xml below 0.41 is left in Cargo.lock: remove {ignored:?} from \
         .cargo/audit.toml and the accepted advisories in ADR 0016"
    );
}

#[test]
fn the_lock_parser_finds_every_version() {
    let lock = "[[package]]\nname = \"quick-xml\"\nversion = \"0.38.4\"\n\n\
        [[package]]\nname = \"quick-xml\"\nversion = \"0.42.0\"\n";
    assert_eq!(locked_versions(lock, "quick-xml"), [(0, 38), (0, 42)]);
    assert!(locked_versions(lock, "serde").is_empty());
}

#[test]
fn commented_ids_do_not_count_as_ignored() {
    assert!(ignore_entries("# RUSTSEC-2026-0194\n[advisories]\nignore = []\n").is_empty());
    assert_eq!(
        ignore_entries("ignore = [\"RUSTSEC-2026-0195\"]"),
        ["RUSTSEC-2026-0195"]
    );
}
