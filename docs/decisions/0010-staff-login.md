# 0010 Staff roles and sign-in with Kanidm (F13)

Status: accepted, 2026-11-25

## Roles

Owner can do everything. The other roles get what their work needs.

The table of permissions per role is generated from code: docs/permissions.md (ADR 0044).

Each permission is one line in `crates/auth/src/role.rs`. Routes call `Staff::require(permission)`,
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
| Authorization code flow with PKCE (S256), state and nonce, each 32 random bytes from ring's `SystemRandom`; scope `openid email`; the client authenticates with its secret (`client_secret_basic`) | Kanidm requires PKCE; a confidential client adds the secret |
| Our own client in `auth::oidc` instead of openidconnect | Owner decision 2026-11-25: openidconnect 4.1 pulls in `rsa 0.9` (RUSTSEC-2023-0071). ring is already in Cargo.lock through lettre and rustls, so no new crate |
| Discovery reads `{issuer}/.well-known/openid-configuration`; its `issuer` must equal `OIDC_ISSUER` byte for byte, and every endpoint must be https | A provider cannot speak for another issuer; no plain-text token exchange |
| ID tokens: ES256 only, verified with ring | Kanidm signs with ES256 by default. `none`, HS256 (key confusion with the client secret), RS256 and anything else are refused before the key lookup. Only EC P-256 keys from the JWKS with `use` sig (or none) count |
| The header needs a `kid` from the key set; no `crit` | An unknown `kid` refuses the login and drops the cached keys, so the next login reads them again after Kanidm rotates its key |
| Claims: `iss` equals the issuer; `aud` contains the client id, and `azp` must be the client id when present or when `aud` has more than one entry; `exp` and `iat` checked against the injected clock with 60 s leeway; `nonce` equals the one stored with the login; `sub` must be a UUID (stored lowercase) | OIDC Core 3.1.3.7. Timestamps are UTC seconds, so summer and winter time do not matter; a test crosses the October change |
| Only `sub`, `email`, `email_verified` and a name claim go on to `auth::staff::sign_in` | Who someone is comes from Kanidm; what they may do comes from their `staff_user` row |
| A login without a name claim keeps the stored name | Scope `openid email` need not include `name` |
| The token's expiry is checked against the injected clock | Same rule as all domain code; tests move the clock past expiry |
| Requests go through `auth::oidc::OidcHttp` | Tests use `testkit::FakeOidc`, which signs ES256 with a fixed test key, so no network |
| The server's client is reqwest 0.12 with `rustls-tls-manual-roots` (ring), trusting `rustls-native-certs` plus `OIDC_CA_FILE`; no redirects, https only, 10 s timeout, 1 MB body limit | No webpki-roots (CDLA-Permissive-2.0), no aws-lc. Kanidm's self-signed CA goes in `OIDC_CA_FILE` |
| Discovery runs on the first login and again after 15 minutes; the HTTP client is rebuilt after a connection failure | Kanidm may start after the server, may rotate keys, and a fresh kanidm volume brings a new CA |
| The client secret is read from `OIDC_CLIENT_SECRET_FILE` at login time; missing or empty means 503 | `make kanidm-setup` writes it after the stack is up, without a server restart |
| Logout ends the local login only | Kanidm's own session stays; a passkey is needed again when it expires |
| `/staff/login` and `/staff/oidc/callback` share a rate limit: 10 at once, one more every 6 s per client, outside their session layer | Spec: rate limits on login. Same placement as the other login routes (ADR 0007) |
| Kanidm's scope map gives `openid email` to the group `bvw_staff` only | Kanidm decides who may log in at all; someone outside the group never gets a code. Inside the group, an unknown `sub` still gets the Dutch "no access" page and is never added |

The callback ends in `staff_session::sign_in`, like the dev login. Without `OIDC_ISSUER`,
`/staff/login` falls back to the dev login, or 503. A provider that cannot be reached
also gives 503, with a "try again in a few minutes" text.

## Local stack

In compose, Kanidm has the network alias `idm.localhost`, so the server reaches the
issuer `https://idm.localhost:8443/...` directly on Kanidm's own port and certificate.
`make kanidm-setup` creates the `bvw_staff` group, registers the OAuth2 client and copies
the client secret and Kanidm's CA into `deploy/secrets/oidc/`, which the server mounts
read-only. `make kanidm-person` creates a person in that group. See deploy/README.md.

The e2e VM keeps the dev login: a Kanidm login needs a passkey ceremony in the browser,
which the WebDriver flows cannot do cheaply. The OIDC flow is covered by the server tests
against `FakeOidc`.
