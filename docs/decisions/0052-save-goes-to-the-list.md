# 0052 Saving an object goes back to its list

Status: accepted, 2026-11-26

Owner request: after saving a recipe (or another object) the back office should not stay
on the same page but go one level up, for a recipe to the recipe list, and the same
everywhere. Agreed rule:

| Kind of form | After a save |
|---|---|
| An object's own create or edit form: product, ingredient, recipe ("Klaar met aanpassen" too), charity (archive too), pickup location, business account, poll, new Bread Drop, a product's label settings | 303 (htmx: `HX-Redirect`) to the list above it, `?toegevoegd=<id>`, `?opgeslagen=<id>` or `?gearchiveerd=<id>`. The list says "«naam» toegevoegd", "opgeslagen" or "gearchiveerd" in its `role="status"` region and highlights that row |
| A pickup window | Its location page, which lists the windows: the window's row is named and highlighted |
| Inline htmx edits in a table (recipe lines and steps, Aanbod maximum, extras, pickup list) and live field checks | Stay in place |
| Sections of a hub page: Bread Drop (schedule, slots, charity, Aanbod, open/close, copy, repeat), batch (adjust, mix, actuals), order, recipe page (versions, label, products), version page (details, freeze, in use), product approval | Stay in place with `?opgeslagen=1` |
| A new price for an ingredient (costing) | Its costing page, which is the list of its prices, with "Opgeslagen" |
| Settings and the kitchen may-contain list | Stay, "Opgeslagen" |
| A form with mistakes | Always the form again, with the input and the errors (422; htmx 200) |

| Choice | Why |
|---|---|
| One helper, `backoffice::saved_to_list(headers, list, RowChange, id)`; lists read it with `Flash::row`, which finds the row whose link is the object's path | Reuses the existing `opgeslagen` flash and `saved()` redirect. The id goes in the URL, so it survives a reload and needs no session state |
| The name comes from the row in the list, never from the URL | A crafted `?opgeslagen=` cannot put text on the page. An id the list does not show gives the plain "Opgeslagen" and no highlight; a value that is not a number shows nothing |
| `Table::links_to` and a `saved` argument on the table macro add the class `table-row-saved` (muted surface and an ink bar, from the tokens) | No change to the 60-odd `Table` values; one place for the style |
| `apps/server/tests/save_to_list.rs` lists every back-office POST route with where it goes; a new route fails until it is added | New forms follow the rule, or say why they don't |
| A new Bread Drop outside the list (before today or more than 366 days ahead) opens on its own page; label settings do the same for a role that cannot open the products list | Otherwise the list would say "Opgeslagen" without the new object to click on |
| Repeating a Bread Drop now goes to the list with only `?herhaald=n` | `opgeslagen=1` would read as object id 1 |
| Not done: staff members, customers and FAQ items have no edit form in the back office yet | Nothing to change; the route test will ask when they come |

The batch adjust form also moves its live preview trigger to a wrapper div with
`hx-include="closest form"`, as the price form did in #62: htmx validated the whole form
before a preview, so clearing a field left stale figures.
