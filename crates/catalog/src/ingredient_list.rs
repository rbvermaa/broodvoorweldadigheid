//! An ingredient list as labels show it (F17, F18, ADR 0046): names in descending order of
//! weight, each marked when it holds an allergen. Labels print marked names in bold; the
//! list keeps the structure, never markup.

use std::collections::BTreeSet;

use domain::Allergen;

use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedIngredient {
    pub name: String,
    /// Holds one of the 14 allergens: bold on labels.
    pub emphasised: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngredientList(pub Vec<ListedIngredient>);

impl IngredientList {
    /// The names separated by commas, without marks.
    #[must_use]
    pub fn text(&self) -> String {
        self.0
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The same ingredients in another order (F18: warn when the order by weight changes).
    #[must_use]
    pub fn same_ingredients_other_order(&self, other: &Self) -> bool {
        if self == other || self.0.len() != other.0.len() {
            return false;
        }
        let mut mine: Vec<(&str, bool)> = self.0.iter().map(|i| (&*i.name, i.emphasised)).collect();
        let mut theirs: Vec<(&str, bool)> =
            other.0.iter().map(|i| (&*i.name, i.emphasised)).collect();
        mine.sort_unstable();
        theirs.sort_unstable();
        mine == theirs
    }

    pub(crate) fn to_db(&self) -> (Vec<String>, Vec<bool>) {
        self.0
            .iter()
            .map(|i| (i.name.clone(), i.emphasised))
            .unzip()
    }

    pub(crate) fn from_db(
        names: Option<Vec<String>>,
        emphasis: Option<Vec<bool>>,
    ) -> Result<Option<Self>, Error> {
        match (names, emphasis) {
            (None, None) => Ok(None),
            (Some(names), Some(emphasis)) if names.len() == emphasis.len() => Ok(Some(Self(
                names
                    .into_iter()
                    .zip(emphasis)
                    .map(|(name, emphasised)| ListedIngredient { name, emphasised })
                    .collect(),
            ))),
            _ => Err(Error::Corrupt("ingredient list".to_owned())),
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

    fn list(items: &[(&str, bool)]) -> IngredientList {
        IngredientList(
            items
                .iter()
                .map(|&(name, emphasised)| ListedIngredient {
                    name: name.to_owned(),
                    emphasised,
                })
                .collect(),
        )
    }

    #[test]
    fn text_is_the_names_with_commas() {
        let flour = list(&[("tarwebloem", true), ("water", false), ("zout", false)]);
        assert_eq!(flour.text(), "tarwebloem, water, zout");
        assert_eq!(IngredientList::default().text(), "");
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
    fn stored_lists_round_trip_and_mismatches_are_corrupt() {
        let flour = list(&[("tarwebloem", true), ("water", false)]);
        let (names, emphasis) = flour.to_db();
        assert_eq!(
            IngredientList::from_db(Some(names), Some(emphasis)).unwrap(),
            Some(flour)
        );
        assert_eq!(IngredientList::from_db(None, None).unwrap(), None);
        assert!(matches!(
            IngredientList::from_db(Some(vec!["water".to_owned()]), Some(vec![])),
            Err(Error::Corrupt(_))
        ));
        assert!(matches!(
            IngredientList::from_db(None, Some(vec![])),
            Err(Error::Corrupt(_))
        ));
    }
}
