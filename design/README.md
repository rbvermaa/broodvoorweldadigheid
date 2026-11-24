Design tokens (tokens.toml), fonts (fonts.css), base styles (app.css), component styles
(components.css), the icon list (icons.toml) and the one small script (app.js).
Stylesheets use only `var(--…)` tokens; `apps/server/tests/design_rules.rs` checks this.
See ADR 0009.
