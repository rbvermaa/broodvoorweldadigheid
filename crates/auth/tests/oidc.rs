#![forbid(unsafe_code)]

use std::collections::HashMap;

use auth::oidc::{OidcError, Provider, url::Url};
use jiff::{SignedDuration, Timestamp};
use testkit::oidc::{BASE, ISSUER};
use testkit::{FakeOidc, FakeUser, Sabotage};

const REDIRECT: &str = "https://shop.test/staff/oidc/callback";

fn now() -> Timestamp {
    "2026-11-24T09:00:00Z".parse().expect("timestamp")
}

async fn provider(fake: &FakeOidc) -> Provider {
    Provider::discover(&FakeOidc::settings(REDIRECT), fake)
        .await
        .expect("discovery")
}

fn query(url: &str) -> HashMap<String, String> {
    Url::parse(url)
        .expect("url")
        .query_pairs()
        .into_owned()
        .collect()
}

#[tokio::test]
async fn login_url_carries_pkce_state_and_nonce() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, _) = provider.begin();
    assert!(url.starts_with(&format!("{BASE}/authorize?")), "{url}");
    let params = query(&url);
    assert_eq!(params["redirect_uri"], REDIRECT);
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["scope"], "openid email profile");
    for name in ["state", "nonce", "code_challenge"] {
        assert!(params[name].len() >= 22, "{name} too short: {url}");
    }
    let (second, _) = provider.begin();
    assert_ne!(query(&second)["state"], params["state"]);
    assert_ne!(query(&second)["nonce"], params["nonce"]);
    assert_eq!(
        fake.requests(),
        [
            format!("GET {ISSUER}/.well-known/openid-configuration"),
            format!("GET {BASE}/jwks"),
        ],
        "discovery reads metadata and keys; begin sends nothing"
    );
}

#[tokio::test]
async fn a_good_login_returns_the_identity() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, pending) = provider.begin();
    let callback = fake.authorize(
        &url,
        FakeUser::new("uuid-1", "Bakker Bas").with_email("bas@example.org", true),
    );
    let identity = provider
        .complete(&fake, pending, &callback.state, &callback.code, now())
        .await
        .expect("login");
    assert_eq!(identity.sub, "uuid-1");
    assert_eq!(identity.name.as_deref(), Some("Bakker Bas"));
    assert_eq!(identity.email.as_deref(), Some("bas@example.org"));
    assert!(identity.email_verified);
}

#[tokio::test]
async fn a_different_state_is_refused_before_the_code_is_used() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, pending) = provider.begin();
    let callback = fake.authorize(&url, FakeUser::new("uuid-1", "Bas"));
    let error = provider
        .complete(&fake, pending, "forged-state", &callback.code, now())
        .await
        .expect_err("state mismatch");
    assert!(matches!(error, OidcError::StateMismatch), "{error}");
    assert!(
        !fake
            .requests()
            .iter()
            .any(|request| request.contains("/token")),
        "no token request after a state mismatch"
    );
}

#[tokio::test]
async fn a_wrong_pkce_verifier_is_refused_by_the_provider() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, _) = provider.begin();
    let (_, other_pending) = provider.begin();
    let callback = fake.authorize(&url, FakeUser::new("uuid-1", "Bas"));
    let forged = forge_state(other_pending, &callback.state);
    let error = provider
        .complete(&fake, forged, &callback.state, &callback.code, now())
        .await
        .expect_err("verifier mismatch");
    assert!(matches!(error, OidcError::Exchange(_)), "{error}");
    assert!(error.to_string().contains("invalid_grant"), "{error}");
}

/// A pending login whose state matches but whose PKCE verifier and nonce are another's.
fn forge_state(pending: auth::oidc::PendingLogin, state: &str) -> auth::oidc::PendingLogin {
    let json = serde_json::to_value(&pending).expect("serialize");
    let mut map = json.as_object().expect("object").clone();
    map.insert("state".to_string(), state.into());
    serde_json::from_value(map.into()).expect("deserialize")
}

#[tokio::test]
async fn broken_id_tokens_are_refused() {
    for sabotage in [
        Sabotage::WrongNonce,
        Sabotage::WrongAudience,
        Sabotage::WrongIssuer,
        Sabotage::ForeignSignature,
    ] {
        let fake = FakeOidc::new(now());
        let provider = provider(&fake).await;
        let (url, pending) = provider.begin();
        let callback = fake.authorize(&url, FakeUser::new("uuid-1", "Bas"));
        fake.sabotage(sabotage);
        let error = provider
            .complete(&fake, pending, &callback.state, &callback.code, now())
            .await
            .expect_err("broken token");
        assert!(
            matches!(error, OidcError::InvalidIdToken(_)),
            "{sabotage:?}: {error}"
        );
    }
}

#[tokio::test]
async fn a_token_response_without_id_token_is_refused() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, pending) = provider.begin();
    let callback = fake.authorize(&url, FakeUser::new("uuid-1", "Bas"));
    fake.sabotage(Sabotage::NoIdToken);
    let error = provider
        .complete(&fake, pending, &callback.state, &callback.code, now())
        .await
        .expect_err("no id token");
    assert!(matches!(error, OidcError::NoIdToken), "{error}");
}

#[tokio::test]
async fn an_expired_id_token_is_refused_by_the_injected_clock() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, pending) = provider.begin();
    let callback = fake.authorize(&url, FakeUser::new("uuid-1", "Bas"));
    let later = now() + SignedDuration::from_mins(6);
    let error = provider
        .complete(&fake, pending, &callback.state, &callback.code, later)
        .await
        .expect_err("expired");
    assert!(matches!(error, OidcError::InvalidIdToken(_)), "{error}");
    assert!(error.to_string().contains("expire"), "{error}");
}

#[tokio::test]
async fn a_code_works_once() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, pending) = provider.begin();
    let callback = fake.authorize(&url, FakeUser::new("uuid-1", "Bas"));
    provider
        .complete(
            &fake,
            pending.clone(),
            &callback.state,
            &callback.code,
            now(),
        )
        .await
        .expect("first use");
    let error = provider
        .complete(&fake, pending, &callback.state, &callback.code, now())
        .await
        .expect_err("replay");
    assert!(matches!(error, OidcError::Exchange(_)), "{error}");
}

#[tokio::test]
async fn bad_settings_and_unreachable_providers_fail_discovery() {
    let fake = FakeOidc::new(now());
    let mut settings = FakeOidc::settings(REDIRECT);
    settings.issuer = "not a url".to_string();
    let Err(error) = Provider::discover(&settings, &fake).await else {
        panic!("bad issuer accepted");
    };
    assert!(
        matches!(error, OidcError::Config { name: "issuer", .. }),
        "{error}"
    );

    let mut settings = FakeOidc::settings(REDIRECT);
    settings.redirect_url = "/relative".to_string();
    let Err(error) = Provider::discover(&settings, &fake).await else {
        panic!("bad redirect accepted");
    };
    assert!(
        matches!(
            error,
            OidcError::Config {
                name: "redirect_url",
                ..
            }
        ),
        "{error}"
    );

    let mut settings = FakeOidc::settings(REDIRECT);
    settings.issuer = format!("{BASE}/oauth2/openid/unknown");
    let Err(error) = Provider::discover(&settings, &fake).await else {
        panic!("unknown issuer accepted");
    };
    assert!(matches!(error, OidcError::Discovery(_)), "{error}");
}
