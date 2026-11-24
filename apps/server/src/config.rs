use std::net::{IpAddr, SocketAddr};

pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
pub const DEFAULT_PUBLIC_URL: &str = "https://broodvoorweldadigheid.nl";

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
    /// Where customers reach the site, without a trailing slash. Links in emails use it.
    pub public_url: String,
    /// Where the contact form (F4f) sends messages. Without it the form shows the 500 page.
    pub contact_email: Option<String>,
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
    #[error(
        "PUBLIC_URL {0:?} must be an https URL without path, like https://broodvoorweldadigheid.nl; http only for localhost or 127.0.0.1"
    )]
    InvalidPublicUrl(String),
    #[error("BAKERY_CONTACT_EMAIL {0:?} is not an email address")]
    InvalidContactEmail(String),
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &"<redacted>")
            .field("cookie_secure", &self.cookie_secure)
            .field("trusted_proxy", &self.trusted_proxy)
            .field("gallery", &self.gallery)
            .field("public_url", &self.public_url)
            .field("contact_email", &self.contact_email)
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
        let public_url = lookup("PUBLIC_URL").unwrap_or_else(|| DEFAULT_PUBLIC_URL.to_string());
        if !is_origin(&public_url) {
            return Err(ConfigError::InvalidPublicUrl(public_url));
        }
        let contact_email = lookup("BAKERY_CONTACT_EMAIL")
            .filter(|raw| !raw.is_empty())
            .map(|raw| {
                if notify::is_address(&raw) {
                    Ok(raw)
                } else {
                    Err(ConfigError::InvalidContactEmail(raw))
                }
            })
            .transpose()?;
        Ok(Self {
            bind_addr,
            database_url,
            cookie_secure,
            trusted_proxy,
            gallery,
            public_url,
            contact_email,
        })
    }
}

/// `https://`, a host of letters, digits, dots and dashes, an optional port, and nothing
/// after it. `http://` only for `localhost` and `127.0.0.1`: email links must be https
/// (ADR 0014).
fn is_origin(url: &str) -> bool {
    let (rest, plain) = match url.strip_prefix("https://") {
        Some(rest) => (rest, false),
        None => match url.strip_prefix("http://") {
            Some(rest) => (rest, true),
            None => return false,
        },
    };
    let (host, port) = rest.split_once(':').unwrap_or((rest, ""));
    !(plain && host != "localhost" && host != "127.0.0.1")
        && !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && (port.is_empty() || port.parse::<u16>().is_ok())
        && !(port.is_empty() && rest.ends_with(':'))
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
    fn public_url_must_be_an_origin() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://db/bakery")]))
            .expect("config");
        assert_eq!(config.public_url, "https://broodvoorweldadigheid.nl");
        let local = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://db/bakery"),
            ("PUBLIC_URL", "https://localhost:8443"),
        ]))
        .expect("config");
        assert_eq!(local.public_url, "https://localhost:8443");
        for loopback in ["http://localhost:8080", "http://127.0.0.1:8080"] {
            let config = Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://db/bakery"),
                ("PUBLIC_URL", loopback),
            ]))
            .expect(loopback);
            assert_eq!(config.public_url, loopback);
        }
        for bad in [
            "broodvoorweldadigheid.nl",
            "https://broodvoorweldadigheid.nl/",
            "https://broodvoorweldadigheid.nl/shop",
            "https://user@host",
            "https://host:",
            "https://host:99999",
            "ftp://host",
            "http://broodvoorweldadigheid.nl",
            "http://localhost.example.nl",
            "http://10.0.0.5:8080",
        ] {
            assert_eq!(
                Config::from_lookup(lookup(&[
                    ("DATABASE_URL", "postgres://db/bakery"),
                    ("PUBLIC_URL", bad),
                ])),
                Err(ConfigError::InvalidPublicUrl(bad.to_string())),
                "{bad}"
            );
        }
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
    fn contact_email_is_optional_and_must_be_an_address() {
        let none = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://db/bakery")]))
            .expect("config");
        assert_eq!(none.contact_email, None);
        let set = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://db/bakery"),
            ("BAKERY_CONTACT_EMAIL", "bakker@example.org"),
        ]))
        .expect("config");
        assert_eq!(set.contact_email.as_deref(), Some("bakker@example.org"));
        for bad in ["bakker", "bakker@example.org\r\nBcc: x@example.org"] {
            assert_eq!(
                Config::from_lookup(lookup(&[
                    ("DATABASE_URL", "postgres://db/bakery"),
                    ("BAKERY_CONTACT_EMAIL", bad),
                ])),
                Err(ConfigError::InvalidContactEmail(bad.to_string()))
            );
        }
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
}
