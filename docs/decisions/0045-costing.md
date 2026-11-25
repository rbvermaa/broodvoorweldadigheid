# 0045 Costing: ingredient prices, standard and batch cost, variance

Status: proposed, 2026-11-25

F16: ingredient prices with a valid-from date, standard cost per product, actual cost per
batch and the variance split into recipe, price and yield. Pure maths in `recipes::cost`,
prices and standard cost in `recipes::prices`, batch cost in `production::costing`.
Screens under `/staff/costing`, owner only (new permission `Costing`).

| Choice | Why |
|---|---|
| A price is whole cents per kilogram (`ingredient_price.price_per_kg_cents`), as in the spec. No purchase units; the form hint says to divide a sack's price by its kilos | One unit for every ingredient keeps the maths and the screens simple. Cents per kg is fine enough: 1 g at €0,01/kg is 0,001 cent |
| Prices are only added. A trigger refuses UPDATE and DELETE. A second price with the same valid-from date counts from then on (highest id wins); both stay in the history | History is never overwritten, and a typo is fixed by entering the price again |
| The price on a date is the row with the latest `valid_from` on or before it. `valid_from` is a date in Europe/Amsterdam; "today" is the injected clock's date there (`prices::today`) | Bake dates are local dates too, so a price from 2 December counts from 00:00 Amsterdam time |
| Price 0 is allowed | Tap water costs nothing but must not count as "no price" |
| An ingredient without a price makes the cost unknown, never too low. Screens list the missing ingredients | Same rule as missing fees and estimates in F11e |
| Costs are exact integers in thousandths of a cent (grams × cents per kg). Only shown figures are rounded, half up to cents | No floats, no rounding until the end |
| **Standard cost per piece**: the recipe's version in use, at the prices valid on a date (today on the screens), scaled to 1000 pieces of `dough_g_per_piece` with the rounding of ADR 0037, ÷ 1000 | Scaling one piece to whole grams would round 2 g of salt by up to half a gram; on 1000 pieces the rounding is below 0,001 g a piece |
| **Actual cost of a batch**: its own lines (sub-recipes resolved from their frozen versions) scaled to its planned dough (ADR 0042), at the prices valid on the bake day's date. Per good piece: that cost × dough per piece ÷ (good pieces × dough per piece), good = baked − waste | Spec: "batch lines × prices valid on the bake day ÷ good pieces". The bake day's date, not the code date, in case the day moved |
| The batch's standard is the version the batch was made from, not the version in use now | The recipe variance must show the baker's adjustments, not a later recipe change |

**Variance.** For a batch with planned dough W grams and good pieces weighing G grams of
dough, with Ps the standard prices (the date picked; today on the screen) and Pa the bake
date's prices, the exact costs are

| Point | Cost |
|---|---|
| s0 | version at Ps for W × G ÷ W: what the good pieces should cost |
| s1 | version at Ps for W |
| s2 | batch lines at Ps for W |
| s3 | batch lines at Pa for W: the actual cost |

Each point is rounded half up to cents (r0…r3), and the parts are the steps between them:
yield = r1 − r0, recipe = r2 − r1, price = r3 − r2, total = r3 − r0 = actual − standard.
This is the usual chained (sequential) substitution: change one factor at a time, in the
order yield, quantities, prices. Because the parts are differences of the same rounded
points they add up to the total exactly, and each is less than one cent from its exact
value. A property test checks both. More good pieces than planned gives a negative
yield part.

**F11e.** "Your contribution" now prices an order line per drop with
`production::costing::unit_costs`. When the drop has baked batches of the product with
good pieces, the cost per piece is all their cost over all their good pieces; batches that
were all waste count too, so a lost batch is still a cost. A batch with several products
shares its cost by the dough of their good pieces (planned pieces when none came out
good). When one of those batches has an ingredient without a price, the cost is unknown
and the product is listed as missing, not replaced by a lower standard. Without baked
batches: the standard cost at the drop date's prices, else the product's
`estimated_cost_cents`, else missing. `estimated_cost_cents` stays as that fallback.

The spec puts showing variances in `reports`. The variance is a figure per batch with no
period or totals, so the screen reads it from `production::costing` directly; a variance
report over a period would go in `reports`.

Open:

- Standard prices are "today's", so the price part of an old batch's variance moves when
  prices change. A fixed standard date (for example when the selling price was set) would
  need its own setting.
- A backdated price changes the cost of batches already baked on or after that date.
  Allowed for now, so a late invoice can be entered.
- F11e uses the version in use now for the standard fallback, not the version in use on
  the drop date; there is no history of `current_version_id`.
- No margin view yet (selling price excl. BTW minus cost).
- The contribution report prices every drop in the period (batches, prices, standard
  costs per drop); fine for a month, slow for years of drops.
