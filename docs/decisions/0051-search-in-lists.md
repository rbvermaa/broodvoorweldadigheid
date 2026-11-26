# 0051 Search in lists

Status: accepted, 2026-11-26

F24: a search field above the long back-office lists that filters as you type, works
without JavaScript, ignores case and accents and keeps the term in the URL.

| List | Matches |
|---|---|
| Products, ingredients (and supplier), recipes (and their products), charities (and website), polls | Name or title |
| Business customers | Company name, KvK number, invoice email |
| Bread Drops | Date written out ("2 december", "zaterdag 5") or as numbers ("02-12"), location, charity |
| Orders (`/staff/orders`) | The Bread Drops by date only |
| Orders of one Bread Drop | Order number, customer name, email; only the number once the customer was erased (F12). The status and location filters are kept |

| Choice | Why |
|---|---|
| `?q=` on a GET form (`templates/components/search.html`), one shared `backoffice::Search` | Bookmarkable, and a plain submit works without JavaScript |
| htmx on the form: `hx-get`, triggered by submit, typing (`input changed delay:250ms`), clearing (`search`) and a changed filter; `hx-sync="this:replace"`; only `#resultaten` swaps; `hx-replace-url` | The field keeps focus, Enter does not reload the page, the URL follows without a history entry per keystroke |
| A list's other filters (status, location on a Bread Drop's orders) sit in the same form | Every search sends the filters as they are now, and the other way round |
| A polite live region in the form says how many rows were found ("3 gevonden", "Niets gevonden"), updated out of band | Screen reader users hear the result without leaving the field (WCAG 2.2 AA) |
| Every word of the term must be in one of the row's fields | "test klant" and "5 december" find what people expect |
| Filtering in Rust (`i18n::search_key`: lower case, a Latin-1 and Latin Extended-A fold, combining accents dropped), not Postgres `unaccent` | `unaccent` is in our PostgreSQL 17, but each list already loads all its rows (none is paged), so the term never reaches SQL and needs no extension migration. Revisit if a list gets paged; then use `unaccent` with sqlx binds |
| The term is cut to 100 characters and only ever shown escaped | No injection into SQL (none is run with it) or HTML |
| Empty result: "Niets gevonden voor «q»" in the empty-state component | One component for every list |
| "/" focuses and selects the field (a few lines in `design/app.js`), not while typing in another field or with a modifier key | Keyboard users get to the search fast |
| `/staff/customers` keeps its POST exact lookup | ADR 0031: an address never lands in a URL there. It is also not a list |
| The term can hold an email address, so it must not reach logs. The server has no request logging and logs no URLs; a test captures all tracing during searches and checks the address is absent. The Caddyfile has no `log` directive, so Caddy writes no access log | Same reason as ADR 0031. Whoever turns on access logging must drop the query string (Caddy: a `log` filter on `request>uri`) |
| Searching orders across all Bread Drops is not done | It needs a new query; it is in `docs/open-questions.md` |
