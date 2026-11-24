# 0010 Staff roles and sign-in with Kanidm (F13)

Status: proposed, 2026-11-24

## Roles

Owner can do everything. The other roles get what their work needs.

| Permission | What it covers | owner | baker | counter |
|---|---|---|---|---|
| ViewOrders | order list and order details | ✓ | ✓ | ✓ |
| PickupList | pickup list, marking orders picked up | ✓ | ✓ | ✓ |
| Production | production list, recipes, batches, labels | ✓ | ✓ | |
| EditAllergens | allergens, ingredient text, nutrition approval | ✓ | ✓ | |
| ManageBakeDays | bake days, cutoffs, pickup slots, capacity | ✓ | ✓ | |
| CloseBakeDay | sick-day cancel (F2), including that day's refunds | ✓ | ✓ | |
| Refund | refunding a single order or line | ✓ | | |
| ViewReports | sales, donation and contribution reports (F11c, F11e) | ✓ | | |
| ManageCharities | charities and payouts | ✓ | | |
| ManageStaff | adding staff and changing roles | ✓ | | |

The table lives in `auth::Role::can` and its test. Routes call `Staff::require(permission)`,
which answers with the Dutch 403 page.

## Sessions and sign-in

| Choice | Why |
|---|---|
| Each sign-in creates a `staff_login` row with a random id (two `gen_random_uuid()`, 244 random bits). The session holds only that id | The database, not the session, decides who is signed in. Logout and deactivation delete the row. A request still in flight that saves its session again (`always_save`) then brings back an id that no longer works |
| The `Staff` extractor looks up the login and its staff row on every request; a missing or timed-out login, or an inactive staff row, ends the session and redirects to `/staff/login` (`HX-Redirect` for HTMX requests) | Deactivating someone or changing a role takes effect on the next request |
| A login ends after 12 hours without a request, and 7 days after sign-in however busy it is. Checked against the injected clock | Shorter than the 14-day shop session (ADR 0007); a shared till or bakery PC does not stay signed in for long. Every sign-in also deletes all expired rows, so `staff_login` lists the live logins plus at most the ones that expired since |
| Every sign-in method ends in `staff_session::sign_in(state, session, staff)`: end any login already on the session, clear the session, `cycle_id()`, create the login, store its id | Fixes ADR 0007's open item; a planted session id and its CSRF token stop working. A shared browser does not keep the previous person signed in. A new method only has to prove who the person is |
| `auth::staff::sign_in` decides who gets in from a verified identity (`sub`, email, `email_verified`): known and active is allowed, known and inactive or unknown is refused | No self-signup |
| `BVW_BOOTSTRAP_OWNER_SUB` or `BVW_BOOTSTRAP_OWNER_EMAIL` (verified email only) becomes owner, but only while there is no owner; an advisory lock serializes bootstrap sign-ins | A leftover setting cannot create a second owner. Prefer the sub: the email rule trusts the provider's email_verified |
| A trigger refuses to delete, demote or deactivate the last active owner. It takes the bootstrap advisory lock and locks the remaining owner rows `FOR UPDATE` | Otherwise bootstrap would open again, and nobody could manage staff. The advisory lock stops two transactions from each removing one of the last two owners; the row lock makes a REPEATABLE READ transaction fail with a serialization error instead of trusting a stale snapshot |
| Others are added with `make staff-add` / `cli staff add --sub UUID --name NAME --role ROLE`. The sub must be a UUID; values are trimmed and may not be empty. The make target passes them as environment variables | A staff screen comes later. Kanidm's `sub` is the person's UUID; a user name there is the usual mistake. Names like "Bas van 't Hof" survive the shell |
| Logout is a CSRF-protected `POST /staff/logout` that ends the local login only | |
| `product.allergens_approved_by` now references `staff_user(id)` | Promised in ADR 0006 |

## Dev login

To use and test the back office on a developer's own machine without Kanidm,
`DEV_STAFF_LOGIN=true` turns on `/staff/login` as a form and `POST /staff/dev-login`
(`staff_id`, CSRF-protected), which signs in as any active staff member.

| Choice | Why |
|---|---|
| A runtime setting, off by default, instead of a cargo feature | One binary and one test run cover both states. A feature would need a second build in CI and in `nix flake check` to test the "off" case, and the image would differ from what the tests ran |
| The config refuses `DEV_STAFF_LOGIN=true` unless `BIND_ADDR` is a loopback address and `TRUSTED_PROXY` is unset | The image binds `0.0.0.0` behind Caddy with a trusted proxy, so it cannot start with dev login on |
| The login form and `POST /staff/dev-login` also refuse any request whose TCP peer is not loopback, that carries `Forwarded`, `X-Forwarded-For` or `X-Real-IP`, or whose `Host` is not `localhost`, `127.0.0.1` or `[::1]` (any port) | A loopback bind alone is not enough: a reverse proxy on the same machine would make it public, and such a proxy adds one of these headers. A web page on a name that resolves to 127.0.0.1 (DNS rebinding) still sends its own `Host` |
| `deploy/compose.yaml` sets `DEV_STAFF_LOGIN: "false"` | Says so explicitly in the prod-like config |
| Without dev login, `/staff/login` answers 503 and `/staff/dev-login` is not routed (404) | |
| Tests do not use the dev login to get a session; `tests/support::signed_in` creates a login and writes a signed-in session to the store directly | Tests do not depend on how staff sign in |

## Login

| Choice | Why |
|---|---|
| Authorization code flow with PKCE (S256), state and nonce; the client authenticates with its secret (basic auth) | Kanidm requires PKCE; a confidential client adds the secret |
| ID tokens must be ES256 or RS256 | Kanidm signs with ES256 by default. HS256 with the client secret is refused |
| The token's expiry is checked against the injected clock | Same rule as all domain code; tests move the clock past expiry |
| openidconnect without its HTTP features; requests go through `auth::oidc::OidcHttp` | Its `rustls-tls` feature uses `webpki-roots` (CDLA-Permissive-2.0, not allowed). Tests use `testkit::FakeOidc`, so no network |
| The server's client is reqwest 0.12 with `rustls-tls-manual-roots` (ring), trusting `rustls-native-certs` plus `OIDC_CA_FILE` | No webpki-roots, no aws-lc (its licence expression includes OpenSSL, which deny.toml does not allow). Kanidm's self-signed CA goes in `OIDC_CA_FILE` |
| Discovery runs on the first login and again after 15 minutes; the HTTP client is rebuilt after a connection failure | Kanidm may start after the server, may rotate keys, and a fresh kanidm volume brings a new CA |
| The client secret is read from `OIDC_CLIENT_SECRET_FILE` at login time; missing or empty means 503 | `make kanidm-setup` writes it after the stack is up, without a server restart |
| Logout ends the local login only | Kanidm's own session stays; a passkey is needed again when it expires |
| `/staff/login` and `/staff/oidc/callback` share a rate limit: 10 at once, one more every 6 s per client | Spec: rate limits on login. The limit sits inside the session layer (ADR 0007 open item stays open) |

The callback ends in `staff_session::sign_in`, like the dev login. Without `OIDC_ISSUER`,
`/staff/login` falls back to the dev login, or 503. A provider that cannot be reached
also gives 503, with a "try again in a few minutes" text.

## Local stack

In compose, Kanidm has the network alias `idm.localhost`, so the server reaches the
issuer `https://idm.localhost:8443/...` directly on Kanidm's own port and certificate.
`make kanidm-setup` registers the OAuth2 client and copies the client secret and Kanidm's
CA into `deploy/secrets/oidc/`, which the server mounts read-only. See deploy/README.md.

## Open

- openidconnect 4.1 depends on `rsa 0.9`, which has RUSTSEC-2023-0071 (Marvin attack,
  no fix in 0.9). It affects RSA private-key operations; the server only verifies
  signatures with public keys. `make audit` fails until openidconnect moves to rsa 0.10
  or the advisory is accepted.
