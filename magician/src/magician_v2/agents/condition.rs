//! Shared condition expression evaluation for agent interpreters.

use tracing::warn;

/// Evaluate a simple condition expression.
///
/// Currently supports only `true`, `false`, and empty/`None` (→ true).
/// Unknown expressions fail closed with a warning — the expression engine
/// is scheduled for a later Phase 3 ticket.
pub fn condition_allows(condition: Option<&str>, context: &str) -> bool {
    match condition.map(str::trim) {
        None | Some("") | Some("true") => true,
        Some("false") => false,
        Some(expr) => {
            warn!(
                expression = expr,
                context,
                "unknown condition expression — failing closed; \
                 check for typos or wait for expression engine"
            );
            false
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn condition_allows_true_variants() {
        assert!(condition_allows(None, "test"));
        assert!(condition_allows(Some(""), "test"));
        assert!(condition_allows(Some("true"), "test"));
        assert!(condition_allows(Some("  true  "), "test"));
    }

    #[test]
    fn condition_allows_false_and_unknown() {
        assert!(!condition_allows(Some("false"), "test"));
        assert!(!condition_allows(Some("episode.outcome.is_failed"), "test"));
    }
}
