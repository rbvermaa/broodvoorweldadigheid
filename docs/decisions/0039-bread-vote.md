# 0039 Bread vote: one vote per confirmed address

Status: proposed, 2026-11-25

Covers F9b: the baker lists candidate breads, optionally for a drop; visitors vote once per
confirmed email address with an optional wish; results for the baker and optionally on the
site; the vote closes at a set time.

| Choice | Why |
|---|---|
| Logic in `notify::polls`, pages in `apps/server` (`votes`, `backoffice::polls`). New `Permission::Polls` for owner and baker (added to the ADR 0010 table) | The confirmation email goes through `notify`, like drop-alert sign-ups (ADR 0021). The spec says the baker runs the vote |
| `vote.email` instead of the spec's `subscriber_id`. One row per poll and `lower(email)` (unique index) | A Subscriber row means "on the drop-alert list". Voting must not sign anyone up for marketing mail, and a confirmed subscriber row would. The unique index makes "once per address per poll" hold in the database too |
| A visitor who is not signed in always gets a confirmation link, also when the address is a confirmed drop-alert subscriber. A signed-in customer's vote counts at once, with the address of their login | Being on the drop-alert list proves someone owned the address once, not that this visitor does: anyone could type a known subscriber's address and vote in their name. A customer login is fresh proof (ADR 0029), like the consent checkbox of ADR 0031 |
| A vote waits unconfirmed with a 32-byte token whose SHA-256 is stored. The link works at most 7 days and never after the poll closes. A new vote from the same address while one waits replaces it and voids the old link; at most one confirmation per address and poll per 5 minutes. Per client: 5 votes, then one per 12 s (tower-governor) | The same token rules as ADR 0021. Nobody can flood an inbox through the form |
| Same answer page for a new, waiting or already-confirmed address; only a bad address, a too-long wish or an unknown candidate shows the form again | No enumeration of who voted |
| `GET /stemmen/bevestig/{token}` shows a button; `POST` with the CSRF token counts the vote, once. Both `private, no-store` | Mail scanners that open links do not cast votes (as ADR 0029) |
| Confirmed votes never change. The first confirmed choice counts | "Once per address" is simplest to explain and to check |
| The confirmation email names the choice; the page behind its link shows the choice and the wish before the button | Anyone can type someone else's address and replace a waiting vote, so the inbox owner must see what they confirm. The wish is free text from anyone and never goes into an email, so the form cannot be used to mail arbitrary text |
| A poll is open while `now < closes_at`, read from the injected clock on every vote and confirmation. No job closes it. The baker can change title, drop, closing time and the public switch later, also to close early. A closed poll never opens again; the candidates are fixed at creation | Nothing to schedule or miss. An erase blanks the address of votes in closed polls, so a reopened poll could count that address twice. Changing candidates after votes came in would change what people voted for |
| 2 to 12 candidates: active products (their name is copied into `poll_option.name`) and free-text lines. Title up to 200, candidate up to 100, wish up to 500 characters | The copy keeps old results readable after a product is renamed |
| Results count confirmed votes only. The site shows counts per candidate when the poll's switch is on; wishes and addresses are never on the site. The back office shows counts and wishes, without addresses | Wishes are free text from visitors and may hold personal details; addresses are not needed to read a vote |
| `/stemmen` lists open polls and closed polls with public results | Closed polls without public results have nothing to show |
| Privacy (F12): the export lists the votes of the address (poll, choice, wish, times). Erasing deletes unconfirmed votes and votes in open polls, and blanks address and wish of confirmed votes in closed polls, which keep counting. `jobs::cleanup` deletes unconfirmed votes whose link ran out. The confirmation email is queued with `enqueue_expiring`, so its body is blanked once sent and the 90-day outbox retention deletes it | Results of a closed vote stay the same after an erase; nothing identifies the voter any more. In an open poll the address could vote again, but never count twice |
| Vote retention (owner, 2026-11-25): `jobs::cleanup` blanks address and wish of confirmed votes 30 days (`notify::polls::KEEP_ADDRESS`) after their poll closed, the same way an erase does. Counts stay the same | Voters without a customer account cannot be erased from `/staff/customers`; after 30 days nothing identifies them anyway. Closed polls never reopen, so a blanked vote cannot let an address vote twice |

Open:

- No link to `/stemmen` in the site header or on the drop page yet; it needs a place in
  the header design.
- A voter without a customer account cannot be found in `/staff/customers`, so the owner
  cannot export or erase their votes from there. Their unconfirmed votes are deleted when
  the link runs out; confirmed ones keep the address until 30 days after the poll closed.
