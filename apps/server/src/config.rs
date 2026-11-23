use std::net::SocketAddr;

pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

#[derive(PartialEq, Eq)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub database_url: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("DATABASE_URL is not set")]
    MissingDatabaseUrl,
    #[error("DATABASE_URL must start with postgres:// or postgresql://")]
    NotPostgres,
    #[error("BIND_ADDR {0:?} is not a socket address like 0.0.0.0:8080")]
    InvalidBindAddr(String),
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &"<redacted>")
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let raw_bind_addr = lookup("BIND_ADDR").unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string());
        let bind_addr = raw_bind_addr
            .parse()
            .map_err(|_| ConfigError::InvalidBindAddr(raw_bind_addr))?;
        let database_url = lookup("DATABASE_URL")
            .filter(|url| !url.is_empty())
            .ok_or(ConfigError::MissingDatabaseUrl)?;
        if !(database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")) {
            return Err(ConfigError::NotPostgres);
        }
        Ok(Self {
            bind_addr,
            database_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn bind_addr_defaults_to_port_8080_on_all_interfaces() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://db/bakery")]))
            .expect("config");
        assert_eq!(config.bind_addr, "0.0.0.0:8080".parse().expect("addr"));
        assert_eq!(config.database_url, "postgres://db/bakery");
    }

    #[test]
    fn bind_addr_is_read_from_the_environment() {
        let config = Config::from_lookup(lookup(&[
            ("BIND_ADDR", "127.0.0.1:9000"),
            ("DATABASE_URL", "postgres://db/bakery"),
        ]))
        .expect("config");
        assert_eq!(config.bind_addr, "127.0.0.1:9000".parse().expect("addr"));
    }

    #[test]
    fn missing_database_url_is_an_error() {
        assert_eq!(
            Config::from_lookup(lookup(&[])),
            Err(ConfigError::MissingDatabaseUrl)
        );
        assert_eq!(
            Config::from_lookup(lookup(&[("DATABASE_URL", "")])),
            Err(ConfigError::MissingDatabaseUrl)
        );
    }

    #[test]
    fn non_postgres_database_url_is_an_error() {
        assert_eq!(
            Config::from_lookup(lookup(&[("DATABASE_URL", "mysql://db/bakery")])),
            Err(ConfigError::NotPostgres)
        );
        assert!(Config::from_lookup(lookup(&[("DATABASE_URL", "postgresql://db/bakery")])).is_ok());
    }

    #[test]
    fn debug_output_hides_the_database_url() {
        let config = Config::from_lookup(lookup(&[(
            "DATABASE_URL",
            "postgres://bakery:s3cret@db/bakery",
        )]))
        .expect("config");
        let debug = format!("{config:?}");
        assert!(!debug.contains("s3cret"), "{debug}");
        assert!(debug.contains("0.0.0.0:8080"), "{debug}");
    }

    #[test]
    fn invalid_bind_addr_is_an_error() {
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("BIND_ADDR", "localhost"),
                ("DATABASE_URL", "postgres://db/bakery"),
            ])),
            Err(ConfigError::InvalidBindAddr("localhost".to_string()))
        );
    }
}
