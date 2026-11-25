//! An ingredient list as labels show it (F17, F18, ADR 0046): names in descending order of
//! weight, each marked when it holds an allergen. A compound ingredient, such as a
//! filling, carries its own ingredients, shown in brackets after its name. Labels print
//! marked names in bold; the list keeps the structure, never markup.

use std::collections::BTreeSet;

use domain::Allergen;

use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ListedIngredient {
    pub name: String,
    /// Holds one of the 14 allergens: bold on labels. Never set on a compound ingredient;
    /// its parts carry the marks.
    pub emphasised: bool,
    /// The ingredients of a compound ingredient, by descending weight; empty for a plain
    /// one. Parts are plain: a list nests one level at most.
    pub parts: Vec<ListedIngredient>,
}

impl ListedIngredient {
    #[must_use]
    pub fn plain(name: impl Into<String>, emphasised: bool) -> Self {
        Self {
            name: name.into(),
            emphasised,
            parts: Vec::new(),
        }
    }

    #[must_use]
    pub fn compound(name: impl Into<String>, parts: Vec<Self>) -> Self {
        Self {
            name: name.into(),
            emphasised: false,
            parts,
        }
    }

    fn text(&self) -> String {
        if self.parts.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, join(&self.parts))
        }
    }

    fn sorted(&self) -> Self {
        let mut parts: Vec<Self> = self.parts.iter().map(Self::sorted).collect();
        parts.sort_unstable();
        Self {
            parts,
            ..self.clone()
        }
    }
}

fn join(items: &[ListedIngredient]) -> String {
    items
        .iter()
        .map(ListedIngredient::text)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One entry of a list in pre-order, as stored and as approval forms send it back: a
/// compound at depth 0 is followed by its parts at depth 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatEntry {
    pub name: String,
    pub emphasised: bool,
    pub depth: i16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngredientList(pub Vec<ListedIngredient>);

impl IngredientList {
    /// The names separated by commas, a compound's parts in brackets, without marks.
    #[must_use]
    pub fn text(&self) -> String {
        join(&self.0)
    }

    /// The same ingredients in another order (F18: warn when the order by weight changes),
    /// at the top level or inside a compound.
    #[must_use]
    pub fn same_ingredients_other_order(&self, other: &Self) -> bool {
        if self == other {
            return false;
        }
        let canonical = |list: &Self| {
            let mut items: Vec<ListedIngredient> =
                list.0.iter().map(ListedIngredient::sorted).collect();
            items.sort_unstable();
            items
        };
        canonical(self) == canonical(other)
    }

    /// The entries in pre-order with their depth.
    #[must_use]
    pub fn flat(&self) -> Vec<FlatEntry> {
        let mut out = Vec::new();
        for item in &self.0 {
            out.push(FlatEntry {
                name: item.name.clone(),
                emphasised: item.emphasised,
                depth: 0,
            });
            out.extend(item.parts.iter().map(|part| FlatEntry {
                name: part.name.clone(),
                emphasised: part.emphasised,
                depth: 1,
            }));
        }
        out
    }

    /// The list [`Self::flat`] gave; `None` when the entries do not form one: a part
    /// without an entry before it, a depth other than 0 or 1, or a marked compound.
    #[must_use]
    pub fn from_flat(entries: impl IntoIterator<Item = FlatEntry>) -> Option<Self> {
        let mut items: Vec<ListedIngredient> = Vec::new();
        for entry in entries {
            match entry.depth {
                0 => items.push(ListedIngredient::plain(entry.name, entry.emphasised)),
                1 => {
                    let compound = items.last_mut()?;
                    if compound.emphasised {
                        return None;
                    }
                    compound
                        .parts
                        .push(ListedIngredient::plain(entry.name, entry.emphasised));
                }
                _ => return None,
            }
        }
        Some(Self(items))
    }

    /// Names, marks and depths as three arrays of the same length.
    #[must_use]
    pub fn to_db(&self) -> (Vec<String>, Vec<bool>, Vec<i16>) {
        let flat = self.flat();
        let mut names = Vec::with_capacity(flat.len());
        let mut emphasis = Vec::with_capacity(flat.len());
        let mut depth = Vec::with_capacity(flat.len());
        for entry in flat {
            names.push(entry.name);
            emphasis.push(entry.emphasised);
            depth.push(entry.depth);
        }
        (names, emphasis, depth)
    }

    /// A list stored by [`Self::to_db`]. No depths (a list stored before compound
    /// ingredients) means every entry is at the top level.
    ///
    /// # Errors
    /// [`Error::Corrupt`] when the arrays do not form a list.
    pub fn from_db(
        names: Option<Vec<String>>,
        emphasis: Option<Vec<bool>>,
        depth: Option<Vec<i16>>,
    ) -> Result<Option<Self>, Error> {
        let corrupt = || Error::Corrupt("ingredient list".to_owned());
        match (names, emphasis) {
            (None, None) if depth.is_none() => Ok(None),
            (Some(names), Some(emphasis)) if names.len() == emphasis.len() => {
                let depth = depth.unwrap_or_else(|| vec![0; names.len()]);
                if depth.len() != names.len() {
                    return Err(corrupt());
                }
                Self::from_flat(names.into_iter().zip(emphasis).zip(depth).map(
                    |((name, emphasised), depth)| FlatEntry {
                        name,
                        emphasised,
                        depth,
                    },
                ))
                .map(Some)
                .ok_or_else(corrupt)
            }
            _ => Err(corrupt()),
        }
    }
}

/// Allergens and ingredient list computed from a recipe or a batch (F17).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComputedAllergens {
    pub allergens: BTreeSet<Allergen>,
    pub ingredients: IngredientList,
}

/// How computed values differ from approved ones; all false means they are equal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Difference {
    pub allergens: bool,
    pub ingredients: bool,
    /// The same ingredients in another order by weight. Implies `ingredients`.
    pub order: bool,
}

impl Difference {
    #[must_use]
    pub fn between(computed: &ComputedAllergens, approved: &ComputedAllergens) -> Self {
        Self {
            allergens: computed.allergens != approved.allergens,
            ingredients: computed.ingredients != approved.ingredients,
            order: computed
                .ingredients
                .same_ingredients_other_order(&approved.ingredients),
        }
    }

    #[must_use]
    pub const fn any(self) -> bool {
        self.allergens || self.ingredients
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(items: &[(&str, bool)]) -> Vec<ListedIngredient> {
        items
            .iter()
            .map(|&(name, emphasised)| ListedIngredient::plain(name, emphasised))
            .collect()
    }

    fn list(entries: &[(&str, bool)]) -> IngredientList {
        IngredientList(items(entries))
    }

    fn filled() -> IngredientList {
        IngredientList(vec![
            ListedIngredient::plain("tarwebloem", true),
            ListedIngredient::compound(
                "vulling",
                items(&[("suiker", false), ("kaneel", false), ("boter", true)]),
            ),
            ListedIngredient::plain("zout", false),
        ])
    }

    #[test]
    fn text_is_the_names_with_commas() {
        let flour = list(&[("tarwebloem", true), ("water", false), ("zout", false)]);
        assert_eq!(flour.text(), "tarwebloem, water, zout");
        assert_eq!(IngredientList::default().text(), "");
    }

    #[test]
    fn a_compound_shows_its_parts_in_brackets() {
        assert_eq!(
            filled().text(),
            "tarwebloem, vulling (suiker, kaneel, boter), zout"
        );
    }

    #[test]
    fn another_order_is_told_apart_from_other_ingredients() {
        let base = list(&[("tarwebloem", true), ("water", false), ("zout", false)]);
        let swapped = list(&[("water", false), ("tarwebloem", true), ("zout", false)]);
        let other = list(&[("speltbloem", true), ("water", false), ("zout", false)]);
        let shorter = list(&[("tarwebloem", true), ("water", false)]);
        assert!(swapped.same_ingredients_other_order(&base));
        assert!(!base.same_ingredients_other_order(&base));
        assert!(!other.same_ingredients_other_order(&base));
        assert!(!shorter.same_ingredients_other_order(&base));

        let approved = ComputedAllergens {
            allergens: BTreeSet::from([Allergen::CerealsContainingGluten]),
            ingredients: base,
        };
        let reordered = ComputedAllergens {
            ingredients: swapped,
            ..approved.clone()
        };
        let d = Difference::between(&reordered, &approved);
        assert_eq!(
            d,
            Difference {
                allergens: false,
                ingredients: true,
                order: true
            }
        );
        assert!(d.any());
        assert!(!Difference::between(&approved, &approved).any());
    }

    #[test]
    fn another_order_inside_a_compound_is_an_order_change() {
        let mut inner = filled();
        inner.0[1].parts.swap(0, 2);
        assert!(inner.same_ingredients_other_order(&filled()));
        let mut outer = filled();
        outer.0.swap(0, 1);
        assert!(outer.same_ingredients_other_order(&filled()));
        let mut moved = filled();
        let sugar = moved.0[1].parts.remove(0);
        moved.0.push(sugar);
        assert!(!moved.same_ingredients_other_order(&filled()));
        let mut merged = filled();
        let parts = std::mem::take(&mut merged.0[1].parts);
        merged.0.splice(1..2, parts);
        assert!(!merged.same_ingredients_other_order(&filled()));
    }

    #[test]
    fn stored_lists_round_trip_and_mismatches_are_corrupt() {
        let flour = list(&[("tarwebloem", true), ("water", false)]);
        let (names, emphasis, depth) = flour.to_db();
        assert_eq!(depth, [0, 0]);
        assert_eq!(
            IngredientList::from_db(Some(names.clone()), Some(emphasis.clone()), Some(depth))
                .unwrap(),
            Some(flour.clone())
        );
        assert_eq!(
            IngredientList::from_db(Some(names), Some(emphasis), None).unwrap(),
            Some(flour)
        );
        let (names, emphasis, depth) = filled().to_db();
        assert_eq!(depth, [0, 0, 1, 1, 1, 0]);
        assert_eq!(
            IngredientList::from_db(Some(names), Some(emphasis), Some(depth)).unwrap(),
            Some(filled())
        );
        assert_eq!(IngredientList::from_db(None, None, None).unwrap(), None);
        assert!(matches!(
            IngredientList::from_db(Some(vec!["water".to_owned()]), Some(vec![]), None),
            Err(Error::Corrupt(_))
        ));
        assert!(matches!(
            IngredientList::from_db(None, Some(vec![]), None),
            Err(Error::Corrupt(_))
        ));
        assert!(matches!(
            IngredientList::from_db(
                Some(vec!["water".to_owned()]),
                Some(vec![false]),
                Some(vec![0, 0])
            ),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn flat_entries_must_form_a_list() {
        let entry = |name: &str, emphasised, depth| FlatEntry {
            name: name.to_owned(),
            emphasised,
            depth,
        };
        assert_eq!(IngredientList::from_flat(filled().flat()), Some(filled()));
        assert_eq!(IngredientList::from_flat([entry("suiker", false, 1)]), None);
        assert_eq!(
            IngredientList::from_flat([entry("vulling", false, 0), entry("x", false, 2)]),
            None
        );
        assert_eq!(
            IngredientList::from_flat([entry("vulling", true, 0), entry("x", false, 1)]),
            None,
            "a compound is never marked"
        );
    }
}
