const OVERALL_THRESHOLD: u64 = 80;
const STRICT_THRESHOLD: u64 = 95;
const STRICT_CRATES: &[&str] = &["domain", "orders", "payments", "recipes", "labels"];
const CRATE_DIRS: &[&str] = &["crates", "apps"];
const EXCLUDED: &[&str] = &["testkit"];

#[derive(Debug, Default, Clone, Copy)]
struct Lines {
    found: u64,
    hit: u64,
}

impl Lines {
    fn add(&mut self, other: Lines) {
        self.found += other.found;
        self.hit += other.hit;
    }

    fn below(self, threshold: u64) -> bool {
        self.found > 0 && self.hit * 100 < threshold * self.found
    }

    fn percent(self) -> String {
        let hundredths = self.hit * 10_000 / self.found.max(1);
        format!("{}.{:02}%", hundredths / 100, hundredths % 100)
    }
}

/// Paths must be relative to the workspace root (`cargo llvm-cov --remap-path-prefix`).
fn crate_of(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    let dir = parts.next()?;
    let name = parts.next()?;
    parts.next()?;
    (CRATE_DIRS.contains(&dir) && !EXCLUDED.contains(&name)).then_some(name)
}

fn parse(report: &str) -> Result<Vec<(String, Lines)>, String> {
    let mut files = Vec::new();
    let mut path = None;
    let (mut found, mut hit) = (None, None);
    for line in report.lines() {
        if let Some(value) = line.strip_prefix("SF:") {
            path = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("LF:") {
            found = Some(count(value)?);
        } else if let Some(value) = line.strip_prefix("LH:") {
            hit = Some(count(value)?);
        } else if line == "end_of_record" {
            let file = path.take().ok_or("lcov record without SF line")?;
            match (found.take(), hit.take()) {
                (Some(found), Some(hit)) => files.push((file, Lines { found, hit })),
                _ => return Err(format!("lcov record for {file} lacks LF or LH")),
            }
        }
    }
    Ok(files)
}

fn count(value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("invalid line count {value:?} in lcov report"))
}

pub fn check(report: &str) -> Result<Vec<String>, String> {
    let mut overall = Lines::default();
    let mut crates: Vec<(&str, Lines)> = Vec::new();
    let files = parse(report)?;
    for (path, lines) in &files {
        let Some(name) = crate_of(path) else {
            continue;
        };
        overall.add(*lines);
        match crates
            .iter_mut()
            .find(|(crate_name, _)| *crate_name == name)
        {
            Some((_, total)) => total.add(*lines),
            None => crates.push((name, *lines)),
        }
    }
    if crates.is_empty() {
        return Err("no files in the report belong to a workspace crate".to_string());
    }
    let mut violations = Vec::new();
    if overall.below(OVERALL_THRESHOLD) {
        violations.push(format!(
            "overall line coverage {} is below {OVERALL_THRESHOLD}%",
            overall.percent()
        ));
    }
    crates.sort_by_key(|(name, _)| *name);
    for (name, lines) in crates {
        if STRICT_CRATES.contains(&name) && lines.below(STRICT_THRESHOLD) {
            violations.push(format!(
                "{name} line coverage {} is below {STRICT_THRESHOLD}%",
                lines.percent()
            ));
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str, found: u64, hit: u64) -> String {
        format!("SF:{path}\nDA:1,1\nLF:{found}\nLH:{hit}\nend_of_record\n")
    }

    fn report(records: &[(&str, u64, u64)]) -> String {
        records
            .iter()
            .map(|(path, found, hit)| record(path, *found, *hit))
            .collect()
    }

    #[test]
    fn all_above_threshold_passes() {
        let lcov = report(&[
            ("crates/domain/src/lib.rs", 100, 96),
            ("crates/orders/src/lib.rs", 100, 100),
            ("crates/catalog/src/lib.rs", 100, 81),
            ("apps/server/src/main.rs", 100, 90),
        ]);
        assert_eq!(check(&lcov), Ok(Vec::new()));
    }

    #[test]
    fn overall_below_threshold_fails() {
        let lcov = report(&[
            ("crates/domain/src/lib.rs", 10, 10),
            ("crates/catalog/src/lib.rs", 100, 70),
            ("apps/server/src/main.rs", 100, 70),
        ]);
        let violations = check(&lcov).expect("report parses");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("overall"), "{violations:?}");
        assert!(violations[0].contains("71.42%"), "{violations:?}");
        assert!(violations[0].contains("80%"), "{violations:?}");
    }

    #[test]
    fn strict_crate_below_threshold_fails_while_overall_passes() {
        let lcov = report(&[
            ("crates/domain/src/lib.rs", 100, 100),
            ("crates/domain/src/money.rs", 100, 88),
            ("crates/catalog/src/lib.rs", 1000, 990),
        ]);
        let violations = check(&lcov).expect("report parses");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].starts_with("domain"), "{violations:?}");
        assert!(violations[0].contains("94.00%"), "{violations:?}");
        assert!(violations[0].contains("95%"), "{violations:?}");
    }

    #[test]
    fn every_failing_strict_crate_is_listed() {
        let lcov = report(&[
            ("crates/payments/src/lib.rs", 100, 90),
            ("crates/labels/src/lib.rs", 100, 94),
            ("crates/recipes/src/lib.rs", 100, 95),
        ]);
        let violations = check(&lcov).expect("report parses");
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(violations.iter().any(|v| v.starts_with("payments")));
        assert!(violations.iter().any(|v| v.starts_with("labels")));
    }

    #[test]
    fn crate_without_instrumented_lines_passes() {
        let lcov = report(&[
            ("crates/domain/src/lib.rs", 0, 0),
            ("crates/catalog/src/lib.rs", 10, 9),
        ]);
        assert_eq!(check(&lcov), Ok(Vec::new()));
    }

    #[test]
    fn report_without_instrumented_lines_passes() {
        let lcov = report(&[("crates/domain/src/lib.rs", 0, 0)]);
        assert_eq!(check(&lcov), Ok(Vec::new()));
    }

    #[test]
    fn files_outside_counted_crates_are_ignored() {
        let lcov = report(&[
            ("crates/domain/src/lib.rs", 10, 10),
            ("xtask/src/main.rs", 100, 0),
            ("crates/testkit/src/lib.rs", 100, 0),
            ("build/generated.rs", 100, 0),
        ]);
        assert_eq!(check(&lcov), Ok(Vec::new()));
    }

    #[test]
    fn report_with_no_workspace_files_is_an_error() {
        let lcov = report(&[("/build/source/crates/domain/src/lib.rs", 10, 10)]);
        let error = check(&lcov).expect_err("absolute paths are not mapped");
        assert!(error.contains("no files"), "{error}");
    }

    #[test]
    fn record_without_line_counts_is_an_error() {
        let lcov = "SF:crates/domain/src/lib.rs\nDA:1,1\nend_of_record\n";
        assert!(check(lcov).is_err());
    }
}
