use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{PaymentError, PaymentStatus, ProviderPaymentId};
use orders::{Error as OrderError, PaidOutcome, PaymentReceived};

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
    let result = if payment.status == PaymentStatus::Paid {
        orders::mark_paid(
            &mut conn,
            clock,
            order,
            &PaymentReceived {
                reference: id.as_str().to_owned(),
                amount: payment.amount,
            },
        )
        .await
        .map(|outcome| match outcome {
            PaidOutcome::Paid { .. } => Settled::Paid,
            PaidOutcome::AlreadyPaid(_) => Settled::AlreadyPaid,
            PaidOutcome::RefundDue { reason, .. } => {
                tracing::warn!(
                    payment = %id,
                    order = order.get(),
                    reason = reason.as_str(),
                    "payment recorded, refund due"
                );
                Settled::RefundDue
            }
        })
    } else {
        orders::record_payment_status(
            &mut conn,
            clock,
            order,
            id.as_str(),
            payment.amount,
            payment.status,
        )
        .await
        .map(|_| Settled::Status(payment.status))
    };
    match result {
        Ok(settled) => Ok(settled),
        Err(OrderError::NotFound) => {
            tracing::warn!(payment = %id, order = order.get(), "payment for an unknown order");
            Ok(Settled::Ignored)
        }
        Err(OrderError::PaymentOfOtherOrder) => {
            tracing::error!(payment = %id, order = order.get(), "payment recorded for another order");
            Ok(Settled::Ignored)
        }
        Err(err) => Err(TryAgain(err.to_string())),
    }
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
