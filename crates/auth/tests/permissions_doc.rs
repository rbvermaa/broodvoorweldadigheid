#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::path::Path;

use auth::{Permission, Role};

fn table() -> String {
    let mut out = String::from(
        "# Permissions\n\n\
         Generated from `crates/auth/src/role.rs` by `make permissions-doc`; do not edit by hand.\n\
         Roles, sign-in and how routes check a permission: ADR 0010.\n\n\
         | Permission | What it covers |",
    );
    for role in Role::ALL {
        let _ = write!(out, " {role} |");
    }
    out.push_str("\n|---|---|");
    out.push_str(&"---|".repeat(Role::ALL.len()));
    out.push('\n');
    for permission in Permission::ALL {
        let _ = write!(out, "| {permission:?} | {} |", permission.description());
        for role in Role::ALL {
            out.push_str(if role.can(permission) { " ✓ |" } else { " |" });
        }
        out.push('\n');
    }
    out
}

#[test]
fn docs_permissions_md_matches_the_code() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/permissions.md");
    let expected = table();
    if std::env::var_os("BVW_WRITE_PERMISSIONS_DOC").is_some() {
        std::fs::write(&path, &expected).expect("write docs/permissions.md");
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        on_disk == expected,
        "docs/permissions.md is out of date; run `make permissions-doc`. Expected:\n{expected}"
    );
}
