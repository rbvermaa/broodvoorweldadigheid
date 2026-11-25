# 0042 Batches: split, code, lock and actuals

Status: proposed, 2026-11-25

F14a: batches per bake day in `production::batches`, the split in `production::split`,
the deadline run in `jobs::batches`, and back-office pages under
`/staff/bake-days/{id}/batches`.

| Choice | Why |
|---|---|
| Mixer capacity is one company setting, `company.mixer_capacity_g` (grams of dough, NULL until set), edited on the batches page. No `Mixer` table and no `batch.mixer_id` yet | The bakery has one mixer and the owner has not given its size. No default is invented: while it is empty the deadline split does nothing, the page says so, and only manual batches work. A `Mixer` table can come later, additively |
| A batch holds whole pieces. Its dough weight is planned pieces × dough per piece (copied into `batch_output` at creation); `Batch.flour_g` from the spec is not stored | Actual pieces and waste need planned pieces per product per batch. Flour and every line in grams follow from the dough weight with the rounding rule of ADR 0037, so grams are never stored |
| **Split rule.** Per dough (the version in use of each product's recipe), take the fewest batches for which dealing fits: pieces go out heaviest first, each to the batch with the least dough so far, the earlier batch on a tie. No batch exceeds the capacity, batches of a dough add up to its dough exactly, and two batches differ by at most one piece's dough. One product: `ceil(pieces / floor(capacity / piece))` batches whose counts differ by at most one | Simple, balanced, provable (property tests). Whole pieces can need one batch more than dough ÷ capacity |
| The split only plans pieces not yet in one of the day's batches of that version. "Maak batches" runs it any time on a day that is not cancelled or done; running it again adds batches only for new pieces | Manual batches and later extras are not counted twice |
| The jobs tick splits each closed day once after its deadline (`batch_split` row, inserted in the same transaction), only while the bake date is today or later, and only when a capacity is set. A refused split records nothing and is tried next tick | Setting the capacity later then still fills the coming days but not old ones |
| A manual batch is any number of pieces of one product with a recipe; it may exceed the capacity and works without one | Spec: "start a batch by hand for any quantity" |
| **Batch code** `YYMMDD-N`: bake date and a number per date, e.g. `261205-2`. Stored as `code_date` + `mix_no` (unique together), never recalculated | Short, readable and printable for labels (F18, F18a). Two bake days can share a date, so numbering is per date; moving a bake day does not change a printed code |
| A batch copies the top-level lines of the version, sub-recipe lines pointing at their version (`base_pct` and `actual_pct`), and calls `recipes::recipes::freeze` first | ADR 0037 hook. Sub-recipe content comes from its frozen version, so the copy cannot change |
| Adjusting: change percentages and planned pieces, add an ingredient line (`base_pct` NULL). 0% leaves a line out; lines are never deleted, so "changed against the base" stays visible. Swapping an ingredient is 0% on the old line plus a new line. One optional reason per save, stored on the changed lines and in the audit log (`entity = 'batch'`) with before and after | Spec: "adjusts lines and quantities with an optional reason" |
| Grams recalculate on the server: the adjust form posts to `…/preview` on input (htmx, 400 ms delay) and swaps `#grams`; nothing is saved | No JavaScript beyond htmx |
| Mixed locks the batch. Database triggers refuse any change to lines and planned pieces of a mixed batch, a status going back, and a change of version or code; actual pieces are refused before mixing | Same approach as ADR 0006 and 0037; holds for writers outside the crate too |
| Actuals: pieces out of the oven and waste among them, per product, after mixing; the batch becomes baked. Recording again corrects the numbers (audited); `baked_at` keeps its first time | Spec: "actual pieces and waste are recorded after baking" |
| Every batch screen needs the Production permission (owner and baker); counter staff get 403 | ADR 0010 |
| `production::batches::formulas_for_day(conn, day) -> Vec<BatchFormula>` gives per batch its recipe, version, outputs, dough weight and own formula | The seam for ingredient needs after the deadline (F15): one F15 dough per batch, grams from `DoughGrams::from_formula(&formula, dough_g)` |

Open:

- F7a says extras can change "until the batch is mixed". Extras are not yet refused after
  mixing, and a change after the split does not resize batches; "Maak batches" adds the
  extra pieces, fewer pieces need a manual adjustment.
- No delete for a planned batch; set its pieces to 0 instead.
- The batch's own allergen roll-up and label block (F17) and frozen pieces (F18a) come
  with those slices.
- `RecipeVersion.bake_loss_pct` is still not stored; yield from actuals comes with costing
  (F16).
