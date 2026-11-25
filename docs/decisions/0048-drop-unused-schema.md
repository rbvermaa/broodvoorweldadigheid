# 0048 Drop unused columns after Phase 2

Status: accepted, 2026-11-25 (owner)

CLAUDE.md rule 7 says migrations only add or extend, and removals happen in a later
release. The application has never been deployed, so no running release reads these
columns and no stored data depends on them. The owner (2026-11-25): "when we are done with
phase 2, remove old database fields, as we have not yet deployed the application".
Migration `20261205000000_drop_unused.sql` drops them in one step. Earlier migrations are
not changed.

| Dropped | Unused since | Code removed with it |
|---|---|---|
| `capacity_pool` table, `bake_day_product.capacity_pool_id` and `units_per_item` | ADR 0026 | Pool functions, `CapacityPool`, `CapacityPoolId` and the pool maths in `bakedays::capacity`. `lock_capacity` locks only the product rows; checkout and extras still enforce each product's maximum under that lock |
| `product.lead_time_hours` | ADR 0027 | The field on `ProductInput` and `Product` |
| `company.default_lead_time_hours` | ADR 0006, update 2026-11-24 | None; nothing read it |
| `product.may_contain` and its clause in `clear_allergen_approval` | ADR 0038 | None; nothing read or wrote it |
| `donation_allocation.choose_token` (plaintext) | ADR 0030 | The `choose_token = NULL` writes. The two CHECKs on the allocation now name only `choose_token_hash` |
| `email_outbox_kind_check` | ADR 0044 | None. The foreign key to `email_kind` refuses an unknown kind, and `email_kind` refuses `''` |

Kept:

| Kept | Why |
|---|---|
| `product.estimated_cost_cents` | F11e still falls back to it when a product has no baked batches and no standard cost (ADR 0045) |
| `recipe_version.lead_time_hours` | Information on the recipe (ADR 0027) |
| `company.may_contain` | The kitchen-wide list (ADR 0038) |

After the first deployment, rule 7 applies again: a removal waits for a later release.
