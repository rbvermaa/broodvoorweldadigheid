# 0017 Reports: sales, invoices, payouts, donations and contribution

Status: proposed, 2026-11-24

The `reports` crate (layer 6) reads orders, payments and allocations and returns plain
data and CSV text (F11, F11a, F11b, F11c, F11e). Screens and PDFs come later in
`apps/server`.

**Which orders are sales (F11).** Orders that are paid, confirmed, picked up or
refunded. A sale is dated by `paid_at`, or `created_at` for an order on account, as a
calendar day in Europe/Amsterdam. Periods are inclusive date ranges; the query turns
them into the instants of local midnight, so summer and winter time give 23- and
25-hour days. Drops without a charity are included. Only refunds on payments the order
accepted count; a refused payment (`refund_due`) never was a sale.

**Refunds are negative lines.** `payment.refunded_cents` is an amount per payment, with
no line and no date. The rule (`refund_lines`):

| Refund | Lines |
|---|---|
| the whole order (or more) | every line reversed exactly, so BTW cancels to the cent |
| part of the order | split over the order's lines in proportion to their gross, largest remainder first, one negative line per line; no line gets back more than it cost, so no line ends with negative BTW |
| part of a mixed-rate order | as above, and the order is listed in `estimated_refunds` |

The split over lines is our answer to the open question on mixed-rate partial refunds until
the accountant decides. Refunds carry no date, so a refund is counted on the sale's
day: a later refund changes an earlier period. See open questions.

**Pay what you want.** Per product: pieces, total paid and total suggested from the
lines; averages per piece are rounded half-up to whole cents. Refunds do not change
them.

**CSV for the accountant.** Semicolon separated, decimal comma without thousands
separator (`1234,56`), dates `dd-mm-jjjj`, UTF-8 with a byte order mark, CRLF line
ends. Dutch Excel opens this directly and bookkeeping imports read it. Column names
come from `locales/nl/reports.ftl`. Sales: one row per period and rate (both rates,
also when zero), then `Totaal` rows per rate.

**B2B invoices (F11a).** New tables `business_account` (one per customer) and
`manual_invoice`, and `order.manual_invoice_id`. The overview lists confirmed and
picked-up on-account orders of the customer, not yet invoiced, on drops dated in the
period (the delivery date). Each line shows its net and BTW from the line split (ADR
0005); the unit price excl. BTW is the unit price split on its own and is for display
only. `mark_invoiced` takes all orders or none: it refuses an order already invoiced,
of another customer, not on account, not confirmed or picked up, or on a drop outside
the invoice's period, a customer without a business account, and a used invoice
number. Triggers lock an invoiced order: it is never deleted, its lines never change
(the line trigger reads the order `FOR SHARE`, so it waits for a running
`mark_invoiced`), the invoiced columns never change and the status only moves from
confirmed to picked up. Columns added later stay writable. `manual_invoice` is
append-only; `order` and `order_line` refuse TRUNCATE.

**Charity payouts (F11b).** `charity_transfer` plus `charity_transfer_allocation`, which
stores per allocation what a transfer settled: the allocation's amount the first time,
and the refunds not settled before. The amount due for drops up to a date is the sum
over the charity's allocations of (amount − settled amount) − (refunds − settled
refunds). `record_transfer` only accepts exactly that amount (else `AmountMismatch`
with the amount due), marks the earmarked allocations transferred and writes an audit
entry. A refund after a transfer is a correction: it lowers the next transfer and shows
as `corrections`. Transfers and their links are append-only; a transferred allocation
cannot change (trigger). Saved allocations are shown per drop charity, not paid out.
A refund larger than the new donations leaves a negative amount due, which cannot be
recorded; it waits for later donations.

**Donations (F11c) and contribution (F11e)** are dated by the drop. They are computed
from allocations, net of refunds, so drop totals and charity totals are the same money
(property tests). An order's refunds go to its allocations oldest first. Allocations
are read whatever the order's status now is, so a refund after a transfer is always
seen as a correction. The
contribution counts only orders with an allocation: BTW from the lines net of refunds,
Mollie fees from `payment.fee_cents` (an order with an unknown fee is counted in
`unknown_fees`, never as zero), ingredients as the product's current
`estimated_cost_cents` × quantity (products without an estimate are listed) and the
drop's `drop_cost` rows. A drop's costs count for the drop's charity, also when the drop
has no donations; an order's other
costs for the charity its donation went to. "Per €1" is `covered × 100 / received`
rounded half-up in integer maths (`per_euro_cents`), also in basis points.

Uncertain:

- Dating refunds on the sale's day changes closed periods. Mollie knows the refund date;
  storing it needs a refund table in the payments slice.
- Ingredient cost uses today's estimate, not the one on the bake day, until F16 batch
  cost exists.
- Whether Mollie fees include BTW (ADR 0013).
