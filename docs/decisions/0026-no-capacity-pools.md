# 0026 No capacity pools, only a maximum per product

Status: accepted, 2026-11-24 (owner)

F3 asks for limits per product and per dough or oven-load pool. The owner does not need
pools. A bake day limits each product with its own maximum only.

The back office no longer shows pools: no pool forms on the bake day page, no pool or
units-per-item field on a product in the range, and no pool lines on the production
list. Editing a product on the range changes only its maximum.

The `capacity_pool` table, `bake_day_product.capacity_pool_id` and `units_per_item`, and
the pool code in `bakedays` and `orders` were removed in ADR 0048. Checkout enforces the
per-product maximum under its lock.
