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

// One line per id, sorted by name (ADR 0044).
id_type!(
    BakeDayId,
    BatchId,
    BatchLineId,
    BusinessAccountId,
    CharityId,
    CustomerId,
    DonationAllocationId,
    IngredientId,
    OrderId,
    OrderLineId,
    PaymentId,
    PickupLocationId,
    PickupSlotId,
    PickupWindowId,
    PollId,
    PollOptionId,
    ProductId,
    RecipeId,
    RecipeVersionId,
    RefundId,
    StaffUserId,
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
