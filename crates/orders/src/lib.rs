//! Cart, checkout with capacity holds, payment status and the audit log (F3, F4, F4b,
//! F4c), the donation choice and saved donations (F4d), and ticking orders off at pickup
//! (F8). See ADRs 0012 and 0030.
#![forbid(unsafe_code)]

pub mod audit;
pub mod cart;
mod checkout;
mod customer;
mod details;
pub mod donations;
mod error;
mod holds;
pub mod manage;
pub mod order;
mod payment;
mod pickup;
mod refunds;
mod source;

pub use cart::{Cart, CartError, CartLine, PricedLine};
pub use checkout::{HOLD, OrderedBy, checkout, checkout_replacing, same_as_order, slot_orders};
pub use customer::CustomerInput;
pub use details::{OrderDetails, PickupDetails, awaiting_pickup, details, lock_awaiting_pickup};
pub use error::Error;
pub use holds::{cancel_pending, release_expired_holds};
pub use order::{CancelReason, Channel, Order, OrderLine, OrderNumber, OrderStatus, PaymentMode};
pub use payment::{
    DonationAllocation, PaidOutcome, Payment, PaymentReceived, RefundReason, allocations,
    latest_payment_status, mark_paid, payment_started, record_payment_status, refunds_due,
};
pub use pickup::{mark_picked_up, undo_picked_up};
pub use refunds::{
    Cancelled, DayCancelled, DayPreview, Refund, RefundState, cancel_day, cancel_paid, day_preview,
    issue, of_order, open_of_day,
};
pub use source::Orders;
