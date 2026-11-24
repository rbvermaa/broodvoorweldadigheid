use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use lettre::message::header::{HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::email::{EmailSender, OutgoingEmail, SendError, SendFuture};

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// Plain connection upgraded with STARTTLS, usually port 587. The default.
    Starttls,
    /// TLS from the first byte, usually port 465.
    Tls,
    /// No encryption: only for Mailpit on the local Compose network.
    None,
}

impl TlsMode {
    const fn default_port(self) -> u16 {
        match self {
            Self::Starttls => 587,
            Self::Tls => 465,
            Self::None => 25,
        }
    }
}

/// A secret read from a file; `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub username: Option<String>,
    pub password: Option<Secret>,
    /// The From mailbox, such as `Brood voor Weldadigheid <bakker@broodvoorweldadigheid.nl>`.
    pub from: Mailbox,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SmtpConfigError {
    #[error("SMTP_HOST is not set")]
    MissingHost,
    #[error("SMTP_PORT {0:?} is not a port number")]
    InvalidPort(String),
    #[error("SMTP_TLS {0:?} must be starttls, tls or none")]
    InvalidTls(String),
    #[error("SMTP_USERNAME and SMTP_PASSWORD_FILE must be set together")]
    HalfCredentials,
    #[error("SMTP_TLS=none cannot be used with a login: the password would go out unencrypted")]
    LoginWithoutTls,
    #[error("cannot read SMTP_PASSWORD_FILE {path}: {error}")]
    PasswordFile { path: String, error: String },
    #[error("SMTP_PASSWORD_FILE {0} is empty")]
    EmptyPassword(String),
    #[error("MAIL_FROM is not set")]
    MissingFrom,
    #[error("MAIL_FROM {0:?} is not a mailbox like Naam <adres@domein.nl>")]
    InvalidFrom(String),
    #[error("cannot set up the SMTP transport: {0}")]
    Transport(String),
}

impl SmtpConfig {
    /// # Errors
    /// [`SmtpConfigError`] for a missing or invalid variable or an unreadable password file.
    pub fn from_env() -> Result<Self, SmtpConfigError> {
        Self::from_lookup(
            |key| std::env::var(key).ok(),
            |path| std::fs::read_to_string(path).map_err(|error| error.to_string()),
        )
    }

    /// `SMTP_HOST`, `SMTP_PORT`, `SMTP_TLS` (starttls, tls or none), `SMTP_USERNAME`,
    /// `SMTP_PASSWORD_FILE` and `MAIL_FROM`.
    ///
    /// # Errors
    /// See [`SmtpConfig::from_env`].
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
        read_file: impl Fn(&Path) -> Result<String, String>,
    ) -> Result<Self, SmtpConfigError> {
        let set = |key: &str| lookup(key).filter(|value| !value.trim().is_empty());
        let host = set("SMTP_HOST").ok_or(SmtpConfigError::MissingHost)?;
        let tls = match set("SMTP_TLS").as_deref() {
            None | Some("starttls") => TlsMode::Starttls,
            Some("tls") => TlsMode::Tls,
            Some("none") => TlsMode::None,
            Some(other) => return Err(SmtpConfigError::InvalidTls(other.to_owned())),
        };
        let port = match set("SMTP_PORT") {
            None => tls.default_port(),
            Some(raw) => raw
                .parse()
                .ok()
                .filter(|port| *port != 0)
                .ok_or(SmtpConfigError::InvalidPort(raw))?,
        };
        let (username, password) = match (set("SMTP_USERNAME"), set("SMTP_PASSWORD_FILE")) {
            (None, None) => (None, None),
            (Some(username), Some(path)) => {
                let password = read_file(Path::new(&path))
                    .map_err(|error| SmtpConfigError::PasswordFile {
                        path: path.clone(),
                        error,
                    })?
                    .trim_end_matches(['\r', '\n'])
                    .to_owned();
                if password.is_empty() {
                    return Err(SmtpConfigError::EmptyPassword(path));
                }
                (Some(username), Some(Secret(password)))
            }
            _ => return Err(SmtpConfigError::HalfCredentials),
        };
        if tls == TlsMode::None && username.is_some() {
            return Err(SmtpConfigError::LoginWithoutTls);
        }
        let raw_from = set("MAIL_FROM").ok_or(SmtpConfigError::MissingFrom)?;
        let from =
            Mailbox::from_str(&raw_from).map_err(|_| SmtpConfigError::InvalidFrom(raw_from))?;
        Ok(Self {
            host,
            port,
            tls,
            username,
            password,
            from,
        })
    }
}

/// Sends through the SMTP relay with lettre.
pub struct SmtpSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpSender {
    /// Call it, and drop the sender, inside a Tokio runtime: the connection pool needs one.
    ///
    /// # Errors
    /// [`SmtpConfigError::Transport`] when TLS cannot be set up for the host.
    pub fn new(config: &SmtpConfig) -> Result<Self, SmtpConfigError> {
        let transport =
            |e: lettre::transport::smtp::Error| SmtpConfigError::Transport(e.to_string());
        let mut builder = match config.tls {
            TlsMode::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
                .map_err(transport)?,
            TlsMode::Tls => {
                AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host).map_err(transport)?
            }
            TlsMode::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host),
        }
        .port(config.port)
        .timeout(Some(TIMEOUT))
        .hello_name(ClientId::Domain(config.from.email.domain().to_owned()));
        if let (Some(username), Some(password)) = (&config.username, &config.password) {
            builder = builder.credentials(Credentials::new(
                username.clone(),
                password.expose().to_owned(),
            ));
        }
        Ok(Self {
            transport: builder.build(),
            from: config.from.clone(),
        })
    }
}

impl EmailSender for SmtpSender {
    fn send<'a>(&'a self, email: &'a OutgoingEmail) -> SendFuture<'a> {
        Box::pin(async move {
            let message = message(&self.from, email)?;
            self.transport
                .send(message)
                .await
                .map(|_| ())
                .map_err(|error| {
                    let code = error.status().map(|code| code.to_string());
                    if error.is_permanent() && is_recipient_refusal(code.as_deref()) {
                        SendError::Permanent(error.to_string())
                    } else {
                        SendError::Transient(error.to_string())
                    }
                })
        })
    }
}

/// Only replies about the recipient mailbox are final. Other 5xx replies, such as 535
/// (authentication failed) or 554, usually mean the relay or our settings are wrong, and
/// would otherwise fail every queued email at once.
fn is_recipient_refusal(code: Option<&str>) -> bool {
    matches!(code, Some("550" | "551" | "553"))
}

/// The MIME message: multipart/alternative with the text part first, a Message-ID on the
/// From domain and, on drop alerts, the RFC 8058 one-click unsubscribe headers.
///
/// # Errors
/// [`SendError::Permanent`] when the recipient is not a valid address.
pub fn message(from: &Mailbox, outgoing: &OutgoingEmail) -> Result<Message, SendError> {
    let email = &outgoing.email;
    let to = Mailbox::from_str(&email.to)
        .map_err(|error| SendError::Permanent(format!("recipient: {error}")))?;
    let mut builder = Message::builder()
        .from(from.clone())
        .to(to)
        .subject(email.subject.clone())
        .message_id(Some(format!("<{}@{}>", outgoing.id, from.email.domain())))
        .date(SystemTime::from(outgoing.date));
    if let Some(reply_to) = &email.reply_to {
        let reply_to = Mailbox::from_str(reply_to)
            .map_err(|error| SendError::Permanent(format!("reply-to: {error}")))?;
        builder = builder.reply_to(reply_to);
    }
    let mut message = builder
        .multipart(MultiPart::alternative_plain_html(
            email.text.clone(),
            email.html.clone(),
        ))
        .map_err(|error| SendError::Permanent(error.to_string()))?;
    if let Some(url) = &email.list_unsubscribe {
        let headers = message.headers_mut();
        headers.insert_raw(HeaderValue::new(
            HeaderName::new_from_ascii_str("List-Unsubscribe"),
            format!("<{url}>"),
        ));
        headers.insert_raw(HeaderValue::new(
            HeaderName::new_from_ascii_str("List-Unsubscribe-Post"),
            "List-Unsubscribe=One-Click".to_owned(),
        ));
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mailbox_refusals_are_final() {
        for code in ["550", "551", "553"] {
            assert!(is_recipient_refusal(Some(code)), "{code}");
        }
        for code in [Some("535"), Some("530"), Some("554"), Some("552"), None] {
            assert!(!is_recipient_refusal(code), "{code:?}");
        }
    }

    fn lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    /// The pooled transport needs a Tokio runtime, also when it is dropped.
    fn builds(config: &SmtpConfig) -> bool {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async { SmtpSender::new(config).is_ok() })
    }

    fn no_files(_: &Path) -> Result<String, String> {
        Err("no such file".to_owned())
    }

    const FROM: (&str, &str) = (
        "MAIL_FROM",
        "Brood voor Weldadigheid <bakker@broodvoorweldadigheid.nl>",
    );

    #[test]
    fn defaults_to_starttls_on_587_without_login() {
        let config =
            SmtpConfig::from_lookup(lookup(&[("SMTP_HOST", "smtp.example.org"), FROM]), no_files)
                .expect("config");
        assert_eq!(config.host, "smtp.example.org");
        assert_eq!(config.port, 587);
        assert_eq!(config.tls, TlsMode::Starttls);
        assert_eq!(config.username, None);
        assert_eq!(config.password, None);
        assert_eq!(
            config.from.email.to_string(),
            "bakker@broodvoorweldadigheid.nl"
        );
        assert!(builds(&config));
    }

    #[test]
    fn tls_modes_pick_their_port() {
        for (mode, tls, port) in [
            ("tls", TlsMode::Tls, 465),
            ("none", TlsMode::None, 25),
            ("starttls", TlsMode::Starttls, 587),
        ] {
            let config = SmtpConfig::from_lookup(
                lookup(&[("SMTP_HOST", "mailpit"), ("SMTP_TLS", mode), FROM]),
                no_files,
            )
            .expect("config");
            assert_eq!((config.tls, config.port), (tls, port));
            assert!(builds(&config));
        }
        let config = SmtpConfig::from_lookup(
            lookup(&[
                ("SMTP_HOST", "mailpit"),
                ("SMTP_TLS", "none"),
                ("SMTP_PORT", "1025"),
                FROM,
            ]),
            no_files,
        )
        .expect("config");
        assert_eq!(config.port, 1025);
    }

    #[test]
    fn invalid_values_are_errors() {
        let host = ("SMTP_HOST", "smtp.example.org");
        let cases: [(&[(&str, &str)], SmtpConfigError); 7] = [
            (&[FROM], SmtpConfigError::MissingHost),
            (&[("SMTP_HOST", " "), FROM], SmtpConfigError::MissingHost),
            (&[host], SmtpConfigError::MissingFrom),
            (
                &[host, ("MAIL_FROM", "bakker")],
                SmtpConfigError::InvalidFrom("bakker".to_owned()),
            ),
            (
                &[host, ("SMTP_PORT", "0"), FROM],
                SmtpConfigError::InvalidPort("0".to_owned()),
            ),
            (
                &[host, ("SMTP_TLS", "ssl"), FROM],
                SmtpConfigError::InvalidTls("ssl".to_owned()),
            ),
            (
                &[host, ("SMTP_USERNAME", "bakker"), FROM],
                SmtpConfigError::HalfCredentials,
            ),
        ];
        for (vars, error) in cases {
            assert_eq!(SmtpConfig::from_lookup(lookup(vars), no_files), Err(error));
        }
    }

    #[test]
    fn the_password_comes_from_a_file_and_never_shows_in_debug() {
        let vars = [
            ("SMTP_HOST", "smtp.example.org"),
            ("SMTP_USERNAME", "bakker"),
            ("SMTP_PASSWORD_FILE", "/run/secrets/smtp_password"),
            FROM,
        ];
        let config = SmtpConfig::from_lookup(lookup(&vars), |path| {
            assert_eq!(path, Path::new("/run/secrets/smtp_password"));
            Ok("hunter2-geheim\n".to_owned())
        })
        .expect("config");
        assert_eq!(
            config.password.as_ref().map(Secret::expose),
            Some("hunter2-geheim")
        );
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(debug.contains("smtp.example.org"), "{debug}");
        assert!(builds(&config));

        let mut plain = vars.to_vec();
        plain.push(("SMTP_TLS", "none"));
        assert_eq!(
            SmtpConfig::from_lookup(lookup(&plain), |_| Ok("geheim".to_owned())),
            Err(SmtpConfigError::LoginWithoutTls)
        );

        assert_eq!(
            SmtpConfig::from_lookup(lookup(&vars), |_| Ok("\n".to_owned())),
            Err(SmtpConfigError::EmptyPassword(
                "/run/secrets/smtp_password".to_owned()
            ))
        );
        assert_eq!(
            SmtpConfig::from_lookup(lookup(&vars), no_files),
            Err(SmtpConfigError::PasswordFile {
                path: "/run/secrets/smtp_password".to_owned(),
                error: "no such file".to_owned(),
            })
        );
    }
}
