# 0043 Coverage gate ignores the signature lines of run async fns

Status: accepted, 2026-11-25 (owner approved)

**The artefact.** An `async fn` compiles to two functions: a small wrapper that takes the
arguments and builds the future, and the future's body. llvm-cov gives the wrapper its own
region, which covers the signature: the `async fn` line, each parameter line rustfmt
splits out, and the line with the `{` of the body. When several test binaries contain a
copy of the wrapper, llvm-cov keeps one copy per function, and that copy may be one no
test ran. The signature lines then show 0 hits, although tests call the function and its
body lines are hit. Which copy wins changes from build to build.

**Evidence.** Two workers measured `crates/orders` swing between 94.9% and 96.5% on
unchanged code, around the 95% threshold of the strict crates. About 90 lines in `orders`
are such signature lines. ADR 0033 already took e2e out of the coverage run for the same
reason; the remaining test binaries still trigger it. Builds of `coverage-report` on dev1
before and after this change:

MEASUREMENTS

**The rule.** `cargo xtask coverage` reads the source file of each lcov `SF` record and
finds every `async fn` with a body (`xtask/src/async_fn.rs`). When at least one line of
the body (the lines after the opening `{` through the closing `}`) has a hit, the
signature lines, from the `async` keyword through the `{`, leave both the found and the hit
count, whether llvm-cov marked them hit or not. When no body line has a hit, the
signature lines stay and count as missed lines. Thresholds stay at 80% overall and 95% in
the strict crates. The coverage output prints per crate how many lines were left out, how
many of those had no hits, and the percentage without the rule.

| Choice | Why |
|---|---|
| Leave out the signature lines in both directions (hit or missed) | Leaving out only the missed ones would still let the count of found lines move with the build |
| Only when a body line is hit | An untested function still lowers coverage, through its body and its signature |
| Parse the source in xtask, with a small tokenizer that skips comments, strings, chars and lifetimes | No new dependency. Checked against all workspace files: it finds every `async fn` with a body that a line search finds |
| The report keeps line data (`--summary-only` is gone); a record without `DA` lines is an error | The rule needs per-line hits; failing loudly keeps the rule from silently turning off |

**Why this does not weaken the gate.** The left-out lines carry no information about
tests: a hit body line proves the wrapper ran, because the body only runs through it. All
code that does work (the body) still counts, and the rule never hides a function that no
test calls. The thresholds did not change.
