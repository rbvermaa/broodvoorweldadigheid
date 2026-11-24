# 0013 Payments: PaymentProvider trait and own Mollie client

Status: accepted, 2026-11-24

| Choice | Why |
|---|---|
| `PaymentProvider` in `domain` returns boxed futures (`ProviderFuture`), not `async fn` | `async fn` in a trait is not dyn-compatible. The server can hold an `Arc<dyn PaymentProvider>` and tests swap in the fake, without the `async-trait` crate |
| Our own small client on reqwest 0.12, not mollie-rs | Four endpoints are all we need; no generated code to review or upgrade |
| reqwest 0.12 with `rustls-tls-native-roots` (ring, roots from `rustls-native-certs`); the client also checks up front that the system has roots | reqwest 0.13's rustls feature pulls in aws-lc (OpenSSL licence); `webpki-roots` is CDLA (open questions). reqwest's own loader skips system certificates rustls cannot parse, which `add_root_certificate` would not. The server image must ship a CA bundle (`SSL_CERT_FILE` or `/etc/ssl/certs`); the client refuses to start without roots |
| Secrets come from files: `MollieConfig { api_key_file, organization_token_file }`. `ApiKey` and `OrganizationToken` redact themselves in `Debug`; errors name the file, never its contents | Spec, Security |
| Base URL must be https; plain http only on 127.0.0.1/localhost (tests). Redirects are not followed | The bearer token never leaves for another host |
| Only GETs are retried (3 attempts, 250 ms doubling) on 5xx, 429, timeouts and connection errors. POSTs are sent once with an `Idempotency-Key`. Refunds must carry a caller-chosen key (for example `refund-<order>-<n>`); creates may, else the client sends a fresh UUID | Mollie keeps a key for one hour and replays the first answer, so a refund retried after a lost answer does not pay out twice |
| Amounts are parsed strictly: `-?(0\|[1-9][0-9]*)\.[0-9]{2}`, EUR only, `-0.00` refused | Exact conversion to `Money` cents, no floats |
| `parse_webhook_body` only extracts `id=tr_…` (alphanumeric, at most 32 characters after `tr_`, body at most 1 KiB) | The webhook carries nothing else to trust; the server calls `get_payment` before changing an order |
| Mollie errors map to `PaymentError`: 401/403 Unauthorized, 404/410 NotFound, 400/422 Rejected (with Mollie's `detail` and `field`), 409 Conflict, 429 RateLimited, 5xx and transport errors Unavailable, anything unparseable InvalidResponse | Callers decide on typed errors, not HTTP codes |
| Fees (F11e) come from `GET /v2/balances/primary/transactions`: per transaction with a paymentId: `payment`/`capture` (Payment), `refund`, `chargeback`, and separate fee transactions (`payment-fee`, `failed-payment-fee`, `payment-commission`, `capture-commission`, `reimbursement-fee`; their own amount is the charge). Fee = −(`deductionDetails.fees` + `deductionDetails.commissions`); only when `deductionDetails` is absent, −`deductions`. Reservations and repayments are not fees. With a `test_` API key the request adds `testmode=true`, since an organization token does not inherit the key's mode. A transaction that cannot be read (no paymentId, bad amount, negative fee) is skipped with a warning instead of failing the page | `settlementAmount` on a payment is deprecated (removed 1 Jan 2027) and points here. This endpoint needs an organization access token with `balances.read`; the website API key cannot read it. Without a token `list_payment_fees` returns `NotConfigured` |
| Methods sent: `ideal`, `wero`, `creditcard`, `bancontact` (all four exist in Mollie's create-payment API). An empty list lets the customer choose from every method on the account | Spec: never hard-code iDEAL. Reading Mollie's method list (`GET /v2/methods`) is left for the checkout slice |
| The payment's metadata carries `{"order_id": n}` and `get_payment` returns it | The server can check that a webhook's payment belongs to the order it updates |
| HTTP tests use a hand-written HTTP/1.1 mock on 127.0.0.1, not axum | The layer check allows HTTP crates only in `server`, also as dev-dependencies |
| The contract test (`make mollie-contract`) runs only when `MOLLIE_TEST_API_KEY_FILE` is set and refuses a `live_` key. It shares its scenario with a mock-backed test that always runs | Normal runs stay offline, and the scenario cannot rot unnoticed |

Uncertain, to confirm against Mollie test mode:

- A refund on an unpaid payment answers 422 (the contract test asserts `Rejected`).
- Whether `amountRefunded` includes refunds still `queued`/`pending`.
- Whether balance-transaction fees include BTW; Mollie invoices fees monthly.
- Whether a single-method payment reports `method` before the customer pays (the contract accepts `null` or `ideal`).
