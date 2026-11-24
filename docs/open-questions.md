# Open questions

Questions about the spec. The spec is only changed when the owner asks.

- [ ] **F19 is missing.** The requirements table goes from F18a to F20. Removed on
  purpose, or lost?
- [ ] **GitHub instead of Forgejo** (ADR 0001). Update the spec's stack table and the
  list of allowed external services?
- [ ] **Off-site backup target.** Which host or provider does the second rustic
  repository go to?
- [ ] **Label fonts.** Which fonts go into the server image for PDFs and labels?
- [ ] **Branch protection.** Not available on a free private GitHub repo (ADR 0001).
  Left unenforced for now; revisit with GitHub Pro or a move to Forgejo.
- [ ] **CDLA-Permissive-2.0.** `webpki-roots` (used by rustls for TLS) has this licence.
  It is open but not OSI-approved, so `deny.toml` rejects it. Allow it when TLS lands?
- [ ] **advisory-db freshness.** cargo-audit is only as fresh as the pinned advisory-db.
  The weekly dependency-update job (spec, "Nightly and weekly") should bump it.
- [ ] **Partial refund by amount on a mixed-rate order.** How is it split across BTW
  rates? (System refunds per line for now; ADR 0005.)
- [ ] **Gate 3 names the till.** The roadmap picture says "Gate 3: till in daily use,
  two weeks at the counter", but the till is out of scope (SumUp). What is Gate 3?
- [ ] Default sourdough lead time: 36 h assumed (company.default_lead_time_hours); confirm with the baker.
- [ ] **Mollie fees need a second secret.** Per-payment fees (F11e) come from Mollie's
  balance transactions, which need an organization access token with `balances.read`,
  not the website API key (ADR 0013). OK to add that token to the sops secrets?
