use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::extract::{Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// Where the mock sends customers to pay. The server's CSP lets forms go to this host;
/// the browser resolves it to localhost (`browser::firefox`), so no test reaches Mollie.
pub const CHECKOUT_BASE: &str = "https://www.mollie.com/checkout/select-method/";

/// A payment as the mock keeps it; `value` is Mollie's decimal string, such as `4.50`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payment {
    pub id: String,
    pub value: String,
    pub redirect_url: String,
    pub webhook_url: String,
    pub metadata: Value,
    pub status: &'static str,
    idempotency_key: Option<String>,
}

impl Payment {
    #[must_use]
    pub fn checkout_url(&self) -> String {
        format!("{CHECKOUT_BASE}{}", self.id)
    }

    fn wire(&self) -> Value {
        let mut wire = json!({
            "resource": "payment",
            "id": self.id,
            "mode": "test",
            "status": self.status,
            "amount": { "currency": "EUR", "value": self.value },
            "amountRefunded": { "currency": "EUR", "value": "0.00" },
            "method": if self.status == "paid" { json!("ideal") } else { Value::Null },
            "metadata": self.metadata,
            "redirectUrl": self.redirect_url,
            "webhookUrl": self.webhook_url,
            "_links": {},
        });
        if self.status == "open" {
            wire["_links"]["checkout"] = json!({ "href": self.checkout_url() });
        }
        wire
    }
}

#[derive(Debug, Default)]
struct Inner {
    next: u64,
    payments: Vec<Payment>,
}

/// The part of Mollie's v2 API that `payments::MollieClient` uses for checkout: create a
/// payment and read it back. Tests move a payment with [`Self::pay`] and send Mollie's
/// webhook with [`Self::notify`].
#[derive(Debug, Clone, Default)]
pub struct MockMollie {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Deserialize)]
struct WireAmount {
    currency: String,
    value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewPayment {
    amount: WireAmount,
    redirect_url: String,
    webhook_url: String,
    #[serde(default)]
    metadata: Value,
}

fn error(status: StatusCode, detail: &str) -> Response {
    (
        status,
        Json(json!({ "status": status.as_u16(), "title": status.canonical_reason(), "detail": detail })),
    )
        .into_response()
}

fn has_test_key(headers: &HeaderMap) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer test_"))
        .is_some_and(|rest| !rest.is_empty())
}

async fn create(
    State(mock): State<MockMollie>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !has_test_key(&headers) {
        return error(StatusCode::UNAUTHORIZED, "Missing or invalid test API key");
    }
    let Ok(request) = serde_json::from_slice::<NewPayment>(&body) else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid payment body");
    };
    if request.amount.currency != "EUR" {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "Only EUR is supported");
    }
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut inner = mock.lock();
    if let Some(key) = &key
        && let Some(known) = inner
            .payments
            .iter()
            .find(|p| p.idempotency_key.as_ref() == Some(key))
    {
        return (StatusCode::CREATED, Json(known.wire())).into_response();
    }
    inner.next += 1;
    let payment = Payment {
        id: format!("tr_e2e{}", inner.next),
        value: request.amount.value,
        redirect_url: request.redirect_url,
        webhook_url: request.webhook_url,
        metadata: request.metadata,
        status: "open",
        idempotency_key: key,
    };
    let wire = payment.wire();
    inner.payments.push(payment);
    (StatusCode::CREATED, Json(wire)).into_response()
}

async fn read(
    State(mock): State<MockMollie>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !has_test_key(&headers) {
        return error(StatusCode::UNAUTHORIZED, "Missing or invalid test API key");
    }
    match mock.payment(&id) {
        Some(payment) => Json(payment.wire()).into_response(),
        None => error(StatusCode::NOT_FOUND, "No payment exists with token"),
    }
}

impl MockMollie {
    /// Serves the mock on `listener` until the process ends. Point the server's
    /// `MOLLIE_API_URL` at `http://<addr>/v2/`.
    ///
    /// # Errors
    /// When the listener has no local address.
    pub fn start(listener: TcpListener) -> io::Result<(Self, SocketAddr)> {
        let addr = listener.local_addr()?;
        let mock = Self::default();
        let app = Router::new()
            .route("/v2/payments", post(create))
            .route("/v2/payments/{id}", get(read))
            .with_state(mock.clone());
        tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                eprintln!("mock Mollie stopped: {err}");
            }
        });
        Ok((mock, addr))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn payment(&self, id: &str) -> Option<Payment> {
        self.lock().payments.iter().find(|p| p.id == id).cloned()
    }

    /// The customer paid at Mollie. False for an unknown payment.
    pub fn pay(&self, id: &str) -> bool {
        let mut inner = self.lock();
        match inner.payments.iter_mut().find(|p| p.id == id) {
            Some(payment) => {
                payment.status = "paid";
                true
            }
            None => false,
        }
    }

    /// Posts Mollie's webhook (`id=tr_…`) to the payment's webhook path on `server`. The
    /// payment's own webhook URL has the shop's public host, which the test cannot reach.
    ///
    /// # Errors
    /// For an unknown payment, a bad URL or a failed request.
    pub async fn notify(&self, id: &str, server: &str) -> Result<u16, String> {
        let payment = self.payment(id).ok_or_else(|| format!("no payment {id}"))?;
        let url = on_server(&payment.webhook_url, server)?;
        let response = reqwest::Client::new()
            .post(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(format!("id={id}"))
            .send()
            .await
            .map_err(|err| format!("webhook: {err}"))?;
        Ok(response.status().as_u16())
    }
}

/// `public` with its scheme and host replaced by `server`'s.
///
/// # Errors
/// When either is not a URL.
pub fn on_server(public: &str, server: &str) -> Result<Url, String> {
    let public = Url::parse(public).map_err(|err| format!("{public}: {err}"))?;
    let mut url = Url::parse(server).map_err(|err| format!("{server}: {err}"))?;
    url.set_path(public.path());
    url.set_query(public.query());
    Ok(url)
}

#[cfg(test)]
mod tests {
    use domain::{
        IdempotencyKey, Money, NewPayment, OrderId, PaymentError, PaymentProvider, PaymentStatus,
        ProviderPaymentId,
    };
    use payments::{ApiKey, MollieClient, MollieOptions};

    use super::*;

    const KEY: &str = "test_dHar4XY7LxsDOtmnkVtjNVWXLSlXsM";

    async fn started() -> (MockMollie, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        MockMollie::start(listener).unwrap()
    }

    fn client(addr: SocketAddr, key: &str) -> MollieClient {
        MollieClient::new(
            ApiKey::new(key).unwrap(),
            None,
            MollieOptions {
                base_url: format!("http://{addr}/v2/"),
                ..MollieOptions::default()
            },
        )
        .unwrap()
    }

    fn request() -> NewPayment {
        NewPayment {
            amount: Money::from_cents(450),
            description: "Bestelling 1".to_owned(),
            redirect_url: "https://shop.example.org/bestelling/abc".to_owned(),
            webhook_url: "https://shop.example.org/webhooks/mollie".to_owned(),
            order: OrderId::new(7),
            methods: Vec::new(),
            idempotency_key: Some(IdempotencyKey::new("order-7-payment").unwrap()),
        }
    }

    #[tokio::test]
    async fn our_mollie_client_creates_reads_and_sees_a_payment_paid() {
        let (mock, addr) = started().await;
        let client = client(addr, KEY);

        let created = client.create_payment(&request()).await.unwrap();
        assert_eq!(created.id.as_str(), "tr_e2e1");
        assert_eq!(created.checkout_url, format!("{CHECKOUT_BASE}tr_e2e1"));
        let replayed = client.create_payment(&request()).await.unwrap();
        assert_eq!(
            replayed.id, created.id,
            "same idempotency key, same payment"
        );

        let open = client.get_payment(&created.id).await.unwrap();
        assert_eq!(open.status, PaymentStatus::Open);
        assert_eq!(open.amount, Money::from_cents(450));
        assert_eq!(open.order, Some(OrderId::new(7)));

        assert!(mock.pay("tr_e2e1"));
        let paid = client.get_payment(&created.id).await.unwrap();
        assert_eq!(paid.status, PaymentStatus::Paid);
        assert_eq!(paid.method.as_deref(), Some("ideal"));
        let kept = mock.payment("tr_e2e1").unwrap();
        assert_eq!(kept.redirect_url, "https://shop.example.org/bestelling/abc");
    }

    #[tokio::test]
    async fn a_live_key_and_an_unknown_payment_are_refused() {
        let (mock, addr) = started().await;
        let live = client(addr, "live_dHar4XY7LxsDOtmnkVtjNVWXLSlXsM");
        assert!(matches!(
            live.create_payment(&request()).await,
            Err(PaymentError::Unauthorized)
        ));
        let unknown = ProviderPaymentId::new("tr_nope").unwrap();
        assert!(matches!(
            client(addr, KEY).get_payment(&unknown).await,
            Err(PaymentError::NotFound)
        ));
        assert!(!mock.pay("tr_nope"));
    }

    #[tokio::test]
    async fn the_webhook_goes_to_the_servers_own_address() {
        let (mock, addr) = started().await;
        client(addr, KEY).create_payment(&request()).await.unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let app = Router::new().route(
            "/webhooks/mollie",
            post(move |body: String| async move {
                record.lock().unwrap().push(body);
                StatusCode::OK
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        assert_eq!(mock.notify("tr_e2e1", &server).await, Ok(200));
        assert_eq!(*seen.lock().unwrap(), vec!["id=tr_e2e1".to_owned()]);
        assert!(mock.notify("tr_nope", &server).await.is_err());
    }

    #[test]
    fn on_server_keeps_path_and_query() {
        let url = on_server(
            "https://shop.example.org/bestelling/abc?x=1",
            "http://127.0.0.1:8080",
        )
        .unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:8080/bestelling/abc?x=1");
        assert!(on_server("not a url", "http://127.0.0.1:8080").is_err());
    }
}
