use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use domain::{
    CreatedPayment, CreatedRefund, FeeCursor, FeePage, IdempotencyKey, Money, NewPayment,
    PaymentError, PaymentFee, PaymentProvider, PaymentSnapshot, PaymentStatus, ProviderFuture,
    ProviderPaymentId, ProviderRefundId, RefundRequest, RefundStatus,
};

/// An in-memory payment provider with deterministic ids (`tr_fake1`, `re_fake1`, …).
///
/// Tests move payments with [`Self::pay`], [`Self::fail`] and friends; every change
/// queues a webhook notification, read with [`Self::take_notifications`].
#[derive(Debug, Default)]
pub struct FakePaymentProvider {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    down: bool,
    next_payment: u64,
    next_refund: u64,
    payments: BTreeMap<ProviderPaymentId, FakePayment>,
    replays: BTreeMap<IdempotencyKey, Replay>,
    notifications: Vec<ProviderPaymentId>,
    created: Vec<NewPayment>,
}

#[derive(Debug, Clone)]
enum Replay {
    Payment(CreatedPayment),
    Refund(CreatedRefund),
}

#[derive(Debug, Clone)]
struct FakePayment {
    seq: u64,
    request: NewPayment,
    status: PaymentStatus,
    method: Option<String>,
    refunds: Vec<(ProviderRefundId, Money)>,
    fee: Option<Money>,
}

impl FakePayment {
    fn refunded(&self) -> Money {
        Money::from_cents(self.refunds.iter().map(|(_, amount)| amount.cents()).sum())
    }
}

impl FakePaymentProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// While down, every call fails with [`PaymentError::Unavailable`].
    pub fn set_down(&self, down: bool) {
        self.state().down = down;
    }

    /// # Panics
    /// When the payment does not exist.
    pub fn set_status(&self, id: &ProviderPaymentId, status: PaymentStatus) {
        let mut state = self.state();
        let payment = state.payments.get_mut(id).expect("unknown fake payment");
        payment.status = status;
        if status == PaymentStatus::Paid && payment.method.is_none() {
            payment.method = Some(
                payment
                    .request
                    .methods
                    .first()
                    .map_or("ideal", |m| m.as_str())
                    .to_string(),
            );
        }
        state.notifications.push(id.clone());
    }

    pub fn pay(&self, id: &ProviderPaymentId) {
        self.set_status(id, PaymentStatus::Paid);
    }

    pub fn fail(&self, id: &ProviderPaymentId) {
        self.set_status(id, PaymentStatus::Failed);
    }

    pub fn expire(&self, id: &ProviderPaymentId) {
        self.set_status(id, PaymentStatus::Expired);
    }

    pub fn cancel(&self, id: &ProviderPaymentId) {
        self.set_status(id, PaymentStatus::Canceled);
    }

    /// Queues another notification without changing the payment, as Mollie does
    /// when it retries a webhook.
    pub fn notify_again(&self, id: &ProviderPaymentId) {
        self.state().notifications.push(id.clone());
    }

    /// The webhook notifications queued since the last call, oldest first.
    pub fn take_notifications(&self) -> Vec<ProviderPaymentId> {
        std::mem::take(&mut self.state().notifications)
    }

    /// # Panics
    /// When the payment does not exist.
    pub fn set_fee(&self, id: &ProviderPaymentId, fee: Money) {
        self.state()
            .payments
            .get_mut(id)
            .expect("unknown fake payment")
            .fee = Some(fee);
    }

    /// Every create request that reached the provider, in order.
    pub fn created(&self) -> Vec<NewPayment> {
        self.state().created.clone()
    }

    fn create(&self, request: &NewPayment) -> Result<CreatedPayment, PaymentError> {
        let mut state = self.state();
        check_up(&state)?;
        if let Some(key) = &request.idempotency_key
            && let Some(Replay::Payment(created)) = state.replays.get(key)
        {
            return Ok(created.clone());
        }
        if request.amount <= Money::ZERO {
            return Err(rejected("amount", "The amount must be positive"));
        }
        state.next_payment += 1;
        let seq = state.next_payment;
        let id = ProviderPaymentId::new(format!("tr_fake{}", state.next_payment))
            .expect("fake ids are valid tokens");
        let created = CreatedPayment {
            checkout_url: format!("https://checkout.fake-mollie.test/{id}"),
            id: id.clone(),
        };
        state.created.push(request.clone());
        state.payments.insert(
            id,
            FakePayment {
                seq,
                request: request.clone(),
                status: PaymentStatus::Open,
                method: None,
                refunds: Vec::new(),
                fee: None,
            },
        );
        if let Some(key) = &request.idempotency_key {
            state
                .replays
                .insert(key.clone(), Replay::Payment(created.clone()));
        }
        Ok(created)
    }

    fn get(&self, id: &ProviderPaymentId) -> Result<PaymentSnapshot, PaymentError> {
        let state = self.state();
        check_up(&state)?;
        let payment = state.payments.get(id).ok_or(PaymentError::NotFound)?;
        Ok(PaymentSnapshot {
            id: id.clone(),
            status: payment.status,
            amount: payment.request.amount,
            amount_refunded: payment.refunded(),
            method: payment.method.clone(),
            order: Some(payment.request.order),
        })
    }

    fn do_refund(&self, request: &RefundRequest) -> Result<CreatedRefund, PaymentError> {
        let mut state = self.state();
        check_up(&state)?;
        if let Some(key) = &request.idempotency_key
            && let Some(Replay::Refund(created)) = state.replays.get(key)
        {
            return Ok(created.clone());
        }
        let refund_id = ProviderRefundId::new(format!("re_fake{}", state.next_refund + 1))
            .expect("fake ids are valid tokens");
        let payment = state
            .payments
            .get_mut(&request.payment)
            .ok_or(PaymentError::NotFound)?;
        if payment.status != PaymentStatus::Paid {
            return Err(rejected("", "The payment is not paid"));
        }
        let remaining = payment.request.amount.cents() - payment.refunded().cents();
        if request.amount <= Money::ZERO || request.amount.cents() > remaining {
            return Err(rejected(
                "amount",
                "The amount is higher than the amount that can be refunded",
            ));
        }
        payment.refunds.push((refund_id.clone(), request.amount));
        state.next_refund += 1;
        let created = CreatedRefund {
            id: refund_id,
            status: RefundStatus::Pending,
        };
        if let Some(key) = &request.idempotency_key {
            state
                .replays
                .insert(key.clone(), Replay::Refund(created.clone()));
        }
        Ok(created)
    }

    fn fees(&self) -> Result<FeePage, PaymentError> {
        let state = self.state();
        check_up(&state)?;
        let mut paid: Vec<_> = state.payments.iter().collect();
        paid.sort_by_key(|(_, payment)| std::cmp::Reverse(payment.seq));
        let fees = paid
            .into_iter()
            .filter_map(|(id, payment)| {
                let fee = payment.fee?;
                Some(PaymentFee {
                    payment: id.clone(),
                    gross: payment.request.amount,
                    fee,
                    net: Money::from_cents(payment.request.amount.cents() - fee.cents()),
                })
            })
            .collect();
        Ok(FeePage { fees, next: None })
    }
}

fn check_up(state: &State) -> Result<(), PaymentError> {
    if state.down {
        Err(PaymentError::Unavailable("fake provider is down".into()))
    } else {
        Ok(())
    }
}

fn rejected(field: &str, detail: &str) -> PaymentError {
    PaymentError::Rejected {
        field: (!field.is_empty()).then(|| field.to_string()),
        detail: detail.to_string(),
    }
}

impl PaymentProvider for FakePaymentProvider {
    fn create_payment<'a>(&'a self, request: &'a NewPayment) -> ProviderFuture<'a, CreatedPayment> {
        let result = self.create(request);
        Box::pin(async move { result })
    }

    fn get_payment<'a>(&'a self, id: &'a ProviderPaymentId) -> ProviderFuture<'a, PaymentSnapshot> {
        let result = self.get(id);
        Box::pin(async move { result })
    }

    fn refund<'a>(&'a self, request: &'a RefundRequest) -> ProviderFuture<'a, CreatedRefund> {
        let result = self.do_refund(request);
        Box::pin(async move { result })
    }

    fn list_payment_fees<'a>(
        &'a self,
        _cursor: Option<&'a FeeCursor>,
    ) -> ProviderFuture<'a, FeePage> {
        let result = self.fees();
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

    use domain::{OrderId, PaymentMethod};

    use super::*;

    fn ready<T>(future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("fake provider futures are always ready"),
        }
    }

    fn new_payment(cents: i64) -> NewPayment {
        NewPayment {
            amount: Money::from_cents(cents),
            description: "Bestelling 7".into(),
            redirect_url: "https://shop.test/bestelling/7".into(),
            webhook_url: "https://shop.test/webhooks/mollie".into(),
            order: OrderId::new(7),
            methods: vec![PaymentMethod::Ideal, PaymentMethod::Wero],
            idempotency_key: None,
        }
    }

    fn refund(id: &ProviderPaymentId, cents: i64) -> RefundRequest {
        RefundRequest {
            payment: id.clone(),
            amount: Money::from_cents(cents),
            description: "Ziek: bakdag gaat niet door".into(),
            idempotency_key: None,
        }
    }

    fn create(fake: &FakePaymentProvider, cents: i64) -> ProviderPaymentId {
        ready(fake.create_payment(&new_payment(cents))).unwrap().id
    }

    #[test]
    fn works_as_a_trait_object() {
        let provider: Arc<dyn PaymentProvider> = Arc::new(FakePaymentProvider::new());
        let created = ready(provider.create_payment(&new_payment(450))).unwrap();
        assert_eq!(created.id.as_str(), "tr_fake1");
        assert_eq!(
            created.checkout_url,
            "https://checkout.fake-mollie.test/tr_fake1"
        );
        let snapshot = ready(provider.get_payment(&created.id)).unwrap();
        assert_eq!(snapshot.status, PaymentStatus::Open);
        assert_eq!(snapshot.amount, Money::from_cents(450));
        assert_eq!(snapshot.amount_refunded, Money::ZERO);
        assert_eq!(snapshot.method, None);
        assert_eq!(snapshot.order, Some(OrderId::new(7)));
    }

    #[test]
    fn paid() {
        let fake = FakePaymentProvider::new();
        let id = create(&fake, 450);
        fake.pay(&id);
        assert_eq!(fake.take_notifications(), std::slice::from_ref(&id));
        let snapshot = ready(fake.get_payment(&id)).unwrap();
        assert_eq!(snapshot.status, PaymentStatus::Paid);
        assert_eq!(snapshot.method.as_deref(), Some("ideal"));
        assert!(fake.take_notifications().is_empty());
    }

    #[test]
    fn failed_expired_and_canceled() {
        let fake = FakePaymentProvider::new();
        let cases = [
            (
                PaymentStatus::Failed,
                FakePaymentProvider::fail as fn(&_, &_),
            ),
            (PaymentStatus::Expired, FakePaymentProvider::expire),
            (PaymentStatus::Canceled, FakePaymentProvider::cancel),
        ];
        for (status, apply) in cases {
            let id = create(&fake, 450);
            apply(&fake, &id);
            assert_eq!(ready(fake.get_payment(&id)).unwrap().status, status);
            assert_eq!(
                ready(fake.refund(&refund(&id, 450))),
                Err(rejected("", "The payment is not paid"))
            );
        }
        assert_eq!(fake.take_notifications().len(), 3);
    }

    #[test]
    fn full_and_partial_refunds() {
        let fake = FakePaymentProvider::new();
        let id = create(&fake, 1000);
        fake.pay(&id);
        let first = ready(fake.refund(&refund(&id, 300))).unwrap();
        assert_eq!(first.id.as_str(), "re_fake1");
        assert_eq!(first.status, RefundStatus::Pending);
        assert_eq!(
            ready(fake.get_payment(&id)).unwrap().amount_refunded,
            Money::from_cents(300)
        );
        assert!(matches!(
            ready(fake.refund(&refund(&id, 701))),
            Err(PaymentError::Rejected { .. })
        ));
        assert!(matches!(
            ready(fake.refund(&refund(&id, 0))),
            Err(PaymentError::Rejected { .. })
        ));
        let second = ready(fake.refund(&refund(&id, 700))).unwrap();
        assert_eq!(second.id.as_str(), "re_fake2");
        assert_eq!(
            ready(fake.get_payment(&id)).unwrap().amount_refunded,
            Money::from_cents(1000)
        );
    }

    #[test]
    fn duplicate_notifications() {
        let fake = FakePaymentProvider::new();
        let id = create(&fake, 450);
        fake.pay(&id);
        fake.notify_again(&id);
        fake.notify_again(&id);
        assert_eq!(fake.take_notifications(), [id.clone(), id.clone(), id]);
    }

    #[test]
    fn idempotency_keys_replay() {
        let fake = FakePaymentProvider::new();
        let key = IdempotencyKey::new("order-7-attempt-1").unwrap();
        let request = NewPayment {
            idempotency_key: Some(key),
            ..new_payment(450)
        };
        let first = ready(fake.create_payment(&request)).unwrap();
        let again = ready(fake.create_payment(&request)).unwrap();
        assert_eq!(first, again);
        assert_eq!(fake.created().len(), 1);

        fake.pay(&first.id);
        let refund_request = RefundRequest {
            idempotency_key: Some(IdempotencyKey::new("refund-7-1").unwrap()),
            ..refund(&first.id, 450)
        };
        let refunded = ready(fake.refund(&refund_request)).unwrap();
        assert_eq!(ready(fake.refund(&refund_request)).unwrap(), refunded);
        assert_eq!(
            ready(fake.get_payment(&first.id)).unwrap().amount_refunded,
            Money::from_cents(450)
        );
    }

    #[test]
    fn mollie_down() {
        let fake = FakePaymentProvider::new();
        let id = create(&fake, 450);
        fake.set_down(true);
        let down = Err(PaymentError::Unavailable("fake provider is down".into()));
        assert_eq!(
            ready(fake.create_payment(&new_payment(450))).map(|_| ()),
            down
        );
        assert_eq!(ready(fake.get_payment(&id)).map(|_| ()), down);
        assert_eq!(ready(fake.refund(&refund(&id, 1))).map(|_| ()), down);
        assert_eq!(ready(fake.list_payment_fees(None)).map(|_| ()), down);
        fake.set_down(false);
        assert!(ready(fake.get_payment(&id)).is_ok());
    }

    #[test]
    fn unknown_payment_and_bad_amount() {
        let fake = FakePaymentProvider::new();
        let unknown = ProviderPaymentId::new("tr_nope").unwrap();
        assert_eq!(
            ready(fake.get_payment(&unknown)),
            Err(PaymentError::NotFound)
        );
        assert_eq!(
            ready(fake.refund(&refund(&unknown, 1))),
            Err(PaymentError::NotFound)
        );
        assert!(matches!(
            ready(fake.create_payment(&new_payment(0))),
            Err(PaymentError::Rejected { .. })
        ));
    }

    #[test]
    fn fees_per_payment() {
        let fake = FakePaymentProvider::new();
        let a = create(&fake, 1000);
        let b = create(&fake, 450);
        let _no_fee = create(&fake, 300);
        fake.set_fee(&a, Money::from_cents(29));
        fake.set_fee(&b, Money::from_cents(32));
        let page = ready(fake.list_payment_fees(None)).unwrap();
        assert_eq!(page.next, None);
        assert_eq!(
            page.fees,
            [
                PaymentFee {
                    payment: b,
                    gross: Money::from_cents(450),
                    fee: Money::from_cents(32),
                    net: Money::from_cents(418),
                },
                PaymentFee {
                    payment: a,
                    gross: Money::from_cents(1000),
                    fee: Money::from_cents(29),
                    net: Money::from_cents(971),
                },
            ]
        );
    }
}
