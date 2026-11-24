use std::net::{IpAddr, SocketAddr};

pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

#[derive(PartialEq, Eq)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub database_url: String,
    /// Off only for local runs over plain http; browsers drop Secure cookies there.
    pub cookie_secure: bool,
    /// The reverse proxy whose X-Forwarded-For is believed for rate limiting.
    pub trusted_proxy: Option<IpAddr>,
    /// Serves the component gallery at `/gallery`. Off unless `GALLERY=true`.
    pub gallery: bool,
    /// Lets anyone sign in as any staff member at `/staff/dev-login`. Off unless
    /// `DEV_STAFF_LOGIN=true`, and only allowed with a loopback `BIND_ADDR`.
    pub dev_staff_login: bool,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("DATABASE_URL is not set")]
    MissingDatabaseUrl,
    #[error("DATABASE_URL must start with postgres:// or postgresql://")]
    NotPostgres,
    #[error("BIND_ADDR {0:?} is not a socket address like 0.0.0.0:8080")]
    InvalidBindAddr(String),
    #[error("COOKIE_SECURE {0:?} must be true or false")]
    InvalidCookieSecure(String),
    #[error("TRUSTED_PROXY {0:?} is not an IP address like 10.89.0.10")]
    InvalidTrustedProxy(String),
    #[error("GALLERY {0:?} must be true or false")]
    InvalidGallery(String),
    #[error("DEV_STAFF_LOGIN {0:?} must be true or false")]
    InvalidDevStaffLogin(String),
    #[error("DEV_STAFF_LOGIN=true needs a loopback BIND_ADDR like 127.0.0.1:8080, not {0}")]
    DevStaffLoginNotLocal(SocketAddr),
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &"<redacted>")
            .field("cookie_secure", &self.cookie_secure)
            .field("trusted_proxy", &self.trusted_proxy)
            .field("gallery", &self.gallery)
            .field("dev_staff_login", &self.dev_staff_login)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let raw_bind_addr = lookup("BIND_ADDR").unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string());
        let bind_addr: SocketAddr = raw_bind_addr
            .parse()
            .map_err(|_| ConfigError::InvalidBindAddr(raw_bind_addr))?;
        let database_url = lookup("DATABASE_URL")
            .filter(|url| !url.is_empty())
            .ok_or(ConfigError::MissingDatabaseUrl)?;
        if !(database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")) {
            return Err(ConfigError::NotPostgres);
        }
        let cookie_secure = match lookup("COOKIE_SECURE").as_deref() {
            None | Some("true") => true,
            Some("false") => false,
            Some(other) => return Err(ConfigError::InvalidCookieSecure(other.to_string())),
        };
        let trusted_proxy = lookup("TRUSTED_PROXY")
            .filter(|raw| !raw.is_empty())
            .map(|raw| {
                raw.parse()
                    .map_err(|_| ConfigError::InvalidTrustedProxy(raw))
            })
            .transpose()?;
        let gallery = match lookup("GALLERY").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(other) => return Err(ConfigError::InvalidGallery(other.to_string())),
        };
        let dev_staff_login = match lookup("DEV_STAFF_LOGIN").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(other) => return Err(ConfigError::InvalidDevStaffLogin(other.to_string())),
        };
        if dev_staff_login && !bind_addr.ip().is_loopback() {
            return Err(ConfigError::DevStaffLoginNotLocal(bind_addr));
        }
        Ok(Self {
            bind_addr,
            database_url,
            cookie_secure,
            trusted_proxy,
            gallery,
            dev_staff_login,
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
        assert!(config.cookie_secure);
        assert_eq!(config.trusted_proxy, None);
        assert!(!config.gallery);
        assert!(!config.dev_staff_login);
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

    #[test]
    fn cookie_secure_can_be_turned_off_for_local_http() {
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://db/bakery"),
            ("COOKIE_SECURE", "false"),
        ]))
        .expect("config");
        assert!(!config.cookie_secure);
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://db/bakery"),
                ("COOKIE_SECURE", "nee"),
            ])),
            Err(ConfigError::InvalidCookieSecure("nee".to_string()))
        );
    }

    #[test]
    fn trusted_proxy_is_an_ip_address() {
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://db/bakery"),
            ("TRUSTED_PROXY", "10.89.0.10"),
        ]))
        .expect("config");
        assert_eq!(
            config.trusted_proxy,
            Some("10.89.0.10".parse().expect("ip"))
        );
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://db/bakery"),
                ("TRUSTED_PROXY", "caddy"),
            ])),
            Err(ConfigError::InvalidTrustedProxy("caddy".to_string()))
        );
    }

    #[test]
    fn gallery_is_off_unless_turned_on() {
        let on = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://db/bakery"),
            ("GALLERY", "true"),
        ]))
        .expect("config");
        assert!(on.gallery);
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://db/bakery"),
                ("GALLERY", "ja"),
            ])),
            Err(ConfigError::InvalidGallery("ja".to_string()))
        );
    }

    #[test]
    fn dev_staff_login_needs_a_loopback_bind_addr() {
        let local = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://db/bakery"),
            ("BIND_ADDR", "127.0.0.1:8080"),
            ("DEV_STAFF_LOGIN", "true"),
        ]))
        .expect("config");
        assert!(local.dev_staff_login);
        assert!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://db/bakery"),
                ("BIND_ADDR", "[::1]:8080"),
                ("DEV_STAFF_LOGIN", "true"),
            ]))
            .is_ok()
        );
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://db/bakery"),
                ("DEV_STAFF_LOGIN", "true"),
            ])),
            Err(ConfigError::DevStaffLoginNotLocal(
                DEFAULT_BIND_ADDR.parse().expect("addr")
            ))
        );
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://db/bakery"),
                ("BIND_ADDR", "127.0.0.1:8080"),
                ("DEV_STAFF_LOGIN", "ja"),
            ])),
            Err(ConfigError::InvalidDevStaffLogin("ja".to_string()))
        );
    }
}
