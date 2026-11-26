# 0052 Saving an object goes back to its list

Status: accepted, 2026-11-26

Owner request: after saving a recipe (or another object) the back office should not stay
on the same page but go one level up, for a recipe to the recipe list, and the same
everywhere. Agreed rule:

| Kind of form | After a save |
|---|---|
| An object's own create or edit form: product, ingredient, recipe ("Klaar met aanpassen" too), charity (also archive), pickup location, business account, poll, new Bread Drop, a product's label settings | 303 (htmx: `HX-Redirect`) to the list above it, `?toegevoegd=<id>` or `?opgeslagen=<id>`. The list says "«naam» toegevoegd" or "«naam» opgeslagen" in its `role="status"` region and highlights that row |
| A pickup window | Its location page, which lists the windows: the window's row is named and highlighted |
| Inline htmx edits in a table (recipe lines and steps, Aanbod maximum, extras, pickup list) and live field checks | Stay in place |
| Sections of a hub page: Bread Drop (schedule, slots, charity, Aanbod, open/close, copy, repeat), batch (adjust, mix, actuals), order, recipe page (versions, label, products), version page (details, freeze, in use), product approval, price of an ingredient | Stay in place with `?opgeslagen=1` |
| Settings and the kitchen may-contain list | Stay, "Opgeslagen" |
| A form with mistakes | Always the form again, with the input and the errors (422; htmx 200) |

| Choice | Why |
|---|---|
| One helper, `backoffice::saved_to_list(headers, list, RowChange, id)`; lists read it with `Flash::row`, which finds the row whose link is the object's path | Reuses the existing `opgeslagen` flash and `saved()` redirect. The id goes in the URL, so it survives a reload and needs no session state |
| The name comes from the row in the list, never from the URL | A crafted `?opgeslagen=` cannot put text on the page. An id the list does not show (archived, out of range, not a number) gives the plain "Opgeslagen" and no highlight |
| `Table::links_to` and a `saved` argument on the table macro add the class `table-row-saved` (muted surface and an ink bar, from the tokens) | No change to the 60-odd `Table` values; one place for the style |
| `apps/server/tests/save_to_list.rs` lists every back-office POST route with where it goes; a new route fails until it is added | New forms follow the rule, or say why they don't |
| Repeating a Bread Drop now goes to the list with only `?herhaald=n` | `opgeslagen=1` would read as object id 1 |
| Not done: staff members, customers and FAQ items have no edit form in the back office yet | Nothing to change; the route test will ask when they come |

The batch adjust form also moves its live preview trigger to a wrapper div with
`hx-include="closest form"`, as the price form did in #62: htmx validated the whole form
before a preview, so clearing a field left stale figures.
