use std::net::SocketAddr;

pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub database_url: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("DATABASE_URL is not set")]
    MissingDatabaseUrl,
    #[error("BIND_ADDR {0:?} is not a socket address like 0.0.0.0:8080")]
    InvalidBindAddr(String),
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
