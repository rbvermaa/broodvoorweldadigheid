# 0016 Production list, pickup list and their PDFs

Status: proposed, 2026-11-24

**What counts.** Both lists show orders that are paid, confirmed or picked up
(`orders::order::list_firm`). A `pending_payment` order is left out even within its hold:
the production list shows what the bakery is sure to bake, while the shop (ADR 0012) keeps
counting holds against capacity. Cancelled and refunded orders never appear. Extra
production (F7a) counts per purpose, also for products that are not on the day's range.

**Production list (F7).** One row per product with something to bake, by name: ordered,
the four extra purposes, and the total to bake. Below it every capacity pool of the day
with the units its products take (`to bake × units_per_item`) and its maximum.

**Pickup list (F8).** Every pickup slot of the day by start time, also slots without
orders. Within a slot, orders by customer name (case-insensitive), then order number. Each
order shows the name and phone entered for it (ADR 0012), its lines, its status and
whether it was picked up. The order table has no notes column, so the list has no notes.

**Ticking off belongs to orders.** `orders::mark_picked_up` and `undo_picked_up` own the
status change; production only reads. Both lock the order row, take only paid or confirmed
orders (`NotPickable` otherwise), write an audit entry with the staff actor, and do nothing
on a repeat, so a double tap on a phone is harmless. Undo restores the status recorded as
`before` in the latest `picked_up` audit entry, so a confirmed order goes back to
confirmed and a paid one to paid. Undo refuses an order on a cancelled bake day
(`DayFinal`): a sick-day cancel keeps picked-up orders and refunds the rest, so undoing
there would leave a refundable order nobody refunds. The pickup screen ticks with
`manage::mark_picked_up`, the same action as the order page.

**One snapshot per list.** Each list reads inside one `REPEATABLE READ, READ ONLY`
transaction, so an order paid halfway does not show in one table and not in another.
Callers pass a connection that is not in a transaction.

**PDFs with Typst as a library.** `typst`, `typst-layout` (for `PagedDocument`, which
`typst` does not re-export) and `typst-pdf` 0.15.1, all Apache-2.0. A minimal `World` in
`crates/production/src/pdf.rs` serves the template and `/data.json` and refuses every other
file, so a template cannot read the disk or packages. `typst-kit` is not used: we need
neither its font search nor its package downloads.

| Choice | Why |
|---|---|
| Templates live in `typst/production.typ` and `typst/pickup.typ`, compiled into the binary with `include_str!` | One place for layout; the Nix source set includes `typst/` |
| Rust passes all text in `/data.json`, translated (Fluent, `locales/nl/production.ftl`) and formatted (`i18n::decimal`, dates, Europe/Amsterdam times) | No Dutch in templates. Values from JSON are inserted as text, never evaluated as Typst markup, so a customer name cannot inject anything |
| Fonts: static Inter Regular and Bold and the variable Oswald as TTF from `nix/fonts.nix` (ADR 0009), embedded with `include_bytes!(FONTS_DIR/…)` | Typst cannot read woff2. No system fonts, so a PDF looks the same everywhere |
| No creation date, no creator, a fixed document id per list and date | The same input gives the same bytes; a test checks this |
| Tests snapshot the JSON input and the laid-out text (runs per line, from Typst's frames), not PDF bytes | Readable diffs; layout changes still show up |
| PDFs are monochrome with thin black rules | Paper for the bakery, not UI; the design tokens apply to the web pages |

**Accepted advisories.** Typst 0.15.1 always depends on `hayagriva` (bibliographies). Chain:
typst-library -> hayagriva -> citationberg 0.7.0 -> quick-xml 0.38.4, which has
RUSTSEC-2026-0194 (quadratic duplicate-attribute check) and RUSTSEC-2026-0195 (unbounded
namespace allocation in `NsReader`), both fixed in quick-xml 0.41. No newer citationberg,
hayagriva or typst exists yet. The vulnerable code only parses CSL bibliography styles and
locales; our templates use no bibliography, and our `World` serves only our own template
and `/data.json`, so no untrusted XML reaches it. The owner decided on 2026-11-24 to
ignore both ids in `.cargo/audit.toml`. Remove them once a Typst release no longer pulls in
a quick-xml below 0.41; `crates/production/tests/advisories.rs` fails from then on until
the entries are gone. Typst also brings unmaintained-crate warnings (ttf-parser, rustybuzz,
yaml-rust, unsafe-libyaml, bincode, paste); `cargo audit` allows those.

**Open: names in other scripts.** Only Inter and Oswald are embedded, and Typst does not
warn about a missing glyph. A name in, say, Arabic or Chinese prints as empty boxes. A
pinned fallback font (Noto) would fix it; not added yet.

Each pickup slot heading carries its own date, because staff can move a slot to another
day than the bake day. `typst::comemo::evict` runs after each compile so memoized results
do not pile up in a long-running server.
