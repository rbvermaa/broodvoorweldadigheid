# 0037 Recipe versions, baker's percentages and scaling

Status: proposed, 2026-12-01

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

Not cleared: renaming a recipe or changing its kind, notes or lead time; freezing; a new
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
| Refused when the recipe already has a draft, with its number | Same rule as saving a frozen version: one draft per recipe |
| Promoting the same batch again returns the version made the first time, also after the batch changed since | One action, no pile of versions. The batch lock (`FOR UPDATE`) and the recipe lock serialise two clicks |
| A batch whose lines equal its version's lines is refused ("unchanged") | Nothing to save |
| Lead time comes from the version the batch was made from. Notes are that version's notes, then the batch code and base version, then the reasons for the line changes from the audit log, each once | The audit log keeps reasons a later save cleared from the lines (ADR 0042) |
