//! Link tokens: 32 random bytes as base64url; only their SHA-256 hash is stored.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use sha2::{Digest, Sha256};

const TOKEN_BYTES: usize = 32;
/// base64url without padding of [`TOKEN_BYTES`].
pub(crate) const TOKEN_LEN: usize = 43;

/// A new token and the hash to store.
pub(crate) fn new_token() -> (String, Vec<u8>) {
    let mut bytes = [0_u8; TOKEN_BYTES];
    rand::rng().fill(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash(&token);
    (token, hash)
}

fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// `None` for anything that cannot be one of our tokens, so junk never reaches the
/// database.
pub(crate) fn token_hash(token: &str) -> Option<Vec<u8>> {
    (token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| hash(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_43_url_safe_characters_and_hash_to_32_bytes() {
        let (token, stored) = new_token();
        assert_eq!(token.len(), TOKEN_LEN);
        assert_eq!(stored.len(), 32);
        assert_eq!(token_hash(&token), Some(stored));
        assert_ne!(new_token().0, token);
    }

    #[test]
    fn junk_tokens_never_reach_the_database() {
        assert_eq!(token_hash(""), None);
        assert_eq!(token_hash("abc"), None);
        assert_eq!(token_hash(&"a".repeat(44)), None);
        assert_eq!(token_hash(&format!("{}!", "a".repeat(42))), None);
        assert!(token_hash(&"a-_".repeat(14).chars().chain(['Z']).collect::<String>()).is_some());
    }
}
