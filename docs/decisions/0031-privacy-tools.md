# 0031 Privacy tools: export, consent and erasing a customer

Status: proposed, 2026-11-24

F12 and the GDPR (AVG) row of the spec's Compliance table. Logic in `orders::privacy`,
`notify::subscribers`, `notify::outbox`, `auth::customer` and `jobs::cleanup`; pages in
`apps/server` (`privacy`, `backoffice::customers`).

| Choice | Why |
|---|---|
| A signed-in customer gets three things on "Mijn bestellingen": a download of their data, a drop-alert checkbox, and a delete button behind a confirm dialog. The owner (new `Permission::Privacy`, owner only; added to the ADR 0010 table test) gets the same export and delete in `/staff/customers`, for requests by email or phone | The spec asks for export, deletion and marketing consent. A customer without a login can ask the bakery; the owner handles it |
| The export is one JSON file: profile, business details if any, orders with lines, payments and donations, logins, login links, and the drop-alert subscription of the same address. Emails in the outbox are left out: they are copies of what the customer received or sent Amounts in cents, times in UTC (RFC 3339). Sent `private, no-store` as an attachment | The spec does not name a format. Nested JSON holds orders with their lines in one file; CSV would need several |
| Orders, payments, refunds and donation allocations are kept (spec: business records 7 years). Erasing blanks what identifies the customer instead: `customer.name` and every `order.customer_name` become `-`, phone numbers and `marketing_opt_in` are cleared, `customer.email` becomes `erased-<id>@erased.invalid`, `erased_at` is set. Status links get a new random token | Amounts, dates, products and BTW stay, so sales, payouts, donation reports and the contribution report do not change. The columns cannot be empty, and `.invalid` can never receive mail. Old status links in old emails stop working |
| Saved donations stay saved with their amount and date; the choose link is replaced by the hash of a random value nobody holds. On its date the job moves it to the next drop's charity, without an email for an erased customer | The money still goes to a charity, and the charity reports keep it. Nobody can reach the customer any more |
| Login links and logins are deleted, which signs every browser out; the session that asked also gets a new id. The subscriber row of the address and its unsubscribe tokens are deleted, and so is every outbox email to or from the address | Nothing that holds the address stays. The audit log keeps only `customer:<id>`, `erased`, no data |
| Erasing locks the customer, then its donation allocations, then its orders, before it checks for open orders. Checkout refuses `.invalid` addresses | The same order as choosing a charity (allocation, then order), so the two cannot deadlock; a pickup undone at the same moment is seen. Nobody can take an erased address, or block an erase by holding it |
| Checkout locks the customer row (a no-op `ON CONFLICT DO UPDATE`), and erasing takes it `FOR UPDATE`, so an erase waits for a checkout of the same address and then sees its order. A link redeemed or a login read for an erased customer gives nothing | A checkout could otherwise attach a new order, with a new name, to an erased customer; a login started in the gap between redeeming a link and signing in would otherwise still work |
| One account-deleted email is queued in the same transaction, before the address is wiped. The cleanup deletes that outbox row once it is sent or failed. If the email cannot be composed (company details missing) the account is erased anyway, with a warning in the log | The customer hears it is done. The right to erasure does not wait for company details |
| Refused, with a message, while an order still waits for payment or pickup (pending, or paid or confirmed with its pickup window not yet over), and for business customers | The counter needs the name at pickup. A business customer's details are on hand-made invoices (F11a), and invoiced orders cannot change |
| The drop-alert checkbox writes `customer.marketing_consent` and signs the address up as confirmed, or takes it off like an unsubscribe link. No confirmation email | Drop alerts are the only marketing mail, so the subscription is the consent; the export reports it from there, because an unsubscribe link or a sign-up on `/meldingen` does not touch `customer.marketing_consent`. The customer proved the address by signing in with a link sent to it |
| `jobs::cleanup` runs every tick: expired login links (used or not), logins past their idle or absolute timeout, and finished account-deleted emails. `jobs` now depends on `auth` | The follow-up of ADR 0029. Links count against the rate limit only within 15 minutes and live 30, so deleting expired ones never lifts the limit |
| The owner searches by email with a POST | The address never lands in a URL, a browser history or an access log. Handlers log no address or name |

Follow-ups:

- Erased orders show `-` as the name in the back office. A label like "verwijderd" would
  need a Fluent text in every list that shows names.
- Sent emails to other customers (with names in the greeting) stay in `email_outbox` until
  its retention question is answered (open questions).
- Unconfirmed subscribers and old unsubscribe tokens (ADR 0021) are not cleaned up yet.
- The processing register and the processor agreements (spec, Compliance) are paperwork
  outside the system.
- Orders that were never paid (hold expired) are kept and blanked like paid ones. They
  are not business records and could be deleted, but they hold order numbers and audit
  entries; deleting them needs its own rules.
- A paid order not collected in its pickup window no longer blocks erasing. A late pickup
  at the counter then shows the name `-`.
- The JSON keys are English and the export has no Dutch explanation next to it.
