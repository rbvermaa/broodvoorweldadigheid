# 0026 No capacity pools, only a maximum per product

Status: accepted, 2026-11-24 (owner)

F3 asks for limits per product and per dough or oven-load pool. The owner does not need
pools. A bake day limits each product with its own maximum only.

The back office no longer shows pools: no pool forms on the bake day page, no pool or
units-per-item field on a product in the range, and no pool lines on the production
list. Editing a product on the range changes only its maximum and leaves any stored pool
and units per item as they are. A product added to the range gets 1 unit per item.

The `capacity_pool` table, `bake_day_product.capacity_pool_id` and the pool code in
`bakedays` and `orders` stay, dormant, because migrations only add. They are removed in a
later release. Checkout still enforces the per-product maximum under its lock. A pool that
was already stored on a bake day (or copied from one) still limits that day, with no
control left to change it; the pool UI was only live briefly, so we accept that until the
removal.
