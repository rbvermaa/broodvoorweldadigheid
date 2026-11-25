# 0006 Where charities, pickup locations and lead times live

Status: proposed, 2026-11-24

**Charities go in `catalog`.** The spec's crate list does not name them. They must sit
at layer 3 or lower, because `bakedays` (layer 3) puts a charity on each drop (F4c) and
`orders` (layer 4) creates donation allocations. `catalog` (layer 2) holds what the
bakery offers and stands for, needs only `domain` and `db`, and stays free of bake-day
logic. A separate `charities` crate would add a crate the spec doesn't list for one
table.

**Pickup locations and windows go in `bakedays`.** The spec gives `bakedays` the pickup
slots, and a new bake day copies its pickup time and deadline from the location's
default window (F1, F4a). Keeping windows and slots together keeps that rule in one
crate. Nothing below layer 3 needs locations.

**Sourdough lead time sits on `product` until recipes ship.** The deadline warning (F1)
uses the longest recipe lead time on a drop, but `RecipeVersion.lead_time_hours` arrives
in Phase 2. Until then `product.lead_time_hours` (nullable) holds a per-product value and
`company.default_lead_time_hours` the fallback. The default is 36 hours: an assumption,
to confirm with the baker. When recipes ship, the recipe value takes over. Both
columns were dropped in ADR 0048.

**Update 2026-11-24: the owner's rule replaces the 36-hour default.** The owner answered
"sourdough lead time can just be 2 days before in the evening". The warning now uses
`company.sourdough_deadline_days_before` (default 2, 0 to 14) and
`company.sourdough_deadline_time` (default 20:00): the latest deadline is that local time
on the bake date minus that many days, with the same summer/winter time handling as the
default deadline. A product that sets its own `lead_time_hours` still adds a check
(`bake_starts_at` minus that lead time); the earlier of the two limits wins. A product
without one has no lead time of its own: there is no fallback any more.
`company.default_lead_time_hours` is no longer read (dropped in ADR 0048). The warning
applies to every online drop, also one without products yet. Per-product lead times were dropped later the same day (ADR 0027).

Smaller choices in the same change:

- SQLx 0.9 has no jiff support. Queries move timestamps as epoch microseconds and times
  of day as seconds since midnight (`db::time`), instead of adding `chrono` or `time`.
- `product.allergens_approved_by` has no foreign key yet; `staff_user` does not exist.
  The migration that adds `staff_user` adds the key.
- Triggers enforce two rules for every writer, not only the repositories: a change to a
  product's allergens, may-contain list or ingredient text clears its approval, and
  `charity` and `company` rows cannot be deleted or truncated.
