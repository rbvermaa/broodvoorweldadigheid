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
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.preferred_username.as_deref())
            .unwrap_or(&self.sub)
    }
}
