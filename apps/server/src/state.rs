use std::sync::Arc;

use db::PgPool;
use domain::Clock;
use i18n::Translator;

use crate::clock::SystemClock;
use crate::config::Config;
use crate::oidc_http::ReqwestHttp;
use crate::staff_login::StaffLogin;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub i18n: Arc<Translator>,
    pub config: Arc<Config>,
    pub clock: Arc<dyn Clock>,
    /// `None` when the config has no OIDC issuer; staff login then answers 503.
    pub staff_login: Option<Arc<StaffLogin>>,
}

impl AppState {
    pub fn new(pool: PgPool, i18n: Translator, config: Config) -> Self {
        let staff_login = config.oidc.clone().map(|oidc| {
            let http = Arc::new(ReqwestHttp::new(oidc.ca_file.clone()));
            Arc::new(StaffLogin::new(oidc, http))
        });
        Self {
            pool,
            i18n: Arc::new(i18n),
            config: Arc::new(config),
            clock: Arc::new(SystemClock),
            staff_login,
        }
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn with_staff_login(mut self, staff_login: StaffLogin) -> Self {
        self.staff_login = Some(Arc::new(staff_login));
        self
    }
}
