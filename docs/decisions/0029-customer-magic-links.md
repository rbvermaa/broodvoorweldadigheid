# 0029 Customer magic links and "Mijn bestellingen"

Status: proposed, 2026-11-24

Customers log in with an email link (spec, Security). This is the base for F4d (choose a
charity later), F11a (business customers) and F12 (export and delete).

| Choice | Why |
|---|---|
| Logic in `auth::customer`; the server queues the email through `notify` in the same transaction | The spec puts customer magic links in `auth`. `auth` sits below `notify`, so it returns the token and the server composes and queues the mail. A link that cannot be mailed is rolled back |
| A link is 32 random bytes, base64url; only its SHA-256 is stored, in `customer_login_link`. It works for 30 minutes (`LINK_VALID`) and once: using it sets `used_at` in one `UPDATE ... WHERE used_at IS NULL` | The token table alone cannot be used to sign in. Two clicks at once cannot both win |
| The email goes through `notify::enqueue_expiring`: `email_outbox.expires_at` is the link's expiry. An expired email is marked failed instead of sent, and its bodies are blanked once it is sent or failed | The outbox holds the plaintext link only while it is waiting, at most 30 minutes. A retry after the link expired would mail a dead link |
| Only existing customers (from checkout) get a link. The email is trimmed and lower-cased like checkout does | There is no sign-up. Checkout creates the customer row |
| Same page for every well-formed address: a customer, an unknown address, a customer over the limit, or a mail that could not be composed (logged) | No account enumeration. Only a malformed address gets the form back |
| Per email: at most 3 links per 15 minutes (`LINKS_PER_WINDOW`, `LINK_WINDOW`), counted in the database with the customer row locked. Over the limit nothing is sent, silently. Per client: the two POSTs share a tower-governor limit of 5, then one per 12 s, like checkout: outside the session and CSRF layers (ADR 0007) | Nobody can flood an inbox, and the per-email limit holds across servers and IPs |
| `GET /inloggen/{token}` shows a button; `POST` to the same path, with the CSRF token, signs in | Mail scanners that open links do not use them up. The CSRF token stops a third-party page from signing someone in. It cannot stop someone sending their own link to a victim |
| A customer login is a row in `customer_login` (like `staff_login`); the session holds only its id under `customer_login`, apart from `staff_login`. Idle timeout 7 days, absolute 30 days | Deleting the row signs that browser out, also if a request in flight saves the session again. Customer and staff logins do not mix |
| Signing in and out drop the customer key and the CSRF token and cycle the session id; the cart and a staff login stay | A planted session id stops working. Signing out of the shop does not empty a cart or sign staff out |
| `/mijn-bestellingen` lists every order of the customer, newest first, linking to the existing status page `/bestelling/{token}`. Without a login it redirects to `/inloggen` | Minimal landing page; F4d, F11a and F12 hang off it. It and the link page are sent with `Cache-Control: private, no-store` |

Follow-ups:

- No link to `/inloggen` in the site header yet. It needs a place in the header design.
- The answer for an unknown address is a little faster than for a customer (no insert, no
  mail composed). Queuing the work in the background would hide that.
- Expired links are deleted when a new link is requested, expired logins when a new
  login starts. A cleanup job should also do it (GDPR), with the other cleanup work.
- A staff sign-in clears the whole session and a staff timeout flushes it. Both leave the
  customer's `customer_login` row until it times out. Unreachable, but stored.
- Redeeming a link and starting the login are two steps. If the session store fails in
  between, the link is used up and the customer asks for a new one.
- Once F4d and F11a add changes under a login, the link page could show which address
  signs in, against someone sending their own link.
- Anyone can type any address at checkout, so the orders under an address may include ones
  someone else placed with it. Only the inbox owner can see them.
