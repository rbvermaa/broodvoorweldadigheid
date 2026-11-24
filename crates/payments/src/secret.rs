use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("cannot read {what} from {path}: {kind}")]
    Read {
        what: &'static str,
        path: String,
        kind: std::io::ErrorKind,
    },
    #[error("{what} must start with {prefixes} followed by letters and digits")]
    Invalid {
        what: &'static str,
        prefixes: &'static str,
    },
}

fn read(path: &Path, what: &'static str) -> Result<String, SecretError> {
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_string())
        .map_err(|err| SecretError::Read {
            what,
            path: path.display().to_string(),
            kind: err.kind(),
        })
}

fn check(
    value: &str,
    allowed: &[&'static str],
    what: &'static str,
    prefixes: &'static str,
) -> Result<&'static str, SecretError> {
    allowed
        .iter()
        .find(|prefix| {
            value.strip_prefix(**prefix).is_some_and(|rest| {
                !rest.is_empty()
                    && rest.len() <= 128
                    && rest.bytes().all(|b| b.is_ascii_alphanumeric())
            })
        })
        .copied()
        .ok_or(SecretError::Invalid { what, prefixes })
}

/// A Mollie website API key (`test_…` or `live_…`). Its `Debug` shows only the mode.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey {
    value: String,
    prefix: &'static str,
}

impl ApiKey {
    const WHAT: &'static str = "Mollie API key";
    const PREFIXES: &'static str = "test_ or live_";

    /// # Errors
    /// [`SecretError::Invalid`] unless the key looks like a Mollie API key.
    pub fn new(value: impl Into<String>) -> Result<Self, SecretError> {
        let value = value.into();
        let prefix = check(&value, &["test_", "live_"], Self::WHAT, Self::PREFIXES)?;
        Ok(Self { value, prefix })
    }

    /// Reads the key from a file (a sops secret); surrounding whitespace is ignored.
    ///
    /// # Errors
    /// [`SecretError`] when the file cannot be read or holds no valid key.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, SecretError> {
        Self::new(read(path.as_ref(), Self::WHAT)?)
    }

    #[must_use]
    pub fn is_test(&self) -> bool {
        self.prefix == "test_"
    }

    pub(crate) fn expose(&self) -> &str {
        &self.value
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ApiKey({}<redacted>)", self.prefix)
    }
}

/// A Mollie organization access token (`access_…`), needed for balance transactions.
#[derive(Clone, PartialEq, Eq)]
pub struct OrganizationToken(String);

impl OrganizationToken {
    const WHAT: &'static str = "Mollie organization access token";
    const PREFIXES: &'static str = "access_";

    /// # Errors
    /// [`SecretError::Invalid`] unless the token looks like a Mollie access token.
    pub fn new(value: impl Into<String>) -> Result<Self, SecretError> {
        let value = value.into();
        check(&value, &["access_"], Self::WHAT, Self::PREFIXES)?;
        Ok(Self(value))
    }

    /// # Errors
    /// [`SecretError`] when the file cannot be read or holds no valid token.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, SecretError> {
        Self::new(read(path.as_ref(), Self::WHAT)?)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OrganizationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OrganizationToken(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "test_dHar4XY7LxsDOtmnkVtjNVWXLSlXsM";

    fn temp_file(name: &str, contents: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bvw-payments-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn api_key_from_file_and_redacted() {
        let path = temp_file("api-key", &format!("{KEY}\n"));
        let key = ApiKey::from_file(&path).unwrap();
        assert!(key.is_test());
        assert_eq!(key.expose(), KEY);
        let debug = format!("{key:?}");
        assert_eq!(debug, "ApiKey(test_<redacted>)");
        assert!(!debug.contains("dHar4"));
        assert!(!ApiKey::new("live_abc").unwrap().is_test());
    }

    #[test]
    fn refuses_bad_keys() {
        for bad in [
            "",
            "test_",
            "test_a b",
            "abc",
            "access_abc",
            " test_abc",
            "TEST_abc",
        ] {
            assert_eq!(
                ApiKey::new(bad),
                Err(SecretError::Invalid {
                    what: "Mollie API key",
                    prefixes: "test_ or live_"
                }),
                "{bad:?}"
            );
        }
        let err = ApiKey::new(format!("test_{}", "a".repeat(129))).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Mollie API key must start with test_ or live_ followed by letters and digits"
        );
    }

    #[test]
    fn missing_file_names_the_path_not_the_contents() {
        let err = ApiKey::from_file("/nonexistent/mollie-key").unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot read Mollie API key from /nonexistent/mollie-key: entity not found"
        );
        let path = temp_file("bad-key", "live_secret value");
        let err = ApiKey::from_file(&path).unwrap_err();
        assert!(!err.to_string().contains("secret"));
    }

    #[test]
    fn organization_token() {
        let path = temp_file("org-token", "access_abc123\n");
        let token = OrganizationToken::from_file(&path).unwrap();
        assert_eq!(token.expose(), "access_abc123");
        assert_eq!(format!("{token:?}"), "OrganizationToken(<redacted>)");
        assert!(OrganizationToken::new(KEY).is_err());
        assert!(OrganizationToken::from_file("/nonexistent/token").is_err());
    }
}
