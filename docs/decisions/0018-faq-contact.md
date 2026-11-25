# 0018 FAQ and contact form

Status: proposed, 2026-11-25

Covers F4e (FAQ) and F4f (contact form). The back-office screens for the FAQ come with
the staff-role slice; the repository functions they need are already in `catalog::faq`.

| Choice | Why |
|---|---|
| FAQ entries live in `catalog` (`catalog::faq`, table `faq_entry`) | Site content, like the charities of ADR 0006: layer 2, needs only `domain` and `db`, and the assistant (`ai`, layer 5) and the server can both read it. A separate crate for one table is not worth it |
| `faq_entry.position` is unique and `DEFERRABLE INITIALLY IMMEDIATE`; `reorder` takes every id once and updates all positions in one statement, or none | The unique check runs at the end of the statement, so positions can swap. A list that misses, repeats or invents an id is refused |
| New entries are unpublished and go last; `create` locks `faq_entry` (SHARE ROW EXCLUSIVE) while it picks the next position | Staff publish when the text is ready. Without the lock two writers read the same `max(position)` |
| Answers use an own Markdown subset (`catalog::markdown`): paragraphs, `**bold**`, `*italic*`, `[text](https://…)` and `-`/`*` lists. Everything else, HTML included, stays text | No new dependency. The parser returns blocks and spans, not HTML; `templates/components/rich.html` renders them, so askama escapes every text and no `\|safe` is needed. Links must be plain `https://` URLs without quotes, spaces or angle brackets; any other link keeps only its text |
| The four seed questions are in the migration, not in Fluent | They are content staff edit, not UI text |
| **The seed texts are placeholders.** broodvoorweldadigheid.nl (still Payhip) refused the automated fetch (Cloudflare block), so the four questions are written from the spec: ordering and paying in the shop, pickup at De Proef, allergens, and where the money goes. They give no pickup time (open question in the spec) | Staff should replace them with the current site's four questions, rewritten for paying through the shop, before go-live |
| The contact form posts to `/contact` with the session's CSRF token. Only the POST is rate limited: 10 at once per client, then one per 5 minutes (`rate_limit::apply`) | Reading the form costs nothing; sending queues an email |
| Spam protection: a trap field `website`, hidden with the `field-trap` class (the `visually-hidden` rules), `aria-hidden`, `tabindex="-1"`, `autocomplete="off"`. When it is filled in, the visitor sees the normal "sent" page and nothing is queued | Bots get no signal to adapt to |
| Minimum fill time 3 seconds, measured from when `/contact` was shown. The time is kept in the session, not in a signed form field | The session is stored server-side (`http_session`), so the client cannot change it, and no signing key is needed. A too-fast submit gets the form back with its values and a Dutch message. The outbox `dedupe_key` is a SHA-256 of session id and the three fields. A double click, a reload of the result page or a retry after a lost response queues one email and shows the "sent" page; a different message, also from a second tab, is a new email |
| The clock reaches the handlers through `site::routes(state, clock)` as an extension, not through `AppState` | Tests set the time; `AppState` stays as it is |
| Name: required, max 100 characters, no control characters. Email: one plain address that both lettre parsers accept (`Address` and `Mailbox`; the latter refuses domain literals such as `x@[192.0.2.1]`, which would otherwise fail later at send time), max 254 bytes. Message: required, max 5000 characters, no control characters except newline, carriage return and tab | A newline in the name or address is how header injection works. `notify::Composer::contact_message` checks the same again |
| The email goes to `BAKERY_CONTACT_EMAIL` with `Reply-To` set to the sender. The sender gets no copy or auto-reply | A copy to any address someone types in would make the form a spam relay. Without `BAKERY_CONTACT_EMAIL` the form shows the 500 page and logs why |
| `email_outbox` gains `reply_to` and the kind `contact_message`; the migration drops and re-adds the kind check with one more value | Extends, removes nothing |
| The contact email carries only the company name in the footer, not address and KvK | It goes to the bakery, not to a customer; it then also works before staff fill in the company details |

Privacy (F12): contact messages are not stored anywhere except the `email_outbox` row
that sends them. That row holds the sender's name, address and message. The cleanup job
deletes it 90 days after it was sent or failed, like every outbox row (ADR 0031). Logs never hold the name, address or message.

Open:

- Delivery: nothing in the server runs `send_pending` with an `SmtpSender` yet (ADR 0014,
  open). Until that lands, contact messages wait in the outbox. Wire it up before the
  contact page is linked from the site.
- The trap field is called `website`. Browsers have no autofill token for it, but a
  password manager could still fill it; a real message would then be dropped silently.
- `deploy/compose.yaml` does not set `BAKERY_CONTACT_EMAIL` yet; the bakery's mailbox
  must be chosen first.
- The site header has no links to /faq and /contact yet; that comes with the site
  navigation.
- The back-office screens (list, edit, publish, reorder) need the staff roles.
