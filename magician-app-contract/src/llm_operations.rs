//! Additive app-declared LLM operation vocabulary (plan 1.4).
//!
//! A package may declare named LLM operations so its prompts can route like
//! core lanes do — through the operator-owned `llm.router.operation_mapping`
//! under the server-side `app:` namespace, inside
//! `app_platform.processing` trust policy. A declaration is review material,
//! never authority: the manifest states a bounded purpose and optional
//! budget hint, and the server policy must independently admit the name
//! before anything routes. Manifests that declare no operations resolve
//! exactly as before; nothing existing changes for them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Maximum purpose text carried by one declared app LLM operation. Mirrors
/// the contribution header's purpose ceiling so owner review reads one
/// consistent bound across every app-declared purpose surface.
pub const APP_LLM_OPERATION_MAX_PURPOSE_BYTES: usize = 192;

/// One package-declared, named LLM operation (`app.llm_operations.<name>`).
///
/// The name is the routing vocabulary; the body is what an owner reviews.
/// Server admission re-derives everything from live policy, so no field
/// here can widen a profile, budget, or locality decision by itself.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestLlmOperation {
    /// Bounded statement of what this operation is for. Surfaced at
    /// installation review next to the operator's acknowledged purpose for
    /// the admitted name.
    pub purpose: String,
    /// Optional per-operation token budget hint. Packages may only narrow
    /// resource ceilings, never widen them; absent means the package and
    /// server resource ceilings apply unchanged.
    ///
    /// The app dispatcher applies `min(operator ceiling, this hint, selected
    /// profile ceiling)` on every live resolution. An operator admission
    /// without an explicit positive ceiling remains parse-compatible but is
    /// deliberately non-executable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_operation_declarations_are_closed_and_additive() {
        let operation: AppManifestLlmOperation = serde_json::from_value(serde_json::json!({
            "purpose": "Summarize one admitted record for the owner.",
            "max_tokens": 2_048
        }))
        .unwrap();
        assert_eq!(operation.max_tokens, Some(2_048));
        assert_eq!(
            operation.purpose,
            "Summarize one admitted record for the owner."
        );

        let without_hint: AppManifestLlmOperation = serde_json::from_value(serde_json::json!({
            "purpose": "Distill a record."
        }))
        .unwrap();
        assert_eq!(without_hint.max_tokens, None);

        let rejected = serde_json::from_value::<AppManifestLlmOperation>(serde_json::json!({
            "purpose": "p",
            "max_tokens": 1,
            "profile": "op-app-workflow-local"
        }));
        assert!(rejected.is_err(), "unknown fields must fail closed");
    }
}
