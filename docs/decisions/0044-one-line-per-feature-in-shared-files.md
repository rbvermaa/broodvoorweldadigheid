# 0044 One line per feature in shared files

Status: accepted, 2026-11-25

**The problem.** Almost every feature PR conflicted with every other one in the same few
shared lists: the permission lists per role and `Permission::ALL` (its length bumped by
hand), the back-office router, the staff menu, the domain id list, the email kind check
that each migration restated in full, `migrations.lock` and `docs/open-questions.md`.
Each conflict cost a merge round. Now a feature adds its own files plus one line per
shared list (two in the back office), sorted by name, so two features rarely touch
neighbouring lines. Behaviour did not change.

| Hotspot | Now | A new feature adds |
|---|---|---|
| Permissions (`auth::role`) | One `permissions!` line per permission, sorted: name and roles, with the doc comment as its description. `Permission::ALL`, `roles()` and `Role::can` come from it | One line (and its doc comment) |
| Permission table in ADR 0010 | `docs/permissions.md`, generated from the code by `make permissions-doc`. A test fails when it is out of date | The regenerated row |
| Back office (`backoffice/mod.rs`) | Each module has `pub fn routes()`. `mod.rs` has its `pub mod` line and one line in `FEATURES`, both sorted, and merges the routes. The module doc no longer lists the features | Two lines, in two separate lists |
| Staff menu (`templates/staff/layout.html`) | `backoffice::nav`: one line per entry with an `order`, sorted by path. The template loops over it | One line |
| Domain ids | `id_type!` list sorted by name; `lib.rs` re-exports with `pub use ids::*` | One line |
| Email kinds | `email_kinds!` line per kind in `notify`, sorted; table `email_kind` with a foreign key from `email_outbox.kind` | One line in Rust, one `INSERT INTO email_kind` in its own migration |
| `migrations.lock`, `docs/open-questions.md` | `merge=union` in `.gitattributes` | Its own lines, as before |

| Choice | Why |
|---|---|
| Permission rows list every role that has it, owner included; a test checks that owner has every permission | ADR 0010 says owner can do everything. Listing it keeps the row readable on its own and the generated table complete |
| The old table test became a frozen fixture (`crates/auth/tests/permission_matrix.rs`) of the 15 answers on 2026-11-25 | Proves the switch kept every (role, permission) answer. It does not list later permissions, so it is not a new hotspot |
| The doc is a checked file plus a test, not an xtask | It needs `auth`, and a test runs in `nix flake check` already. `docs/permissions.md` is in the Nix source set for that test |
| Plain `pub mod` lines, not a macro that declares the module and its routes in one line | rustfmt does not see modules declared inside a macro, so their files would drop out of the fmt check |
| Menu entries carry an `order` and the source is sorted by path | Sorting by menu position puts every new entry at the end, which is where two features collide. Orders are spaced by 10. Entries are not filtered by permission; that would change what staff see |
| `email_kind` lookup table with a foreign key, not "no check, Rust enum only" | The database still refuses an unknown kind. A test checks the table holds exactly the kinds in `EmailKind::ALL` |
| `email_outbox_kind_check` keeps its name but now only refuses `''` | Migrations only add or extend (CLAUDE.md rule 7). The check is widened, not removed. Keeping the name means a branch written before this, which drops and re-adds the check with its own list, still migrates; that check then narrows again until a later migration widens it. New branches add an `email_kind` row instead |
| Union merge for `migrations.lock` and `open-questions.md`, no split into one file per question | Both are mostly appended to; union keeps both sides' new lines. The lock test reads it as a set, so order does not matter, and it reports a line listed twice. Splitting open questions would change where CLAUDE.md rule 11 points. Risk: when two branches edit the same question, union keeps both versions instead of a conflict; that shows in the diff |

Union applies when a branch merges `origin/main` locally, which is how we resolve
conflicts; GitHub's own conflict check may not honour it. The Makefile's target lists and the `flake.nix` checks are still single
lists.
