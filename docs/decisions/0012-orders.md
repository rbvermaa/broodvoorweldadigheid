# 0012 Orders: checkout, holds and late payments

Status: proposed, 2026-11-24

**Checkout is one transaction with a fixed lock order.** `orders::checkout` follows
ADR 0008 and adds three locks at the end. Every writer that touches orders takes them in
this order:

1. `bake_day` `FOR SHARE` (status, deadline and charity cannot change underneath);
2. `bakedays::range::lock_capacity` for the cart's products (product rows by id, then
   pools by id);
3. the `pickup_slot` row `FOR UPDATE`, which guards `max_orders`;
4. the `customer` row (upsert by email) and the `order_number` row of the year
   (`INSERT ... ON CONFLICT DO UPDATE`), then the insert. Payment (`mark_paid`) takes the
   order row `FOR UPDATE` here instead.

Ordered quantities are read after step 2 through `orders::Orders`, the real
`bakedays::OrderSource`. A cart is checked as a whole (`range::check_order`, new in
bakedays): two products in one pool count together, so `Capacity::check_all` adds each
line to the usage before checking the next. Checkout checks `cutoff_at` itself and never
relies on the stored status alone (ADR 0011).

**What counts as ordered.** Lines of orders that are paid, confirmed or picked up, or
`pending_payment` with `hold_expires_at` still in the future. A hold is 15 minutes. The
capacity is free the moment the hold passes, before any job runs.
`release_expired_holds` (run by the jobs tick every minute) then cancels those orders
with `cancel_reason = hold_expired` and an audit entry, so the order list shows it. It
takes rows `FOR UPDATE SKIP LOCKED` and waits for nothing, so it cannot deadlock with a
payment. For the charity rule (`OrderSource::count`), every order counts except one
cancelled before it was paid; a pending order with an expired hold still counts until it
is released, because it can still be paid.

**Who may order when.** Web: the day is stored open and now is before `cutoff_at`.
Staff (`OrderedBy::Staff`, channel `manual`): any day that sells online and is not done
or cancelled, also after the deadline, still within capacity. Production-only days take
no orders from anyone. Products must be active, sold online, on the day's range and have
approved allergen data. The last one is our reading of "block when in doubt": an order
confirmation and the pickup list would otherwise carry a product whose allergen data no
one approved.

**Late payments.** The spec says an order counts if checkout started before the deadline
and payment completes within its hold. Mollie can still report a payment after the hold
(a slow bank, a customer who left the tab open). `mark_paid` decides:

| Order | Day | Result |
|---|---|---|
| pending, within hold | not done or cancelled | paid |
| pending after hold, or cancelled `hold_expired` | web order, deadline passed | refund due (`PastDeadline`) |
| pending after hold, or cancelled `hold_expired` | before the deadline, or a staff order | paid if the pieces and the slot still fit, else refund due (`SoldOut`) |
| pending or released | done or cancelled | refund due (`DayFinal`) |
| cancelled for another reason | any | refund due (`OrderCancelled`) |
| paid with another payment | any | refund due (`PaidTwice`), order unchanged |

Accepting a late payment while the loaf is still there is kinder than refunding it and
still never oversells: the check takes the same locks as checkout. A refused payment
leaves the order cancelled with the payment reference set (`Order::refund_due`); the
refund itself belongs to the payments crate. The same reference reported twice changes
nothing. An amount that differs from the order total is an error and changes nothing.

**Donations.** On paid, one `donation_allocation` of the full amount paid is earmarked to
the drop's charity at that moment; a drop without charity creates none (F4c). The charity
cannot have changed since checkout: `set_charity` counts the order from checkout on. A
late payment after release can meet a changed charity; that is rare and the allocation
then follows the drop. Choosing another charity (F4d) fills `saved`, `choose_token` and
`choose_by` later.

**No PaymentProvider trait yet.** Checkout only creates the order. The payments crate
(layer 5) calls `orders::mark_paid` from its webhook with the provider's payment id and
amount; it may depend on orders, so no trait is needed for that direction. Starting a
payment from checkout will go through the domain trait when the server wires them.

Smaller choices in the same change:

- Order numbers are `YYYY-NNNN` per calendar year in Europe/Amsterdam, from a counter
  row taken last in the transaction, so a rolled-back checkout leaves no gap.
- A returning guest is found by lower-cased email. Name and phone are replaced by the
  latest checkout; marketing consent is only ever switched on here (withdrawing is F12).
- `order.pickup_slot_id` references `(pickup_slot.id, bake_day_id)`, so a slot of another
  day cannot be stored. There is no archived slot in the schema; a slot of another day is
  refused as `SlotNotOnDay`.
- Columns beyond the spec's Order: `cancel_reason`, `payment_ref`, `paid_at`. They make
  the late-payment rule and "refund due" queryable without the payments table.
- `audit_log` is append-only (a trigger refuses update, delete and truncate). Actors are
  stored as `customer:<id>`, `staff:<id>` or `system:<job>`. Every order status change
  and every allocation writes an entry in the same transaction.
- A cart line holds at most 100 pieces. The cart serialises to JSON with plain ids and
  cents and is checked again at checkout, so a tampered session cart gains nothing.
- Staff orders are `pending_payment` with a hold like web orders until payment links or
  on-account orders arrive (F5, F11a).
