#![forbid(unsafe_code)]

mod allergen;
mod ids;
mod money;
mod pwyw;
mod totals;
mod vat;

pub use allergen::{Allergen, UnknownAllergen};
pub use ids::{
    BakeDayId, BusinessAccountId, CapacityPoolId, CharityId, CustomerId, DonationAllocationId,
    OrderId, OrderLineId, PaymentId, PickupLocationId, PickupSlotId, PickupWindowId, ProductId,
    StaffUserId,
};
pub use money::{Money, MoneyError};
pub use pwyw::{InvalidPriceRange, PayWhatYouWant, PriceRefused};
pub use totals::{Line, RateTotal, VatTotals};
pub use vat::VatRate;

pub const TIME_ZONE: &str = "Europe/Amsterdam";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bakery_time_zone_is_amsterdam() {
        assert_eq!(TIME_ZONE, "Europe/Amsterdam");
    }
}
