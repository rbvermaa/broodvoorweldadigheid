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
| CSV uses the accountant format of ADR 0017, with `csv` in `production` | Same format staff already open in Excel |

**Seam for batches.** The spec says: before cutoff from recipe versions, after cutoff from
batch lines. Batches are built separately (F14a), so only the recipe-version path exists.
The seam is `production::needs::doughs`: it returns the doughs a day's needs are summed
from, each a `Dough` with its `DoughGrams`. The batch path plugs in there: after cutoff
it returns one `Dough` per batch, with `DoughGrams` from the batch lines, and products
from the batch outputs. `shopping_list`, the screen, PDF and CSV stay as they are. It
will need a clock to compare with the cutoff, and a second basis text next to
`needs-basis-recipes`. Until then the list uses recipe versions after cutoff too.
