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
to confirm with the baker. When recipes ship, the recipe value takes over and the
product column is dropped in a later release.

Smaller choices in the same change:

- SQLx 0.9 has no jiff support. Queries move timestamps as epoch microseconds and times
  of day as seconds since midnight (`db::time`), instead of adding `chrono` or `time`.
- `product.allergens_approved_by` has no foreign key yet; `staff_user` does not exist.
  The migration that adds `staff_user` adds the key.
- Triggers enforce two rules for every writer, not only the repositories: a change to a
  product's allergens, may-contain list or ingredient text clears its approval, and
  `charity` and `company` rows cannot be deleted or truncated.
