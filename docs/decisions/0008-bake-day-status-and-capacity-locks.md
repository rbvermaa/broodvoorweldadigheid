# 0008 Bake day status and capacity locks

Status: proposed, 2026-11-24

**Stored status vs. public status.** The data model stores `draft | open | closed |
cancelled | done` (BakeDay). The public drop calendar (F1a) shows planned, menu
announced, open, sold out and closed. The public status is computed, never stored:

| Stored | Condition | Public |
|---|---|---|
| draft | `menu_publish_at` empty or in the future | planned |
| draft | `menu_publish_at` reached | menu announced |
| open | before `cutoff_at`, something left | open |
| open | before `cutoff_at`, nothing left | sold out |
| open | `cutoff_at` passed (the closing job has not run yet) | closed |
| closed, done | | closed |
| cancelled | | cancelled |
| any, production-only | | not shown |

"Sold out" depends on orders, so the caller passes it in. "Cancelled" is not in F1a's
list; a sick-day drop (F2) needs some label, so the public page gets its own.

**Transitions.** `draft -> open -> closed -> done`, and any status except done to
cancelled. A production-only day (`online_ordering = false`) goes from draft straight to
closed and can never be open; a CHECK enforces this for every writer. Opening is refused
once the deadline has passed. Done is refused before `bake_starts_at`. Transition
functions take `&dyn Clock`; the jobs crate will call them at the set times.

**Done or cancelled days are frozen.** Schedule, slots, charity, pools, range and extra
production can no longer change. Spec F7a says extras change "until the batch is mixed";
batches arrive in Phase 2, so until then the end is the day being done or cancelled.

**Capacity locks.** Anything that takes capacity (extra production now, checkout later)
runs in one transaction that:

1. locks the `bake_day` row `FOR SHARE`, so the status cannot change underneath it
   (transitions take `FOR UPDATE`);
2. locks the `bake_day_product` row `FOR UPDATE`, which guards the product's `max_qty`;
3. locks that product's `capacity_pool` row `FOR UPDATE`, which guards the pool's units;
4. reads the usage, checks, writes, commits.

`bakedays::range::lock_capacity` takes locks 2 and 3. A checkout with several products
must take the product rows in product-id order, then the pool rows in pool-id order, so
two checkouts cannot deadlock. Under READ COMMITTED each statement after the locks sees
the other writer's committed rows, so the usage read in step 4 is current. A database
test races two writers for the last unit of a pool.

**Ordered quantities are a parameter for now.** The orders table does not exist yet.
`range::availability` and `extras::add/update` take the ordered pieces per product as a
plain map. The orders slice reads them inside the same transaction, after the locks, and
passes them in. The pure rule lives in `bakedays::capacity` and is property-tested:
accepted requests never oversell, and the remaining count is exactly what still fits.

Smaller choices in the same change:

- Extra production references the product, not the day's range. Extras of a product
  that is not on the range (granola for SumUp, say) are not limited; extras of a product
  on the range count against its maximum and pool.
- Lowering an extra is always allowed, even when the day is already over capacity
  (after shrinking a pool). Only an increase is checked.
- A new bake day gets one pickup slot per active window of the location on that weekday.
  The deadline is the earliest of their default deadlines. A date with no window is
  refused; staff add a window first.
- Copying a day keeps local wall-clock times: every timestamp moves by the same number
  of calendar days in Europe/Amsterdam, so a Saturday 11:00 slot stays 11:00 across a
  clock change. Pools, range and slots are copied; extras, status and cancel message are
  not. An archived charity is dropped from the copy.
- Dates travel as days since 1970-01-01 (`db::time`), like the other jiff types.
- `bake_starts_at` is entered per day. The spec has no default rule for it.
