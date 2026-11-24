#![forbid(unsafe_code)]

mod allergen;
mod clock;
mod ids;
mod money;
mod pwyw;
mod totals;
mod vat;

pub use allergen::{Allergen, UnknownAllergen};
pub use clock::{Clock, bakery_time_zone, in_bakery_time};
pub use ids::{
    BakeDayId, BusinessAccountId, CapacityPoolId, CharityId, CustomerId, DonationAllocationId,
    OrderId, OrderLineId, PaymentId, PickupLocationId, PickupSlotId, PickupWindowId, ProductId,
    StaffUserId,
};
pub use money::{Money, MoneyError};
pub use pwyw::{InvalidPriceRange, PayWhatYouWant, PriceRefused};
pub use totals::{Line, RateTotal, VatTotals};
pub use vat::{UnknownVatRate, VatRate};

pub const TIME_ZONE: &str = "Europe/Amsterdam";
