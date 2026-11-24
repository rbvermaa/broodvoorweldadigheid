# 0001 GitHub and GitHub Actions instead of Forgejo

Status: accepted, 2026-11-23

The spec ("Technology stack") names Forgejo with Forgejo Actions for code hosting and CI.
We use GitHub for source control and GitHub Actions for CI instead. There is no Forgejo
instance yet, and running one is extra work in Phase 0 for no gain to the bakery.

CI stays a thin wrapper: the workflow installs Nix and runs `make check`, which runs
`nix flake check`. Moving to Forgejo later means rewriting one workflow file.

This conflicts with the spec's rule that external services are limited to Mollie, the
SMTP relay and the off-site backup. GitHub holds code only, never customer data or
secrets. Tracked in `docs/open-questions.md` until the spec is updated.

Update 2026-11-24: the repository moved to a new public repo (rbvermaa/broodvoorweldadigheid,
history rewritten to the owner's personal email; the old private repo is kept as
-private). Public repos get free Actions minutes and branch protection, so `main` now
requires the CI checks (see the CI fan-out update below), applies to admins, and refuses
force-pushes and deletion. "No merge without a green pipeline" (spec, "Rules that keep
it safe") is enforced by GitHub. PRs still also need a Claude and a Codex review.

Update 2026-11-24 (later): `main` uses a merge queue. A PR is queued with auto-merge once its
reviews are clean; the queue re-runs the checks on the PR merged with the latest `main` and
merges only if they pass, so PRs no longer need main merged in by hand before merging.

Update 2026-11-24 (Cachix): CI pushes and pulls build output through the public Cachix
cache `rbvermaa` (owner's choice), and flake.nix lists it as an extra substituter, so dev1,
dev2 and the Mac can reuse CI's builds. Like GitHub, it holds only build output of this
public repository, never customer data or secrets. The spec names a self-hosted attic cache;
Cachix replaces it for now. The push token is the `CACHIX_AUTH_TOKEN` repository secret.

Update 2026-11-24 (CI fan-out): CI runs on x86_64 only (ubuntu-24.04); the arm runner is gone
(ADR 0002). The workflow no longer runs one `make check`. A `deps` job builds the shared
dependency artifacts and toolchain (`make ci-deps`) and pushes them to Cachix. Then one job
per check group runs the `make` target for its flake checks, and the smoke job runs beside
them. Every flake check still runs in CI, and `make check` still runs them all locally.
A final `ci-ok` job fails unless every other job succeeded; it is the only check `main` and
the merge queue require. The GitHub Actions Nix store cache (cache-nix-action) is dropped:
Cachix already serves everything we build, and parallel jobs would each restore and save a
4 GB store snapshot.
