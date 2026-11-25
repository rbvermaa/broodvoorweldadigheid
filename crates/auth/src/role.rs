use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Owner,
    Baker,
    Counter,
}

macro_rules! permissions {
    ($($(#[doc = $doc:literal])+ $name:ident => [$($role:ident),+],)+) => {
        /// What a back-office action needs. The table per role is in docs/permissions.md.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Permission {
            $($(#[doc = $doc])+ $name,)+
        }

        impl Permission {
            pub const ALL: [Self; [$(Self::$name),+].len()] = [$(Self::$name),+];

            /// The roles that have this permission.
            #[must_use]
            pub const fn roles(self) -> &'static [Role] {
                match self {
                    $(Self::$name => &[$(Role::$role),+],)+
                }
            }

            /// What it covers, for docs/permissions.md.
            #[must_use]
            pub fn description(self) -> &'static str {
                match self {
                    $(Self::$name => concat!($($doc),+).trim(),)+
                }
            }
        }
    };
}

// One line per permission, sorted by name (ADR 0044). Owner is in every line.
permissions! {
    /// Sick-day cancel (F2), including that day's refunds (ADR 0028).
    CloseBakeDay => [Owner],
    /// Allergens, ingredient text, nutrition approval.
    EditAllergens => [Owner, Baker],
    /// Moving an order to another pickup slot, cancelling an unpaid order (F6), placing an
    /// order on account (F11a).
    EditOrders => [Owner, Baker],
    /// Business accounts, the invoice overview, marking orders invoiced (F11a).
    Invoicing => [Owner],
    /// Bake days, cutoffs, pickup slots, capacity.
    ManageBakeDays => [Owner, Baker],
    /// Charities and payouts.
    ManageCharities => [Owner],
    /// Adding staff and changing roles.
    ManageStaff => [Owner],
    /// Pickup list, marking orders picked up.
    PickupList => [Owner, Baker, Counter],
    /// Bread votes: create, change, see results and wishes (F9b, ADR 0039).
    Polls => [Owner, Baker],
    /// Exporting and erasing a customer's data on their request (F12, ADR 0031).
    Privacy => [Owner],
    /// Production list, recipes, batches, labels.
    Production => [Owner, Baker],
    /// Cancelling a paid order with its refund, retrying a refund (F6, ADR 0028).
    Refund => [Owner, Baker],
    /// Company settings, such as switching the public donation page on (F11d, ADR 0040).
    Settings => [Owner],
    /// Order list and order details.
    ViewOrders => [Owner, Baker, Counter],
    /// Sales, donation and contribution reports (F11c, F11e).
    ViewReports => [Owner],
}

impl Role {
    pub const ALL: [Self; 3] = [Self::Owner, Self::Baker, Self::Counter];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Baker => "baker",
            Self::Counter => "counter",
        }
    }

    #[must_use]
    pub const fn can(self, permission: Permission) -> bool {
        let roles = permission.roles();
        let mut i = 0;
        while i < roles.len() {
            if roles[i] as u8 == self as u8 {
                return true;
            }
            i += 1;
        }
        false
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown role {0:?}; expected owner, baker or counter")]
pub struct UnknownRole(pub String);

impl FromStr for Role {
    type Err = UnknownRole;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == raw)
            .ok_or_else(|| UnknownRole(raw.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_can_do_everything_and_can_follows_the_table() {
        for permission in Permission::ALL {
            assert!(Role::Owner.can(permission), "owner {permission:?}");
            for role in Role::ALL {
                assert_eq!(
                    role.can(permission),
                    permission.roles().contains(&role),
                    "{role} {permission:?}"
                );
            }
            assert!(!permission.description().is_empty(), "{permission:?}");
        }
    }

    #[test]
    fn permissions_are_sorted_by_name() {
        let names = Permission::ALL.map(|permission| format!("{permission:?}"));
        assert!(names.is_sorted(), "{names:?}");
    }

    #[test]
    fn roles_round_trip_through_their_database_names() {
        for role in Role::ALL {
            assert_eq!(role.as_str().parse::<Role>(), Ok(role));
            assert_eq!(role.to_string(), role.as_str());
        }
        assert_eq!(
            "Owner".parse::<Role>(),
            Err(UnknownRole("Owner".to_string()))
        );
        assert!(
            UnknownRole("admin".to_string())
                .to_string()
                .contains("owner, baker or counter")
        );
    }
}
