# 0004 Compiled-in Europe/Amsterdam time zone

Status: proposed, 2026-11-24

`domain` gets the bakery time zone from `jiff::tz::get!("Europe/Amsterdam")` (jiff
feature `static`). The rules are compiled in from jiff's bundled tz database.

By default jiff reads `/usr/share/zoneinfo` at runtime. The Nix build sandbox and our
minimal images don't have it, so lookups would fail there or behave differently from a
laptop. A compiled-in zone gives the same answer in tests, CI and production, and the
lookup can't fail.

The cost: a change to Dutch time rules needs a jiff update and a rebuild. The rules have
been stable since 1996, and the weekly dependency update picks up new jiff releases.
`tzdb-bundle-always` was the alternative; it embeds every zone and still prefers the
host's files when present, so results could differ between hosts.
