use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Owner,
    Baker,
    Counter,
}

/// What a back-office action needs. The table per role is in ADR 0010.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    ViewOrders,
    /// Moving an order to another pickup slot and cancelling an unpaid one (F6).
    EditOrders,
    PickupList,
    Production,
    EditAllergens,
    ManageBakeDays,
    /// Sick-day cancel (F2), including the refunds it sends for that day.
    CloseBakeDay,
    /// Refunding a single order or line.
    Refund,
    ViewReports,
    ManageCharities,
    ManageStaff,
}

impl Permission {
    pub const ALL: [Self; 11] = [
        Self::ViewOrders,
        Self::EditOrders,
        Self::PickupList,
        Self::Production,
        Self::EditAllergens,
        Self::ManageBakeDays,
        Self::CloseBakeDay,
        Self::Refund,
        Self::ViewReports,
        Self::ManageCharities,
        Self::ManageStaff,
    ];
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
        use Permission as P;
        match self {
            Self::Owner => true,
            Self::Baker => matches!(
                permission,
                P::ViewOrders
                    | P::EditOrders
                    | P::PickupList
                    | P::Production
                    | P::EditAllergens
                    | P::ManageBakeDays
                    | P::CloseBakeDay
            ),
            Self::Counter => matches!(permission, P::ViewOrders | P::PickupList),
        }
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
    fn permission_table_matches_adr_0010() {
        use Permission as P;
        let expected: [(P, bool, bool, bool); 11] = [
            // permission, owner, baker, counter
            (P::ViewOrders, true, true, true),
            (P::EditOrders, true, true, false),
            (P::PickupList, true, true, true),
            (P::Production, true, true, false),
            (P::EditAllergens, true, true, false),
            (P::ManageBakeDays, true, true, false),
            (P::CloseBakeDay, true, true, false),
            (P::Refund, true, false, false),
            (P::ViewReports, true, false, false),
            (P::ManageCharities, true, false, false),
            (P::ManageStaff, true, false, false),
        ];
        assert_eq!(
            expected.map(|(permission, ..)| permission),
            Permission::ALL,
            "every permission is in the table once"
        );
        for (permission, owner, baker, counter) in expected {
            assert_eq!(Role::Owner.can(permission), owner, "owner {permission:?}");
            assert_eq!(Role::Baker.can(permission), baker, "baker {permission:?}");
            assert_eq!(
                Role::Counter.can(permission),
                counter,
                "counter {permission:?}"
            );
        }
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
