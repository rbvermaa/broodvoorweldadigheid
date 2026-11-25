use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use auth::oidc::{
    HttpFuture, HttpRequest, HttpResponse, Method, OidcHttp, Settings, form_urlencode,
    pkce_challenge,
};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jiff::{SignedDuration, Timestamp};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};

use crate::oidc_keys::{OTHER_KEY, SIGNING_KEY};

pub const BASE: &str = "https://idm.test";
pub const ISSUER: &str = "https://idm.test/oauth2/openid/bvw-staff";
pub const CLIENT_ID: &str = "bvw-staff";
pub const CLIENT_SECRET: &str = "fake-client-secret";
pub const KID: &str = "fake-es256";
const ENCRYPTION_KID: &str = "fake-enc";
pub const TOKEN_LIFETIME: SignedDuration = SignedDuration::from_mins(5);

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

    /// Without `name` or `preferred_username`.
    #[must_use]
    pub fn nameless(sub: &str) -> Self {
        Self {
            name: None,
            ..Self::new(sub, "")
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
    NoNonce,
    WrongAudience,
    /// `aud` holds this client and another one, without `azp`.
    ExtraAudience,
    WrongAuthorizedParty,
    WrongIssuer,
    /// Signed by another P-256 key under the known `kid`.
    ForeignSignature,
    /// The claims changed after signing.
    Tampered,
    UnknownKid,
    /// Signed by the key set's encryption key, under its own `kid`.
    EncryptionKey,
    NoKid,
    AlgNone,
    /// HMAC with the client secret, the classic key-confusion attack.
    AlgHs256,
    AlgRs256,
    IssuedInTheFuture,
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
    ///
    /// # Panics
    /// When the redirect URI is not an absolute URL.
    #[must_use]
    pub fn path_and_query(&self) -> String {
        let after_scheme = self
            .redirect_uri
            .split_once("://")
            .expect("absolute redirect URL")
            .1;
        let path = after_scheme
            .find('/')
            .map_or("/", |start| &after_scheme[start..]);
        let query = form_urlencode(&[("code", &self.code), ("state", &self.state)]);
        format!("{path}?{query}")
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
    down: bool,
}

/// An OIDC provider in memory, like Kanidm: discovery, JWKS and token endpoints, and
/// ES256-signed ID tokens. `authorize` plays the browser and the login page.
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
                down: false,
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

    /// When the provider issues its next tokens.
    pub fn set_now(&self, now: Timestamp) {
        self.lock().now = now;
    }

    /// While down, every request fails as if the provider could not be reached.
    pub fn set_down(&self, down: bool) {
        self.lock().down = down;
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
        let (endpoint, query) = authorization_url
            .split_once('?')
            .expect("authorization URL has a query");
        assert_eq!(endpoint, format!("{BASE}/authorize"));
        let params = form_decode(query.as_bytes());
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

    fn handle(&self, request: &HttpRequest) -> Option<HttpResponse> {
        let method = match request.method {
            Method::Get => "GET",
            Method::Post => "POST",
        };
        let url = request.url.as_str();
        let down = {
            let mut inner = self.lock();
            inner.requests.push(format!("{method} {url}"));
            inner.down
        };
        if down {
            return None;
        }
        let discovery = format!("{ISSUER}/.well-known/openid-configuration");
        Some(match (request.method, url) {
            (Method::Get, path) if path == discovery => respond(200, &metadata()),
            (Method::Get, path) if path == format!("{BASE}/jwks") => respond(200, &key_set()),
            (Method::Post, path) if path == format!("{BASE}/token") => self.token(request),
            _ => respond(404, &json!({})),
        })
    }

    fn token(&self, request: &HttpRequest) -> HttpResponse {
        let expected_auth = format!(
            "Basic {}",
            STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
        );
        if request.header("authorization") != Some(expected_auth.as_str()) {
            return oauth_error(401, "invalid_client");
        }
        let form = form_decode(&request.body);
        let mut inner = self.lock();
        let Some(grant) = form.get("code").and_then(|code| inner.grants.remove(code)) else {
            return oauth_error(400, "invalid_grant");
        };
        let verifier_matches = form
            .get("code_verifier")
            .is_some_and(|verifier| pkce_challenge(verifier) == grant.challenge);
        let redirect_matches = form.get("redirect_uri") == Some(&grant.redirect_uri);
        if form.get("grant_type").map(String::as_str) != Some("authorization_code")
            || !verifier_matches
            || !redirect_matches
        {
            return oauth_error(400, "invalid_grant");
        }
        let sabotage = inner.sabotage.take();
        let now = inner.now;
        drop(inner);
        let mut body = json!({
            "access_token": "fake-access-token",
            "token_type": "Bearer",
            "expires_in": 300,
        });
        if sabotage != Some(Sabotage::NoIdToken) {
            body["id_token"] = Value::String(id_token(&grant, now, sabotage));
        }
        respond(200, &body)
    }
}

impl OidcHttp for FakeOidc {
    fn send(&self, request: HttpRequest) -> HttpFuture {
        let response = self.handle(&request);
        Box::pin(async move {
            response.ok_or_else(|| auth::oidc::HttpError("connection refused".to_string()))
        })
    }
}

fn key_pair(pkcs8: &str) -> EcdsaKeyPair {
    let der = STANDARD.decode(pkcs8).expect("test key base64");
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &SystemRandom::new())
        .expect("test P-256 key")
}

fn jwk_point(pkcs8: &str) -> (String, String) {
    let key = key_pair(pkcs8);
    let point = key.public_key().as_ref();
    (
        URL_SAFE_NO_PAD.encode(&point[1..33]),
        URL_SAFE_NO_PAD.encode(&point[33..65]),
    )
}

/// The signing key, plus keys a client must skip: an encryption key and an RSA key.
fn key_set() -> Value {
    let (x, y) = jwk_point(SIGNING_KEY);
    let (other_x, other_y) = jwk_point(OTHER_KEY);
    json!({
        "keys": [
            {"kty": "EC", "crv": "P-256", "use": "sig", "alg": "ES256", "kid": KID, "x": x, "y": y},
            {"kty": "EC", "crv": "P-256", "use": "enc", "kid": ENCRYPTION_KID, "x": other_x, "y": other_y},
            {"kty": "RSA", "kid": "fake-rsa", "alg": "RS256", "n": "AQAB", "e": "AQAB"},
        ]
    })
}

fn metadata() -> Value {
    json!({
        "issuer": ISSUER,
        "authorization_endpoint": format!("{BASE}/authorize"),
        "token_endpoint": format!("{BASE}/token"),
        "jwks_uri": format!("{BASE}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
    })
}

fn claims(grant: &Grant, now: Timestamp, is: impl Fn(Sabotage) -> bool) -> Value {
    let user = &grant.user;
    let issued = if is(Sabotage::IssuedInTheFuture) {
        now + SignedDuration::from_mins(10)
    } else {
        now
    };
    let audience = if is(Sabotage::WrongAudience) {
        json!("another-client")
    } else if is(Sabotage::ExtraAudience) {
        json!([CLIENT_ID, "another-client"])
    } else {
        json!(CLIENT_ID)
    };
    let issuer = if is(Sabotage::WrongIssuer) {
        "https://elders.test"
    } else {
        ISSUER
    };
    let mut claims = json!({
        "iss": issuer,
        "sub": user.sub,
        "aud": audience,
        "exp": (issued + TOKEN_LIFETIME).as_second(),
        "iat": issued.as_second(),
    });
    if is(Sabotage::WrongAuthorizedParty) {
        claims["azp"] = json!("another-client");
    }
    if !is(Sabotage::NoNonce) {
        let nonce = if is(Sabotage::WrongNonce) {
            "not-the-nonce"
        } else {
            grant.nonce.as_str()
        };
        claims["nonce"] = json!(nonce);
    }
    for (name, value) in [
        ("name", &user.name),
        ("preferred_username", &user.username),
        ("email", &user.email),
    ] {
        if let Some(value) = value {
            claims[name] = json!(value);
        }
    }
    if user.email.is_some() {
        claims["email_verified"] = json!(user.email_verified);
    }
    claims
}

fn id_token(grant: &Grant, now: Timestamp, sabotage: Option<Sabotage>) -> String {
    let is = |wanted: Sabotage| sabotage == Some(wanted);
    let mut claims = claims(grant, now, is);
    let alg = if is(Sabotage::AlgNone) {
        "none"
    } else if is(Sabotage::AlgHs256) {
        "HS256"
    } else if is(Sabotage::AlgRs256) {
        "RS256"
    } else {
        "ES256"
    };
    let mut header = json!({"alg": alg, "typ": "JWT"});
    if !is(Sabotage::NoKid) {
        let kid = if is(Sabotage::UnknownKid) {
            "rotated-away"
        } else if is(Sabotage::EncryptionKey) {
            ENCRYPTION_KID
        } else {
            KID
        };
        header["kid"] = json!(kid);
    }
    let encode = |value: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("JSON"));
    let signing_input = format!("{}.{}", encode(&header), encode(&claims));
    let signature = match alg {
        "none" => Vec::new(),
        "HS256" => {
            let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, CLIENT_SECRET.as_bytes());
            ring::hmac::sign(&key, signing_input.as_bytes())
                .as_ref()
                .to_vec()
        }
        "RS256" => vec![0x5a; 256],
        _ => {
            let pkcs8 = if is(Sabotage::ForeignSignature) || is(Sabotage::EncryptionKey) {
                OTHER_KEY
            } else {
                SIGNING_KEY
            };
            key_pair(pkcs8)
                .sign(&SystemRandom::new(), signing_input.as_bytes())
                .expect("sign")
                .as_ref()
                .to_vec()
        }
    };
    let signed_part = if is(Sabotage::Tampered) {
        claims["sub"] = json!("00000000-0000-0000-0000-000000000000");
        format!("{}.{}", encode(&header), encode(&claims))
    } else {
        signing_input
    };
    format!("{signed_part}.{}", URL_SAFE_NO_PAD.encode(signature))
}

/// Parses `application/x-www-form-urlencoded`.
///
/// # Panics
/// On malformed percent escapes or text that is not UTF-8.
#[must_use]
pub fn form_decode(input: &[u8]) -> HashMap<String, String> {
    let decode = |part: &[u8]| {
        let mut out = Vec::with_capacity(part.len());
        let mut bytes = part.iter();
        while let Some(&byte) = bytes.next() {
            match byte {
                b'+' => out.push(b' '),
                b'%' => {
                    let hex: Vec<u8> = bytes.by_ref().take(2).copied().collect();
                    let hex = std::str::from_utf8(&hex).expect("hex escape");
                    out.push(u8::from_str_radix(hex, 16).expect("hex escape"));
                }
                _ => out.push(byte),
            }
        }
        String::from_utf8(out).expect("UTF-8")
    };
    input
        .split(|byte| *byte == b'&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let mut halves = pair.splitn(2, |byte| *byte == b'=');
            let key = decode(halves.next().unwrap_or_default());
            let value = decode(halves.next().unwrap_or_default());
            (key, value)
        })
        .collect()
}

fn respond(status: u16, body: &Value) -> HttpResponse {
    HttpResponse {
        status,
        body: serde_json::to_vec(body).expect("JSON"),
    }
}

fn oauth_error(status: u16, error: &str) -> HttpResponse {
    respond(status, &json!({"error": error}))
}
