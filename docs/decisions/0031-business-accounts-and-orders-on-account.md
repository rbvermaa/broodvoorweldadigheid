# 0031 Business accounts, staff orders on account and the invoice screen

Status: proposed, 2026-11-24

Back-office part of F11a, on top of the tables and the overview in ADR 0017.

| Choice | Why |
|---|---|
| The owner opens an account with a contact email and name. The email is the account's `customer`; a new email creates the customer, an existing one is linked as it is and never changed. The contact email cannot be changed afterwards | Same rule as checkout (ADR 0012): anyone can type any email. Moving an account to another customer would move its order history |
| Opening and changing an account write `business_account` audit entries (`created`, `changed` with before and after) | Who allowed whom to order on account, and when |
| New permission `Invoicing` (owner only) for accounts, the overview, its CSV and marking orders invoiced. Placing an order on account needs `EditOrders` (owner, baker) | Invoices are money matters like the reports. Taking a phone order from a café is daily work |
| `orders::checkout_on_account`: staff only (channel `manual`), confirmed straight away, no hold and no payment. Pay-what-you-want lines take the suggested price, whatever price the cart holds (F4b). The order's name is the company name | Spec, "Data model": on-account orders skip Mollie. The pickup list shows the company |
| It follows checkout's lock order (ADR 0012) and reads the `business_account` row `FOR SHARE` after the customer step, before the order number. No account, or `on_account_allowed` off, is `NotOnAccount` and rolls back, also a customer it had just created | Turning the account off waits for a running order and then wins |
| No donation allocation for an order on account, also on a charity drop | Unchanged behaviour; see the open question "On-account orders on charity drops" |
| The overview's period defaults to this calendar year, dated by drop (ADR 0017). The mark-invoiced form posts the ids of the orders on screen. An order that arrived meanwhile stays uninvoiced; one that changed refuses the whole invoice with a message | What the owner checked is exactly what gets locked, so nothing is billed twice or missed |

Not in this slice:

- The overview as PDF (F11a asks screen, PDF and CSV). The PDFs live in the
  `production` crate (Typst, ADR 0016); the overview needs a template there or a shared
  PDF crate.
- Ordering on account in the shop by a signed-in business customer, and business
  customers seeing their invoices. Both go through the customer login.
- A confirmation email for orders on account. Order emails go out on payment (ADR 0022).
- "Sent" and "paid" states for invoices, and a payment term. The spec records only the
  invoice number and date; dunning and bank matching are out of scope.
