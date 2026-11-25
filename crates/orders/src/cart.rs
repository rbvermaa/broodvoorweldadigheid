use std::collections::BTreeMap;

use bakedays::BakeDayProduct;
use catalog::{KitchenMayContain, Pricing, Product};
use domain::{BakeDayId, Line, Money, MoneyError, PriceRefused, ProductId, VatRate, VatTotals};
use serde::{Deserialize, Serialize};

/// Most pieces of one product in one order.
pub const MAX_QTY: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CartError {
    #[error("the cart is empty")]
    Empty,
    #[error("at most {MAX_QTY} pieces of product {0:?}")]
    TooMany(ProductId),
    #[error("product {0:?} appears twice")]
    Duplicate(ProductId),
    #[error("product {0:?} is not for sale on this bake day")]
    Unavailable(ProductId),
    #[error("the allergen data of product {0:?} is not approved")]
    AllergensNotApproved(ProductId),
    #[error("the kitchen-wide may-contain list is not approved")]
    KitchenAllergensNotApproved,
    #[error("product {0:?} has a fixed price")]
    PriceNotChoosable(ProductId),
    #[error("product {product_id:?}: {refused}")]
    Price {
        product_id: ProductId,
        refused: PriceRefused,
    },
}

/// Pieces of one product, and the price the customer chose if it is pay what you want.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CartLine {
    pub qty: u32,
    pub chosen_price: Option<Money>,
}

/// What a customer intends to order for one bake day. Kept in the session, so it is
/// checked again against the catalog and the range at checkout ([`Cart::price`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "CartRepr", try_from = "CartRepr")]
pub struct Cart {
    bake_day: BakeDayId,
    lines: BTreeMap<ProductId, CartLine>,
}

#[derive(Serialize, Deserialize)]
struct CartRepr {
    bake_day: i64,
    lines: Vec<LineRepr>,
}

#[derive(Serialize, Deserialize)]
struct LineRepr {
    product: i64,
    qty: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chosen_price_cents: Option<i64>,
}

impl From<Cart> for CartRepr {
    fn from(cart: Cart) -> Self {
        Self {
            bake_day: cart.bake_day.get(),
            lines: cart
                .lines
                .into_iter()
                .map(|(product, line)| LineRepr {
                    product: product.get(),
                    qty: line.qty,
                    chosen_price_cents: line.chosen_price.map(Money::cents),
                })
                .collect(),
        }
    }
}

impl TryFrom<CartRepr> for Cart {
    type Error = CartError;

    fn try_from(repr: CartRepr) -> Result<Self, CartError> {
        let mut cart = Self::new(BakeDayId::new(repr.bake_day));
        for line in repr.lines {
            let product = ProductId::new(line.product);
            if cart.lines.contains_key(&product) {
                return Err(CartError::Duplicate(product));
            }
            cart.set(
                product,
                line.qty,
                line.chosen_price_cents.map(Money::from_cents),
            )?;
        }
        Ok(cart)
    }
}

/// A cart line priced from the catalog: the price and BTW rate an order line copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricedLine {
    pub product_id: ProductId,
    pub qty: u32,
    pub unit_price: Money,
    pub suggested_price: Option<Money>,
    pub vat_rate: VatRate,
}

impl PricedLine {
    #[must_use]
    pub const fn line(&self) -> Line {
        Line {
            unit_price: self.unit_price,
            qty: self.qty,
            vat_rate: self.vat_rate,
        }
    }
}

/// Totals per BTW rate of priced lines, rounded per line (ADR 0005).
///
/// # Errors
/// [`MoneyError::Overflow`].
pub fn totals(lines: &[PricedLine]) -> Result<VatTotals, MoneyError> {
    let lines: Vec<Line> = lines.iter().map(PricedLine::line).collect();
    VatTotals::from_lines(&lines)
}

impl Cart {
    #[must_use]
    pub const fn new(bake_day: BakeDayId) -> Self {
        Self {
            bake_day,
            lines: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn bake_day(&self) -> BakeDayId {
        self.bake_day
    }

    /// Sets the pieces of a product; zero removes it. The chosen price is checked at
    /// [`Cart::price`], against the catalog of that moment.
    ///
    /// # Errors
    /// [`CartError::TooMany`] above [`MAX_QTY`].
    pub fn set(
        &mut self,
        product: ProductId,
        qty: u32,
        chosen_price: Option<Money>,
    ) -> Result<(), CartError> {
        if qty > MAX_QTY {
            return Err(CartError::TooMany(product));
        }
        if qty == 0 {
            self.lines.remove(&product);
        } else {
            self.lines.insert(product, CartLine { qty, chosen_price });
        }
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Lines by product id.
    pub fn lines(&self) -> impl Iterator<Item = (ProductId, CartLine)> + '_ {
        self.lines.iter().map(|(&product, &line)| (product, line))
    }

    /// Product ids in ascending order, the order capacity locks are taken in.
    #[must_use]
    pub fn products(&self) -> Vec<ProductId> {
        self.lines.keys().copied().collect()
    }

    /// Prices every line from `products` (the catalog) and checks it against the bake
    /// day's `range`: each product must be active, sold online, on the range and have
    /// approved allergen data, and the kitchen-wide may-contain list must be approved. A
    /// pay-what-you-want price must lie within the product's minimum and maximum; without
    /// a choice it is the suggested price (F4b).
    ///
    /// # Errors
    /// The first [`CartError`] found, by product id.
    pub fn price(
        &self,
        products: &[Product],
        range: &[BakeDayProduct],
        kitchen: &KitchenMayContain,
    ) -> Result<Vec<PricedLine>, CartError> {
        if self.is_empty() {
            return Err(CartError::Empty);
        }
        self.lines()
            .map(|(product_id, line)| {
                let product = products
                    .iter()
                    .find(|p| p.id == product_id)
                    .filter(|p| p.active && p.sold_online)
                    .filter(|_| range.iter().any(|r| r.product_id == product_id))
                    .ok_or(CartError::Unavailable(product_id))?;
                if product.approval.is_none() {
                    return Err(CartError::AllergensNotApproved(product_id));
                }
                if kitchen.approval.is_none() {
                    return Err(CartError::KitchenAllergensNotApproved);
                }
                let (unit_price, suggested_price) = match (product.pricing, line.chosen_price) {
                    (Pricing::Fixed(price), None) => (price, None),
                    (Pricing::Fixed(_), Some(_)) => {
                        return Err(CartError::PriceNotChoosable(product_id));
                    }
                    (Pricing::PayWhatYouWant(pwyw), chosen) => {
                        let price =
                            pwyw.check(chosen.unwrap_or(pwyw.suggested()))
                                .map_err(|refused| CartError::Price {
                                    product_id,
                                    refused,
                                })?;
                        (price, Some(pwyw.suggested()))
                    }
                };
                Ok(PricedLine {
                    product_id,
                    qty: line.qty,
                    unit_price,
                    suggested_price,
                    vat_rate: product.vat_rate,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catalog::{AllergenInfo, Approval, KitchenMayContain};
    use domain::{Allergen, PayWhatYouWant, StaffUserId};
    use proptest::prelude::*;

    const DAY: BakeDayId = BakeDayId::new(7);
    const BREAD: ProductId = ProductId::new(1);
    const DONATION_LOAF: ProductId = ProductId::new(2);
    const COOKIES: ProductId = ProductId::new(3);

    fn eur(cents: i64) -> Money {
        Money::from_cents(cents)
    }

    fn product(id: ProductId, pricing: Pricing, vat_rate: VatRate) -> Product {
        Product {
            id,
            name_nl: format!("Product {}", id.get()),
            description_nl: String::new(),
            photo_path: None,
            pricing,
            vat_rate,
            unit_weight_g: 800,
            dough_g_per_piece: 900,
            freezer_days: None,
            lead_time_hours: None,
            allergen_info: AllergenInfo::default(),
            ingredient_list: None,
            has_recipe: false,
            approval: Some(Approval {
                by: StaffUserId::new(1),
                at: "2026-10-01T10:00:00Z".parse().unwrap(),
            }),
            approval_cleared: None,
            allergen_revision: 0,
            packaged: false,
            sold_online: true,
            estimated_cost: None,
            active: true,
        }
    }

    fn kitchen() -> KitchenMayContain {
        KitchenMayContain {
            allergens: [Allergen::Nuts].into(),
            approval: Some(Approval {
                by: StaffUserId::new(1),
                at: "2026-10-01T10:00:00Z".parse().unwrap(),
            }),
        }
    }

    fn pwyw(suggested: i64, min: i64, max: i64) -> Pricing {
        Pricing::PayWhatYouWant(PayWhatYouWant::new(eur(suggested), eur(min), eur(max)).unwrap())
    }

    fn catalog() -> Vec<Product> {
        vec![
            product(BREAD, Pricing::Fixed(eur(450)), VatRate::Nine),
            product(DONATION_LOAF, pwyw(600, 300, 1500), VatRate::Nine),
            product(COOKIES, Pricing::Fixed(eur(395)), VatRate::TwentyOne),
        ]
    }

    fn on_range(product_id: ProductId) -> BakeDayProduct {
        BakeDayProduct {
            product_id,
            max_qty: None,
            capacity_pool_id: None,
            units_per_item: 1,
        }
    }

    fn range() -> Vec<BakeDayProduct> {
        vec![on_range(BREAD), on_range(DONATION_LOAF), on_range(COOKIES)]
    }

    fn cart(lines: &[(ProductId, u32, Option<i64>)]) -> Cart {
        let mut cart = Cart::new(DAY);
        for &(product, qty, price) in lines {
            cart.set(product, qty, price.map(eur)).unwrap();
        }
        cart
    }

    #[test]
    fn prices_copy_the_catalog_and_the_chosen_price() {
        let priced = cart(&[
            (COOKIES, 1, None),
            (BREAD, 2, None),
            (DONATION_LOAF, 1, Some(1000)),
        ])
        .price(&catalog(), &range(), &kitchen())
        .unwrap();
        assert_eq!(
            priced,
            [
                PricedLine {
                    product_id: BREAD,
                    qty: 2,
                    unit_price: eur(450),
                    suggested_price: None,
                    vat_rate: VatRate::Nine
                },
                PricedLine {
                    product_id: DONATION_LOAF,
                    qty: 1,
                    unit_price: eur(1000),
                    suggested_price: Some(eur(600)),
                    vat_rate: VatRate::Nine
                },
                PricedLine {
                    product_id: COOKIES,
                    qty: 1,
                    unit_price: eur(395),
                    suggested_price: None,
                    vat_rate: VatRate::TwentyOne
                },
            ]
        );
        let totals = totals(&priced).unwrap();
        assert_eq!(totals.gross(), Ok(eur(2295)));
        assert_eq!(totals.rate(VatRate::Nine).gross(), eur(1900));
        assert_eq!(totals.rate(VatRate::TwentyOne).vat(), eur(69));
    }

    #[test]
    fn a_pay_what_you_want_line_without_a_choice_pays_the_suggestion() {
        let priced = cart(&[(DONATION_LOAF, 1, None)])
            .price(&catalog(), &range(), &kitchen())
            .unwrap();
        assert_eq!(priced[0].unit_price, eur(600));
        assert_eq!(priced[0].suggested_price, Some(eur(600)));
    }

    #[test]
    fn refusals() {
        let refused = |cart: Cart, catalog: &[Product], range: &[BakeDayProduct]| {
            cart.price(catalog, range, &kitchen()).unwrap_err()
        };
        assert_eq!(
            refused(Cart::new(DAY), &catalog(), &range()),
            CartError::Empty
        );
        assert_eq!(
            refused(cart(&[(ProductId::new(99), 1, None)]), &catalog(), &range()),
            CartError::Unavailable(ProductId::new(99))
        );
        assert_eq!(
            refused(cart(&[(BREAD, 1, None)]), &catalog(), &[on_range(COOKIES)]),
            CartError::Unavailable(BREAD)
        );
        let mut offline = catalog();
        offline[0].sold_online = false;
        assert_eq!(
            refused(cart(&[(BREAD, 1, None)]), &offline, &range()),
            CartError::Unavailable(BREAD)
        );
        let mut inactive = catalog();
        inactive[0].active = false;
        assert_eq!(
            refused(cart(&[(BREAD, 1, None)]), &inactive, &range()),
            CartError::Unavailable(BREAD)
        );
        let mut unapproved = catalog();
        unapproved[0].approval = None;
        assert_eq!(
            refused(cart(&[(BREAD, 1, None)]), &unapproved, &range()),
            CartError::AllergensNotApproved(BREAD)
        );
        assert_eq!(
            cart(&[(BREAD, 1, None)])
                .price(&catalog(), &range(), &KitchenMayContain::default())
                .unwrap_err(),
            CartError::KitchenAllergensNotApproved
        );
        assert_eq!(
            refused(cart(&[(BREAD, 1, Some(450))]), &catalog(), &range()),
            CartError::PriceNotChoosable(BREAD)
        );
        assert_eq!(
            refused(cart(&[(DONATION_LOAF, 1, Some(299))]), &catalog(), &range()),
            CartError::Price {
                product_id: DONATION_LOAF,
                refused: PriceRefused::BelowMinimum {
                    chosen: eur(299),
                    min: eur(300)
                }
            }
        );
    }

    #[test]
    fn quantities_are_bounded_and_zero_removes() {
        let mut cart = cart(&[(BREAD, 2, None)]);
        assert_eq!(
            cart.set(BREAD, MAX_QTY + 1, None),
            Err(CartError::TooMany(BREAD))
        );
        cart.set(BREAD, MAX_QTY, None).unwrap();
        assert_eq!(
            cart.lines().collect::<Vec<_>>(),
            [(
                BREAD,
                CartLine {
                    qty: MAX_QTY,
                    chosen_price: None
                }
            )]
        );
        cart.set(BREAD, 0, None).unwrap();
        assert!(cart.is_empty());
        assert_eq!(cart.bake_day(), DAY);
    }

    #[test]
    fn round_trips_through_json() {
        let cart = cart(&[(COOKIES, 1, None), (DONATION_LOAF, 2, Some(800))]);
        let json = serde_json::to_string(&cart).unwrap();
        assert_eq!(
            json,
            r#"{"bake_day":7,"lines":[{"product":2,"qty":2,"chosen_price_cents":800},{"product":3,"qty":1}]}"#
        );
        assert_eq!(serde_json::from_str::<Cart>(&json).unwrap(), cart);
        assert_eq!(cart.products(), [DONATION_LOAF, COOKIES]);
    }

    #[test]
    fn a_tampered_session_cart_is_refused() {
        let duplicate = r#"{"bake_day":7,"lines":[{"product":1,"qty":1},{"product":1,"qty":2}]}"#;
        let err = serde_json::from_str::<Cart>(duplicate).unwrap_err();
        assert!(err.to_string().contains("appears twice"), "{err}");
        let too_many = r#"{"bake_day":7,"lines":[{"product":1,"qty":101}]}"#;
        assert!(serde_json::from_str::<Cart>(too_many).is_err());
    }

    fn arb_cart() -> impl Strategy<Value = Vec<(u8, u32, Option<i64>)>> {
        prop::collection::vec(
            (0_u8..3, 1..=MAX_QTY, prop::option::of(-10_i64..3000)),
            0..6,
        )
    }

    proptest! {
        #[test]
        fn a_priced_cart_totals_per_rate_equal_its_lines(lines in arb_cart()) {
            let ids = [BREAD, DONATION_LOAF, COOKIES];
            let mut cart = Cart::new(DAY);
            for (index, qty, price) in lines {
                let product = ids[usize::from(index)];
                let price = if product == DONATION_LOAF { price.map(eur) } else { None };
                cart.set(product, qty, price).unwrap();
            }
            if let Ok(priced) = cart.price(&catalog(), &range(), &kitchen()) {
                let totals = totals(&priced).unwrap();
                for rate in VatRate::ALL {
                    let gross: i64 = priced.iter()
                        .filter(|l| l.vat_rate == rate)
                        .map(|l| l.unit_price.cents() * i64::from(l.qty))
                        .sum();
                    prop_assert_eq!(totals.rate(rate).gross().cents(), gross);
                    prop_assert_eq!(totals.rate(rate).net().cents() + totals.rate(rate).vat().cents(), gross);
                }
            }
        }

        #[test]
        fn a_chosen_price_outside_the_bounds_is_always_refused(chosen in any::<i64>()) {
            let result = cart(&[(DONATION_LOAF, 1, Some(chosen))]).price(&catalog(), &range(), &kitchen());
            if (300..=1500).contains(&chosen) {
                prop_assert_eq!(result.unwrap()[0].unit_price, eur(chosen));
            } else {
                let refused = matches!(result, Err(CartError::Price { product_id, .. }) if product_id == DONATION_LOAF);
                prop_assert!(refused);
            }
        }
    }
}
