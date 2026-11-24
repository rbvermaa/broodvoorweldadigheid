//! What we rely on from Mollie. Runs nightly against Mollie test mode and in every
//! test run against the mock in `mollie_client.rs`.

use domain::{
    IdempotencyKey, Money, NewPayment, OrderId, PaymentError, PaymentMethod, PaymentProvider,
    PaymentStatus, ProviderPaymentId, RefundRequest,
};

pub async fn run(provider: &dyn PaymentProvider, run_id: &str) {
    let key = IdempotencyKey::new(format!("contract-{run_id}")).unwrap();
    let request = NewPayment {
        amount: Money::from_cents(123),
        description: format!("Contracttest {run_id}"),
        redirect_url: "https://example.org/bestelling/424242".into(),
        webhook_url: "https://example.org/webhooks/mollie".into(),
        order: OrderId::new(424_242),
        methods: vec![PaymentMethod::Ideal],
        idempotency_key: Some(key),
    };
    let created = provider.create_payment(&request).await.unwrap();
    assert!(created.id.as_str().starts_with("tr_"), "{}", created.id);
    assert!(created.checkout_url.starts_with("https://"));

    let replayed = provider.create_payment(&request).await.unwrap();
    assert_eq!(
        replayed.id, created.id,
        "Idempotency-Key replays the create"
    );

    let snapshot = provider.get_payment(&created.id).await.unwrap();
    assert_eq!(snapshot.id, created.id);
    assert_eq!(snapshot.status, PaymentStatus::Open);
    assert_eq!(snapshot.amount, Money::from_cents(123));
    assert_eq!(snapshot.amount_refunded, Money::ZERO);
    assert_eq!(snapshot.order, Some(OrderId::new(424_242)));
    assert!(
        matches!(snapshot.method.as_deref(), None | Some("ideal")),
        "{:?}",
        snapshot.method
    );

    let refund = RefundRequest {
        payment: created.id.clone(),
        amount: Money::from_cents(100),
        description: "Contracttest".into(),
        idempotency_key: None,
    };
    let refused = provider.refund(&refund).await;
    assert!(
        matches!(refused, Err(PaymentError::Rejected { .. })),
        "{refused:?}"
    );

    let unknown = ProviderPaymentId::new("tr_bvwContractUnknown1").unwrap();
    assert_eq!(
        provider.get_payment(&unknown).await,
        Err(PaymentError::NotFound)
    );
}
