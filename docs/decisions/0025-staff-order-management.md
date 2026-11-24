# 0025 Staff order management: list, move, picked up, cancel

Status: proposed, 2026-11-24

First slice of F6, in `orders::manage` and the back office under `/staff/orders`.

| Choice | Why |
|---|---|
| The order list is per bake day, filtered by status and pickup location, sorted by ophaalmoment then order number. It shows every status, also cancelled ones | Staff need to see what happened to an order, not only the open ones |
| Staff can move a paid or confirmed order to another slot of the same bake day, also after the deadline, as long as the slot has room and the day is not done or cancelled | A slot changes no quantities, so production is not affected. The spec lets staff add orders after the deadline (ADR 0012); moving one is less |
| Mark picked up: paid or confirmed to `picked_up`, not on a cancelled day | F8's tick-off uses the same call later |
| Staff cancel: a pending order is cancelled like `cancel_pending` (a payment that is completing decides). A paid or confirmed order is cancelled with `cancel_reason = staff` and its accepted payment gets `refund_due` with `order_cancelled`, so it shows in `refunds_due`. The cancellation email is queued in the same transaction | No refund is made yet: the Mollie refund call is the next slice. Setting `refund_due` is how `mark_paid` already flags money that must go back; amount and status of the payment stay as they are |
| Each change writes one audit entry with `staff:<id>` as actor: `pickup_moved` (old and new slot), `picked_up` and `cancelled` (order snapshots; a cancel includes the payment due for refund). The detail page shows the order's audit history | F6 audit log: who, when, what |
| Permissions (ADR 0010): the list and detail need `ViewOrders`; picked up needs `PickupList`; moving a slot and cancelling an unpaid order need the new `EditOrders` (owner, baker); cancelling a paid or confirmed order needs `Refund` (owner) | Cancelling a paid order sends money back; the spec lets roles limit who can refund |

Not in this slice:

- Changing quantities. On a paid order that means a partial refund or an extra payment,
  and the spec says paid orders change only through new rows. It waits for refunds.
- Refunds through Mollie, and the refunded status.
- Customer notes (F6) need a new column.
