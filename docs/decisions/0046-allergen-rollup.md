# 0046 Allergen roll-up: computed values, approval and batch label block

Status: proposed, 2026-11-25

F17. Allergens and ingredient text are computed from the recipe and approved by the
baker. A batch that differs is re-checked and cannot be labelled until the difference is
approved. Products without a recipe keep their hand-entered values (F10).

**Ingredient text rule.** `recipes::rollup::compute`, from a recipe version or a batch's
own lines:

| Choice | Why |
|---|---|
| Sub-recipe ingredients are merged into the dough, not listed as a compound ingredient with its components in brackets | The spec does not say. EU 1169/2011 allows both; for a levain of flour and water, listing "tarwebloem" once at its full weight is what bakeries print and reads best |
| Order: descending weight in the final dough, sub-recipes included. Equal weights keep the order in which the recipe first uses them | EU 1169/2011 art. 18. Weights are exact fractions of the dough on one common denominator, so equal weights through a sub-recipe stay a tie. When that does not fit in 128 bits, whole units of a 10^12 g dough (ADR 0037 rounding) decide |
| Each entry uses the ingredient's label name (its name when empty). Label names that differ only in case are one entry with their weights added, under the spelling the recipe uses first | Two mills of "tarwebloem" are one ingredient on a label |
| A line at 0% is left out, with everything below it, before anything is weighed | That is how a batch swaps an ingredient (ADR 0042) |
| An entry is marked when its ingredients hold any of the 14 allergens; labels print marked entries in bold. The allergens are the union over every ingredient left in, at any depth | Spec compliance table. The list stores the structure (`catalog::IngredientList`: names and marks), never markup |
| Water is listed like any other ingredient | Simplest; the 5% exemption for added water is optional |

**Product approval.** The product's approve page shows the computed values next to the
current ones. "Berekende waarden goedkeuren" posts the values as shown plus the recipe
revision; `recipes::rollup::approve_product` takes the recipe lock, computes again, refuses
when they differ from what was shown or the revision moved (409 with the new values), and
then `catalog::products::approve_computed` stores allergens, ingredient text (the names
joined with commas) and the list (`product.ingredient_names`, `ingredient_emphasis`) and
approves them. Nothing is approved without that explicit post. A trigger clears the
approval when the stored list changes, and a recipe change (new revision or another recipe) drops the list, so the hand approval cannot bring back a list the recipe no longer computes. Saving the product form with other allergens or
ingredient text drops the list, so the product is back to hand-entered values. The
existing hand approval and its revision check are unchanged.

**Batch label block.** `production::label_check`:

| Choice | Why |
|---|---|
| Every batch is checked, not only those that swap or add an ingredient: its values are computed from its own lines and compared with the product's approved computed values | A change of quantity alone can change the order by weight, and the spec warns about that. A batch as in the recipe compares equal and needs nothing |
| A difference in allergens, in the ingredients, or only in their order by weight blocks the label | Spec: labels blocked until any difference is approved; CLAUDE.md: when in doubt, block. The order warning (F18) is still reported once approved |
| Approval is per batch: `batch.allergens`, `ingredient_names`, `ingredient_emphasis`, `allergens_approved_by/at`, set by "Waarden van deze batch goedkeuren" on `…/batches/{batch}/etiket/{product}` (EditAllergens) | The spec's data model gives the batch its own allergens and approval. Approving a new product state instead would change the shop and every other batch for a one-off swap |
| A batch approval counts only while the batch still computes exactly the approved values; nothing clears it | A later adjustment or ingredient change makes the values differ and blocks again, without triggers that could miss a path. Approving after mixing is allowed, since labels are printed after baking |
| A product whose allergen data was entered by hand covers no batch label: every batch needs the computed values approved on the product or on the batch | The hand text cannot be compared with a computed list |
| Labels also need the product's own approval and the kitchen-wide list's approval | Spec: labelled only while both are approved (ADR 0038) |

`label_allowed(conn, batch, product) -> Result<LabelContent, production::Error>` is the
seam for F18 and F18a. `Ok` carries allergens, the kitchen may-contain list without them,
the ingredient list, whose approval covers it and `order_changed` (warn before printing).
A block is `Error::LabelBlocked(LabelBlock)`: kitchen list or product not approved, batch
cannot be weighed, or `Differs { computed, approved, difference }`. `Error::NotFound` when
the batch does not make the product. Each batch approval writes an audit entry.

**Follow-up (2026-11-25).**

| Choice | Why |
|---|---|
| A product made from a recipe takes only computed values: `approve_allergens` refuses it (`Error::ComputedOnly`, checked in the same UPDATE as the approval) and the approve page hides the hand form. Products without a recipe keep the hand approval | The hand path could re-approve allergens the recipe no longer computes |
| The shop tiles and the pickup list show the ingredient list from the stored structure, allergen entries in bold (`components/product.html` `ingredient_text`). Hand-entered text shows as it was typed | Spec compliance: allergens emphasised in the ingredient list. The pickup list shows each product on it once, with the same approved data as the shop |
| The pickup list also warns per product about each batch of the day whose computed values differ from the product's approved ones, and shows that batch's values | The counter must not hand out bread with an allergen the product declaration lacks |
| A migration takes away the hand approval of products that have a recipe but no stored list | Otherwise hand-typed data would stay published for them |

Open:

- The pickup list PDF does not show allergens yet.
- The shop shows the product's approved data, not a day's differing batch (docs/open-questions.md).
- Whether the order by weight should block or only warn (docs/open-questions.md).
