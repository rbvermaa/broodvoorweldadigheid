# Open questions

Questions about the spec. The spec is only changed when the owner asks.

- [ ] **F19 is missing.** The requirements table goes from F18a to F20. Removed on
  purpose, or lost?
- [x] **GitHub instead of Forgejo** (ADR 0001). Update the spec's stack table and the
  list of allowed external services?
  **Spec updated 2026-11-24:** stack table (GitHub, GitHub Actions, Cachix), "Rules that
  keep it safe", and a note that GitHub and Cachix hold only public code and build output.
- [ ] **Off-site backup target.** Which host or provider does the second rustic
  repository go to?
- [x] **Label fonts.** Which fonts go into the server image for PDFs and labels?
  **Answered:** Oswald and Inter, pinned by Nix (ADR 0009); the PDFs embed their TTFs (ADR 0016).
  Spec updated (Design system, brand).
- [x] **Branch protection.** Not available on a free private GitHub repo (ADR 0001).
  Done 2026-11-24: the repo is public now and `main` requires the CI jobs (ADR 0001).
- [ ] **CDLA-Permissive-2.0.** `webpki-roots` (used by rustls for TLS) has this licence.
  It is open but not OSI-approved, so `deny.toml` rejects it. Allow it when TLS lands?
  SMTP avoids it: lettre verifies with `rustls-native-certs` (ADR 0014).
- [ ] **Public donation page threshold** (F11d, ADR 0040). Amounts on `/giften` show only
  with at least 5 orders behind them. Is 5 right for the owner, and should the page show
  all-time totals only, or also per year?
- [ ] **advisory-db freshness.** cargo-audit is only as fresh as the pinned advisory-db.
  The weekly dependency-update job (spec, "Nightly and weekly") should bump it.
- [ ] **Partial refund by amount on a mixed-rate order.** How is it split across BTW
  rates? (System refunds per line for now; ADR 0005.)
- [ ] **Gate 3 names the till.** The roadmap picture says "Gate 3: till in daily use,
  two weeks at the counter", but the till is out of scope (SumUp). What is Gate 3?
- [x] Default sourdough lead time: 36 h assumed (company.default_lead_time_hours); confirm with the baker.
  **Answered 2026-11-24:** "2 days before in the evening": the deadline is at the latest
  20:00 local two days before the bake date (company.sourdough_deadline_*, ADR 0006).
- [ ] **Mollie fees need a second secret.** Per-payment fees (F11e) come from Mollie's
  balance transactions, which need an organization access token with `balances.read`,
  not the website API key (ADR 0013). OK to add that token to the sops secrets?
- [ ] **Cancelled drops on the public calendar.** F1a lists planned, menu announced, open,
  sold out and closed. A cancelled (sick-day) drop is shown as "cancelled" for now
  (ADR 0008). OK?
- [x] **Bake start time.** No default rule in the spec; staff enter `bake_starts_at` per
  bake day. Should it default from the pickup window (e.g. 04:00 on the bake date)?
  **Answered 2026-11-24:** baking always starts at 05:00 on the bake date; the server
  derives it and staff no longer enter it (ADR 0027).
- [x] **Default lead time vs. default deadline.** 36 h before a Saturday bake start
  earlier than 08:00 is before the default Thursday 20:00 deadline, so every default
  drop gets the lead-time warning. Either the lead time, the deadline or the bake start
  default is off.
  **Answered 2026-11-24:** the warning now uses the owner's rule above, which matches the
  default Thursday 20:00 deadline, so default drops no longer warn.
- [ ] **Unapproved allergens block checkout** (ADR 0012). A product without approved
  allergen data cannot be ordered, also not by staff. OK?
- [ ] **Staff orders and payment.** Staff orders wait for payment with a 15-minute hold
  like web orders. How do phone orders get paid: payment link, at pickup, on account?
- [x] **Bread vote data model** (F9b, ADR 0039). The spec has `Vote.subscriber_id`; the
  code stores the voter's email on the vote instead, so voting never puts anyone on the
  drop-alert list. Update the spec to `Vote poll_id, poll_option_id, email, wish?,
  confirmed_at?, created_at`?
  **Spec updated 2026-11-25:** the data model lists `email` and `confirmed_at` on Vote.
- [x] **Voters without an account** (F9b, F12). Their votes are only in the export and
  erase of a customer with the same address. How long may a confirmed vote keep its
  address: blank it some time after the poll closes, or add "erase by address" for the
  owner?
  **Owner, 2026-11-25:** blank the address (and wish) 30 days after the poll closes;
  the counts stay the same. Done by `jobs::cleanup` (ADR 0039).
- [x] **Failed payment, same order.** After a failed or cancelled Mollie payment the order
  stays pending until its hold ends; the customer orders again from the drop page. Should
  the status page offer "try paying again" for the same order within its hold instead?
  **Done:** a failed payment start can be retried for the same order within its hold (ADR 0015).
- [x] **Logo file.** Emails, PDFs and labels need the brand mark. Add an SVG (or the
  1024-pixel PNG) to `design/`; emails use a text wordmark until then (ADR 0014).
  **Partly answered 2026-11-24:** the PNG is in `design/` now (logo, favicon and touch
  icon, ADR 0023). Still open: an SVG for sharp labels and PDFs. Emails keep the text
  wordmark until the server knows its public URL.
  **Answered 2026-11-24:** the owner keeps the PNG for everything; no SVG is needed (the
  supplied DXF only wraps the same PNG). Spec updated.
- [x] **Accent colour.** The spec leaves "one accent colour" to choose.
  **Answered 2026-11-24:** none. The owner chose style "Stempel": ink #111111 on white,
  like the logo (ADR 0023). Spec updated.
- [ ] **Email Reply-To.** Emails say "Beantwoord deze mail". Is `MAIL_FROM` an inbox the
  bakery reads, or should replies go to another address? (ADR 0014)
- [x] **Email outbox retention.** How long do sent emails stay in `email_outbox`? A short
  period (for example 30 days) is enough for support questions (ADR 0014).
  **Answered 2026-11-25:** 90 days after an email was sent or failed for good. The cleanup
  job deletes the row, body and addresses included; pending emails stay. Emails with a
  secret keep their faster blanking (ADR 0031).
- [ ] **Company KvK number and address.** Order emails need both (Compliance), but the
  `company` row has neither and there is no form yet. Until they are filled in, payments
  go through without a confirmation email (ADR 0022). What are they, and should the
  server refuse to start without them?
- [ ] **Staff login through Kanidm is parked** (ADR 0010, branch `rv-p1-staff-oidc`).
  openidconnect 4.1 pulls in `rsa 0.9` with RUSTSEC-2023-0071, so `make audit` fails.
  Wait for openidconnect on rsa 0.10, accept the advisory (we only verify with public
  keys), or verify ID tokens without openidconnect? Until then staff can only sign in
  through the local dev login.
- [ ] **Typst advisories.** On the next Typst release, check whether quick-xml < 0.41 is
  gone and drop the two ignores in `.cargo/audit.toml` (ADR 0016).
- [x] **Payhip subscribers for drop alerts.** F9a says the list starts with subscribers
  exported from Payhip.
  Answer: F9a: no Payhip subscribers exist; no import (owner, 2026-11-24). Subscribers
  only come from sign-ups on the site (ADR 0021). Spec updated (F9a, Subscriber).
- [ ] **Refund dates in the sales report** (ADR 0017). Refunds have no date in the
  database, so the sales report takes them off on the day of the sale; a late refund
  changes a period that may already be in a BTW return. Store Mollie's refund date and
  count refunds on that day instead?
- [ ] **On-account orders on charity drops.** Do they create a donation allocation
  (F4b says they pay the suggested price), and when: at confirmation or once invoiced?
- [x] **Confirmation email for orders on account.** F9 sends an order confirmation, but order
  emails go out on payment (ADR 0022) and orders on account have none (ADR 0032). Send one
  when staff place the order, to the contact email or the invoice email?
  **Answered 2026-11-25:** yes, when staff place the order, in the same transaction. To the
  contact email with a copy to the invoice email when that differs, with "op rekening"
  wording and no payment (ADR 0032).
- [ ] **Ingredient may-contain and the kitchen list** (F17, ADR 0038). May-contain is now one
  kitchen-wide list. The data model still gives each `Ingredient` a `may_contain[]` (from
  supplier spec sheets, F21). When recipes roll up allergens, should an ingredient's
  may-contain go into the product's own allergens, be added to the kitchen list as a
  suggestion, or be dropped?
- [ ] **Paying a pending order after an allergen approval is cleared** (F10, ADR 0038). An
  order placed while its products and the kitchen list were approved can still be paid
  after staff change them (which clears the approval). Leave it payable, let staff block
  or cancel such pending orders, or check the approvals again at payment?
- [x] **Niimbot driver licence** (spec, Risks: NiimBlueLib asks not to be used
  commercially without Niimbot's consent).
  **Answered 2026-11-24:** no own Niimbot driver. The system produces each label as a
  300 dpi PNG at its exact size, plus CSV rows for the Niimbot app's import; the owner
  prints through the official Niimbot app (ADR 0024). Spec updated.
- [x] **Redirects from old Payhip links** (spec, Architecture: `/b/…` and `/contact`).
  **Answered 2026-11-24:** not needed; the owner dropped them. Spec updated.
- [x] **Spec F3 wording.** F3 still says "limits per product and per dough or oven-load
  pool". The owner dropped pools on 2026-11-24 (ADR 0026). Update F3 to "a maximum per
  product per bake day"?
  **Spec updated 2026-11-24:** F3, scope, data model, checkout lock rule and crate list.
- [x] **Spec wording on bake start and lead time** (ADR 0027). The data model lists
  `bake_starts_at` as a per-day field, and "Recipe versions and batches" plus the
  deadline rule speak of a lead time per recipe ("bake_starts_at minus the longest recipe
  lead time"). The owner decided on 2026-11-24 that baking always starts at 05:00 on the
  bake date (derived, not entered) and that only the sourdough rule (20:00 two days
  before) warns. Update the spec to match, and drop `lead_time_hours` from
  `RecipeVersion`?
  **Spec updated 2026-11-24:** F1, data model and the deadline rules. `RecipeVersion` keeps
  its lead time as information only; it no longer drives warnings.
- [ ] **Spec data model for recipe lines and kinds** (ADR 0037). A sub-recipe line points
  at a recipe version, so a frozen dough keeps the levain it was frozen with: rename
  `RecipeLine.sub_recipe_id` to `sub_recipe_version_id`? And add `preferment` and
  `soaker` to `Recipe.kind` (dough | filling | topping), so prefermented flour can be
  shown?
- [x] **Allergen approval before the roll-up** (ADR 0037). The spec says changing a
  product's recipe or an ingredient's allergens clears the product's allergen approval.
  Until F17, product allergens are entered by hand (F10) and nothing is computed from
  recipes, so this slice does not clear the approval when a recipe is linked or an
  ingredient's allergens change. Clear it now already, or wait for F17?
  **Answered 2026-11-25:** clear it now. Linking, unlinking or relinking a recipe, taking
  another version into use, editing the lines of the version in use and changing an
  ingredient's allergens or label name in it all clear the approval (database triggers,
  ADR 0037).
- [x] **Extras after batches exist** (ADR 0042). F7a says extras can change until the
  batch is mixed. Should a change resize planned batches?
  **Answered 2026-11-25:** no, the baker adjusts batches by hand (ADR 0042).
