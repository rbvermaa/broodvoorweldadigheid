//! The staff menu in templates/staff/layout.html.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Where the entry shows; the lowest comes first.
    pub order: u16,
    pub path: &'static str,
    /// Fluent message id.
    pub label: &'static str,
}

const fn entry(order: u16, path: &'static str, label: &'static str) -> Entry {
    Entry { order, path, label }
}

// One line per entry, sorted by path; `order` sets the menu order (ADR 0044).
const ENTRIES: &[Entry] = &[
    entry(0, "/staff", "staff-title"),
    entry(10, "/staff/bake-days", "staff-nav-bake-days"),
    entry(100, "/staff/business-accounts", "staff-nav-b2b"),
    entry(80, "/staff/charities", "staff-nav-charities"),
    entry(105, "/staff/costing", "staff-nav-costing"),
    entry(30, "/staff/customers", "staff-nav-customers"),
    entry(50, "/staff/ingredients", "staff-nav-ingredients"),
    entry(70, "/staff/locations", "staff-nav-locations"),
    entry(20, "/staff/orders", "staff-nav-orders"),
    entry(90, "/staff/polls", "staff-nav-polls"),
    entry(40, "/staff/products", "staff-nav-products"),
    entry(60, "/staff/recipes", "staff-nav-recipes"),
    entry(110, "/staff/reports", "reports-title"),
    entry(120, "/staff/settings", "staff-nav-settings"),
];

/// The entries in menu order.
pub fn entries() -> Vec<Entry> {
    let mut entries = ENTRIES.to_vec();
    entries.sort_by_key(|entry| entry.order);
    entries
}

/// The entry whose section holds `path`: the longest entry path that equals it or is a
/// parent of it, so `/staff` marks only the staff home.
pub fn current(path: &str) -> Option<&'static str> {
    ENTRIES
        .iter()
        .map(|entry| entry.path)
        .filter(|candidate| {
            path == *candidate
                || (*candidate != "/staff"
                    && path
                        .strip_prefix(candidate)
                        .is_some_and(|rest| rest.starts_with('/')))
        })
        .max_by_key(|candidate| candidate.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_is_sorted_by_path_and_orders_are_unique() {
        assert!(ENTRIES.is_sorted_by_key(|entry| entry.path));
        let orders: Vec<u16> = entries().iter().map(|entry| entry.order).collect();
        assert!(
            orders.windows(2).all(|pair| pair[0] < pair[1]),
            "{orders:?}"
        );
    }

    #[test]
    fn the_current_entry_is_the_section_of_the_path() {
        assert_eq!(current("/staff"), Some("/staff"));
        assert_eq!(current("/staff/bake-days"), Some("/staff/bake-days"));
        assert_eq!(
            current("/staff/bake-days/7/batches/2"),
            Some("/staff/bake-days")
        );
        assert_eq!(current("/staff/reports/donations"), Some("/staff/reports"));
        assert_eq!(current("/staff/bake-daysx"), None);
        assert_eq!(current("/staff/login"), None);
        assert_eq!(current("/drops/1"), None);
    }
}
