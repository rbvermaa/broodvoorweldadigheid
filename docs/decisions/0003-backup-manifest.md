# 0003 Backup manifest and table fingerprints

Status: accepted, 2026-11-23

`cli backup run` stores a manifest next to each database dump. The nightly restore test
restores the dump into a scratch database and compares it with the manifest. A dump
that restores without errors but lost or changed rows then fails the test.

The manifest lists every user table with its row count and a fingerprint. Both are read
in the same transaction whose snapshot pg_dump uses, so they describe exactly what is in
the dump. The fingerprint hashes each row's text with md5 and adds up the two 64-bit
halves of the hashes as two numerics. Adding is order-independent and uses constant
memory, so it works for tables of any size. Aggregating all row text into one value
would hit Postgres' 1 GB limit. The row text output settings (time zone, date, interval,
float and bytea formats) are fixed for the read, so database defaults cannot change it.

The dump and the manifest are separate rustic snapshots. A shared `run-<id>` tag pairs
them. The restore test uses the newest db snapshot whose run has a manifest. A failed
backup forgets its partial db snapshot.

Table names come from `pg_class` and go into SQL as quoted identifiers
(`quote_ident`, which doubles embedded quotes). This deliberately departs from the
spec's "parameterised queries only" rule ("Security, backup and operations"): Postgres
can bind values but not identifiers. The names come from the database itself, never
from user input.
