#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use domain::{
    FeeCursor, FeeKind, IdempotencyKey, Money, NewPayment, OrderId, PaymentError, PaymentFee,
    PaymentMethod, PaymentProvider, PaymentStatus, ProviderPaymentId, RefundRequest, RefundStatus,
};
use payments::{ApiKey, MollieClient, MollieConfig, MollieOptions, OrganizationToken};
use serde_json::{Value, json};

#[path = "support/contract.rs"]
mod contract;
#[path = "support/mock.rs"]
mod mock;

use mock::{MockServer, Recorded, Reply};

const KEY: &str = "test_dHar4XY7LxsDOtmnkVtjNVWXLSlXsM";
const TOKEN: &str = "access_Wwvu7egPcJLLJ9Kb7J632x8wJ2zMeJ";

fn options(server: &MockServer) -> MollieOptions {
    MollieOptions {
        base_url: server.base_url(),
        timeout: Duration::from_secs(5),
        retry_delay: Duration::ZERO,
        ..MollieOptions::default()
    }
}

fn client(server: &MockServer) -> MollieClient {
    MollieClient::new(ApiKey::new(KEY).unwrap(), None, options(server)).unwrap()
}

fn client_with_token(server: &MockServer) -> MollieClient {
    MollieClient::new(
        ApiKey::new(KEY).unwrap(),
        Some(OrganizationToken::new(TOKEN).unwrap()),
        options(server),
    )
    .unwrap()
}

fn new_payment() -> NewPayment {
    NewPayment {
        amount: Money::from_cents(1234),
        description: "Bestelling 7".into(),
        redirect_url: "https://shop.test/bestelling/7".into(),
        webhook_url: "https://shop.test/webhooks/mollie".into(),
        order: OrderId::new(7),
        methods: vec![
            PaymentMethod::Ideal,
            PaymentMethod::Wero,
            PaymentMethod::CreditCard,
            PaymentMethod::Bancontact,
        ],
        idempotency_key: Some(IdempotencyKey::new("order-7-1").unwrap()),
    }
}

fn payment_json(id: &str, status: &str) -> Value {
    json!({
        "resource": "payment",
        "id": id,
        "mode": "test",
        "status": status,
        "amount": { "currency": "EUR", "value": "12.34" },
        "description": "Bestelling 7",
        "method": null,
        "metadata": { "order_id": 7 },
        "_links": {
            "self": { "href": format!("https://api.mollie.com/v2/payments/{id}"), "type": "application/hal+json" },
            "checkout": { "href": format!("https://www.mollie.com/checkout/select-method/{id}"), "type": "text/html" }
        }
    })
}

fn error_json(status: u16, detail: &str) -> Value {
    json!({ "status": status, "title": "Error", "detail": detail })
}

fn tr(id: &str) -> ProviderPaymentId {
    ProviderPaymentId::new(id).unwrap()
}

/// Mollie as far as the contract test needs it.
fn fake_mollie() -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
    let payments: Arc<Mutex<HashMap<String, Value>>> = Arc::default();
    let replays: Arc<Mutex<HashMap<String, Value>>> = Arc::default();
    move |request| {
        let mut payments = payments.lock().unwrap();
        match (request.method.as_str(), request.target.as_str()) {
            ("POST", "/v2/payments") => {
                let key = request.header("idempotency-key").unwrap().to_string();
                if let Some(replay) = replays.lock().unwrap().get(&key) {
                    return Reply::json(201, replay.clone());
                }
                let body = request.json();
                let id = format!("tr_mock{}", payments.len() + 1);
                let mut payment = payment_json(&id, "open");
                payment["amount"] = body["amount"].clone();
                payment["metadata"] = body["metadata"].clone();
                payment["method"] = body["method"][0].clone();
                payments.insert(id, payment.clone());
                replays.lock().unwrap().insert(key, payment.clone());
                Reply::json(201, payment)
            }
            ("GET", target) => match payments.get(target.trim_start_matches("/v2/payments/")) {
                Some(payment) => Reply::json(200, payment.clone()),
                None => Reply::json(404, error_json(404, "No payment exists with token")),
            },
            ("POST", target) if target.ends_with("/refunds") => {
                Reply::json(422, error_json(422, "The payment is not refundable"))
            }
            _ => Reply::raw(500, ""),
        }
    }
}

#[tokio::test]
async fn contract_against_the_mock() {
    let server = MockServer::start(fake_mollie()).await;
    contract::run(&client(&server), "mock").await;
}

#[tokio::test]
async fn create_payment_request_shape() {
    let server =
        MockServer::start(|_| Reply::json(201, payment_json("tr_WDqYK6vllg", "open"))).await;
    let created = client(&server)
        .create_payment(&new_payment())
        .await
        .unwrap();
    assert_eq!(created.id.as_str(), "tr_WDqYK6vllg");
    assert_eq!(
        created.checkout_url,
        "https://www.mollie.com/checkout/select-method/tr_WDqYK6vllg"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/v2/payments");
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(request.header("idempotency-key"), Some("order-7-1"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("accept"), Some("application/json"));
    assert!(
        request
            .header("user-agent")
            .is_some_and(|ua| ua.starts_with("bakflow-payments/"))
    );
    assert_eq!(
        request.json(),
        json!({
            "amount": { "currency": "EUR", "value": "12.34" },
            "description": "Bestelling 7",
            "redirectUrl": "https://shop.test/bestelling/7",
            "webhookUrl": "https://shop.test/webhooks/mollie",
            "metadata": { "order_id": 7 },
            "method": ["ideal", "wero", "creditcard", "bancontact"],
        })
    );
}

#[tokio::test]
async fn no_methods_lets_mollie_choose_and_a_key_is_generated() {
    let server = MockServer::start(|_| Reply::json(201, payment_json("tr_a1", "open"))).await;
    let request = NewPayment {
        methods: Vec::new(),
        idempotency_key: None,
        ..new_payment()
    };
    let client = client(&server);
    client.create_payment(&request).await.unwrap();
    client.create_payment(&request).await.unwrap();
    let requests = server.requests();
    assert!(requests[0].json().get("method").is_none());
    let keys: Vec<_> = requests
        .iter()
        .map(|r| r.header("idempotency-key").unwrap().to_string())
        .collect();
    assert_eq!(keys[0].len(), 32);
    assert_ne!(keys[0], keys[1]);
}

#[tokio::test]
async fn invalid_requests_are_not_sent() {
    let server = MockServer::start(|_| Reply::raw(500, "")).await;
    let client = client(&server);
    let zero = NewPayment {
        amount: Money::ZERO,
        ..new_payment()
    };
    assert!(matches!(
        client.create_payment(&zero).await,
        Err(PaymentError::InvalidRequest(_))
    ));
    let blank = NewPayment {
        description: " ".into(),
        ..new_payment()
    };
    assert!(matches!(
        client.create_payment(&blank).await,
        Err(PaymentError::InvalidRequest(_))
    ));
    let refund = RefundRequest {
        payment: tr("tr_a1"),
        amount: Money::from_cents(-5),
        description: "Terug".into(),
        idempotency_key: IdempotencyKey::new("refund-a1-1").unwrap(),
    };
    assert!(matches!(
        client.refund(&refund).await,
        Err(PaymentError::InvalidRequest(_))
    ));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn get_payment_reads_a_paid_and_refunded_payment() {
    let server = MockServer::start(|_| {
        let mut payment = payment_json("tr_paid1", "paid");
        payment["method"] = json!("wero");
        payment["amountRefunded"] = json!({ "currency": "EUR", "value": "2.34" });
        payment["amountRemaining"] = json!({ "currency": "EUR", "value": "10.00" });
        Reply::json(200, payment)
    })
    .await;
    let snapshot = client(&server).get_payment(&tr("tr_paid1")).await.unwrap();
    assert_eq!(snapshot.status, PaymentStatus::Paid);
    assert_eq!(snapshot.amount, Money::from_cents(1234));
    assert_eq!(snapshot.amount_refunded, Money::from_cents(234));
    assert_eq!(snapshot.method.as_deref(), Some("wero"));
    assert_eq!(snapshot.order, Some(OrderId::new(7)));
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/v2/payments/tr_paid1");
    assert_eq!(request.header("idempotency-key"), None);
}

#[tokio::test]
async fn every_payment_status() {
    for (raw, status) in [
        ("open", PaymentStatus::Open),
        ("pending", PaymentStatus::Pending),
        ("authorized", PaymentStatus::Authorized),
        ("paid", PaymentStatus::Paid),
        ("failed", PaymentStatus::Failed),
        ("canceled", PaymentStatus::Canceled),
        ("expired", PaymentStatus::Expired),
    ] {
        let server = MockServer::start(move |_| {
            let mut payment = payment_json("tr_s1", raw);
            payment["metadata"] = Value::Null;
            Reply::json(200, payment)
        })
        .await;
        let snapshot = client(&server).get_payment(&tr("tr_s1")).await.unwrap();
        assert_eq!(snapshot.status, status);
        assert_eq!(snapshot.order, None);
        assert_eq!(snapshot.amount_refunded, Money::ZERO);
    }
}

#[tokio::test]
async fn error_statuses_map_to_typed_errors() {
    let cases = [
        (401, PaymentError::Unauthorized),
        (403, PaymentError::Unauthorized),
        (404, PaymentError::NotFound),
        (
            422,
            PaymentError::Rejected {
                field: None,
                detail: "The payment is not refundable".into(),
            },
        ),
        (409, PaymentError::Conflict),
        (429, PaymentError::RateLimited),
        (500, PaymentError::Unavailable("HTTP 500".into())),
        (503, PaymentError::Unavailable("HTTP 503".into())),
    ];
    for (status, expected) in cases {
        let server = MockServer::start(move |_| {
            Reply::json(status, error_json(status, "The payment is not refundable"))
        })
        .await;
        let result = client(&server).create_payment(&new_payment()).await;
        assert_eq!(result, Err(expected), "HTTP {status}");
        assert_eq!(server.requests().len(), 1, "POST is never retried");
    }
}

#[tokio::test]
async fn gets_are_retried_on_5xx_and_429() {
    let server = MockServer::start(|_| Reply::raw(502, "<html>bad gateway</html>")).await;
    let result = client(&server).get_payment(&tr("tr_r1")).await;
    assert_eq!(result, Err(PaymentError::Unavailable("HTTP 502".into())));
    assert_eq!(server.requests().len(), 3);

    let calls = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&calls);
    let server = MockServer::start(move |_| {
        let mut calls = counter.lock().unwrap();
        *calls += 1;
        if *calls == 1 {
            Reply::json(429, error_json(429, "Too many requests"))
        } else {
            Reply::json(200, payment_json("tr_r2", "paid"))
        }
    })
    .await;
    let snapshot = client(&server).get_payment(&tr("tr_r2")).await.unwrap();
    assert_eq!(snapshot.status, PaymentStatus::Paid);
    assert_eq!(*calls.lock().unwrap(), 2);
}

#[tokio::test]
async fn client_errors_on_get_are_not_retried() {
    let server = MockServer::start(|_| Reply::json(404, error_json(404, "No payment"))).await;
    assert_eq!(
        client(&server).get_payment(&tr("tr_x1")).await,
        Err(PaymentError::NotFound)
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn timeouts_and_refused_connections() {
    let server = MockServer::start(|_| Reply::raw(200, "{}").delayed(Duration::from_secs(5))).await;
    let client = MollieClient::new(
        ApiKey::new(KEY).unwrap(),
        None,
        MollieOptions {
            timeout: Duration::from_millis(100),
            get_attempts: 1,
            ..options(&server)
        },
    )
    .unwrap();
    assert_eq!(
        client.get_payment(&tr("tr_t1")).await,
        Err(PaymentError::Unavailable("request timed out".into()))
    );

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let client = MollieClient::new(
        ApiKey::new(KEY).unwrap(),
        None,
        MollieOptions {
            base_url: format!("http://127.0.0.1:{port}/v2/"),
            get_attempts: 2,
            retry_delay: Duration::ZERO,
            ..MollieOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        client.get_payment(&tr("tr_t2")).await,
        Err(PaymentError::Unavailable("connection failed".into()))
    );
}

#[tokio::test]
async fn malformed_answers_are_invalid_responses() {
    let cases: Vec<(&str, Value)> = vec![
        ("unknown status", payment_json("tr_m1", "settled")),
        ("other id", payment_json("tr_other", "paid")),
        ("bad amount", {
            let mut p = payment_json("tr_m1", "paid");
            p["amount"]["value"] = json!("12.3");
            p
        }),
        ("currency", {
            let mut p = payment_json("tr_m1", "paid");
            p["amount"]["currency"] = json!("USD");
            p
        }),
        ("bad refunded", {
            let mut p = payment_json("tr_m1", "paid");
            p["amountRefunded"] = json!({ "currency": "EUR", "value": "1,00" });
            p
        }),
        ("missing field", json!({ "id": "tr_m1" })),
    ];
    for (what, body) in cases {
        let server = MockServer::start(move |_| Reply::json(200, body.clone())).await;
        let result = client(&server).get_payment(&tr("tr_m1")).await;
        assert!(
            matches!(result, Err(PaymentError::InvalidResponse(_))),
            "{what}: {result:?}"
        );
    }
    let server = MockServer::start(|_| Reply::raw(200, "not json")).await;
    assert!(matches!(
        client(&server).get_payment(&tr("tr_m1")).await,
        Err(PaymentError::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn create_needs_an_https_checkout_link_and_a_valid_id() {
    let cases: Vec<Value> = vec![
        {
            let mut p = payment_json("tr_c1", "open");
            p["_links"] = json!({});
            p
        },
        {
            let mut p = payment_json("tr_c1", "open");
            p["_links"]["checkout"]["href"] = json!("http://evil.test/checkout");
            p
        },
        {
            let mut p = payment_json("tr_c1", "open");
            p.as_object_mut().unwrap().remove("_links");
            p
        },
        payment_json("tr_c/../x", "open"),
    ];
    for body in cases {
        let server = MockServer::start(move |_| Reply::json(201, body.clone())).await;
        let result = client(&server).create_payment(&new_payment()).await;
        assert!(
            matches!(result, Err(PaymentError::InvalidResponse(_))),
            "{result:?}"
        );
    }
}

#[tokio::test]
async fn refund_request_shape_and_statuses() {
    for (raw, status) in [
        ("queued", RefundStatus::Queued),
        ("pending", RefundStatus::Pending),
        ("processing", RefundStatus::Processing),
        ("refunded", RefundStatus::Refunded),
        ("failed", RefundStatus::Failed),
        ("canceled", RefundStatus::Canceled),
    ] {
        let server = MockServer::start(move |_| {
            Reply::json(
                201,
                json!({
                    "resource": "refund",
                    "id": "re_4qqhO89gsT",
                    "amount": { "currency": "EUR", "value": "4.50" },
                    "status": raw,
                    "paymentId": "tr_p1",
                }),
            )
        })
        .await;
        let request = RefundRequest {
            payment: tr("tr_p1"),
            amount: Money::from_cents(450),
            description: "Ziek: bakdag gaat niet door".into(),
            idempotency_key: IdempotencyKey::new("refund-7-1").unwrap(),
        };
        let refund = client(&server).refund(&request).await.unwrap();
        assert_eq!(refund.id.as_str(), "re_4qqhO89gsT");
        assert_eq!(refund.status, status);
        let sent = &server.requests()[0];
        assert_eq!(sent.method, "POST");
        assert_eq!(sent.target, "/v2/payments/tr_p1/refunds");
        assert_eq!(sent.header("idempotency-key"), Some("refund-7-1"));
        assert_eq!(
            sent.json(),
            json!({
                "amount": { "currency": "EUR", "value": "4.50" },
                "description": "Ziek: bakdag gaat niet door",
            })
        );
    }
}

#[tokio::test]
async fn refund_with_odd_answers() {
    for body in [
        json!({ "id": "re_1", "status": "lost" }),
        json!({ "id": "re 1", "status": "queued" }),
    ] {
        let server = MockServer::start(move |_| Reply::json(201, body.clone())).await;
        let request = RefundRequest {
            payment: tr("tr_p1"),
            amount: Money::from_cents(1),
            description: "Terug".into(),
            idempotency_key: IdempotencyKey::new("refund-p1-1").unwrap(),
        };
        assert!(matches!(
            client(&server).refund(&request).await,
            Err(PaymentError::InvalidResponse(_))
        ));
    }
}

#[tokio::test]
async fn secrets_never_leak() {
    let server =
        MockServer::start(|_| Reply::json(401, error_json(401, "Missing authentication"))).await;
    let client = client_with_token(&server);
    let debug = format!("{client:?}");
    assert!(!debug.contains("dHar4"), "{debug}");
    assert!(!debug.contains("Wwvu7"), "{debug}");
    assert!(debug.contains("ApiKey(test_<redacted>)"));
    let err = client.get_payment(&tr("tr_l1")).await.unwrap_err();
    assert_eq!(err, PaymentError::Unauthorized);
    assert!(!format!("{err} {err:?}").contains("dHar4"));
}

fn transactions_page() -> Value {
    json!({
        "count": 4,
        "_embedded": { "balance_transactions": [
            {
                "resource": "balance-transaction",
                "id": "baltr_1",
                "type": "payment",
                "resultAmount": { "currency": "EUR", "value": "9.71" },
                "initialAmount": { "currency": "EUR", "value": "10.00" },
                "deductions": { "currency": "EUR", "value": "-0.29" },
                "deductionDetails": {
                    "fees": { "currency": "EUR", "value": "-0.20" },
                    "commissions": { "currency": "EUR", "value": "-0.09" }
                },
                "context": { "paymentId": "tr_5B8cwPMGnU", "paymentDescription": "Bestelling 7" },
                "createdAt": "2026-11-20T11:23:25+00:00"
            },
            {
                "resource": "balance-transaction",
                "id": "baltr_2",
                "type": "refund",
                "resultAmount": { "currency": "EUR", "value": "-4.75" },
                "initialAmount": { "currency": "EUR", "value": "-4.50" },
                "deductions": { "currency": "EUR", "value": "-0.25" },
                "deductionDetails": { "fees": { "currency": "EUR", "value": "-0.25" } },
                "context": { "paymentId": "tr_5B8cwPMGnU", "refundId": "re_1" }
            },
            {
                "resource": "balance-transaction",
                "id": "baltr_3",
                "type": "payment",
                "resultAmount": { "currency": "EUR", "value": "4.18" },
                "initialAmount": { "currency": "EUR", "value": "4.50" },
                "deductions": { "currency": "EUR", "value": "-0.32" },
                "context": { "paymentId": "tr_second1" }
            },
            {
                "resource": "balance-transaction",
                "id": "baltr_4",
                "type": "payment",
                "resultAmount": { "currency": "EUR", "value": "3.00" },
                "initialAmount": { "currency": "EUR", "value": "3.00" },
                "deductions": null,
                "context": { "paymentId": "tr_free1" }
            }
        ]},
        "_links": {
            "next": { "href": "https://api.mollie.com/v2/balances/primary/transactions?from=baltr_5&limit=250", "type": "application/hal+json" }
        }
    })
}

#[tokio::test]
async fn payment_fees_from_balance_transactions() {
    let server = MockServer::start(|_| Reply::json(200, transactions_page())).await;
    let client = client_with_token(&server);
    let page = client.list_payment_fees(None).await.unwrap();
    assert_eq!(
        page.fees,
        [
            PaymentFee {
                payment: tr("tr_5B8cwPMGnU"),
                kind: FeeKind::Payment,
                gross: Money::from_cents(1000),
                fee: Money::from_cents(29),
                net: Money::from_cents(971),
            },
            PaymentFee {
                payment: tr("tr_5B8cwPMGnU"),
                kind: FeeKind::Refund,
                gross: Money::from_cents(-450),
                fee: Money::from_cents(25),
                net: Money::from_cents(-475),
            },
            PaymentFee {
                payment: tr("tr_second1"),
                kind: FeeKind::Payment,
                gross: Money::from_cents(450),
                fee: Money::from_cents(32),
                net: Money::from_cents(418),
            },
            PaymentFee {
                payment: tr("tr_free1"),
                kind: FeeKind::Payment,
                gross: Money::from_cents(300),
                fee: Money::ZERO,
                net: Money::from_cents(300),
            },
        ]
    );
    assert_eq!(page.next, Some(FeeCursor::new("baltr_5").unwrap()));

    let next = client.list_payment_fees(page.next.as_ref()).await.unwrap();
    assert_eq!(next.fees.len(), 4);
    let requests = server.requests();
    assert_eq!(
        requests[0].target,
        "/v2/balances/primary/transactions?limit=250&testmode=true"
    );
    assert_eq!(
        requests[1].target,
        "/v2/balances/primary/transactions?limit=250&testmode=true&from=baltr_5"
    );
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );

    let live = MollieClient::new(
        ApiKey::new("live_abc").unwrap(),
        Some(OrganizationToken::new(TOKEN).unwrap()),
        options(&server),
    )
    .unwrap();
    live.list_payment_fees(None).await.unwrap();
    assert_eq!(
        server.requests()[2].target,
        "/v2/balances/primary/transactions?limit=250"
    );
}

#[tokio::test]
async fn last_fee_page_and_bad_transactions() {
    let server = MockServer::start(|_| {
        let mut page = transactions_page();
        page["_links"] = json!({ "self": { "href": "https://api.mollie.com/v2/x" } });
        Reply::json(200, page)
    })
    .await;
    let page = client_with_token(&server)
        .list_payment_fees(None)
        .await
        .unwrap();
    assert_eq!(page.next, None);

    let odd = json!({ "count": 4, "_embedded": { "balance_transactions": [
        { "id": "baltr_a", "type": "payment",
          "resultAmount": { "currency": "EUR", "value": "1.00" },
          "initialAmount": { "currency": "EUR", "value": "1.00" },
          "deductions": null },
        { "id": "baltr_b", "type": "payment",
          "resultAmount": { "currency": "EUR", "value": "1.10" },
          "initialAmount": { "currency": "EUR", "value": "1.00" },
          "deductions": { "currency": "EUR", "value": "0.10" },
          "context": { "paymentId": "tr_neg1" } },
        { "id": "baltr_c", "type": "payment",
          "resultAmount": { "currency": "EUR", "value": "1.0" },
          "initialAmount": { "currency": "EUR", "value": "1.00" },
          "deductions": null,
          "context": { "paymentId": "tr_bad1" } },
        { "id": "baltr_d", "type": "payment",
          "resultAmount": { "currency": "EUR", "value": "0.75" },
          "initialAmount": { "currency": "EUR", "value": "1.00" },
          "deductions": { "currency": "EUR", "value": "-0.25" },
          "deductionDetails": { "reservations": { "currency": "EUR", "value": "-0.25" } },
          "context": { "paymentId": "tr_ok1" } }
    ]}, "_links": {} });
    let server = MockServer::start(move |_| Reply::json(200, odd.clone())).await;
    let page = client_with_token(&server)
        .list_payment_fees(None)
        .await
        .unwrap();
    assert_eq!(
        page.fees,
        [PaymentFee {
            payment: tr("tr_ok1"),
            kind: FeeKind::Payment,
            gross: Money::from_cents(100),
            fee: Money::ZERO,
            net: Money::from_cents(75),
        }],
        "odd transactions are skipped; reservations are not fees"
    );

    let bad_pages = [
        json!({ "count": 1, "_embedded": { "balance_transactions": [{
            "type": "payment", "id_missing": true,
            "resultAmount": { "currency": "EUR", "value": "1.00" },
            "initialAmount": { "currency": "EUR", "value": "1.00" },
            "deductions": null
        }]}, "_links": {} }),
        json!({ "count": 0, "_embedded": { "balance_transactions": [] },
            "_links": { "next": { "href": "https://api.mollie.com/v2/x?limit=250" } } }),
    ];
    for body in bad_pages {
        let server = MockServer::start(move |_| Reply::json(200, body.clone())).await;
        assert!(matches!(
            client_with_token(&server).list_payment_fees(None).await,
            Err(PaymentError::InvalidResponse(_))
        ));
    }
}

#[tokio::test]
async fn fees_need_an_organization_token() {
    let server = MockServer::start(|_| Reply::json(200, transactions_page())).await;
    assert!(matches!(
        client(&server).list_payment_fees(None).await,
        Err(PaymentError::NotConfigured(_))
    ));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn from_config_reads_secret_files() {
    let dir = std::env::temp_dir().join(format!("bvw-mollie-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("key"), format!("{KEY}\n")).unwrap();
    std::fs::write(dir.join("token"), TOKEN).unwrap();
    let server = MockServer::start(|_| Reply::json(200, transactions_page())).await;
    let config = MollieConfig {
        api_key_file: dir.join("key"),
        organization_token_file: Some(dir.join("token")),
    };
    let client = MollieClient::from_config(&config, options(&server)).unwrap();
    assert!(client.list_payment_fees(None).await.is_ok());

    let missing = MollieConfig {
        api_key_file: dir.join("nope"),
        organization_token_file: None,
    };
    let err = MollieClient::from_config(&missing, options(&server)).unwrap_err();
    assert!(err.to_string().contains("cannot read Mollie API key"));
    let bad_url = MollieOptions {
        base_url: "http://api.mollie.com/v2/".into(),
        ..options(&server)
    };
    assert!(MollieClient::from_config(&config, bad_url).is_err());
}
