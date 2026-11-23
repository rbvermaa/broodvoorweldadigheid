# 0002 Phase 0 build choices

Status: accepted, 2026-11-23

| Choice | Why |
|---|---|
| Rust toolchain from `rust-toolchain.toml`, loaded with the `rust-overlay` flake input | One file pins the toolchain for the dev shell, crane and CI, including `llvm-tools` for coverage |
| Images with `dockerTools.streamLayeredImage` | Allowed by the spec, no extra flake input |
| Container first-start work (initdb, Kanidm cert) in Rust `cli entrypoint` subcommands | "Rust for all our code"; images contain no shell. Exception: the postgres image has `dash` as `/bin/sh`, because `initdb` starts `postgres` through `popen()` |
| Images for both `aarch64-linux` and `x86_64-linux` | The server's CPU is not known yet; Docker Desktop on the dev Mac needs aarch64 |
| On macOS, `make images` builds `x86_64-linux` images on a remote Linux builder through the local Nix daemon (`LINUX_BUILDER`, default `dev1`; `LINUX_STORE` overrides with a remote store) and Docker Desktop runs them under emulation; `aarch64-linux` images are built and tested only in CI | The dev Mac has no working aarch64-linux builder. The image stream script only runs on Linux, so each image package is a tar written on the builder |
| Repo tooling (layer check, coverage thresholds, compose smoke test) in an `xtask/` crate | The usual Cargo place for project tooling; not in the spec's workspace layout and never shipped |
| Root `Makefile` wraps every top-level command | One easy entry point; build logic stays in `flake.nix` |
| Clippy's default lint set at `deny`, without `clippy::pedantic` | Pedantic lints such as `must_use_candidate` would need `#[allow]`s, which CLAUDE.md forbids. Individual pedantic lints can be added to `[workspace.lints.clippy]` later |
| The HTTP-crate rule also applies to `testkit` and to dev-dependencies | The spec's fakes (Mollie, clock, printer) are in-process; revisit only if a fake needs a real HTTP server |
| Inline `#[cfg(test)]` modules count toward line coverage | Simple and the same for every crate; revisit if large inline test modules start to inflate the strict-crate numbers |
| `cargo xtask unsafe-forbid` reads crate roots and manifests as text, not with a parser | It errs on the strict side: `/*! */` before the attribute, `forbid(unsafe_code, ...)` lists, multi-line attributes and `workspace=true` without spaces are rejected. Write those the plain way instead |
