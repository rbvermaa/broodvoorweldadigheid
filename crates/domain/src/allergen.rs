use std::str::FromStr;

/// The 14 allergens of EU Regulation 1169/2011, Annex II, in Annex order.
///
/// [`Allergen::code`] values are stored in the database and must never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Allergen {
    CerealsContainingGluten,
    Crustaceans,
    Eggs,
    Fish,
    Peanuts,
    Soybeans,
    Milk,
    Nuts,
    Celery,
    Mustard,
    SesameSeeds,
    SulphurDioxideAndSulphites,
    Lupin,
    Molluscs,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown allergen code: {0}")]
pub struct UnknownAllergen(pub String);

impl Allergen {
    pub const ALL: [Self; 14] = [
        Self::CerealsContainingGluten,
        Self::Crustaceans,
        Self::Eggs,
        Self::Fish,
        Self::Peanuts,
        Self::Soybeans,
        Self::Milk,
        Self::Nuts,
        Self::Celery,
        Self::Mustard,
        Self::SesameSeeds,
        Self::SulphurDioxideAndSulphites,
        Self::Lupin,
        Self::Molluscs,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::CerealsContainingGluten => "cereals_containing_gluten",
            Self::Crustaceans => "crustaceans",
            Self::Eggs => "eggs",
            Self::Fish => "fish",
            Self::Peanuts => "peanuts",
            Self::Soybeans => "soybeans",
            Self::Milk => "milk",
            Self::Nuts => "nuts",
            Self::Celery => "celery",
            Self::Mustard => "mustard",
            Self::SesameSeeds => "sesame_seeds",
            Self::SulphurDioxideAndSulphites => "sulphur_dioxide_and_sulphites",
            Self::Lupin => "lupin",
            Self::Molluscs => "molluscs",
        }
    }
}

impl FromStr for Allergen {
    type Err = UnknownAllergen;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|allergen| allergen.code() == code)
            .ok_or_else(|| UnknownAllergen(code.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODES: [&str; 14] = [
        "cereals_containing_gluten",
        "crustaceans",
        "eggs",
        "fish",
        "peanuts",
        "soybeans",
        "milk",
        "nuts",
        "celery",
        "mustard",
        "sesame_seeds",
        "sulphur_dioxide_and_sulphites",
        "lupin",
        "molluscs",
    ];

    #[test]
    fn fourteen_allergens_in_annex_ii_order_with_stable_codes() {
        let codes: Vec<&str> = Allergen::ALL.iter().map(|a| a.code()).collect();
        assert_eq!(codes, CODES);
    }

    #[test]
    fn codes_parse_back() {
        for allergen in Allergen::ALL {
            assert_eq!(allergen.code().parse::<Allergen>(), Ok(allergen));
        }
    }

    #[test]
    fn unknown_code_is_refused() {
        assert_eq!(
            "gluten".parse::<Allergen>(),
            Err(UnknownAllergen("gluten".to_owned()))
        );
        assert_eq!(
            UnknownAllergen("x".to_owned()).to_string(),
            "unknown allergen code: x"
        );
    }
}
