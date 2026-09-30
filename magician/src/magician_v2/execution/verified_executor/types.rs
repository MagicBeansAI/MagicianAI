//! Core types for runtime action candidates.

use serde::{Deserialize, Serialize};
use tool_runtime_core::{
    credential_preparation::CredentialSecretReference,
    credential_profiles::{CredentialProviderId, CredentialScope},
};

use crate::magician_v2::execution::actions::ExecutableAction;
use crate::magician_v2::secrets::credential_material_adapter::CredentialGrantRoute;

/// Move-only proof that the verified executor admitted one exact delegated credential
/// use for the dormant generic runtime.
///
/// It contains metadata bindings only—never a credential, grant token, or prepared
/// value—and has no generic debug or serialization surface. The credential adapter
/// consumes it before issuing the existing short-lived grant.
pub struct DelegatedCredentialAdmission {
    scope: CredentialScope,
    secret_ref: CredentialSecretReference,
    route: CredentialGrantRoute,
    provider: CredentialProviderId,
    agent_id: String,
}

impl DelegatedCredentialAdmission {
    #[allow(
        dead_code,
        reason = "Phase 5F3 admission remains dormant until governed production routing"
    )]
    pub(super) fn new(
        scope: CredentialScope,
        secret_ref: CredentialSecretReference,
        route: CredentialGrantRoute,
        provider: CredentialProviderId,
        agent_id: String,
    ) -> Self {
        Self {
            scope,
            secret_ref,
            route,
            provider,
            agent_id,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        CredentialScope,
        CredentialSecretReference,
        CredentialGrantRoute,
        CredentialProviderId,
        String,
    ) {
        (
            self.scope,
            self.secret_ref,
            self.route,
            self.provider,
            self.agent_id,
        )
    }
}

/// A candidate action from the LLM with runtime metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionCandidate {
    /// Position in ranked list (1 = highest priority).
    pub rank: usize,

    /// LLM confidence score (0.0-1.0).
    pub confidence: f64,

    /// Why this action might work, retained for history and debugging.
    pub reasoning: String,

    /// LLM-provided criticality level.
    pub criticality_hint: Option<String>,

    /// Whether the action cannot be safely retried.
    pub is_non_idempotent: Option<bool>,

    /// LLM-provided page context such as payment, auth, admin, or settings.
    pub page_context_hint: Option<String>,

    /// Whether the model explicitly requested human confirmation.
    pub requires_confirmation: Option<bool>,

    /// Goal context used by verification/feedback text.
    pub goal_context: Option<String>,

    /// The executable runtime action.
    pub action: ExecutableAction,

    /// Stable provisioned secret identifier for direct executor resolution.
    pub credential_id: Option<String>,

    /// Short-lived broker grant token for external/capability secret transport.
    #[serde(default, skip_serializing)]
    pub credential_token: Option<String>,

    /// Provider-issued tool-call id for this candidate (Responses/Anthropic
    /// `tool_use` id). Carried so the live conversation can pair the resulting
    /// tool result back to the exact assistant tool call. `None` for candidates
    /// not derived from a native tool call.
    pub tool_call_id: Option<String>,
}

impl ActionCandidate {
    /// Create a new ActionCandidate with minimal required fields.
    pub fn new(rank: usize, confidence: f64, action: ExecutableAction) -> Self {
        Self {
            rank,
            confidence,
            reasoning: String::new(),
            criticality_hint: None,
            is_non_idempotent: None,
            page_context_hint: None,
            requires_confirmation: None,
            goal_context: None,
            action,
            credential_id: None,
            credential_token: None,
            tool_call_id: None,
        }
    }

    /// Set the reasoning.
    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = reasoning.into();
        self
    }

    /// Set the criticality hint.
    pub fn with_criticality_hint(mut self, hint: impl Into<String>) -> Self {
        self.criticality_hint = Some(hint.into());
        self
    }

    /// Set the is_non_idempotent flag.
    pub fn with_non_idempotent(mut self, non_idempotent: bool) -> Self {
        self.is_non_idempotent = Some(non_idempotent);
        self
    }

    /// Set the page context hint.
    pub fn with_page_context(mut self, context: impl Into<String>) -> Self {
        self.page_context_hint = Some(context.into());
        self
    }

    /// Set the requires_confirmation flag.
    pub fn with_requires_confirmation(mut self, requires: bool) -> Self {
        self.requires_confirmation = Some(requires);
        self
    }

    /// Set the goal context.
    pub fn with_goal_context(mut self, goal: impl Into<String>) -> Self {
        self.goal_context = Some(goal.into());
        self
    }

    /// Set the provisioned secret identifier to resolve at execution time.
    pub fn with_credential_id(mut self, credential_id: impl Into<String>) -> Self {
        self.credential_id = Some(credential_id.into());
        self
    }

    /// Set the short-lived broker token to redeem at execution time.
    pub fn with_credential_token(mut self, credential_token: impl Into<String>) -> Self {
        self.credential_token = Some(credential_token.into());
        self
    }

    /// Set the provider-issued tool-call id this candidate was lowered from.
    pub fn with_tool_call_id(mut self, tool_call_id: impl Into<String>) -> Self {
        self.tool_call_id = Some(tool_call_id.into());
        self
    }

    /// Get the effective criticality hint.
    pub fn effective_criticality(&self) -> &str {
        self.criticality_hint
            .as_deref()
            .unwrap_or(super::constants::DEFAULT_CRITICALITY)
    }

    /// Check if this action is non-idempotent.
    pub fn is_non_idempotent(&self) -> bool {
        self.is_non_idempotent
            .unwrap_or(super::constants::DEFAULT_IS_NON_IDEMPOTENT)
    }

    /// Check if this action requires user confirmation.
    pub fn requires_confirmation(&self) -> bool {
        self.requires_confirmation.unwrap_or(false)
    }
}

/// Candidate envelope used by the agentic decision layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateBatch {
    /// Candidate actions in rank order.
    pub candidates: Vec<ActionCandidate>,

    /// LLM's thinking/reasoning for this decision.
    pub thinking: String,
}

impl CandidateBatch {
    /// Create a new CandidateBatch.
    pub fn new(candidates: Vec<ActionCandidate>, thinking: impl Into<String>) -> Self {
        Self {
            candidates,
            thinking: thinking.into(),
        }
    }

    /// Get the primary (rank 1) candidate.
    pub fn primary(&self) -> Option<&ActionCandidate> {
        self.candidates.iter().find(|c| c.rank == 1)
    }

    /// Get candidate by rank.
    pub fn by_rank(&self, rank: usize) -> Option<&ActionCandidate> {
        self.candidates.iter().find(|c| c.rank == rank)
    }

    /// Check if any candidate requires confirmation.
    pub fn any_requires_confirmation(&self) -> bool {
        self.candidates.iter().any(|c| c.requires_confirmation())
    }

    /// Get the maximum criticality across all candidates.
    pub fn max_criticality(&self) -> &str {
        for level in &["critical", "high", "medium", "low"] {
            if self
                .candidates
                .iter()
                .any(|c| c.effective_criticality() == *level)
            {
                return level;
            }
        }
        super::constants::DEFAULT_CRITICALITY
    }

    /// Filter candidates by minimum confidence.
    pub fn filter_by_confidence(&self, min_confidence: f64) -> Vec<&ActionCandidate> {
        self.candidates
            .iter()
            .filter(|c| c.confidence >= min_confidence)
            .collect()
    }

    /// Get the number of candidates.
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Check if the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::actions::FileAction;

    fn make_test_action() -> ExecutableAction {
        ExecutableAction::File(FileAction::Read {
            path: "README.md".into(),
            encoding: None,
        })
    }

    #[test]
    fn test_action_candidate_builder() {
        let candidate = ActionCandidate::new(1, 0.95, make_test_action())
            .with_reasoning("Primary submit button")
            .with_criticality_hint("high");

        assert_eq!(candidate.rank, 1);
        assert_eq!(candidate.confidence, 0.95);
        assert_eq!(candidate.effective_criticality(), "high");
        assert_eq!(candidate.reasoning, "Primary submit button");
    }

    #[test]
    fn test_action_candidate_defaults() {
        let candidate = ActionCandidate::new(1, 0.9, make_test_action());

        assert_eq!(candidate.effective_criticality(), "medium");
        assert!(!candidate.is_non_idempotent());
        assert!(!candidate.requires_confirmation());
    }

    #[test]
    fn test_candidate_batch() {
        let c1 = ActionCandidate::new(1, 0.9, make_test_action()).with_criticality_hint("high");
        let c2 = ActionCandidate::new(2, 0.7, make_test_action()).with_criticality_hint("medium");

        let batch = CandidateBatch::new(vec![c1, c2], "Testing batch");

        assert_eq!(batch.len(), 2);
        assert_eq!(batch.max_criticality(), "high");
        assert!(batch.primary().is_some());
        assert_eq!(batch.primary().unwrap().rank, 1);
    }

    #[test]
    fn credential_tokens_never_enter_a_serialized_candidate_batch() {
        let candidate = ActionCandidate::new(1, 0.9, make_test_action())
            .with_credential_id("provisioned-secret")
            .with_credential_token("ephemeral-broker-token");
        let encoded = serde_json::to_string(&CandidateBatch::new(vec![candidate], "secret test"))
            .expect("candidate batch serializes");

        assert!(encoded.contains("provisioned-secret"));
        assert!(!encoded.contains("ephemeral-broker-token"));
        let restored: CandidateBatch =
            serde_json::from_str(&encoded).expect("candidate batch deserializes");
        assert_eq!(restored.candidates[0].credential_token, None);
    }
}
