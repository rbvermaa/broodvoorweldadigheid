use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use auth::oidc::{HttpFuture, HttpRequest, HttpResponse, OidcHttp, Settings, http};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jiff::{SignedDuration, Timestamp};
use openidconnect::core::{
    CoreIdToken, CoreIdTokenClaims, CoreIdTokenFields, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
    CoreProviderMetadata, CoreResponseType, CoreRsaPrivateSigningKey, CoreSubjectIdentifierType,
    CoreTokenResponse, CoreTokenType,
};
use openidconnect::url::{Url, form_urlencoded};
use openidconnect::{
    AccessToken, Audience, AuthUrl, EmptyAdditionalClaims, EmptyAdditionalProviderMetadata,
    EmptyExtraTokenFields, EndUserEmail, EndUserName, EndUserUsername, IssuerUrl, JsonWebKeySetUrl,
    Nonce, PrivateSigningKey, ResponseTypes, StandardClaims, SubjectIdentifier, TokenUrl,
};
use sha2::{Digest, Sha256};

use crate::oidc_keys::{OTHER_KEY, SIGNING_KEY};

pub const BASE: &str = "https://idm.test";
pub const ISSUER: &str = "https://idm.test/oauth2/openid/bvw-staff";
pub const CLIENT_ID: &str = "bvw-staff";
pub const CLIENT_SECRET: &str = "fake-client-secret";
const TOKEN_LIFETIME: SignedDuration = SignedDuration::from_mins(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeUser {
    pub sub: String,
    pub name: Option<String>,
    pub username: Option<String>,
    pub email: Option<String>,
    pub email_verified: bool,
}

impl FakeUser {
    #[must_use]
    pub fn new(sub: &str, name: &str) -> Self {
        Self {
            sub: sub.to_string(),
            name: Some(name.to_string()),
            username: None,
            email: None,
            email_verified: false,
        }
    }

    #[must_use]
    pub fn with_email(mut self, email: &str, verified: bool) -> Self {
        self.email = Some(email.to_string());
        self.email_verified = verified;
        self
    }
}

/// Ways to make the next ID token wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sabotage {
    WrongNonce,
    WrongAudience,
    WrongIssuer,
    ForeignSignature,
    NoIdToken,
}

/// What the provider sends the browser back to the redirect URL with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    pub redirect_uri: String,
    pub code: String,
    pub state: String,
}

impl Callback {
    /// The callback path and query, for a request to the app under test.
    #[must_use]
    pub fn path_and_query(&self) -> String {
        let url = Url::parse(&self.redirect_uri).expect("redirect URL");
        let query: String = form_urlencoded::Serializer::new(String::new())
            .append_pair("code", &self.code)
            .append_pair("state", &self.state)
            .finish();
        format!("{}?{query}", url.path())
    }
}

struct Grant {
    nonce: String,
    challenge: String,
    redirect_uri: String,
    user: FakeUser,
}

struct Inner {
    now: Timestamp,
    grants: HashMap<String, Grant>,
    sabotage: Option<Sabotage>,
    requests: Vec<String>,
    next_code: u64,
}

/// An OIDC provider in memory: discovery, JWKS and token endpoints, RS256-signed ID tokens.
/// `authorize` plays the browser and the login page.
#[derive(Clone)]
pub struct FakeOidc {
    inner: Arc<Mutex<Inner>>,
}

impl FakeOidc {
    /// Tokens are issued at `now` and expire five minutes later.
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                now,
                grants: HashMap::new(),
                sabotage: None,
                requests: Vec::new(),
                next_code: 0,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn settings(redirect_url: &str) -> Settings {
        Settings {
            issuer: ISSUER.to_string(),
            client_id: CLIENT_ID.to_string(),
            client_secret: CLIENT_SECRET.to_string(),
            redirect_url: redirect_url.to_string(),
        }
    }

    pub fn sabotage(&self, sabotage: Sabotage) {
        self.lock().sabotage = Some(sabotage);
    }

    /// Method and URL of every request so far.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.lock().requests.clone()
    }

    /// Reads an authorization URL the app produced, logs `user` in and returns the redirect.
    ///
    /// # Panics
    /// When the URL lacks what a code flow with PKCE needs.
    #[must_use]
    pub fn authorize(&self, authorization_url: &str, user: FakeUser) -> Callback {
        let url = Url::parse(authorization_url).expect("authorization URL");
        assert_eq!(
            format!("{}{}", url.origin().ascii_serialization(), url.path()),
            format!("{BASE}/authorize")
        );
        let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
        let param = |name: &str| {
            params
                .get(name)
                .unwrap_or_else(|| panic!("authorization URL lacks {name}: {authorization_url}"))
                .clone()
        };
        assert_eq!(param("response_type"), "code");
        assert_eq!(param("client_id"), CLIENT_ID);
        assert_eq!(param("code_challenge_method"), "S256");
        assert!(param("scope").split(' ').any(|scope| scope == "openid"));
        let mut inner = self.lock();
        inner.next_code += 1;
        let code = format!("code-{}", inner.next_code);
        let redirect_uri = param("redirect_uri");
        inner.grants.insert(
            code.clone(),
            Grant {
                nonce: param("nonce"),
                challenge: param("code_challenge"),
                redirect_uri: redirect_uri.clone(),
                user,
            },
        );
        Callback {
            redirect_uri,
            code,
            state: param("state"),
        }
    }

    fn handle(&self, request: &HttpRequest) -> HttpResponse {
        let url = request.uri().to_string();
        self.lock()
            .requests
            .push(format!("{} {url}", request.method()));
        let discovery = format!("{ISSUER}/.well-known/openid-configuration");
        match (request.method().as_str(), url.as_str()) {
            ("GET", path) if path == discovery => json(200, &metadata()),
            ("GET", path) if path == format!("{BASE}/jwks") => json(
                200,
                &CoreJsonWebKeySet::new(vec![key(SIGNING_KEY).as_verification_key()]),
            ),
            ("POST", path) if path == format!("{BASE}/token") => self.token(request),
            _ => respond(404, "application/json", b"{}".to_vec()),
        }
    }

    fn token(&self, request: &HttpRequest) -> HttpResponse {
        let expected_auth = format!(
            "Basic {}",
            STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
        );
        let authorized = request
            .headers()
            .get(http::header::AUTHORIZATION)
            .is_some_and(|value| value.as_bytes() == expected_auth.as_bytes());
        if !authorized {
            return oauth_error(401, "invalid_client");
        }
        let form: HashMap<String, String> = form_urlencoded::parse(request.body())
            .into_owned()
            .collect();
        let mut inner = self.lock();
        let Some(grant) = form.get("code").and_then(|code| inner.grants.remove(code)) else {
            return oauth_error(400, "invalid_grant");
        };
        let verifier_matches = form.get("code_verifier").is_some_and(|verifier| {
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == grant.challenge
        });
        let redirect_matches = form.get("redirect_uri") == Some(&grant.redirect_uri);
        if form.get("grant_type").map(String::as_str) != Some("authorization_code")
            || !verifier_matches
            || !redirect_matches
        {
            return oauth_error(400, "invalid_grant");
        }
        let sabotage = inner.sabotage.take();
        let id_token =
            (sabotage != Some(Sabotage::NoIdToken)).then(|| id_token(&grant, inner.now, sabotage));
        drop(inner);
        let response = CoreTokenResponse::new(
            AccessToken::new("fake-access-token".to_string()),
            CoreTokenType::Bearer,
            CoreIdTokenFields::new(id_token, EmptyExtraTokenFields {}),
        );
        json(200, &response)
    }
}

impl OidcHttp for FakeOidc {
    fn send(&self, request: HttpRequest) -> HttpFuture {
        let response = self.handle(&request);
        Box::pin(async move { Ok(response) })
    }
}

fn key(pem: &str) -> CoreRsaPrivateSigningKey {
    CoreRsaPrivateSigningKey::from_pem(pem, None).expect("test RSA key")
}

fn metadata() -> CoreProviderMetadata {
    CoreProviderMetadata::new(
        IssuerUrl::new(ISSUER.to_string()).expect("issuer"),
        AuthUrl::new(format!("{BASE}/authorize")).expect("auth URL"),
        JsonWebKeySetUrl::new(format!("{BASE}/jwks")).expect("jwks URL"),
        vec![ResponseTypes::new(vec![CoreResponseType::Code])],
        vec![CoreSubjectIdentifierType::Public],
        vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
        EmptyAdditionalProviderMetadata {},
    )
    .set_token_endpoint(Some(
        TokenUrl::new(format!("{BASE}/token")).expect("token URL"),
    ))
}

fn id_token(grant: &Grant, now: Timestamp, sabotage: Option<Sabotage>) -> CoreIdToken {
    let issuer = if sabotage == Some(Sabotage::WrongIssuer) {
        "https://elders.test"
    } else {
        ISSUER
    };
    let audience = if sabotage == Some(Sabotage::WrongAudience) {
        "another-client"
    } else {
        CLIENT_ID
    };
    let nonce = if sabotage == Some(Sabotage::WrongNonce) {
        "not-the-nonce".to_string()
    } else {
        grant.nonce.clone()
    };
    let user = &grant.user;
    let standard = StandardClaims::new(SubjectIdentifier::new(user.sub.clone()))
        .set_name(user.name.clone().map(|name| EndUserName::new(name).into()))
        .set_preferred_username(user.username.clone().map(EndUserUsername::new))
        .set_email(user.email.clone().map(EndUserEmail::new))
        .set_email_verified(user.email.as_ref().map(|_| user.email_verified));
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(issuer.to_string()).expect("issuer"),
        vec![Audience::new(audience.to_string())],
        SystemTime::from(now + TOKEN_LIFETIME).into(),
        SystemTime::from(now).into(),
        standard,
        EmptyAdditionalClaims {},
    )
    .set_nonce(Some(Nonce::new(nonce)));
    let signer = key(if sabotage == Some(Sabotage::ForeignSignature) {
        OTHER_KEY
    } else {
        SIGNING_KEY
    });
    CoreIdToken::new(
        claims,
        &signer,
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        None,
        None,
    )
    .expect("sign ID token")
}

fn json(status: u16, body: &impl serde::Serialize) -> HttpResponse {
    respond(
        status,
        "application/json",
        serde_json::to_vec(body).expect("JSON"),
    )
}

fn oauth_error(status: u16, error: &str) -> HttpResponse {
    respond(
        status,
        "application/json",
        format!("{{\"error\":\"{error}\"}}").into_bytes(),
    )
}

fn respond(status: u16, content_type: &str, body: Vec<u8>) -> HttpResponse {
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, content_type)
        .body(body)
        .expect("response")
}
