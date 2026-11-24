use std::sync::Arc;

use db::PgPool;
use domain::{Clock, PaymentProvider};
use i18n::Translator;

use crate::clock::SystemClock;
use crate::config::Config;
use crate::payment_provider::Unconfigured;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub i18n: Arc<Translator>,
    pub config: Arc<Config>,
    pub payments: Arc<dyn PaymentProvider>,
    pub clock: Arc<dyn Clock>,
}

impl AppState {
    /// With the system clock and no payment provider: checkout then fails politely.
    pub fn new(pool: PgPool, i18n: Translator, config: Config) -> Self {
        Self {
            pool,
            i18n: Arc::new(i18n),
            config: Arc::new(config),
            payments: Arc::new(Unconfigured),
            clock: Arc::new(SystemClock),
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
}
