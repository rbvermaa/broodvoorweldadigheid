# 0027 Bake start is derived, sourdough rule is the only deadline warning

Status: accepted, 2026-11-24 (owner)

**Baking starts at 05:00 on the bake date.** Staff no longer enter "Begin van het
bakken" on a new, edited or copied bake day (F1). The server sets `bake_starts_at` to
05:00 Europe/Amsterdam on the bake date (`bakedays::schedule::BAKE_STARTS`), also on a
clock-change day. Saving a day's schedule writes the derived value again, so a day stored
with an older hand-entered time gets 05:00 on its next save. A copy gets 05:00 on its own
date. The column and its CHECK (deadline not after bake start) stay. The rules that use
it are unchanged: a deadline after 05:00 on the bake date is refused, and a day cannot be
done before baking started.

**No per-product lead time.** The deadline warning only follows the bakery's sourdough
rule (ADR 0006 update: `company.sourdough_deadline_*`, default 20:00 two days before).
The check against `bake_starts_at` minus a product's `lead_time_hours` is gone, and so is
the lead-time field on the product form. `product.lead_time_hours` stays in the database
and in `catalog`, unused; an edit keeps the stored value. It goes in a later release, or
is replaced by the recipe lead time when recipes ship (F14).

This replaces the per-product part of ADR 0006, the last bullet of ADR 0008
("`bake_starts_at` is entered per day") and the lead-time line in ADR 0019.
