# 0014 Emails: templates, inline styles and the outbox

Status: proposed, 2026-11-24

Covers F9 (order confirmation, pickup reminder, cancellation), the sick-day email of F2
and the drop-alert and subscription emails of F9a. No order or subscriber code is
needed: `notify` takes plain input structs, and the callers map their rows to them.

| Choice | Why |
|---|---|
| lettre 0.11 with `default-features = false` and `builder`, `smtp-transport`, `pool`, `tokio1`, `tokio1-rustls`, `ring`, `rustls-native-certs` | `rustls-tls`/`tokio1-rustls-tls` pull in `webpki-roots` (CDLA-Permissive-2.0, refused by `deny.toml`), and the default rustls provider is `aws-lc-rs`. With `ring` and native certs the tree has neither; the images already ship `cacert` and set `SSL_CERT_FILE` |
| `EmailSender` is an object-safe trait in `notify` returning a boxed future. `SmtpSender` implements it; `testkit::FakeEmailSender` (feature `email`) keeps what it is given and can fail on demand | No test talks to a relay. The feature keeps lettre out of every other crate's test build |
| An email is a `Content` (heading, greeting, blocks, footer) built from Fluent. The plain-text part is written by Rust, the HTML part by `templates/email/layout.html`; both come from the same `Content`, so they always say the same thing | One source of words, two renderings. Every email is `multipart/alternative`, text first |
| Email clients drop stylesheets, so the HTML needs `style` attributes, which the design-system check forbids in templates. Templates use classes from `design/email.css`; after rendering, `notify::style` swaps each `class="…"` for `style="…"`, with `var(--token)` replaced by its value from `design/tokens.toml` and `rem` turned into `px` (16px root) | Templates and CSS stay under the existing checks unchanged: no `style=` in templates, only token variables in CSS, every class defined. `email.css` only allows single-class rules. A server test checks that `notify` reads the same token values as the generated `tokens.css`. `email.css` is never linked from a page |
| The brand bar is the name in the heading font, black on white, with `#payitforward` | There is no logo file in the repository yet. An inline or hosted image is a later change, once an SVG or PNG of the mark is added. Update: the PNG is in `design/` now; emails wait for a public URL setting (ADR 0023) |
| Every email ends with trade name, address and KvK number (`Company`, all three required), and order confirmations list BTW per rate: net and BTW, rounded per line (ADR 0005) | Compliance table: KvK details in emails; rate and BTW amount on order confirmations |
| Drop alerts carry `List-Unsubscribe: <https://…>` and `List-Unsubscribe-Post: List-Unsubscribe=One-Click` (RFC 8058), plus a visible unsubscribe link | F9a "one-click unsubscribe". The URL must answer the POST without a login or CSRF token; that route comes with the subscriber slice. RFC 8058 also needs a DKIM signature over these headers, which the relay adds (SPF/DKIM/DMARC in "External integrations") |
| Links must be plain `https://` URLs without spaces, quotes or angle brackets; recipient addresses are checked with lettre when composing | Links go into HTML attributes and a header; a bad address fails at queue time, not at send time |
| Outbox table `email_outbox`. `enqueue(conn, clock, email, dedupe_key)` runs inside the caller's transaction; an optional `dedupe_key` (unique) makes queuing the same email twice a no-op | The email is queued if and only if the change it reports commits |
| `send_pending(conn, sender, clock, batch)` takes one due row at a time with `FOR UPDATE SKIP LOCKED` in its own transaction, sends it, records the result and commits | Several workers never send the same row, and a crash re-sends at most the one email in flight. Delivery is at least once |
| The Message-ID is `<uuid@from-domain>`, with the uuid stored on the row, so a retry uses the same id | Receivers and the relay can spot a duplicate |
| Temporary failures retry after 1 min, 5 min, 15 min, 1 h, 3 h, 6 h, 12 h; after 8 attempts the row is `failed`. Only a bad address or a 550, 551 or 553 reply with a recipient status (5.1.1, 5.1.2, 5.1.3, 5.1.6, 5.1.10) fails a row at once; every other error, 5xx included (550 5.7.1 relay or sender policy, 535 login failed, 554), counts as temporary and a refusing relay is logged at error level with only its codes. The send transaction sets `idle_in_transaction_session_timeout = 0` |  A wrong password or relay policy would otherwise fail the whole queue in one tick. A slow relay must not get the transaction killed after a successful send, which would send the email twice.  About 22 hours of retrying covers a relay outage overnight. Logs name only the outbox id, never the address or the relay's error text, which may hold the address; the error is kept on the row |
| `SmtpConfig` reads `SMTP_HOST`, `SMTP_PORT`, `SMTP_TLS` (`starttls` default, `tls`, `none`), `SMTP_USERNAME`, `SMTP_PASSWORD_FILE` and `MAIL_FROM`; `Debug` shows the password as `<redacted>` | Secrets come from files, like the database password. `none` exists only for Mailpit, so it refuses a login |
| Mailpit for local runs: `bvw-mailpit` built from nixpkgs `mailpit` with `dockerTools` like the other images, in compose profile `mail` (started by `make up`, not by the smoke test), SMTP on `mailpit:1025` inside the network, UI at `https://mail.localhost:8443/` through Caddy | No Docker Hub image. The UI shows every email, so it stays on the local stack only |

Open, for the slices that use this:

- Wiring: the server builds `SmtpSender` from `SmtpConfig` and the jobs tick calls
  `send_pending`; the compose stack then sets `SMTP_HOST=mailpit`, `SMTP_PORT=1025`,
  `SMTP_TLS=none` for the server.
- The emails say "Beantwoord deze mail", so `MAIL_FROM` must be a mailbox the bakery reads,
  or a `Reply-To` must be added.
- Failed rows need a place in the morning email to the owner. Retention: rows are deleted
  90 days after they were sent or failed (ADR 0031).
