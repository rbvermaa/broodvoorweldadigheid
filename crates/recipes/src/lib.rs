#![forbid(unsafe_code)]
//! Ingredients and versioned recipes in baker's percentages (F14), ingredient needs per
//! bake day (F15), and ingredient prices and standard cost (F16).

pub mod cost;
mod error;
pub mod formula;
pub mod ingredients;
pub mod needs;
pub mod prices;
pub mod recipes;

pub use error::Error;
pub use formula::{
    Analysis, Formula, IngredientRef, IngredientRole, Line, Part, Pct, RecipeKind, ScaleError,
    Scaled, SubRecipe, ingredient_totals,
};
pub use ingredients::{Ingredient, IngredientInput};
pub use needs::{DoughGrams, Need, ProductRecipe, RecipeInUse, shopping_list};
pub use recipes::{
    DraftInput, LineInput, LinePart, LineRow, LinkedProduct, Promotion, Recipe, RecipeInput,
    RecipeSummary, SubChoice, Version,
};

/// How many levels of sub-recipes a recipe may have below it.
pub const MAX_NESTING: usize = 3;
