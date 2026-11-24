# 0015 Webshop: pages, cart, checkout and the Mollie webhook

Status: proposed, 2026-11-24

| Choice | Why |
|---|---|
| Routes: `/drops` (12 weeks ahead), `/drops.ics` (a month back to a year ahead), `/drops/{id}`, `POST /drops/{id}/mandje`, `/drops/{id}/afrekenen`, `POST /drops/{id}/bestellen`, `/bestelling/{token}`, `POST /webhooks/mollie` | One page per drop; the drop page holds the menu and the cart |
| The cart is `orders::Cart` in the session, for one drop. Adding from another drop replaces it and says so | The session already exists for CSRF. Checkout prices and checks the cart again, so a tampered cart gains nothing (ADR 0012) |
| Cart changes are one form per product tile and per cart line, posted with htmx; the server answers with the whole cart panel and a Dutch message, also for refusals (200, since htmx does not swap 4xx). Without JavaScript the post redirects back, or shows the page with the message (422) | Server-side validation only; one swap target |
| Stock on the menu and in the cart check is `range::availability`, a display read. Checkout decides under the locks | ADR 0008 |
| Product tiles take `components::ApprovedAllergens`, which only `from_catalog` builds, from `published_allergen_info()`. Without approval the tile says the allergen information follows, shows no allergens or ingredients and cannot be ordered | ADR 0009 open item; spec: never publish unapproved allergen data. `orders::checkout` refuses such products anyway |
| Checkout runs `orders::checkout` (web channel; it checks the deadline itself, ADR 0011), then `create_payment` with all four methods, metadata order id, redirect `PUBLIC_URL/bestelling/<token>`, webhook `PUBLIC_URL/webhooks/mollie` and idempotency key `order-<id>-payment`. The payment is recorded as `open` (`orders::payment_started`) and the customer gets a 303 to Mollie's checkout URL | Mollie replays the first answer for the same key, so a retried create never makes a second payment for one order |
| When the create fails, the order stays pending until its hold runs out and the customer sees "Betalen lukt nu even niet" with the cart restored | No cancel action for a pending order yet; the hold is 15 minutes |
| A cart that costs € 0 (only zero-priced products) is refused by `orders::checkout` with `Error::FreeOrder` before any hold or order exists; the customer sees "Je mandje kost € 0" | Mollie takes no payment of € 0, and every retry would fail the same way. Pay-what-you-want minimums are above zero already |
| Checkout gives its database connection back before `create_payment` and takes a new one to record the payment; the status token is read on its own connection first | A slow Mollie (up to 15 s) would otherwise hold one of the pool's ten connections per checkout and starve the other pages |
| Pages read the session before they take a database connection | The session store takes its own pool connection; holding one while waiting for another can stall every request on a full pool |
| A cart line whose product left the menu (archived, not sold online, unapproved or off the range) stays in the cart panel as "Niet meer beschikbaar", outside the total and with its remove button. Checkout keeps refusing it | The customer sees why checkout refuses and can remove it |
| No Mollie `expiresAt`: Mollie's create-payment API has no request field for it (`expiresAt` is read-only; `dueDate` is a date, not a time). A payment that completes after the hold is recorded as refund due by `mark_paid` | ADR 0012 assumed the field exists |
| The status page uses `order.status_token`, a `uuid` from `gen_random_uuid()` in a new migration, never the order number | Order numbers are sequential |
| The status page polls itself over htmx every 3 s while waiting and stops once final. Its first full load (not the polls) also asks Mollie through the webhook code, in case the webhook is late or cannot reach a local machine | One extra GET at Mollie per visit, none per poll |
| Shown status: paid, confirmed or picked up = paid; pending with the latest payment failed or canceled = failed; expired payment, passed hold or cancelled `hold_expired` = expired; else waiting | |
| The webhook sits outside the session and CSRF layers (merged next to `/health`), takes the body as bytes, uses `payments::parse_webhook_body`, then `get_payment`. Paid goes to `orders::mark_paid`; other statuses go to `orders::record_payment_status`, which updates the payment row and never the order. 400 for a malformed body; 200 for an unknown payment, a payment without order metadata, one recorded for another order (logged as error) and every recorded outcome; 503 when Mollie or the database fails, so Mollie retries | Mollie retries non-2xx answers; a 4xx for unknown ids would only cause retries. Repeats change nothing (ADR 0012) |
| A payment row now exists from the moment the payment is created (`open`), and `mark_paid` turns it into `paid` in place. Reports must count `status = 'paid'` rows only | Keeps every Mollie payment visible to staff, also failed ones |
| Rate limit: the checkout POST only, 5 at once then one per 12 s per client, outermost, with its own session and CSRF layers (ADR 0007 open item) | A blocked client costs no session write. Cart and page views are not limited |
| CSP `form-action 'self' https://www.mollie.com` | Browsers apply form-action to the redirect after the checkout post |
| Config: `PUBLIC_URL` (default `https://broodvoorweldadigheid.nl`, an origin without path; also the iCal UID domain), `MOLLIE_API_KEY_FILE`, `MOLLIE_API_URL`. Without a key file the server runs and checkout answers "Betalen lukt nu even niet" | The stack and tests run without secrets |
| New components: `drop_facts`/`drop_card`, `cart_panel`; the product tile gains the order form, stock and ingredients; the pickup picker allows a slot without limit. All in the gallery | "Design system and UX": pages only combine components |

Not in this slice: choosing another charity or saving the donation (F4d), confirmation emails (F9).

The jobs runner: with apps in layer 7 (ADR 0020) the server starts it next to the web
server (`server::background`). It runs `jobs::setup`, then `jobs::run` with the worker id
`hostname-pid` and the same shutdown signal as axum. After any failure, such as a database
restart, both start again 5 s later with a new worker id. Setup gives way to shutdown, and
the server waits for the runner before it exits.

Known limits, from review:

- When the payment cannot be started, the session remembers the order (`unpaid_order`) and
  saves itself explicitly, since tower-sessions does not save on a 5xx answer. The next
  checkout of that drop pays that order again with the same idempotency key, so Mollie
  returns the same payment when the first create did reach it. The second try keeps the
  first order's details.
- Two checkout posts of one session at the same moment (a double click past the browser)
  both read the cart and make two orders with two holds. A claim per session would need a
  lock or a column; the rate limit and the 303 make it rare. The second order's hold runs
  out unpaid.
- Two cart changes of one session at the same moment: the last session save wins, so one
  change can be lost. The cart panel that comes back shows what was kept.
- `MOLLIE_API_KEY_FILE` in compose names a path inside the container; mount the file with
  a compose override, for example a `secrets:` entry that lands on that path.
- The webhook asks Mollie about any well-formed `tr_` id it receives, without a rate limit.
  Ignoring ids without a payment row would lose a payment whose `open` row failed to save,
  so it stays as is for now.
