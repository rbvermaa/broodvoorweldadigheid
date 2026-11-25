//! A small OpenID Connect client for staff login through Kanidm: discovery, the
//! authorization code flow with PKCE, and ID tokens signed with ES256 only (ADR 0010).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jiff::{SignedDuration, Timestamp};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

use crate::identity::{Identity, canonical_uuid};

pub const SCOPE: &str = "openid email";
/// Room for clock drift between Kanidm and this server.
pub const LEEWAY: SignedDuration = SignedDuration::from_secs(60);
const RANDOM_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    fn get(url: &str) -> Self {
        Self {
            method: Method::Get,
            url: url.to_string(),
            headers: vec![("accept", "application/json".to_string())],
            body: Vec::new(),
        }
    }

    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HttpError(pub String);

pub type HttpFuture = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send>>;

/// The provider's HTTP endpoints, behind a trait so tests can answer without a network.
pub trait OidcHttp: Send + Sync {
    fn send(&self, request: HttpRequest) -> HttpFuture;
}

#[derive(Clone, PartialEq, Eq)]
pub struct Settings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_url", &self.redirect_url)
            .finish()
    }
}

/// What the callback needs from the login request. Kept in the session between the two.
#[derive(Clone, Serialize, Deserialize)]
pub struct PendingLogin {
    state: String,
    nonce: String,
    pkce_verifier: String,
}

impl std::fmt::Debug for PendingLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingLogin(<redacted>)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("invalid OIDC setting {name}: {reason}")]
    Config { name: &'static str, reason: String },
    #[error("OIDC discovery failed: {0}")]
    Discovery(String),
    #[error("the returned state does not match the login request")]
    StateMismatch,
    #[error("the provider refused the code: {0}")]
    Exchange(String),
    #[error("the token endpoint cannot be reached: {0}")]
    Unreachable(String),
    #[error("the token response has no ID token")]
    NoIdToken,
    #[error("ID token signed with a key that is not in the provider's key set")]
    UnknownKey,
    #[error("ID token rejected: {0}")]
    InvalidIdToken(String),
}

fn invalid(reason: impl Into<String>) -> OidcError {
    OidcError::InvalidIdToken(reason.into())
}

#[derive(Deserialize)]
struct Metadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct KeySet {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: Option<String>,
    crv: Option<String>,
    kid: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    x: Option<String>,
    y: Option<String>,
}

impl Jwk {
    /// The `kid` and uncompressed point `0x04 || x || y` of a P-256 signing key, or `None`
    /// for any other key.
    fn es256_point(&self) -> Option<(String, Vec<u8>)> {
        let ec = self.kty.as_deref() == Some("EC") && self.crv.as_deref() == Some("P-256");
        let es256 = self.alg.as_deref().is_none_or(|alg| alg == "ES256");
        let signing = self.usage.as_deref().is_none_or(|usage| usage == "sig");
        if !(ec && es256 && signing) {
            return None;
        }
        let x = URL_SAFE_NO_PAD.decode(self.x.as_deref()?).ok()?;
        let y = URL_SAFE_NO_PAD.decode(self.y.as_deref()?).ok()?;
        if x.len() != 32 || y.len() != 32 {
            return None;
        }
        let mut point = Vec::with_capacity(65);
        point.push(0x04);
        point.extend_from_slice(&x);
        point.extend_from_slice(&y);
        Some((self.kid.clone()?, point))
    }
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: Option<String>,
    crit: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Audience,
    azp: Option<String>,
    exp: i64,
    iat: i64,
    nonce: Option<String>,
    name: Option<String>,
    preferred_username: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
}

/// The provider as discovery found it: its endpoints and ES256 keys by `kid`.
pub struct Provider {
    settings: Settings,
    authorization_endpoint: String,
    token_endpoint: String,
    keys: HashMap<String, Vec<u8>>,
}

impl Provider {
    /// Reads the provider's metadata and signing keys.
    ///
    /// # Errors
    /// `Config` for a malformed setting, `Discovery` when the provider cannot be read, names
    /// another issuer, or has no ES256 key.
    pub async fn discover(settings: &Settings, http: &dyn OidcHttp) -> Result<Self, OidcError> {
        check_url("issuer", &settings.issuer)?;
        check_url("redirect_url", &settings.redirect_url)?;
        let url = format!(
            "{}/.well-known/openid-configuration",
            settings.issuer.trim_end_matches('/')
        );
        let metadata: Metadata = get_json(http, &url).await?;
        if metadata.issuer != settings.issuer {
            return Err(OidcError::Discovery(format!(
                "the provider calls itself {:?}, not {:?}",
                metadata.issuer, settings.issuer
            )));
        }
        for (name, endpoint) in [
            ("authorization_endpoint", &metadata.authorization_endpoint),
            ("token_endpoint", &metadata.token_endpoint),
            ("jwks_uri", &metadata.jwks_uri),
        ] {
            if !is_https(endpoint) {
                return Err(OidcError::Discovery(format!(
                    "{name} {endpoint:?} is not an https URL"
                )));
            }
        }
        let key_set: KeySet = get_json(http, &metadata.jwks_uri).await?;
        let keys: HashMap<String, Vec<u8>> =
            key_set.keys.iter().filter_map(Jwk::es256_point).collect();
        if keys.is_empty() {
            return Err(OidcError::Discovery(
                "the key set has no ES256 key with a kid".to_string(),
            ));
        }
        Ok(Self {
            settings: settings.clone(),
            authorization_endpoint: metadata.authorization_endpoint,
            token_endpoint: metadata.token_endpoint,
            keys,
        })
    }

    /// The URL to send the browser to, with PKCE (S256), state and nonce.
    ///
    /// # Panics
    /// When the system's random number generator fails.
    #[must_use]
    pub fn begin(&self) -> (String, PendingLogin) {
        let pending = PendingLogin {
            state: random_token(),
            nonce: random_token(),
            pkce_verifier: random_token(),
        };
        let challenge = pkce_challenge(&pending.pkce_verifier);
        let query = form_urlencode(&[
            ("response_type", "code"),
            ("client_id", &self.settings.client_id),
            ("redirect_uri", &self.settings.redirect_url),
            ("scope", SCOPE),
            ("state", &pending.state),
            ("nonce", &pending.nonce),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        let separator = if self.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        (
            format!("{}{separator}{query}", self.authorization_endpoint),
            pending,
        )
    }

    /// Checks the returned state, trades the code for tokens and verifies the ID token.
    ///
    /// # Errors
    /// `StateMismatch`, `Exchange`, `Unreachable`, `NoIdToken`, `UnknownKey` or
    /// `InvalidIdToken`.
    pub async fn complete(
        &self,
        http: &dyn OidcHttp,
        pending: PendingLogin,
        state: &str,
        code: &str,
        now: Timestamp,
    ) -> Result<Identity, OidcError> {
        if !constant_time_eq(state.as_bytes(), pending.state.as_bytes()) {
            return Err(OidcError::StateMismatch);
        }
        let credentials = format!(
            "{}:{}",
            form_urlencode_value(&self.settings.client_id),
            form_urlencode_value(&self.settings.client_secret)
        );
        let request = HttpRequest {
            method: Method::Post,
            url: self.token_endpoint.clone(),
            headers: vec![
                ("accept", "application/json".to_string()),
                (
                    "content-type",
                    "application/x-www-form-urlencoded".to_string(),
                ),
                (
                    "authorization",
                    format!("Basic {}", STANDARD.encode(credentials)),
                ),
            ],
            body: form_urlencode(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.settings.redirect_url),
                ("code_verifier", &pending.pkce_verifier),
            ])
            .into_bytes(),
        };
        let response = http
            .send(request)
            .await
            .map_err(|error| OidcError::Unreachable(error.0))?;
        if response.status >= 500 {
            return Err(OidcError::Unreachable(format!("HTTP {}", response.status)));
        }
        if response.status != 200 {
            let error = serde_json::from_slice::<OAuthError>(&response.body)
                .map_or_else(|_| String::new(), |body| body.error);
            return Err(OidcError::Exchange(format!(
                "HTTP {} {error}",
                response.status
            )));
        }
        let tokens: TokenResponse = serde_json::from_slice(&response.body)
            .map_err(|error| OidcError::Exchange(format!("token response: {error}")))?;
        let id_token = tokens.id_token.ok_or(OidcError::NoIdToken)?;
        self.verify(&id_token, &pending.nonce, now)
    }

    /// Accepts only a compact JWS with `alg` ES256, a `kid` from the key set, a valid
    /// signature, this issuer, this client as audience, the nonce of this login, and an
    /// `exp` and `iat` that fit `now`.
    ///
    /// # Errors
    /// `UnknownKey` or `InvalidIdToken`.
    pub fn verify(
        &self,
        id_token: &str,
        nonce: &str,
        now: Timestamp,
    ) -> Result<Identity, OidcError> {
        let parts: Vec<&str> = id_token.split('.').collect();
        let [header, payload, signature] = parts[..] else {
            return Err(invalid("not a compact JWS"));
        };
        let header: Header =
            decode_json(header).map_err(|error| invalid(format!("header: {error}")))?;
        if header.alg != "ES256" {
            return Err(invalid(format!("alg {:?} is not ES256", header.alg)));
        }
        if header.crit.is_some() {
            return Err(invalid("crit header"));
        }
        let kid = header.kid.ok_or_else(|| invalid("no kid"))?;
        let key = self.keys.get(&kid).ok_or(OidcError::UnknownKey)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| invalid("signature is not base64url"))?;
        let signed = &id_token[..id_token.len() - parts[2].len() - 1];
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key)
            .verify(signed.as_bytes(), &signature)
            .map_err(|_| invalid("bad signature"))?;
        let claims: Claims =
            decode_json(payload).map_err(|error| invalid(format!("claims: {error}")))?;
        self.check_claims(claims, nonce, now)
    }

    fn check_claims(
        &self,
        claims: Claims,
        nonce: &str,
        now: Timestamp,
    ) -> Result<Identity, OidcError> {
        if claims.iss != self.settings.issuer {
            return Err(invalid(format!("issuer {:?}", claims.iss)));
        }
        let client_id = self.settings.client_id.as_str();
        let audiences = match &claims.aud {
            Audience::One(aud) => vec![aud.as_str()],
            Audience::Many(auds) => auds.iter().map(String::as_str).collect(),
        };
        if !audiences.contains(&client_id) {
            return Err(invalid(format!("audience {audiences:?}")));
        }
        let azp_ok = match claims.azp.as_deref() {
            Some(azp) => azp == client_id,
            None => audiences.len() == 1,
        };
        if !azp_ok {
            return Err(invalid(format!("authorized party {:?}", claims.azp)));
        }
        let exp = Timestamp::from_second(claims.exp).map_err(|_| invalid("exp out of range"))?;
        let iat = Timestamp::from_second(claims.iat).map_err(|_| invalid("iat out of range"))?;
        if now >= exp + LEEWAY {
            return Err(invalid(format!("expired at {exp}")));
        }
        if iat > now + LEEWAY {
            return Err(invalid(format!("issued in the future, at {iat}")));
        }
        let nonce_ok = claims
            .nonce
            .as_deref()
            .is_some_and(|got| constant_time_eq(got.as_bytes(), nonce.as_bytes()));
        if !nonce_ok {
            return Err(invalid("nonce does not match the login request"));
        }
        let sub = canonical_uuid(&claims.sub)
            .ok_or_else(|| invalid(format!("sub {:?} is not a UUID", claims.sub)))?;
        Ok(Identity {
            sub,
            name: claims.name,
            preferred_username: claims.preferred_username,
            email: claims.email,
            email_verified: claims.email_verified.unwrap_or(false),
        })
    }
}

fn decode_json<T: for<'de> Deserialize<'de>>(part: &str) -> Result<T, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| "not base64url".to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    http: &dyn OidcHttp,
    url: &str,
) -> Result<T, OidcError> {
    let response = http
        .send(HttpRequest::get(url))
        .await
        .map_err(|error| OidcError::Discovery(format!("{url}: {}", error.0)))?;
    if response.status != 200 {
        return Err(OidcError::Discovery(format!(
            "{url}: HTTP {}",
            response.status
        )));
    }
    serde_json::from_slice(&response.body)
        .map_err(|error| OidcError::Discovery(format!("{url}: {error}")))
}

fn has_host(rest: Option<&str>) -> bool {
    rest.is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
}

fn is_https(url: &str) -> bool {
    has_host(url.strip_prefix("https://")) && !url.contains(char::is_whitespace)
}

fn check_url(name: &'static str, url: &str) -> Result<(), OidcError> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    if has_host(rest) && !url.contains(char::is_whitespace) {
        Ok(())
    } else {
        Err(OidcError::Config {
            name,
            reason: format!("{url:?} is not an absolute http(s) URL"),
        })
    }
}

/// 32 random bytes, base64url without padding (43 characters).
fn random_token() -> String {
    let mut bytes = [0_u8; RANDOM_BYTES];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("system random number generator");
    URL_SAFE_NO_PAD.encode(bytes)
}

#[must_use]
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

/// `application/x-www-form-urlencoded`, also used for query strings.
#[must_use]
pub fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                form_urlencode_value(key),
                form_urlencode_value(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn form_urlencode_value(value: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The error and its sources, joined with ": ".
#[must_use]
pub fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let settings = Settings {
            issuer: "https://idm.test".to_string(),
            client_id: "bvw-staff".to_string(),
            client_secret: "s3cret".to_string(),
            redirect_url: "https://shop.test/staff/oidc/callback".to_string(),
        };
        assert!(!format!("{settings:?}").contains("s3cret"));
        let pending = PendingLogin {
            state: "state-value".to_string(),
            nonce: "nonce-value".to_string(),
            pkce_verifier: "verifier-value".to_string(),
        };
        let debug = format!("{pending:?}");
        assert!(!debug.contains("value"), "{debug}");
    }

    #[test]
    fn error_chain_joins_the_causes() {
        #[derive(Debug, thiserror::Error)]
        #[error("outer")]
        struct Outer(#[source] HttpError);
        assert_eq!(
            error_chain(&Outer(HttpError("inner".to_string()))),
            "outer: inner"
        );
    }

    #[test]
    fn constant_time_eq_compares_length_and_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn form_encoding_escapes_everything_but_unreserved_characters() {
        assert_eq!(
            form_urlencode(&[("scope", "openid email"), ("r", "https://a.test/b?c=d&e")]),
            "scope=openid%20email&r=https%3A%2F%2Fa.test%2Fb%3Fc%3Dd%26e"
        );
        assert_eq!(form_urlencode_value("aZ09-._~"), "aZ09-._~");
    }

    #[test]
    fn the_pkce_challenge_is_base64url_of_sha256() {
        // `printf %s VERIFIER | openssl dgst -sha256 -binary | basenc --base64url`
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gXk-XULJE"),
            "Og7vtWrUzkh7bdULmbt9GuE961UutEmc8TxycPkXPFY"
        );
    }

    #[test]
    fn random_tokens_are_32_bytes_of_base64url() {
        let token = random_token();
        assert_eq!(token.len(), 43);
        assert_eq!(URL_SAFE_NO_PAD.decode(&token).expect("base64url").len(), 32);
        assert_ne!(token, random_token());
    }

    #[test]
    fn settings_must_be_absolute_urls_and_endpoints_https() {
        assert!(check_url("issuer", "https://idm.test/oauth2/openid/bvw-staff").is_ok());
        assert!(check_url("redirect_url", "http://localhost:8080/cb").is_ok());
        for bad in [
            "/relative",
            "https://",
            "https:///x",
            "idm.test",
            "https://a b",
        ] {
            assert!(check_url("issuer", bad).is_err(), "{bad}");
        }
        assert!(is_https("https://idm.test/token"));
        assert!(!is_https("http://idm.test/token"));
    }
}
