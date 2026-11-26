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
| Drafts whose all flour shows as 100% but whose own flour does not (copied by "Nieuwe versie" or promoted from an old-way version) | Rescaled like a frozen version: they hold old-way numbers |
| Other drafts, like the owner's Landbrood (own flours 100% plus a starter) | Untouched: typed the new way. Their grams at a dough weight do not change either; only the footer reads differently |

The migration switches off the frozen-version, mixed-batch and allergen-approval
triggers for its two updates only. Rounding to 0.001% can make two equal weights unequal
and so reorder an ingredient list. The migration works out each list's order before and
after, the way `recipes::rollup` does, with exact weights: a product whose recipe's list
changes order loses its allergen approval (reason "recipe"), a batch whose own approved
list changes loses that approval. Every other approval stays: its list is the same.
Weights are summed exactly and rounded to 38 decimals only to compare. Known gap: the
database lowercases only ASCII, so two label names that differ only in the case of a
non-ASCII letter count as two entries there; a missed reorder still blocks labels,
because the label check compares the computed list with the approved one.

The SQL is not idempotent by itself (a second run would rescale again); like every
migration it runs once, recorded in `_sqlx_migrations`. A db test covers grams before and
after (an even and an uneven factor, a migrated preferment inside a migrated dough, a
batch line at 0%, a mixed batch), the 1000% cap, drafts of both kinds, the approval that
goes and the one that stays, the triggers being back on, and a second `migrate`.

Not rewritten: `audit_log` entries of earlier batch adjustments keep their percentages in
the old units. Nothing reads them back today.
