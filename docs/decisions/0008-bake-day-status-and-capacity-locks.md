# 0008 Bake day status and capacity locks

Status: proposed, 2026-11-24

**Stored status vs. public status.** The data model stores `draft | open | closed |
cancelled | done` (BakeDay). The public drop calendar (F1a) shows planned, menu
announced, open, sold out and closed. The public status is computed, never stored:

| Stored | Condition | Public |
|---|---|---|
| draft | `menu_publish_at` empty or in the future | planned |
| draft | `menu_publish_at` reached | menu announced |
| draft | `cutoff_at` passed (it never opened) | closed |
| open | before `cutoff_at`, something left | open |
| open | before `cutoff_at`, nothing left | sold out |
| open | `cutoff_at` passed (the closing job has not run yet) | closed |
| closed, done | | closed |
| cancelled | | cancelled |
| any, production-only | | not shown |

"Sold out" depends on orders, so the caller passes it in (`range::sold_out`: the range is
not empty and nothing is left). `opens_at` plays no part in the public status: a day shows
open only once it is stored as open. The jobs crate opens it at `opens_at`; until that
job runs, a draft keeps showing planned or menu announced. "Cancelled" is not in F1a's
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
2. calls `bakedays::range::lock_capacity(conn, day, products)`, which locks the
   `bake_day_product` rows of all products it will touch `ORDER BY product_id FOR UPDATE`
   (guarding each `max_qty`), then their `capacity_pool` rows `ORDER BY id FOR UPDATE`
   (guarding the pools' units);
3. reads ordered quantities and extra production, checks, writes, commits.

Every writer takes all its product rows before any pool row, each set in id order, so
two writers cannot deadlock even when their products sit in crossing pools. Under READ
COMMITTED each statement after the locks sees the other writer's committed rows, so the
reads in step 3 are current. Database tests race two writers for the last unit of a
pool, for the last piece of a product without a pool, and two multi-product writers with
crossing pools.

**Orders come in through a trait, read after the locks.** The orders table lives in a
higher crate, so bakedays cannot query it. Functions that need orders take
`&impl OrderSource` and call it inside their own transaction (a savepoint when the
caller already holds one), after taking the locks: `ordered` for capacity
(`range::availability`, `extras::add/update`), `count` for the charity rule
(`days::set_charity` locks the bake day row `FOR UPDATE` first). Reading orders before
the locks would let a checkout commit in between and oversell. Until the orders slice
exists, callers pass `NoOrders`. A test with a table-backed fake shows a staff extra
waiting for a checkout's locks and then seeing its order lines. The pure rule lives in
`bakedays::capacity` and is property-tested: accepted requests never oversell, and the
remaining count is exactly what still fits.

Smaller choices in the same change:

- Extra production references the product, not the day's range. Extras of a product
  that is not on the range (granola for SumUp, say) are not limited; extras of a product
  on the range count against its maximum and pool.
- Lowering an extra is always allowed, even when the day is already over capacity
  (after shrinking a pool). Only an increase is checked.
- Shrinking a pool (`resize_pool`), lowering a product's maximum, or moving a product
  into a pool that is already full (`set_product`) are limit changes by staff, not
  writes that take capacity. They are allowed; the remaining count then shows zero.
- A new bake day gets one pickup slot per active window of the location on that weekday.
  The deadline is the earliest of their default deadlines. An online date with no window
  is refused; staff add a window first. A production-only day may have no window: it
  then has no slots and no deadline, and never gets a lead-time warning.
- A deadline is never after `bake_starts_at`, and `menu_publish_at` never after the
  deadline (CHECKs and validation).
- Copying a day keeps local wall-clock times: every timestamp moves by the same number
  of calendar days in Europe/Amsterdam, so a Saturday 11:00 slot stays 11:00 across a
  clock change. Pools, range and slots are copied; extras, status and cancel message are
  not. An archived charity is dropped from the copy and `CopiedDay.charity_dropped`
  says so. A copy is refused (`DateTaken`) when a bake day that is not cancelled already
  uses one of the source's pickup locations on that date, or, for a day without slots,
  when another day without slots exists then. `repeat_weekly` copies all weeks or none.
- Dates travel as days since 1970-01-01 (`db::time`), like the other jiff types.
- `bake_starts_at` is entered per day. The spec has no default rule for it.
