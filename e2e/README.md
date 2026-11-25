Browser tests for the webshop and back office (thirtyfour). ADR 0033 has the design.

`make e2e` runs them in a NixOS VM (x86_64-linux with kvm; on macOS on `LINUX_BUILDER`).

`make e2e-local` runs the same binary against a server you start yourself, for example
while debugging a flow. It is not part of CI.

| What | How |
|---|---|
| Database | `make db-up`, then `cli migrate` with the same `DATABASE_URL`; the flows add their own data each run |
| Server | `BIND_ADDR=127.0.0.1:8080` and `DEV_STAFF_LOGIN=true` (the staff flows sign in through the dev login), `MOLLIE_API_KEY_FILE` holding any `test_` key, `MOLLIE_API_URL=http://127.0.0.1:8090/v2/`, an https `PUBLIC_URL`, `COOKIE_SECURE=false`, `GALLERY=true`, and no `SMTP_HOST` (the flows read emails from the outbox, which a sent email leaves without its text) |
| WebDriver | geckodriver with Firefox on 127.0.0.1:4444 |
| e2e | `make e2e-local`: `DATABASE_URL` comes from the Makefile, `AXE_JS` from `nix develop`; `E2E_SERVER_URL`, `E2E_WEBDRIVER_URL` and `E2E_MOLLIE_LISTEN` change the addresses |

The binary itself serves the mock Mollie on `--mollie-listen` and reads emails from
`email_outbox`. Firefox resolves `www.mollie.com` to localhost, so nothing reaches Mollie.
