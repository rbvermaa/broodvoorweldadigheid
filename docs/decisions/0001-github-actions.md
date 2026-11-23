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
