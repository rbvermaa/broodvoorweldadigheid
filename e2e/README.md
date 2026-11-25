Browser tests for the webshop and back office (thirtyfour). ADR 0033 has the design.

`make e2e` runs them in a NixOS VM (x86_64-linux with kvm; on macOS on `LINUX_BUILDER`).

The `e2e` binary also runs against any server, for example while debugging a flow:

| What | How |
|---|---|
| Database | Migrated (`cli migrate`); the flows add their own drop each run |
| Server | `MOLLIE_API_KEY_FILE` holding any `test_` key, `MOLLIE_API_URL=http://127.0.0.1:8090/v2/`, an https `PUBLIC_URL`, `COOKIE_SECURE=false`, `GALLERY=true` |
| WebDriver | geckodriver on 127.0.0.1:4444 |
| e2e | `DATABASE_URL` and `AXE_JS` (set in `nix develop`); `--help` lists the URLs and ports |

The binary itself serves the mock Mollie on `--mollie-listen` and reads emails from
`email_outbox`. Firefox resolves `www.mollie.com` to localhost, so nothing reaches Mollie.
