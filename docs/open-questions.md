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
- [ ] **Cancelled drops on the public calendar.** F1a lists planned, menu announced, open,
  sold out and closed. A cancelled (sick-day) drop is shown as "cancelled" for now
  (ADR 0008). OK?
- [ ] **Bake start time.** No default rule in the spec; staff enter `bake_starts_at` per
  bake day. Should it default from the pickup window (e.g. 04:00 on the bake date)?
- [ ] **Default lead time vs. default deadline.** 36 h before a Saturday bake start
  earlier than 08:00 is before the default Thursday 20:00 deadline, so every default
  drop gets the lead-time warning. Either the lead time, the deadline or the bake start
  default is off.
