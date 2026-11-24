use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use domain::{PaymentStatus, ProviderPaymentId};
use i18n::FluentArgs;
use orders::{CancelReason, Order, OrderStatus};
use sqlx::PgConnection;

use super::{drop_path, server_error};
use crate::pages::{self, Layout};
use crate::state::AppState;

/// What the customer sees after Mollie sends them back (F5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Waiting,
    Paid,
    Failed,
    Expired,
    Cancelled,
}

impl Shown {
    const fn text_id(self) -> &'static str {
        match self {
            Self::Waiting => "order-status-waiting",
            Self::Paid => "order-status-paid",
            Self::Failed => "order-status-failed",
            Self::Expired => "order-status-expired",
            Self::Cancelled => "order-status-cancelled",
        }
    }

    const fn alert(self) -> &'static str {
        match self {
            Self::Waiting => "info",
            Self::Paid => "success",
            Self::Failed | Self::Expired => "error",
            Self::Cancelled => "warning",
        }
    }
}

#[derive(Template)]
#[template(path = "shop/order.html")]
struct OrderPage<'a> {
    layout: Layout<'a>,
    status: StatusView,
}

#[derive(Template)]
#[template(source = "{% include \"shop/order_status.html\" %}", ext = "html")]
struct StatusFragment<'a> {
    layout: Layout<'a>,
    status: StatusView,
}

struct StatusView {
    /// Where htmx polls while waiting; empty once the status is final.
    poll: String,
    number: String,
    kind: &'static str,
    text: String,
    thanks: String,
    total_cents: i64,
    again: String,
}

async fn shown(
    conn: &mut PgConnection,
    state: &AppState,
    order: &Order,
) -> Result<(Shown, Option<(String, PaymentStatus)>), orders::Error> {
    let latest = orders::latest_payment_status(conn, order.id).await?;
    let shown = match order.status {
        OrderStatus::Paid | OrderStatus::Confirmed | OrderStatus::PickedUp => Shown::Paid,
        OrderStatus::Cancelled | OrderStatus::Refunded => {
            if order.cancel_reason == Some(CancelReason::HoldExpired) {
                Shown::Expired
            } else {
                Shown::Cancelled
            }
        }
        OrderStatus::PendingPayment => match latest.as_ref().map(|(_, status)| *status) {
            Some(PaymentStatus::Failed | PaymentStatus::Canceled) => Shown::Failed,
            Some(PaymentStatus::Expired) => Shown::Expired,
            _ if order
                .hold_expires_at
                .is_none_or(|hold| state.clock.now() >= hold) =>
            {
                Shown::Expired
            }
            _ => Shown::Waiting,
        },
    };
    Ok((shown, latest))
}

/// The order's status page after paying, at `/bestelling/<status token>`. htmx polls it
/// while the payment is open. The first full load also asks Mollie, in case its webhook
/// has not arrived (yet).
pub async fn page(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    let polled = headers.contains_key("hx-request");
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return server_error(&state, "no database connection", &err),
    };
    let mut order = match orders::order::by_status_token(&mut conn, &token).await {
        Ok(order) => order,
        Err(orders::Error::NotFound) => return pages::error(&state, StatusCode::NOT_FOUND),
        Err(err) => return server_error(&state, "cannot load the order", &err),
    };
    let (mut status, latest) = match shown(&mut conn, &state, &order).await {
        Ok(shown) => shown,
        Err(err) => return server_error(&state, "cannot load the payment", &err),
    };
    if !polled
        && status == Shown::Waiting
        && let Some((reference, _)) = latest
        && let Ok(id) = ProviderPaymentId::new(reference)
    {
        drop(conn);
        if let Err(err) = super::settle(&state, &id).await {
            tracing::warn!(error = %err, "cannot ask Mollie about the payment");
        }
        conn = match state.pool.acquire().await {
            Ok(conn) => conn,
            Err(err) => return server_error(&state, "no database connection", &err),
        };
        order = match orders::order::get(&mut conn, order.id).await {
            Ok(order) => order,
            Err(err) => return server_error(&state, "cannot load the order", &err),
        };
        status = match shown(&mut conn, &state, &order).await {
            Ok((status, _)) => status,
            Err(err) => return server_error(&state, "cannot load the payment", &err),
        };
    }
    let view = match status_view(&state, &order, status, &token) {
        Ok(view) => view,
        Err(err) => return server_error(&state, "cannot show the order", &err),
    };
    let layout = Layout::new(&state);
    if polled {
        pages::render(
            &state,
            StatusCode::OK,
            &StatusFragment {
                layout,
                status: view,
            },
        )
    } else {
        pages::render(
            &state,
            StatusCode::OK,
            &OrderPage {
                layout,
                status: view,
            },
        )
    }
}

fn status_view(
    state: &AppState,
    order: &Order,
    shown: Shown,
    token: &str,
) -> Result<StatusView, i18n::I18nError> {
    let t = &state.i18n;
    let mut number = FluentArgs::new();
    number.set("number", order.number.to_string());
    let thanks = if shown == Shown::Paid {
        let mut args = FluentArgs::new();
        args.set("name", order.customer_name.clone());
        t.t("order-thanks", Some(&args))?
    } else {
        String::new()
    };
    Ok(StatusView {
        poll: if shown == Shown::Waiting {
            format!("/bestelling/{token}")
        } else {
            String::new()
        },
        number: t.t("order-number", Some(&number))?,
        kind: shown.alert(),
        text: t.t(shown.text_id(), None)?,
        thanks,
        total_cents: order.total.cents(),
        again: if matches!(shown, Shown::Failed | Shown::Expired) {
            drop_path(order.bake_day_id)
        } else {
            String::new()
        },
    })
}
