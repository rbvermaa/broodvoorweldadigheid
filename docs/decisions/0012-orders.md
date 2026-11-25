# 0012 Orders: checkout, holds and payments

Status: proposed, 2026-11-24

**Checkout is one transaction with a fixed lock order.** `orders::checkout` follows
ADR 0008 and adds locks at the end. Every writer that touches orders takes them in this
order:

1. `bake_day` `FOR SHARE` (status, deadline and charity cannot change underneath);
2. `bakedays::range::lock_capacity` for the order's products (product rows by id);
3. the `pickup_slot` row `FOR UPDATE`, which guards `max_orders`;
4. checkout: the `customer` row (insert if absent) and the year's `order_number` row
   (an upsert, the last lock), then the insert. Payment (`mark_paid`): the order row
   `FOR UPDATE`.

Ordered quantities are read after step 2 through `orders::Orders`, the real
`bakedays::OrderSource`. A cart is checked as a whole (`range::check_order`, new in
bakedays): `Capacity::check_all` adds each line to the usage before checking the next. Checkout checks `cutoff_at` itself and never
relies on the stored status alone (ADR 0011).

**What counts as ordered.** Lines of orders that are paid, confirmed or picked up, or
`pending_payment` with `hold_expires_at` still in the future. A hold is 15 minutes. The
capacity is free the moment the hold passes, before any job runs.
`release_expired_holds` (run by the jobs tick every minute) then cancels those orders
with `cancel_reason = hold_expired` and an audit entry, so the order list shows it. It
takes rows `FOR UPDATE SKIP LOCKED` and waits for nothing, so it cannot deadlock with a
payment. For the charity rule (`OrderSource::count`), every order counts except one
cancelled before it was paid.

Holds are compared with the app server's clock (`domain::Clock`), not the database's.
Servers keep time with NTP; a skew of a few seconds only moves the edge of a 15-minute
hold.

**Who may order when.** Web: the day is stored open and now is before `cutoff_at`.
Staff (`OrderedBy::Staff`, channel `manual`): any day that sells online and is not done
or cancelled, also after the deadline, still within capacity. Production-only days take
no orders from anyone. Products must be active, sold online, on the day's range and have
approved allergen data. The last one is our reading of "block when in doubt": an order
confirmation and the pickup list would otherwise carry a product whose allergen data no
one approved.

**Payments: only within the hold.** The spec says an order counts if checkout started
before the deadline and payment completes within its hold. We follow it literally: a
payment that completes at or after `hold_expires_at` is never accepted, whatever capacity
is left. The payments slice sets Mollie's `expiresAt` to about the hold, so most late
payments cannot happen at all. `mark_paid` decides:

| Order | Result |
|---|---|
| pending, within hold, amount equals total, day not done or cancelled | paid |
| pending, amount differs | refund due (`amount_mismatch`), order stays pending |
| pending, day done or cancelled | refund due (`day_final`), order cancelled `day_cancelled` |
| pending after hold, or cancelled `hold_expired` | refund due (`hold_expired`), order cancelled `hold_expired` |
| cancelled for another reason | refund due (`order_cancelled` or `day_final`) |
| confirmed, picked up or refunded without a payment | refund due (`order_confirmed`) |
| already paid | refund due (`paid_twice`), order unchanged |

**Every payment is recorded.** `mark_paid` always writes a `payment` row in the same
transaction as its decision, also for refused payments: `refund_due` with
`refund_reason`. `refunds_due` lists payments with a refund due that are not yet fully
refunded; the refund itself belongs to the payments crate, which raises
`refunded_cents`. `provider_payment_id` is unique, and `mark_paid` looks it up after
locking the order row: a webhook retry returns the outcome of the first report
(`AlreadyPaid`, or `RefundDue` with the original reason) and writes nothing, no audit
entry either. A reference already recorded for another order is an error.

**Why payment still takes the capacity and slot locks.** Within the hold a payment
changes no ordered quantity: the pending order counts until it is paid, and paid counts
too. But at the exact moment of expiry a checkout can see the hold as expired and take
its pieces or its slot place, while the payment, judged a moment earlier, accepts it.
Taking the same locks as checkout and reading the clock only after them makes the two
take turns: whichever is second sees the other's result at its own, later time. Tests
cover a payment that waits on a checkout in another slot (only the capacity lock
serialises them) and a payment that waits on the release job.

**Donations.** On paid, one `donation_allocation` of the full amount paid is earmarked to
the drop's charity at that moment; a drop without charity creates none (F4c). The charity
cannot have changed since checkout: `set_charity` counts the order from checkout on, and
an order can only be paid within its hold. Choosing another charity (F4d) fills `saved`,
`choose_token_hash` and `choose_by` later (ADR 0030, 0048).

**Customer details are kept per order.** Anyone can type any email address, so a guest
checkout never changes an existing `customer` row: it inserts one if the email is new
and otherwise only links to it. The order keeps the name and phone entered for it
(`customer_name`, `customer_phone`); the pickup list (F8) shows those. Name and phone are
single lines (no control characters). Marketing consent is never set from checkout: the
order records the wish (`marketing_opt_in`) and F9a's double opt-in does the subscribing.

**No PaymentProvider trait yet.** Checkout only creates the order. The payments crate
(layer 5) calls `orders::mark_paid` from its webhook with the provider's payment id and
amount; it may depend on orders, so no trait is needed for that direction. Starting a
payment from checkout will go through the domain trait when the server wires them.

Smaller choices in the same change:

- Order numbers are `YYYY-NNNN` per calendar year in Europe/Amsterdam (the order's
  creation time), from a counter row bumped last in the transaction. A rolled-back
  checkout leaves no gap; an abandoned or expired order keeps its number, so the
  numbers of paid orders do have gaps.
- `order.pickup_slot_id` references `(pickup_slot.id, bake_day_id)`, so a slot of another
  day cannot be stored. There is no archived slot in the schema; a slot of another day is
  refused as `SlotNotOnDay`.
- Columns beyond the spec's Order: `customer_name`, `customer_phone`,
  `marketing_opt_in`, `cancel_reason`, `paid_at`. Beyond the spec's Payment:
  `refund_due`, `refund_reason`, `received_at`.
- `audit_log` is append-only (a trigger refuses update, delete and truncate). Actors are
  stored as `customer:<id>`, `staff:<id>` or `system:<job>`. Every order status change,
  every recorded payment and every allocation writes an entry in the same transaction.
- A cart line holds at most 100 pieces. The cart serialises to JSON with plain ids and
  cents and is checked again at checkout, so a tampered session cart gains nothing.
- Staff orders are `pending_payment` with a hold like web orders until payment links or
  on-account orders arrive (F5, F11a).
