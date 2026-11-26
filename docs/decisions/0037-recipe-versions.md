# 0037 Recipe versions, baker's percentages and scaling

Status: proposed, 2026-12-01

Amended 2026-11-26: the percentage base (all flour), the total-flour warning and rebasing
drafts on starter changes are superseded by ADR 0053: the base is a version's own flour
lines, the main dough, at 100%.

First slice of F14: ingredients, recipes with versions, sub-recipes, hydration and scaling,
with back-office screens under `/staff/ingredients` and `/staff/recipes`. Costing (F16),
the allergen roll-up (F17) and batches (F14a) come later.

| Choice | Why |
|---|---|
| Percentages are integers in thousandths of a percent (`recipes::Pct`, columns `*_pct_x1000`; 100% is 100000) | `rust_decimal` is not in the workspace, and SQLx without it cannot read `numeric`. 0.001% is finer than any scale in the bakery (0.1 g yeast in 10 kg flour is 0.001%) |
| Every line's percentage is relative to the total flour of the recipe it is in, flour inside sub-recipes included. A sub-recipe line at p% weighs p% of that flour; its own lines split that weight in their own percentages | The usual "overall formula" convention: a 1:1 levain at 20% brings 10% flour and 10% water |
| Total flour is shown and gives a warning when it is not 100%; drafts with other totals can still be saved | A baker who writes the main dough at 100% flour plus 20% levain sees "110%" and can fix it. Scaling and hydration do not need the total to be 100%: they work from the leaf ingredients |
| An ingredient's role is flour, water or other. Hydration is water ÷ flour over all leaves, sub-recipes included. Prefermented flour is flour inside a recipe of kind `preferment` | Milk, eggs or a mother starter as one ingredient count as "other"; model a starter as its own preferment recipe to have its flour and water counted. Water content per ingredient can come later |
| Recipe kinds: dough, preferment, soaker, filling, topping. The spec lists dough, filling and topping | Preferments are part of F14 and need to be recognisable for the prefermented-flour figure. A soaker is its own kind so it does not count as preferment |
| A sub-recipe line points at a recipe **version** (`sub_recipe_version_id`), not at a recipe as the spec's `sub_recipe_id` says | A frozen version must keep its formula. Pointing at a recipe would let a new levain version change every frozen dough that uses it |
| Freezing a version freezes every version it uses as sub-recipe, in one statement. `recipes::recipes::freeze` is the hook: staff call it with "Vastzetten", production calls it when the first batch uses a version (F14a) | Otherwise a frozen dough could still change through a draft levain inside it |
| Database triggers refuse any change to a frozen version and to the lines of a frozen version, also for writers outside the crate | Same approach as ADR 0006 |
| One draft per recipe (partial unique index). "Nieuwe versie" returns the draft if there is one, else copies the newest version into version n + 1. Saving a frozen version creates version n + 1 with the posted content; if a draft already exists, the save is refused with its number | Editing a frozen version creates the next version (spec), without a growing pile of drafts |
| A new version does not become the version in use by itself; "In gebruik nemen" sets `recipe.current_version_id`, which may be a draft | The baker decides when production switches. A draft in use freezes when a batch uses it |
| A line may not use a version of its own recipe directly (through another recipe is allowed, as long as no version uses itself). Sub-recipes may nest three levels deep (`MAX_NESTING`); a save is refused when the saved version, or any version that uses it, would go deeper. A loop shows up as nesting beyond the limit | Keeps loading bounded with one recursive query and catches loops without a separate check |
| `product.recipe_id` (spec) is set from the recipe page. The `recipes` crate writes that one column; catalog's product update does not touch it | The recipe page is where the link is made; catalog sits below recipes and does not know recipes |
| Lead time per version is stored and shown, information only | ADR 0027 |
| Ingredients carry name, name on the label, role, supplier and their own allergens. No may-contain, nutrition or NEVO fields | Owner: may-contain is one kitchen-wide list, nutrition values are dropped (small-producer exemption) |

**Rounding rule.** Scaling works in whole grams. Each level splits its integer weight
over its lines in proportion to their percentages with the largest-remainder method:
every line gets the floor of its exact share, then the leftover grams go one each to the
lines with the largest remainders, ties to the earlier line. A sub-recipe line then
splits its own whole grams the same way. So the ingredients always add up to the target
weight exactly, and an ingredient d levels deep is less than d + 1 grams from its exact
share (less than 1 g at the top level). Hydration, total flour and prefermented flour use
the same split on a reference weight of 10^12 g and are rounded half up to 0.001%.

**Allergens (update 2026-11-25).** Owner: clear a product's allergen approval when its
recipe changes, already before F17. Database triggers do it in the same transaction as the
change, whoever writes it, so the shop blocks (ADR 0038) until someone approves again:

| Change | Clears the approval of |
|---|---|
| `product.recipe_id` changes: link, unlink, another recipe | that product |
| `recipe.current_version_id` changes ("In gebruik nemen") | the products of that recipe |
| A line is added, changed or removed in a version | products whose recipe's version in use is that version or uses it as sub-recipe, at any depth |
| An ingredient's set of allergens changes, or its label name (an empty label name means the name) | products whose version in use holds the ingredient, directly or through sub-recipes |
| What a recipe shows on the ingredient list as sub-recipe changes: compound or merged, or the compound name (ADR 0046, amended 2026-11-25) | products whose version in use holds a version of that recipe as sub-recipe, at any depth |

Not cleared: renaming a recipe or changing its kind, notes or lead time, unless that changes what it shows on the ingredient list (row above); freezing; a new
draft that is not in use; an ingredient's supplier or role; its name when it has a label
name; versions that are not in use. A sub-recipe line points at a version, so taking
another version of a levain into use does not change a dough that uses it. Saving a draft
with the same lines does not rewrite them, so it does not clear anything. The label name
counts because it becomes the ingredient text in F17.

`product.allergens_cleared_reason` (`recipe` or `ingredient`) records why, and the staff
product page says so. Any approval resets it. It is a column, not an `audit_log` entry,
because a trigger would have to take the time from the database clock instead of the
injected one.

`product.allergens_recipe_rev` goes up with every change in the table, approved or not.
The approve page sends it back, and `catalog::products::approve_allergens` refuses an
approval for an older revision, so a page opened before a recipe change cannot approve
what nobody looked at again. The triggers lock the affected products in id order, as
checkout does (ADR 0038), and wait for an approval in progress. `set_current`,
`link_product` and `unlink_product` take the graph lock too, so a line or ingredient
change never reads a version in use or a recipe link that is about to change.

Linking a product to a recipe needs the EditAllergens permission as well as Production.

Open:

- Spec data model: `RecipeLine.sub_recipe_id` should read `sub_recipe_version_id`, and
  `Recipe.kind` should list preferment and soaker.
- `RecipeVersion.bake_loss_pct` is not stored; with nutrition dropped it is only useful
  for yield, which batches (F14a) can add.
- No delete or archive for ingredients and recipes yet.
- The weight field on a version starts at 1000 g, not at a linked product's dough weight.

**Promoting a batch (F14b, amended 2026-11-25).** "Opslaan als nieuwe receptversie" on a
batch page saves the batch's formula as the next version of its recipe, through
`recipes::recipes::promote`, with `promoted_from_batch_id` set.

| Choice | Why |
|---|---|
| The new version is a draft and does not become the version in use; the baker takes it into use | Same rule as any new version above. Taking it into use is where a recipe change reaches products (and their allergen approval) |
| Any batch status can be promoted, also before mixing | Owner, 2026-11-25 |
| Refused when the recipe already has a draft, with its number | Same rule as saving a frozen version: one draft per recipe |
| Promoting the same batch again returns the version made the first time, also after the batch changed since | One action, no pile of versions. The batch lock (`FOR UPDATE`) and the recipe lock serialise two clicks |
| A batch whose lines equal its version's lines is refused ("unchanged") | Nothing to save |
| Lead time comes from the version the batch was made from. Notes are that version's notes, then the batch code and base version, then the reasons for the line changes from the audit log, each once | The audit log keeps reasons a later save cleared from the lines (ADR 0042) |

**Recipe editor (F14, amended 2026-11-25).** Owner: the version page becomes one compact
table, grams | name | %, editable inline on a phone. The full-form edit with blank rows is
gone.

| Choice | Why |
|---|---|
| Every line edit (by % or grams, add, remove) keeps the grams per percent; the dough weight follows (`recipes::dough_for_sum`) | The other lines keep their grams, as a baker expects when changing the salt. Typed grams become a percentage at the current scale (`pct_for_grams`, half up to 0.001%); scaled back, the line is within half the grams of 0.001% plus 1.5 g of what was typed (property test) |
| One line per request: `recipes::edit_line` sets, adds or removes a line under the graph lock and refuses a frozen version (`Error::Frozen`). The page posts a stamp of the lines it showed (`recipes::recipes::stamp`); other lines refuse the change (`Error::Stale`) | A stale page, a second tab or two quick edits never change the wrong line or convert grams against another sum, also when an ingredient is in the recipe twice. Lines go through the same write and nesting check as a full save, so the allergen triggers fire as before |
| Lead time and notes save on their own (`save_details`) | Saving them no longer rewrites the lines |
| Lines inside a sub-recipe are shown, not edited | They belong to that recipe's version, which may be frozen; its own page edits it |
| The scale (`gewicht`, or `stuks` × a linked product's dough per piece) and the open sub-recipes live in the URL | Remembered per viewer without storing anything |
| A new ingredient is created and added in one transaction from the add row, with role and allergens; it needs EditAllergens as well as Production | Same rule as the ingredient pages (ADR 0010) |
| Hydration and total flour show in the table footer; warnings sit on their line | Owner: like the baking app on the phone |

**Starters with a hydration (F14, amended 2026-11-26).** Owner: a starter entered as one
ingredient should count in total flour and hydration, with its own hydration.

| Choice | Why |
|---|---|
| A fourth ingredient role, starter ("Desem/starter"), with `ingredient.starter_hydration_x1000`: grams of water per 100 g of flour, 0–500%, in thousandths like `Pct`. The database requires a hydration for a starter and none for other roles | The row in the table above that made a mother starter "other" left its flour and water out. A starter as its own preferment recipe still works |
| A starter line of G g holds `G · 100 / (100 + h)` g flour and the rest water. `IngredientRef::flour_and_water` is the one place that splits any ingredient line into flour and water; total flour, hydration and prefermented flour all add up its result, so the editor, scaling by pieces and batches agree | The same rounding as everything else: whole grams with the largest-remainder rule, so flour + water = G and each is less than 1 g off; analysis splits on the reference weight of 10^12 g. A 1:1 starter at p% gives the same figures as a 1:1 levain recipe at p% (property test) |
| Starter flour counts as prefermented flour | It is fermented flour, like flour in a preferment recipe |
| Ingredient needs (F15), costing (F16) and the allergen roll-up (F17) keep the starter as one ingredient | It is bought or kept and priced per kg, and its allergens are its own |
| Changing a starter's hydration follows the rule for role changes: refused while a frozen version uses the ingredient. Both are now also refused while a batch line holds it | A frozen version keeps its figures. A batch reads the role and hydration of its ingredients, and a baker can add any ingredient to a batch (F14a) |
| When a change involves a starter (to or from the role, or its hydration), every draft that holds the ingredient, directly or through sub-recipes, keeps its grams and its total flour: all its own percentages are multiplied by old flour ÷ new flour (`Formula::rebased`), half up to 0.001% and at least 0.001%. All new percentages are worked out before any draft is written. A draft whose new percentages go above 1000% keeps its own and shows the total-flour warning | The baker weighed grams; the percentages follow the new flour base. Rewriting the lines clears the allergen approval of products whose version in use is that draft, like any line change |
| Other role changes (flour, water, other) keep the percentages as before | Unchanged behaviour |
| The starter's row shows "Desem 100%" (a missing hydration is a warning on the row), a sub-recipe row its own hydration, and the footer "Waarvan uit desem: x g bloem en y g water" | Compact, on the phone too |
