# 0040 Public donation page

Status: proposed, 2026-11-25

F11d, in `reports::public`, `apps/server/src/public_donations.rs` and a new
company settings page.

| Choice | Why |
|---|---|
| `/giften` shows all-time totals: overall, per drop (newest first) and per charity, plus "Nog te verdelen" for saved donations without a charity yet. Amounts come from the owner's report functions (`per_drop` net and `per_charity` all-time), over the same allocations with refunds subtracted as soon as they are requested (ADR 0028) | One set of numbers for the owner and the public. A drop's total and a charity's total stay the same money |
| **Small-number rule:** an amount is shown only when at least 5 orders (`MIN_ORDERS`) still give something to it; fully refunded orders do not count. Within each breakdown (drops; charities plus "Nog te verdelen"), while the hidden amounts together hold 1 to 4 orders, the shown amount with the fewest orders is hidden too. The overall total needs 5 orders as well. A hidden amount reads "Niet apart getoond"; the page says why | A drop with one order would show that customer's amount. Hiding only that drop is not enough: the total minus the shown drops would give it back. With the extra hiding, anything that can be worked out is a sum of at least 5 orders |
| No names, emails, order counts or single amounts on the page; charity names are shown | Charities are public organisations; customers are not |
| The owner switches the page on at `/staff/settings` (new permission `Settings`, owner only, POST with the CSRF token). `company.public_donations`, off by default. Off means `/giften` is a 404 | The spec: "switched on when you are ready" |
| The footer links to `/giften` only while it is on. A middleware reads the switch for every page through a copy cached per server for 30 seconds (`Switch`), set at once on the server that saved it, and passes it to `Layout` through a task-local, like the login link (ADR 0030) | No change to the pages that build a `Layout`, and no query per request. Another server shows or drops the link within 30 seconds; `/giften` itself always reads the database |
| `Cache-Control: private, max-age=300` | Short browser caching. Not `public`: the header differs for signed-in customers |
| One server-rendered bar chart per charity with the existing `BarChart` component; hidden amounts are left out of it | Design tokens and components only |

Limits:

- Totals change over time. Someone who sees the page before and after one order, or
  one refund, learns that order's amount. Live totals cannot avoid this.
- The two breakdowns are not suppressed against each other. A donation moved to
  another charity (F4d) can, in rare cases, be worked out from a drop total and a
  charity total. The amount is not tied to a name.
- Someone who knows 4 of the 5 amounts behind a total can work out the fifth.
