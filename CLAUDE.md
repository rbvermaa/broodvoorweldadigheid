# Brood voor Weldadigheid — bakery platform

Webshop, drop calendar, charity donations, production planning, recipes and labels
for a sourdough microbakery in Frederiksoord. Served at https://broodvoorweldadigheid.nl/.

**Source of truth: `docs/spec.md`.** Read the relevant section before every task.
Requirements have IDs (F1, F4c, F14a …); refer to them in plans, tests and commits.
If this file and the spec disagree, stop and ask.

## Non-negotiables

- **Rust for all our code.** No Python anywhere: not in code, scripts, CI or tooling.
  The only JavaScript is HTMX and the one small file the spec allows.
- **Open source only.** Every dependency must pass `cargo deny`. External services are
  limited to Mollie, the SMTP relay and the off-site backup.
- **Everything builds with Nix.** Use `nix develop` for tools; never install tools globally.
  Images are built by `flake.nix`, never by a Dockerfile.
- **Makefile is the entry point.** Every top-level build, test, image and compose command
  has a `make` target that wraps the Nix command. Add a target when you add a command;
  CI calls the same targets.
- **Money** is integer cents plus a `VatRate` enum. Never floats.
- **Time** uses jiff with Europe/Amsterdam. Domain code never reads the clock directly;
  the clock is injected so tests can control it (including summer/winter time changes).
- **Dutch UI.** All user-facing text lives in `locales/nl/*.ftl` (Fluent). Never hard-code
  Dutch strings. Use the glossary: bakdag/drop, bestel-deadline, ophaalmoment, bestelling,
  batch. Tone: "je", friendly and short.
- **UI** uses only the design tokens and components in `design/` and `templates/`.
  No inline styles, no ad-hoc colours or classes.
- **Allergens, nutrition and labels** are never published or printed without the approval
  flow in the spec. When in doubt, block and ask.
- **Charities get the full amount paid** minus refunds; see the donation rules in the spec.
- **Crate layers** follow "Cargo workspace layout" in the spec. Lower layers never depend
  on higher ones. Only `apps/server` knows HTTP.
- `#![forbid(unsafe_code)]` in every crate we write.
- **No real customer data** in tests, fixtures, logs or prompts.

## How to work

1. For anything beyond a small fix, start in plan mode: list the requirement IDs covered,
   the files to change and the tests to add. Wait for approval before writing code.
2. Write the failing test first, then the code.
3. Work in thin vertical slices: one requirement, or part of one, per change.
4. A task is done only when `nix flake check` passes.
5. **Never weaken a gate to get green.** Do not delete, skip or `#[ignore]` tests, lower
   coverage or mutation thresholds, add Clippy allows, or loosen snapshot tolerances.
   If a gate looks wrong, stop and explain why.
6. Accept snapshot changes (insta, visual) only when the change is intended, and say so
   in the commit message.
7. Database migrations only add or extend; removals happen in a later release.
8. Ask before adding a new dependency. Prefer crates already in the workspace.
9. Small commits on a branch, messages referencing requirement IDs. Never push to main.
10. Record real design choices as short ADRs in `docs/decisions/NNNN-title.md`.
11. Don't edit `docs/spec.md` unless asked. Put spec questions in `docs/open-questions.md`.
12. **Every change is reviewed by both Claude and Codex** before it counts as done.
    Claude: `/code-review` or a review subagent. Codex: `codex review --base main`
    (or `--uncommitted`). Fix what they find, or explain why a finding is wrong.
13. **Use parallel subagents where possible:** up to 3 at a time, with one of them
    doing reviews. Give each a narrow, independent slice so they don't edit the same files.
14. **Keep iterating.** Once a plan is approved, loop test → code → review → fix
    until `nix flake check` passes and both reviews are clean. Don't stop to ask
    unless you're blocked or a rule above says to ask.
15. **Deploy only at the end**, after everything above is green. Test locally first.

## Local testing

Docker Desktop on this Mac runs the `x86_64-linux` images under emulation; `aarch64-linux`
images are built and tested in CI only (ADR 0002). `make images` builds the images on a
Linux builder (`LINUX_BUILDER` / `LINUX_STORE` in the Makefile) and loads them.
`make up` / `make down` run the stack from `deploy/compose.yaml`; `make smoke` runs a
throwaway stack next to it and checks health, Kanidm, backup and restore.

## Commands

Run `make help` for the full list. Each target wraps a Nix command.

- `make check` — everything CI runs (`nix flake check`)
- `make test` — fast test loop (`cargo nextest run` in the dev shell)
- `make fmt` / `make fmt-check` / `make lint` — format, check formatting, Clippy
- `nix develop` — dev shell with the pinned toolchain
- `cargo insta review` — review snapshot changes

## Current phase

**Phase 1 — MVP: selling online.** F1–F13 (drop calendar, charities and donation
reports, FAQ, contact, drop alerts), the design system and component gallery, and the
move from Payhip. Done when Gate 2 passes: the first bake day is sold online.

Phase 0 is done: Gate 1 passed on 2026-11-23 (stack from Nix-built images, CI on every
push, backup restore test).
