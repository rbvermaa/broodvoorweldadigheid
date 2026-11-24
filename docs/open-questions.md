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
  SMTP avoids it: lettre verifies with `rustls-native-certs` (ADR 0014).
- [ ] **advisory-db freshness.** cargo-audit is only as fresh as the pinned advisory-db.
  The weekly dependency-update job (spec, "Nightly and weekly") should bump it.
- [ ] **Partial refund by amount on a mixed-rate order.** How is it split across BTW
  rates? (System refunds per line for now; ADR 0005.)
- [ ] **Gate 3 names the till.** The roadmap picture says "Gate 3: till in daily use,
  two weeks at the counter", but the till is out of scope (SumUp). What is Gate 3?
- [ ] Default sourdough lead time: 36 h assumed (company.default_lead_time_hours); confirm with the baker.
- [ ] **Logo file.** Emails, PDFs and labels need the brand mark. Add an SVG (or the
  1024-pixel PNG) to `design/`; emails use a text wordmark until then (ADR 0014).
- [ ] **Email Reply-To.** Emails say "Beantwoord deze mail". Is `MAIL_FROM` an inbox the
  bakery reads, or should replies go to another address? (ADR 0014)
- [ ] **Email outbox retention.** How long do sent emails stay in `email_outbox`? A short
  period (for example 30 days) is enough for support questions (ADR 0014).
