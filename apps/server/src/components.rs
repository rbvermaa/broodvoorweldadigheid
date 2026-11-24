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
    /// Domain allergen codes, such as `cereals_containing_gluten`.
    pub allergens: Vec<&'static str>,
    pub may_contain: Vec<&'static str>,
    pub sold_out: bool,
}

pub struct PickupWindow {
    pub value: String,
    pub day: String,
    pub time: String,
    pub place: String,
    pub places_left: i64,
    pub selected: bool,
}

impl PickupWindow {
    pub fn is_full(&self) -> bool {
        self.places_left <= 0
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
    /// Makes the text a link to this path.
    pub href: Option<String>,
}

impl Cell {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            numeric: false,
            href: None,
        }
    }

    pub fn number(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            numeric: true,
            href: None,
        }
    }

    pub fn link(text: impl Into<String>, href: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            numeric: false,
            href: Some(href.into()),
        }
    }
}
