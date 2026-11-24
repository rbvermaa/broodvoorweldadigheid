macro_rules! id_type {
    ($($(#[$doc:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(i64);

        impl $name {
            #[must_use]
            pub const fn new(id: i64) -> Self {
                Self(id)
            }

            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }
        }
    )+};
}

id_type!(
    StaffUserId,
    CustomerId,
    BusinessAccountId,
    PickupLocationId,
    PickupWindowId,
    ProductId,
    BakeDayId,
    PickupSlotId,
    CapacityPoolId,
    OrderId,
    OrderLineId,
    PaymentId,
    CharityId,
    DonationAllocationId,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_wrap_an_i64() {
        let id = ProductId::new(42);
        assert_eq!(id.get(), 42);
        assert!(OrderId::new(1) < OrderId::new(2));
    }
}
