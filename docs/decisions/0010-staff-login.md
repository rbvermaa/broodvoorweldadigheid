# 0010 Staff login with Kanidm and roles (F13)

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

The table lives in `auth::Role::can` and its test. Routes call `Staff::require(permission)`.

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
| On login: clear the session, `cycle_id()`, then store only the staff id | Fixes ADR 0007's open item; a planted session id and its CSRF token stop working |
| The `Staff` extractor loads the staff row on every request | Deactivating someone or changing a role works at once |
| Unknown `sub` is refused. `BVW_BOOTSTRAP_OWNER_SUB` or `BVW_BOOTSTRAP_OWNER_EMAIL` (verified email only) becomes owner, but only while there is no owner; an advisory lock serializes bootstrap logins | No self-signup; a leftover env var cannot create a second owner. Prefer the sub: the email rule trusts Kanidm's email_verified |
| Others are added with `cli staff add --sub UUID --name NAME --role ROLE` | A staff screen comes later. Kanidm's `sub` is the person's UUID |
| Logout ends the local session only | Kanidm's own session stays; a passkey is needed again when it expires |
| `/staff/login` and `/staff/oidc/callback` share a rate limit: 10 at once, one more every 6 s per client | Spec: rate limits on login. The limit sits inside the session layer (ADR 0007 open item stays open) |

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
