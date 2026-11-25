# 0023 Style "Stempel": ink on paper, taken from the logo

Status: accepted, 2026-11-24

The owner chose direction **C · Stempel** from three mockups on 2026-11-24
(https://claude.ai/artifact/NCp4nayMZRoTPFavyurW6i, phone artboard). Spec: "Design
system and UX", brand. Extends ADR 0009 (components) and ADR 0014 (emails).

| Choice | Why |
|---|---|
| Colours are `ink` #111111 on `paper` #ffffff. There is **no accent colour**: `accent`, `on-accent` and the tinted status surfaces are gone. Greys stay for muted text, disabled states and photo placeholders. The status colours (success, warning, error, info) stay for icons, field errors and the danger button | Answers the spec's "one accent colour still to choose": the logo is black and white, and the owner picked the style without one. Status colours carry meaning, not brand |
| `black`/`white` are renamed `ink`/`paper` | One name per colour, the names the mockup uses |
| Headings and `.site-name`: Oswald 700, uppercase, `letter-spacing-heading` 0.03em. Buttons, prices, table heads and the stamp pill: Oswald 500, uppercase, `letter-spacing-label` 0.05em | Close to the logo's condensed capitals. Body text stays Inter for reading |
| `tokens.toml` gains `[letter-spacing]`, `radius.xl` (1.25rem), `border.stamp` (2.5px) and `border.rule` (3px), `size.logo-hero` (210px) and `size.logo-header` (44px), and `shadow.ring` | Every new size is a token, as before |
| The double ring is `shadow.ring` = `0 0 0 2px ink, 0 0 0 5px paper, 0 0 0 7px ink`, used by `.stamp` and `.button-stamp` (the future cart button). The token generator now refuses a `#rrggbb` in a shadow that is not a `[color]` value | A box-shadow cannot use `var()` in tokens.toml; the check keeps the copy in step with `ink` and `paper`. The ring adds 7px outside the element, so ringed elements get a margin and a larger focus offset |
| Buttons and inputs are pills (`radius.full`, 2px ink border); textareas use `radius.xl`. Cards, alerts, the dialog and the checked pickup option have a 2.5px ink border with `radius.xl` and no soft shadow. Alerts are white with a coloured icon. The section divider is `.rule`: 3px and 1px ink lines | The stamp look of the mockup |
| Focus is a 3px ink outline with a 4px offset; 12px on ringed elements and pickup options, so it clears the ring | Ink on paper is 18.9:1; the header and footer are white now, so one focus style works on every element, including black buttons (the outline sits outside them on paper) |
| The logo is `design/logo.png`, 420px (sharp at 210px on 2x screens). `favicon.png` (64px) and `apple-touch-icon.png` (180px) are the inner disc only: the fist and bread, without the lettering. All three are made by `make logo` from `docs/media/image1.png` with ImageMagick and oxipng from the flake's nixpkgs, and committed | The spec asks for "the fist-and-bread mark alone as the favicon". Committed outputs keep ImageMagick out of every build; they are small and change only when the logo does. A 512px icon waits for a web app manifest, which nothing needs yet |
| The `logo` macro (`templates/components/brand.html`) has two sizes. `hero` reads out the Fluent `logo-alt`; `header` is decorative (`alt=""`) because the site name is next to it. On home (and later the drop pages) the header holds only the hero logo; other pages have the small logo and name in the header. The checked pickup option gets the double ring | Mockup: the hero logo opens the home and drop pages; a second logo in the header would repeat it |
| Emails keep the text wordmark, now centred over a 3px double line, with pill buttons and uppercase headings. White page instead of grey | The server has no public base URL setting, and an email image must be an absolute URL (or an attachment). Add the logo once a `PUBLIC_URL` setting exists |

Open:

- Product tiles have no photo yet; the mockup's round 92px photo with an ink ring needs
  a photo field on the product first.
- Labels and PDFs still want the logo as SVG (open question "Logo file").
- `/favicon.ico` is not served; browsers that ignore `<link rel="icon">` get a 404.

## Amendment, 2026-11-25: desktop layout

The owner asked for an app that works well on a desktop too, not only on a phone. Phones
keep the layout above; from the `lg` breakpoint (64rem) the page widens.

| Choice | Why |
|---|---|
| From `lg`, header, main and footer are `size.page` (80rem) wide. Paragraphs in main keep `size.content` (48rem) | Tables, grids and forms use the width; running text stays readable |
| From `lg`, the staff menu is a sidebar (`size.sidebar`, 14rem). The entry of the current section is shown inverted and has `aria-current`: `page` on its own page, `true` on a page below it. On a phone the menu stays the wrapping list, with the current entry as an ink pill | The back office is used at a desk; a sidebar keeps 14 entries in view without pushing the page down. Neither the sidebar nor the cart is sticky: a sticky box taller than the window hides its lower part, such as the checkout link. `aria-current` tells screen readers where they are on every width |
| `templates/components/layout.html`: `form_grid` (short fields side by side from `md`, at most `size.tile-max` wide; textareas, checkboxes, fieldsets and text take a row), `choice_grid` (checkbox columns), `card_grid` (bordered cards from `md`) and `split` (a side column from `lg`, used for the cart). Each adds one div and nothing on a phone | Opt-in per page, so pages and shared macros keep their API and a page that does not call them is unchanged. Flex instead of grid: the token checks allow no `fr` values |
| Table cells break only between words (`overflow-wrap: break-word`, no hyphens), and numeric headers may wrap. A table wider than its column scrolls inside `.table-wrap` | With `overflow-wrap: anywhere` a table squeezed text columns to one letter per line, on every width |
| New size tokens: `tile-max`, `page`, `sidebar`, `aside` | Every size is a token |
| The bake day "Aanbod" is one table: product, maximum, ordered, left and a small remove button. Each maximum is its own form in its cell; htmx saves it on change or Enter and swaps only that row, plus the save message. The add row is the last row: its cells point at one form through the `form` attribute. Same table on a phone, where it scrolls. New macros: `f::inline_number` and `f::inline_select` (label for screen readers only), `b::button` with `small` and `form`, `l::cluster` (buttons on one row) and `l::link_row` (a named row of pill links) | One row per product instead of a fieldset per product made the list long. Swapping only the row keeps focus in the next field after a blur. A form cannot span table cells, so the add row uses `form` |
