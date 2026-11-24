//! Cart, checkout with capacity holds, payment status and the audit log (F3, F4, F4b,
//! F4c). See ADR 0012.
#![forbid(unsafe_code)]

pub mod audit;
pub mod cart;
mod checkout;
mod customer;
mod details;
mod error;
mod holds;
pub mod order;
mod payment;
mod source;

pub use cart::{Cart, CartError, CartLine, PricedLine};
pub use checkout::{HOLD, OrderedBy, checkout, slot_orders};
pub use customer::CustomerInput;
pub use details::{OrderDetails, PickupDetails, awaiting_pickup, details, lock_awaiting_pickup};
pub use error::Error;
pub use holds::release_expired_holds;
pub use order::{CancelReason, Channel, Order, OrderLine, OrderNumber, OrderStatus, PaymentMode};
pub use payment::{
    DonationAllocation, PaidOutcome, Payment, PaymentReceived, RefundReason, allocations,
    latest_payment_status, mark_paid, payment_started, record_payment_status, refunds_due,
};
pub use source::Orders;
