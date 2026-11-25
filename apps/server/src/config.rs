use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use auth::BootstrapOwner;
use jiff::civil::Time;

pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
pub const DEFAULT_PUBLIC_URL: &str = "https://broodvoorweldadigheid.nl";
pub const DEFAULT_OIDC_CLIENT_ID: &str = "bvw-staff";

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
    /// `DEV_STAFF_LOGIN=true`, and only allowed with a loopback `BIND_ADDR` and no
    /// `TRUSTED_PROXY`.
    pub dev_staff_login: bool,
    /// Where customers reach the shop, without a trailing slash. Mollie sends customers
    /// back and posts webhooks here, links in emails use it, and the iCal UIDs use its host.
    pub public_url: String,
    /// Without it the shop shows drops but cannot take payments.
    pub mollie_api_key_file: Option<PathBuf>,
    /// Mollie's API base URL; tests point it at a local mock.
    pub mollie_api_url: Option<String>,
    /// Local time on the day before pickup when pickup reminders go out (F9).
    pub pickup_reminder_at: Time,
    /// Where the contact form (F4f) sends messages. Without it the form shows the 500 page.
    pub contact_email: Option<String>,
    /// Staff login through Kanidm; `None` when `OIDC_ISSUER` is not set.
    pub oidc: Option<OidcConfig>,
    pub bootstrap_owner: BootstrapOwner,
}

/// The client secret and CA are files that may appear after the server starts (`make
/// kanidm-setup`), so they are read when someone logs in, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub redirect_url: String,
    pub client_secret_file: PathBuf,
    /// Extra PEM CA certificates to trust on top of the system ones.
    pub ca_file: Option<PathBuf>,
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
    #[error("DEV_STAFF_LOGIN=true cannot be combined with TRUSTED_PROXY")]
    DevStaffLoginBehindProxy,
    #[error("{0} must be set when OIDC_ISSUER is set")]
    MissingOidc(&'static str),
    #[error(
        "PUBLIC_URL {0:?} must be an https URL without path, like https://broodvoorweldadigheid.nl; http only for localhost or 127.0.0.1"
    )]
    InvalidPublicUrl(String),
    #[error("PICKUP_REMINDER_AT {0:?} must be a time like 18:00")]
    InvalidPickupReminderAt(String),
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
            .field("dev_staff_login", &self.dev_staff_login)
            .field("oidc", &self.oidc)
            .field("bootstrap_owner", &self.bootstrap_owner)
            .field("public_url", &self.public_url)
            .field("mollie_api_key_file", &self.mollie_api_key_file)
            .field("mollie_api_url", &self.mollie_api_url)
            .field("pickup_reminder_at", &self.pickup_reminder_at)
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
        if dev_staff_login && trusted_proxy.is_some() {
            return Err(ConfigError::DevStaffLoginBehindProxy);
        }
        let oidc = oidc_config(&lookup)?;
        let bootstrap_owner = BootstrapOwner {
            sub: non_empty(&lookup, "BVW_BOOTSTRAP_OWNER_SUB"),
            email: non_empty(&lookup, "BVW_BOOTSTRAP_OWNER_EMAIL"),
        };
        let public_url = lookup("PUBLIC_URL").unwrap_or_else(|| DEFAULT_PUBLIC_URL.to_string());
        if !is_origin(&public_url) {
            return Err(ConfigError::InvalidPublicUrl(public_url));
        }
        let mollie_api_key_file = lookup("MOLLIE_API_KEY_FILE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let mollie_api_url = lookup("MOLLIE_API_URL").filter(|url| !url.is_empty());
        let pickup_reminder_at = match lookup("PICKUP_REMINDER_AT").filter(|raw| !raw.is_empty()) {
            None => jobs::order_emails::DEFAULT_REMINDER_AT,
            Some(raw) => parse_hh_mm(&raw).ok_or(ConfigError::InvalidPickupReminderAt(raw))?,
        };
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
            dev_staff_login,
            oidc,
            bootstrap_owner,
            public_url,
            mollie_api_key_file,
            mollie_api_url,
            pickup_reminder_at,
            contact_email,
        })
    }

    /// The host of [`Config::public_url`], such as `broodvoorweldadigheid.nl`.
    pub fn public_host(&self) -> &str {
        let rest = self
            .public_url
            .split_once("://")
            .map_or(self.public_url.as_str(), |(_, rest)| rest);
        rest.split([':', '/']).next().unwrap_or(rest)
    }
}

/// `HH:MM`, 24-hour.
fn parse_hh_mm(raw: &str) -> Option<Time> {
    let (hour, minute) = raw.split_once(':')?;
    let digits = |part: &str| part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(hour) || !digits(minute) {
        return None;
    }
    Time::new(hour.parse().ok()?, minute.parse().ok()?, 0, 0).ok()
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

fn non_empty(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    lookup(key).filter(|value| !value.is_empty())
}

fn oidc_config(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<OidcConfig>, ConfigError> {
    let Some(issuer) = non_empty(lookup, "OIDC_ISSUER") else {
        return Ok(None);
    };
    let required = |key: &'static str| non_empty(lookup, key).ok_or(ConfigError::MissingOidc(key));
    Ok(Some(OidcConfig {
        issuer,
        client_id: non_empty(lookup, "OIDC_CLIENT_ID")
            .unwrap_or_else(|| DEFAULT_OIDC_CLIENT_ID.to_string()),
        redirect_url: required("OIDC_REDIRECT_URL")?,
        client_secret_file: required("OIDC_CLIENT_SECRET_FILE")?.into(),
        ca_file: non_empty(lookup, "OIDC_CA_FILE").map(PathBuf::from),
    }))
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
    fn pickup_reminders_default_to_six_in_the_evening() {
        let db = ("DATABASE_URL", "postgres://db/bakery");
        let config = Config::from_lookup(lookup(&[db])).expect("config");
        assert_eq!(config.pickup_reminder_at, jiff::civil::time(18, 0, 0, 0));
        let config =
            Config::from_lookup(lookup(&[db, ("PICKUP_REMINDER_AT", "07:30")])).expect("config");
        assert_eq!(config.pickup_reminder_at, jiff::civil::time(7, 30, 0, 0));
        for bad in ["18", "24:00", "7:30", "+1:00", "18:00:00"] {
            assert_eq!(
                Config::from_lookup(lookup(&[db, ("PICKUP_REMINDER_AT", bad)])),
                Err(ConfigError::InvalidPickupReminderAt(bad.to_owned())),
                "{bad}"
            );
        }
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
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://db/bakery"),
                ("BIND_ADDR", "127.0.0.1:8080"),
                ("TRUSTED_PROXY", "127.0.0.1"),
                ("DEV_STAFF_LOGIN", "true"),
            ])),
            Err(ConfigError::DevStaffLoginBehindProxy)
        );
    }

    /// The server image sets no `BIND_ADDR` (so 0.0.0.0) and compose adds the proxy and
    /// Kanidm; dev login must not start next to OIDC there, whatever else is set.
    #[test]
    fn the_image_config_with_oidc_refuses_dev_login() {
        let image = [
            ("DATABASE_URL", "postgres://db/bakery"),
            ("TRUSTED_PROXY", "10.89.0.10"),
            (
                "OIDC_ISSUER",
                "https://idm.localhost:8443/oauth2/openid/bvw-staff",
            ),
            (
                "OIDC_REDIRECT_URL",
                "https://localhost:8443/staff/oidc/callback",
            ),
            ("OIDC_CLIENT_SECRET_FILE", "/run/oidc/client-secret"),
            ("DEV_STAFF_LOGIN", "true"),
        ];
        assert_eq!(
            Config::from_lookup(lookup(&image)),
            Err(ConfigError::DevStaffLoginNotLocal(
                DEFAULT_BIND_ADDR.parse().expect("addr")
            ))
        );
        let mut loopback = image.to_vec();
        loopback.push(("BIND_ADDR", "127.0.0.1:8080"));
        assert_eq!(
            Config::from_lookup(lookup(&loopback)),
            Err(ConfigError::DevStaffLoginBehindProxy)
        );
    }

    #[test]
    fn staff_login_is_off_without_an_issuer() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://db/bakery")]))
            .expect("config");
        assert_eq!(config.oidc, None);
        assert_eq!(config.bootstrap_owner, BootstrapOwner::default());
    }

    #[test]
    fn staff_login_reads_issuer_redirect_secret_and_ca() {
        let config = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://db/bakery"),
            (
                "OIDC_ISSUER",
                "https://idm.localhost:8443/oauth2/openid/bvw-staff",
            ),
            (
                "OIDC_REDIRECT_URL",
                "https://localhost:8443/staff/oidc/callback",
            ),
            ("OIDC_CLIENT_SECRET_FILE", "/run/oidc/client-secret"),
            ("OIDC_CA_FILE", "/run/oidc/kanidm-ca.pem"),
            ("BVW_BOOTSTRAP_OWNER_EMAIL", "eigenaar@example.org"),
            ("BVW_BOOTSTRAP_OWNER_SUB", ""),
        ]))
        .expect("config");
        assert_eq!(
            config.oidc,
            Some(OidcConfig {
                issuer: "https://idm.localhost:8443/oauth2/openid/bvw-staff".to_string(),
                client_id: DEFAULT_OIDC_CLIENT_ID.to_string(),
                redirect_url: "https://localhost:8443/staff/oidc/callback".to_string(),
                client_secret_file: "/run/oidc/client-secret".into(),
                ca_file: Some("/run/oidc/kanidm-ca.pem".into()),
            })
        );
        assert_eq!(
            config.bootstrap_owner,
            BootstrapOwner {
                sub: None,
                email: Some("eigenaar@example.org".to_string()),
            }
        );
    }

    #[test]
    fn staff_login_needs_a_redirect_url_and_secret_file() {
        let base = [
            ("DATABASE_URL", "postgres://db/bakery"),
            ("OIDC_ISSUER", "https://idm.test"),
        ];
        assert_eq!(
            Config::from_lookup(lookup(&base)),
            Err(ConfigError::MissingOidc("OIDC_REDIRECT_URL"))
        );
        let with_redirect = [
            base[0],
            base[1],
            ("OIDC_REDIRECT_URL", "https://shop.test/cb"),
        ];
        assert_eq!(
            Config::from_lookup(lookup(&with_redirect)),
            Err(ConfigError::MissingOidc("OIDC_CLIENT_SECRET_FILE"))
        );
    }

    #[test]
    fn public_url_is_an_origin_and_gives_the_host() {
        let config = Config::from_lookup(lookup(&[("DATABASE_URL", "postgres://db/bakery")]))
            .expect("config");
        assert_eq!(config.public_url, "https://broodvoorweldadigheid.nl");
        assert_eq!(config.public_host(), "broodvoorweldadigheid.nl");
        assert_eq!(config.mollie_api_key_file, None);
        let local = Config::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://db/bakery"),
            ("PUBLIC_URL", "https://localhost:8443"),
            ("MOLLIE_API_KEY_FILE", "/run/secrets/mollie"),
        ]))
        .expect("config");
        assert_eq!(local.public_host(), "localhost");
        assert_eq!(
            local.mollie_api_key_file,
            Some(PathBuf::from("/run/secrets/mollie"))
        );
        for bad in [
            "broodvoorweldadigheid.nl",
            "https://broodvoorweldadigheid.nl/",
            "https://broodvoorweldadigheid.nl/shop",
            "https://user@host",
            "https://host:",
            "https://host:99999",
            "ftp://host",
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
}
