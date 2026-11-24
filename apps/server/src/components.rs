//! What the component macros in templates/components/ take. Text fields hold display text
//! (already translated, or user data); the templates escape it.

pub struct SelectOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

pub struct Product {
    pub name: String,
    pub description: String,
    pub price_cents: i64,
    /// `None` until the baker has approved the allergen data; the tile then says so.
    pub allergens: Option<ApprovedAllergens>,
    pub sold_out: bool,
    /// Pieces left, shown when the product has a limit.
    pub remaining: Option<i64>,
    /// The add-to-cart form; `None` shows the button disabled.
    pub order: Option<ProductOrder>,
}

/// Allergen data the baker approved (spec: allergen approval flow). Only
/// [`ApprovedAllergens::from_catalog`] builds it for the shop, so an unapproved
/// declaration can never reach a tile.
pub struct ApprovedAllergens {
    contains: Vec<&'static str>,
    may_contain: Vec<&'static str>,
    ingredients: String,
}

impl ApprovedAllergens {
    pub fn from_catalog(product: &catalog::Product) -> Option<Self> {
        product.published_allergen_info().map(|info| Self {
            contains: info.allergens.iter().map(|a| a.code()).collect(),
            may_contain: info.may_contain.iter().map(|a| a.code()).collect(),
            ingredients: info.ingredient_text.clone(),
        })
    }

    /// Example data for the component gallery.
    pub(crate) fn example(
        contains: Vec<&'static str>,
        may_contain: Vec<&'static str>,
        ingredients: String,
    ) -> Self {
        Self {
            contains,
            may_contain,
            ingredients,
        }
    }

    pub fn contains(&self) -> &[&'static str] {
        &self.contains
    }

    pub fn may_contain(&self) -> &[&'static str] {
        &self.may_contain
    }

    pub fn ingredients(&self) -> &str {
        &self.ingredients
    }
}

/// The add-to-cart form of a product tile. Field names carry the product id, so every
/// tile on a page has its own field ids.
pub struct ProductOrder {
    pub action: String,
    pub product_id: i64,
    pub qty: String,
    /// For pay what you want (F4b): the chosen price field.
    pub price: Option<PriceChoice>,
    pub error: String,
}

pub struct PriceChoice {
    pub value: String,
    /// Suggested price, minimum and maximum, as display text.
    pub hint: String,
}

pub struct PickupWindow {
    pub value: String,
    pub day: String,
    pub time: String,
    pub place: String,
    /// `None` when the window has no order limit.
    pub places_left: Option<i64>,
    pub selected: bool,
}

impl PickupWindow {
    pub fn is_full(&self) -> bool {
        self.places_left.is_some_and(|n| n <= 0)
    }
}

pub struct Table {
    /// Unique on the page; the caption gets the id `{id}-caption`.
    pub id: String,
    pub caption: String,
    pub head: Vec<Cell>,
    pub rows: Vec<Vec<Cell>>,
}

pub struct Cell {
    pub text: String,
    /// Right-aligned with tabular figures: amounts and counts.
    pub numeric: bool,
}

impl Cell {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            numeric: false,
        }
    }

    pub fn number(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            numeric: true,
        }
    }
}

impl ProductOrder {
    pub fn qty_name(&self) -> String {
        format!("aantal-{}", self.product_id)
    }

    pub fn price_name(&self) -> String {
        format!("prijs-{}", self.product_id)
    }
}

/// One drop on the calendar (F1a). Text fields are display text.
pub struct Drop {
    pub date: String,
    pub status: String,
    /// Open for orders: the status is highlighted.
    pub open: bool,
    /// One line per pickup slot: when and where.
    pub pickups: Vec<String>,
    pub deadline: String,
    /// `None` for a normal sale (F4c).
    pub charity: Option<DropCharity>,
    /// The drop's page; empty for none.
    pub link: String,
}

pub struct DropCharity {
    /// Says who the drop is for.
    pub text: String,
    pub url: String,
    pub link: String,
}

pub struct Cart {
    /// Where the remove buttons post.
    pub action: String,
    pub lines: Table,
    pub total_cents: i64,
    pub removes: Vec<CartRemove>,
    /// The checkout page; empty hides the button.
    pub checkout: String,
    pub message: Option<Message>,
}

pub struct CartRemove {
    pub product_id: i64,
    pub label: String,
}

pub struct Message {
    /// info, success, warning or error, as in the alert component.
    pub kind: &'static str,
    pub text: String,
}
