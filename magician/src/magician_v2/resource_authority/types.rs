use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Budget-row `id` that matches any principal / agent / tool at that scope.
///
/// One shared token is issued for the row (`issued_to` is `{scope}:*`). The
/// ceiling is a single pool, not a per-identity copy.
pub const WILDCARD_OWNER_ID: &str = "*";

/// Canonical commodity name: trim + ASCII uppercase.
///
/// Commodities are free-form strings (`USD`, `EMAIL_SENDS`, `INR`). Callers
/// historically mixed `usd` / `USD`; the ledger and token lookup compare
/// canonical forms so a budget row and a pack `spend:` declaration agree.
pub fn canonicalize_commodity(raw: &str) -> String {
    raw.trim().to_ascii_uppercase()
}

/// True when two commodity names refer to the same currency after canonicalization.
pub fn commodities_eq(left: &str, right: &str) -> bool {
    canonicalize_commodity(left) == canonicalize_commodity(right)
}

/// True when `issued_to` is a wildcard (`agent:*`) that covers `candidate`
/// (`agent:presto`, `agent:chat-session-owner`, …).
pub fn wildcard_issued_to_covers(issued_to: &str, candidate: &str) -> bool {
    let Some((issued_scope, issued_id)) = issued_to.split_once(':') else {
        return false;
    };
    if issued_id != WILDCARD_OWNER_ID {
        return false;
    }
    let Some((candidate_scope, candidate_id)) = candidate.split_once(':') else {
        return false;
    };
    issued_scope == candidate_scope && !candidate_id.is_empty() && candidate_id != WILDCARD_OWNER_ID
}

/// Account identifier — hierarchical string like "agent:cfo:available:USD"
pub type AccountId = String;

/// Token identifier — used to look up SpendTokens
pub type TokenId = String;

/// Unique identifier for an in-flight reservation
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReservationId(pub String);

impl ReservationId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl std::fmt::Display for ReservationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Default for ReservationId {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_commodity_trims_and_uppercases() {
        assert_eq!(canonicalize_commodity(" usd "), "USD");
        assert_eq!(canonicalize_commodity("EMAIL_SENDS"), "EMAIL_SENDS");
        assert_eq!(canonicalize_commodity("inr"), "INR");
        assert!(commodities_eq("usd", "USD"));
    }

    #[test]
    fn wildcard_issued_to_covers_same_scope_only() {
        assert!(wildcard_issued_to_covers("agent:*", "agent:presto"));
        assert!(wildcard_issued_to_covers("tool:*", "tool:agentmail-send"));
        assert!(!wildcard_issued_to_covers("agent:*", "principal:owner"));
        assert!(!wildcard_issued_to_covers("agent:presto", "agent:bob"));
        assert!(!wildcard_issued_to_covers("agent:*", "agent:*"));
        assert!(!wildcard_issued_to_covers("agent:*", "agent:"));
    }
}
