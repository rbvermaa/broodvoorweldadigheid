# 0030 Choose another charity and saved donations

Status: proposed, 2026-11-24

F4d, in `orders::donations`, the `jobs` tick and two pages in `apps/server`.

| Choice | Why |
|---|---|
| The choice is made at checkout: a select on charity drops with the drop's charity (default), the other active charities, and "bewaren". It is stored on the order (`donation_charity_id`, `donation_saved`) in the transaction that places it, or again before a retried payment, and `mark_paid` follows it | The spec puts the choice at checkout. The webhook needs nothing from the session. A refused choice (an unknown or archived charity) rolls back the order, so it never holds stock or cancels a replaced order |
| A saved donation is an allocation with status `saved`, no charity, a choose link and `choose_by`: local midnight 3 months (`SAVE_MONTHS`) after the payment's day in Amsterdam | The data model already had `saved` and `choose_by`. Local midnight keeps the date in the email exact over summer and winter time |
| The link is 32 random bytes, base64url; only its SHA-256 is stored, in the new `choose_token_hash`. The old plaintext `choose_token` is never written. The link is in the order confirmation, which is queued expiring at `choose_by`, so its body is blanked once sent | Like login links (ADR 0029): the database alone cannot be used to choose. The confirmation template already had a link for it |
| `GET /donatie/kies/{token}` shows the active charities; `POST`, with the CSRF token, chooses. Signed-in customers get the same form per saved donation on "Mijn bestellingen" (`POST /mijn-bestellingen/donatie/{id}`, own orders only). Choosing makes the allocation `earmarked` for that charity and clears the link and date, with an audit entry | One rule for both ways in. A mail scanner opening the link changes nothing. The link works only once, only before `choose_by`, and only while the order is still a sale |
| Every tick moves saved donations past `choose_by` to the charity of the next drop: the first from today (Amsterdam) that is open or closed for orders, takes orders online and has an active charity. Draft drops are skipped. One donation per transaction, `FOR UPDATE SKIP LOCKED`, with a `donation-moved:<allocation>` email in the same transaction. Without such a drop the donation waits for a later tick | Safe to repeat and to run on two servers. An email that cannot be composed is logged and the donation moves anyway, so one bad address cannot block the rest |
| A cancelled order's saved donation cannot be chosen; it is still moved, without an email | The money goes back, so its net is zero in the reports, and the customer has nothing to choose |
| Reports need no change: payouts and donation reports already read allocations and subtract refunds as soon as they are requested (ADR 0028). A moved or chosen donation counts for its new charity, dated by the order's own drop | A drop's total and a charity's total stay the same money |
| The site header links to "Inloggen", or "Mijn bestellingen" when the session has a customer login. A middleware reads only the session key and passes it to `Layout` through a task-local | No change to the 30 pages that build a `Layout`. Reading the key loads the session row on requests that carry a session cookie; the login row is not read. Staff pages hide the link |

Follow-ups:

- If the confirmation cannot be queued (for example company details missing), the payment
  still counts and the choose link is lost. The customer can still choose on "Mijn
  bestellingen" after logging in; nothing tells them so.
- A moved or chosen donation keeps its own drop's date, so it can land in a past period of
  the payout overview: it shows in `to_transfer` now but changes the older period's
  `received`. A "late arrivals" line would make that visible.
- The moved email shows the donation's full amount, not net of refunds. Cancelled orders
  get no email; partial refunds do not exist yet.
- The 3-month period is a constant. The spec calls it a setting; the back office has no
  field for it yet.
- On "Mijn bestellingen" a refused choice (a charity archived in between) only redirects
  back, without a message.
- A saved donation on a drop whose charity is later archived, or with no coming charity
  drop, waits without telling anyone. The payout overview shows it as saved.
- The home page has the hero header and so no login link.
- `choose_token` (plaintext) can be dropped in a later release.
