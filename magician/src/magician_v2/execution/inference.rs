//! Observation-based inference for resolving unresolved inputs.
//!
//! This module provides logic to infer input values from page observations (vision analysis)
//! before triggering JIT clarification. For example, if vision detects a "Login" page,
//! we can infer that session-state inputs should be `false`.

use serde_json::{json, Value};
use tracing::{debug, info};

use super::types::PageStage;

/// Result of attempting to infer a value from observations
#[derive(Debug, Clone)]
pub struct InferenceResult {
    /// The inferred value, if successful
    pub value: Value,
    /// Confidence in the inference (0.0 - 1.0)
    pub confidence: f32,
    /// Human-readable reason for the inference
    pub reason: String,
}

/// Attempt to infer a value for a parameter based on the current page stage.
///
/// This uses pattern matching on common parameter names to derive values from
/// the observed page state. For example:
/// - Login page + "authorized" parameter → false (user still needs to sign in)
/// - Dashboard page + "logged_in" parameter → true (user is logged in)
///
/// Returns `Some(InferenceResult)` if inference is possible, `None` otherwise.
pub fn infer_from_page_stage(
    page_stage: &PageStage,
    parameter_name: &str,
    confidence_threshold: f32,
) -> Option<InferenceResult> {
    let param_lower = parameter_name.to_lowercase();

    // Pattern matching for authentication/session-state parameters
    let is_session_state_param = param_lower.contains("authorized")
        || param_lower.contains("logged_in")
        || param_lower.contains("authenticated")
        || param_lower.contains("signed_in")
        || param_lower.contains("auth_status");

    // Pattern matching for availability/working state
    let is_availability_param = param_lower.contains("available")
        || param_lower.contains("working")
        || param_lower.contains("accessible")
        || param_lower.contains("online");

    // Pattern matching for error state
    let is_error_param = param_lower.contains("error")
        || param_lower.contains("failed")
        || param_lower.contains("has_error");

    // Infer based on page stage and parameter type
    let result = match page_stage {
        PageStage::Login => {
            if is_session_state_param {
                Some(InferenceResult {
                    value: json!(false),
                    confidence: 0.95,
                    reason: format!(
                        "Page is at Login stage, inferring '{}' = false (not signed in yet)",
                        parameter_name
                    ),
                })
            } else {
                None
            }
        },

        PageStage::Dashboard | PageStage::Content | PageStage::Search | PageStage::Results => {
            if is_session_state_param {
                Some(InferenceResult {
                    value: json!(true),
                    confidence: 0.95,
                    reason: format!(
                        "Page is at {:?} stage, inferring '{}' = true (signed-in state detected)",
                        page_stage, parameter_name
                    ),
                })
            } else if is_availability_param {
                Some(InferenceResult {
                    value: json!(true),
                    confidence: 0.90,
                    reason: format!(
                        "Page is at {:?} stage, inferring '{}' = true (service is available)",
                        page_stage, parameter_name
                    ),
                })
            } else {
                None
            }
        },

        PageStage::Error => {
            if is_availability_param {
                Some(InferenceResult {
                    value: json!(false),
                    confidence: 0.90,
                    reason: format!(
                        "Page is at Error stage, inferring '{}' = false (service may be unavailable)",
                        parameter_name
                    ),
                })
            } else if is_error_param {
                Some(InferenceResult {
                    value: json!(true),
                    confidence: 0.95,
                    reason: format!(
                        "Page is at Error stage, inferring '{}' = true (error detected)",
                        parameter_name
                    ),
                })
            } else {
                None
            }
        },

        PageStage::Modal => {
            // Modal often indicates intermediate state (auth dialogs, confirmations)
            if is_session_state_param {
                // Could be auth modal - be more cautious
                Some(InferenceResult {
                    value: json!(false),
                    confidence: 0.70,
                    reason: format!(
                        "Page has Modal stage, tentatively inferring '{}' = false (may be sign-in dialog)",
                        parameter_name
                    ),
                })
            } else {
                None
            }
        },

        PageStage::Form => {
            // Form could be login form or other input form
            if is_session_state_param && param_lower.contains("login") {
                Some(InferenceResult {
                    value: json!(false),
                    confidence: 0.80,
                    reason: format!(
                        "Page is at Form stage, inferring '{}' = false (likely login form)",
                        parameter_name
                    ),
                })
            } else {
                None
            }
        },

        PageStage::Loading => {
            // Don't infer during loading - wait for page to stabilize
            debug!(
                "Page is still loading, skipping inference for '{}'",
                parameter_name
            );
            None
        },

        PageStage::Unknown => {
            // Can't infer from unknown page state
            debug!(
                "Page stage is Unknown, skipping inference for '{}'",
                parameter_name
            );
            None
        },
    };

    // Apply confidence threshold
    if let Some(ref r) = result {
        if r.confidence < confidence_threshold {
            debug!(
                "Inference confidence {} below threshold {} for '{}', skipping",
                r.confidence, confidence_threshold, parameter_name
            );
            return None;
        }
        info!("[INFERENCE] {} (confidence: {:.2})", r.reason, r.confidence);
    }

    result
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_login_page_infers_not_authorized() {
        let result = infer_from_page_stage(&PageStage::Login, "is_authorized", 0.5);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.value, json!(false));
        assert!(r.confidence >= 0.9);
    }

    #[test]
    fn test_dashboard_page_infers_authorized() {
        let result = infer_from_page_stage(&PageStage::Dashboard, "logged_in", 0.5);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.value, json!(true));
        assert!(r.confidence >= 0.9);
    }

    #[test]
    fn test_error_page_infers_unavailable() {
        let result = infer_from_page_stage(&PageStage::Error, "service_available", 0.5);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.value, json!(false));
    }

    #[test]
    fn test_loading_page_no_inference() {
        let result = infer_from_page_stage(&PageStage::Loading, "is_authorized", 0.5);
        assert!(result.is_none());
    }

    #[test]
    fn test_unknown_param_no_inference() {
        let result = infer_from_page_stage(&PageStage::Login, "random_field", 0.5);
        assert!(result.is_none());
    }

    #[test]
    fn test_confidence_threshold() {
        // Modal auth has 0.70 confidence
        let result = infer_from_page_stage(&PageStage::Modal, "is_authorized", 0.8);
        assert!(result.is_none()); // Below threshold

        let result = infer_from_page_stage(&PageStage::Modal, "is_authorized", 0.6);
        assert!(result.is_some()); // Above threshold
    }
}
