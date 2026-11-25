use std::collections::BTreeMap;

use domain::ProductId;

/// A product's limits on one bake day (F3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductLimit {
    pub product_id: ProductId,
    pub max_qty: Option<u32>,
}

/// Pieces of a product already taken on a bake day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Used {
    pub ordered: u32,
    pub extras: u32,
}

impl Used {
    fn total(self) -> u64 {
        u64::from(self.ordered) + u64::from(self.extras)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remaining {
    Unlimited,
    Pieces(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("only {remaining} left of product {product_id:?}")]
pub struct Exceeded {
    pub product_id: ProductId,
    pub remaining: u32,
}

/// Limits and usage of one bake day.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capacity {
    products: BTreeMap<ProductId, ProductLimit>,
    used: BTreeMap<ProductId, Used>,
}

impl Capacity {
    #[must_use]
    pub fn new(
        products: impl IntoIterator<Item = ProductLimit>,
        used: BTreeMap<ProductId, Used>,
    ) -> Self {
        Self {
            products: products.into_iter().map(|p| (p.product_id, p)).collect(),
            used,
        }
    }

    fn used(&self, product_id: ProductId) -> Used {
        self.used.get(&product_id).copied().unwrap_or_default()
    }

    /// `max_qty - used`, never below zero. A product that is not on the bake day, or has
    /// no maximum, is unlimited.
    #[must_use]
    pub fn remaining(&self, product_id: ProductId) -> Remaining {
        let Some(max) = self.products.get(&product_id).and_then(|l| l.max_qty) else {
            return Remaining::Unlimited;
        };
        let left = u64::from(max).saturating_sub(self.used(product_id).total());
        Remaining::Pieces(u32::try_from(left).unwrap_or(u32::MAX))
    }

    /// # Errors
    /// [`Exceeded`] when `qty` more pieces do not fit.
    pub fn check(&self, product_id: ProductId, qty: u32) -> Result<(), Exceeded> {
        match self.remaining(product_id) {
            Remaining::Pieces(remaining) if qty > remaining => Err(Exceeded {
                product_id,
                remaining,
            }),
            _ => Ok(()),
        }
    }

    /// Checks several lines as one request: each line also counts the lines before it.
    ///
    /// # Errors
    /// [`Exceeded`] for the first line that does not fit.
    pub fn check_all(&self, request: &[(ProductId, u32)]) -> Result<(), Exceeded> {
        let mut capacity = self.clone();
        for &(product_id, qty) in request {
            capacity.check(product_id, qty)?;
            let used = capacity.used.entry(product_id).or_default();
            used.ordered = used.ordered.saturating_add(qty);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const BREAD: ProductId = ProductId::new(1);
    const COOKIES: ProductId = ProductId::new(3);

    fn limit(product_id: ProductId, max_qty: Option<u32>) -> ProductLimit {
        ProductLimit {
            product_id,
            max_qty,
        }
    }

    fn used(ordered: u32, extras: u32) -> Used {
        Used { ordered, extras }
    }

    #[test]
    fn product_maximum_minus_orders_and_extras() {
        let capacity = Capacity::new(
            [limit(BREAD, Some(20))],
            BTreeMap::from([(BREAD, used(12, 3))]),
        );
        assert_eq!(capacity.remaining(BREAD), Remaining::Pieces(5));
        assert_eq!(capacity.check(BREAD, 5), Ok(()));
        assert_eq!(
            capacity.check(BREAD, 6),
            Err(Exceeded {
                product_id: BREAD,
                remaining: 5
            })
        );
    }

    #[test]
    fn a_request_counts_its_own_lines_of_the_same_product() {
        let capacity = Capacity::new([limit(BREAD, Some(10))], BTreeMap::new());
        assert_eq!(
            capacity.check_all(&[(BREAD, 6), (BREAD, 6)]),
            Err(Exceeded {
                product_id: BREAD,
                remaining: 4
            })
        );
        assert_eq!(capacity.check_all(&[(BREAD, 6), (BREAD, 4)]), Ok(()));
        assert_eq!(capacity.check_all(&[]), Ok(()));
    }

    #[test]
    fn unlimited_and_unknown_products() {
        let capacity = Capacity::new([limit(COOKIES, None)], BTreeMap::new());
        assert_eq!(capacity.remaining(COOKIES), Remaining::Unlimited);
        assert_eq!(capacity.remaining(BREAD), Remaining::Unlimited);
        assert_eq!(capacity.check(BREAD, u32::MAX), Ok(()));
    }

    #[test]
    fn over_used_limits_leave_zero() {
        let capacity = Capacity::new(
            [limit(BREAD, Some(5))],
            BTreeMap::from([(BREAD, used(u32::MAX, u32::MAX))]),
        );
        assert_eq!(capacity.remaining(BREAD), Remaining::Pieces(0));
    }

    #[test]
    fn exceeded_message() {
        let err = Exceeded {
            product_id: BREAD,
            remaining: 2,
        };
        assert_eq!(err.to_string(), "only 2 left of product ProductId(1)");
    }

    fn day() -> impl Strategy<Value = Vec<ProductLimit>> {
        prop::collection::vec(prop::option::of(0_u32..100), 1..6).prop_map(|maxima| {
            maxima
                .into_iter()
                .enumerate()
                .map(|(i, max_qty)| limit(ProductId::new(i64::try_from(i).unwrap()), max_qty))
                .collect()
        })
    }

    proptest! {
        #[test]
        fn accepted_requests_never_oversell(
            day in day(),
            requests in prop::collection::vec((0_usize..6, 0_u32..40, any::<bool>()), 0..60),
        ) {
            let mut taken: BTreeMap<ProductId, Used> = BTreeMap::new();
            for (index, qty, is_order) in requests {
                let Some(product) = day.get(index % day.len()) else { continue };
                let capacity = Capacity::new(day.clone(), taken.clone());
                if capacity.check(product.product_id, qty).is_ok() {
                    let entry = taken.entry(product.product_id).or_default();
                    if is_order { entry.ordered += qty } else { entry.extras += qty }
                }
            }
            for product in &day {
                let total = taken.get(&product.product_id).copied().unwrap_or_default().total();
                if let Some(max) = product.max_qty {
                    prop_assert!(total <= u64::from(max));
                }
            }
        }

        #[test]
        fn an_accepted_request_never_oversells(
            day in day(),
            request in prop::collection::vec((0_usize..6, 0_u32..60), 0..8),
        ) {
            let request: Vec<(ProductId, u32)> = request
                .into_iter()
                .map(|(index, qty)| (day[index % day.len()].product_id, qty))
                .collect();
            let empty = Capacity::new(day.clone(), BTreeMap::new());
            if empty.check_all(&request).is_ok() {
                let mut taken: BTreeMap<ProductId, Used> = BTreeMap::new();
                for &(product_id, qty) in &request {
                    taken.entry(product_id).or_default().ordered += qty;
                }
                for product in &day {
                    let total = taken.get(&product.product_id).copied().unwrap_or_default().total();
                    if let Some(max) = product.max_qty {
                        prop_assert!(total <= u64::from(max));
                    }
                }
            }
        }

        #[test]
        fn remaining_is_exactly_what_still_fits(
            day in day(),
            usage in prop::collection::vec((0_u32..300, 0_u32..300), 6),
        ) {
            let taken: BTreeMap<_, _> = day.iter()
                .zip(usage)
                .map(|(p, (ordered, extras))| (p.product_id, Used { ordered, extras }))
                .collect();
            let capacity = Capacity::new(day.clone(), taken);
            for product in &day {
                match capacity.remaining(product.product_id) {
                    Remaining::Unlimited => prop_assert!(product.max_qty.is_none()),
                    Remaining::Pieces(left) => {
                        prop_assert!(capacity.check(product.product_id, left).is_ok());
                        prop_assert!(capacity.check(product.product_id, left + 1).is_err());
                    }
                }
            }
        }
    }
}
