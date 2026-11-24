# 0004 Compiled-in Europe/Amsterdam time zone

Status: accepted, 2026-11-24

`domain` gets the bakery time zone from `jiff::tz::get!("Europe/Amsterdam")` (jiff
feature `static`). The rules are compiled in from the `jiff-tzdb` crate, so the tz data
is pinned by the `jiff-tzdb` version in `Cargo.lock`.

All code gets the zone through `domain::bakery_time_zone()` and the time through a
`domain::Clock`. A test in xtask (`time_zone.rs`) fails on `TimeZone::system`,
`Zoned::now` or `TimeZone::get(` outside `crates/domain`.

By default jiff reads `/usr/share/zoneinfo` at runtime. The Nix build sandbox and our
minimal images don't have it, so lookups would fail there or behave differently from a
laptop. A compiled-in zone gives the same answer in tests, CI and production, and the
lookup can't fail.

The cost: a change to Dutch time rules needs `cargo update -p jiff-tzdb` and a rebuild.
`tzdb-bundle-always` was the alternative; it embeds every zone and still prefers the
host's files when present, so results could differ between hosts.
