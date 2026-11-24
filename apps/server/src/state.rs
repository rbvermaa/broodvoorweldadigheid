use std::sync::Arc;

use db::PgPool;
use domain::Clock;
use i18n::Translator;

use crate::clock::SystemClock;
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub i18n: Arc<Translator>,
    pub config: Arc<Config>,
    pub clock: Arc<dyn Clock>,
}

impl AppState {
    pub fn new(pool: PgPool, i18n: Translator, config: Config) -> Self {
        Self {
            pool,
            i18n: Arc::new(i18n),
            config: Arc::new(config),
            clock: Arc::new(SystemClock),
        }
    }

    /// Replaces the system clock, so tests can set "now".
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}
