# 0025 No capacity pools, only a maximum per product

Status: accepted, 2026-11-24 (owner)

F3 asks for limits per product and per dough or oven-load pool. The owner does not need
pools. A bake day limits each product with its own maximum only.

The back office no longer shows pools: no pool forms on the bake day page and no pool
field on a product in the range. Editing a product on the range changes its maximum and
units per item and leaves any stored pool as it is.

The `capacity_pool` table, `bake_day_product.capacity_pool_id` and the pool code in
`bakedays` and `orders` stay, dormant, because migrations only add. They are removed in a
later release. Checkout still enforces the per-product maximum under its lock.
