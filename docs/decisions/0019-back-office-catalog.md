# 0019 Back office: catalogue, locations, charities, bake days

Status: proposed, 2026-11-24

The first back-office screens under `/staff` (F1, F3, F4a, F4c, F4g, F7a, F10). All routes
live in `apps/server/src/backoffice/` and join the router in one place
(`backoffice::routes()`).

| Choice | Why |
|---|---|
| Every form is a plain `POST` that redirects (303) to the page with `?opgeslagen=1`, which shows the one save confirmation. A form with mistakes comes back as the full page with 422 and a message per field | Works without JavaScript. One way to save, one confirmation |
| Inline checks: each checked field sits in a wrapper (`staff/macros.html`, `live`) that posts the whole form to a `…/check` route on `change` and swaps in the same wrapper from the answer (`hx-select`) | No per-field endpoints and no JSON; the server rules are the only rules. `change` (leaving the field) instead of every keystroke, so a swap never eats what someone is still typing |
| htmx answers are 200 even for a refused form | htmx does not swap 4xx/5xx by default (ADR 0007's open item); plain posts keep 422 |
| Confirmation dialogs (archive, open, close, remove) answer htmx with `HX-Redirect` to the saved page. A refusal (say, the deadline has passed) comes back as an error alert in the dialog's result region; without htmx it is the page with the alert and 409 | The dialog closes on success, as app.js already does |
| Extra production updates one line at a time with htmx: the line's form swaps itself (`hx-select`) and the capacity table (`hx-select-oob`) | Mobile first: a baker changes one number without reloading the page |
| Permissions (ADR 0010): products and allergen approval need `EditAllergens`; pickup locations, bake days, range, pools and a drop's charity need `ManageBakeDays`; charities need `ManageCharities`; extra production needs `Production` | The product form holds the allergens and ingredient text, so one permission covers the whole form. Counter staff see none of these screens |
| Allergen approval sends back exactly what the page showed (hidden fields) and calls `products::approve_allergens`, which refuses data that changed in the meantime (409 with a Dutch message). Line breaks are stored as LF, since browsers post CRLF | Nothing is approved unseen |
| An edit that changes allergens, may-contain or ingredient text redirects with `herkeuren=1`, and the page says the approval is gone (the database trigger cleared it) | Staff see the effect of their edit |
| Product fields not on the form (photo, dough per piece, freezer days, packaged, estimated cost) keep their stored value on edit; new products get none or zero | Later slices add them; editing now must not wipe them |
| `catalog::products::list_all` lists inactive products too | Staff must be able to reactivate a product |
| `AppState` holds the clock (`Arc<dyn Clock>`, the system clock by default, `with_clock` for tests) | Status changes and archiving need "now"; tests set it |
| The table component's `Cell` can be a link (`Cell::link`) | Lists open their item; the gallery shows it |
| The design-system check also reads ids passed as `t(state, "…")`, `t_with` and `t_count` | The back office builds texts in Rust; a missing id must fail the check, not render as the id |
| Availability, extra production and the charity rule count orders through `orders::Orders` with the app clock, so a pending order holds capacity until its hold expires | ADR 0008, ADR 0012 |
| The lead-time warning names the rule that sets the latest deadline: the bakery's sourdough deadline (`company.sourdough_deadline_*`) or a product's own lead time | Staff know what to change |
| Update (ADR 0027): only the sourdough rule warns; per-product lead times are gone | Owner, 2026-11-24 |
| Pickup slots are edited per bake day (F1, F4a): date, start and end, add, remove. A slot without orders has an inline form; a slot with orders is edited in a confirmation dialog that says the customers get an email. The dialog component takes fields as the body of its `call` | Owner: "should probably allow easy changing". Moving bread that people already ordered should not be one click away |
| A slot never starts before baking does (`bake_starts_at`), may move to another date, and is removed only when no order uses it, cancelled ones included. A day with online ordering keeps at least one slot | The order's foreign key keeps the slot; the rule that an online day has a slot stays as it was |
| A change of date or time queues a `pickup_moved` email to every paid or confirmed order in the slot, in the same transaction, without a dedupe key, and withdraws its unsent pickup reminder, as moving one order does (ADR 0022). Saving the same times sends nothing | Each real change is news for the customer; the reminder job queues a new reminder with the new time |

Open:

- A slot's order limit (`max_orders`) is kept on edit but not shown; staff cannot set it yet.
- The charity select on a bake day uses the select component's empty "Kies…" option for
  "normal sale"; a labelled option would be clearer.
- An inline check swaps the field wrapper with the value the request carried. On a slow
  connection, text typed into that field after leaving it and before the answer arrives
  is replaced. Swapping only the error text needs a change to the field component.
- Counter staff see the back-office links but get the 403 page behind them; the nav does
  not yet follow the role.
- Repeating weekly reports dropped (archived) charities only on a single copy, not per
  week in the repeat summary.
