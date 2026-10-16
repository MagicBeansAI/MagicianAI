//! Criticality Handling
//!
//! This module handles LLM-provided criticality hints to determine
//! pre-action confirmation behavior.
//!
//! ## LLM-Only Design
//!
//! Criticality is determined SOLELY from the LLM's `criticality_hint` field.
//! There are NO heuristics or keyword matching to infer criticality.
//!
//! When `criticality_hint` is missing:
//! - Default to `Medium` criticality
//! - Log an error (prompt bug that needs fixing)
//!
//! ## Criticality Levels
//!
//! | Level | Description | Human Confirmation |
//! |-------|-------------|-------------------|
//! | Low | Non-destructive, reversible | Never |
//! | Medium | Default for mutations | Never |
//! | High | Important actions | Optional |
//! | Critical | Destructive, irreversible | Required |

use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use crate::magician_v2::execution::verified_executor::types::ActionCandidate;

// =============================================================================
// Criticality Level
// =============================================================================

/// Criticality level for an action.
///
/// This is parsed from the LLM's `criticality_hint` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum CriticalityLevel {
    /// Low criticality - non-destructive, reversible actions.
    ///
    /// Examples: clicking a link, scrolling, hovering.
    Low,

    /// Medium criticality - default for mutations.
    ///
    /// Examples: typing in a form, selecting an option.
    #[default]
    Medium,

    /// High criticality - important actions that need care.
    ///
    /// Examples: submitting a form, making a purchase (non-final).
    High,

    /// Critical - destructive, irreversible actions.
    ///
    /// Examples: deleting data, confirming payment, account deletion.
    Critical,
}

impl CriticalityLevel {
    /// Parse criticality from LLM hint string.
    ///
    /// If the hint is invalid or missing, defaults to Medium and logs an error.
    pub fn from_hint(hint: Option<&str>) -> Self {
        match hint {
            Some(h) => match h.to_lowercase().as_str() {
                "low" => CriticalityLevel::Low,
                "medium" => CriticalityLevel::Medium,
                "high" => CriticalityLevel::High,
                "critical" => CriticalityLevel::Critical,
                other => {
                    error!(
                        "[CRITICALITY] Unknown criticality_hint '{}', defaulting to Medium. Fix LLM prompt.",
                        other
                    );
                    CriticalityLevel::Medium
                },
            },
            None => {
                debug!("[CRITICALITY] criticality_hint absent, defaulting to Medium");
                CriticalityLevel::Medium
            },
        }
    }

    /// Check if this criticality requires human confirmation.
    pub fn requires_human_confirmation(&self) -> bool {
        matches!(self, CriticalityLevel::Critical)
    }

    /// Get a string representation for logging.
    pub fn as_str(&self) -> &'static str {
        match self {
            CriticalityLevel::Low => "low",
            CriticalityLevel::Medium => "medium",
            CriticalityLevel::High => "high",
            CriticalityLevel::Critical => "critical",
        }
    }
}

// =============================================================================
// Criticality Evaluator
// =============================================================================

/// Evaluator for action criticality based on LLM hints.
///
/// This evaluator uses ONLY LLM-provided hints. There are no heuristics.
#[derive(Debug, Clone, Default)]
pub struct CriticalityEvaluator;

impl CriticalityEvaluator {
    /// Create a new criticality evaluator.
    pub fn new() -> Self {
        Self
    }

    /// Evaluate the criticality of an action candidate.
    ///
    /// Uses the LLM-provided `criticality_hint` field, elevated by `page_context_hint`
    /// when the page context indicates a sensitive operation.
    pub fn evaluate(&self, candidate: &ActionCandidate) -> CriticalityLevel {
        let mut level = CriticalityLevel::from_hint(candidate.criticality_hint.as_deref());

        // Elevate criticality based on page_context_hint
        // Sensitive page contexts warrant higher criticality even if LLM said "low"
        if let Some(ref context) = candidate.page_context_hint {
            let context_lower = context.to_lowercase();
            let elevated = match context_lower.as_str() {
                // These contexts always elevate to Critical
                "payment"
                | "checkout"
                | "purchase"
                | "billing"
                | "delete"
                | "remove"
                | "terminate"
                | "cancel_subscription"
                | "account_deletion" => {
                    if level < CriticalityLevel::Critical {
                        info!(
                            "[CRITICALITY] Elevating from {:?} to Critical due to page_context_hint='{}'",
                            level, context
                        );
                        CriticalityLevel::Critical
                    } else {
                        level
                    }
                },
                // These contexts elevate to at least High
                "authentication" | "login" | "password" | "security" | "settings" | "admin"
                | "2fa" | "mfa" => {
                    if level < CriticalityLevel::High {
                        info!(
                            "[CRITICALITY] Elevating from {:?} to High due to page_context_hint='{}'",
                            level, context
                        );
                        CriticalityLevel::High
                    } else {
                        level
                    }
                },
                // Other contexts don't affect criticality
                _ => level,
            };
            level = elevated;
        }

        info!(
            "[CRITICALITY] Action '{}' evaluated as {:?} (hint: {:?}, page_context: {:?})",
            candidate.action.action_type_name(),
            level,
            candidate.criticality_hint,
            candidate.page_context_hint
        );

        level
    }

    /// Evaluate the maximum criticality across a batch of candidates.
    ///
    /// Returns the highest criticality level in the batch.
    pub fn evaluate_batch(&self, candidates: &[ActionCandidate]) -> CriticalityLevel {
        candidates
            .iter()
            .map(|c| self.evaluate(c))
            .max()
            .unwrap_or_default()
    }

    /// Determine if the candidate should trigger human confirmation.
    ///
    /// Based on criticality and `requires_confirmation` flag.
    pub fn requires_confirmation(&self, candidate: &ActionCandidate) -> bool {
        let criticality = self.evaluate(candidate);

        // Critical actions always require confirmation
        if criticality.requires_human_confirmation() {
            return true;
        }

        // LLM can also explicitly request confirmation
        if candidate.requires_confirmation.unwrap_or(false) {
            info!(
                "[CRITICALITY] Action '{}' has requires_confirmation=true",
                candidate.action.action_type_name()
            );
            return true;
        }

        false
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_criticality_from_hint() {
        assert_eq!(
            CriticalityLevel::from_hint(Some("low")),
            CriticalityLevel::Low
        );
        assert_eq!(
            CriticalityLevel::from_hint(Some("LOW")),
            CriticalityLevel::Low
        );
        assert_eq!(
            CriticalityLevel::from_hint(Some("medium")),
            CriticalityLevel::Medium
        );
        assert_eq!(
            CriticalityLevel::from_hint(Some("high")),
            CriticalityLevel::High
        );
        assert_eq!(
            CriticalityLevel::from_hint(Some("critical")),
            CriticalityLevel::Critical
        );
        assert_eq!(
            CriticalityLevel::from_hint(Some("invalid")),
            CriticalityLevel::Medium
        );
        assert_eq!(CriticalityLevel::from_hint(None), CriticalityLevel::Medium);
    }

    #[test]
    fn test_criticality_ordering() {
        assert!(CriticalityLevel::Low < CriticalityLevel::Medium);
        assert!(CriticalityLevel::Medium < CriticalityLevel::High);
        assert!(CriticalityLevel::High < CriticalityLevel::Critical);
    }

    #[test]
    fn test_criticality_human_confirmation() {
        assert!(!CriticalityLevel::Low.requires_human_confirmation());
        assert!(!CriticalityLevel::Medium.requires_human_confirmation());
        assert!(!CriticalityLevel::High.requires_human_confirmation());
        assert!(CriticalityLevel::Critical.requires_human_confirmation());
    }
}
