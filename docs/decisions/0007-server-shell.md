# 0007 Server shell: assets, sessions, CSRF and rate limits

Status: accepted, 2026-11-24

| Choice | Why |
|---|---|
| htmx comes from `nix/htmx.nix` (`fetchurl` of the GitHub release, pinned by hash) and is embedded with `include_str!(env!("HTMX_JS"))` | No minified third-party JS in the repository, no CDN at runtime. nixpkgs has no htmx package. Building outside Nix fails with a clear message |
| htmx runs with `includeIndicatorStyles`, `allowEval` and `allowScriptTags` off; app.css carries the indicator styles | The CSP allows no inline styles or eval |
| Static files at `/static/NAME?v=HASH` (16 hex digits of SHA-256). A matching `v` gets `immutable` caching for a year; anything else gets `no-cache` with the ETag | A deploy changes the URL, so long caching is safe; old or bare URLs still revalidate |
| The session table `http_session` is created by a migration in `crates/db/migrations`; `PostgresStore::migrate` is never called | `db::MIGRATOR` stays the only place that changes the schema. Expired rows are deleted hourly by the server |
| `cli migrate` applies `db::MIGRATOR`; compose runs it as a one-shot `migrate` service (backup image, which holds the cli) and the server waits for it to complete | The server keeps starting while the database is down, so it does not migrate itself |
| The home page starts a session, so every visitor gets the `bvw_session` cookie | The shop's htmx actions need a CSRF token; the compose smoke test uses it to prove sessions are stored |
| Sessions save on every request through the session layer (`always_save`), so the 14 days are an idle timeout. `/health` and `/static` sit outside that layer | Without it, tower-sessions only moves the expiry when session data changes. Keeping assets out avoids a DB write per asset |
| Own CSRF check: a 32-byte random token per session, compared in constant time, sent as `X-CSRF-Token` (HTMX, via app.js) or a urlencoded `_csrf` field | A few dozen lines instead of another dependency. Multipart forms must send the header |
| The rate limiter keys on the TCP peer address, and on the last `X-Forwarded-For` entry only when the peer is `TRUSTED_PROXY` (Caddy's fixed address in compose) | Caddy appends the address it saw as the last entry; earlier entries come from the client and can be forged |
| The rate-limit key is the IPv4 address or the IPv6 /64 | One IPv6 client usually holds a whole /64 |
| Open for login and checkout: apply the rate limit outside the session and CSRF layers, call `session.cycle_id()` on login, consider a `__Host-` cookie name, and decide how htmx shows 4xx/5xx responses | Out of scope for the shell |
| `COOKIE_SECURE=false` turns off the Secure flag | Only for local runs over plain http; it defaults to on |
| CSP `form-action 'self'` | Open: the checkout slice must allow Mollie's checkout host |
| askama's `config` feature (for `apps/server/askama.toml`) pulls in `basic-toml`, which is unmaintained. Accepted: build-time only | Way out: move `templates/` under `apps/server/` and drop the feature |
