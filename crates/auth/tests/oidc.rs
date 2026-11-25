#![forbid(unsafe_code)]

use std::collections::HashMap;

use auth::oidc::{OidcError, PendingLogin, Provider};
use jiff::{SignedDuration, Timestamp};
use testkit::oidc::{BASE, ISSUER, form_decode};
use testkit::{FakeOidc, FakeUser, Sabotage};

const REDIRECT: &str = "https://shop.test/staff/oidc/callback";
const BAS: &str = "0d2f1f9e-1111-2222-3333-444455556666";

fn now() -> Timestamp {
    "2026-11-24T09:00:00Z".parse().expect("timestamp")
}

async fn provider(fake: &FakeOidc) -> Provider {
    Provider::discover(&FakeOidc::settings(REDIRECT), fake)
        .await
        .expect("discovery")
}

fn query(url: &str) -> HashMap<String, String> {
    form_decode(url.split_once('?').expect("query").1.as_bytes())
}

/// Runs a login for `user` with `sabotage` on the ID token, verified at `at`.
async fn login(
    user: FakeUser,
    sabotage: Option<Sabotage>,
    issued: Timestamp,
    at: Timestamp,
) -> Result<auth::Identity, OidcError> {
    let fake = FakeOidc::new(issued);
    let provider = provider(&fake).await;
    let (url, pending) = provider.begin();
    let callback = fake.authorize(&url, user);
    if let Some(sabotage) = sabotage {
        fake.sabotage(sabotage);
    }
    provider
        .complete(&fake, pending, &callback.state, &callback.code, at)
        .await
}

#[tokio::test]
async fn login_url_carries_pkce_state_and_nonce() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, _) = provider.begin();
    assert!(url.starts_with(&format!("{BASE}/authorize?")), "{url}");
    let params = query(&url);
    assert_eq!(params["redirect_uri"], REDIRECT);
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["scope"], "openid email");
    for name in ["state", "nonce", "code_challenge"] {
        assert_eq!(params[name].len(), 43, "{name}: {url}");
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
    let identity = login(
        FakeUser::new(BAS, "Bakker Bas").with_email("bas@example.org", true),
        None,
        now(),
        now(),
    )
    .await
    .expect("login");
    assert_eq!(identity.sub, BAS);
    assert_eq!(identity.name.as_deref(), Some("Bakker Bas"));
    assert_eq!(identity.email.as_deref(), Some("bas@example.org"));
    assert!(identity.email_verified);
}

#[tokio::test]
async fn an_uppercase_uuid_sub_is_lowercased() {
    let identity = login(
        FakeUser::new(&BAS.to_uppercase(), "Bas"),
        None,
        now(),
        now(),
    )
    .await
    .expect("login");
    assert_eq!(identity.sub, BAS);
}

#[tokio::test]
async fn a_sub_that_is_not_a_uuid_is_refused() {
    let error = login(FakeUser::new("bas", "Bas"), None, now(), now())
        .await
        .expect_err("user name as sub");
    assert!(matches!(error, OidcError::InvalidIdToken(_)), "{error}");
    assert!(error.to_string().contains("UUID"), "{error}");
}

#[tokio::test]
async fn only_es256_is_accepted() {
    for sabotage in [Sabotage::AlgNone, Sabotage::AlgHs256, Sabotage::AlgRs256] {
        let error = login(FakeUser::new(BAS, "Bas"), Some(sabotage), now(), now())
            .await
            .expect_err("wrong alg");
        assert!(
            matches!(&error, OidcError::InvalidIdToken(reason) if reason.contains("not ES256")),
            "{sabotage:?}: {error}"
        );
    }
}

#[tokio::test]
async fn a_kid_outside_the_key_set_is_refused() {
    let error = login(
        FakeUser::new(BAS, "Bas"),
        Some(Sabotage::UnknownKid),
        now(),
        now(),
    )
    .await
    .expect_err("unknown kid");
    assert!(matches!(error, OidcError::UnknownKey), "{error}");

    let error = login(
        FakeUser::new(BAS, "Bas"),
        Some(Sabotage::NoKid),
        now(),
        now(),
    )
    .await
    .expect_err("no kid");
    assert!(matches!(error, OidcError::InvalidIdToken(_)), "{error}");
}

#[tokio::test]
async fn broken_id_tokens_are_refused() {
    for sabotage in [
        Sabotage::ForeignSignature,
        Sabotage::Tampered,
        Sabotage::WrongNonce,
        Sabotage::NoNonce,
        Sabotage::WrongAudience,
        Sabotage::ExtraAudience,
        Sabotage::WrongAuthorizedParty,
        Sabotage::WrongIssuer,
        Sabotage::IssuedInTheFuture,
    ] {
        let error = login(FakeUser::new(BAS, "Bas"), Some(sabotage), now(), now())
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
    let error = login(
        FakeUser::new(BAS, "Bas"),
        Some(Sabotage::NoIdToken),
        now(),
        now(),
    )
    .await
    .expect_err("no id token");
    assert!(matches!(error, OidcError::NoIdToken), "{error}");
}

#[tokio::test]
async fn expiry_is_checked_against_the_injected_clock_with_a_minute_of_leeway() {
    let lifetime = testkit::oidc::TOKEN_LIFETIME;
    let user = || FakeUser::new(BAS, "Bas");
    let almost = now() + lifetime + SignedDuration::from_secs(59);
    login(user(), None, now(), almost)
        .await
        .expect("within the leeway");
    let error = login(user(), None, now(), now() + lifetime + auth::oidc::LEEWAY)
        .await
        .expect_err("expired");
    assert!(error.to_string().contains("expired"), "{error}");
}

#[tokio::test]
async fn expiry_holds_across_the_end_of_summer_time() {
    // 02:58 CEST; the clocks go back to 02:00 CET at 03:00 CEST.
    let issued: Timestamp = "2026-10-25T00:58:00Z".parse().expect("timestamp");
    let user = || FakeUser::new(BAS, "Bas");
    let wall_clock_earlier: Timestamp = "2026-10-25T02:03:00+01:00".parse().expect("timestamp");
    login(user(), None, issued, wall_clock_earlier)
        .await
        .expect("4 minutes after issue");
    let later: Timestamp = "2026-10-25T02:05:00+01:00".parse().expect("timestamp");
    let error = login(user(), None, issued, later)
        .await
        .expect_err("7 minutes after issue, though the wall clock shows an earlier time");
    assert!(error.to_string().contains("expired"), "{error}");
}

#[tokio::test]
async fn a_different_state_is_refused_before_the_code_is_used() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, pending) = provider.begin();
    let callback = fake.authorize(&url, FakeUser::new(BAS, "Bas"));
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

/// A pending login whose state matches but whose PKCE verifier and nonce are another's.
fn forge_state(pending: PendingLogin, state: &str) -> PendingLogin {
    let json = serde_json::to_value(&pending).expect("serialize");
    let mut map = json.as_object().expect("object").clone();
    map.insert("state".to_string(), state.into());
    serde_json::from_value(map.into()).expect("deserialize")
}

#[tokio::test]
async fn a_wrong_pkce_verifier_is_refused_by_the_provider() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, _) = provider.begin();
    let (_, other_pending) = provider.begin();
    let callback = fake.authorize(&url, FakeUser::new(BAS, "Bas"));
    let forged = forge_state(other_pending, &callback.state);
    let error = provider
        .complete(&fake, forged, &callback.state, &callback.code, now())
        .await
        .expect_err("verifier mismatch");
    assert!(matches!(error, OidcError::Exchange(_)), "{error}");
    assert!(error.to_string().contains("invalid_grant"), "{error}");
}

#[tokio::test]
async fn a_code_works_once() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, pending) = provider.begin();
    let callback = fake.authorize(&url, FakeUser::new(BAS, "Bas"));
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
async fn a_provider_that_goes_down_is_unreachable() {
    let fake = FakeOidc::new(now());
    let provider = provider(&fake).await;
    let (url, pending) = provider.begin();
    let callback = fake.authorize(&url, FakeUser::new(BAS, "Bas"));
    fake.set_down(true);
    let error = provider
        .complete(&fake, pending, &callback.state, &callback.code, now())
        .await
        .expect_err("down");
    assert!(matches!(error, OidcError::Unreachable(_)), "{error}");
}

#[tokio::test]
async fn bad_settings_and_wrong_issuers_fail_discovery() {
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

    let mut settings = FakeOidc::settings(REDIRECT);
    settings.issuer = format!("{ISSUER}/");
    let Err(error) = Provider::discover(&settings, &fake).await else {
        panic!("issuer that differs by a slash accepted");
    };
    assert!(error.to_string().contains("calls itself"), "{error}");

    fake.set_down(true);
    let Err(error) = Provider::discover(&FakeOidc::settings(REDIRECT), &fake).await else {
        panic!("unreachable provider accepted");
    };
    assert!(matches!(error, OidcError::Discovery(_)), "{error}");
}
