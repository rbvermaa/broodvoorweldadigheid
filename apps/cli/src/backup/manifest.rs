use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableFingerprint {
    pub rows: i64,
    pub fingerprint: String,
}

/// Keyed by `schema.table`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub tables: BTreeMap<String, TableFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    MissingTable(String),
    UnexpectedTable(String),
    RowCount {
        table: String,
        expected: i64,
        restored: i64,
    },
    Contents(String),
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mismatch::MissingTable(table) => write!(f, "{table}: missing after restore"),
            Mismatch::UnexpectedTable(table) => write!(f, "{table}: not in the manifest"),
            Mismatch::RowCount {
                table,
                expected,
                restored,
            } => write!(f, "{table}: expected {expected} rows, restored {restored}"),
            Mismatch::Contents(table) => write!(f, "{table}: contents differ"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatches(pub Vec<Mismatch>);

impl fmt::Display for Mismatches {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for mismatch in &self.0 {
            write!(f, "\n  {mismatch}")?;
        }
        Ok(())
    }
}

pub fn compare(expected: &Manifest, restored: &Manifest) -> Vec<Mismatch> {
    let mut mismatches = Vec::new();
    for (table, want) in &expected.tables {
        match restored.tables.get(table) {
            None => mismatches.push(Mismatch::MissingTable(table.clone())),
            Some(got) if got.rows != want.rows => mismatches.push(Mismatch::RowCount {
                table: table.clone(),
                expected: want.rows,
                restored: got.rows,
            }),
            Some(got) if got.fingerprint != want.fingerprint => {
                mismatches.push(Mismatch::Contents(table.clone()));
            }
            Some(_) => {}
        }
    }
    for table in restored.tables.keys() {
        if !expected.tables.contains_key(table) {
            mismatches.push(Mismatch::UnexpectedTable(table.clone()));
        }
    }
    mismatches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(tables: &[(&str, i64, &str)]) -> Manifest {
        Manifest {
            tables: tables
                .iter()
                .map(|(name, rows, fingerprint)| {
                    (
                        name.to_string(),
                        TableFingerprint {
                            rows: *rows,
                            fingerprint: fingerprint.to_string(),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn identical_manifests_match() {
        let expected = manifest(&[("public.loaves", 3, "abc"), ("public.orders", 0, "d41d")]);
        assert_eq!(compare(&expected, &expected.clone()), []);
    }

    #[test]
    fn reports_every_kind_of_mismatch() {
        let expected = manifest(&[
            ("public.gone", 1, "a"),
            ("public.loaves", 3, "abc"),
            ("public.orders", 2, "old"),
        ]);
        let restored = manifest(&[
            ("public.extra", 0, "e"),
            ("public.loaves", 2, "abc"),
            ("public.orders", 2, "new"),
        ]);
        let mismatches = compare(&expected, &restored);
        assert_eq!(
            mismatches,
            [
                Mismatch::MissingTable("public.gone".into()),
                Mismatch::RowCount {
                    table: "public.loaves".into(),
                    expected: 3,
                    restored: 2,
                },
                Mismatch::Contents("public.orders".into()),
                Mismatch::UnexpectedTable("public.extra".into()),
            ]
        );
        assert_eq!(
            Mismatches(mismatches).to_string(),
            "\n  public.gone: missing after restore\
             \n  public.loaves: expected 3 rows, restored 2\
             \n  public.orders: contents differ\
             \n  public.extra: not in the manifest"
        );
    }

    #[test]
    fn json_round_trip_is_stable() {
        let expected = manifest(&[("public.loaves", 3, "abc")]);
        let json = serde_json::to_string(&expected).expect("serialise");
        assert_eq!(
            json,
            r#"{"tables":{"public.loaves":{"rows":3,"fingerprint":"abc"}}}"#
        );
        let parsed: Manifest = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed, expected);
    }
}
