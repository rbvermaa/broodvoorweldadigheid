# 0009 Components, fonts, icons and the design-system check

Status: accepted, 2026-11-24

| Choice | Why |
|---|---|
| Inter is `web/InterVariable.woff2` from nixpkgs' own `inter.src` (the v4.1 release). Oswald is `Oswald[wght].ttf` fetched from google/fonts at the commit nixpkgs' `google-fonts` pins, turned into woff2 with `woff2_compress` (`nix/fonts.nix`) | Both pinned by hash, without pulling the whole google/fonts tree. Variable fonts: one file each covers every weight |
| Both fonts are SIL OFL 1.1. They are embedded in the server binary (`include_bytes!(FONTS_DIR/…)`), and the OFL texts sit next to them in the Nix output | The OFL allows bundling and serving. Fonts are not crates, so cargo-deny does not see them; this ADR records the licence |
| Fonts are served at `/static/fonts/NAME.woff2?v=HASH`. `design/fonts.css` names them as `url("fonts/NAME")`; the server swaps in the hashed URL at start-up | A new font changes the stylesheet's URL too, so both can be cached for good |
| Lucide comes from the `lucide-static` 1.66.0 npm tarball (`nix/lucide.nix`, ISC). build.rs copies only the icons listed in `design/icons.toml` and refuses anything but plain shapes | No unused icons in the binary, no third-party markup that could carry scripts or styles |
| Icons are inline `<svg>`. Without a label they are `aria-hidden`; standalone icons get `role="img"` and a Fluent label | No extra request, `currentColor` follows the text, and the CSP needs no change |
| The 14 allergen icons are Lucide pictograms, one per domain allergen code, all different (`[allergens]` in `design/icons.toml`). The badge always shows the Dutch name next to the icon | Drawing our own set was out of scope. Peanuts (bean), soy (sprout), mustard (flame), sesame (circle-dot) and celery (leafy-green) have no exact Lucide match; a designer can replace the mapping or add own SVGs later without touching templates |
| Components are askama macros in `templates/components/`. They take display text (already translated, or user data) and reach Fluent through the caller's `layout` | One way to build each element; askama escapes all text |
| `/gallery` exists only with `GALLERY=true` (compose sets `false`). Its confirmation dialog posts to `/gallery`, which returns the "Opgeslagen" confirmation | The gallery is for staging and visual tests until staff login exists. The POST shows the dialog working with HTMX and CSRF |
| Gallery example texts live in `locales/nl/gallery.ftl` | The rule "no Dutch text outside Fluent" holds for the gallery too |
| The dialog is a native `<dialog>`; app.js opens it (`data-dialog-open`), closes it (`data-dialog-close`) and closes it after an htmx request | No inline handlers (CSP). Escape closes it natively |
| `tokens.toml` gains `[border]` (1px, 2px) and `[size]` (icon, touch target, tile and reading width) | Components need these, and every size must be a token |
| The design-system check (`apps/server/tests/design_rules.rs`) parses CSS into rules: values may only use `var(--…)`, `0`, `1`, `50%`, `100%`; no hex, colour functions or named colours; no custom properties; `@media` may only use breakpoint token values; inside `@font-face` only `src` and `font-weight` may hold literals | The spec asks that no code set a colour or size outside the tokens |
| Template classes must be defined in `design/*.css`; askama `{% %}` tags inside `class="…"` are dropped so every branch's names count, and `{{ }}` in a class fails | Dynamic class names could not be checked |
| Fluent ids passed as `t("…")`/`t_count("…")` in templates and `apps/server/src` must exist. Every Fluent id must appear as a string literal in a template or in a crate's `src`, except ids starting with `allergen-` (built from the code; checked against `icons.toml`) and the reserved `order-thanks`, `order-deadline` and `pickup-moment` (checkout slice) | Catches missing and dead texts with a small explicit exception list |
| insta snapshots the gallery HTML with asset hashes replaced by `HASH` | Markup changes show up in review; CSS edits do not churn the snapshot |
