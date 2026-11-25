# 0041 Ingredient needs per bake day

Status: proposed, 2026-11-25

F15: the ingredient totals of a bake day as a shopping list, on screen under
`/staff/bake-days/{id}/ingredients` (Production permission), as PDF and as CSV.

**What counts.** The same pieces as the production list (ADR 0016): paid, confirmed and
picked-up orders plus all extra production (F7a), read in the same snapshot.

**From pieces to grams.** Products are grouped by the recipe version their recipe has in
use (`recipe.current_version_id`). Each version is scaled once, to the sum of pieces ×
`dough_g_per_piece` of its products, with the rounding rule of ADR 0037, and its
sub-recipes are resolved down to ingredients. The ingredients of all doughs are then
added up per ingredient. Adding whole grams needs no further rounding, so a total is
exactly the sum of its parts, and all totals together are exactly the day's dough weight.

| Choice | Why |
|---|---|
| Scale per recipe version, not per product | One rounding per dough, as the baker mixes it; batches (F14a) also sum per dough |
| Weights shown as `850 g` below 1 kg and `12,345 kg` from there; the CSV has whole grams | Exact to the gram, so the sums on paper add up; no rounding to explain |
| Products whose grams cannot be computed are listed under "Geen recept" with their pieces and why: no recipe, no version in use, a recipe (or sub-recipe) without lines, or 0 g dough per piece | Never drop pieces silently. The screen also shows a warning |
| The pure part lives in `recipes::needs` (`DoughGrams`, `shopping_list`); reading pieces and grouping lives in `production::needs` | The spec puts ingredient needs in `recipes`, and only `production` sees both orders and recipes |
| CSV uses the accountant format of ADR 0017, with `csv` in `production`; products left out follow the ingredients as "Niet meegeteld (…)" rows with an empty gram cell | Same format staff already open in Excel, and the CSV never looks complete when it is not |

**After the deadline: batches.** The spec says: before cutoff from recipe versions, after
cutoff from batch lines. `production::needs::doughs` takes the injected clock. Once the
day's order deadline has passed and the day has batches (F14a, ADR 0042), it returns one
`Dough` per batch from `production::batches::formulas_for_day`: the batch's recipe and
version, its outputs as products, and `DoughGrams::from_formula(&formula, dough_g)`, so
the baker's adjusted percentages count. Pieces still in no batch (to bake minus planned
pieces in batches, per product) are added from the recipe version in use, so nothing is
dropped. Before the deadline, or without batches, it is the recipe path above.
`IngredientNeeds.basis` says which; the screen and PDF show `needs-basis-recipes` or
`needs-basis-batches`, and a batch dough is named with its code (`needs-dough-batch`).
`shopping_list` and the CSV stay as they are.
