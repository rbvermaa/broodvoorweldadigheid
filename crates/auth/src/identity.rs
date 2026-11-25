/// A person as the identity provider vouches for them, after it checked their login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub sub: String,
    pub name: Option<String>,
    pub preferred_username: Option<String>,
    pub email: Option<String>,
    pub email_verified: bool,
}

impl Identity {
    /// The name the provider sent, if any.
    #[must_use]
    pub fn claimed_name(&self) -> Option<&str> {
        self.name.as_deref().or(self.preferred_username.as_deref())
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        self.claimed_name()
            .or(self.email.as_deref())
            .unwrap_or(&self.sub)
    }
}

/// The lowercase form of a UUID like Kanidm's `sub`, or `None` for anything else.
#[must_use]
pub fn canonical_uuid(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let groups: Vec<&str> = lower.split('-').collect();
    let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    let hex = groups
        .iter()
        .all(|group| group.bytes().all(|b| b.is_ascii_hexdigit()));
    (hex && lengths == [8, 4, 4, 4, 12]).then_some(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_falls_back_to_username_email_then_subject() {
        let mut identity = Identity {
            sub: "uuid".to_string(),
            name: Some("Bakker Bas".to_string()),
            preferred_username: Some("bas".to_string()),
            email: Some("bas@example.org".to_string()),
            email_verified: false,
        };
        assert_eq!(identity.display_name(), "Bakker Bas");
        identity.name = None;
        assert_eq!(identity.display_name(), "bas");
        identity.preferred_username = None;
        assert_eq!(identity.claimed_name(), None);
        assert_eq!(identity.display_name(), "bas@example.org");
        identity.email = None;
        assert_eq!(identity.display_name(), "uuid");
    }

    #[test]
    fn only_uuids_are_canonical() {
        assert_eq!(
            canonical_uuid("0D2F1F9E-1111-2222-3333-444455556666").as_deref(),
            Some("0d2f1f9e-1111-2222-3333-444455556666")
        );
        for bad in [
            "bas",
            "",
            "0d2f1f9e-1111-2222-3333-44445555666",
            "0d2f1f9e11112222333344445555666600",
            "0d2f1f9e-1111-2222-3333-44445555666g",
            " 0d2f1f9e-1111-2222-3333-444455556666",
        ] {
            assert_eq!(canonical_uuid(bad), None, "{bad:?}");
        }
    }
}
