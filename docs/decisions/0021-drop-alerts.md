# 0021 Drop alerts: subscribers, tokens and sending once

Status: proposed, 2026-11-24

Covers F9a: sign-up with double opt-in, one-click unsubscribe, and an email when a drop's
menu is published or ordering opens.

| Choice | Why |
|---|---|
| No `source` column and no import. Subscribers only come from the form on `/meldingen` | Owner, 2026-11-24: there are no Payhip subscribers. A column with one value says nothing; adding one later is an add-only migration |
| `subscriber.email` keeps the address as typed (trimmed); a unique index on `lower(email)` | One row per address however it is capitalised, and the email goes to what the person typed |
| Logic lives in `notify::subscribers` | The server (sign-up, links) and `jobs` (alerts) both need it, and both sit above `notify` |
| Tokens are 32 random bytes, base64url; only their SHA-256 is stored | A database copy or backup cannot be used to confirm or unsubscribe anyone |
| One open confirmation link per subscriber, valid 7 days; a new sign-up replaces it. At most one confirmation email per address per 5 minutes | The form cannot be used to flood someone's inbox; the rate limit (5, then one per 12 s per client) covers the rest |
| Every drop alert gets its own unsubscribe token (`subscriber_unsubscribe_token`), so links in old emails keep working | A per-subscriber token stored hashed could not be put into the next email. An HMAC would need a new secret |
| Unsubscribing also voids an open confirmation link and deletes drop alerts to that address still pending in the outbox. The alert tick reads subscribers `FOR SHARE`, so an unsubscribe during queuing waits and then withdraws them too. Signing up again after unsubscribing sends a new confirmation; the address stays off the list until it is confirmed | An old confirmation link cannot undo an unsubscribe |
| The sign-up answer is the same page for a new, waiting or confirmed address; an already-confirmed address gets no email | No account enumeration. Only a malformed address gets the form back with an error |
| `GET /meldingen/bevestig/{token}` confirms. `GET /meldingen/afmelden/{token}` shows a button; `POST` to the same path unsubscribes and is safe to repeat | Mail scanners that open links cannot unsubscribe anyone. The POST route sits outside the session and CSRF layers: RFC 8058 clients send neither, and the token is the authority. It does not read the body |
| `drop_alert_sent (bake_day_id, kind)` is the primary key. The tick inserts it with `ON CONFLICT DO NOTHING` in the same transaction that queues the emails, after locking the day `FOR SHARE SKIP LOCKED` | Each alert goes out once, also with two servers: the second insert waits for the first transaction and then finds the row. A day a staff edit holds is skipped until the next minute. Outbox rows also carry the dedupe key `drop-alert:<day>:<kind>:<subscriber>` |
| "Menu published": an online draft whose `menu_publish_at` has passed. "Ordering open": an open day. Both only before the deadline. The alert step runs after the open/close step | Production-only and cancelled days never alert. A day whose menu and opening time are the same minute gets only the ordering alert. Days whose deadline passed before the feature shipped never alert |
| A drop that cannot be composed (company KvK or address missing, no pickup slot) is logged and retried every minute; nothing is recorded as sent | No half-sent alert, and it goes out once the data is fixed |
| An alert names the first ophaalmoment of the drop and links to `/drops/<id>` | `notify::DropAlert` takes one pickup. The webshop routes use `/drops/<id>` |
| The tick reads the clock once for the bake-day and alert steps, then hands due emails to an injected `Arc<dyn EmailSender>` one at a time: at most 500 and no new one after 40 s. `jobs::run` takes a `jobs::Mail` with the sender, translator and links | Wiring of ADR 0014. One instant means a day opening in the same minute as its menu gets only the ordering alert. The budget keeps a slow relay from holding a tick and its connection past the next one |

Open:

- Done since the merge with the order emails (ADR 0020, ADR 0022): the server starts
  `jobs::run`, builds `SmtpSender` from `SmtpConfig`
  and passes it in `jobs::Mail`.
- Unconfirmed subscribers whose link expired, and old unsubscribe tokens, stay in the
  database. A cleanup job should delete them (GDPR), with the other cleanup work.
- `PUBLIC_URL` (default `https://broodvoorweldadigheid.nl`) builds the links. The server refuses
  to start with an `http://` value, except for `localhost` and `127.0.0.1`. Email links must be
  https (ADR 0014), so such a loopback run cannot send sign-up or alert emails. The compose
  stack sets `https://localhost:$BVW_HTTPS_PORT` (override with `BVW_PUBLIC_URL`).
