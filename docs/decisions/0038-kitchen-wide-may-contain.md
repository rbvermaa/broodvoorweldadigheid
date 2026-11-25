# 0038 Kitchen-wide may-contain list

Status: proposed, 2026-11-25

F10, F17. The owner (2026-11-25): "may contain" is about ingredients used in the kitchen,
not in the recipe, so it is one list for the whole kitchen instead of one per product.

| Choice | Why |
|---|---|
| One list of allergens (a subset of the 14) in `company.may_contain`, with its own `may_contain_approved_by` (a staff user) and `may_contain_approved_at`. A trigger clears the approval when the set of allergens changes, whoever writes it; the same set in another order or with duplicates keeps it | Same rule as product allergens (ADR 0006): nothing reaches a customer unapproved, and no code path can forget to clear it |
| A product shows what it contains, plus the kitchen list minus what it already contains (`catalog::PublishedAllergens`) | "Bevat melk" and "kan sporen bevatten van melk" side by side would be confusing. Shop tiles, cart and later labels use the same function |
| Showing allergens, adding to the cart and checkout need both the product's approval and the kitchen list's approval (`Product::allergens_approved`). Checkout locks its product rows by id and then the company row `FOR SHARE` (ADR 0008, update 2026-11-25), reads both approvals after those locks and refuses with `CartError::KitchenAllergensNotApproved`; the shop shows the usual "allergeneninformatie volgt" and "niet beschikbaar" | ADR 0012's "block when in doubt": without an approved list we cannot tell the customer what the bread may contain. The locks make a staff change that clears an approval wait for a running checkout, so no order is placed against a revoked approval |
| The list starts empty and unapproved. The staff home and the product list show a warning with a link until the owner or baker approves it; until then nothing can be ordered online | An empty list is a statement too ("geen sporen"), so someone has to approve it once. The warning makes the block visible right after deploying |
| Owner and baker edit and approve it on `/staff/products/may-contain` (`Permission::EditAllergens`). Approving sends the shown list back in hidden fields; a list that changed in the meantime gets a 409 and the new list | Same flow as the product approve page |
| Product approval now covers allergens and ingredient text only; the form and approve page no longer show may-contain. `product.may_contain` stays in the schema but is not read or written. Its old trigger clause stays too | Migrations only add (CLAUDE.md). The column and the trigger clause go in a later release |

Follow-ups:

- Drop `product.may_contain` and its clause in `clear_allergen_approval` in a later release.
- F17 (recipes): decide whether an ingredient's `may_contain` feeds the kitchen list or a
  product's own allergens (docs/open-questions.md).

Owner question (open):

- An order placed while everything was approved stays payable after an approval is
  cleared: the payment step (`payment.rs`) does not check allergen approvals again, so a
  customer can pay a pending order whose product or kitchen list lost its approval while
  the order waited. That order was placed against approved data, so this change leaves it
  as is. Should staff be able to block or cancel such pending orders, or should payment
  check the approvals again? (docs/open-questions.md)
