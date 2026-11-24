use std::future::Future;
use std::pin::Pin;
use std::time::SystemTime;

use jiff::Timestamp;
use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreJwsSigningAlgorithm, CoreProviderMetadata,
};
use openidconnect::{
    AsyncHttpClient, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
};
pub use openidconnect::{HttpRequest, HttpResponse, http, url};
use serde::{Deserialize, Serialize};

/// Kanidm signs with ES256 by default and RS256 when told to; nothing else is accepted,
/// HS256 with the client secret included.
const ALLOWED_ALGS: [CoreJwsSigningAlgorithm; 2] = [
    CoreJwsSigningAlgorithm::EcdsaP256Sha256,
    CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
];

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HttpError(pub String);

pub type HttpFuture = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send>>;

/// The provider's HTTP endpoints, behind a trait so tests can answer without a network.
pub trait OidcHttp: Send + Sync {
    fn send(&self, request: HttpRequest) -> HttpFuture;
}

/// A named type instead of a closure, so the request futures stay `Send` for axum.
struct Client<'a>(&'a dyn OidcHttp);

impl<'c> AsyncHttpClient<'c> for Client<'_> {
    type Error = HttpError;
    type Future = HttpFuture;

    fn call(&'c self, request: HttpRequest) -> HttpFuture {
        self.0.send(request)
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub sub: String,
    pub name: Option<String>,
    pub preferred_username: Option<String>,
    pub email: Option<String>,
    pub email_verified: bool,
}

impl Identity {
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.preferred_username.as_deref())
            .unwrap_or(&self.sub)
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
    #[error("the token response has no ID token")]
    NoIdToken,
    #[error("ID token rejected: {0}")]
    InvalidIdToken(String),
}

type DiscoveredClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

pub struct Provider {
    client: DiscoveredClient,
}

impl Provider {
    /// Reads the provider's metadata and signing keys.
    ///
    /// # Errors
    /// `Config` for a malformed URL, `Discovery` when the provider cannot be read.
    pub async fn discover(settings: &Settings, http: &dyn OidcHttp) -> Result<Self, OidcError> {
        let issuer =
            IssuerUrl::new(settings.issuer.clone()).map_err(|error| OidcError::Config {
                name: "issuer",
                reason: error.to_string(),
            })?;
        let redirect =
            RedirectUrl::new(settings.redirect_url.clone()).map_err(|error| OidcError::Config {
                name: "redirect_url",
                reason: error.to_string(),
            })?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &Client(http))
            .await
            .map_err(|error| OidcError::Discovery(error_chain(&error)))?;
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(settings.client_id.clone()),
            Some(ClientSecret::new(settings.client_secret.clone())),
        )
        .set_redirect_uri(redirect);
        Ok(Self { client })
    }

    /// The URL to send the browser to, with PKCE (S256), state and nonce.
    #[must_use]
    pub fn begin(&self) -> (String, PendingLogin) {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = self
            .client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("email".to_string()))
            .add_scope(Scope::new("profile".to_string()))
            .set_pkce_challenge(challenge)
            .url();
        let pending = PendingLogin {
            state: state.secret().clone(),
            nonce: nonce.secret().clone(),
            pkce_verifier: verifier.secret().clone(),
        };
        (url.to_string(), pending)
    }

    /// Checks the returned state, trades the code for tokens and verifies the ID token's
    /// signature, issuer, audience, nonce and expiry against `now`.
    ///
    /// # Errors
    /// `StateMismatch`, `Exchange`, `NoIdToken` or `InvalidIdToken`.
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
        let request = self
            .client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .map_err(|error| OidcError::Exchange(error.to_string()))?;
        let response = request
            .set_pkce_verifier(PkceCodeVerifier::new(pending.pkce_verifier))
            .request_async(&Client(http))
            .await
            .map_err(|error| OidcError::Exchange(error_chain(&error)))?;
        let id_token = response.id_token().ok_or(OidcError::NoIdToken)?;
        let verifier = self
            .client
            .id_token_verifier()
            .set_allowed_algs(ALLOWED_ALGS)
            .set_time_fn(move || SystemTime::from(now).into());
        let claims = id_token
            .claims(&verifier, &Nonce::new(pending.nonce))
            .map_err(|error| OidcError::InvalidIdToken(error_chain(&error)))?;
        Ok(Identity {
            sub: claims.subject().as_str().to_string(),
            name: claims
                .name()
                .and_then(|name| name.get(None))
                .map(|name| name.as_str().to_string()),
            preferred_username: claims
                .preferred_username()
                .map(|name| name.as_str().to_string()),
            email: claims.email().map(|email| email.as_str().to_string()),
            email_verified: claims.email_verified().unwrap_or(false),
        })
    }
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
    fn display_name_falls_back_to_username_then_subject() {
        let mut identity = Identity {
            sub: "uuid".to_string(),
            name: Some("Bakker Bas".to_string()),
            preferred_username: Some("bas".to_string()),
            email: None,
            email_verified: false,
        };
        assert_eq!(identity.display_name(), "Bakker Bas");
        identity.name = None;
        assert_eq!(identity.display_name(), "bas");
        identity.preferred_username = None;
        assert_eq!(identity.display_name(), "uuid");
    }

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
}
