# 0033 Browser tests in a NixOS VM without the Python test driver

Status: accepted, 2026-11-25 (owner approved the HTTP exception for e2e)

"Testing and quality": browser tests (thirtyfour) inside a NixOS VM test, with axe-core.
The crate is `e2e/`; the VM is `nix/e2e-vm.nix`, run by `make e2e` and the `e2e` CI job.

| Choice | Why |
|---|---|
| No `runNixOSTest`. A NixOS system built with the `qemu-vm` module runs PostgreSQL, `cli migrate`, the server and geckodriver as systemd units. A one-shot `e2e` unit runs the e2e binary, prints `E2E_EXIT_CODE=<n>` (and on failure the service logs) on the serial console and powers off. `checks.x86_64-linux.e2e` is a plain derivation with the `kvm` feature that boots the VM, keeps the console log in its output and fails unless the code is 0 | The NixOS test driver is a Python program and its test script is Python. CLAUDE.md allows no Python in code, scripts, CI or tooling |
| Neither the VM nor its runner has Python in its closure, and the check fails if a store path named `python…` enters the closure of the runner (which includes the VM). Three changes were needed: geckodriver runs `firefox-unwrapped`, because the wrapper adds libcanberra and pipewire (gstreamer, Python); qemu is built with `nixosTestRunner` and without brltty, because the default links brltty, spice and audio libraries (Python); and the VM has no shared directories, because with any the runner starts virtiofsd through a Python script. The store comes from an erofs image built when the VM starts | Checked with `nix path-info -r` on x86_64-linux (nixpkgs 26.11): `python3-3.14` came in through each of the three; 0 Python paths now. The guard keeps a nixpkgs bump from bringing it back unnoticed. The qemu variant is not in cache.nixos.org, so it builds once (about 3 minutes on dev1) and then comes from Cachix |
| The e2e binary runs the mock Mollie itself (axum, on `--mollie-listen`); the server reaches it through `MOLLIE_API_URL`. It speaks the part of Mollie's API `payments::MollieClient` uses, and its unit tests run that client against it | The flows can pay a payment and send the webhook without a second process. Testing the mock with our own client keeps the two from drifting |
| The mock's checkout link is `https://www.mollie.com/checkout/…`, and Firefox resolves `www.mollie.com` to localhost (`network.dns.localDomains`). The flow waits for that URL, pays in the mock, posts the webhook to the server and opens the redirect URL | The server only accepts https checkout links, and its CSP only lets forms go to `www.mollie.com`. Nothing reaches the internet, inside the VM or outside it |
| The server runs with an https `PUBLIC_URL` that does not resolve; the browser uses `BIND_ADDR` over http with `COOKIE_SECURE=false`. The mock rewrites webhook and redirect URLs to the server's own address | Order confirmations are only queued with an https `PUBLIC_URL` |
| Emails are read from `email_outbox`; no SMTP server or Mailpit | Without `SMTP_HOST` the confirmation stays in the outbox, queued in the webhook's transaction |
| Each flow seeds its own data (a drop at least a week ahead, a product with approved allergens, a charity) through `bakedays` and SQL, so a flow can run again on the same database | No shared fixtures between flows; the binary also runs outside the VM (`e2e/README.md`) |
| axe-core 4.14.1 is pinned in `nix/axe-core.nix` (npm tarball) and injected with `execute`; the check runs the `wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa` and `wcag22aa` rules and fails on any violation | WCAG 2.2 AA from the spec. The server never serves it |
| `e2e` is layer 8: it may use every crate and HTTP crates, and nothing may depend on it (`cargo xtask layers`). Like xtask it sits outside `crates/` and `apps/`, so the coverage gate does not count it, and the coverage run leaves it out (`--exclude e2e`); its unit tests run in the nextest check | It is a test of the apps, not part of them. Its binaries add copies of the small async fn wrappers of the crates it calls. llvm-cov counts one copy per function, so the signature lines of async fns flip between run and missed with the set of test binaries; e2e should not move the gate of the crates it tests |
| thirtyfour turns on serde_json's `preserve_order`, and workspace builds share it. The two `production` snapshot tests of the JSON handed to Typst now sort keys first; the snapshots did not change. The nextest check runs with it, the coverage run without it; nothing else depends on key order. The images build the server and cli per package, without it | Feature unification would otherwise make those snapshots depend on whether e2e is in the build |
| CI: one matrix job `e2e` that makes `/dev/kvm` usable with a udev rule and adds `kvm` to Nix's system features; it fails loudly if the runner has no `/dev/kvm` | GitHub's ubuntu-24.04 runners have KVM, but only root can open it by default |

Follow-ups, each its own PR:

- Visual tests of the component gallery: screenshots compared with approved images
  (image-compare in the spec), with the approved images in the repo.
- Flows: back office through the dev login, sick-day cancel, choose another charity,
  customer login link, pickup list.
- Chromium and WebKit next to Firefox, and iPhone and desktop window sizes next to laptop.
- Boot the OCI images with Kanidm and Caddy in the VM, as the spec's pipeline describes,
  instead of the binaries as systemd units.
- A `make` target to run the e2e binary against a local server outside the VM.
