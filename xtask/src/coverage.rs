use std::collections::{BTreeMap, BTreeSet};

use crate::async_fn::{self, AsyncFn};

const OVERALL_THRESHOLD: u64 = 80;
const STRICT_THRESHOLD: u64 = 95;
const STRICT_CRATES: &[&str] = &["domain", "orders", "payments", "recipes", "labels"];
const CRATE_DIRS: &[&str] = &["crates", "apps"];
const EXCLUDED: &[&str] = &["testkit"];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    counted: Lines,
    excluded: Lines,
}

impl Totals {
    fn add(&mut self, other: Totals) {
        self.counted.add(other.counted);
        self.excluded.add(other.excluded);
    }

    fn summary(self, name: &str) -> String {
        let raw = Lines {
            found: self.counted.found + self.excluded.found,
            hit: self.counted.hit + self.excluded.hit,
        };
        format!(
            "{name} {} ({}/{} lines); {} async fn signature lines excluded, {} of them unhit; {} without exclusion",
            self.counted.percent(),
            self.counted.hit,
            self.counted.found,
            self.excluded.found,
            self.excluded.found - self.excluded.hit,
            raw.percent(),
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub summary: Vec<String>,
    pub violations: Vec<String>,
}

struct Record {
    path: String,
    lines: Lines,
    /// Execution count per line (lcov `DA`).
    hits: BTreeMap<u32, u64>,
}

/// Paths must be relative to the workspace root (`cargo llvm-cov --remap-path-prefix`).
fn crate_of(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    let dir = parts.next()?;
    let name = parts.next()?;
    parts.next()?;
    (CRATE_DIRS.contains(&dir) && !EXCLUDED.contains(&name)).then_some(name)
}

fn parse(report: &str) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    let mut path = None;
    let (mut found, mut hit) = (None, None);
    let mut hits = BTreeMap::new();
    for line in report.lines() {
        if let Some(value) = line.strip_prefix("SF:") {
            path = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("DA:") {
            let (line, count) = line_hits(value)?;
            let entry = hits.entry(line).or_insert(0);
            *entry = count.max(*entry);
        } else if let Some(value) = line.strip_prefix("LF:") {
            found = Some(count(value)?);
        } else if let Some(value) = line.strip_prefix("LH:") {
            hit = Some(count(value)?);
        } else if line == "end_of_record" {
            let file = path.take().ok_or("lcov record without SF line")?;
            let hits = std::mem::take(&mut hits);
            match (found.take(), hit.take()) {
                (Some(found), Some(_)) if found > 0 && hits.is_empty() => {
                    return Err(format!(
                        "lcov record for {file} has no DA lines; the report must not be --summary-only"
                    ));
                }
                (Some(found), Some(hit)) => records.push(Record {
                    path: file,
                    lines: Lines { found, hit },
                    hits,
                }),
                _ => return Err(format!("lcov record for {file} lacks LF or LH")),
            }
        }
    }
    Ok(records)
}

fn line_hits(value: &str) -> Result<(u32, u64), String> {
    let mut fields = value.split(',');
    let line = fields.next().and_then(|line| line.parse().ok());
    let hits = fields.next().and_then(|hits| hits.parse().ok());
    line.zip(hits)
        .ok_or_else(|| format!("invalid DA line {value:?} in lcov report"))
}

fn count(value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("invalid line count {value:?} in lcov report"))
}

/// The signature lines (from `async` through the `{` of the body) of each async fn whose
/// body has a hit line. llvm-cov counts the wrapper that builds the future apart from the
/// body, and whether it reports the wrapper as run varies between builds (ADR 0043).
fn excluded(hits: &BTreeMap<u32, u64>, functions: &[AsyncFn]) -> Lines {
    let mut lines = BTreeSet::new();
    for function in functions {
        let body_hit = function.open < function.close
            && hits
                .range(function.open + 1..=function.close)
                .any(|(_, &count)| count > 0);
        if body_hit {
            lines.extend(function.start..=function.open);
        }
    }
    let mut excluded = Lines::default();
    for count in lines.iter().filter_map(|line| hits.get(line)) {
        excluded.found += 1;
        excluded.hit += u64::from(*count > 0);
    }
    excluded
}

fn totals(record: &Record, source: &str) -> Result<Totals, String> {
    let excluded = excluded(&record.hits, &async_fn::find(source));
    let counted = record
        .lines
        .found
        .checked_sub(excluded.found)
        .zip(record.lines.hit.checked_sub(excluded.hit))
        .map(|(found, hit)| Lines { found, hit })
        .ok_or_else(|| {
            format!(
                "lcov record for {}: DA lines disagree with LF/LH",
                record.path
            )
        })?;
    Ok(Totals { counted, excluded })
}

/// `read_source` gets the `SF` path of each counted file, relative to the workspace root.
pub fn check(
    report: &str,
    read_source: impl Fn(&str) -> Result<String, String>,
) -> Result<Outcome, String> {
    let mut overall = Totals::default();
    let mut crates: Vec<(&str, Totals)> = Vec::new();
    let records = parse(report)?;
    for record in &records {
        let Some(name) = crate_of(&record.path) else {
            continue;
        };
        let file = totals(record, &read_source(&record.path)?)?;
        overall.add(file);
        match crates
            .iter_mut()
            .find(|(crate_name, _)| *crate_name == name)
        {
            Some((_, total)) => total.add(file),
            None => crates.push((name, file)),
        }
    }
    if crates.is_empty() {
        return Err("no files in the report belong to a workspace crate".to_string());
    }
    let mut summary = vec![overall.summary("overall")];
    let mut violations = Vec::new();
    if overall.counted.below(OVERALL_THRESHOLD) {
        violations.push(format!(
            "overall line coverage {} is below {OVERALL_THRESHOLD}%",
            overall.counted.percent()
        ));
    }
    crates.sort_by_key(|(name, _)| *name);
    for (name, totals) in crates {
        summary.push(totals.summary(name));
        if STRICT_CRATES.contains(&name) && totals.counted.below(STRICT_THRESHOLD) {
            violations.push(format!(
                "{name} line coverage {} is below {STRICT_THRESHOLD}%",
                totals.counted.percent()
            ));
        }
    }
    Ok(Outcome {
        summary,
        violations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violations(lcov: &str) -> Result<Vec<String>, String> {
        check(lcov, |_| Ok(String::new())).map(|outcome| outcome.violations)
    }

    const HANDLER: &str = "\
use x;

#[instrument]
pub async fn handler(
    a: u8,
    b: u8,
) -> u8 {
    let c = a + b;
    c
}

async fn untested(
    a: u8,
) {
    drop(a);
}
";

    fn line_record(path: &str, hits: &[(u32, u64)]) -> String {
        let mut record = format!("SF:{path}\n");
        for (line, count) in hits {
            record.push_str(&format!("DA:{line},{count}\n"));
        }
        let hit = hits.iter().filter(|(_, count)| *count > 0).count();
        record.push_str(&format!("LF:{}\nLH:{hit}\nend_of_record\n", hits.len()));
        record
    }

    fn handler_outcome(hits: &[(u32, u64)]) -> Outcome {
        let lcov = line_record("crates/orders/src/handler.rs", hits);
        check(&lcov, |path| {
            assert_eq!(path, "crates/orders/src/handler.rs");
            Ok(HANDLER.to_string())
        })
        .expect("report parses")
    }

    #[test]
    fn signature_lines_of_a_run_async_fn_are_excluded_whether_hit_or_not() {
        let body = [(8, 3), (9, 3), (10, 3)];
        let missed = handler_outcome(&[&[(4, 0), (5, 0), (6, 0), (7, 0)][..], &body].concat());
        let run = handler_outcome(&[&[(4, 3), (5, 3), (6, 3), (7, 3)][..], &body].concat());
        assert_eq!(missed.violations, Vec::<String>::new());
        assert_eq!(run.violations, Vec::<String>::new());
        assert_eq!(
            missed.summary[1],
            "orders 100.00% (3/3 lines); 4 async fn signature lines excluded, 4 of them unhit; 42.85% without exclusion"
        );
        assert_eq!(
            run.summary[1],
            "orders 100.00% (3/3 lines); 4 async fn signature lines excluded, 0 of them unhit; 100.00% without exclusion"
        );
    }

    #[test]
    fn signature_lines_of_an_untested_async_fn_still_count() {
        let outcome = handler_outcome(&[
            (4, 0),
            (7, 0),
            (8, 1),
            (12, 0),
            (13, 0),
            (14, 0),
            (15, 0),
            (16, 0),
        ]);
        assert_eq!(
            outcome.summary[1],
            "orders 16.66% (1/6 lines); 2 async fn signature lines excluded, 2 of them unhit; 12.50% without exclusion"
        );
        assert_eq!(outcome.violations.len(), 2, "{:?}", outcome.violations);
        assert!(
            outcome.violations[1].starts_with("orders line coverage 16.66%"),
            "{:?}",
            outcome.violations
        );
    }

    #[test]
    fn exclusions_apply_to_the_threshold_and_are_summed_per_crate() {
        let mut lcov = line_record(
            "crates/orders/src/handler.rs",
            &[(4, 0), (7, 0), (8, 1), (9, 1), (10, 1)],
        );
        lcov.push_str(&line_record("crates/orders/src/other.rs", &[(1, 1)]));
        let outcome = check(&lcov, |path| {
            Ok(if path.ends_with("handler.rs") {
                HANDLER.to_string()
            } else {
                String::new()
            })
        })
        .expect("report parses");
        assert_eq!(outcome.violations, Vec::<String>::new());
        assert_eq!(
            outcome.summary,
            [
                "overall 100.00% (4/4 lines); 2 async fn signature lines excluded, 2 of them unhit; 66.66% without exclusion",
                "orders 100.00% (4/4 lines); 2 async fn signature lines excluded, 2 of them unhit; 66.66% without exclusion",
            ]
        );
    }

    #[test]
    fn unreadable_source_is_an_error() {
        let lcov = line_record("crates/orders/src/gone.rs", &[(1, 1)]);
        let error = check(&lcov, |path| Err(format!("cannot read {path}")))
            .expect_err("source is required");
        assert!(error.contains("gone.rs"), "{error}");
    }

    #[test]
    fn summary_only_report_is_an_error() {
        let lcov = "SF:crates/domain/src/lib.rs\nLF:10\nLH:10\nend_of_record\n";
        let error = violations(lcov).expect_err("line data is required");
        assert!(error.contains("summary-only"), "{error}");
    }

    #[test]
    fn invalid_line_data_is_an_error() {
        let lcov = "SF:crates/domain/src/lib.rs\nDA:x,1\nLF:1\nLH:1\nend_of_record\n";
        assert!(violations(lcov).is_err());
    }

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
        assert_eq!(violations(&lcov), Ok(Vec::new()));
    }

    #[test]
    fn overall_below_threshold_fails() {
        let lcov = report(&[
            ("crates/domain/src/lib.rs", 10, 10),
            ("crates/catalog/src/lib.rs", 100, 70),
            ("apps/server/src/main.rs", 100, 70),
        ]);
        let violations = violations(&lcov).expect("report parses");
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
        let violations = violations(&lcov).expect("report parses");
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
        let violations = violations(&lcov).expect("report parses");
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
        assert_eq!(violations(&lcov), Ok(Vec::new()));
    }

    #[test]
    fn report_without_instrumented_lines_passes() {
        let lcov = report(&[("crates/domain/src/lib.rs", 0, 0)]);
        assert_eq!(violations(&lcov), Ok(Vec::new()));
    }

    #[test]
    fn files_outside_counted_crates_are_ignored() {
        let lcov = report(&[
            ("crates/domain/src/lib.rs", 10, 10),
            ("xtask/src/main.rs", 100, 0),
            ("crates/testkit/src/lib.rs", 100, 0),
            ("build/generated.rs", 100, 0),
        ]);
        assert_eq!(violations(&lcov), Ok(Vec::new()));
    }

    #[test]
    fn report_with_no_workspace_files_is_an_error() {
        let lcov = report(&[("/build/source/crates/domain/src/lib.rs", 10, 10)]);
        let error = violations(&lcov).expect_err("absolute paths are not mapped");
        assert!(error.contains("no files"), "{error}");
    }

    #[test]
    fn record_without_line_counts_is_an_error() {
        let lcov = "SF:crates/domain/src/lib.rs\nDA:1,1\nend_of_record\n";
        assert!(violations(lcov).is_err());
    }
}
