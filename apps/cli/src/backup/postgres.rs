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

fn percent_decode(text: &str) -> Result<String, Error> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3).ok_or(Error::DatabaseUrl)?;
            decoded.push(u8::from_str_radix(hex, 16).map_err(|_| Error::DatabaseUrl)?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| Error::DatabaseUrl)
}

/// libpq lets these query parameters override the database in the URL path.
fn names_a_database(query: &str) -> Result<bool, Error> {
    for pair in query.trim_start_matches('?').split('&') {
        let key = percent_decode(pair.split('=').next().unwrap_or_default())?;
        if key.eq_ignore_ascii_case("dbname") || key.eq_ignore_ascii_case("database") {
            return Ok(true);
        }
    }
    Ok(false)
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
    if names_a_database(query)? {
        return Err(Error::DatabaseInQuery);
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

/// A connection URL for libpq tools with the password moved out of it: a password on a
/// command line shows up in `ps` and in rustic's snapshot metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Libpq {
    pub url: String,
    password: Option<String>,
}

impl Libpq {
    pub fn parse(url: &str) -> Result<Libpq, Error> {
        let authority_start = url.find("://").ok_or(Error::DatabaseUrl)? + 3;
        let rest = &url[authority_start..];
        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let (path, query) = rest[authority_end..]
            .split_once('?')
            .unwrap_or((&rest[authority_end..], ""));

        let mut password = None;
        let authority = match authority.rsplit_once('@') {
            Some((userinfo, host)) => match userinfo.split_once(':') {
                Some((user, secret)) => {
                    password = Some(percent_decode(secret)?);
                    format!("{user}@{host}")
                }
                None => authority.to_string(),
            },
            None => authority.to_string(),
        };
        let mut kept = Vec::new();
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if percent_decode(key)?.eq_ignore_ascii_case("password") {
                password = Some(percent_decode(value)?);
            } else {
                kept.push(pair);
            }
        }
        let query = if kept.is_empty() {
            String::new()
        } else {
            format!("?{}", kept.join("&"))
        };
        Ok(Libpq {
            url: format!("{}{authority}{path}{query}", &url[..authority_start]),
            password,
        })
    }

    pub fn apply(&self, command: &mut Command) {
        if let Some(password) = &self.password {
            command.env("PGPASSWORD", password);
        }
    }
}

pub fn pg_restore(database: &Libpq) -> Command {
    let mut command = Command::new("pg_restore");
    command.args([
        "--exit-on-error",
        "--no-owner",
        "--no-privileges",
        &format!("--dbname={}", database.url),
    ]);
    database.apply(&mut command);
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
    fn with_database_rejects_a_database_in_the_query() {
        for url in [
            "postgres://db/postgres?dbname=live",
            "postgres://db/postgres?host=/s&database=live",
            "postgres://db/postgres?sslmode=disable&DBNAME=live",
            "postgres://db/postgres?db%6Eame=live",
        ] {
            let error = with_database(url, "scratch").expect_err(url);
            assert_eq!(
                error.to_string(),
                "the database URL must not name a database in its query (dbname=, database=)"
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
    fn libpq_moves_the_password_out_of_the_url() {
        let cases = [
            (
                "postgres://app:s3cret@db:5432/bakflow",
                "postgres://app@db:5432/bakflow",
            ),
            (
                "postgres://app:p%40ss%3Aw@localhost/x?host=/sock",
                "postgres://app@localhost/x?host=/sock",
            ),
            (
                "postgres://app@db/x?sslmode=disable&password=s3cret&host=/s",
                "postgres://app@db/x?sslmode=disable&host=/s",
            ),
            ("postgres://app@db/x?password=s3cret", "postgres://app@db/x"),
        ];
        for (url, stripped) in cases {
            let libpq = Libpq::parse(url).expect(url);
            assert_eq!(libpq.url, stripped);
        }
        assert_eq!(
            Libpq::parse("postgres://app:p%40ss%3Aw@localhost/x")
                .expect("valid")
                .password
                .as_deref(),
            Some("p@ss:w")
        );
        assert_eq!(
            Libpq::parse("postgres://app@db/x").expect("valid").password,
            None
        );
    }

    #[test]
    fn libpq_rejects_broken_percent_encoding() {
        let error = Libpq::parse("postgres://app:%zz@db/x").expect_err("invalid");
        assert!(matches!(error, Error::DatabaseUrl), "{error}");
    }

    #[test]
    fn libpq_passes_the_password_in_the_environment() {
        let libpq = Libpq::parse("postgres://app:s3cret@db/x").expect("valid");
        let mut command = Command::new("pg_dump");
        libpq.apply(&mut command);
        let envs: Vec<_> = command.get_envs().collect();
        assert_eq!(
            envs,
            [(
                std::ffi::OsStr::new("PGPASSWORD"),
                Some(std::ffi::OsStr::new("s3cret"))
            )]
        );

        let mut without = Command::new("pg_dump");
        Libpq::parse("postgres://app@db/x")
            .expect("valid")
            .apply(&mut without);
        assert_eq!(without.get_envs().count(), 0);
    }

    #[test]
    fn pg_restore_keeps_the_password_off_the_command_line() {
        let command = pg_restore(&Libpq::parse("postgres://app:s3cret@db/scratch").expect("valid"));
        assert!(
            command
                .get_args()
                .all(|arg| !arg.to_string_lossy().contains("s3cret"))
        );
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "PGPASSWORD" && value == Some("s3cret".as_ref()))
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
        let command = pg_restore(&Libpq::parse("postgres://db/scratch").expect("valid"));
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
