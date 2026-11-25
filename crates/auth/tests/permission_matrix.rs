#![forbid(unsafe_code)]

use auth::{Permission, Role};

/// The answers of `Role::can` on 2026-11-25, before the table became declarative
/// (ADR 0044). Permissions added later are not listed here; their rows are in
/// docs/permissions.md.
const FROZEN: [(&str, bool, bool, bool); 15] = [
    // permission, owner, baker, counter
    ("ViewOrders", true, true, true),
    ("EditOrders", true, true, false),
    ("PickupList", true, true, true),
    ("Production", true, true, false),
    ("EditAllergens", true, true, false),
    ("ManageBakeDays", true, true, false),
    ("CloseBakeDay", true, false, false),
    ("Refund", true, true, false),
    ("ViewReports", true, false, false),
    ("Invoicing", true, false, false),
    ("ManageCharities", true, false, false),
    ("ManageStaff", true, false, false),
    ("Privacy", true, false, false),
    ("Polls", true, true, false),
    ("Settings", true, false, false),
];

#[test]
fn every_frozen_answer_still_holds() {
    for (name, owner, baker, counter) in FROZEN {
        let permission = Permission::ALL
            .into_iter()
            .find(|permission| format!("{permission:?}") == name)
            .unwrap_or_else(|| panic!("{name} is gone"));
        for (role, expected) in [
            (Role::Owner, owner),
            (Role::Baker, baker),
            (Role::Counter, counter),
        ] {
            assert_eq!(role.can(permission), expected, "{role} {name}");
        }
    }
}
