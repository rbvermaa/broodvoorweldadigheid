use std::sync::{Arc, Mutex, PoisonError};

use auth::oidc::{OidcError, OidcHttp, Provider, Settings};
use jiff::{SignedDuration, Timestamp};

use crate::config::OidcConfig;

/// Kanidm may rotate its signing keys; discovery runs again after this long.
const REDISCOVER_AFTER: SignedDuration = SignedDuration::from_mins(15);

#[derive(Debug, thiserror::Error)]
pub enum Unavailable {
    #[error("no client secret in {0}; run `make kanidm-setup`")]
    NotSetUp(String),
    #[error(transparent)]
    Provider(#[from] OidcError),
}

struct Cached {
    discovered: Timestamp,
    client_secret: String,
    provider: Arc<Provider>,
}

/// The OIDC provider for staff, discovered on first use and cached.
pub struct StaffLogin {
    config: OidcConfig,
    http: Arc<dyn OidcHttp>,
    cached: Mutex<Option<Cached>>,
}

impl StaffLogin {
    pub fn new(config: OidcConfig, http: Arc<dyn OidcHttp>) -> Self {
        Self {
            config,
            http,
            cached: Mutex::new(None),
        }
    }

    pub fn http(&self) -> &dyn OidcHttp {
        self.http.as_ref()
    }

    /// Discovery runs again on the next login, e.g. after an ID token named a key that the
    /// cached key set lacks.
    pub fn forget(&self) {
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The client secret is read on every call, so a replaced secret file counts at once.
    ///
    /// # Errors
    /// `NotSetUp` while the client secret file is missing or empty, `Provider` when
    /// discovery fails.
    pub async fn provider(&self, now: Timestamp) -> Result<Arc<Provider>, Unavailable> {
        let secret_file = &self.config.client_secret_file;
        let not_set_up = || Unavailable::NotSetUp(secret_file.display().to_string());
        let client_secret = std::fs::read_to_string(secret_file)
            .map_err(|_| not_set_up())?
            .trim()
            .to_string();
        if client_secret.is_empty() {
            return Err(not_set_up());
        }
        if let Some(cached) = self
            .cached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            && cached.client_secret == client_secret
            && now.duration_since(cached.discovered) < REDISCOVER_AFTER
        {
            return Ok(Arc::clone(&cached.provider));
        }
        let settings = Settings {
            issuer: self.config.issuer.clone(),
            client_id: self.config.client_id.clone(),
            client_secret: client_secret.clone(),
            redirect_url: self.config.redirect_url.clone(),
        };
        let provider = Arc::new(Provider::discover(&settings, self.http()).await?);
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = Some(Cached {
            discovered: now,
            client_secret,
            provider: Arc::clone(&provider),
        });
        Ok(provider)
    }
}
