# 0026 Refunds, cancelling paid orders and the sick-day cancel

Status: proposed, 2026-11-24

Second slice of F6 and the first of F2, in `orders::refunds` and the back office.

| Choice | Why |
|---|---|
| A refund is its own row in the new `refund` table: payment, amount in cents, status `requested`, `done` or `failed`, the provider's refund id, attempts, the last error, who asked and when. A trigger refuses deleting a refund, a refund on a payment that is not paid or was refused (`refund_due`), and refunds that add up to more than the payment | Paid orders and payments change only through new rows (spec, "Data model"). `refund_due` means "refused payment, never a sale", and reports skip those payments (ADR 0017), so reusing it would hide the refund from the payout |
| Cancelling a paid or confirmed order (`cancel_paid`) sets it `cancelled` with reason `staff`, adds a `requested` refund for what is left of each accepted payment, and writes one `cancelled` audit entry with the refunds in it. The server queues the cancellation email with the refund amount and drops an unsent pickup reminder in the same transaction. An on-account order gets no refund. A second click finds the order cancelled and changes nothing | One transaction for the order, the refund rows, the audit entry and the email; the Mollie call comes after the commit, so a failed call never undoes the cancel |
| `issue` sends one refund to the provider while it holds the refund row `FOR UPDATE`. The idempotency key is `refund-<refund id>-<attempt>`. Before a retry it reads the payment from the provider: if the provider already refunded that much, the row becomes `done` without a new call. Mollie accepted (queued, pending, processing, refunded) is `done`; refused, failed or no answer is `failed` with the error. A `done` row is never sent again | Two clicks wait for each other on the row lock. A refund that reached Mollie but whose answer was lost is found on the retry instead of paid twice; a full refund is also refused by Mollie when nothing is left to refund |
| Reports count a refund as soon as its row exists, whatever its status. Per accepted payment the refund is the larger of `payment.refunded_cents` and the sum of its refund rows. Sales also count a cancelled order that was paid (`paid_at` set): the sale and its refund lines cancel out to the cent | The order is cancelled, so the money is owed to the customer even while a refund failed; the charity never gets money that goes back. A failed refund keeps its row and is retried in place, so it is never subtracted twice |
| Permissions (ADR 0010): cancelling a paid order and retrying its refund need `Refund`, now owner and baker. The sick-day cancel needs `CloseBakeDay`, now owner only | Decided with the owner: a baker may cancel a single paid order, only the owner cancels a whole bake day |
| Sick-day cancel (`cancel_day`): the owner writes the message on `/staff/bake-days/{id}/cancel` (linked from the day's order list), which says how many orders are cancelled and refunded, the total, and that every customer gets an email. It is a page with a danger button, not the confirm dialog, because the dialog component has no field for the message. One transaction cancels the bake day (`bake_day.cancel_message`), cancels every pending, paid or confirmed order with reason `day_cancelled`, adds refunds for the paid ones and queues one email per order (`sick-day:<order>`). After the commit every open refund of the day is sent. Running it again on a cancelled day keeps the first message, cancels nothing twice, queues no email twice and sends only the refunds that are not `done` | Safe after a partial failure: some refunds failed, Mollie was down, or the server stopped halfway |
| An invoiced on-account order is left as it is on a cancelled day and listed on the page | Invoiced orders cannot change (ADR 0017); the correction is a credit by hand |
| Moving an order to another pickup slot, or cancelling it, deletes its unsent `pickup-reminder:<order>` email. The reminder job queues a new one with the new time and place when it is due | One place builds the reminder; a cancelled order never gets one |

Follow-ups:

- Refunds stay `done` once Mollie accepts them. Mollie's refund webhook and a refund that
  fails later at the bank are not read yet.
- Payments refused with `refund_due` (for example a payment that completes after the day
  is cancelled) are still listed by `refunds_due` and not refunded automatically.
- A `requested` refund whose call never happened (server stopped after the commit) waits
  for the retry button; no job picks it up yet.
- Changing quantities and partial refunds.
- A pickup reminder that was already sent before a move is not sent again with the new
  time.
