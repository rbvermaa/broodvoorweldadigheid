# 0042 Batches: by hand, code, lock and actuals

Status: proposed, 2026-11-25

F14a: batches per bake day in `production::batches` and back-office pages under
`/staff/bake-days/{id}/batches`.

| Choice | Why |
|---|---|
| Batches are only started by hand, for any quantity. No mixer capacity, no `Mixer` table and no automatic split | Owner decision 2026-11-25: the baker manages mixer capacity |
| A batch holds whole pieces. Its dough weight is planned pieces × dough per piece (copied into `batch_output` at creation); `Batch.flour_g` from the spec is not stored | Actual pieces and waste need planned pieces per product per batch. Flour and every line in grams follow from the dough weight with the rounding rule of ADR 0037, so grams are never stored |
| The batches page lists, per product, the pieces to bake (firm orders plus extra production) that are in none of the day's batches, whatever version those use | The baker sees what still needs a batch |
| A batch is any number of pieces of one product with a recipe version in use and dough per piece | Spec: "start a batch by hand for any quantity" |
| **Batch code** `YYMMDD-N`: bake date and a number per date, e.g. `261205-2`. Stored as `code_date` + `mix_no` (unique together), never recalculated | Short, readable and printable for labels (F18, F18a). Two bake days can share a date, so numbering is per date; moving a bake day does not change a printed code |
| A batch copies the top-level lines of the version, sub-recipe lines pointing at their version (`base_pct` and `actual_pct`), and calls `recipes::recipes::freeze` first | ADR 0037 hook. Sub-recipe content comes from its frozen version, so the copy cannot change |
| Adjusting: change percentages and planned pieces, add an ingredient line (`base_pct` NULL). 0% leaves a line out; lines are never deleted, so "changed against the base" stays visible. Swapping an ingredient is 0% on the old line plus a new line. One optional reason per save, stored on the changed lines and in the audit log (`entity = 'batch'`) with before and after | Spec: "adjusts lines and quantities with an optional reason" |
| Grams recalculate on the server: the adjust form posts to `…/preview` on input (htmx, 400 ms delay) and swaps `#grams`; nothing is saved | No JavaScript beyond htmx |
| Mixed locks the batch. Database triggers refuse any change to lines and planned pieces of a mixed batch, a status going back, and a change of version or code; actual pieces are refused before mixing | Same approach as ADR 0006 and 0037; holds for writers outside the crate too |
| Actuals: pieces out of the oven and waste among them, per product, after mixing; the batch becomes baked. Recording again corrects the numbers (audited); `baked_at` keeps its first time | Spec: "actual pieces and waste are recorded after baking" |
| Every batch screen needs the Production permission (owner and baker); counter staff get 403 | ADR 0010 |
| The product form gets "Deeg per stuk (gram)" (`product.dough_g_per_piece`); a post without the field keeps the stored value | Batches need it, and no screen could set it before |
| `production::batches::formulas_for_day(conn, day) -> Vec<BatchFormula>` gives per batch its recipe, version, outputs, dough weight and own formula | The seam for ingredient needs after the deadline (F15): one F15 dough per batch, grams from `DoughGrams::from_formula(&formula, dough_g)` |
| Extra production added or changed after batches exist does not resize them: more pieces show as in no batch and get a batch by hand, fewer pieces are adjusted by hand | Owner decision 2026-11-25 (F7a) |

Open:

- F7a says extras can change "until the batch is mixed". Extras are not yet refused after
  mixing.
- No delete for a planned batch; set its pieces to 0 instead.
- A later save without a reason clears the stored reason on the lines it changes; the audit
  log keeps every reason.
- Marking a batch mixed is refused on a cancelled or done day, so actuals for an unmixed
  batch on a done day cannot be recorded.
- An adjustment that sets every line to 0% is refused.
- A version whose sub-recipe has no lines passes the check for lines and gets batches
  that cannot be weighed (the page says so).
- Products that can never get a batch (no recipe) stay listed as in no batch.
- The batch's own allergen roll-up and label block (F17) and frozen pieces (F18a) come
  with those slices.
- `RecipeVersion.bake_loss_pct` is still not stored; yield from actuals comes with costing
  (F16).
