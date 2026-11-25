# 0032 Business accounts, staff orders on account and the invoice screen

Status: proposed, 2026-11-24

Back-office part of F11a, on top of the tables and the overview in ADR 0017.

| Choice | Why |
|---|---|
| The owner opens an account with a contact email and name. The email is the account's `customer`; a new email creates the customer, an existing one is linked as it is and never changed. The contact email cannot be changed afterwards | Same rule as checkout (ADR 0012): anyone can type any email. Moving an account to another customer would move its order history |
| Opening an account reads an existing customer `FOR SHARE` and refuses one that is erased or being erased (F12, ADR 0031). Erasing refuses a customer with a business account | An account never points at an erased customer; its orders stay with the bookkeeping |
| Opening and changing an account write `business_account` audit entries (`created`, `changed` with before and after) | Who allowed whom to order on account, and when |
| New permission `Invoicing` (owner only) for opening and changing accounts, the overview, its CSV and marking orders invoiced. The account list and placing an order on account need `EditOrders` (owner, baker); without `Invoicing` the list shows only the order links | Invoices are money matters like the reports. Taking a phone order from a café is daily work |
| The contact email and the company name must pass checkout's customer check (`orders::check_customer`) when an account is opened or changed | Every order on account carries them; an account that checkout refuses could never order |
| The order form posts the product ids it showed; a product no longer on the range or for sale refuses the whole order | A partial order would be confirmed without anyone noticing |
| `orders::checkout_on_account`: staff only (channel `manual`), confirmed straight away, no hold and no payment. Pay-what-you-want lines take the suggested price, whatever price the cart holds (F4b). The order's name is the company name | Spec, "Data model": on-account orders skip Mollie. The pickup list shows the company |
| It follows checkout's lock order (ADR 0012) and reads the `business_account` row `FOR SHARE` after the customer step, before the order number. No account, or `on_account_allowed` off, is `NotOnAccount` and rolls back, also a customer it had just created | Turning the account off waits for a running order and then wins |
| No donation allocation for an order on account, also on a charity drop | Unchanged behaviour; see the open question "On-account orders on charity drops" |
| Placing an order on account queues a confirmation in the same transaction (owner, 2026-11-25): to the contact email, with a Cc to the invoice email when that differs (`email_outbox.cc_address`, sent with lettre's Cc). Like the normal confirmation (lines, BTW per rate, ophaalmoment) but with "op rekening" wording, no payment, no "jouw prijs" and no status page link. Kind `order_confirmation`, dedupe key `order-confirmation:<order>`. Queued in a savepoint like ADR 0022: without company details the order is still placed and the missing email is logged. A relay that refuses the Cc address fails the whole email, To included | The status page speaks of a received payment. Staff should not be blocked by missing KvK details. Erased customers cannot get one: erasing refuses business customers and checkout refuses erased addresses |
| The overview's period defaults to this calendar year, dated by drop (ADR 0017). The mark-invoiced form posts the ids of the orders on screen. An order that arrived meanwhile stays uninvoiced; one that changed refuses the whole invoice with a message | What the owner checked is exactly what gets locked, so nothing is billed twice or missed |

Not in this slice:

- Double submits. Each post of the order form places an order; post-redirect-get stops a
  page reload, but a double click or a retry after a lost response makes two confirmed
  orders. Staff see both on the order list and cancel one before invoicing. A submission
  key needs a new column; it comes with the shop's order on account.
- Tying the CSV and a refused invoice form to the order set first shown. The CSV and a
  form sent back after a mistake (such as a used invoice number) show the orders
  uninvoiced at that moment, so an order placed in between can appear in them. The owner
  sees it in the table above the form; a snapshot of the shown set would need storage.
- The overview as PDF (F11a asks screen, PDF and CSV): done in ADR 0034.
- Ordering on account in the shop by a signed-in business customer, and business
  customers seeing their invoices. Both go through the customer login.
- KvK numbers are checked in both the form and the `reports` crate; BTW-ids only for
  being filled in.
- "Sent" and "paid" states for invoices, and a payment term. The spec records only the
  invoice number and date; dunning and bank matching are out of scope.
