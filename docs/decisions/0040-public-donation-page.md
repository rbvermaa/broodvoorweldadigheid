# 0040 Public donation page

Status: proposed, 2026-11-25

F11d, in `reports::public`, `apps/server/src/public_donations.rs` and a new
company settings page.

| Choice | Why |
|---|---|
| `/giften` shows all-time totals: overall, per drop (newest first) and per charity, plus "Nog te verdelen" for saved donations without a charity yet. Only drops whose ordering has closed (`closed` or `done`) count; open drops are left out until the owner decides otherwise. Amounts come from the owner's report functions (`per_drop` net and `per_charity` all-time), over the same allocations with refunds subtracted as soon as they are requested (ADR 0028) | One set of numbers for the owner and the public. A drop's total and a charity's total stay the same money |
| **Small-number rule:** an amount is shown only when at least 5 orders (`MIN_ORDERS`) still give something to it; fully refunded orders do not count. The page holds sums a visitor can use: the total and its drops, the total and its charities plus "Nog te verdelen", and each charity and its drops (taken as exact). After hiding, `reveals` plays the visitor, who also knows that an amount nobody gives to is zero (a cancelled drop is public): any sum with one unknown amount gives it away, and that amount becomes known. If an amount of 1 to 4 orders becomes known, the shown amount with the fewest orders in the sum that gave it away is hidden too, and the check starts over. The overall total follows the same rule, so it can be hidden as well while there are few drops. A hidden amount reads "Niet apart getoond"; the page says why | A drop with one order would show that customer's amount. Hiding only that drop is not enough: the total minus the shown drops, or its charity minus the charity's other drops, would give it back. Amounts hidden as cover can in turn be worked out from other sums, so the check follows every chain. With the extra hiding, anything that can be worked out is a sum of at least 5 orders |
| No names, emails, order counts or single amounts on the page; charity names are shown | Charities are public organisations; customers are not |
| The owner switches the page on at `/staff/settings` (new permission `Settings`, owner only, POST with the CSRF token). `company.public_donations`, off by default. Off means `/giften` is a 404 | The spec: "switched on when you are ready" |
| The footer links to `/giften` only while it is on. A middleware reads the switch for every page through a copy cached per server for 30 seconds (`Switch`), set at once on the server that saved it, and passes it to `Layout` through a task-local, like the login link (ADR 0030) | No change to the pages that build a `Layout`, and no query per request. Another server shows or drops the link within 30 seconds; `/giften` itself always reads the database |
| `Cache-Control: private, max-age=60` | Short browser caching. Not `public`: the header differs for signed-in customers |
| One server-rendered bar chart per charity with the existing `BarChart` component; hidden amounts are left out of it | Design tokens and components only |

Limits:

- Totals change over time. Open drops are left out, so a total does not move with each
  new order, but someone who sees the page before and after one late refund, or a drop
  closing, learns that difference. Live totals cannot avoid this.
- A charity and its drops are treated as an exact sum. A donation moved to another
  charity or saved (F4d) makes them differ, so in rare cases that one amount can be
  worked out. It is not tied to a name.
- The footer switch is cached per server; a read that started just before a save can
  put the old value back for up to 30 seconds on that server. `/giften` reads the
  database itself.
- Someone who knows 4 of the 5 amounts behind a total can work out the fifth.
