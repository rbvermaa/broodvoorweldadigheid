use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{OrderId, PaymentError, PaymentStatus, ProviderPaymentId};
use jobs::order_emails;
use orders::{Error as OrderError, OrderStatus, PaidOutcome, PaymentReceived, RefundReason};
use sqlx::{Acquire, PgConnection};

use crate::state::AppState;

/// What a payment report led to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    Paid,
    AlreadyPaid,
    /// Recorded, but the money must go back (ADR 0012).
    RefundDue,
    /// A status other than paid was recorded or was already known.
    Status(PaymentStatus),
    /// Nothing to do: the provider or our database does not know the payment's order.
    Ignored,
}

/// Mollie should try again later.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TryAgain(String);

/// Fetches the payment from the provider and records what it says (F5). Never acts on
/// anything but the provider's own answer. Safe to repeat: a known report changes nothing.
///
/// # Errors
/// [`TryAgain`] when the provider or the database is unavailable.
pub async fn settle(state: &AppState, id: &ProviderPaymentId) -> Result<Settled, TryAgain> {
    let payment = match state.payments.get_payment(id).await {
        Ok(payment) => payment,
        Err(PaymentError::NotFound) => {
            tracing::warn!(payment = %id, "webhook for a payment the provider does not know");
            return Ok(Settled::Ignored);
        }
        Err(err) => return Err(TryAgain(err.to_string())),
    };
    let Some(order) = payment.order else {
        tracing::warn!(payment = %id, "payment without an order id in its metadata");
        return Ok(Settled::Ignored);
    };
    let mut conn = state
        .pool
        .acquire()
        .await
        .map_err(|err| TryAgain(err.to_string()))?;
    let clock = state.clock.as_ref();
    if payment.status == PaymentStatus::Paid {
        let received = PaymentReceived {
            reference: id.as_str().to_owned(),
            amount: payment.amount,
        };
        return paid(state, &mut conn, id, order, &received).await;
    }
    match orders::record_payment_status(
        &mut conn,
        clock,
        order,
        id.as_str(),
        payment.amount,
        payment.status,
    )
    .await
    {
        Ok(_) => Ok(Settled::Status(payment.status)),
        Err(err) => order_failure(err, id, order),
    }
}

fn order_failure(
    err: OrderError,
    id: &ProviderPaymentId,
    order: OrderId,
) -> Result<Settled, TryAgain> {
    match err {
        OrderError::NotFound => {
            tracing::warn!(payment = %id, order = order.get(), "payment for an unknown order");
            Ok(Settled::Ignored)
        }
        OrderError::PaymentOfOtherOrder => {
            tracing::error!(payment = %id, order = order.get(), "payment recorded for another order");
            Ok(Settled::Ignored)
        }
        err => Err(TryAgain(err.to_string())),
    }
}

/// Marks the order paid and queues its confirmation in one transaction (ADR 0022), or,
/// for a payment that came in for a cancelled order, the cancellation with its refund.
/// An email that cannot be composed is logged; the payment is recorded anyway.
async fn paid(
    state: &AppState,
    conn: &mut PgConnection,
    id: &ProviderPaymentId,
    order: OrderId,
    received: &PaymentReceived,
) -> Result<Settled, TryAgain> {
    let clock = state.clock.as_ref();
    let emails = state.order_emails.as_ref();
    let mut tx = conn
        .begin()
        .await
        .map_err(|err| TryAgain(err.to_string()))?;
    let outcome = match orders::mark_paid(&mut tx, clock, order, received).await {
        Ok(outcome) => outcome,
        Err(err) => return order_failure(err, id, order),
    };
    let (settled, email) = match outcome {
        PaidOutcome::Paid { .. } => (
            Settled::Paid,
            Some(order_emails::enqueue_confirmation(&mut tx, clock, emails, order).await),
        ),
        PaidOutcome::AlreadyPaid(_) => (Settled::AlreadyPaid, None),
        PaidOutcome::RefundDue {
            order: refused,
            payment,
            reason,
        } => {
            tracing::warn!(
                payment = %id,
                order = order.get(),
                reason = reason.as_str(),
                "payment recorded, refund due"
            );
            let cancelled = refused.status == OrderStatus::Cancelled
                && matches!(
                    reason,
                    RefundReason::HoldExpired | RefundReason::OrderCancelled
                );
            let email = if cancelled {
                Some(order_emails::enqueue_refund_notice(&mut tx, clock, emails, &payment).await)
            } else {
                None
            };
            (Settled::RefundDue, email)
        }
    };
    if let Some(Err(error)) = email {
        if error.is_transient() {
            return Err(TryAgain(error.to_string()));
        }
        tracing::error!(payment = %id, order = order.get(), %error, "payment recorded without its email");
    }
    tx.commit().await.map_err(|err| TryAgain(err.to_string()))?;
    Ok(settled)
}

/// `POST /webhooks/mollie`. 400 for a body without a valid payment id; 503 when Mollie
/// or the database cannot be reached, so Mollie retries; 200 otherwise.
pub async fn mollie(State(state): State<AppState>, body: Bytes) -> Response {
    let id = match payments::parse_webhook_body(&body) {
        Ok(id) => id,
        Err(err) => {
            tracing::warn!(error = %err, "malformed Mollie webhook");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    match settle(&state, &id).await {
        Ok(settled) => {
            tracing::info!(payment = %id, ?settled, "Mollie webhook");
            StatusCode::OK.into_response()
        }
        Err(err) => {
            tracing::warn!(payment = %id, error = %err, "Mollie webhook, try again later");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}
