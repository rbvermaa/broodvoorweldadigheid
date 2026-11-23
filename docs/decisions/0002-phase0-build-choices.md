# 0002 Phase 0 build choices

Status: accepted, 2026-11-23

| Choice | Why |
|---|---|
| Rust toolchain from `rust-toolchain.toml`, loaded with the `rust-overlay` flake input | One file pins the toolchain for the dev shell, crane and CI, including `llvm-tools` for coverage |
| Images with `dockerTools.streamLayeredImage` | Allowed by the spec, no extra flake input; streamed so the store holds no tarball |
| Container first-start work (initdb, Kanidm cert) in Rust `cli entrypoint` subcommands | "Rust for all our code"; images contain no shell |
| Images for both `aarch64-linux` and `x86_64-linux` | The server's CPU is not known yet; Docker Desktop on the dev Mac needs aarch64 |
| Repo tooling (layer check, coverage thresholds, compose smoke test) in an `xtask/` crate | The usual Cargo place for project tooling; not in the spec's workspace layout and never shipped |
| Root `Makefile` wraps every top-level command | One easy entry point; build logic stays in `flake.nix` |
| Clippy's default lint set at `deny`, without `clippy::pedantic` | Pedantic lints such as `must_use_candidate` would need `#[allow]`s, which CLAUDE.md forbids. Individual pedantic lints can be added to `[workspace.lints.clippy]` later |
