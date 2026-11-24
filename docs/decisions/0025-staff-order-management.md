# 0025 Staff order management: list, move, picked up, cancel

Status: proposed, 2026-11-24

First slice of F6, in `orders::manage` and the back office under `/staff/orders`.

| Choice | Why |
|---|---|
| The order list is per bake day, filtered by status and pickup location, sorted by ophaalmoment then order number. It shows every status, also cancelled ones | Staff need to see what happened to an order, not only the open ones |
| Staff can move a paid or confirmed order to another slot of the same bake day, also after the deadline, as long as the slot has room and the day is not done or cancelled | A slot changes no quantities, so production is not affected. The spec lets staff add orders after the deadline (ADR 0012); moving one is less |
| Mark picked up: paid or confirmed to `picked_up`, not on a cancelled day | F8's tick-off uses the same call later |
| Staff cancel covers only orders waiting for payment, like `cancel_pending` with the staff member as actor; a payment that is completing decides. The cancellation email is queued in the same transaction. A paid or confirmed order cannot be cancelled yet | Cancelling a paid order needs a refund, and the donation rules need it settled: its allocation stays earmarked, and the payout (F11b, ADR 0017) only subtracts refunds of accepted payments. Reusing `refund_due` would hide that refund from the payout, since `refund_due` means "refused payment, never a sale". That comes with the refund slice |
| Each change writes one audit entry with `staff:<id>` as actor: `pickup_moved` (old and new slot), `picked_up` and `cancelled` (order snapshots). The detail page shows the order's audit history, with staff names | F6 audit log: who, when, what |
| Permissions (ADR 0010): the list and detail need `ViewOrders`; picked up needs `PickupList`; moving a slot and cancelling an unpaid order need the new `EditOrders` (owner, baker) | Neither moves money. Cancelling a paid order will need `Refund` (owner) |

Not in this slice:

- Changing quantities. On a paid order that means a partial refund or an extra payment,
  and the spec says paid orders change only through new rows. It waits for refunds.
- Cancelling a paid or confirmed order, refunds through Mollie and the refunded status,
  with the payout rule for an order that is cancelled but not yet refunded.
- Customer notes (F6) need a new column.
