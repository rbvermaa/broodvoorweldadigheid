# 0034 The B2B invoice overview as PDF

Status: proposed, 2026-11-25

F11a asks for the invoice overview as screen, PDF and CSV. ADR 0032 left the PDF out.

| Choice | Why |
|---|---|
| `reports::invoice_pdf` builds the template data from the overview: every text translated, amounts formatted from cents, totals per BTW rate. `production` holds the template (`typst/invoice_overview.typ`) and lays it out, through `invoice_overview_pdf` | `reports` (layer 6) owns the overview; `production` (layer 5) cannot see it. The data-only JSON keeps `production` free of invoice types, and `reports` may depend on `production`. No new crate |
| `production::invoice_overview_text` returns the laid-out text and warnings, so `reports` snapshots the PDF text like the production list tests do | The bytes are not readable in a review; the text is |
| The top shows the bakery (name, address, KvK, BTW-id from the company settings) and the customer (company name, invoice address, KvK, BTW-id, invoice email). Company fields not filled in are left out | The owner copies these to the hand-made invoice. A missing field should not block the overview |
| Same `Invoicing` permission, period and order set as the screen and the CSV. Sent `inline`, `private, no-store` | Same rules as the other PDFs and the CSV |

Follow-up: a small shared `pdf` crate under both `production` and `reports` would be a
cleaner home for the Typst world once a third document needs it.
