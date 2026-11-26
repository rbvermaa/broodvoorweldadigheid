# 0053 Percentages relative to the main dough's flour

Status: accepted, 2026-11-26

Supersedes the percentage base in ADR 0037. Owner decision (F14): work like the baking
app on the phone. The flour lines directly in a recipe version together are 100%; every
other line is relative to them.

| Choice | Why |
|---|---|
| A version's own flour lines (role flour, not inside a starter or sub-recipe) are the base: together 100%. Every line, also a starter or a sub-recipe line, is a percentage of that flour. Sub-recipes follow the same rule for their own lines | The owner enters recipes this way: main dough 100%, then "Desem 20%". The old base, all flour including starters and sub-recipes, made the main flour 90% or 83.3% |
| No flour lines of its own (a dough made from a levain only, a starter mix): all its flour, from starters and sub-recipes, is the base, as in ADR 0037 | Something has to be 100%. That flour is what the baker would weigh |
| No flour at all (a filling, a topping): the percentages are shares of the weight; no 100% check | Nothing to compare with; the grams only depend on the proportions |
| Hydration stays all water ÷ all flour, starters and sub-recipes included. Prefermented flour stays starter and preferment flour ÷ all flour | Unchanged; those are about the whole dough |
| The footer shows "Bloem hoofddeeg" (grams, its %), "Totaal bloem incl. desem/voordeeg" (grams, % in the same units, for example 113,3%) when it differs, hydration and dough. The warning is on the base: main flour, or all flour without own flour lines, when it does not show as 100% | The base is what the baker types against |
| Percentages show with at most one decimal everywhere on screens and PDFs, rounded half up, without a trailing ",0" (`Pct::tenths`, `form::pct_text`). Inputs keep the stored thousandths | Owner: "een cijfer achter de komma is prima". Editing must not lose data |
| A starter change (role to or from starter, or its hydration) no longer rebases a draft that has flour lines of its own: its base did not move. A draft without own flour lines still keeps its grams and total flour (`Formula::rebased`) | Rebasing would rescale every line of a draft whose base is unchanged. A flour line that becomes a starter is like any other role change and keeps the numbers the baker typed |

**Rounding.** Scaling is unchanged: a weight splits over the lines in proportion to their
percentages (ADR 0037). Multiplying all of a version's lines by one factor leaves its
grams the same, so the base only changes what the numbers mean, not what goes in the bowl.

**Existing data.** Migration `20261208000400_main_dough_pct.sql`, in SQL like the earlier
data changes:

| Data | What happens |
|---|---|
| Frozen versions whose own flour is less than all their flour (a starter or sub-recipe with flour) | Every line × total flour ÷ main flour, half up to 0.001%. The grams at any weight stay within the rounding bound (property test: less than weight · (n + 1) / (2 · S′) plus 2 · (depth + 1) grams per leaf, n lines summing to S′ thousandths; within 1 g in the db test). The main flour becomes the old total flour, so a consistent version shows 100% |
| Batches of those versions | Base and actual percentages × the same factor, so the base still equals the version's lines and the batch grams stay the same; dough weight per batch is stored separately and does not change |
| Frozen versions without own flour, with all their flour in own lines, or without flour | Untouched |
| A version where a line would go above 1000% | Untouched; it shows the main-flour warning |
| Drafts | Untouched. The owner typed them in the new convention. Their grams at a dough weight do not change either; only the footer reads differently |

The migration switches off the frozen-version, mixed-batch and allergen-approval
triggers for its two updates only: the grams do not change, so no approval is cleared.
It runs once, like every migration (`_sqlx_migrations`); a db test checks grams before
and after, untouched drafts, and that running the migrations again changes nothing.
