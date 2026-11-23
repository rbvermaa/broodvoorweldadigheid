use std::process::Command;

use sqlx::{AssertSqlSafe, PgConnection};

use super::Error;
use super::manifest::{Manifest, TableFingerprint};

const USER_TABLES: &str = "
    SELECT n.nspname::text, c.relname::text
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relkind = 'r'
      AND n.nspname <> 'information_schema'
      AND n.nspname NOT LIKE 'pg\\_%'
    ORDER BY 1, 2";

pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Swaps the database name in a `postgres://` URL, keeping user, host and query parameters.
pub fn with_database(url: &str, database: &str) -> Result<String, Error> {
    let authority_start = url.find("://").ok_or(Error::DatabaseUrl)? + 3;
    let rest = &url[authority_start..];
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let query = rest.find('?').map_or("", |start| &rest[start..]);
    if rest[..authority_end].is_empty() {
        return Err(Error::DatabaseUrl);
    }
    Ok(format!(
        "{}{}/{database}{query}",
        &url[..authority_start],
        &rest[..authority_end]
    ))
}

fn fingerprint_query(schema: &str, table: &str) -> String {
    format!(
        "SELECT count(*), md5(coalesce(string_agg(t::text, E'\\n' ORDER BY t::text), '')) \
         FROM {}.{} AS t",
        quote_ident(schema),
        quote_ident(table)
    )
}

pub fn pg_dump(database_url: &str, snapshot: &str) -> Vec<String> {
    vec![
        "pg_dump".to_string(),
        "--format=custom".to_string(),
        format!("--snapshot={snapshot}"),
        format!("--dbname={database_url}"),
    ]
}

pub fn pg_restore(database_url: &str) -> Command {
    let mut command = Command::new("pg_restore");
    command.args([
        "--exit-on-error",
        "--no-owner",
        "--no-privileges",
        &format!("--dbname={database_url}"),
    ]);
    command
}

pub async fn read_manifest(conn: &mut PgConnection) -> Result<Manifest, Error> {
    let tables: Vec<(String, String)> = sqlx::query_as(USER_TABLES).fetch_all(&mut *conn).await?;
    let mut manifest = Manifest::default();
    for (schema, table) in tables {
        let (rows, md5): (i64, String) =
            sqlx::query_as(AssertSqlSafe(fingerprint_query(&schema, &table)))
                .fetch_one(&mut *conn)
                .await?;
        manifest
            .tables
            .insert(format!("{schema}.{table}"), TableFingerprint { rows, md5 });
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_ident_doubles_quotes() {
        assert_eq!(quote_ident("loaves"), r#""loaves""#);
        assert_eq!(quote_ident(r#"we"ird"#), r#""we""ird""#);
    }

    #[test]
    fn with_database_replaces_the_path() {
        assert_eq!(
            with_database("postgres://app:pw@db:5432/bakflow", "scratch").expect("valid"),
            "postgres://app:pw@db:5432/scratch"
        );
    }

    #[test]
    fn with_database_keeps_socket_query() {
        assert_eq!(
            with_database("postgres://postgres@localhost/postgres?host=/tmp/sock", "x")
                .expect("valid"),
            "postgres://postgres@localhost/x?host=/tmp/sock"
        );
        assert_eq!(
            with_database("postgresql://localhost?sslmode=disable", "x").expect("valid"),
            "postgresql://localhost/x?sslmode=disable"
        );
        assert_eq!(
            with_database("postgres://localhost", "x").expect("valid"),
            "postgres://localhost/x"
        );
    }

    #[test]
    fn with_database_rejects_non_urls() {
        for url in ["host=localhost dbname=x", "postgres:///bakflow"] {
            let error = with_database(url, "x").expect_err(url);
            assert_eq!(
                error.to_string(),
                "the database URL must look like postgres://user@host/database"
            );
        }
    }

    #[test]
    fn fingerprint_query_quotes_names() {
        assert_eq!(
            fingerprint_query("public", "Loaves"),
            "SELECT count(*), md5(coalesce(string_agg(t::text, E'\\n' ORDER BY t::text), '')) \
             FROM \"public\".\"Loaves\" AS t"
        );
    }

    #[test]
    fn pg_dump_uses_the_exported_snapshot() {
        assert_eq!(
            pg_dump("postgres://db/bakflow", "00000003-0000001B-1"),
            [
                "pg_dump",
                "--format=custom",
                "--snapshot=00000003-0000001B-1",
                "--dbname=postgres://db/bakflow",
            ]
        );
    }

    #[test]
    fn pg_restore_stops_on_the_first_error() {
        let command = pg_restore("postgres://db/scratch");
        assert_eq!(command.get_program(), "pg_restore");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "--exit-on-error",
                "--no-owner",
                "--no-privileges",
                "--dbname=postgres://db/scratch",
            ]
        );
    }
}
