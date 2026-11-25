use std::sync::Arc;

use db::PgPool;
use domain::{Clock, PaymentProvider};
use i18n::Translator;
use jobs::OrderEmails;

use crate::clock::SystemClock;
use crate::config::Config;
use crate::oidc_http::ReqwestHttp;
use crate::payment_provider::Unconfigured;
use crate::public_donations::Switch;
use crate::shop::PaymentLookups;
use crate::staff_login::StaffLogin;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub i18n: Arc<Translator>,
    pub config: Arc<Config>,
    pub payments: Arc<dyn PaymentProvider>,
    pub clock: Arc<dyn Clock>,
    /// `None` when the config has no OIDC issuer; staff login then falls back to the dev
    /// login, or 503.
    pub staff_login: Option<Arc<StaffLogin>>,
    pub order_emails: Arc<OrderEmails>,
    pub payment_lookups: Arc<PaymentLookups>,
    pub public_donations: Arc<Switch>,
}

impl AppState {
    /// With the system clock and no payment provider: checkout then fails politely.
    pub fn new(pool: PgPool, i18n: Translator, config: Config) -> Self {
        let staff_login = config.oidc.clone().map(|oidc| {
            let http = Arc::new(ReqwestHttp::new(oidc.ca_file.clone()));
            Arc::new(StaffLogin::new(oidc, http))
        });
        let i18n = Arc::new(i18n);
        let order_emails = OrderEmails::new(Arc::clone(&i18n), config.public_url.clone())
            .with_reminder_at(config.pickup_reminder_at);
        Self {
            pool,
            i18n,
            config: Arc::new(config),
            order_emails: Arc::new(order_emails),
            payment_lookups: Arc::new(PaymentLookups::default()),
            public_donations: Arc::new(Switch::default()),
            payments: Arc::new(Unconfigured),
            clock: Arc::new(SystemClock),
            staff_login,
        }
    }

    #[must_use]
    pub fn with_payments(mut self, payments: Arc<dyn PaymentProvider>) -> Self {
        self.payments = payments;
        self
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
