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

/// Allergen data the baker approved (spec: allergen approval flow): the product's own
/// and the kitchen-wide may-contain list. Only [`ApprovedAllergens::from_catalog`] builds
/// it for the shop, so an unapproved declaration can never reach a tile.
pub struct ApprovedAllergens {
    contains: Vec<&'static str>,
    may_contain: Vec<&'static str>,
    ingredients: Vec<ListedIngredient>,
}

/// One entry of an ingredient list; an `emphasised` one holds an allergen and is bold.
/// A compound ingredient has its own ingredients in `parts`, shown in brackets.
pub struct ListedIngredient {
    pub name: String,
    pub emphasised: bool,
    pub parts: Vec<ListedIngredient>,
}

fn listed_one(item: &catalog::ListedIngredient) -> ListedIngredient {
    ListedIngredient {
        name: item.name.clone(),
        emphasised: item.emphasised,
        parts: item.parts.iter().map(listed_one).collect(),
    }
}

pub fn listed(list: &catalog::IngredientList) -> Vec<ListedIngredient> {
    list.0.iter().map(listed_one).collect()
}

/// Hand-entered text as one plain entry; nothing when it is empty.
fn plain(text: String) -> Vec<ListedIngredient> {
    if text.is_empty() {
        return Vec::new();
    }
    vec![ListedIngredient {
        name: text,
        emphasised: false,
        parts: Vec::new(),
    }]
}

impl ApprovedAllergens {
    pub fn from_catalog(
        product: &catalog::Product,
        kitchen: &catalog::KitchenMayContain,
    ) -> Option<Self> {
        product.published_allergens(kitchen).map(|shown| Self {
            contains: shown.contains.iter().map(|a| a.code()).collect(),
            may_contain: shown.may_contain.iter().map(|a| a.code()).collect(),
            ingredients: product
                .ingredient_list
                .as_ref()
                .map_or_else(|| plain(shown.ingredient_text), listed),
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
            ingredients: plain(ingredients),
        }
    }

    pub fn contains(&self) -> &[&'static str] {
        &self.contains
    }

    pub fn may_contain(&self) -> &[&'static str] {
        &self.may_contain
    }

    pub fn ingredients(&self) -> &[ListedIngredient] {
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

impl Table {
    /// Whether a cell in `row` links to `href`: the row a save came from (ADR 0052).
    pub fn links_to(&self, row: &[Cell], href: &str) -> bool {
        !href.is_empty() && row.iter().any(|cell| cell.href.as_deref() == Some(href))
    }

    /// The text of the first cell in a row that links to `href`.
    pub fn name_of(&self, href: &str) -> Option<&str> {
        self.rows
            .iter()
            .flatten()
            .find(|cell| cell.href.as_deref() == Some(href))
            .map(|cell| cell.text.as_str())
    }
}

pub struct Cell {
    pub text: String,
    /// Right-aligned with tabular figures: amounts and counts.
    pub numeric: bool,
    /// Makes the text a link to this path.
    pub href: Option<String>,
    /// Shown in the muted style: a placeholder, not data.
    pub muted: bool,
}

impl Cell {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            numeric: false,
            href: None,
            muted: false,
        }
    }

    pub fn number(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            numeric: true,
            href: None,
            muted: false,
        }
    }

    pub fn link(text: impl Into<String>, href: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            numeric: false,
            href: Some(href.into()),
            muted: false,
        }
    }

    pub fn muted(text: impl Into<String>) -> Self {
        Self {
            muted: true,
            ..Self::text(text)
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
    /// An http(s) link to the charity; empty for none.
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

/// A horizontal bar chart drawn as SVG on the server, each label above its bar so long
/// Dutch names have the full width. Coordinates are in the SVG's own units; the
/// stylesheet scales it to the page width.
pub struct BarChart {
    /// Unique on the page; the title gets the id `{id}-title`.
    pub id: String,
    pub caption: String,
    pub height: u32,
    pub bars: Vec<Bar>,
}

pub struct Bar {
    pub label: String,
    pub value: String,
    /// Baseline of the label.
    pub label_y: u32,
    /// Top of the bar.
    pub y: u32,
    /// Baseline of the value text beside the bar.
    pub value_y: u32,
    pub width: u32,
    pub value_x: u32,
}

impl BarChart {
    const MAX_BAR: u32 = 480;
    const ROW: u32 = 60;
    const GAP: u32 = 8;

    /// One bar per `(label, amount, value text)`. The longest bar is the largest amount;
    /// a negative amount draws no bar.
    pub fn new(
        id: impl Into<String>,
        caption: impl Into<String>,
        items: Vec<(String, i64, String)>,
    ) -> Self {
        let max = items.iter().map(|(_, n, _)| *n).max().unwrap_or(0).max(1);
        let bars = (0_u32..)
            .zip(items)
            .map(|(i, (label, amount, value))| {
                let scaled =
                    i128::from(amount.max(0)) * i128::from(Self::MAX_BAR) / i128::from(max);
                let width = u32::try_from(scaled).unwrap_or(Self::MAX_BAR);
                let top = i * Self::ROW;
                Bar {
                    label,
                    value,
                    label_y: top + 18,
                    y: top + 26,
                    value_y: top + 44,
                    width,
                    value_x: width + Self::GAP,
                }
            })
            .collect::<Vec<_>>();
        let rows = u32::try_from(bars.len()).unwrap_or(u32::MAX).max(1);
        Self {
            id: id.into(),
            caption: caption.into(),
            height: rows.saturating_mul(Self::ROW),
            bars,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BarChart;

    #[test]
    fn bars_scale_to_the_largest_amount() {
        let chart = BarChart::new(
            "c",
            "Giften",
            vec![
                ("jan".into(), 400, "€ 4".into()),
                ("feb".into(), 100, "€ 1".into()),
                ("mrt".into(), -50, "€ -0,50".into()),
            ],
        );
        let widths: Vec<u32> = chart.bars.iter().map(|b| b.width).collect();
        assert_eq!(widths, [480, 120, 0]);
        assert_eq!(chart.height, 180);
        let second = &chart.bars[1];
        assert_eq!((second.label_y, second.y, second.value_y), (78, 86, 104));
        assert_eq!(chart.bars[0].value_x, 488);
    }

    #[test]
    fn an_empty_chart_keeps_one_row() {
        let chart = BarChart::new("c", "Giften", Vec::new());
        assert!(chart.bars.is_empty());
        assert_eq!(chart.height, 60);
    }
}
