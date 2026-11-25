#![forbid(unsafe_code)]
//! Ingredients and versioned recipes in baker's percentages (F14).

mod error;
pub mod formula;
pub mod ingredients;
pub mod recipes;

pub use error::Error;
pub use formula::{
    Analysis, Formula, IngredientRef, IngredientRole, Line, Part, Pct, RecipeKind, ScaleError,
    Scaled, SubRecipe, ingredient_totals,
};
pub use ingredients::{Ingredient, IngredientInput};
pub use recipes::{
    DraftInput, LineInput, LinePart, LineRow, LinkedProduct, Recipe, RecipeInput, RecipeSummary,
    SubChoice, Version,
};

/// How many levels of sub-recipes a recipe may have below it.
pub const MAX_NESTING: usize = 3;
