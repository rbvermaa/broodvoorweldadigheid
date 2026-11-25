# 0020 Apps sit in a layer above jobs and reports

Status: accepted, 2026-11-24 (owner decision)

The spec puts `reports`, `jobs` and everything under `apps/` in layer 6, and a crate may only
depend on lower layers. The spec also says the server is the one binary that wires the
crates together, and that it runs the scheduled work. With apps in layer 6 the server could
not start `jobs::run` (ADR 0015).

The layer check (`cargo xtask layers`) therefore places `server`, `pos` and `cli` in layer 7.
Apps may depend on every library crate, `jobs` and `reports` included, but not on each other.
The library crates keep layers 0 to 6 exactly as the spec lists them, so `jobs` and
`reports` still may not use each other, and testkit still stays below layer 6. The HTTP-crate
and `ai` feature rules are unchanged.

The spec text is unchanged; this ADR records how the check reads it.
