use std::collections::BTreeMap;

use domain::{CapacityPoolId, ProductId};

/// A product's limits on one bake day (F3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductLimit {
    pub product_id: ProductId,
    pub max_qty: Option<u32>,
    pub pool: Option<CapacityPoolId>,
    pub units_per_item: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolLimit {
    pub id: CapacityPoolId,
    pub max_units: u32,
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
    pools: BTreeMap<CapacityPoolId, u32>,
    used: BTreeMap<ProductId, Used>,
}

impl Capacity {
    #[must_use]
    pub fn new(
        products: impl IntoIterator<Item = ProductLimit>,
        pools: impl IntoIterator<Item = PoolLimit>,
        used: BTreeMap<ProductId, Used>,
    ) -> Self {
        Self {
            products: products.into_iter().map(|p| (p.product_id, p)).collect(),
            pools: pools.into_iter().map(|p| (p.id, p.max_units)).collect(),
            used,
        }
    }

    fn used(&self, product_id: ProductId) -> Used {
        self.used.get(&product_id).copied().unwrap_or_default()
    }

    fn pool_units_used(&self, pool: CapacityPoolId) -> u64 {
        self.products
            .values()
            .filter(|p| p.pool == Some(pool))
            .map(|p| self.used(p.product_id).total() * u64::from(p.units_per_item))
            .sum()
    }

    /// `min(max_qty - used, (pool max_units - pool units used) / units_per_item)`, never
    /// below zero. A product that is not on the bake day, or has neither a maximum nor a
    /// pool, is unlimited. A pool that is not listed counts as empty.
    #[must_use]
    pub fn remaining(&self, product_id: ProductId) -> Remaining {
        let Some(limit) = self.products.get(&product_id) else {
            return Remaining::Unlimited;
        };
        let used = self.used(product_id).total();
        let by_qty = limit.max_qty.map(|max| u64::from(max).saturating_sub(used));
        let by_pool = limit.pool.map(|pool| {
            let max_units = u64::from(self.pools.get(&pool).copied().unwrap_or(0));
            max_units.saturating_sub(self.pool_units_used(pool))
                / u64::from(limit.units_per_item.max(1))
        });
        match by_qty.into_iter().chain(by_pool).min() {
            None => Remaining::Unlimited,
            Some(pieces) => Remaining::Pieces(u32::try_from(pieces).unwrap_or(u32::MAX)),
        }
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

    /// Checks several products as one request: each line also counts the lines before
    /// it, so products that share a pool cannot overfill it together.
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
    const ROLLS: ProductId = ProductId::new(2);
    const COOKIES: ProductId = ProductId::new(3);
    const OVEN: CapacityPoolId = CapacityPoolId::new(10);

    fn limit(
        product_id: ProductId,
        max_qty: Option<u32>,
        pool: Option<CapacityPoolId>,
        units: u32,
    ) -> ProductLimit {
        ProductLimit {
            product_id,
            max_qty,
            pool,
            units_per_item: units,
        }
    }

    fn used(ordered: u32, extras: u32) -> Used {
        Used { ordered, extras }
    }

    #[test]
    fn product_maximum_minus_orders_and_extras() {
        let capacity = Capacity::new(
            [limit(BREAD, Some(20), None, 1)],
            [],
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
    fn pool_is_shared_by_units() {
        // Oven holds 30 units; a loaf takes 2, a bag of rolls 1.
        let capacity = Capacity::new(
            [
                limit(BREAD, Some(20), Some(OVEN), 2),
                limit(ROLLS, None, Some(OVEN), 1),
            ],
            [PoolLimit {
                id: OVEN,
                max_units: 30,
            }],
            BTreeMap::from([(BREAD, used(8, 2)), (ROLLS, used(3, 0))]),
        );
        // 30 - (10 * 2 + 3) = 7 units left.
        assert_eq!(capacity.remaining(BREAD), Remaining::Pieces(3));
        assert_eq!(capacity.remaining(ROLLS), Remaining::Pieces(7));
    }

    #[test]
    fn a_request_counts_its_own_lines_against_a_shared_pool() {
        let capacity = Capacity::new(
            [
                limit(BREAD, None, Some(OVEN), 2),
                limit(ROLLS, None, Some(OVEN), 1),
            ],
            [PoolLimit {
                id: OVEN,
                max_units: 10,
            }],
            BTreeMap::new(),
        );
        assert_eq!(capacity.check(BREAD, 4), Ok(()));
        assert_eq!(capacity.check(ROLLS, 4), Ok(()));
        assert_eq!(
            capacity.check_all(&[(BREAD, 4), (ROLLS, 4)]),
            Err(Exceeded {
                product_id: ROLLS,
                remaining: 2
            })
        );
        assert_eq!(capacity.check_all(&[(BREAD, 4), (ROLLS, 2)]), Ok(()));
        assert_eq!(capacity.check_all(&[]), Ok(()));
    }

    #[test]
    fn unlimited_and_unknown_products() {
        let capacity = Capacity::new([limit(COOKIES, None, None, 1)], [], BTreeMap::new());
        assert_eq!(capacity.remaining(COOKIES), Remaining::Unlimited);
        assert_eq!(capacity.remaining(BREAD), Remaining::Unlimited);
        assert_eq!(capacity.check(BREAD, u32::MAX), Ok(()));
    }

    #[test]
    fn over_used_limits_leave_zero() {
        let capacity = Capacity::new(
            [limit(BREAD, Some(5), Some(OVEN), 1)],
            [PoolLimit {
                id: OVEN,
                max_units: 3,
            }],
            BTreeMap::from([(BREAD, used(u32::MAX, u32::MAX))]),
        );
        assert_eq!(capacity.remaining(BREAD), Remaining::Pieces(0));
        let missing_pool = Capacity::new([limit(ROLLS, None, Some(OVEN), 1)], [], BTreeMap::new());
        assert_eq!(missing_pool.remaining(ROLLS), Remaining::Pieces(0));
    }

    #[test]
    fn exceeded_message() {
        let err = Exceeded {
            product_id: BREAD,
            remaining: 2,
        };
        assert_eq!(err.to_string(), "only 2 left of product ProductId(1)");
    }

    #[derive(Debug, Clone)]
    struct Day {
        products: Vec<ProductLimit>,
        pools: Vec<PoolLimit>,
    }

    fn day() -> impl Strategy<Value = Day> {
        let pools = prop::collection::vec(0_u32..200, 0..3);
        pools.prop_flat_map(|pool_sizes| {
            let pool_count = pool_sizes.len();
            let pools: Vec<_> = pool_sizes
                .iter()
                .enumerate()
                .map(|(i, &max_units)| PoolLimit {
                    id: CapacityPoolId::new(i64::try_from(i).unwrap()),
                    max_units,
                })
                .collect();
            let product = (
                prop::option::of(0_u32..100),
                prop::option::of(0..pool_count.max(1)),
                1_u32..5,
            );
            prop::collection::vec(product, 1..6).prop_map(move |specs| Day {
                pools: pools.clone(),
                products: specs
                    .into_iter()
                    .enumerate()
                    .map(|(i, (max_qty, pool, units))| ProductLimit {
                        product_id: ProductId::new(i64::try_from(i).unwrap()),
                        max_qty,
                        pool: pool
                            .filter(|_| pool_count > 0)
                            .map(|p| CapacityPoolId::new(i64::try_from(p).unwrap())),
                        units_per_item: units,
                    })
                    .collect(),
            })
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
                let Some(product) = day.products.get(index % day.products.len()) else { continue };
                let capacity = Capacity::new(day.products.clone(), day.pools.clone(), taken.clone());
                if capacity.check(product.product_id, qty).is_ok() {
                    let entry = taken.entry(product.product_id).or_default();
                    if is_order { entry.ordered += qty } else { entry.extras += qty }
                }
            }
            for product in &day.products {
                let total = taken.get(&product.product_id).copied().unwrap_or_default().total();
                if let Some(max) = product.max_qty {
                    prop_assert!(total <= u64::from(max));
                }
            }
            for pool in &day.pools {
                let units: u64 = day.products.iter()
                    .filter(|p| p.pool == Some(pool.id))
                    .map(|p| taken.get(&p.product_id).copied().unwrap_or_default().total() * u64::from(p.units_per_item))
                    .sum();
                prop_assert!(units <= u64::from(pool.max_units));
            }
        }

        #[test]
        fn an_accepted_request_never_oversells(
            day in day(),
            request in prop::collection::vec((0_usize..6, 0_u32..60), 0..8),
        ) {
            let request: Vec<(ProductId, u32)> = request
                .into_iter()
                .map(|(index, qty)| (day.products[index % day.products.len()].product_id, qty))
                .collect();
            let empty = Capacity::new(day.products.clone(), day.pools.clone(), BTreeMap::new());
            if empty.check_all(&request).is_ok() {
                let mut taken: BTreeMap<ProductId, Used> = BTreeMap::new();
                for &(product_id, qty) in &request {
                    taken.entry(product_id).or_default().ordered += qty;
                }
                for product in &day.products {
                    let total = taken.get(&product.product_id).copied().unwrap_or_default().total();
                    if let Some(max) = product.max_qty {
                        prop_assert!(total <= u64::from(max));
                    }
                }
                for pool in &day.pools {
                    let units: u64 = day.products.iter()
                        .filter(|p| p.pool == Some(pool.id))
                        .map(|p| taken.get(&p.product_id).copied().unwrap_or_default().total() * u64::from(p.units_per_item))
                        .sum();
                    prop_assert!(units <= u64::from(pool.max_units));
                }
            }
        }

        #[test]
        fn remaining_is_exactly_what_still_fits(
            day in day(),
            usage in prop::collection::vec((0_u32..300, 0_u32..300), 6),
        ) {
            let taken: BTreeMap<_, _> = day.products.iter()
                .zip(usage)
                .map(|(p, (ordered, extras))| (p.product_id, Used { ordered, extras }))
                .collect();
            let capacity = Capacity::new(day.products.clone(), day.pools.clone(), taken);
            for product in &day.products {
                match capacity.remaining(product.product_id) {
                    Remaining::Unlimited => {
                        prop_assert!(product.max_qty.is_none() && product.pool.is_none());
                    }
                    Remaining::Pieces(left) => {
                        prop_assert!(capacity.check(product.product_id, left).is_ok());
                        prop_assert!(capacity.check(product.product_id, left + 1).is_err());
                    }
                }
            }
        }
    }
}
