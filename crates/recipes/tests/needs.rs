#![forbid(unsafe_code)]

use domain::{IngredientId, RecipeId, RecipeVersionId};
use proptest::prelude::*;
use recipes::{
    DoughGrams, Formula, IngredientRef, IngredientRole, Line, Part, Pct, RecipeKind, ScaleError,
    SubRecipe, shopping_list,
};

fn pct(percent: u32) -> Pct {
    Pct::from_thousandths(percent * 1000)
}

fn ingredient(id: i64, name: &str, role: IngredientRole) -> IngredientRef {
    IngredientRef {
        id: IngredientId::new(id),
        name: name.to_owned(),
        role,
        hydration: None,
    }
}

fn leaf(id: i64, name: &str, role: IngredientRole, percent: u32) -> Line {
    Line {
        pct: pct(percent),
        part: Part::Ingredient(ingredient(id, name, role)),
    }
}

fn flour(percent: u32) -> Line {
    leaf(1, "Tarwebloem", IngredientRole::Flour, percent)
}

fn water(percent: u32) -> Line {
    leaf(2, "Water", IngredientRole::Water, percent)
}

fn salt(percent: u32) -> Line {
    leaf(3, "zeezout", IngredientRole::Other, percent)
}

fn rye(percent: u32) -> Line {
    leaf(4, "Roggemeel", IngredientRole::Flour, percent)
}

fn levain(percent: u32) -> Line {
    Line {
        pct: pct(percent),
        part: Part::Sub(SubRecipe {
            version_id: RecipeVersionId::new(10),
            recipe_id: RecipeId::new(10),
            name: "Levain".to_owned(),
            version: 2,
            kind: RecipeKind::Preferment,
            formula: Formula {
                lines: vec![rye(100), water(100)],
            },
        }),
    }
}

fn names_and_grams(list: &[recipes::Need]) -> Vec<(String, u64)> {
    list.iter()
        .map(|need| (need.ingredient.name.clone(), need.grams))
        .collect()
}

#[test]
fn sub_recipes_resolve_down_to_their_ingredients() {
    let dough = Formula {
        lines: vec![flour(90), water(65), salt(2), levain(20)],
    };
    let grams = DoughGrams::from_formula(&dough, 17_700).unwrap();
    assert_eq!(grams.total(), 17_700);
    let list = shopping_list([&grams]);
    assert_eq!(
        names_and_grams(&list),
        vec![
            ("Roggemeel".to_owned(), 1000),
            ("Tarwebloem".to_owned(), 9000),
            ("Water".to_owned(), 7500),
            ("zeezout".to_owned(), 200),
        ]
    );
}

#[test]
fn doughs_add_up_per_ingredient_and_keep_their_parts() {
    let desem = Formula {
        lines: vec![flour(90), water(65), salt(2), levain(20)],
    };
    let rogge = Formula {
        lines: vec![rye(100), water(80), salt(2)],
    };
    let first = DoughGrams::from_formula(&desem, 1770).unwrap();
    let second = DoughGrams::from_formula(&rogge, 1820).unwrap();
    let list = shopping_list([&first, &second]);
    let water = list.iter().find(|n| n.ingredient.name == "Water").unwrap();
    assert_eq!(water.parts, vec![(0, 750), (1, 800)]);
    assert_eq!(water.grams, 1550);
    let rye = list
        .iter()
        .find(|n| n.ingredient.name == "Roggemeel")
        .unwrap();
    assert_eq!(rye.parts, vec![(0, 100), (1, 1000)]);
    let total: u64 = list.iter().map(|n| n.grams).sum();
    assert_eq!(total, 1770 + 1820);
}

#[test]
fn an_empty_recipe_cannot_give_grams() {
    let empty = Formula { lines: vec![] };
    assert_eq!(
        DoughGrams::from_formula(&empty, 900),
        Err(ScaleError::Empty(None))
    );
    assert_eq!(shopping_list(std::iter::empty::<&DoughGrams>()), vec![]);
}

#[test]
fn the_list_is_by_name_whatever_the_case() {
    let grams = DoughGrams {
        ingredients: vec![
            (ingredient(7, "boter", IngredientRole::Other), 5),
            (ingredient(8, "Appel", IngredientRole::Other), 3),
            (ingredient(6, "boter", IngredientRole::Other), 2),
        ],
    };
    let ids: Vec<i64> = shopping_list([&grams])
        .iter()
        .map(|n| n.ingredient.id.get())
        .collect();
    assert_eq!(ids, vec![8, 6, 7]);
}

fn any_leaf() -> impl Strategy<Value = Line> {
    (1u32..=200_000, 0usize..3, 1i64..8).prop_map(|(thousandths, role, id)| Line {
        pct: Pct::from_thousandths(thousandths),
        part: Part::Ingredient(IngredientRef {
            id: IngredientId::new(id),
            name: format!("Ingrediënt {id}"),
            role: IngredientRole::ALL[role],
            hydration: None,
        }),
    })
}

fn any_formula() -> impl Strategy<Value = Formula> {
    let line = any_leaf().prop_recursive(3, 40, 5, |inner| {
        (1u32..=200_000, prop::collection::vec(inner, 1..5)).prop_map(|(thousandths, lines)| Line {
            pct: Pct::from_thousandths(thousandths),
            part: Part::Sub(SubRecipe {
                version_id: RecipeVersionId::new(99),
                recipe_id: RecipeId::new(99),
                name: "Sub".to_owned(),
                version: 1,
                kind: RecipeKind::Preferment,
                formula: Formula { lines },
            }),
        })
    });
    prop::collection::vec(line, 1..6).prop_map(|lines| Formula { lines })
}

proptest! {
    #[test]
    fn every_gram_of_every_dough_is_on_the_list_once(
        doughs in prop::collection::vec((any_formula(), 0u64..=500_000), 0..6),
    ) {
        let grams: Vec<DoughGrams> = doughs
            .iter()
            .map(|(formula, grams)| DoughGrams::from_formula(formula, *grams).unwrap())
            .collect();
        let list = shopping_list(&grams);
        let target: u64 = doughs.iter().map(|(_, grams)| grams).sum();
        prop_assert_eq!(list.iter().map(|n| n.grams).sum::<u64>(), target);
        for need in &list {
            prop_assert_eq!(need.parts.iter().map(|(_, g)| g).sum::<u64>(), need.grams);
        }
        for (index, dough) in grams.iter().enumerate() {
            let on_list: u64 = list
                .iter()
                .flat_map(|n| &n.parts)
                .filter(|(i, _)| *i == index)
                .map(|(_, g)| g)
                .sum();
            prop_assert_eq!(on_list, dough.total());
        }
    }
}
