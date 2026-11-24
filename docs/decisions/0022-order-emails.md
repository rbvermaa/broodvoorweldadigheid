# 0022 Order emails: confirmation, pickup reminder, cancellation and sending

Status: proposed, 2026-11-24

Covers F9 on top of the outbox of ADR 0014 and the webhook of ADR 0015.

| Choice | Why |
|---|---|
| `jobs::order_emails` loads an order (`orders::details`), maps it to `notify`'s input structs and queues the email. The server's webhook and the jobs tick both call it | `notify` (layer 5) stays free of order code (ADR 0014) and `orders` (layer 4) cannot use `notify`. `jobs` (layer 6) may use both, and the server (layer 7) may use `jobs` (ADR 0020) |
| The webhook opens a transaction, runs `orders::mark_paid` in it (its own `begin` becomes a savepoint) and queues the confirmation before the commit | The confirmation is queued if and only if the payment counts. A Mollie retry gets `AlreadyPaid` and queues nothing; the dedupe key `order-confirmation:<order>` covers anything else |
| Confirmation only for the `Paid` outcome. A `RefundDue` payment whose order is cancelled because its hold expired, or was cancelled before the payment came in, gets the cancellation email naming the refund, dedupe key `refund-due:<payment>`. Other refund reasons send nothing: amount mismatch and paid twice leave the order as it is, and a cancelled day is for the sick-day flow (F2) | One email per refused payment; an order can have more than one |
| The confirmation lists the lines with "jouw prijs" for pay what you want (`suggested_price` set), BTW per rate (ADR 0005), the charity of the order's donation allocation or nothing for a normal sale, the ophaalmoment and location, and a button to `PUBLIC_URL/bestelling/<status token>` | Compliance: rate and BTW amount on confirmations. The status token, never the order number, is the link (ADR 0015) |
| `enqueue_cancellation(order, refund)` with dedupe key `order-cancelled:<order>` is ready for staff cancel (F6). An unpaid order whose hold expires gets no email | Nothing was paid, and the customer saw the expiry on the status page |
| Pickup reminder: the jobs tick queues one per paid or confirmed order once `now` is at or after `PICKUP_REMINDER_AT` (default 18:00) Amsterdam time on the day before the pickup's local date, and before the pickup starts. Dedupe key `pickup-reminder:<order>`; keys already in the outbox are skipped before composing. Cancelled, refunded and picked-up orders, and orders on a cancelled bake day, get none. Each order is checked again with its bake day locked `FOR SHARE` in the transaction that queues it, so a cancel or slot change cannot slip in. An order that cannot be read or composed is logged and skipped; the others still go | Local time through jiff, so the October and March changes need no special case. An order paid after 18:00 the day before still gets one |
| The webhook queues the email in a savepoint. When that fails for any reason (missing KvK or address, bad link, a database error on the insert), it is logged at error level, only the savepoint rolls back, and the payment is recorded. Only a failed `mark_paid` or commit answers 503 | Recording a payment matters more than its email; a 503 loop on an email error would leave the payment unrecorded until the hold expires. The company's KvK number and address are empty until the owner fills them in, and no form for that exists yet |
| Email links must be https (ADR 0014), so with an `http://` `PUBLIC_URL` (local runs only) no confirmation is queued; the server warns about that at startup | Production and compose use https |
| The server builds `SmtpSender` from `SmtpConfig::from_env`. Without `SMTP_HOST` it logs that email is off and runs; emails wait in the outbox. Any other config error stops the server | A typo in the relay settings should not go unnoticed |
| A small loop in `server::background` calls `notify::send_pending` every minute, 20 emails per round, next to the jobs runner and stopped by the same shutdown, which is checked between emails. Each send has a 40 s deadline (lettre's own timeout only covers connecting); after a send runs out, `SmtpSender` starts a fresh connection pool, because lettre would reuse the cut-off connection and read its late reply as the next answer; compose gives the server a 60 s stop grace period, enough for one send and its commit | Sending stays out of the apalis tick, so a slow relay never delays opening bake days. `FOR UPDATE SKIP LOCKED` lets several servers send |
| The compose stack points the server at Mailpit (`SMTP_HOST=mailpit`, port 1025, TLS none), overridable with `BVW_SMTP_*` and `BVW_MAIL_FROM` | `make up` shows every email at `https://mail.localhost:8443/` |

Open:

- If outbox rows are deleted before the pickup (ADR 0014, retention), a reminder could be
  queued again. Retention must keep `pickup-reminder` rows until after the pickup.
- The owner needs a way to enter the KvK number and address; until then no order email
  goes out (deploy/README.md shows the SQL for the local stack).
