//! Who exists — the identity half of `system/auth/identities.json`.
//!
//! Design: `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md` §2.2.
//! An identity is a login name plus the scope root its data lives under.
//! The login name and the data directory are deliberately decoupled
//! (`scope_root`) so the first identity can adopt the existing
//! `scopes/anonymous/` tree without moving anything.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::AuthError;

/// Principal (and workspace-slug) names are directory names: **validated,
/// never hashed** (identity doc §8b correction 1). Lowercase ASCII letters,
/// digits, hyphen, underscore; 1–32 characters.
pub const PRINCIPAL_PATTERN: &str = r"^[a-z0-9_-]{1,32}$";

/// Hand-rolled `PRINCIPAL_PATTERN` check — the pattern is simple enough that
/// a regex dependency buys nothing.
pub fn validate_principal_name(name: &str) -> Result<(), AuthError> {
    let valid = (1..=32).contains(&name.chars().count())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(AuthError::InvalidPrincipalName(name.to_string()))
    }
}

/// One login identity. `scope_root` names the directory under `scopes/`
/// holding this identity's data — `"anonymous"` for the adopted owner,
/// the identity's own name for everyone created after auth landed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub display_name: String,
    pub scope_root: String,
    pub created_at: DateTime<Utc>,
}

/// `identities.json` — written only by this store; unknown fields are
/// tolerated on read so a newer writer's records still open.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IdentitiesFile {
    pub schema_version: u32,
    #[serde(default)]
    pub identities: Vec<Identity>,
}

/// The scope root adopted by the first identity created on an install that
/// already has data — everything live sits under `scopes/anonymous/default`
/// (identity doc §6).
pub const ADOPTED_SCOPE_ROOT: &str = "anonymous";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_lowercase_slugs() {
        for name in [
            "a",
            "owner",
            "live-eval",
            "storage_live-eval2",
            "x".repeat(32).as_str(),
        ] {
            assert!(
                validate_principal_name(name).is_ok(),
                "{name} should be valid"
            );
        }
    }

    #[test]
    fn rejects_path_tricks_unicode_case_and_length() {
        for name in [
            "",
            "../etc",
            "a/b",
            "UPPER",
            "Uniçode",
            "space name",
            "tab\tname",
            "x".repeat(33).as_str(),
            "semi;colon",
        ] {
            assert!(
                validate_principal_name(name).is_err(),
                "{name:?} should be rejected"
            );
        }
    }
}
