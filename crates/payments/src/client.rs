use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use domain::{
    CreatedPayment, CreatedRefund, FeeCursor, FeeKind, FeePage, IdempotencyKey, Money, NewPayment,
    OrderId, PaymentError, PaymentFee, PaymentProvider, PaymentSnapshot, PaymentStatus,
    ProviderFuture, ProviderPaymentId, ProviderRefundId, RefundRequest, RefundStatus,
};
use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::amount::{AmountError, format_value, parse_amount};
use crate::secret::{ApiKey, OrganizationToken, SecretError};

pub const MOLLIE_API_URL: &str = "https://api.mollie.com/v2/";
const MAX_DESCRIPTION: usize = 255;
const FEE_PAGE_SIZE: &str = "250";

/// Where the Mollie secrets live; read once at start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MollieConfig {
    pub api_key_file: PathBuf,
    /// Only needed for [`PaymentProvider::list_payment_fees`].
    pub organization_token_file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MollieOptions {
    /// Must end in `/`. `http` is only allowed for 127.0.0.1 and localhost.
    pub base_url: String,
    pub connect_timeout: Duration,
    pub timeout: Duration,
    /// Attempts for GET requests; POSTs are sent once.
    pub get_attempts: u32,
    /// Waited before the second attempt, doubled for each one after.
    pub retry_delay: Duration,
}

impl Default for MollieOptions {
    fn default() -> Self {
        Self {
            base_url: MOLLIE_API_URL.to_string(),
            connect_timeout: Duration::from_secs(5),
            timeout: Duration::from_secs(15),
            get_attempts: 3,
            retry_delay: Duration::from_millis(250),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error("base URL {0:?} must be https (or http on 127.0.0.1/localhost) and end in /")]
    BaseUrl(String),
    #[error("no trusted root certificates found on this system")]
    NoRootCertificates,
    #[error("cannot build the HTTP client: {0}")]
    Http(String),
}

/// Our own small client for Mollie's v2 REST API (ADR 0013).
pub struct MollieClient {
    http: reqwest::Client,
    base: Url,
    api_key: ApiKey,
    organization_token: Option<OrganizationToken>,
    get_attempts: u32,
    retry_delay: Duration,
}

impl fmt::Debug for MollieClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MollieClient")
            .field("base", &self.base.as_str())
            .field("api_key", &self.api_key)
            .field("organization_token", &self.organization_token)
            .finish_non_exhaustive()
    }
}

fn check_base_url(raw: &str) -> Result<Url, SetupError> {
    let bad = || SetupError::BaseUrl(raw.to_string());
    let url = Url::parse(raw).map_err(|_| bad())?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost"));
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && local);
    if !scheme_ok
        || !url.path().ends_with('/')
        || url.query().is_some()
        || !url.username().is_empty()
    {
        return Err(bad());
    }
    Ok(url)
}

fn check_native_roots() -> Result<(), SetupError> {
    let loaded = rustls_native_certs::load_native_certs();
    if !loaded.errors.is_empty() {
        tracing::warn!(
            errors = loaded.errors.len(),
            "some root certificates could not be loaded"
        );
    }
    if loaded.certs.is_empty() {
        return Err(SetupError::NoRootCertificates);
    }
    Ok(())
}

impl MollieClient {
    /// # Errors
    /// [`SetupError`] for an unreadable secret, a bad base URL or missing root certificates.
    pub fn from_config(config: &MollieConfig, options: MollieOptions) -> Result<Self, SetupError> {
        let api_key = ApiKey::from_file(&config.api_key_file)?;
        let organization_token = config
            .organization_token_file
            .as_ref()
            .map(OrganizationToken::from_file)
            .transpose()?;
        Self::new(api_key, organization_token, options)
    }

    /// # Errors
    /// [`SetupError`] for a bad base URL or missing root certificates.
    pub fn new(
        api_key: ApiKey,
        organization_token: Option<OrganizationToken>,
        options: MollieOptions,
    ) -> Result<Self, SetupError> {
        let base = check_base_url(&options.base_url)?;
        let https = base.scheme() == "https";
        if https {
            check_native_roots()?;
        }
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .tls_built_in_root_certs(https)
            .https_only(https)
            .connect_timeout(options.connect_timeout)
            .timeout(options.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("bakflow-payments/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|err| SetupError::Http(err.to_string()))?;
        Ok(Self {
            http,
            base,
            api_key,
            organization_token,
            get_attempts: options.get_attempts.max(1),
            retry_delay: options.retry_delay,
        })
    }

    fn url(&self, path: &str) -> Url {
        self.base
            .join(path)
            .expect("paths are built from validated tokens")
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        token: &str,
        body: Option<(&Value, &IdempotencyKey)>,
    ) -> Result<T, PaymentError> {
        let attempts = if method == Method::GET {
            self.get_attempts
        } else {
            1
        };
        let mut delay = self.retry_delay;
        let mut attempt = 1;
        loop {
            let result = self.send(method.clone(), url.clone(), token, body).await;
            match result {
                Err(PaymentError::Unavailable(_) | PaymentError::RateLimited)
                    if attempt < attempts =>
                {
                    tokio::time::sleep(delay).await;
                    delay = delay.saturating_mul(2);
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        token: &str,
        body: Option<(&Value, &IdempotencyKey)>,
    ) -> Result<T, PaymentError> {
        let path = url.path().to_string();
        let mut request = self
            .http
            .request(method.clone(), url)
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some((body, key)) = body {
            request = request.header("Idempotency-Key", key.as_str()).json(body);
        }
        let response = request.send().await.map_err(transport_error)?;
        let status = response.status();
        tracing::debug!(%method, path, status = status.as_u16(), "mollie request");
        let bytes = response.bytes().await.map_err(transport_error)?;
        if status.is_success() {
            serde_json::from_slice(&bytes).map_err(|err| {
                PaymentError::InvalidResponse(format!(
                    "{path}: {:?} JSON error at line {} column {}",
                    err.classify(),
                    err.line(),
                    err.column()
                ))
            })
        } else {
            Err(status_error(status, &bytes))
        }
    }

    async fn create(&self, request: &NewPayment) -> Result<CreatedPayment, PaymentError> {
        check_amount(request.amount)?;
        check_description(&request.description)?;
        let mut body = json!({
            "amount": { "currency": "EUR", "value": format_value(request.amount) },
            "description": request.description,
            "redirectUrl": request.redirect_url,
            "webhookUrl": request.webhook_url,
            "metadata": { "order_id": request.order.get() },
        });
        if !request.methods.is_empty() {
            let methods: Vec<_> = request.methods.iter().map(|m| m.as_str()).collect();
            body["method"] = json!(methods);
        }
        let key = key_or_fresh(request.idempotency_key.as_ref());
        let payment: WirePayment = self
            .call(
                Method::POST,
                self.url("payments"),
                self.api_key.expose(),
                Some((&body, &key)),
            )
            .await?;
        let checkout_url = payment
            .links
            .checkout
            .as_ref()
            .map(|link| link.href.clone())
            .filter(|href| href.starts_with("https://"))
            .ok_or_else(|| invalid("payment has no https checkout link"))?;
        Ok(CreatedPayment {
            id: payment_id(&payment.id)?,
            checkout_url,
        })
    }

    async fn get(&self, id: &ProviderPaymentId) -> Result<PaymentSnapshot, PaymentError> {
        let payment: WirePayment = self
            .call(
                Method::GET,
                self.url(&format!("payments/{id}")),
                self.api_key.expose(),
                None,
            )
            .await?;
        if payment.id != id.as_str() {
            return Err(invalid("payment id in the answer does not match"));
        }
        payment.into_snapshot()
    }

    async fn do_refund(&self, request: &RefundRequest) -> Result<CreatedRefund, PaymentError> {
        check_amount(request.amount)?;
        check_description(&request.description)?;
        let body = json!({
            "amount": { "currency": "EUR", "value": format_value(request.amount) },
            "description": request.description,
        });
        let key = &request.idempotency_key;
        let refund: WireRefund = self
            .call(
                Method::POST,
                self.url(&format!("payments/{}/refunds", request.payment)),
                self.api_key.expose(),
                Some((&body, key)),
            )
            .await?;
        Ok(CreatedRefund {
            id: ProviderRefundId::new(refund.id)
                .map_err(|_| invalid("refund id is not a valid token"))?,
            status: refund_status(&refund.status)?,
        })
    }

    async fn fees(&self, cursor: Option<&FeeCursor>) -> Result<FeePage, PaymentError> {
        let token = self.organization_token.as_ref().ok_or_else(|| {
            PaymentError::NotConfigured("Mollie organization access token (balances.read)".into())
        })?;
        let mut url = self.url("balances/primary/transactions");
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", FEE_PAGE_SIZE);
            if self.api_key.is_test() {
                query.append_pair("testmode", "true");
            }
            if let Some(cursor) = cursor {
                query.append_pair("from", cursor.as_str());
            }
        }
        let page: WireTransactions = self.call(Method::GET, url, token.expose(), None).await?;
        let mut fees = Vec::new();
        for transaction in page.embedded.balance_transactions {
            let id = transaction.id.clone();
            match transaction.into_fee() {
                Ok(Some(fee)) => fees.push(fee),
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(transaction = id, error = %err, "skipped a Mollie balance transaction")
                }
            }
        }
        let next = page
            .links
            .next
            .map(|link| next_cursor(&link.href))
            .transpose()?;
        Ok(FeePage { fees, next })
    }
}

impl PaymentProvider for MollieClient {
    fn create_payment<'a>(&'a self, request: &'a NewPayment) -> ProviderFuture<'a, CreatedPayment> {
        Box::pin(self.create(request))
    }

    fn get_payment<'a>(&'a self, id: &'a ProviderPaymentId) -> ProviderFuture<'a, PaymentSnapshot> {
        Box::pin(self.get(id))
    }

    fn refund<'a>(&'a self, request: &'a RefundRequest) -> ProviderFuture<'a, CreatedRefund> {
        Box::pin(self.do_refund(request))
    }

    fn list_payment_fees<'a>(
        &'a self,
        cursor: Option<&'a FeeCursor>,
    ) -> ProviderFuture<'a, FeePage> {
        Box::pin(self.fees(cursor))
    }
}

fn key_or_fresh(key: Option<&IdempotencyKey>) -> IdempotencyKey {
    key.cloned().unwrap_or_else(|| {
        IdempotencyKey::new(uuid::Uuid::new_v4().simple().to_string())
            .expect("a simple uuid is a valid token")
    })
}

fn check_amount(amount: Money) -> Result<(), PaymentError> {
    if amount <= Money::ZERO {
        return Err(PaymentError::InvalidRequest(
            "amount must be positive".into(),
        ));
    }
    Ok(())
}

fn check_description(description: &str) -> Result<(), PaymentError> {
    let chars = description.chars().count();
    if description.trim().is_empty() || chars > MAX_DESCRIPTION {
        return Err(PaymentError::InvalidRequest(format!(
            "description must have 1 to {MAX_DESCRIPTION} characters"
        )));
    }
    Ok(())
}

fn transport_error(err: reqwest::Error) -> PaymentError {
    let what = if err.is_timeout() {
        "request timed out"
    } else if err.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    PaymentError::Unavailable(what.to_string())
}

#[derive(Deserialize)]
struct WireError {
    detail: Option<String>,
    field: Option<String>,
}

fn status_error(status: StatusCode, body: &[u8]) -> PaymentError {
    let parsed: Option<WireError> = serde_json::from_slice(body).ok();
    let (detail, field) = parsed.map_or((None, None), |e| (e.detail, e.field));
    match status.as_u16() {
        401 | 403 => PaymentError::Unauthorized,
        404 | 410 => PaymentError::NotFound,
        409 => PaymentError::Conflict,
        429 => PaymentError::RateLimited,
        400 | 422 => PaymentError::Rejected {
            field,
            detail: detail.unwrap_or_else(|| status.to_string()),
        },
        500..=599 => PaymentError::Unavailable(format!("HTTP {}", status.as_u16())),
        other => PaymentError::InvalidResponse(format!("unexpected HTTP {other}")),
    }
}

fn invalid(what: &str) -> PaymentError {
    PaymentError::InvalidResponse(what.to_string())
}

fn amount_error(err: AmountError) -> PaymentError {
    PaymentError::InvalidResponse(err.to_string())
}

fn payment_id(raw: &str) -> Result<ProviderPaymentId, PaymentError> {
    ProviderPaymentId::new(raw).map_err(|_| invalid("payment id is not a valid token"))
}

fn next_cursor(href: &str) -> Result<FeeCursor, PaymentError> {
    let url = Url::parse(href).map_err(|_| invalid("next link is not a URL"))?;
    let from = url
        .query_pairs()
        .find(|(key, _)| key == "from")
        .ok_or_else(|| invalid("next link has no from parameter"))?;
    FeeCursor::new(from.1.into_owned()).map_err(|_| invalid("next cursor is not a valid token"))
}

#[derive(Deserialize)]
struct WireAmount {
    currency: String,
    value: String,
}

impl WireAmount {
    fn money(&self) -> Result<Money, PaymentError> {
        parse_amount(&self.currency, &self.value).map_err(amount_error)
    }
}

#[derive(Deserialize)]
struct WireLink {
    href: String,
}

#[derive(Deserialize, Default)]
struct WirePaymentLinks {
    checkout: Option<WireLink>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePayment {
    id: String,
    status: String,
    amount: WireAmount,
    amount_refunded: Option<WireAmount>,
    method: Option<String>,
    metadata: Option<Value>,
    #[serde(rename = "_links", default)]
    links: WirePaymentLinks,
}

impl WirePayment {
    fn into_snapshot(self) -> Result<PaymentSnapshot, PaymentError> {
        let status = match self.status.as_str() {
            "open" => PaymentStatus::Open,
            "pending" => PaymentStatus::Pending,
            "authorized" => PaymentStatus::Authorized,
            "paid" => PaymentStatus::Paid,
            "failed" => PaymentStatus::Failed,
            "canceled" => PaymentStatus::Canceled,
            "expired" => PaymentStatus::Expired,
            other => return Err(invalid(&format!("unknown payment status {other:?}"))),
        };
        let amount_refunded = match &self.amount_refunded {
            Some(amount) => amount.money()?,
            None => Money::ZERO,
        };
        let order = self
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("order_id"))
            .and_then(Value::as_i64)
            .map(OrderId::new);
        Ok(PaymentSnapshot {
            id: payment_id(&self.id)?,
            status,
            amount: self.amount.money()?,
            amount_refunded,
            method: self.method,
            order,
        })
    }
}

#[derive(Deserialize)]
struct WireRefund {
    id: String,
    status: String,
}

fn refund_status(raw: &str) -> Result<RefundStatus, PaymentError> {
    Ok(match raw {
        "queued" => RefundStatus::Queued,
        "pending" => RefundStatus::Pending,
        "processing" => RefundStatus::Processing,
        "refunded" => RefundStatus::Refunded,
        "failed" => RefundStatus::Failed,
        "canceled" => RefundStatus::Canceled,
        other => return Err(invalid(&format!("unknown refund status {other:?}"))),
    })
}

#[derive(Deserialize)]
struct WireTransactions {
    #[serde(rename = "_embedded")]
    embedded: WireEmbedded,
    #[serde(rename = "_links")]
    links: WireTransactionLinks,
}

#[derive(Deserialize)]
struct WireEmbedded {
    balance_transactions: Vec<WireTransaction>,
}

#[derive(Deserialize)]
struct WireTransactionLinks {
    next: Option<WireLink>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTransaction {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    initial_amount: WireAmount,
    result_amount: WireAmount,
    deductions: Option<WireAmount>,
    deduction_details: Option<WireDeductionDetails>,
    context: Option<WireContext>,
}

#[derive(Deserialize)]
struct WireDeductionDetails {
    fees: Option<WireAmount>,
    commissions: Option<WireAmount>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireContext {
    payment_id: Option<String>,
}

fn optional_money(amount: Option<&WireAmount>) -> Result<Money, PaymentError> {
    amount.map_or(Ok(Money::ZERO), WireAmount::money)
}

impl WireTransaction {
    /// Mollie reports deductions as negative amounts; the fee is what Mollie charged
    /// (fees plus commissions), not reservations or repayments.
    fn into_fee(self) -> Result<Option<PaymentFee>, PaymentError> {
        let kind = match self.kind.as_str() {
            "payment" => FeeKind::Payment,
            "refund" => FeeKind::Refund,
            _ => return Ok(None),
        };
        let payment = self
            .context
            .and_then(|context| context.payment_id)
            .ok_or_else(|| invalid("transaction without paymentId"))?;
        let charged = match &self.deduction_details {
            Some(details) => optional_money(details.fees.as_ref())?
                .checked_add(optional_money(details.commissions.as_ref())?)
                .map_err(|_| invalid("fee out of range"))?,
            None => optional_money(self.deductions.as_ref())?,
        };
        let fee = charged
            .checked_neg()
            .map_err(|_| invalid("fee out of range"))?;
        if fee < Money::ZERO {
            return Err(invalid("fee is negative"));
        }
        Ok(Some(PaymentFee {
            payment: payment_id(&payment)?,
            kind,
            gross: self.initial_amount.money()?,
            fee,
            net: self.result_amount.money()?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls() {
        for good in [
            "https://api.mollie.com/v2/",
            "http://127.0.0.1:8080/v2/",
            "http://localhost/",
        ] {
            assert!(check_base_url(good).is_ok(), "{good}");
        }
        for bad in [
            "http://api.mollie.com/v2/",
            "https://api.mollie.com/v2",
            "https://api.mollie.com/v2/?x=1",
            "https://user@api.mollie.com/v2/",
            "ftp://127.0.0.1/",
            "not a url",
        ] {
            assert!(
                matches!(check_base_url(bad), Err(SetupError::BaseUrl(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn status_mapping() {
        let body = br#"{"status":422,"title":"Unprocessable Entity","detail":"The amount is too low","field":"amount"}"#;
        assert_eq!(
            status_error(StatusCode::UNPROCESSABLE_ENTITY, body),
            PaymentError::Rejected {
                field: Some("amount".into()),
                detail: "The amount is too low".into()
            }
        );
        assert_eq!(
            status_error(StatusCode::BAD_REQUEST, b"not json"),
            PaymentError::Rejected {
                field: None,
                detail: "400 Bad Request".into()
            }
        );
        assert_eq!(
            status_error(StatusCode::FORBIDDEN, b""),
            PaymentError::Unauthorized
        );
        assert_eq!(status_error(StatusCode::GONE, b""), PaymentError::NotFound);
        assert_eq!(
            status_error(StatusCode::CONFLICT, b""),
            PaymentError::Conflict
        );
        assert_eq!(
            status_error(StatusCode::IM_A_TEAPOT, b""),
            PaymentError::InvalidResponse("unexpected HTTP 418".into())
        );
    }

    #[test]
    fn descriptions_and_amounts() {
        assert!(check_description("Bestelling 7").is_ok());
        assert!(check_description(&"é".repeat(255)).is_ok());
        assert!(check_description(&"é".repeat(256)).is_err());
        assert!(check_description("  ").is_err());
        assert!(check_amount(Money::from_cents(1)).is_ok());
        assert!(check_amount(Money::ZERO).is_err());
        assert!(check_amount(Money::from_cents(-1)).is_err());
    }

    #[test]
    fn fresh_idempotency_keys() {
        let a = key_or_fresh(None);
        let b = key_or_fresh(None);
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 32);
        let given = IdempotencyKey::new("refund-7-1").unwrap();
        assert_eq!(key_or_fresh(Some(&given)), given);
    }

    #[test]
    fn next_cursors() {
        assert_eq!(
            next_cursor(
                "https://api.mollie.com/v2/balances/primary/transactions?from=baltr_abc&limit=250"
            )
            .unwrap()
            .as_str(),
            "baltr_abc"
        );
        assert!(next_cursor("nope").is_err());
        assert!(next_cursor("https://api.mollie.com/v2/x?limit=5").is_err());
        assert!(next_cursor("https://api.mollie.com/v2/x?from=a%20b").is_err());
    }

    #[test]
    fn https_client_loads_system_roots_or_says_why() {
        let key = ApiKey::new("test_abc").unwrap();
        match MollieClient::new(key, None, MollieOptions::default()) {
            Ok(client) => assert!(format!("{client:?}").contains("<redacted>")),
            Err(err) => assert!(
                matches!(err, SetupError::NoRootCertificates | SetupError::Http(_)),
                "{err}"
            ),
        }
    }
}
