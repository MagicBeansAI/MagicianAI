//! How identities prove themselves — `system/auth/credentials.json`.
//!
//! Design: `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md` §2.3.
//! Identity ↔ credential is 1:N — a password, a Google link, and a GitHub
//! link on one account are three rows. `OAuthLink` keys on the provider's
//! **subject id, never the email** (emails change and are takeover bait).

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::AuthError;

/// Social login providers shipping in v1 (workspace design §1: Google and
/// GitHub both, owner decision 2026-08-23).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Google,
    Github,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Github => "github",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "google" => Some(Provider::Google),
            "github" => Some(Provider::Github),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialKind {
    /// Argon2 PHC-string hash. Never the plaintext, never reversible.
    Password { hash: String },
    /// Provider user id — stable and unique per provider. Not the email.
    OAuthLink { provider: Provider, subject: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credential {
    pub id: Uuid,
    /// `Identity.name` this credential proves.
    pub identity: String,
    pub kind: CredentialKind,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<DateTime<Utc>>,
}

/// How a session was minted — recorded on the session for the settings UI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Password,
    Google,
    Github,
}

/// `credentials.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialsFile {
    pub schema_version: u32,
    #[serde(default)]
    pub credentials: Vec<Credential>,
}

/// Hash a password with argon2 (crate defaults, random salt, PHC string).
/// The RustCrypto standard path — never hand-roll.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| AuthError::PasswordHash(error.to_string()))
}

/// Verify a password against a PHC-string hash. Malformed stored hashes
/// verify as `false` — a corrupt row must not lock out every login with a
/// 500, and it must never succeed.
pub fn verify_password(stored_hash: &str, password: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_roundtrip_and_rejection() {
        let hash = hash_password("correct horse battery staple").expect("hash");
        assert!(hash.starts_with("$argon2"));
        assert!(verify_password(&hash, "correct horse battery staple"));
        assert!(!verify_password(&hash, "wrong password"));
    }

    #[test]
    fn malformed_hash_fails_closed() {
        assert!(!verify_password("not-a-phc-string", "anything"));
        assert!(!verify_password("", "anything"));
    }
}
