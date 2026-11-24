use std::sync::Arc;

use domain::{
    CreatedPayment, CreatedRefund, FeeCursor, FeePage, NewPayment, PaymentError, PaymentProvider,
    PaymentSnapshot, ProviderFuture, ProviderPaymentId, RefundRequest,
};
use payments::{MollieClient, MollieConfig, MollieOptions, SetupError};

use crate::config::Config;

/// Stands in when no Mollie key is configured, so the shop still shows drops.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unconfigured;

fn not_configured<'a, T: Send + 'a>() -> ProviderFuture<'a, T> {
    Box::pin(async {
        Err(PaymentError::NotConfigured(
            "MOLLIE_API_KEY_FILE is not set".to_string(),
        ))
    })
}

impl PaymentProvider for Unconfigured {
    fn create_payment<'a>(&'a self, _: &'a NewPayment) -> ProviderFuture<'a, CreatedPayment> {
        not_configured()
    }

    fn get_payment<'a>(&'a self, _: &'a ProviderPaymentId) -> ProviderFuture<'a, PaymentSnapshot> {
        not_configured()
    }

    fn refund<'a>(&'a self, _: &'a RefundRequest) -> ProviderFuture<'a, CreatedRefund> {
        not_configured()
    }

    fn list_payment_fees<'a>(&'a self, _: Option<&'a FeeCursor>) -> ProviderFuture<'a, FeePage> {
        not_configured()
    }
}

/// The Mollie client when `MOLLIE_API_KEY_FILE` is set, else [`Unconfigured`].
///
/// # Errors
/// [`SetupError`] for an unreadable key, a bad base URL or missing root certificates.
pub fn from_config(config: &Config) -> Result<Arc<dyn PaymentProvider>, SetupError> {
    let Some(api_key_file) = &config.mollie_api_key_file else {
        tracing::warn!("MOLLIE_API_KEY_FILE is not set; checkout cannot take payments");
        return Ok(Arc::new(Unconfigured));
    };
    let mut options = MollieOptions::default();
    if let Some(url) = &config.mollie_api_url {
        options.base_url.clone_from(url);
    }
    let client = MollieClient::from_config(
        &MollieConfig {
            api_key_file: api_key_file.clone(),
            organization_token_file: None,
        },
        options,
    )?;
    Ok(Arc::new(client))
}
