use std::sync::Arc;

use db::PgPool;
use i18n::Translator;

use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub i18n: Arc<Translator>,
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(pool: PgPool, i18n: Translator, config: Config) -> Self {
        Self {
            pool,
            i18n: Arc::new(i18n),
            config: Arc::new(config),
        }
    }
}
