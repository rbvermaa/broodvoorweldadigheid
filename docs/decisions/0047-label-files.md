# 0047 Label files: sizes, PNG rendering, CSV and freezer pieces

Status: proposed, 2026-11-25

F18 (product labels) and F18a (freezer labels), built on ADR 0024 (files for the official
Niimbot app) and ADR 0046 (the F17 label check).

| Choice | Why |
|---|---|
| The `labels` crate (layer 4) only lays out a `labels::Label` it is given: Typst template `typst/label.typ`, PNG, CSV. `production::label_files` (layer 5) builds the label: it calls `label_check::label_allowed` first, then checks the label settings, and is the only way to a `BatchLabel`; the files come only from a `BatchLabel` | `labels` sits below `production` in the spec's layers, so it cannot call the check itself. `BatchLabel` has private fields and getters, so it cannot be built or changed outside `production`. The server still depends on `labels` for `Side` and its errors, so code there could render a hand-built `labels::Label`; review keeps that out |
| Order of refusals: the allergen check (`Error::LabelBlocked`), then what else is missing (`Error::LabelGap`): company address, packaged, shelf life, storage instruction, freezer period, actual pieces, good pieces, frozen pieces | CLAUDE.md: never print without the approval flow; when in doubt, block. Each gap has its own Dutch reason on the page |
| Labels are 50 mm wide (the B21 Pro's whole 591-dot head) and 30, 40, 50, 70 or 100 mm long: the shortest that fits. Text that fits none goes on a front (name, weight, dates, batch code, storage, business) and a back (ingredients, may contain), each on its own shortest fit; too long for that is refused | Spec: 50 × 30 mm by default, a longer label or a front and back when it does not fit. The lengths are common Niimbot B21 rolls; the owner confirms which rolls are in the bakery (open question) |
| PNG at 300 dpi and the label's exact size in dots (`round(mm × 300 / 25.4)`: 591 × 354 for 50 × 30). Every pixel black or white | The printer prints dots, not greys; thresholding here gives the same print whatever the app does with grey |
| Rendering: Typst lays out (`typst`, `typst-layout`), `typst-svg` turns the page into SVG and `resvg` draws it. A missing glyph, any Typst warning, or text that runs past the right margin or into the logo (a word too long to break) is an error: nothing is cut off silently. Rendering runs in `spawn_blocking`, as the PDFs do | No new crate in `Cargo.lock`: both were already in it through `typst-pdf`. `typst-render` would be the direct way but adds `typst-render` and `pixglyph` (open question) |
| Text: Inter 6 pt (x-height about 1.2 mm), product name in Oswald 9 pt, the 1024-px logo at 12 mm top right (ADR 0023). Allergen ingredients bold | EU 1169/2011 asks at least 0.9 mm x-height on packs under 80 cm²; a 50 × 30 label is 15 cm² |
| CSV for the app's import, in the shape of the other exports (ADR 0017): semicolons, CRLF, UTF-8 with BOM, Dutch headers. Columns: Bestand, Soort, Product, Ingrediënten, Kan sporen bevatten van, Netto gewicht, Ten minste houdbaar tot, Ingevroren op, Bewaren, Partij, Bedrijf, Aantal. Allergen ingredients in capitals there | A text field cannot be bold; capitals are an accepted emphasis. `Bestand` names the PNGs (front and back when split), `Aantal` the copies. No CSV for a label whose PNG cannot be drawn |
| Best-before: product label = bake date + `product.shelf_life_days`; freezer label = freeze date + `product.freezer_days`. Calendar days, so summer and winter time do not move them | Spec data model; shelf life is new because the spec had no field for it |
| Freezer pieces: `batch_output.frozen_pieces` and `frozen_on`, entered after baking on the labels page. `frozen_on` is the bakery's date (injected clock, Europe/Amsterdam) of the first entry; corrections keep it; 0 clears. At most the good pieces (actual − waste), also when actuals are corrected later | Spec: "after baking, enter how many pieces of a batch go into the freezer". The freeze date is not entered, so it cannot be mistyped |
| Copies: product label = good pieces minus frozen pieces; freezer label = frozen pieces | One label per piece; a frozen piece gets only the freezer label, with its own best-before |
| Label settings per product on `/staff/products/{id}/etiket` (packaged, shelf life, storage instruction, freezer period), Production permission ("labels" in docs/permissions.md). The company address on `/staff/settings` (Settings permission). Neither writes an audit entry, like the product form | Neither could be set in the back office before; a separate page keeps the product form unchanged |
| Downloads: `…/batches/{batch}/etiketten` lists per product and kind the PNGs, the CSV and the copy count, or the reason it is refused. Each file is made anew after the check; a refused file answers 409 with the page and the reason. Production permission | Spec: downloads from the batch page, blocked with the reason |
| Individual downloads, no ZIP | No ZIP crate in the workspace (open question) |
| No QR code on the freezer label yet | The spec does not say what it encodes, there is no QR crate, and Typst packages are not vendored (open question) |
| `LabelPrint` is not stored | A download is not a print; the official app prints, so the system cannot know |

Open: the Niimbot app's import format and image limits are still to be checked with the
owner's phone (ADR 0024).
