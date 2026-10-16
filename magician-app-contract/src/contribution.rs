//! Destination-owned contribution contracts.
//!
//! Apps may propose bounded, source-linked material. They never own memory,
//! retrieval, attention, claims, or commitment state, destination ranking, or
//! an owner's decision. The common header carries only source/provenance
//! identity; every destination retains a distinct closed body so this module
//! cannot become a universal untyped contribution channel.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const APP_CONTRIBUTION_CONTRACT_VERSION: u16 = 1;
pub const APP_MEMORY_CANDIDATE_CONTRACT_ID: &str = "magician.memory.candidate";
pub const APP_RETRIEVAL_PROJECTION_CONTRACT_ID: &str =
    "magician.personal-agent.retrieval-projection";
pub const APP_ATTENTION_CANDIDATE_CONTRACT_ID: &str = "magician.attention.candidate";
pub const APP_LEARNING_DECISION_CONTRACT_ID: &str = "magician.learning-decision";
pub const APP_CLAIMS_DECISION_CONTRACT_ID: &str = "magician.claims-decision";
pub const APP_CLAIMS_DECISION_SOURCE_ENTITY: &str = "review_decision";
pub const APP_MEETING_CONTROL_CONTRACT_ID: &str = "magician.meeting-control";
pub const APP_MEETING_CONTROL_SOURCE_ENTITY: &str = "control_request";
pub const APP_MEETING_CONTROL_DESTINATION_SCHEMA_SEED: &[u8] =
    b"magician.meeting-control.destination-schema.v1";
pub const APP_CLAIMS_DECISION_DESTINATION_SCHEMA_SEED: &[u8] =
    b"magician.claims-decision.destination-schema.v1";
pub const APP_CONTRIBUTION_MAX_DOCUMENT_BYTES: usize = 256 * 1024;
pub const APP_CONTRIBUTION_MAX_INGRESS_RECEIPT_BYTES: usize = 64 * 1024;
pub const APP_CONTRIBUTION_MAX_INVALIDATION_BYTES: usize = 64 * 1024;
/// A destination receipt may contain one maximum-sized sealed proposal plus
/// a bounded receipt envelope. It is deliberately distinct from the small
/// source acknowledgement and invalidation ceilings.
pub const APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES: usize =
    APP_CONTRIBUTION_MAX_DOCUMENT_BYTES + 64 * 1024;
pub const APP_CONTRIBUTION_MAX_CLAIM_BYTES: usize = 32 * 1024;
pub const APP_CONTRIBUTION_MAX_SOURCES: usize = 32;
pub const APP_CONTRIBUTION_MAX_SELECTED_FIELDS: usize = 64;
pub const APP_CONTRIBUTION_MAX_AUDIENCES: usize = 16;
pub const APP_CONTRIBUTION_MAX_EVIDENCE_REFS: usize = 32;
pub const APP_CONTRIBUTION_MAX_TTL_MS: i64 = 90 * 24 * 60 * 60 * 1_000;
pub const APP_MEMORY_OWNER_REVIEW_MAX_ITEMS: usize = 16;
pub const APP_MEMORY_OWNER_REVIEW_MAX_BYTES: usize =
    APP_CONTRIBUTION_MAX_DOCUMENT_BYTES + 32 * 1024;
pub const APP_MEMORY_OWNER_DECISION_MAX_BYTES: usize = APP_MEMORY_OWNER_REVIEW_MAX_BYTES + 4 * 1024;
pub const APP_MEMORY_OWNER_REVIEW_LIST_MAX_BYTES: usize = 320 * 1024;
pub const APP_ATTENTION_MAX_TITLE_BYTES: usize = 256;
pub const APP_ATTENTION_MAX_SUMMARY_BYTES: usize = 4 * 1024;
/// Attention owner reviews embed one maximum-sized sealed proposal plus the
/// same bounded review envelope shape as the memory port.
pub const APP_ATTENTION_OWNER_REVIEW_MAX_BYTES: usize =
    APP_CONTRIBUTION_MAX_DOCUMENT_BYTES + 32 * 1024;
pub const APP_ATTENTION_OWNER_DECISION_MAX_BYTES: usize =
    APP_ATTENTION_OWNER_REVIEW_MAX_BYTES + 4 * 1024;
pub const APP_ATTENTION_OWNER_REVIEW_MAX_ITEMS: usize = 16;
pub const APP_ATTENTION_OWNER_REVIEW_LIST_MAX_BYTES: usize = 320 * 1024;
pub const APP_LEARNING_MAX_REASON_BYTES: usize = 4 * 1024;
/// Learning owner reviews embed one maximum-sized sealed proposal plus the
/// same bounded review envelope shape as the memory and attention ports.
pub const APP_LEARNING_OWNER_REVIEW_MAX_BYTES: usize =
    APP_CONTRIBUTION_MAX_DOCUMENT_BYTES + 32 * 1024;
pub const APP_LEARNING_OWNER_DECISION_MAX_BYTES: usize =
    APP_LEARNING_OWNER_REVIEW_MAX_BYTES + 4 * 1024;
pub const APP_CLAIMS_DECISION_MAX_NOTE_BYTES: usize = 4 * 1024;
pub const APP_CLAIMS_DECISION_OWNER_REVIEW_MAX_BYTES: usize =
    APP_CONTRIBUTION_MAX_DOCUMENT_BYTES + 32 * 1024;
pub const APP_CLAIMS_DECISION_OWNER_DECISION_MAX_BYTES: usize =
    APP_CLAIMS_DECISION_OWNER_REVIEW_MAX_BYTES + 4 * 1024;
pub const APP_MEETING_CONTROL_MAX_NOTE_BYTES: usize = 1024;
pub const APP_MEETING_CONTROL_MAX_URL_BYTES: usize = 2 * 1024;
pub const APP_MEETING_CONTROL_MAX_TITLE_BYTES: usize = 256;
/// Longest window a surface gesture may be trusted as "the owner just asked
/// for this". Authority reuse is not intent: an app that already holds the
/// control grant still cannot start an hours-long capture on a stale gesture.
pub const APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS: i64 = 120_000;
/// Tolerance for desktop-versus-host clock skew when judging a signature's age.
pub const APP_MEETING_CONTROL_MAX_CLOCK_SKEW_MS: i64 = 30_000;
pub const APP_MEETING_CONTROL_OWNER_REVIEW_MAX_BYTES: usize =
    APP_CONTRIBUTION_MAX_DOCUMENT_BYTES + 32 * 1024;
pub const APP_MEETING_CONTROL_OWNER_DECISION_MAX_BYTES: usize =
    APP_MEETING_CONTROL_OWNER_REVIEW_MAX_BYTES + 4 * 1024;

const DIGEST_PREFIX: &str = "blake3:";
const MAX_IDENTITY_BYTES: usize = 192;
const MAX_FIELD_BYTES: usize = 512;
const MAX_PURPOSE_BYTES: usize = 192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppContributionContractError {
    InvalidField(&'static str),
    DigestMismatch(&'static str),
    DocumentTooLarge,
    Encoding,
    InvalidSignature,
}

impl std::fmt::Display for AppContributionContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidField(field) => write!(formatter, "invalid contribution field `{field}`"),
            Self::DigestMismatch(field) => {
                write!(formatter, "contribution digest mismatch in `{field}`")
            },
            Self::DocumentTooLarge => {
                formatter.write_str("contribution document exceeds its byte ceiling")
            },
            Self::Encoding => formatter.write_str("contribution encoding failed"),
            Self::InvalidSignature => {
                formatter.write_str("contribution owner signature is invalid")
            },
        }
    }
}

impl std::error::Error for AppContributionContractError {}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum AppContributionClassification {
    Public,
    Internal,
    Personal,
    Sensitive,
    Secret,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum AppContributionModelProcessing {
    None,
    LocalOnly,
    RemoteAllowed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppContributionHandlingLabelsV1 {
    pub classification: AppContributionClassification,
    pub model_processing: AppContributionModelProcessing,
    pub policy_digest: String,
    pub provenance_digest: String,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum AppContributionEvidenceClass {
    Authoritative,
    Derived,
    Hypothesis,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppContributionUpdatePolicy {
    NewProposalRevision,
    ReplaceExactSourceHead,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppContributionRetractionPolicy {
    TombstoneOnAnySourceDrift,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppContributionSourceRefV1 {
    pub installation_id: String,
    pub entity_name: String,
    pub record_id: String,
    pub record_revision: u64,
    pub selected_fields: Vec<String>,
    pub canonical_source_ref: String,
    pub canonical_source_digest: String,
    pub handling_labels: AppContributionHandlingLabelsV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppContributionSettlementRefV1 {
    Mutation {
        mutation_receipt_id: String,
        first_change_sequence: u64,
        last_change_sequence: u64,
    },
    TypedResult {
        result_ref: String,
        output_revision: u64,
        result_digest: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppContributionSourceHeaderV1 {
    pub contract_version: u16,
    pub destination_contract_id: String,
    pub destination_contract_version: u16,
    pub destination_schema_digest: String,
    pub proposal_id: String,
    pub proposal_revision: u64,
    pub scope_binding_ref: String,
    pub installation_id: String,
    pub installation_generation: u64,
    pub package_revision_ref: String,
    pub package_content_digest: String,
    pub grant_revision: u64,
    pub grant_authority_digest: String,
    pub schema_revision: u64,
    pub schema_digest: String,
    pub workflow_id: String,
    pub workflow_digest: String,
    pub action_id: String,
    pub action_digest: String,
    pub contribution_port_id: String,
    pub contribution_port_digest: String,
    pub settlement: AppContributionSettlementRefV1,
    pub sources: Vec<AppContributionSourceRefV1>,
    pub handling_labels: AppContributionHandlingLabelsV1,
    pub purpose: String,
    pub audiences: Vec<String>,
    pub evidence_class: AppContributionEvidenceClass,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub dedupe_key: String,
    pub update_policy: AppContributionUpdatePolicy,
    pub retraction_policy: AppContributionRetractionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppMemoryTierScopeV1 {
    User,
    Agent { agent_id: String },
    AgentGoal { agent_id: String, goal_id: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemorySemanticDestinationV1 {
    TaskProgress,
    Entities,
    Knowledge,
    Archive,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryCandidateProposalV1 {
    pub header: AppContributionSourceHeaderV1,
    pub intended_tier_scope: AppMemoryTierScopeV1,
    pub semantic_destination: AppMemorySemanticDestinationV1,
    pub claim_or_summary: String,
    pub claim_digest: String,
    pub evidence_refs: Vec<String>,
    pub proposal_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPersonalAgentRetrievalProjectionProposalV1 {
    pub header: AppContributionSourceHeaderV1,
    pub target_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_goal_id: Option<String>,
    pub projection_text: String,
    pub projection_digest: String,
    pub proposal_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryIngressDispositionV1 {
    Staged,
    Duplicate,
    PolicyRejected,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryIngressReceiptV1 {
    pub contract_version: u16,
    pub receipt_id: String,
    pub proposal_id: String,
    pub proposal_digest: String,
    pub destination_generation: u64,
    pub destination_receipt_digest: String,
    pub disposition: AppMemoryIngressDispositionV1,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryOwnerDecisionV1 {
    Accept,
    Reject,
    Revoke,
}

/// Exact destination snapshot shown by the trusted desktop before it signs an
/// owner decision. The complete proposal is embedded so a UI cannot replace
/// sensitive text, source identity, handling labels, or retention context
/// while retaining the same friendly proposal id.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryOwnerReviewV1 {
    pub contract_version: u16,
    pub review_id: String,
    pub destination_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_receipt_digest: Option<String>,
    pub desktop_identity_key_id: String,
    pub desktop_identity_digest: String,
    pub proposal: AppMemoryCandidateProposalV1,
    pub display_digest: String,
}

/// Keychain-signed, destination-head-bound owner decision. This is a closed
/// transport document, not generic signing authority. The destination stores
/// only `receipt_digest` in its projection receipt, while the caller may replay
/// this exact envelope after response loss.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryOwnerDecisionEnvelopeV1 {
    pub contract_version: u16,
    pub decision_id: String,
    pub review: AppMemoryOwnerReviewV1,
    pub decision: AppMemoryOwnerDecisionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_until_ms: Option<i64>,
    pub desktop_identity_signature_hex: String,
    pub receipt_digest: String,
}

impl AppMemoryOwnerReviewV1 {
    pub fn mint(
        destination_generation: u64,
        destination_receipt_digest: Option<String>,
        desktop_identity_key_id: String,
        desktop_identity_digest: String,
        proposal: AppMemoryCandidateProposalV1,
    ) -> Result<Self, AppContributionContractError> {
        let mut review = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            review_id: String::new(),
            destination_generation,
            destination_receipt_digest,
            desktop_identity_key_id,
            desktop_identity_digest,
            proposal,
            display_digest: String::new(),
        };
        review.review_id = domain_digest(
            "magician.memory-owner-review-id.v1",
            &(
                &review.proposal.header.scope_binding_ref,
                &review.proposal.header.proposal_id,
                &review.proposal.proposal_digest,
                review.destination_generation,
                &review.destination_receipt_digest,
                &review.desktop_identity_digest,
            ),
        )?;
        review.display_digest = review.expected_display_digest()?;
        review.validate()?;
        Ok(review)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_review.contract_version",
            ));
        }
        self.proposal.validate()?;
        validate_digest("owner_review.review_id", &self.review_id)?;
        validate_token(
            "owner_review.desktop_identity_key_id",
            &self.desktop_identity_key_id,
            MAX_IDENTITY_BYTES,
        )?;
        validate_digest(
            "owner_review.desktop_identity_digest",
            &self.desktop_identity_digest,
        )?;
        match (
            self.destination_generation,
            self.destination_receipt_digest.as_deref(),
        ) {
            (0, None) => {},
            (0, Some(_)) | (_, None) => {
                return Err(AppContributionContractError::InvalidField(
                    "owner_review.destination_receipt_digest",
                ));
            },
            (_, Some(digest)) => {
                validate_digest("owner_review.destination_receipt_digest", digest)?
            },
        }
        let expected_review_id = domain_digest(
            "magician.memory-owner-review-id.v1",
            &(
                &self.proposal.header.scope_binding_ref,
                &self.proposal.header.proposal_id,
                &self.proposal.proposal_digest,
                self.destination_generation,
                &self.destination_receipt_digest,
                &self.desktop_identity_digest,
            ),
        )?;
        if self.review_id != expected_review_id {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.review_id",
            ));
        }
        validate_digest("owner_review.display_digest", &self.display_digest)?;
        if self.display_digest != self.expected_display_digest()? {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.display_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_MEMORY_OWNER_REVIEW_MAX_BYTES)
    }

    fn expected_display_digest(&self) -> Result<String, AppContributionContractError> {
        let mut unsigned = self.clone();
        unsigned.display_digest.clear();
        domain_digest("magician.memory-owner-review-display.v1", &unsigned)
    }
}

impl AppMemoryOwnerDecisionEnvelopeV1 {
    pub fn prepare(
        review: AppMemoryOwnerReviewV1,
        decision: AppMemoryOwnerDecisionV1,
        retained_until_ms: Option<i64>,
    ) -> Result<Self, AppContributionContractError> {
        review.validate()?;
        let mut envelope = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            decision_id: String::new(),
            review,
            decision,
            retained_until_ms,
            desktop_identity_signature_hex: String::new(),
            receipt_digest: String::new(),
        };
        envelope.decision_id = domain_digest(
            "magician.memory-owner-decision-id.v1",
            &(
                &envelope.review.review_id,
                envelope.decision,
                envelope.retained_until_ms,
                &envelope.review.desktop_identity_digest,
            ),
        )?;
        envelope.validate_unsigned()?;
        Ok(envelope)
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppContributionContractError> {
        self.validate_unsigned()?;
        let mut unsigned = self.clone();
        unsigned.desktop_identity_signature_hex.clear();
        unsigned.receipt_digest.clear();
        serde_json::to_vec(&("magician.memory-owner-decision-signature.v1", unsigned))
            .map_err(|_| AppContributionContractError::Encoding)
    }

    pub fn with_signature_hex(
        mut self,
        signature_hex: String,
    ) -> Result<Self, AppContributionContractError> {
        self.desktop_identity_signature_hex = signature_hex;
        self.receipt_digest.clear();
        self.receipt_digest = domain_digest("magician.memory-owner-decision-receipt.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        self.validate_unsigned()?;
        validate_lower_hex(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
            64,
        )?;
        validate_digest("owner_decision.receipt_digest", &self.receipt_digest)?;
        let mut unsigned_receipt = self.clone();
        unsigned_receipt.receipt_digest.clear();
        if self.receipt_digest
            != domain_digest(
                "magician.memory-owner-decision-receipt.v1",
                &unsigned_receipt,
            )?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.receipt_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_MEMORY_OWNER_DECISION_MAX_BYTES)
    }

    pub fn verify_signature(
        &self,
        desktop_identity_public_key_hex: &str,
    ) -> Result<(), AppContributionContractError> {
        self.validate()?;
        let public_key = decode_lower_hex::<32>(
            "owner_decision.desktop_identity_public_key_hex",
            desktop_identity_public_key_hex,
        )?;
        let signature = decode_lower_hex::<64>(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
        )?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
            .verify(&self.signing_bytes()?, &signature)
            .map_err(|_| AppContributionContractError::InvalidSignature)
    }

    fn validate_unsigned(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.contract_version",
            ));
        }
        self.review.validate()?;
        validate_digest("owner_decision.decision_id", &self.decision_id)?;
        let expected_decision_id = domain_digest(
            "magician.memory-owner-decision-id.v1",
            &(
                &self.review.review_id,
                self.decision,
                self.retained_until_ms,
                &self.review.desktop_identity_digest,
            ),
        )?;
        if self.decision_id != expected_decision_id {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.decision_id",
            ));
        }
        let retention_is_valid = match (self.decision, self.retained_until_ms) {
            (AppMemoryOwnerDecisionV1::Reject | AppMemoryOwnerDecisionV1::Revoke, None) => true,
            (AppMemoryOwnerDecisionV1::Accept, Some(expiry)) => {
                expiry > self.review.proposal.header.issued_at_ms
                    && expiry <= self.review.proposal.header.expires_at_ms
            },
            _ => false,
        };
        if !retention_is_valid {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.retained_until_ms",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryInvalidationReasonV1 {
    SourceUpdated,
    SourceDeleted,
    SourceRestored,
    SourceForgotten,
    PolicyChanged,
    GrantRevoked,
    InstallationDisabled,
    InstallationQuarantined,
    InstallationUninstalledRetained,
    InstallationPurged,
    ContributionExpired,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryInvalidationV1 {
    pub contract_version: u16,
    pub invalidation_id: String,
    pub installation_id: String,
    pub scope_binding_ref: String,
    pub proposal_id: String,
    pub proposal_digest: String,
    pub source_event_ref: String,
    pub source_event_revision: u64,
    pub source_identity_digest: String,
    pub dedupe_key: String,
    pub reason: AppMemoryInvalidationReasonV1,
    pub issued_at_ms: i64,
    pub invalidation_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppMemoryDestinationOperationV1 {
    StageProposal {
        proposal: AppMemoryCandidateProposalV1,
    },
    Decide {
        proposal_id: String,
        proposal_digest: String,
        decision: AppMemoryOwnerDecisionV1,
        owner_decision_receipt_digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained_until_ms: Option<i64>,
    },
    Invalidate {
        invalidation: AppMemoryInvalidationV1,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryDestinationReceiptV1 {
    pub contract_version: u16,
    pub receipt_id: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_receipt_digest: Option<String>,
    pub operation: AppMemoryDestinationOperationV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation_disposition: Option<AppMemoryInvalidationDispositionV1>,
    pub resulting_projection_digest: String,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryInvalidationDispositionV1 {
    Tombstoned,
    AlreadyTombstoned,
    SupersededBeforeAdmission,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryInvalidationReceiptV1 {
    pub contract_version: u16,
    pub receipt_id: String,
    pub invalidation_id: String,
    pub invalidation_digest: String,
    pub proposal_id: String,
    pub destination_generation: u64,
    pub destination_receipt_digest: String,
    pub disposition: AppMemoryInvalidationDispositionV1,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

impl AppMemoryCandidateProposalV1 {
    pub fn seal(mut self) -> Result<Self, AppContributionContractError> {
        self.proposal_digest.clear();
        self.proposal_digest = domain_digest("magician.memory-candidate-proposal.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        validate_header(&self.header, APP_MEMORY_CANDIDATE_CONTRACT_ID)?;
        validate_nonempty_text(
            "claim_or_summary",
            &self.claim_or_summary,
            APP_CONTRIBUTION_MAX_CLAIM_BYTES,
        )?;
        validate_digest("claim_digest", &self.claim_digest)?;
        if self.claim_digest != content_digest(self.claim_or_summary.as_bytes()) {
            return Err(AppContributionContractError::DigestMismatch("claim_digest"));
        }
        validate_sorted_tokens(
            "evidence_refs",
            &self.evidence_refs,
            APP_CONTRIBUTION_MAX_EVIDENCE_REFS,
            MAX_IDENTITY_BYTES,
        )?;
        if self.evidence_refs.is_empty()
            || self.evidence_refs.iter().any(|evidence_ref| {
                !self
                    .header
                    .sources
                    .iter()
                    .any(|source| source.canonical_source_ref == *evidence_ref)
            })
        {
            return Err(AppContributionContractError::InvalidField("evidence_refs"));
        }
        validate_tier_scope(&self.intended_tier_scope)?;
        validate_digest("proposal_digest", &self.proposal_digest)?;
        let mut unsigned = self.clone();
        unsigned.proposal_digest.clear();
        if self.proposal_digest
            != domain_digest("magician.memory-candidate-proposal.v1", &unsigned)?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "proposal_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_CONTRIBUTION_MAX_DOCUMENT_BYTES)
    }
}

impl AppPersonalAgentRetrievalProjectionProposalV1 {
    pub fn seal(mut self) -> Result<Self, AppContributionContractError> {
        self.proposal_digest.clear();
        self.proposal_digest = domain_digest("magician.retrieval-projection-proposal.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        validate_header(&self.header, APP_RETRIEVAL_PROJECTION_CONTRACT_ID)?;
        validate_token("target_agent_id", &self.target_agent_id, MAX_IDENTITY_BYTES)?;
        if let Some(goal) = self.target_goal_id.as_deref() {
            validate_token("target_goal_id", goal, MAX_IDENTITY_BYTES)?;
        }
        validate_nonempty_text(
            "projection_text",
            &self.projection_text,
            APP_CONTRIBUTION_MAX_CLAIM_BYTES,
        )?;
        validate_digest("projection_digest", &self.projection_digest)?;
        if self.projection_digest != content_digest(self.projection_text.as_bytes()) {
            return Err(AppContributionContractError::DigestMismatch(
                "projection_digest",
            ));
        }
        validate_digest("proposal_digest", &self.proposal_digest)?;
        let mut unsigned = self.clone();
        unsigned.proposal_digest.clear();
        if self.proposal_digest
            != domain_digest("magician.retrieval-projection-proposal.v1", &unsigned)?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "proposal_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_CONTRIBUTION_MAX_DOCUMENT_BYTES)
    }
}

macro_rules! impl_sealed_receipt {
    ($type:ty, $field:ident, $domain:literal, $validate_body:ident, $ceiling:expr) => {
        impl $type {
            pub fn seal(mut self) -> Result<Self, AppContributionContractError> {
                self.$field.clear();
                self.$field = domain_digest($domain, &self)?;
                self.validate()?;
                Ok(self)
            }

            pub fn validate(&self) -> Result<(), AppContributionContractError> {
                $validate_body(self)?;
                validate_digest(stringify!($field), &self.$field)?;
                let mut unsigned = self.clone();
                unsigned.$field.clear();
                if self.$field != domain_digest($domain, &unsigned)? {
                    return Err(AppContributionContractError::DigestMismatch(stringify!(
                        $field
                    )));
                }
                validate_encoded_ceiling(self, $ceiling)
            }
        }
    };
}

impl_sealed_receipt!(
    AppMemoryIngressReceiptV1,
    receipt_digest,
    "magician.memory-ingress-receipt.v1",
    validate_ingress_receipt,
    APP_CONTRIBUTION_MAX_INGRESS_RECEIPT_BYTES
);
impl_sealed_receipt!(
    AppMemoryInvalidationV1,
    invalidation_digest,
    "magician.memory-invalidation.v1",
    validate_invalidation,
    APP_CONTRIBUTION_MAX_INVALIDATION_BYTES
);
impl_sealed_receipt!(
    AppMemoryDestinationReceiptV1,
    receipt_digest,
    "magician.memory-destination-receipt.v1",
    validate_destination_receipt,
    APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES
);
impl_sealed_receipt!(
    AppMemoryInvalidationReceiptV1,
    receipt_digest,
    "magician.memory-invalidation-receipt.v1",
    validate_invalidation_receipt,
    APP_CONTRIBUTION_MAX_INGRESS_RECEIPT_BYTES
);

/// Closed destination-lane vocabulary for V1 attention candidates.
///
/// Wire values deliberately match the core attention lane vocabulary
/// (`AttentionLane` in `magician-core/src/attention_funnel.rs`) so the
/// destination owner maps a proposed lane without a translation table that
/// can drift. This crate keeps its own copy so the public contract stays
/// dependency-free. A proposal names the lane its card is offered to;
/// admission and final placement stay destination-owned.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAttentionLaneV1 {
    NeedsYou,
    FollowUp,
    WorthALook,
    ActiveWork,
    Delivered,
    Changed,
    Failed,
}

impl AppAttentionLaneV1 {
    /// Canonical wire string shared with the core lane vocabulary.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs_you",
            Self::FollowUp => "follow_up",
            Self::WorthALook => "worth_a_look",
            Self::ActiveWork => "active_work",
            Self::Delivered => "delivered",
            Self::Changed => "changed",
            Self::Failed => "failed",
        }
    }
}

/// Closed urgency claim carried by an attention candidate.
///
/// This is card content an owner interprets at admission — never ordering
/// authority. The destination may map it, cap it, or ignore it, and
/// free-form numeric ranking stays out of the contract on purpose.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum AppAttentionPriorityV1 {
    Background,
    Normal,
    Elevated,
    Urgent,
}

impl AppAttentionPriorityV1 {
    /// Canonical wire string for the closed urgency vocabulary.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Normal => "normal",
            Self::Elevated => "elevated",
            Self::Urgent => "urgent",
        }
    }
}

/// Bounded, source-linked lane-card proposal from one app installation.
///
/// The body carries only what an owner can review: a proposed lane from the
/// closed vocabulary, a closed urgency claim, bounded card text, and the one
/// declared source the card speaks for. The header's `dedupe_key` is the
/// idempotency key, exactly as for memory candidates; apps never own
/// attention state, they propose and the owner decides.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAttentionCandidateProposalV1 {
    pub header: AppContributionSourceHeaderV1,
    pub lane: AppAttentionLaneV1,
    pub priority: AppAttentionPriorityV1,
    pub title: String,
    pub title_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub primary_source_ref: String,
    pub proposal_digest: String,
}

impl AppAttentionCandidateProposalV1 {
    pub fn seal(mut self) -> Result<Self, AppContributionContractError> {
        self.proposal_digest.clear();
        self.proposal_digest = domain_digest("magician.attention-candidate-proposal.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        validate_header(&self.header, APP_ATTENTION_CANDIDATE_CONTRACT_ID)?;
        validate_nonempty_text("title", &self.title, APP_ATTENTION_MAX_TITLE_BYTES)?;
        validate_digest("title_digest", &self.title_digest)?;
        if self.title_digest != content_digest(self.title.as_bytes()) {
            return Err(AppContributionContractError::DigestMismatch("title_digest"));
        }
        if let Some(summary) = self.summary.as_deref() {
            validate_nonempty_text("summary", summary, APP_ATTENTION_MAX_SUMMARY_BYTES)?;
        }
        validate_token(
            "primary_source_ref",
            &self.primary_source_ref,
            MAX_IDENTITY_BYTES,
        )?;
        if !self
            .header
            .sources
            .iter()
            .any(|source| source.canonical_source_ref == self.primary_source_ref)
        {
            return Err(AppContributionContractError::InvalidField(
                "primary_source_ref",
            ));
        }
        validate_digest("proposal_digest", &self.proposal_digest)?;
        let mut unsigned = self.clone();
        unsigned.proposal_digest.clear();
        if self.proposal_digest
            != domain_digest("magician.attention-candidate-proposal.v1", &unsigned)?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "proposal_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_CONTRIBUTION_MAX_DOCUMENT_BYTES)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAttentionOwnerDecisionV1 {
    Accept,
    Reject,
    Revoke,
}

/// Exact destination snapshot shown by the trusted desktop before it signs an
/// owner decision. The complete proposal is embedded so a UI cannot replace
/// card text, lane, urgency, source identity, handling labels, or retention
/// context while retaining the same friendly proposal id.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAttentionOwnerReviewV1 {
    pub contract_version: u16,
    pub review_id: String,
    pub destination_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_receipt_digest: Option<String>,
    pub desktop_identity_key_id: String,
    pub desktop_identity_digest: String,
    pub proposal: AppAttentionCandidateProposalV1,
    pub display_digest: String,
}

/// Keychain-signed, destination-head-bound owner decision. This is a closed
/// transport document, not generic signing authority. The destination stores
/// only `receipt_digest` in its projection receipt, while the caller may
/// replay this exact envelope after response loss.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAttentionOwnerDecisionEnvelopeV1 {
    pub contract_version: u16,
    pub decision_id: String,
    pub review: AppAttentionOwnerReviewV1,
    pub decision: AppAttentionOwnerDecisionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_until_ms: Option<i64>,
    pub desktop_identity_signature_hex: String,
    pub receipt_digest: String,
}

impl AppAttentionOwnerReviewV1 {
    pub fn mint(
        destination_generation: u64,
        destination_receipt_digest: Option<String>,
        desktop_identity_key_id: String,
        desktop_identity_digest: String,
        proposal: AppAttentionCandidateProposalV1,
    ) -> Result<Self, AppContributionContractError> {
        let mut review = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            review_id: String::new(),
            destination_generation,
            destination_receipt_digest,
            desktop_identity_key_id,
            desktop_identity_digest,
            proposal,
            display_digest: String::new(),
        };
        review.review_id = domain_digest(
            "magician.attention-owner-review-id.v1",
            &(
                &review.proposal.header.scope_binding_ref,
                &review.proposal.header.proposal_id,
                &review.proposal.proposal_digest,
                review.destination_generation,
                &review.destination_receipt_digest,
                &review.desktop_identity_digest,
            ),
        )?;
        review.display_digest = review.expected_display_digest()?;
        review.validate()?;
        Ok(review)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_review.contract_version",
            ));
        }
        self.proposal.validate()?;
        validate_digest("owner_review.review_id", &self.review_id)?;
        validate_token(
            "owner_review.desktop_identity_key_id",
            &self.desktop_identity_key_id,
            MAX_IDENTITY_BYTES,
        )?;
        validate_digest(
            "owner_review.desktop_identity_digest",
            &self.desktop_identity_digest,
        )?;
        match (
            self.destination_generation,
            self.destination_receipt_digest.as_deref(),
        ) {
            (0, None) => {},
            (0, Some(_)) | (_, None) => {
                return Err(AppContributionContractError::InvalidField(
                    "owner_review.destination_receipt_digest",
                ));
            },
            (_, Some(digest)) => {
                validate_digest("owner_review.destination_receipt_digest", digest)?
            },
        }
        let expected_review_id = domain_digest(
            "magician.attention-owner-review-id.v1",
            &(
                &self.proposal.header.scope_binding_ref,
                &self.proposal.header.proposal_id,
                &self.proposal.proposal_digest,
                self.destination_generation,
                &self.destination_receipt_digest,
                &self.desktop_identity_digest,
            ),
        )?;
        if self.review_id != expected_review_id {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.review_id",
            ));
        }
        validate_digest("owner_review.display_digest", &self.display_digest)?;
        if self.display_digest != self.expected_display_digest()? {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.display_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_ATTENTION_OWNER_REVIEW_MAX_BYTES)
    }

    fn expected_display_digest(&self) -> Result<String, AppContributionContractError> {
        let mut unsigned = self.clone();
        unsigned.display_digest.clear();
        domain_digest("magician.attention-owner-review-display.v1", &unsigned)
    }
}

impl AppAttentionOwnerDecisionEnvelopeV1 {
    pub fn prepare(
        review: AppAttentionOwnerReviewV1,
        decision: AppAttentionOwnerDecisionV1,
        retained_until_ms: Option<i64>,
    ) -> Result<Self, AppContributionContractError> {
        review.validate()?;
        let mut envelope = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            decision_id: String::new(),
            review,
            decision,
            retained_until_ms,
            desktop_identity_signature_hex: String::new(),
            receipt_digest: String::new(),
        };
        envelope.decision_id = domain_digest(
            "magician.attention-owner-decision-id.v1",
            &(
                &envelope.review.review_id,
                envelope.decision,
                envelope.retained_until_ms,
                &envelope.review.desktop_identity_digest,
            ),
        )?;
        envelope.validate_unsigned()?;
        Ok(envelope)
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppContributionContractError> {
        self.validate_unsigned()?;
        let mut unsigned = self.clone();
        unsigned.desktop_identity_signature_hex.clear();
        unsigned.receipt_digest.clear();
        serde_json::to_vec(&("magician.attention-owner-decision-signature.v1", unsigned))
            .map_err(|_| AppContributionContractError::Encoding)
    }

    pub fn with_signature_hex(
        mut self,
        signature_hex: String,
    ) -> Result<Self, AppContributionContractError> {
        self.desktop_identity_signature_hex = signature_hex;
        self.receipt_digest.clear();
        self.receipt_digest = domain_digest("magician.attention-owner-decision-receipt.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        self.validate_unsigned()?;
        validate_lower_hex(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
            64,
        )?;
        validate_digest("owner_decision.receipt_digest", &self.receipt_digest)?;
        let mut unsigned_receipt = self.clone();
        unsigned_receipt.receipt_digest.clear();
        if self.receipt_digest
            != domain_digest(
                "magician.attention-owner-decision-receipt.v1",
                &unsigned_receipt,
            )?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.receipt_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_ATTENTION_OWNER_DECISION_MAX_BYTES)
    }

    pub fn verify_signature(
        &self,
        desktop_identity_public_key_hex: &str,
    ) -> Result<(), AppContributionContractError> {
        self.validate()?;
        let public_key = decode_lower_hex::<32>(
            "owner_decision.desktop_identity_public_key_hex",
            desktop_identity_public_key_hex,
        )?;
        let signature = decode_lower_hex::<64>(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
        )?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
            .verify(&self.signing_bytes()?, &signature)
            .map_err(|_| AppContributionContractError::InvalidSignature)
    }

    fn validate_unsigned(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.contract_version",
            ));
        }
        self.review.validate()?;
        validate_digest("owner_decision.decision_id", &self.decision_id)?;
        let expected_decision_id = domain_digest(
            "magician.attention-owner-decision-id.v1",
            &(
                &self.review.review_id,
                self.decision,
                self.retained_until_ms,
                &self.review.desktop_identity_digest,
            ),
        )?;
        if self.decision_id != expected_decision_id {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.decision_id",
            ));
        }
        let retention_is_valid = match (self.decision, self.retained_until_ms) {
            (AppAttentionOwnerDecisionV1::Reject | AppAttentionOwnerDecisionV1::Revoke, None) => {
                true
            },
            (AppAttentionOwnerDecisionV1::Accept, Some(expiry)) => {
                expiry > self.review.proposal.header.issued_at_ms
                    && expiry <= self.review.proposal.header.expires_at_ms
            },
            _ => false,
        };
        if !retention_is_valid {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.retained_until_ms",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAttentionInvalidationReasonV1 {
    SourceUpdated,
    SourceDeleted,
    SourceRestored,
    SourceForgotten,
    PolicyChanged,
    GrantRevoked,
    InstallationDisabled,
    InstallationQuarantined,
    InstallationUninstalledRetained,
    InstallationPurged,
    ContributionExpired,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAttentionInvalidationV1 {
    pub contract_version: u16,
    pub invalidation_id: String,
    pub installation_id: String,
    pub scope_binding_ref: String,
    pub proposal_id: String,
    pub proposal_digest: String,
    pub source_event_ref: String,
    pub source_event_revision: u64,
    pub source_identity_digest: String,
    pub dedupe_key: String,
    pub reason: AppAttentionInvalidationReasonV1,
    pub issued_at_ms: i64,
    pub invalidation_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAttentionIngressDispositionV1 {
    Staged,
    Duplicate,
    PolicyRejected,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAttentionIngressReceiptV1 {
    pub contract_version: u16,
    pub receipt_id: String,
    pub proposal_id: String,
    pub proposal_digest: String,
    pub destination_generation: u64,
    pub destination_receipt_digest: String,
    pub disposition: AppAttentionIngressDispositionV1,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppAttentionDestinationOperationV1 {
    StageProposal {
        proposal: AppAttentionCandidateProposalV1,
    },
    Decide {
        proposal_id: String,
        proposal_digest: String,
        decision: AppAttentionOwnerDecisionV1,
        owner_decision_receipt_digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained_until_ms: Option<i64>,
    },
    Invalidate {
        invalidation: AppAttentionInvalidationV1,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAttentionDestinationReceiptV1 {
    pub contract_version: u16,
    pub receipt_id: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_receipt_digest: Option<String>,
    pub operation: AppAttentionDestinationOperationV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation_disposition: Option<AppAttentionInvalidationDispositionV1>,
    pub resulting_projection_digest: String,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAttentionInvalidationDispositionV1 {
    Tombstoned,
    AlreadyTombstoned,
    SupersededBeforeAdmission,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAttentionInvalidationReceiptV1 {
    pub contract_version: u16,
    pub receipt_id: String,
    pub invalidation_id: String,
    pub invalidation_digest: String,
    pub proposal_id: String,
    pub destination_generation: u64,
    pub destination_receipt_digest: String,
    pub disposition: AppAttentionInvalidationDispositionV1,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

impl_sealed_receipt!(
    AppAttentionIngressReceiptV1,
    receipt_digest,
    "magician.attention-ingress-receipt.v1",
    validate_attention_ingress_receipt,
    APP_CONTRIBUTION_MAX_INGRESS_RECEIPT_BYTES
);
impl_sealed_receipt!(
    AppAttentionInvalidationV1,
    invalidation_digest,
    "magician.attention-invalidation.v1",
    validate_attention_invalidation,
    APP_CONTRIBUTION_MAX_INVALIDATION_BYTES
);
impl_sealed_receipt!(
    AppAttentionDestinationReceiptV1,
    receipt_digest,
    "magician.attention-destination-receipt.v1",
    validate_attention_destination_receipt,
    APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES
);
impl_sealed_receipt!(
    AppAttentionInvalidationReceiptV1,
    receipt_digest,
    "magician.attention-invalidation-receipt.v1",
    validate_attention_invalidation_receipt,
    APP_CONTRIBUTION_MAX_INGRESS_RECEIPT_BYTES
);

/// The only authoritative claim/commitment commands an app may place before
/// the owner.
///
/// This is deliberately not the contribution terminal's hypothesis
/// vocabulary. The signed envelope is consumed by the destination owner and
/// each verb maps to one existing expected-revision transition.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppClaimsDecisionVerbV1 {
    ConfirmClaim,
    RejectClaim,
    RecordCommitment,
    ConfirmCommitment,
}

impl AppClaimsDecisionVerbV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfirmClaim => "confirm_claim",
            Self::RejectClaim => "reject_claim",
            Self::RecordCommitment => "record_commitment",
            Self::ConfirmCommitment => "confirm_commitment",
        }
    }
}

/// Closed audience vocabulary shared with the authoritative commitment
/// register. Keeping it typed prevents an unknown kind from being silently
/// filed under a familiar default.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppClaimsDecisionAudienceKindV1 {
    Engagement,
    Program,
    Account,
    Panel,
    Person,
}

impl AppClaimsDecisionAudienceKindV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Engagement => "engagement",
            Self::Program => "program",
            Self::Account => "account",
            Self::Panel => "panel",
            Self::Person => "person",
        }
    }
}

/// Exact authoritative record head the command was reviewed against.
///
/// Claims and commitments are intentionally distinct variants. A commitment
/// is stored below an audience key, so its target must seal both the audience
/// and id; a naked commitment id is not an authoritative address.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppClaimsDecisionTargetV1 {
    Claim {
        claim_id: String,
        expected_revision: u64,
    },
    Commitment {
        audience_kind: AppClaimsDecisionAudienceKindV1,
        audience_id: String,
        commitment_id: String,
        expected_revision: u64,
    },
}

impl AppClaimsDecisionTargetV1 {
    pub const fn expected_revision(&self) -> u64 {
        match self {
            Self::Claim {
                expected_revision, ..
            }
            | Self::Commitment {
                expected_revision, ..
            } => *expected_revision,
        }
    }

    pub fn target_id(&self) -> &str {
        match self {
            Self::Claim { claim_id, .. } => claim_id,
            Self::Commitment { commitment_id, .. } => commitment_id,
        }
    }

    fn validate(&self) -> Result<(), AppContributionContractError> {
        if self.expected_revision() == 0 {
            return Err(AppContributionContractError::InvalidField(
                "target.expected_revision",
            ));
        }
        validate_token("target.id", self.target_id(), MAX_IDENTITY_BYTES)?;
        if matches!(self.target_id(), "." | "..")
            || self
                .target_id()
                .chars()
                .any(|character| matches!(character, '/' | '\\'))
        {
            return Err(AppContributionContractError::InvalidField("target.id"));
        }
        if let Self::Commitment { audience_id, .. } = self {
            validate_token("target.audience_id", audience_id, MAX_IDENTITY_BYTES)?;
        }
        Ok(())
    }
}

/// Closed source and current-authority identity for one claims command.
///
/// This is intentionally not [`AppContributionSourceHeaderV1`]. Claims
/// decisions are owner-signed commands, not hypothesis contributions, so they
/// have neither a generic contribution port nor a contribution evidence class.
/// The host must resolve these exact installation, package, grant, schema,
/// workflow, action, and `review_decision` source-head fields before presenting
/// the command for signature and again before destination application.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppClaimsDecisionSourceHeaderV1 {
    pub contract_version: u16,
    pub destination_contract_id: String,
    pub destination_contract_version: u16,
    pub destination_schema_digest: String,
    pub proposal_id: String,
    pub proposal_revision: u64,
    pub scope_binding_ref: String,
    pub installation_id: String,
    pub installation_generation: u64,
    pub package_revision_ref: String,
    pub package_content_digest: String,
    pub grant_revision: u64,
    pub grant_authority_digest: String,
    pub schema_revision: u64,
    pub schema_digest: String,
    pub source_entity_name: String,
    pub source_record_id: String,
    pub source_record_revision: u64,
    pub source_record_digest: String,
    pub workflow_id: String,
    pub workflow_digest: String,
    pub action_id: String,
    pub action_digest: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub dedupe_key: String,
}

impl AppClaimsDecisionSourceHeaderV1 {
    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
            || self.destination_contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
            || self.destination_contract_id != APP_CLAIMS_DECISION_CONTRACT_ID
            || self.proposal_revision == 0
            || self.installation_generation == 0
            || self.grant_revision == 0
            || self.schema_revision == 0
            || self.source_record_revision == 0
            || self.source_entity_name != APP_CLAIMS_DECISION_SOURCE_ENTITY
            || self.issued_at_ms < 0
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms - self.issued_at_ms > APP_CONTRIBUTION_MAX_TTL_MS
        {
            return Err(AppContributionContractError::InvalidField("header"));
        }
        for (name, value) in [
            (
                "header.destination_contract_id",
                self.destination_contract_id.as_str(),
            ),
            ("header.proposal_id", self.proposal_id.as_str()),
            ("header.scope_binding_ref", self.scope_binding_ref.as_str()),
            ("header.installation_id", self.installation_id.as_str()),
            (
                "header.package_revision_ref",
                self.package_revision_ref.as_str(),
            ),
            (
                "header.source_entity_name",
                self.source_entity_name.as_str(),
            ),
            ("header.source_record_id", self.source_record_id.as_str()),
            ("header.workflow_id", self.workflow_id.as_str()),
            ("header.action_id", self.action_id.as_str()),
            ("header.dedupe_key", self.dedupe_key.as_str()),
        ] {
            validate_token(name, value, MAX_IDENTITY_BYTES)?;
        }
        for (name, value) in [
            (
                "header.destination_schema_digest",
                self.destination_schema_digest.as_str(),
            ),
            (
                "header.package_content_digest",
                self.package_content_digest.as_str(),
            ),
            (
                "header.grant_authority_digest",
                self.grant_authority_digest.as_str(),
            ),
            ("header.schema_digest", self.schema_digest.as_str()),
            (
                "header.source_record_digest",
                self.source_record_digest.as_str(),
            ),
            ("header.workflow_digest", self.workflow_digest.as_str()),
            ("header.action_digest", self.action_digest.as_str()),
        ] {
            validate_digest(name, value)?;
        }
        Ok(())
    }
}

/// Bounded authoritative command proposed from one app installation.
///
/// `expected_revision` is part of the sealed target and the signed display.
/// The destination therefore cannot apply an owner's decision to a head they
/// did not review. `by` is never defaultable; the canonical stores retain the
/// extractor-self-confirm and named-person rules. Notes are accepted only for
/// claim decisions, because the commitment transitions have no note field and
/// silently discarding signed input would make the review misleading.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppClaimsDecisionProposalV1 {
    pub header: AppClaimsDecisionSourceHeaderV1,
    pub verb: AppClaimsDecisionVerbV1,
    pub target: AppClaimsDecisionTargetV1,
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub proposal_digest: String,
}

impl AppClaimsDecisionProposalV1 {
    pub fn seal(mut self) -> Result<Self, AppContributionContractError> {
        self.proposal_digest.clear();
        self.proposal_digest = domain_digest("magician.claims-decision-proposal.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        self.header.validate()?;
        self.target.validate()?;
        validate_token("by", &self.by, MAX_IDENTITY_BYTES)?;
        if !self.by.is_ascii() {
            return Err(AppContributionContractError::InvalidField("by"));
        }
        if let Some(note) = self.note.as_deref() {
            validate_nonempty_text("note", note, APP_CLAIMS_DECISION_MAX_NOTE_BYTES)?;
        }
        let target_is_valid = matches!(
            (self.verb, &self.target),
            (
                AppClaimsDecisionVerbV1::ConfirmClaim
                    | AppClaimsDecisionVerbV1::RejectClaim
                    | AppClaimsDecisionVerbV1::RecordCommitment,
                AppClaimsDecisionTargetV1::Claim { .. }
            ) | (
                AppClaimsDecisionVerbV1::ConfirmCommitment,
                AppClaimsDecisionTargetV1::Commitment { .. }
            )
        );
        if !target_is_valid {
            return Err(AppContributionContractError::InvalidField("target"));
        }
        if self.note.is_some()
            && matches!(
                self.verb,
                AppClaimsDecisionVerbV1::RecordCommitment
                    | AppClaimsDecisionVerbV1::ConfirmCommitment
            )
        {
            return Err(AppContributionContractError::InvalidField("note"));
        }
        validate_digest("proposal_digest", &self.proposal_digest)?;
        let mut unsigned = self.clone();
        unsigned.proposal_digest.clear();
        if self.proposal_digest != domain_digest("magician.claims-decision-proposal.v1", &unsigned)?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "proposal_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_CONTRIBUTION_MAX_DOCUMENT_BYTES)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppClaimsDecisionOwnerDecisionV1 {
    Accept,
    Reject,
    Revoke,
}

/// Exact command snapshot shown by the trusted desktop before signing.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppClaimsDecisionOwnerReviewV1 {
    pub contract_version: u16,
    pub review_id: String,
    pub desktop_pairing_generation: u64,
    pub desktop_identity_key_id: String,
    pub desktop_identity_digest: String,
    pub proposal: AppClaimsDecisionProposalV1,
    pub display_digest: String,
}

/// Keychain-signed owner decision over one revision-bound command.
///
/// The envelope is a replayable command receipt, not generic signing
/// authority. The target revision, actor, note, source provenance, active
/// desktop pairing generation, and closed verb are all embedded in the signed
/// review. `decision_id` is derived from the authoritative scope and command
/// identity rather than proposal/review presentation metadata, so an equivalent
/// second envelope is a clean replay at the canonical store.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppClaimsDecisionOwnerDecisionEnvelopeV1 {
    pub contract_version: u16,
    pub decision_id: String,
    pub review: AppClaimsDecisionOwnerReviewV1,
    pub decision: AppClaimsDecisionOwnerDecisionV1,
    pub desktop_identity_signature_hex: String,
    pub receipt_digest: String,
}

impl AppClaimsDecisionOwnerReviewV1 {
    pub fn mint(
        desktop_pairing_generation: u64,
        desktop_identity_key_id: String,
        desktop_identity_digest: String,
        proposal: AppClaimsDecisionProposalV1,
    ) -> Result<Self, AppContributionContractError> {
        let mut review = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            review_id: String::new(),
            desktop_pairing_generation,
            desktop_identity_key_id,
            desktop_identity_digest,
            proposal,
            display_digest: String::new(),
        };
        review.review_id = domain_digest(
            "magician.claims-decision-owner-review-id.v1",
            &(
                &review.proposal.header.scope_binding_ref,
                &review.proposal.header.proposal_id,
                &review.proposal.proposal_digest,
                review.desktop_pairing_generation,
                &review.desktop_identity_digest,
            ),
        )?;
        review.display_digest = review.expected_display_digest()?;
        review.validate()?;
        Ok(review)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_review.contract_version",
            ));
        }
        if self.desktop_pairing_generation == 0 {
            return Err(AppContributionContractError::InvalidField(
                "owner_review.desktop_pairing_generation",
            ));
        }
        self.proposal.validate()?;
        validate_digest("owner_review.review_id", &self.review_id)?;
        validate_token(
            "owner_review.desktop_identity_key_id",
            &self.desktop_identity_key_id,
            MAX_IDENTITY_BYTES,
        )?;
        validate_digest(
            "owner_review.desktop_identity_digest",
            &self.desktop_identity_digest,
        )?;
        let expected_review_id = domain_digest(
            "magician.claims-decision-owner-review-id.v1",
            &(
                &self.proposal.header.scope_binding_ref,
                &self.proposal.header.proposal_id,
                &self.proposal.proposal_digest,
                self.desktop_pairing_generation,
                &self.desktop_identity_digest,
            ),
        )?;
        if self.review_id != expected_review_id {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.review_id",
            ));
        }
        validate_digest("owner_review.display_digest", &self.display_digest)?;
        if self.display_digest != self.expected_display_digest()? {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.display_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_CLAIMS_DECISION_OWNER_REVIEW_MAX_BYTES)
    }

    fn expected_display_digest(&self) -> Result<String, AppContributionContractError> {
        let mut unsigned = self.clone();
        unsigned.display_digest.clear();
        domain_digest(
            "magician.claims-decision-owner-review-display.v1",
            &unsigned,
        )
    }
}

impl AppClaimsDecisionOwnerDecisionEnvelopeV1 {
    pub fn prepare(
        review: AppClaimsDecisionOwnerReviewV1,
        decision: AppClaimsDecisionOwnerDecisionV1,
    ) -> Result<Self, AppContributionContractError> {
        review.validate()?;
        let mut envelope = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            decision_id: String::new(),
            review,
            decision,
            desktop_identity_signature_hex: String::new(),
            receipt_digest: String::new(),
        };
        envelope.decision_id = domain_digest(
            "magician.claims-decision-owner-decision-id.v1",
            &(
                &envelope.review.proposal.header.scope_binding_ref,
                envelope.review.proposal.verb,
                &envelope.review.proposal.target,
                &envelope.review.proposal.by,
                &envelope.review.proposal.note,
                envelope.decision,
            ),
        )?;
        envelope.validate_unsigned()?;
        Ok(envelope)
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppContributionContractError> {
        self.validate_unsigned()?;
        let mut unsigned = self.clone();
        unsigned.desktop_identity_signature_hex.clear();
        unsigned.receipt_digest.clear();
        serde_json::to_vec(&(
            "magician.claims-decision-owner-decision-signature.v1",
            unsigned,
        ))
        .map_err(|_| AppContributionContractError::Encoding)
    }

    pub fn with_signature_hex(
        mut self,
        signature_hex: String,
    ) -> Result<Self, AppContributionContractError> {
        self.desktop_identity_signature_hex = signature_hex;
        self.receipt_digest.clear();
        self.receipt_digest =
            domain_digest("magician.claims-decision-owner-decision-receipt.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        self.validate_unsigned()?;
        validate_lower_hex(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
            64,
        )?;
        validate_digest("owner_decision.receipt_digest", &self.receipt_digest)?;
        let mut unsigned_receipt = self.clone();
        unsigned_receipt.receipt_digest.clear();
        if self.receipt_digest
            != domain_digest(
                "magician.claims-decision-owner-decision-receipt.v1",
                &unsigned_receipt,
            )?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.receipt_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_CLAIMS_DECISION_OWNER_DECISION_MAX_BYTES)
    }

    pub fn verify_signature(
        &self,
        desktop_identity_public_key_hex: &str,
    ) -> Result<(), AppContributionContractError> {
        self.validate()?;
        let public_key = decode_lower_hex::<32>(
            "owner_decision.desktop_identity_public_key_hex",
            desktop_identity_public_key_hex,
        )?;
        let signature = decode_lower_hex::<64>(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
        )?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
            .verify(&self.signing_bytes()?, &signature)
            .map_err(|_| AppContributionContractError::InvalidSignature)
    }

    fn validate_unsigned(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.contract_version",
            ));
        }
        self.review.validate()?;
        validate_digest("owner_decision.decision_id", &self.decision_id)?;
        let expected_decision_id = domain_digest(
            "magician.claims-decision-owner-decision-id.v1",
            &(
                &self.review.proposal.header.scope_binding_ref,
                self.review.proposal.verb,
                &self.review.proposal.target,
                &self.review.proposal.by,
                &self.review.proposal.note,
                self.decision,
            ),
        )?;
        if self.decision_id != expected_decision_id {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.decision_id",
            ));
        }
        Ok(())
    }
}

/// The only capture commands an app may place before the owner.
///
/// `Listen` and `Join` are the START class: they can begin an hours-long
/// capture session, so they carry a separate intent proof (see
/// [`AppMeetingControlGestureV1`]). `Pause`, `Resume` and `Stop` act on a
/// session that already exists and are risk-reducing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMeetingControlVerbV1 {
    Listen,
    Join,
    Pause,
    Resume,
    Stop,
}

impl AppMeetingControlVerbV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Listen => "listen",
            Self::Join => "join",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Stop => "stop",
        }
    }

    /// True for the two verbs that can begin a capture session.
    pub const fn starts_capture(self) -> bool {
        matches!(self, Self::Listen | Self::Join)
    }
}

/// Exact capture target the command was reviewed against.
///
/// The two variants are deliberately distinct. A start names a meeting that
/// does not exist as a session yet; a control names a live session id, whose
/// rail prefix the managers dispatch on. A naked string could be either, and
/// "either" is how a stop turns into a start.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppMeetingControlTargetV1 {
    NewCapture {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// `YYYY-MM-DD`; the shared thread resolver's date component.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        date: Option<String>,
        /// Passive rail only. The mic tap reads the OS default input device,
        /// which an app-level mute does not silence, so it is never defaulted
        /// on: the value is sealed into the signed display the owner reads.
        capture_mic: bool,
    },
    LiveSession {
        session_id: String,
    },
}

/// Rail prefixes minted by the two session managers.
const MEETING_CONTROL_PASSIVE_SESSION_PREFIX: &str = "listen-";
const MEETING_CONTROL_ATTENDEE_SESSION_PREFIX: &str = "meet-";

impl AppMeetingControlTargetV1 {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::LiveSession { session_id } => Some(session_id),
            Self::NewCapture { .. } => None,
        }
    }

    pub fn meeting_url(&self) -> Option<&str> {
        match self {
            Self::NewCapture { url, .. } => url.as_deref(),
            Self::LiveSession { .. } => None,
        }
    }

    fn validate(&self, verb: AppMeetingControlVerbV1) -> Result<(), AppContributionContractError> {
        match self {
            Self::NewCapture {
                url,
                title,
                date,
                capture_mic,
            } => {
                if !verb.starts_capture() {
                    return Err(AppContributionContractError::InvalidField("target"));
                }
                // The attendee rail drives a browser to a meeting URL; without
                // one there is nothing to join, and a defaulted URL would be a
                // command the owner never read.
                if verb == AppMeetingControlVerbV1::Join && url.is_none() {
                    return Err(AppContributionContractError::InvalidField("target.url"));
                }
                // Only the passive listener owns a microphone tap.
                if *capture_mic && verb != AppMeetingControlVerbV1::Listen {
                    return Err(AppContributionContractError::InvalidField(
                        "target.capture_mic",
                    ));
                }
                if let Some(url) = url.as_deref() {
                    validate_token("target.url", url, APP_MEETING_CONTROL_MAX_URL_BYTES)?;
                    validate_meeting_url(url)?;
                }
                if let Some(title) = title.as_deref() {
                    validate_nonempty_text(
                        "target.title",
                        title,
                        APP_MEETING_CONTROL_MAX_TITLE_BYTES,
                    )?;
                }
                if let Some(date) = date.as_deref() {
                    let calendar_shaped = date.len() == 10
                        && date.as_bytes()[4] == b'-'
                        && date.as_bytes()[7] == b'-'
                        && date
                            .bytes()
                            .enumerate()
                            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
                    if !calendar_shaped {
                        return Err(AppContributionContractError::InvalidField("target.date"));
                    }
                }
                Ok(())
            },
            Self::LiveSession { session_id } => {
                if verb.starts_capture() {
                    return Err(AppContributionContractError::InvalidField("target"));
                }
                validate_token("target.session_id", session_id, MAX_IDENTITY_BYTES)?;
                // The managers dispatch on the rail prefix; an id without one
                // would silently fall through to the attendee manager.
                if !(session_id.starts_with(MEETING_CONTROL_PASSIVE_SESSION_PREFIX)
                    || session_id.starts_with(MEETING_CONTROL_ATTENDEE_SESSION_PREFIX))
                {
                    return Err(AppContributionContractError::InvalidField(
                        "target.session_id",
                    ));
                }
                // Session ids are uuid-suffixed rail names and reach a marker
                // path; a separator or a parent hop is never one of them.
                if session_id.contains('/')
                    || session_id.contains('\\')
                    || session_id.contains("..")
                {
                    return Err(AppContributionContractError::InvalidField(
                        "target.session_id",
                    ));
                }
                Ok(())
            },
        }
    }
}

/// A meeting link an app may put in front of the owner for signature.
///
/// A signed `join` navigates the owner's PERSISTENT, SIGNED-IN browser profile
/// to this URL. `https://` and a plain public host are therefore not a style
/// preference: plaintext would carry that session over the wire, and an IP
/// literal, `localhost`, an explicit port or embedded userinfo are how a
/// plausible-looking link reaches something that is not a meeting. The
/// first-party path keeps its wider latitude because the operator types the URL
/// there; an app proposal does not.
fn validate_meeting_url(url: &str) -> Result<(), AppContributionContractError> {
    const FIELD: &str = "target.url";
    // Schemes are case-insensitive per RFC 3986; a pasted `HTTPS://` link is a
    // real meeting link, not an attack.
    let lowered = url.to_ascii_lowercase();
    let Some(scheme_end) = lowered
        .strip_prefix("https://")
        .map(|rest| url.len() - rest.len())
    else {
        return Err(AppContributionContractError::InvalidField(FIELD));
    };
    let rest = &url[scheme_end..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // A trailing-dot FQDN is the same host; normalize rather than refuse.
    let authority = authority.strip_suffix('.').unwrap_or(authority);
    if authority.is_empty()
        // Userinfo: `https://meet.google.com@evil.example/…` reads as the
        // meeting host and resolves to the attacker's.
        || authority.contains('@')
        // An explicit port is never part of a public meeting link.
        || authority.contains(':')
        || authority.starts_with('.')
        || authority.contains("..")
    {
        return Err(AppContributionContractError::InvalidField(FIELD));
    }
    let labels = authority.split('.').collect::<Vec<_>>();
    // Require a dotted public name: this rejects `localhost` and every
    // single-label intranet name in one test.
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty()) {
        return Err(AppContributionContractError::InvalidField(FIELD));
    }
    // Underscores appear in real tenant hostnames (`my_org.zoom.us`); brackets,
    // percent-escapes and backslashes — the userinfo- and IPv6-smuggling
    // shapes — remain refused by exclusion.
    if !authority
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
    {
        return Err(AppContributionContractError::InvalidField(FIELD));
    }
    // An all-numeric final label is an IPv4 literal, not a hostname. (IPv6
    // literals need brackets, which the character class above already refuses.)
    let last = labels.last().copied().unwrap_or_default();
    if last.is_empty() || last.chars().all(|character| character.is_ascii_digit()) {
        return Err(AppContributionContractError::InvalidField(FIELD));
    }
    // Loopback, mDNS, private-network and reserved names. `localhost` needs an
    // explicit arm even though the bare single label is caught by the
    // two-label rule: `anything.localhost` has two labels and resolves to
    // 127.0.0.1 under RFC 6761.
    if matches!(
        last.to_ascii_lowercase().as_str(),
        "localhost"
            | "local"
            | "internal"
            | "localdomain"
            | "home"
            | "lan"
            | "intranet"
            | "corp"
            | "test"
            | "invalid"
            | "example"
    ) {
        return Err(AppContributionContractError::InvalidField(FIELD));
    }
    Ok(())
}

/// Proof that a person acted in the surface, moments ago, to ask for THIS
/// start.
///
/// The reviewer's finding the design carries: routing a start through the same
/// join path the API uses proves authority reuse, not user intent. The gesture
/// binds the surface session that observed the act and an expiry the
/// destination re-checks against its own clock, so a signed start cannot be
/// banked and replayed later. It is sealed into the proposal digest and the
/// owner's signed display, and it participates in the decision id, so two
/// starts from two gestures are two decisions rather than one replay.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMeetingControlGestureV1 {
    /// The custom-surface bridge session the gesture happened in.
    pub surface_session_id: String,
    /// Unique per act, so two clicks are two commands.
    pub gesture_id: String,
    pub observed_at_ms: i64,
    pub expires_at_ms: i64,
}

impl AppMeetingControlGestureV1 {
    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        validate_token(
            "gesture.surface_session_id",
            &self.surface_session_id,
            MAX_IDENTITY_BYTES,
        )?;
        validate_token("gesture.gesture_id", &self.gesture_id, MAX_IDENTITY_BYTES)?;
        if self.observed_at_ms < 0
            || self.expires_at_ms <= self.observed_at_ms
            || self.expires_at_ms - self.observed_at_ms > APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS
        {
            return Err(AppContributionContractError::InvalidField("gesture"));
        }
        Ok(())
    }

    /// Fresh at `now_ms` — the destination's own clock, never the app's.
    pub const fn is_fresh_at(&self, now_ms: i64) -> bool {
        now_ms >= self.observed_at_ms && now_ms < self.expires_at_ms
    }
}

/// Closed source and current-authority identity for one meeting-control
/// command. Structurally the claims-decision header with its own contract id
/// and source entity: the two families must never validate each other's
/// envelopes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMeetingControlSourceHeaderV1 {
    pub contract_version: u16,
    pub destination_contract_id: String,
    pub destination_contract_version: u16,
    pub destination_schema_digest: String,
    pub proposal_id: String,
    pub proposal_revision: u64,
    pub scope_binding_ref: String,
    pub installation_id: String,
    pub installation_generation: u64,
    pub package_revision_ref: String,
    pub package_content_digest: String,
    pub grant_revision: u64,
    pub grant_authority_digest: String,
    pub schema_revision: u64,
    pub schema_digest: String,
    pub source_entity_name: String,
    pub source_record_id: String,
    pub source_record_revision: u64,
    pub source_record_digest: String,
    pub workflow_id: String,
    pub workflow_digest: String,
    pub action_id: String,
    pub action_digest: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub dedupe_key: String,
}

impl AppMeetingControlSourceHeaderV1 {
    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
            || self.destination_contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
            || self.destination_contract_id != APP_MEETING_CONTROL_CONTRACT_ID
            || self.proposal_revision == 0
            || self.installation_generation == 0
            || self.grant_revision == 0
            || self.schema_revision == 0
            || self.source_record_revision == 0
            || self.source_entity_name != APP_MEETING_CONTROL_SOURCE_ENTITY
            || self.issued_at_ms < 0
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms - self.issued_at_ms > APP_CONTRIBUTION_MAX_TTL_MS
        {
            return Err(AppContributionContractError::InvalidField("header"));
        }
        for (name, value) in [
            (
                "header.destination_contract_id",
                self.destination_contract_id.as_str(),
            ),
            ("header.proposal_id", self.proposal_id.as_str()),
            ("header.scope_binding_ref", self.scope_binding_ref.as_str()),
            ("header.installation_id", self.installation_id.as_str()),
            (
                "header.package_revision_ref",
                self.package_revision_ref.as_str(),
            ),
            (
                "header.source_entity_name",
                self.source_entity_name.as_str(),
            ),
            ("header.source_record_id", self.source_record_id.as_str()),
            ("header.workflow_id", self.workflow_id.as_str()),
            ("header.action_id", self.action_id.as_str()),
            ("header.dedupe_key", self.dedupe_key.as_str()),
        ] {
            validate_token(name, value, MAX_IDENTITY_BYTES)?;
        }
        for (name, value) in [
            (
                "header.destination_schema_digest",
                self.destination_schema_digest.as_str(),
            ),
            (
                "header.package_content_digest",
                self.package_content_digest.as_str(),
            ),
            (
                "header.grant_authority_digest",
                self.grant_authority_digest.as_str(),
            ),
            ("header.schema_digest", self.schema_digest.as_str()),
            (
                "header.source_record_digest",
                self.source_record_digest.as_str(),
            ),
            ("header.workflow_digest", self.workflow_digest.as_str()),
            ("header.action_digest", self.action_digest.as_str()),
        ] {
            validate_digest(name, value)?;
        }
        Ok(())
    }
}

/// Bounded capture command proposed from one app installation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMeetingControlProposalV1 {
    pub header: AppMeetingControlSourceHeaderV1,
    pub verb: AppMeetingControlVerbV1,
    pub target: AppMeetingControlTargetV1,
    /// Present exactly for the START class. A stop that carried a gesture
    /// would read, in the owner's signed display, like an intent-bound start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture: Option<AppMeetingControlGestureV1>,
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub proposal_digest: String,
}

impl AppMeetingControlProposalV1 {
    pub fn seal(mut self) -> Result<Self, AppContributionContractError> {
        self.proposal_digest.clear();
        self.proposal_digest = domain_digest("magician.meeting-control-proposal.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        self.header.validate()?;
        self.target.validate(self.verb)?;
        validate_token("by", &self.by, MAX_IDENTITY_BYTES)?;
        if !self.by.is_ascii() {
            return Err(AppContributionContractError::InvalidField("by"));
        }
        if let Some(note) = self.note.as_deref() {
            validate_nonempty_text("note", note, APP_MEETING_CONTROL_MAX_NOTE_BYTES)?;
        }
        match (&self.gesture, self.verb.starts_capture()) {
            (Some(gesture), true) => {
                gesture.validate()?;
                // The gesture's WIDTH was capped by its own validate; this caps
                // its POSITION. Without both, an app could seal a start whose
                // gesture window opens weeks from now, have it signed today
                // against a plausible display, and apply it a month later —
                // "banked", which is exactly what the gesture exists to
                // prevent. Pinning the gesture inside the proposal's lifetime
                // and capping that lifetime makes the whole signed START a
                // two-minute object the owner can see the bounds of.
                if gesture.observed_at_ms < self.header.issued_at_ms
                    || gesture.expires_at_ms > self.header.expires_at_ms
                    || self.header.expires_at_ms - self.header.issued_at_ms
                        > APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS
                {
                    return Err(AppContributionContractError::InvalidField("gesture.window"));
                }
            },
            (None, false) => {},
            _ => return Err(AppContributionContractError::InvalidField("gesture")),
        }
        // The signed workflow and action must be the verb itself: a command
        // cannot be reviewed as one control and dispatched as another.
        if self.header.workflow_id != self.verb.as_str()
            || self.header.action_id != self.verb.as_str()
        {
            return Err(AppContributionContractError::InvalidField(
                "header.action_id",
            ));
        }
        validate_digest("proposal_digest", &self.proposal_digest)?;
        let mut unsigned = self.clone();
        unsigned.proposal_digest.clear();
        if self.proposal_digest != domain_digest("magician.meeting-control-proposal.v1", &unsigned)?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "proposal_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_CONTRIBUTION_MAX_DOCUMENT_BYTES)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMeetingControlOwnerDecisionV1 {
    Accept,
    Reject,
    Revoke,
}

/// Exact command snapshot the trusted desktop shows before signing.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMeetingControlOwnerReviewV1 {
    pub contract_version: u16,
    pub review_id: String,
    pub desktop_pairing_generation: u64,
    pub desktop_identity_key_id: String,
    pub desktop_identity_digest: String,
    pub proposal: AppMeetingControlProposalV1,
    pub display_digest: String,
}

/// Keychain-signed owner decision over one capture command.
///
/// Unlike the claims family, the decision id includes the gesture. A capture
/// start is not idempotent in the way a revision-bound claim decision is:
/// re-applying one signed start an hour later would open a NEW hours-long
/// session. Binding the gesture makes a second start from a second act a
/// second decision rather than a replay of the first.
///
/// Three layers carry that through, and it is worth being exact about which
/// each one closes:
///
/// 1. The proposal pins the gesture inside its own lifetime and caps that
///    lifetime at [`APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS`], so a signed START
///    is a two-minute object rather than one that can sit valid for the
///    contribution TTL.
/// 2. The destination re-checks that window against its OWN clock immediately
///    before it touches a manager — never the app's clock, and never the clock
///    sampled when the request first arrived.
/// 3. The destination consumes the decision id durably, so one signed envelope
///    applies at most once.
///
/// **What is NOT proven:** that a person was present. `surface_session_id` is
/// minted by frame-side code and no host-side registry witnesses the act, so
/// the gesture attests recency and single-use, not humanity. The owner's
/// signature over a display that shows the window is what carries intent. A
/// host-minted intent ticket would close that last gap and is recorded as owed
/// work, not claimed here.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMeetingControlOwnerDecisionEnvelopeV1 {
    pub contract_version: u16,
    pub decision_id: String,
    pub review: AppMeetingControlOwnerReviewV1,
    pub decision: AppMeetingControlOwnerDecisionV1,
    /// When the trusted desktop signed this decision, by ITS clock.
    ///
    /// This is the anchor. Every constraint that is merely relative to the
    /// proposal's own `issued_at_ms` can be waited out: an app that sets
    /// `issued_at_ms` thirty days out satisfies a two-minute width, a
    /// gesture-inside-lifetime rule and a lifetime cap alike, gets the command
    /// signed today against a plausible display, and applies it in a month. The
    /// app cannot forge this field because it is inside the signature and the
    /// app does not hold the key; the destination compares it to its own clock.
    pub decided_at_ms: i64,
    pub desktop_identity_signature_hex: String,
    pub receipt_digest: String,
}

impl AppMeetingControlOwnerReviewV1 {
    pub fn mint(
        desktop_pairing_generation: u64,
        desktop_identity_key_id: String,
        desktop_identity_digest: String,
        proposal: AppMeetingControlProposalV1,
    ) -> Result<Self, AppContributionContractError> {
        let mut review = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            review_id: String::new(),
            desktop_pairing_generation,
            desktop_identity_key_id,
            desktop_identity_digest,
            proposal,
            display_digest: String::new(),
        };
        review.review_id = review.expected_review_id()?;
        review.display_digest = review.expected_display_digest()?;
        review.validate()?;
        Ok(review)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_review.contract_version",
            ));
        }
        if self.desktop_pairing_generation == 0 {
            return Err(AppContributionContractError::InvalidField(
                "owner_review.desktop_pairing_generation",
            ));
        }
        self.proposal.validate()?;
        validate_digest("owner_review.review_id", &self.review_id)?;
        validate_token(
            "owner_review.desktop_identity_key_id",
            &self.desktop_identity_key_id,
            MAX_IDENTITY_BYTES,
        )?;
        validate_digest(
            "owner_review.desktop_identity_digest",
            &self.desktop_identity_digest,
        )?;
        if self.review_id != self.expected_review_id()? {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.review_id",
            ));
        }
        validate_digest("owner_review.display_digest", &self.display_digest)?;
        if self.display_digest != self.expected_display_digest()? {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.display_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_MEETING_CONTROL_OWNER_REVIEW_MAX_BYTES)
    }

    fn expected_review_id(&self) -> Result<String, AppContributionContractError> {
        domain_digest(
            "magician.meeting-control-owner-review-id.v1",
            &(
                &self.proposal.header.scope_binding_ref,
                &self.proposal.header.proposal_id,
                &self.proposal.proposal_digest,
                self.desktop_pairing_generation,
                &self.desktop_identity_digest,
            ),
        )
    }

    fn expected_display_digest(&self) -> Result<String, AppContributionContractError> {
        let mut unsigned = self.clone();
        unsigned.display_digest.clear();
        domain_digest(
            "magician.meeting-control-owner-review-display.v1",
            &unsigned,
        )
    }
}

impl AppMeetingControlOwnerDecisionEnvelopeV1 {
    /// `decided_at_ms` is the SIGNER's clock, supplied by the trusted desktop at
    /// signature time. The contract stays clock-free on purpose — it is a pure
    /// validation crate — so the caller passes the instant rather than reading
    /// one here.
    pub fn prepare(
        review: AppMeetingControlOwnerReviewV1,
        decision: AppMeetingControlOwnerDecisionV1,
        decided_at_ms: i64,
    ) -> Result<Self, AppContributionContractError> {
        review.validate()?;
        let mut envelope = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            decision_id: String::new(),
            review,
            decision,
            decided_at_ms,
            desktop_identity_signature_hex: String::new(),
            receipt_digest: String::new(),
        };
        envelope.decision_id = envelope.expected_decision_id()?;
        envelope.validate_unsigned()?;
        Ok(envelope)
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppContributionContractError> {
        self.validate_unsigned()?;
        let mut unsigned = self.clone();
        unsigned.desktop_identity_signature_hex.clear();
        unsigned.receipt_digest.clear();
        serde_json::to_vec(&(
            "magician.meeting-control-owner-decision-signature.v1",
            unsigned,
        ))
        .map_err(|_| AppContributionContractError::Encoding)
    }

    pub fn with_signature_hex(
        mut self,
        signature_hex: String,
    ) -> Result<Self, AppContributionContractError> {
        self.desktop_identity_signature_hex = signature_hex;
        self.receipt_digest.clear();
        self.receipt_digest =
            domain_digest("magician.meeting-control-owner-decision-receipt.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        self.validate_unsigned()?;
        validate_lower_hex(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
            64,
        )?;
        validate_digest("owner_decision.receipt_digest", &self.receipt_digest)?;
        let mut unsigned_receipt = self.clone();
        unsigned_receipt.receipt_digest.clear();
        if self.receipt_digest
            != domain_digest(
                "magician.meeting-control-owner-decision-receipt.v1",
                &unsigned_receipt,
            )?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.receipt_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_MEETING_CONTROL_OWNER_DECISION_MAX_BYTES)
    }

    pub fn verify_signature(
        &self,
        desktop_identity_public_key_hex: &str,
    ) -> Result<(), AppContributionContractError> {
        self.validate()?;
        let public_key = decode_lower_hex::<32>(
            "owner_decision.desktop_identity_public_key_hex",
            desktop_identity_public_key_hex,
        )?;
        let signature = decode_lower_hex::<64>(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
        )?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
            .verify(&self.signing_bytes()?, &signature)
            .map_err(|_| AppContributionContractError::InvalidSignature)
    }

    fn expected_decision_id(&self) -> Result<String, AppContributionContractError> {
        domain_digest(
            "magician.meeting-control-owner-decision-id.v1",
            &(
                &self.review.proposal.header.scope_binding_ref,
                self.review.proposal.verb,
                &self.review.proposal.target,
                // The gesture participates: two starts from two acts are two
                // decisions, never one replay of the first.
                &self.review.proposal.gesture,
                &self.review.proposal.by,
                &self.review.proposal.note,
                self.decision,
                self.decided_at_ms,
                // The sealed proposal itself. Without it the three verbs that
                // carry NO gesture have no discriminator at all: "pause
                // listen-4f2c" today and the same pause ninety seconds later
                // would hash identically, and a destination that consumes
                // decision ids exactly-once would refuse every repeat control
                // on a session for the life of that session id — making
                // pause/resume cycling, the whole point of those verbs,
                // impossible.
                &self.review.proposal.proposal_digest,
            ),
        )
    }

    fn validate_unsigned(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.contract_version",
            ));
        }
        self.review.validate()?;
        if self.decided_at_ms < 0 {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.decided_at_ms",
            ));
        }
        // A signature can only be about a command that already exists, and a
        // START's whole signed object is bounded by the gesture ceiling, so the
        // signature must fall inside the proposal's own lifetime.
        if self.review.proposal.verb.starts_capture()
            && (self.decided_at_ms < self.review.proposal.header.issued_at_ms
                || self.decided_at_ms >= self.review.proposal.header.expires_at_ms)
        {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.decided_at_ms",
            ));
        }
        validate_digest("owner_decision.decision_id", &self.decision_id)?;
        if self.decision_id != self.expected_decision_id()? {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.decision_id",
            ));
        }
        Ok(())
    }

    /// True when a decision signed at `decided_at_ms` is still fresh at
    /// `now_ms` by the destination's own clock.
    ///
    /// A small backward tolerance absorbs ordinary desktop/host clock skew; a
    /// signature claiming the future beyond that tolerance is refused rather
    /// than trusted.
    pub const fn is_recently_decided_at(&self, now_ms: i64) -> bool {
        let elapsed = now_ms - self.decided_at_ms;
        elapsed >= -APP_MEETING_CONTROL_MAX_CLOCK_SKEW_MS
            && elapsed <= APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS
    }
}

/// Closed review-decision vocabulary for V1 learning candidates.
///
/// Wire values deliberately match the learning review console's decision
/// ledger (`approve`/`reject`/`snooze`) so a package records and proposes the
/// same decision vocabulary without a translation table that can drift. The
/// destination owner, not the app, maps a decision onto the substrate's state
/// machine; `promoted` is deliberately absent — the promotion bridges stay
/// first-party.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppLearningDecisionKindV1 {
    Approve,
    Reject,
    Snooze,
}

impl AppLearningDecisionKindV1 {
    /// Canonical wire string shared with the review ledger vocabulary.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Snooze => "snooze",
        }
    }
}

/// Bounded, source-linked learning-decision proposal from one app
/// installation.
///
/// The body carries only what an owner can review: the core candidate id the
/// decision speaks about, one decision from the closed vocabulary, a bounded
/// reason, and — for snooze — a deferral bound inside the proposal's
/// lifetime. The header's `dedupe_key` is the idempotency key, exactly as for
/// memory and attention candidates; apps never own the learning substrate,
/// they propose decisions and the owner applies them.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLearningDecisionProposalV1 {
    pub header: AppContributionSourceHeaderV1,
    pub candidate_id: String,
    pub decision: AppLearningDecisionKindV1,
    pub reason: String,
    pub reason_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze_until_ms: Option<i64>,
    pub proposal_digest: String,
}

impl AppLearningDecisionProposalV1 {
    pub fn seal(mut self) -> Result<Self, AppContributionContractError> {
        self.proposal_digest.clear();
        self.proposal_digest = domain_digest("magician.learning-decision-proposal.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        validate_header(&self.header, APP_LEARNING_DECISION_CONTRACT_ID)?;
        validate_token("candidate_id", &self.candidate_id, MAX_IDENTITY_BYTES)?;
        validate_nonempty_text("reason", &self.reason, APP_LEARNING_MAX_REASON_BYTES)?;
        validate_digest("reason_digest", &self.reason_digest)?;
        if self.reason_digest != content_digest(self.reason.as_bytes()) {
            return Err(AppContributionContractError::DigestMismatch(
                "reason_digest",
            ));
        }
        // A snooze names its deferral bound; every other decision must not.
        // The bound must land inside the proposal lifetime so a stale snooze
        // cannot defer a candidate past its reviewed retention.
        match (self.decision, self.snooze_until_ms) {
            (AppLearningDecisionKindV1::Snooze, Some(bound)) => {
                if bound <= self.header.issued_at_ms || bound > self.header.expires_at_ms {
                    return Err(AppContributionContractError::InvalidField(
                        "snooze_until_ms",
                    ));
                }
            },
            (AppLearningDecisionKindV1::Snooze, None) => {
                return Err(AppContributionContractError::InvalidField(
                    "snooze_until_ms",
                ));
            },
            (_, Some(_)) => {
                return Err(AppContributionContractError::InvalidField(
                    "snooze_until_ms",
                ));
            },
            (_, None) => {},
        }
        validate_digest("proposal_digest", &self.proposal_digest)?;
        let mut unsigned = self.clone();
        unsigned.proposal_digest.clear();
        if self.proposal_digest
            != domain_digest("magician.learning-decision-proposal.v1", &unsigned)?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "proposal_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_CONTRIBUTION_MAX_DOCUMENT_BYTES)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppLearningOwnerDecisionV1 {
    Accept,
    Reject,
    Revoke,
}

/// Exact destination snapshot shown by the trusted desktop before it signs an
/// owner decision. The complete proposal is embedded so a UI cannot replace
/// the candidate id, decision, reason, snooze bound, source identity, handling
/// labels, or retention context while retaining the same friendly proposal id.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLearningOwnerReviewV1 {
    pub contract_version: u16,
    pub review_id: String,
    pub destination_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_receipt_digest: Option<String>,
    pub desktop_identity_key_id: String,
    pub desktop_identity_digest: String,
    pub proposal: AppLearningDecisionProposalV1,
    pub display_digest: String,
}

/// Keychain-signed, destination-head-bound owner decision. This is a closed
/// transport document, not generic signing authority. The destination stores
/// only `receipt_digest` in its projection receipt, while the caller may replay
/// this exact envelope after response loss.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLearningOwnerDecisionEnvelopeV1 {
    pub contract_version: u16,
    pub decision_id: String,
    pub review: AppLearningOwnerReviewV1,
    pub decision: AppLearningOwnerDecisionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_until_ms: Option<i64>,
    pub desktop_identity_signature_hex: String,
    pub receipt_digest: String,
}

impl AppLearningOwnerReviewV1 {
    pub fn mint(
        destination_generation: u64,
        destination_receipt_digest: Option<String>,
        desktop_identity_key_id: String,
        desktop_identity_digest: String,
        proposal: AppLearningDecisionProposalV1,
    ) -> Result<Self, AppContributionContractError> {
        let mut review = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            review_id: String::new(),
            destination_generation,
            destination_receipt_digest,
            desktop_identity_key_id,
            desktop_identity_digest,
            proposal,
            display_digest: String::new(),
        };
        review.review_id = domain_digest(
            "magician.learning-owner-review-id.v1",
            &(
                &review.proposal.header.scope_binding_ref,
                &review.proposal.header.proposal_id,
                &review.proposal.proposal_digest,
                review.destination_generation,
                &review.destination_receipt_digest,
                &review.desktop_identity_digest,
            ),
        )?;
        review.display_digest = review.expected_display_digest()?;
        review.validate()?;
        Ok(review)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_review.contract_version",
            ));
        }
        self.proposal.validate()?;
        validate_digest("owner_review.review_id", &self.review_id)?;
        validate_token(
            "owner_review.desktop_identity_key_id",
            &self.desktop_identity_key_id,
            MAX_IDENTITY_BYTES,
        )?;
        validate_digest(
            "owner_review.desktop_identity_digest",
            &self.desktop_identity_digest,
        )?;
        match (
            self.destination_generation,
            self.destination_receipt_digest.as_deref(),
        ) {
            (0, None) => {},
            (0, Some(_)) | (_, None) => {
                return Err(AppContributionContractError::InvalidField(
                    "owner_review.destination_receipt_digest",
                ));
            },
            (_, Some(digest)) => {
                validate_digest("owner_review.destination_receipt_digest", digest)?
            },
        }
        let expected_review_id = domain_digest(
            "magician.learning-owner-review-id.v1",
            &(
                &self.proposal.header.scope_binding_ref,
                &self.proposal.header.proposal_id,
                &self.proposal.proposal_digest,
                self.destination_generation,
                &self.destination_receipt_digest,
                &self.desktop_identity_digest,
            ),
        )?;
        if self.review_id != expected_review_id {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.review_id",
            ));
        }
        validate_digest("owner_review.display_digest", &self.display_digest)?;
        if self.display_digest != self.expected_display_digest()? {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_review.display_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_LEARNING_OWNER_REVIEW_MAX_BYTES)
    }

    fn expected_display_digest(&self) -> Result<String, AppContributionContractError> {
        let mut unsigned = self.clone();
        unsigned.display_digest.clear();
        domain_digest("magician.learning-owner-review-display.v1", &unsigned)
    }
}

impl AppLearningOwnerDecisionEnvelopeV1 {
    pub fn prepare(
        review: AppLearningOwnerReviewV1,
        decision: AppLearningOwnerDecisionV1,
        retained_until_ms: Option<i64>,
    ) -> Result<Self, AppContributionContractError> {
        review.validate()?;
        let mut envelope = Self {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            decision_id: String::new(),
            review,
            decision,
            retained_until_ms,
            desktop_identity_signature_hex: String::new(),
            receipt_digest: String::new(),
        };
        envelope.decision_id = domain_digest(
            "magician.learning-owner-decision-id.v1",
            &(
                &envelope.review.review_id,
                envelope.decision,
                envelope.retained_until_ms,
                &envelope.review.desktop_identity_digest,
            ),
        )?;
        envelope.validate_unsigned()?;
        Ok(envelope)
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppContributionContractError> {
        self.validate_unsigned()?;
        let mut unsigned = self.clone();
        unsigned.desktop_identity_signature_hex.clear();
        unsigned.receipt_digest.clear();
        serde_json::to_vec(&("magician.learning-owner-decision-signature.v1", unsigned))
            .map_err(|_| AppContributionContractError::Encoding)
    }

    pub fn with_signature_hex(
        mut self,
        signature_hex: String,
    ) -> Result<Self, AppContributionContractError> {
        self.desktop_identity_signature_hex = signature_hex;
        self.receipt_digest.clear();
        self.receipt_digest = domain_digest("magician.learning-owner-decision-receipt.v1", &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppContributionContractError> {
        self.validate_unsigned()?;
        validate_lower_hex(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
            64,
        )?;
        validate_digest("owner_decision.receipt_digest", &self.receipt_digest)?;
        let mut unsigned_receipt = self.clone();
        unsigned_receipt.receipt_digest.clear();
        if self.receipt_digest
            != domain_digest(
                "magician.learning-owner-decision-receipt.v1",
                &unsigned_receipt,
            )?
        {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.receipt_digest",
            ));
        }
        validate_encoded_ceiling(self, APP_LEARNING_OWNER_DECISION_MAX_BYTES)
    }

    pub fn verify_signature(
        &self,
        desktop_identity_public_key_hex: &str,
    ) -> Result<(), AppContributionContractError> {
        self.validate()?;
        let public_key = decode_lower_hex::<32>(
            "owner_decision.desktop_identity_public_key_hex",
            desktop_identity_public_key_hex,
        )?;
        let signature = decode_lower_hex::<64>(
            "owner_decision.desktop_identity_signature_hex",
            &self.desktop_identity_signature_hex,
        )?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
            .verify(&self.signing_bytes()?, &signature)
            .map_err(|_| AppContributionContractError::InvalidSignature)
    }

    fn validate_unsigned(&self) -> Result<(), AppContributionContractError> {
        if self.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.contract_version",
            ));
        }
        self.review.validate()?;
        validate_digest("owner_decision.decision_id", &self.decision_id)?;
        let expected_decision_id = domain_digest(
            "magician.learning-owner-decision-id.v1",
            &(
                &self.review.review_id,
                self.decision,
                self.retained_until_ms,
                &self.review.desktop_identity_digest,
            ),
        )?;
        if self.decision_id != expected_decision_id {
            return Err(AppContributionContractError::DigestMismatch(
                "owner_decision.decision_id",
            ));
        }
        let retention_is_valid = match (self.decision, self.retained_until_ms) {
            (AppLearningOwnerDecisionV1::Reject | AppLearningOwnerDecisionV1::Revoke, None) => true,
            (AppLearningOwnerDecisionV1::Accept, Some(expiry)) => {
                expiry > self.review.proposal.header.issued_at_ms
                    && expiry <= self.review.proposal.header.expires_at_ms
            },
            _ => false,
        };
        if !retention_is_valid {
            return Err(AppContributionContractError::InvalidField(
                "owner_decision.retained_until_ms",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppLearningInvalidationReasonV1 {
    SourceUpdated,
    SourceDeleted,
    SourceRestored,
    SourceForgotten,
    PolicyChanged,
    GrantRevoked,
    InstallationDisabled,
    InstallationQuarantined,
    InstallationUninstalledRetained,
    InstallationPurged,
    ContributionExpired,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLearningInvalidationV1 {
    pub contract_version: u16,
    pub invalidation_id: String,
    pub installation_id: String,
    pub scope_binding_ref: String,
    pub proposal_id: String,
    pub proposal_digest: String,
    pub source_event_ref: String,
    pub source_event_revision: u64,
    pub source_identity_digest: String,
    pub dedupe_key: String,
    pub reason: AppLearningInvalidationReasonV1,
    pub issued_at_ms: i64,
    pub invalidation_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppLearningIngressDispositionV1 {
    Staged,
    Duplicate,
    PolicyRejected,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLearningIngressReceiptV1 {
    pub contract_version: u16,
    pub receipt_id: String,
    pub proposal_id: String,
    pub proposal_digest: String,
    pub destination_generation: u64,
    pub destination_receipt_digest: String,
    pub disposition: AppLearningIngressDispositionV1,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppLearningDestinationOperationV1 {
    StageProposal {
        proposal: AppLearningDecisionProposalV1,
    },
    Decide {
        proposal_id: String,
        proposal_digest: String,
        decision: AppLearningOwnerDecisionV1,
        owner_decision_receipt_digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained_until_ms: Option<i64>,
    },
    Invalidate {
        invalidation: AppLearningInvalidationV1,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLearningDestinationReceiptV1 {
    pub contract_version: u16,
    pub receipt_id: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_receipt_digest: Option<String>,
    pub operation: AppLearningDestinationOperationV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation_disposition: Option<AppLearningInvalidationDispositionV1>,
    pub resulting_projection_digest: String,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppLearningInvalidationDispositionV1 {
    Tombstoned,
    AlreadyTombstoned,
    SupersededBeforeAdmission,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLearningInvalidationReceiptV1 {
    pub contract_version: u16,
    pub receipt_id: String,
    pub invalidation_id: String,
    pub invalidation_digest: String,
    pub proposal_id: String,
    pub destination_generation: u64,
    pub destination_receipt_digest: String,
    pub disposition: AppLearningInvalidationDispositionV1,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

impl_sealed_receipt!(
    AppLearningIngressReceiptV1,
    receipt_digest,
    "magician.learning-ingress-receipt.v1",
    validate_learning_ingress_receipt,
    APP_CONTRIBUTION_MAX_INGRESS_RECEIPT_BYTES
);
impl_sealed_receipt!(
    AppLearningInvalidationV1,
    invalidation_digest,
    "magician.learning-invalidation.v1",
    validate_learning_invalidation,
    APP_CONTRIBUTION_MAX_INVALIDATION_BYTES
);
impl_sealed_receipt!(
    AppLearningDestinationReceiptV1,
    receipt_digest,
    "magician.learning-destination-receipt.v1",
    validate_learning_destination_receipt,
    APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES
);
impl_sealed_receipt!(
    AppLearningInvalidationReceiptV1,
    receipt_digest,
    "magician.learning-invalidation-receipt.v1",
    validate_learning_invalidation_receipt,
    APP_CONTRIBUTION_MAX_INGRESS_RECEIPT_BYTES
);

fn validate_header(
    header: &AppContributionSourceHeaderV1,
    expected_contract: &str,
) -> Result<(), AppContributionContractError> {
    if header.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || header.destination_contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || header.destination_contract_id != expected_contract
        || header.proposal_revision == 0
        || header.installation_generation == 0
        || header.grant_revision == 0
        || header.schema_revision == 0
        || header.issued_at_ms < 0
        || header.expires_at_ms <= header.issued_at_ms
        || header.expires_at_ms - header.issued_at_ms > APP_CONTRIBUTION_MAX_TTL_MS
        || header.sources.is_empty()
        || header.sources.len() > APP_CONTRIBUTION_MAX_SOURCES
    {
        return Err(AppContributionContractError::InvalidField("header"));
    }
    for (name, value) in [
        (
            "destination_contract_id",
            header.destination_contract_id.as_str(),
        ),
        ("proposal_id", header.proposal_id.as_str()),
        ("scope_binding_ref", header.scope_binding_ref.as_str()),
        ("installation_id", header.installation_id.as_str()),
        ("package_revision_ref", header.package_revision_ref.as_str()),
        ("workflow_id", header.workflow_id.as_str()),
        ("action_id", header.action_id.as_str()),
        ("contribution_port_id", header.contribution_port_id.as_str()),
        ("dedupe_key", header.dedupe_key.as_str()),
    ] {
        validate_token(name, value, MAX_IDENTITY_BYTES)?;
    }
    validate_nonempty_text("purpose", &header.purpose, MAX_PURPOSE_BYTES)?;
    for (name, value) in [
        (
            "destination_schema_digest",
            header.destination_schema_digest.as_str(),
        ),
        (
            "package_content_digest",
            header.package_content_digest.as_str(),
        ),
        (
            "grant_authority_digest",
            header.grant_authority_digest.as_str(),
        ),
        ("schema_digest", header.schema_digest.as_str()),
        ("workflow_digest", header.workflow_digest.as_str()),
        ("action_digest", header.action_digest.as_str()),
        (
            "contribution_port_digest",
            header.contribution_port_digest.as_str(),
        ),
        (
            "policy_digest",
            header.handling_labels.policy_digest.as_str(),
        ),
        (
            "provenance_digest",
            header.handling_labels.provenance_digest.as_str(),
        ),
    ] {
        validate_digest(name, value)?;
    }
    validate_settlement(&header.settlement)?;
    validate_sorted_tokens(
        "audiences",
        &header.audiences,
        APP_CONTRIBUTION_MAX_AUDIENCES,
        MAX_IDENTITY_BYTES,
    )?;

    let mut aggregate_fields = 0usize;
    let mut previous_identity: Option<(&str, &str, &str)> = None;
    for source in &header.sources {
        validate_token(
            "source.installation_id",
            &source.installation_id,
            MAX_IDENTITY_BYTES,
        )?;
        validate_token(
            "source.entity_name",
            &source.entity_name,
            MAX_IDENTITY_BYTES,
        )?;
        validate_token("source.record_id", &source.record_id, MAX_IDENTITY_BYTES)?;
        validate_token(
            "source.canonical_source_ref",
            &source.canonical_source_ref,
            MAX_IDENTITY_BYTES,
        )?;
        validate_digest(
            "source.canonical_source_digest",
            &source.canonical_source_digest,
        )?;
        validate_digest(
            "source.policy_digest",
            &source.handling_labels.policy_digest,
        )?;
        validate_digest(
            "source.provenance_digest",
            &source.handling_labels.provenance_digest,
        )?;
        if source.installation_id != header.installation_id {
            return Err(AppContributionContractError::InvalidField(
                "source.installation_id",
            ));
        }
        if header.handling_labels.classification < source.handling_labels.classification
            || header.handling_labels.model_processing > source.handling_labels.model_processing
        {
            return Err(AppContributionContractError::InvalidField(
                "handling_labels",
            ));
        }
        if source.record_revision == 0 || source.selected_fields.is_empty() {
            return Err(AppContributionContractError::InvalidField("sources"));
        }
        validate_sorted_tokens(
            "source.selected_fields",
            &source.selected_fields,
            APP_CONTRIBUTION_MAX_SELECTED_FIELDS,
            MAX_FIELD_BYTES,
        )?;
        aggregate_fields = aggregate_fields.saturating_add(source.selected_fields.len());
        if aggregate_fields > APP_CONTRIBUTION_MAX_SELECTED_FIELDS {
            return Err(AppContributionContractError::InvalidField(
                "sources.selected_fields",
            ));
        }
        let identity = (
            source.installation_id.as_str(),
            source.entity_name.as_str(),
            source.record_id.as_str(),
        );
        if previous_identity.is_some_and(|previous| previous >= identity) {
            return Err(AppContributionContractError::InvalidField("sources"));
        }
        previous_identity = Some(identity);
    }
    Ok(())
}

fn validate_settlement(
    value: &AppContributionSettlementRefV1,
) -> Result<(), AppContributionContractError> {
    match value {
        AppContributionSettlementRefV1::Mutation {
            mutation_receipt_id,
            first_change_sequence,
            last_change_sequence,
        } => {
            validate_token(
                "mutation_receipt_id",
                mutation_receipt_id,
                MAX_IDENTITY_BYTES,
            )?;
            if *first_change_sequence == 0 || last_change_sequence < first_change_sequence {
                return Err(AppContributionContractError::InvalidField("settlement"));
            }
        },
        AppContributionSettlementRefV1::TypedResult {
            result_ref,
            output_revision,
            result_digest,
        } => {
            validate_token("result_ref", result_ref, MAX_IDENTITY_BYTES)?;
            validate_digest("result_digest", result_digest)?;
            if *output_revision == 0 {
                return Err(AppContributionContractError::InvalidField("settlement"));
            }
        },
    }
    Ok(())
}

fn validate_tier_scope(value: &AppMemoryTierScopeV1) -> Result<(), AppContributionContractError> {
    match value {
        AppMemoryTierScopeV1::User => Ok(()),
        AppMemoryTierScopeV1::Agent { agent_id } => {
            validate_token("agent_id", agent_id, MAX_IDENTITY_BYTES)
        },
        AppMemoryTierScopeV1::AgentGoal { agent_id, goal_id } => {
            validate_token("agent_id", agent_id, MAX_IDENTITY_BYTES)?;
            validate_token("goal_id", goal_id, MAX_IDENTITY_BYTES)
        },
    }
}

fn validate_ingress_receipt(
    value: &AppMemoryIngressReceiptV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.destination_generation == 0
        || value.recorded_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField(
            "ingress_receipt",
        ));
    }
    validate_token("receipt_id", &value.receipt_id, MAX_IDENTITY_BYTES)?;
    validate_token("proposal_id", &value.proposal_id, MAX_IDENTITY_BYTES)?;
    validate_digest("proposal_digest", &value.proposal_digest)?;
    validate_digest(
        "destination_receipt_digest",
        &value.destination_receipt_digest,
    )
}

fn validate_invalidation(
    value: &AppMemoryInvalidationV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.source_event_revision == 0
        || value.issued_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField("invalidation"));
    }
    validate_token(
        "invalidation_id",
        &value.invalidation_id,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token(
        "installation_id",
        &value.installation_id,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token(
        "scope_binding_ref",
        &value.scope_binding_ref,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token("proposal_id", &value.proposal_id, MAX_IDENTITY_BYTES)?;
    validate_token(
        "source_event_ref",
        &value.source_event_ref,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token("dedupe_key", &value.dedupe_key, MAX_IDENTITY_BYTES)?;
    validate_digest("proposal_digest", &value.proposal_digest)?;
    validate_digest("source_identity_digest", &value.source_identity_digest)
}

fn validate_destination_receipt(
    value: &AppMemoryDestinationReceiptV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.generation == 0
        || value.recorded_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField(
            "destination_receipt",
        ));
    }
    validate_token("receipt_id", &value.receipt_id, MAX_IDENTITY_BYTES)?;
    validate_digest(
        "resulting_projection_digest",
        &value.resulting_projection_digest,
    )?;
    match (value.generation, value.previous_receipt_digest.as_deref()) {
        (1, None) => {},
        (1, Some(_)) | (_, None) => {
            return Err(AppContributionContractError::InvalidField(
                "previous_receipt_digest",
            ))
        },
        (_, Some(digest)) => validate_digest("previous_receipt_digest", digest)?,
    }
    match &value.operation {
        AppMemoryDestinationOperationV1::StageProposal { proposal } => {
            if value.invalidation_disposition.is_some() {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation_disposition",
                ));
            }
            proposal.validate()?;
            if proposal.header.issued_at_ms > value.recorded_at_ms
                || proposal.header.expires_at_ms <= value.recorded_at_ms
            {
                return Err(AppContributionContractError::InvalidField(
                    "proposal_lifetime",
                ));
            }
        },
        AppMemoryDestinationOperationV1::Decide {
            proposal_id,
            proposal_digest,
            owner_decision_receipt_digest,
            retained_until_ms,
            ..
        } => {
            if value.invalidation_disposition.is_some() {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation_disposition",
                ));
            }
            validate_token("proposal_id", proposal_id, MAX_IDENTITY_BYTES)?;
            validate_digest("proposal_digest", proposal_digest)?;
            validate_digest(
                "owner_decision_receipt_digest",
                owner_decision_receipt_digest,
            )?;
            if retained_until_ms.is_some_and(|expiry| {
                expiry <= value.recorded_at_ms
                    || expiry - value.recorded_at_ms > APP_CONTRIBUTION_MAX_TTL_MS
            }) {
                return Err(AppContributionContractError::InvalidField(
                    "retained_until_ms",
                ));
            }
        },
        AppMemoryDestinationOperationV1::Invalidate { invalidation } => {
            if value.invalidation_disposition.is_none() {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation_disposition",
                ));
            }
            invalidation.validate()?;
            if invalidation.issued_at_ms > value.recorded_at_ms {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation.issued_at_ms",
                ));
            }
        },
    }
    Ok(())
}

fn validate_invalidation_receipt(
    value: &AppMemoryInvalidationReceiptV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.destination_generation == 0
        || value.recorded_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField(
            "invalidation_receipt",
        ));
    }
    validate_token("receipt_id", &value.receipt_id, MAX_IDENTITY_BYTES)?;
    validate_token(
        "invalidation_id",
        &value.invalidation_id,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token("proposal_id", &value.proposal_id, MAX_IDENTITY_BYTES)?;
    validate_digest("invalidation_digest", &value.invalidation_digest)?;
    validate_digest(
        "destination_receipt_digest",
        &value.destination_receipt_digest,
    )
}

fn validate_attention_ingress_receipt(
    value: &AppAttentionIngressReceiptV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.destination_generation == 0
        || value.recorded_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField(
            "ingress_receipt",
        ));
    }
    validate_token("receipt_id", &value.receipt_id, MAX_IDENTITY_BYTES)?;
    validate_token("proposal_id", &value.proposal_id, MAX_IDENTITY_BYTES)?;
    validate_digest("proposal_digest", &value.proposal_digest)?;
    validate_digest(
        "destination_receipt_digest",
        &value.destination_receipt_digest,
    )
}

fn validate_attention_invalidation(
    value: &AppAttentionInvalidationV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.source_event_revision == 0
        || value.issued_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField("invalidation"));
    }
    validate_token(
        "invalidation_id",
        &value.invalidation_id,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token(
        "installation_id",
        &value.installation_id,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token(
        "scope_binding_ref",
        &value.scope_binding_ref,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token("proposal_id", &value.proposal_id, MAX_IDENTITY_BYTES)?;
    validate_token(
        "source_event_ref",
        &value.source_event_ref,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token("dedupe_key", &value.dedupe_key, MAX_IDENTITY_BYTES)?;
    validate_digest("proposal_digest", &value.proposal_digest)?;
    validate_digest("source_identity_digest", &value.source_identity_digest)
}

fn validate_attention_destination_receipt(
    value: &AppAttentionDestinationReceiptV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.generation == 0
        || value.recorded_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField(
            "destination_receipt",
        ));
    }
    validate_token("receipt_id", &value.receipt_id, MAX_IDENTITY_BYTES)?;
    validate_digest(
        "resulting_projection_digest",
        &value.resulting_projection_digest,
    )?;
    match (value.generation, value.previous_receipt_digest.as_deref()) {
        (1, None) => {},
        (1, Some(_)) | (_, None) => {
            return Err(AppContributionContractError::InvalidField(
                "previous_receipt_digest",
            ))
        },
        (_, Some(digest)) => validate_digest("previous_receipt_digest", digest)?,
    }
    match &value.operation {
        AppAttentionDestinationOperationV1::StageProposal { proposal } => {
            if value.invalidation_disposition.is_some() {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation_disposition",
                ));
            }
            proposal.validate()?;
            if proposal.header.issued_at_ms > value.recorded_at_ms
                || proposal.header.expires_at_ms <= value.recorded_at_ms
            {
                return Err(AppContributionContractError::InvalidField(
                    "proposal_lifetime",
                ));
            }
        },
        AppAttentionDestinationOperationV1::Decide {
            proposal_id,
            proposal_digest,
            owner_decision_receipt_digest,
            retained_until_ms,
            ..
        } => {
            if value.invalidation_disposition.is_some() {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation_disposition",
                ));
            }
            validate_token("proposal_id", proposal_id, MAX_IDENTITY_BYTES)?;
            validate_digest("proposal_digest", proposal_digest)?;
            validate_digest(
                "owner_decision_receipt_digest",
                owner_decision_receipt_digest,
            )?;
            if retained_until_ms.is_some_and(|expiry| {
                expiry <= value.recorded_at_ms
                    || expiry - value.recorded_at_ms > APP_CONTRIBUTION_MAX_TTL_MS
            }) {
                return Err(AppContributionContractError::InvalidField(
                    "retained_until_ms",
                ));
            }
        },
        AppAttentionDestinationOperationV1::Invalidate { invalidation } => {
            if value.invalidation_disposition.is_none() {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation_disposition",
                ));
            }
            invalidation.validate()?;
            if invalidation.issued_at_ms > value.recorded_at_ms {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation.issued_at_ms",
                ));
            }
        },
    }
    Ok(())
}

fn validate_attention_invalidation_receipt(
    value: &AppAttentionInvalidationReceiptV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.destination_generation == 0
        || value.recorded_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField(
            "invalidation_receipt",
        ));
    }
    validate_token("receipt_id", &value.receipt_id, MAX_IDENTITY_BYTES)?;
    validate_token(
        "invalidation_id",
        &value.invalidation_id,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token("proposal_id", &value.proposal_id, MAX_IDENTITY_BYTES)?;
    validate_digest("invalidation_digest", &value.invalidation_digest)?;
    validate_digest(
        "destination_receipt_digest",
        &value.destination_receipt_digest,
    )
}

fn validate_learning_ingress_receipt(
    value: &AppLearningIngressReceiptV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.destination_generation == 0
        || value.recorded_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField(
            "ingress_receipt",
        ));
    }
    validate_token("receipt_id", &value.receipt_id, MAX_IDENTITY_BYTES)?;
    validate_token("proposal_id", &value.proposal_id, MAX_IDENTITY_BYTES)?;
    validate_digest("proposal_digest", &value.proposal_digest)?;
    validate_digest(
        "destination_receipt_digest",
        &value.destination_receipt_digest,
    )
}

fn validate_learning_invalidation(
    value: &AppLearningInvalidationV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.source_event_revision == 0
        || value.issued_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField("invalidation"));
    }
    validate_token(
        "invalidation_id",
        &value.invalidation_id,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token(
        "installation_id",
        &value.installation_id,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token(
        "scope_binding_ref",
        &value.scope_binding_ref,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token("proposal_id", &value.proposal_id, MAX_IDENTITY_BYTES)?;
    validate_token(
        "source_event_ref",
        &value.source_event_ref,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token("dedupe_key", &value.dedupe_key, MAX_IDENTITY_BYTES)?;
    validate_digest("proposal_digest", &value.proposal_digest)?;
    validate_digest("source_identity_digest", &value.source_identity_digest)
}

fn validate_learning_destination_receipt(
    value: &AppLearningDestinationReceiptV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.generation == 0
        || value.recorded_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField(
            "destination_receipt",
        ));
    }
    validate_token("receipt_id", &value.receipt_id, MAX_IDENTITY_BYTES)?;
    validate_digest(
        "resulting_projection_digest",
        &value.resulting_projection_digest,
    )?;
    match (value.generation, value.previous_receipt_digest.as_deref()) {
        (1, None) => {},
        (1, Some(_)) | (_, None) => {
            return Err(AppContributionContractError::InvalidField(
                "previous_receipt_digest",
            ))
        },
        (_, Some(digest)) => validate_digest("previous_receipt_digest", digest)?,
    }
    match &value.operation {
        AppLearningDestinationOperationV1::StageProposal { proposal } => {
            if value.invalidation_disposition.is_some() {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation_disposition",
                ));
            }
            proposal.validate()?;
            if proposal.header.issued_at_ms > value.recorded_at_ms
                || proposal.header.expires_at_ms <= value.recorded_at_ms
            {
                return Err(AppContributionContractError::InvalidField(
                    "proposal_lifetime",
                ));
            }
        },
        AppLearningDestinationOperationV1::Decide {
            proposal_id,
            proposal_digest,
            owner_decision_receipt_digest,
            retained_until_ms,
            ..
        } => {
            if value.invalidation_disposition.is_some() {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation_disposition",
                ));
            }
            validate_token("proposal_id", proposal_id, MAX_IDENTITY_BYTES)?;
            validate_digest("proposal_digest", proposal_digest)?;
            validate_digest(
                "owner_decision_receipt_digest",
                owner_decision_receipt_digest,
            )?;
            if retained_until_ms.is_some_and(|expiry| {
                expiry <= value.recorded_at_ms
                    || expiry - value.recorded_at_ms > APP_CONTRIBUTION_MAX_TTL_MS
            }) {
                return Err(AppContributionContractError::InvalidField(
                    "retained_until_ms",
                ));
            }
        },
        AppLearningDestinationOperationV1::Invalidate { invalidation } => {
            if value.invalidation_disposition.is_none() {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation_disposition",
                ));
            }
            invalidation.validate()?;
            if invalidation.issued_at_ms > value.recorded_at_ms {
                return Err(AppContributionContractError::InvalidField(
                    "invalidation.issued_at_ms",
                ));
            }
        },
    }
    Ok(())
}

fn validate_learning_invalidation_receipt(
    value: &AppLearningInvalidationReceiptV1,
) -> Result<(), AppContributionContractError> {
    if value.contract_version != APP_CONTRIBUTION_CONTRACT_VERSION
        || value.destination_generation == 0
        || value.recorded_at_ms < 0
    {
        return Err(AppContributionContractError::InvalidField(
            "invalidation_receipt",
        ));
    }
    validate_token("receipt_id", &value.receipt_id, MAX_IDENTITY_BYTES)?;
    validate_token(
        "invalidation_id",
        &value.invalidation_id,
        MAX_IDENTITY_BYTES,
    )?;
    validate_token("proposal_id", &value.proposal_id, MAX_IDENTITY_BYTES)?;
    validate_digest("invalidation_digest", &value.invalidation_digest)?;
    validate_digest(
        "destination_receipt_digest",
        &value.destination_receipt_digest,
    )
}

fn validate_sorted_tokens(
    field: &'static str,
    values: &[String],
    maximum_items: usize,
    maximum_bytes: usize,
) -> Result<(), AppContributionContractError> {
    if values.len() > maximum_items || !values.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(AppContributionContractError::InvalidField(field));
    }
    for value in values {
        validate_token(field, value, maximum_bytes)?;
    }
    Ok(())
}

fn validate_token(
    field: &'static str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), AppContributionContractError> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(AppContributionContractError::InvalidField(field));
    }
    Ok(())
}

fn validate_nonempty_text(
    field: &'static str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), AppContributionContractError> {
    if value.trim().is_empty() || value.len() > maximum_bytes || value.chars().any(char::is_control)
    {
        return Err(AppContributionContractError::InvalidField(field));
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), AppContributionContractError> {
    let Some(hex) = value.strip_prefix(DIGEST_PREFIX) else {
        return Err(AppContributionContractError::InvalidField(field));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(AppContributionContractError::InvalidField(field));
    }
    Ok(())
}

fn validate_lower_hex(
    field: &'static str,
    value: &str,
    byte_len: usize,
) -> Result<(), AppContributionContractError> {
    if value.len() != byte_len.saturating_mul(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(AppContributionContractError::InvalidField(field));
    }
    Ok(())
}

fn decode_lower_hex<const N: usize>(
    field: &'static str,
    value: &str,
) -> Result<[u8; N], AppContributionContractError> {
    validate_lower_hex(field, value, N)?;
    let mut bytes = [0_u8; N];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| AppContributionContractError::InvalidField(field))?;
    }
    if bytes.iter().all(|byte| *byte == 0) {
        return Err(AppContributionContractError::InvalidField(field));
    }
    Ok(bytes)
}

fn validate_encoded_ceiling<T: Serialize>(
    value: &T,
    ceiling: usize,
) -> Result<(), AppContributionContractError> {
    let encoded = serde_json::to_vec(value).map_err(|_| AppContributionContractError::Encoding)?;
    if encoded.len() > ceiling {
        return Err(AppContributionContractError::DocumentTooLarge);
    }
    Ok(())
}

fn domain_digest<T: Serialize>(
    domain: &str,
    value: &T,
) -> Result<String, AppContributionContractError> {
    let bytes = serde_json::to_vec(value).map_err(|_| AppContributionContractError::Encoding)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(&bytes);
    Ok(format!("{DIGEST_PREFIX}{}", hasher.finalize().to_hex()))
}

pub fn content_digest(bytes: &[u8]) -> String {
    format!("{DIGEST_PREFIX}{}", blake3::hash(bytes).to_hex())
}

/// Stable digest of the closed claims/commitments destination command schema.
/// It is destination-owned, not copied from an app proposal or package.
pub fn app_claims_decision_destination_schema_digest() -> String {
    content_digest(APP_CLAIMS_DECISION_DESTINATION_SCHEMA_SEED)
}

/// Stable digest of the closed meeting-control destination command schema.
/// Destination-owned for the same reason as its claims sibling.
pub fn app_meeting_control_destination_schema_digest() -> String {
    content_digest(APP_MEETING_CONTROL_DESTINATION_SCHEMA_SEED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_join_urls_admit_real_meeting_links_and_refuse_credentialed_detours() {
        // A signed join navigates the owner's PERSISTENT, SIGNED-IN browser
        // profile, so this validator is the difference between a meeting link
        // and a credentialed request to somewhere else.
        for admitted in [
            "https://meet.google.com/abc-defg-hij",
            "https://us02web.zoom.us/j/1234567890?pwd=abc",
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting",
            "https://acme.webex.com/meet/priya",
            "https://my_org.zoom.us/j/99",
            "HTTPS://meet.google.com/abc-defg-hij",
            "https://meet.google.com./abc-defg-hij",
            "https://meet.google.com",
        ] {
            assert!(
                validate_meeting_url(admitted).is_ok(),
                "`{admitted}` is an ordinary meeting link and must be admitted"
            );
        }
        for refused in [
            // Plaintext would carry the profile's session over the wire.
            "http://meet.google.com/abc",
            // Userinfo reads as the meeting host and resolves elsewhere.
            "https://meet.google.com@evil.example/abc",
            // Loopback and private-network names.
            "https://localhost/abc",
            "https://anything.localhost/abc",
            "https://printer.local/abc",
            "https://wiki.internal/abc",
            "https://box.lan/abc",
            // Literals and ports are never public meeting links.
            "https://127.0.0.1/abc",
            "https://10.0.0.5/abc",
            "https://[::1]/abc",
            "https://meet.google.com:8443/abc",
            // Malformed authorities.
            "https:///abc",
            "https://.google.com/abc",
            "https://meet..google.com/abc",
            "https://meet\\google.com/abc",
            "ftp://meet.google.com/abc",
            "meet.google.com/abc",
        ] {
            assert!(
                validate_meeting_url(refused).is_err(),
                "`{refused}` must not be an admissible signed join target"
            );
        }
    }

    #[test]
    fn a_meeting_gesture_window_must_be_narrow_and_recent() {
        let gesture = AppMeetingControlGestureV1 {
            surface_session_id: "surface-1".to_owned(),
            gesture_id: "gesture-1".to_owned(),
            observed_at_ms: 1_000,
            expires_at_ms: 1_000 + APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS,
        };
        gesture
            .validate()
            .expect("a ceiling-wide window is admissible");
        assert!(gesture.is_fresh_at(1_000));
        assert!(gesture.is_fresh_at(gesture.expires_at_ms - 1));
        assert!(!gesture.is_fresh_at(gesture.expires_at_ms));
        assert!(!gesture.is_fresh_at(999));

        let too_wide = AppMeetingControlGestureV1 {
            expires_at_ms: 1_000 + APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS + 1,
            ..gesture.clone()
        };
        assert!(
            too_wide.validate().is_err(),
            "a window wider than the ceiling is not a recent act"
        );

        let inverted = AppMeetingControlGestureV1 {
            expires_at_ms: gesture.observed_at_ms,
            ..gesture
        };
        assert!(inverted.validate().is_err());
    }

    fn digest(label: &str) -> String {
        content_digest(label.as_bytes())
    }

    fn header(contract: &str) -> AppContributionSourceHeaderV1 {
        AppContributionSourceHeaderV1 {
            contract_version: 1,
            destination_contract_id: contract.to_owned(),
            destination_contract_version: 1,
            destination_schema_digest: digest("destination-schema"),
            proposal_id: "memory-proposal:1".to_owned(),
            proposal_revision: 1,
            scope_binding_ref: "scope_binding:1".to_owned(),
            installation_id: "installation:1".to_owned(),
            installation_generation: 2,
            package_revision_ref: "package:1".to_owned(),
            package_content_digest: digest("package"),
            grant_revision: 3,
            grant_authority_digest: digest("grant"),
            schema_revision: 4,
            schema_digest: digest("schema"),
            workflow_id: "workflow:remember".to_owned(),
            workflow_digest: digest("workflow"),
            action_id: "action:remember".to_owned(),
            action_digest: digest("action"),
            contribution_port_id: "memory_candidate".to_owned(),
            contribution_port_digest: digest("port"),
            settlement: AppContributionSettlementRefV1::Mutation {
                mutation_receipt_id: "mutation:1".to_owned(),
                first_change_sequence: 7,
                last_change_sequence: 7,
            },
            sources: vec![AppContributionSourceRefV1 {
                installation_id: "installation:1".to_owned(),
                entity_name: "note".to_owned(),
                record_id: "record:1".to_owned(),
                record_revision: 1,
                selected_fields: vec!["summary".to_owned()],
                canonical_source_ref: "source:record:1".to_owned(),
                canonical_source_digest: digest("source"),
                handling_labels: AppContributionHandlingLabelsV1 {
                    classification: AppContributionClassification::Personal,
                    model_processing: AppContributionModelProcessing::LocalOnly,
                    policy_digest: digest("source-policy"),
                    provenance_digest: digest("source-provenance"),
                },
            }],
            handling_labels: AppContributionHandlingLabelsV1 {
                classification: AppContributionClassification::Personal,
                model_processing: AppContributionModelProcessing::LocalOnly,
                policy_digest: digest("policy"),
                provenance_digest: digest("provenance"),
            },
            purpose: "remember-user-approved-summary".to_owned(),
            audiences: vec!["personal-agent".to_owned()],
            evidence_class: AppContributionEvidenceClass::Hypothesis,
            issued_at_ms: 1_000,
            expires_at_ms: 2_000,
            dedupe_key: "dedupe:1".to_owned(),
            update_policy: AppContributionUpdatePolicy::NewProposalRevision,
            retraction_policy: AppContributionRetractionPolicy::TombstoneOnAnySourceDrift,
        }
    }

    fn proposal() -> AppMemoryCandidateProposalV1 {
        let claim = "A bounded hypothesis".to_owned();
        AppMemoryCandidateProposalV1 {
            header: header(APP_MEMORY_CANDIDATE_CONTRACT_ID),
            intended_tier_scope: AppMemoryTierScopeV1::User,
            semantic_destination: AppMemorySemanticDestinationV1::Knowledge,
            claim_digest: content_digest(claim.as_bytes()),
            claim_or_summary: claim,
            evidence_refs: vec!["source:record:1".to_owned()],
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed proposal")
    }

    #[test]
    fn memory_proposal_rejects_substitution_and_unknown_fields() {
        let proposal = proposal();
        proposal.validate().expect("valid proposal");
        let mut substituted = proposal.clone();
        substituted.claim_or_summary.push('!');
        assert!(matches!(
            substituted.validate(),
            Err(AppContributionContractError::DigestMismatch(_))
        ));
        let mut value = serde_json::to_value(&proposal).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("rank".to_owned(), serde_json::json!(1));
        assert!(serde_json::from_value::<AppMemoryCandidateProposalV1>(value).is_err());
    }

    #[test]
    fn memory_proposal_has_no_destination_ranking_fields() {
        let encoded = serde_json::to_string(&proposal()).unwrap();
        for denied in ["confidence", "heat", "salience", "rank", "temperature"] {
            assert!(!encoded.contains(denied));
        }
    }

    #[test]
    fn aggregate_handling_cannot_weaken_any_source() {
        let mut classification_substitution = proposal();
        classification_substitution
            .header
            .handling_labels
            .classification = AppContributionClassification::Public;
        classification_substitution.proposal_digest.clear();
        assert!(classification_substitution.seal().is_err());

        let mut processing_substitution = proposal();
        processing_substitution
            .header
            .handling_labels
            .model_processing = AppContributionModelProcessing::RemoteAllowed;
        processing_substitution.proposal_digest.clear();
        assert!(processing_substitution.seal().is_err());
    }

    #[test]
    fn owner_decision_is_head_identity_and_retention_bound() {
        let review = AppMemoryOwnerReviewV1::mint(
            0,
            None,
            "desktop-key:1".to_owned(),
            digest("desktop-identity"),
            proposal(),
        )
        .expect("review");
        let accepted = AppMemoryOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppMemoryOwnerDecisionV1::Accept,
            Some(review.proposal.header.expires_at_ms),
        )
        .expect("bounded accept");
        assert!(!accepted.signing_bytes().expect("signing bytes").is_empty());
        assert!(AppMemoryOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppMemoryOwnerDecisionV1::Accept,
            None,
        )
        .is_err());
        assert!(AppMemoryOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppMemoryOwnerDecisionV1::Accept,
            Some(review.proposal.header.expires_at_ms + 1),
        )
        .is_err());
        assert!(AppMemoryOwnerDecisionEnvelopeV1::prepare(
            review,
            AppMemoryOwnerDecisionV1::Reject,
            Some(1_500),
        )
        .is_err());
        let review = AppMemoryOwnerReviewV1::mint(
            1,
            Some(digest("accepted-head")),
            "desktop-key:1".to_owned(),
            digest("desktop-identity"),
            proposal(),
        )
        .expect("accepted review");
        assert!(AppMemoryOwnerDecisionEnvelopeV1::prepare(
            review,
            AppMemoryOwnerDecisionV1::Revoke,
            None,
        )
        .is_ok());
    }

    #[test]
    fn retrieval_remains_a_distinct_closed_body() {
        let projection = "short-lived projection".to_owned();
        let proposal = AppPersonalAgentRetrievalProjectionProposalV1 {
            header: header(APP_RETRIEVAL_PROJECTION_CONTRACT_ID),
            target_agent_id: "personal-assistant".to_owned(),
            target_goal_id: None,
            projection_digest: content_digest(projection.as_bytes()),
            projection_text: projection,
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed projection");
        proposal.validate().expect("valid projection");
        assert!(serde_json::from_value::<AppMemoryCandidateProposalV1>(
            serde_json::to_value(proposal).unwrap()
        )
        .is_err());
    }

    #[test]
    fn destination_receipt_ceiling_admits_a_large_sealed_proposal_envelope() {
        let mut large = proposal();
        large.claim_or_summary = "x".repeat(APP_CONTRIBUTION_MAX_CLAIM_BYTES);
        large.claim_digest = content_digest(large.claim_or_summary.as_bytes());
        large.header.sources = (0..APP_CONTRIBUTION_MAX_SOURCES)
            .map(|index| {
                let suffix = format!("{index:02}");
                AppContributionSourceRefV1 {
                    installation_id: large.header.installation_id.clone(),
                    entity_name: "note".to_owned(),
                    record_id: format!("record:{suffix}"),
                    record_revision: 1,
                    selected_fields: vec![
                        format!("a{suffix}{}", "a".repeat(500 - suffix.len())),
                        format!("b{suffix}{}", "b".repeat(500 - suffix.len())),
                    ],
                    canonical_source_ref: format!("source:record:{suffix}"),
                    canonical_source_digest: digest(&format!("source:{suffix}")),
                    handling_labels: AppContributionHandlingLabelsV1 {
                        classification: AppContributionClassification::Personal,
                        model_processing: AppContributionModelProcessing::LocalOnly,
                        policy_digest: digest("source-policy"),
                        provenance_digest: digest("source-provenance"),
                    },
                }
            })
            .collect();
        large.evidence_refs = vec!["source:record:00".to_owned()];
        large.proposal_digest.clear();
        let large = large
            .seal()
            .expect("large proposal remains within its document ceiling");
        assert!(
            serde_json::to_vec(&large).unwrap().len() > APP_CONTRIBUTION_MAX_INGRESS_RECEIPT_BYTES
        );

        let receipt = AppMemoryDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "destination-receipt:1".to_owned(),
            generation: 1,
            previous_receipt_digest: None,
            operation: AppMemoryDestinationOperationV1::StageProposal { proposal: large },
            invalidation_disposition: None,
            resulting_projection_digest: digest("projection"),
            recorded_at_ms: 1_500,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("destination receipt admits its bounded embedded proposal");
        assert!(
            serde_json::to_vec(&receipt).unwrap().len()
                <= APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES
        );
    }
}

#[cfg(test)]
mod attention_tests {
    use super::*;

    fn digest(label: &str) -> String {
        content_digest(label.as_bytes())
    }

    fn header() -> AppContributionSourceHeaderV1 {
        AppContributionSourceHeaderV1 {
            contract_version: 1,
            destination_contract_id: APP_ATTENTION_CANDIDATE_CONTRACT_ID.to_owned(),
            destination_contract_version: 1,
            destination_schema_digest: digest("attention-destination-schema"),
            proposal_id: "attention-proposal:1".to_owned(),
            proposal_revision: 1,
            scope_binding_ref: "scope_binding:1".to_owned(),
            installation_id: "installation:1".to_owned(),
            installation_generation: 2,
            package_revision_ref: "package:1".to_owned(),
            package_content_digest: digest("package"),
            grant_revision: 3,
            grant_authority_digest: digest("grant"),
            schema_revision: 4,
            schema_digest: digest("schema"),
            workflow_id: "workflow:surface_card".to_owned(),
            workflow_digest: digest("workflow"),
            action_id: "action:surface_card".to_owned(),
            action_digest: digest("action"),
            contribution_port_id: "attention_candidate".to_owned(),
            contribution_port_digest: digest("attention-port"),
            settlement: AppContributionSettlementRefV1::Mutation {
                mutation_receipt_id: "mutation:1".to_owned(),
                first_change_sequence: 7,
                last_change_sequence: 7,
            },
            sources: vec![AppContributionSourceRefV1 {
                installation_id: "installation:1".to_owned(),
                entity_name: "card".to_owned(),
                record_id: "record:1".to_owned(),
                record_revision: 1,
                selected_fields: vec!["title".to_owned()],
                canonical_source_ref: "source:record:1".to_owned(),
                canonical_source_digest: digest("source"),
                handling_labels: AppContributionHandlingLabelsV1 {
                    classification: AppContributionClassification::Personal,
                    model_processing: AppContributionModelProcessing::LocalOnly,
                    policy_digest: digest("source-policy"),
                    provenance_digest: digest("source-provenance"),
                },
            }],
            handling_labels: AppContributionHandlingLabelsV1 {
                classification: AppContributionClassification::Personal,
                model_processing: AppContributionModelProcessing::LocalOnly,
                policy_digest: digest("policy"),
                provenance_digest: digest("provenance"),
            },
            purpose: "surface-owner-reviewed-lane-card".to_owned(),
            audiences: vec!["personal-agent".to_owned()],
            evidence_class: AppContributionEvidenceClass::Hypothesis,
            issued_at_ms: 1_000,
            expires_at_ms: 2_000,
            dedupe_key: "dedupe:1".to_owned(),
            update_policy: AppContributionUpdatePolicy::ReplaceExactSourceHead,
            retraction_policy: AppContributionRetractionPolicy::TombstoneOnAnySourceDrift,
        }
    }

    fn proposal() -> AppAttentionCandidateProposalV1 {
        let title = "A bounded lane card".to_owned();
        AppAttentionCandidateProposalV1 {
            header: header(),
            lane: AppAttentionLaneV1::WorthALook,
            priority: AppAttentionPriorityV1::Normal,
            title_digest: content_digest(title.as_bytes()),
            title,
            summary: Some("One reviewed summary sentence.".to_owned()),
            primary_source_ref: "source:record:1".to_owned(),
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed attention proposal")
    }

    #[test]
    fn attention_proposal_rejects_substitution_and_unknown_fields() {
        let proposal = proposal();
        proposal.validate().expect("valid attention proposal");
        let mut substituted = proposal.clone();
        substituted.title.push('!');
        assert!(matches!(
            substituted.validate(),
            Err(AppContributionContractError::DigestMismatch(_))
        ));
        let mut substituted_lane = proposal.clone();
        substituted_lane.lane = AppAttentionLaneV1::NeedsYou;
        assert!(matches!(
            substituted_lane.validate(),
            Err(AppContributionContractError::DigestMismatch(_))
        ));
        let mut value = serde_json::to_value(&proposal).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("rank".to_owned(), serde_json::json!(1));
        assert!(serde_json::from_value::<AppAttentionCandidateProposalV1>(value).is_err());
    }

    #[test]
    fn attention_proposal_has_no_destination_ranking_authority() {
        let encoded = serde_json::to_string(&proposal()).unwrap();
        for denied in [
            "confidence",
            "heat",
            "salience",
            "rank",
            "temperature",
            "score",
            "weight",
            "order",
        ] {
            assert!(!encoded.contains(denied));
        }
        assert!(encoded.contains("\"priority\":\"normal\""));
        assert!(encoded.contains("\"lane\":\"worth_a_look\""));
    }

    #[test]
    fn attention_proposal_source_link_and_text_stay_bounded() {
        let mut undeclared_source = proposal();
        undeclared_source.primary_source_ref = "source:record:99".to_owned();
        assert!(matches!(
            undeclared_source.seal(),
            Err(AppContributionContractError::InvalidField(
                "primary_source_ref"
            ))
        ));

        let mut control_character_summary = proposal();
        control_character_summary.summary = Some("bad \u{7} summary".to_owned());
        control_character_summary.proposal_digest.clear();
        assert!(control_character_summary.seal().is_err());

        let mut oversized_title = proposal();
        oversized_title.title = "x".repeat(APP_ATTENTION_MAX_TITLE_BYTES + 1);
        oversized_title.title_digest = content_digest(oversized_title.title.as_bytes());
        oversized_title.proposal_digest.clear();
        assert!(oversized_title.seal().is_err());

        let mut wrong_destination = proposal();
        wrong_destination.header.destination_contract_id =
            APP_MEMORY_CANDIDATE_CONTRACT_ID.to_owned();
        wrong_destination.proposal_digest.clear();
        assert!(matches!(
            wrong_destination.seal(),
            Err(AppContributionContractError::InvalidField("header"))
        ));
    }

    #[test]
    fn attention_owner_decision_is_head_identity_and_retention_bound() {
        let review = AppAttentionOwnerReviewV1::mint(
            0,
            None,
            "desktop-key:1".to_owned(),
            digest("desktop-identity"),
            proposal(),
        )
        .expect("attention review");
        let accepted = AppAttentionOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppAttentionOwnerDecisionV1::Accept,
            Some(review.proposal.header.expires_at_ms),
        )
        .expect("bounded attention accept");
        assert!(!accepted
            .signing_bytes()
            .expect("attention signing bytes")
            .is_empty());
        assert!(AppAttentionOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppAttentionOwnerDecisionV1::Accept,
            None,
        )
        .is_err());
        assert!(AppAttentionOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppAttentionOwnerDecisionV1::Accept,
            Some(review.proposal.header.expires_at_ms + 1),
        )
        .is_err());
        assert!(AppAttentionOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppAttentionOwnerDecisionV1::Reject,
            Some(1_500),
        )
        .is_err());
        let review = AppAttentionOwnerReviewV1::mint(
            1,
            Some(digest("accepted-attention-head")),
            "desktop-key:1".to_owned(),
            digest("desktop-identity"),
            proposal(),
        )
        .expect("accepted attention review");
        assert!(AppAttentionOwnerDecisionEnvelopeV1::prepare(
            review,
            AppAttentionOwnerDecisionV1::Revoke,
            None,
        )
        .is_ok());
    }

    #[test]
    fn attention_port_stays_a_distinct_closed_body() {
        let attention = proposal();
        assert!(serde_json::from_value::<AppMemoryCandidateProposalV1>(
            serde_json::to_value(&attention).unwrap()
        )
        .is_err());
        let mut memory_shaped = serde_json::to_value(&attention).unwrap();
        memory_shaped
            .as_object_mut()
            .unwrap()
            .insert("claim_or_summary".to_owned(), serde_json::json!("x"));
        assert!(serde_json::from_value::<AppMemoryCandidateProposalV1>(memory_shaped).is_err());
        let mut unknown_lane = serde_json::to_value(&attention).unwrap();
        unknown_lane["lane"] = serde_json::json!("top_secret");
        assert!(serde_json::from_value::<AppAttentionCandidateProposalV1>(unknown_lane).is_err());
        let mut unknown_priority = serde_json::to_value(&attention).unwrap();
        unknown_priority["priority"] = serde_json::json!(9);
        assert!(
            serde_json::from_value::<AppAttentionCandidateProposalV1>(unknown_priority).is_err()
        );
    }

    #[test]
    fn attention_receipt_domains_stay_separated_from_memory() {
        let attention_receipt = AppAttentionIngressReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "receipt:shared".to_owned(),
            proposal_id: "attention-proposal:1".to_owned(),
            proposal_digest: digest("shared-proposal"),
            destination_generation: 1,
            destination_receipt_digest: digest("shared-destination"),
            disposition: AppAttentionIngressDispositionV1::Staged,
            recorded_at_ms: 1_500,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("sealed attention ingress receipt");
        let memory_shaped = serde_json::to_value(&attention_receipt).unwrap();
        assert!(serde_json::from_value::<AppMemoryIngressReceiptV1>(memory_shaped).is_ok());
        let memory_receipt = AppMemoryIngressReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "receipt:shared".to_owned(),
            proposal_id: "attention-proposal:1".to_owned(),
            proposal_digest: digest("shared-proposal"),
            destination_generation: 1,
            destination_receipt_digest: digest("shared-destination"),
            disposition: AppMemoryIngressDispositionV1::Staged,
            recorded_at_ms: 1_500,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("sealed memory ingress receipt");
        assert_ne!(
            attention_receipt.receipt_digest, memory_receipt.receipt_digest,
            "identical bytes must still seal to different domain-separated digests"
        );
        attention_receipt.validate().expect("still valid");
        memory_receipt.validate().expect("still valid");
    }

    #[test]
    fn attention_destination_receipt_admits_sealed_operations() {
        let staged = AppAttentionDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "attention-destination-receipt:1".to_owned(),
            generation: 1,
            previous_receipt_digest: None,
            operation: AppAttentionDestinationOperationV1::StageProposal {
                proposal: proposal(),
            },
            invalidation_disposition: None,
            resulting_projection_digest: digest("attention-projection"),
            recorded_at_ms: 1_500,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("sealed attention destination receipt");
        staged
            .validate()
            .expect("valid attention destination receipt");

        let mut expired_stage = staged.clone();
        expired_stage.recorded_at_ms = 3_000;
        expired_stage.receipt_digest.clear();
        assert!(expired_stage.seal().is_err());

        let invalidation = AppAttentionInvalidationV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            invalidation_id: "attention-invalidation:1".to_owned(),
            installation_id: "installation:1".to_owned(),
            scope_binding_ref: "scope_binding:1".to_owned(),
            proposal_id: "attention-proposal:1".to_owned(),
            proposal_digest: digest("attention-proposal"),
            source_event_ref: "source:record:1".to_owned(),
            source_event_revision: 2,
            source_identity_digest: digest("attention-source"),
            dedupe_key: "dedupe:1".to_owned(),
            reason: AppAttentionInvalidationReasonV1::SourceUpdated,
            issued_at_ms: 1_600,
            invalidation_digest: String::new(),
        }
        .seal()
        .expect("sealed attention invalidation");
        let mut invalidating = AppAttentionDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "attention-destination-receipt:2".to_owned(),
            generation: 2,
            previous_receipt_digest: Some(staged.receipt_digest.clone()),
            operation: AppAttentionDestinationOperationV1::Invalidate {
                invalidation: invalidation.clone(),
            },
            invalidation_disposition: None,
            resulting_projection_digest: digest("attention-projection-2"),
            recorded_at_ms: 1_700,
            receipt_digest: String::new(),
        };
        assert!(invalidating.clone().seal().is_err());
        invalidating.invalidation_disposition =
            Some(AppAttentionInvalidationDispositionV1::Tombstoned);
        let invalidating = invalidating
            .seal()
            .expect("sealed attention invalidation receipt");
        invalidating.validate().expect("valid invalidation receipt");

        let decided = AppAttentionDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "attention-destination-receipt:3".to_owned(),
            generation: 3,
            previous_receipt_digest: Some(invalidating.receipt_digest.clone()),
            operation: AppAttentionDestinationOperationV1::Decide {
                proposal_id: "attention-proposal:1".to_owned(),
                proposal_digest: digest("attention-proposal"),
                decision: AppAttentionOwnerDecisionV1::Reject,
                owner_decision_receipt_digest: digest("owner-decision"),
                retained_until_ms: None,
            },
            invalidation_disposition: None,
            resulting_projection_digest: digest("attention-projection-3"),
            recorded_at_ms: 1_800,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("sealed attention decision receipt");
        decided
            .validate()
            .expect("valid attention decision receipt");
    }
}

#[cfg(test)]
mod learning_tests {
    use super::*;

    fn digest(label: &str) -> String {
        content_digest(label.as_bytes())
    }

    fn header() -> AppContributionSourceHeaderV1 {
        AppContributionSourceHeaderV1 {
            contract_version: 1,
            destination_contract_id: APP_LEARNING_DECISION_CONTRACT_ID.to_owned(),
            destination_contract_version: 1,
            destination_schema_digest: digest("learning-destination-schema"),
            proposal_id: "learning-proposal:1".to_owned(),
            proposal_revision: 1,
            scope_binding_ref: "scope_binding:1".to_owned(),
            installation_id: "installation:1".to_owned(),
            installation_generation: 2,
            package_revision_ref: "package:1".to_owned(),
            package_content_digest: digest("package"),
            grant_revision: 3,
            grant_authority_digest: digest("grant"),
            schema_revision: 4,
            schema_digest: digest("schema"),
            workflow_id: "workflow:approve_candidate".to_owned(),
            workflow_digest: digest("workflow"),
            action_id: "action:approve_candidate".to_owned(),
            action_digest: digest("action"),
            contribution_port_id: "learning_decision".to_owned(),
            contribution_port_digest: digest("learning-port"),
            settlement: AppContributionSettlementRefV1::Mutation {
                mutation_receipt_id: "mutation:1".to_owned(),
                first_change_sequence: 7,
                last_change_sequence: 7,
            },
            sources: vec![AppContributionSourceRefV1 {
                installation_id: "installation:1".to_owned(),
                entity_name: "review_decision".to_owned(),
                record_id: "record:1".to_owned(),
                record_revision: 1,
                selected_fields: vec!["decision".to_owned()],
                canonical_source_ref: "source:record:1".to_owned(),
                canonical_source_digest: digest("source"),
                handling_labels: AppContributionHandlingLabelsV1 {
                    classification: AppContributionClassification::Personal,
                    model_processing: AppContributionModelProcessing::LocalOnly,
                    policy_digest: digest("source-policy"),
                    provenance_digest: digest("source-provenance"),
                },
            }],
            handling_labels: AppContributionHandlingLabelsV1 {
                classification: AppContributionClassification::Personal,
                model_processing: AppContributionModelProcessing::LocalOnly,
                policy_digest: digest("policy"),
                provenance_digest: digest("provenance"),
            },
            purpose: "apply-owner-reviewed-learning-decision".to_owned(),
            audiences: vec!["learning-reviewer".to_owned()],
            evidence_class: AppContributionEvidenceClass::Hypothesis,
            issued_at_ms: 1_000,
            expires_at_ms: 2_000,
            dedupe_key: "dedupe:1".to_owned(),
            update_policy: AppContributionUpdatePolicy::ReplaceExactSourceHead,
            retraction_policy: AppContributionRetractionPolicy::TombstoneOnAnySourceDrift,
        }
    }

    fn proposal(decision: AppLearningDecisionKindV1) -> AppLearningDecisionProposalV1 {
        let reason = "Approve after reviewing the evidence.".to_owned();
        AppLearningDecisionProposalV1 {
            header: header(),
            candidate_id: "lc_test_candidate".to_owned(),
            decision,
            reason_digest: content_digest(reason.as_bytes()),
            reason,
            snooze_until_ms: matches!(decision, AppLearningDecisionKindV1::Snooze).then_some(1_500),
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed learning proposal")
    }

    #[test]
    fn learning_proposal_rejects_substitution_and_unknown_fields() {
        let proposal = proposal(AppLearningDecisionKindV1::Approve);
        proposal.validate().expect("valid learning proposal");
        let mut substituted = proposal.clone();
        substituted.reason.push('!');
        assert!(matches!(
            substituted.validate(),
            Err(AppContributionContractError::DigestMismatch(_))
        ));
        let mut substituted_decision = proposal.clone();
        substituted_decision.decision = AppLearningDecisionKindV1::Reject;
        assert!(matches!(
            substituted_decision.validate(),
            Err(AppContributionContractError::DigestMismatch(_))
        ));
        let mut substituted_candidate = proposal.clone();
        substituted_candidate.candidate_id = "lc_other_candidate".to_owned();
        assert!(matches!(
            substituted_candidate.validate(),
            Err(AppContributionContractError::DigestMismatch(_))
        ));
        let mut value = serde_json::to_value(&proposal).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("rank".to_owned(), serde_json::json!(1));
        assert!(serde_json::from_value::<AppLearningDecisionProposalV1>(value).is_err());
    }

    #[test]
    fn learning_proposal_snooze_bound_is_closed_and_lifetime_bound() {
        // A snooze names its deferral bound; other decisions must not.
        let mut snooze = proposal(AppLearningDecisionKindV1::Snooze);
        snooze.snooze_until_ms = Some(1_500);
        snooze.proposal_digest.clear();
        // `seal` consumes the proposal, so keep the configured pre-seal
        // shape for the mutation variants below.
        let snooze_base = snooze.clone();
        snooze.seal().expect("snooze within its lifetime");

        let mut snooze_without_bound = snooze_base.clone();
        snooze_without_bound.snooze_until_ms = None;
        snooze_without_bound.proposal_digest.clear();
        assert!(matches!(
            snooze_without_bound.seal(),
            Err(AppContributionContractError::InvalidField(
                "snooze_until_ms"
            ))
        ));

        // The deferral bound is lifetime-bound exactly like the owner
        // retention: `issued_at_ms < bound <= expires_at_ms`, so the expiry
        // instant itself stays valid while one millisecond past it fails.
        let mut at_expiry = snooze_base.clone();
        at_expiry.snooze_until_ms = Some(2_000);
        at_expiry.proposal_digest.clear();
        at_expiry
            .seal()
            .expect("a snooze bound at the proposal expiry remains inside its lifetime");

        for bound in [999_i64, 2_001, 2_500] {
            let mut stale = snooze_base.clone();
            stale.snooze_until_ms = Some(bound);
            stale.proposal_digest.clear();
            assert!(matches!(
                stale.seal(),
                Err(AppContributionContractError::InvalidField(
                    "snooze_until_ms"
                ))
            ));
        }

        let mut approve_with_bound = proposal(AppLearningDecisionKindV1::Approve);
        approve_with_bound.snooze_until_ms = Some(1_500);
        approve_with_bound.proposal_digest.clear();
        assert!(matches!(
            approve_with_bound.seal(),
            Err(AppContributionContractError::InvalidField(
                "snooze_until_ms"
            ))
        ));

        // The decision vocabulary stays the closed review set.
        assert_eq!(
            [
                AppLearningDecisionKindV1::Approve.as_str(),
                AppLearningDecisionKindV1::Reject.as_str(),
                AppLearningDecisionKindV1::Snooze.as_str()
            ],
            ["approve", "reject", "snooze"]
        );
    }

    #[test]
    fn learning_proposal_reason_stays_bounded_and_fail_closed() {
        let mut oversized = proposal(AppLearningDecisionKindV1::Approve);
        oversized.reason = "x".repeat(APP_LEARNING_MAX_REASON_BYTES + 1);
        oversized.reason_digest = content_digest(oversized.reason.as_bytes());
        oversized.proposal_digest.clear();
        assert!(oversized.seal().is_err());

        let mut control_character_reason = proposal(AppLearningDecisionKindV1::Reject);
        control_character_reason.reason = "bad \u{7} reason".to_owned();
        control_character_reason.reason_digest =
            content_digest(control_character_reason.reason.as_bytes());
        control_character_reason.proposal_digest.clear();
        assert!(control_character_reason.seal().is_err());

        let mut wrong_destination = proposal(AppLearningDecisionKindV1::Approve);
        wrong_destination.header.destination_contract_id =
            APP_MEMORY_CANDIDATE_CONTRACT_ID.to_owned();
        wrong_destination.proposal_digest.clear();
        assert!(matches!(
            wrong_destination.seal(),
            Err(AppContributionContractError::InvalidField("header"))
        ));
    }

    #[test]
    fn learning_owner_decision_is_head_identity_and_retention_bound() {
        let review = AppLearningOwnerReviewV1::mint(
            0,
            None,
            "desktop-key:1".to_owned(),
            digest("desktop-identity"),
            proposal(AppLearningDecisionKindV1::Approve),
        )
        .expect("learning review");
        let accepted = AppLearningOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppLearningOwnerDecisionV1::Accept,
            Some(review.proposal.header.expires_at_ms),
        )
        .expect("bounded learning accept");
        assert!(!accepted
            .signing_bytes()
            .expect("learning signing bytes")
            .is_empty());
        assert!(AppLearningOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppLearningOwnerDecisionV1::Accept,
            None,
        )
        .is_err());
        assert!(AppLearningOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppLearningOwnerDecisionV1::Accept,
            Some(review.proposal.header.expires_at_ms + 1),
        )
        .is_err());
        assert!(AppLearningOwnerDecisionEnvelopeV1::prepare(
            review.clone(),
            AppLearningOwnerDecisionV1::Reject,
            Some(1_500),
        )
        .is_err());
        let review = AppLearningOwnerReviewV1::mint(
            1,
            Some(digest("accepted-learning-head")),
            "desktop-key:1".to_owned(),
            digest("desktop-identity"),
            proposal(AppLearningDecisionKindV1::Reject),
        )
        .expect("accepted learning review");
        assert!(AppLearningOwnerDecisionEnvelopeV1::prepare(
            review,
            AppLearningOwnerDecisionV1::Revoke,
            None,
        )
        .is_ok());
    }

    #[test]
    fn learning_port_stays_a_distinct_closed_body() {
        let learning = proposal(AppLearningDecisionKindV1::Approve);
        assert!(serde_json::from_value::<AppMemoryCandidateProposalV1>(
            serde_json::to_value(&learning).unwrap()
        )
        .is_err());
        assert!(serde_json::from_value::<AppAttentionCandidateProposalV1>(
            serde_json::to_value(&learning).unwrap()
        )
        .is_err());
        let mut unknown_decision = serde_json::to_value(&learning).unwrap();
        unknown_decision["decision"] = serde_json::json!("promote");
        assert!(serde_json::from_value::<AppLearningDecisionProposalV1>(unknown_decision).is_err());
        let mut memory_shaped = serde_json::to_value(&learning).unwrap();
        memory_shaped
            .as_object_mut()
            .unwrap()
            .insert("claim_or_summary".to_owned(), serde_json::json!("x"));
        assert!(serde_json::from_value::<AppLearningDecisionProposalV1>(memory_shaped).is_err());
    }

    #[test]
    fn learning_receipt_domains_stay_separated_from_attention_and_memory() {
        let learning_receipt = AppLearningIngressReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "receipt:shared".to_owned(),
            proposal_id: "learning-proposal:1".to_owned(),
            proposal_digest: digest("shared-proposal"),
            destination_generation: 1,
            destination_receipt_digest: digest("shared-destination"),
            disposition: AppLearningIngressDispositionV1::Staged,
            recorded_at_ms: 1_500,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("sealed learning ingress receipt");
        let memory_receipt = AppMemoryIngressReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "receipt:shared".to_owned(),
            proposal_id: "learning-proposal:1".to_owned(),
            proposal_digest: digest("shared-proposal"),
            destination_generation: 1,
            destination_receipt_digest: digest("shared-destination"),
            disposition: AppMemoryIngressDispositionV1::Staged,
            recorded_at_ms: 1_500,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("sealed memory ingress receipt");
        assert_ne!(
            learning_receipt.receipt_digest, memory_receipt.receipt_digest,
            "identical bytes must still seal to different domain-separated digests"
        );
        learning_receipt.validate().expect("still valid");
        memory_receipt.validate().expect("still valid");
    }

    #[test]
    fn learning_destination_receipt_admits_sealed_operations() {
        let staged = AppLearningDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "learning-destination-receipt:1".to_owned(),
            generation: 1,
            previous_receipt_digest: None,
            operation: AppLearningDestinationOperationV1::StageProposal {
                proposal: proposal(AppLearningDecisionKindV1::Approve),
            },
            invalidation_disposition: None,
            resulting_projection_digest: digest("learning-projection"),
            recorded_at_ms: 1_500,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("sealed learning destination receipt");
        staged
            .validate()
            .expect("valid learning destination receipt");

        let mut expired_stage = staged.clone();
        expired_stage.recorded_at_ms = 3_000;
        expired_stage.receipt_digest.clear();
        assert!(expired_stage.seal().is_err());

        let decided = AppLearningDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "learning-destination-receipt:2".to_owned(),
            generation: 2,
            previous_receipt_digest: Some(staged.receipt_digest.clone()),
            operation: AppLearningDestinationOperationV1::Decide {
                proposal_id: "learning-proposal:1".to_owned(),
                proposal_digest: digest("learning-proposal"),
                decision: AppLearningOwnerDecisionV1::Accept,
                owner_decision_receipt_digest: digest("owner-decision"),
                retained_until_ms: None,
            },
            invalidation_disposition: None,
            resulting_projection_digest: digest("learning-projection-2"),
            recorded_at_ms: 1_800,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("sealed learning decision receipt");
        decided.validate().expect("valid learning decision receipt");

        let invalidation = AppLearningInvalidationV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            invalidation_id: "learning-invalidation:1".to_owned(),
            installation_id: "installation:1".to_owned(),
            scope_binding_ref: "scope_binding:1".to_owned(),
            proposal_id: "learning-proposal:1".to_owned(),
            proposal_digest: digest("learning-proposal"),
            source_event_ref: "source:record:1".to_owned(),
            source_event_revision: 2,
            source_identity_digest: digest("learning-source"),
            dedupe_key: "dedupe:1".to_owned(),
            reason: AppLearningInvalidationReasonV1::SourceUpdated,
            issued_at_ms: 1_600,
            invalidation_digest: String::new(),
        }
        .seal()
        .expect("sealed learning invalidation");
        let invalidating = AppLearningDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "learning-destination-receipt:3".to_owned(),
            generation: 3,
            previous_receipt_digest: Some(decided.receipt_digest.clone()),
            operation: AppLearningDestinationOperationV1::Invalidate {
                invalidation: invalidation.clone(),
            },
            invalidation_disposition: Some(AppLearningInvalidationDispositionV1::Tombstoned),
            resulting_projection_digest: digest("learning-projection-3"),
            recorded_at_ms: 1_700,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("sealed learning invalidation receipt");
        invalidating.validate().expect("valid invalidation receipt");
    }
}

#[cfg(test)]
mod claims_decision_tests {
    use ring::signature::KeyPair;

    use super::*;

    fn digest(label: &str) -> String {
        content_digest(label.as_bytes())
    }

    fn header() -> AppClaimsDecisionSourceHeaderV1 {
        AppClaimsDecisionSourceHeaderV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            destination_contract_id: APP_CLAIMS_DECISION_CONTRACT_ID.to_owned(),
            destination_contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            destination_schema_digest: app_claims_decision_destination_schema_digest(),
            proposal_id: "claims-decision:1".to_owned(),
            proposal_revision: 1,
            scope_binding_ref: "scope-binding:1".to_owned(),
            installation_id: "installation:claims-review".to_owned(),
            installation_generation: 2,
            package_revision_ref: "package:claims-review:1".to_owned(),
            package_content_digest: digest("package"),
            grant_revision: 3,
            grant_authority_digest: digest("grant"),
            schema_revision: 4,
            schema_digest: digest("schema"),
            source_entity_name: APP_CLAIMS_DECISION_SOURCE_ENTITY.to_owned(),
            source_record_id: "decision:1".to_owned(),
            source_record_revision: 1,
            source_record_digest: digest("source"),
            workflow_id: "workflow:decide-claim".to_owned(),
            workflow_digest: digest("workflow"),
            action_id: "action:decide-claim".to_owned(),
            action_digest: digest("action"),
            issued_at_ms: 1_000,
            expires_at_ms: 2_000,
            dedupe_key: "decision:claim:1:revision:1".to_owned(),
        }
    }

    fn claim_proposal(verb: AppClaimsDecisionVerbV1) -> AppClaimsDecisionProposalV1 {
        AppClaimsDecisionProposalV1 {
            header: header(),
            verb,
            target: AppClaimsDecisionTargetV1::Claim {
                claim_id: "claim:1".to_owned(),
                expected_revision: 1,
            },
            by: "actor:owner-fingerprint".to_owned(),
            note: matches!(
                verb,
                AppClaimsDecisionVerbV1::ConfirmClaim | AppClaimsDecisionVerbV1::RejectClaim
            )
            .then(|| "Reviewed against the transcript.".to_owned()),
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed claims decision")
    }

    fn commitment_proposal() -> AppClaimsDecisionProposalV1 {
        AppClaimsDecisionProposalV1 {
            header: header(),
            verb: AppClaimsDecisionVerbV1::ConfirmCommitment,
            target: AppClaimsDecisionTargetV1::Commitment {
                audience_kind: AppClaimsDecisionAudienceKindV1::Account,
                audience_id: "account:1".to_owned(),
                commitment_id: "commitment:1".to_owned(),
                expected_revision: 2,
            },
            by: "actor:owner-fingerprint".to_owned(),
            note: None,
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed commitment decision")
    }

    fn hex_lower(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn claims_decision_vocabulary_and_targets_are_closed() {
        assert_eq!(
            [
                AppClaimsDecisionVerbV1::ConfirmClaim.as_str(),
                AppClaimsDecisionVerbV1::RejectClaim.as_str(),
                AppClaimsDecisionVerbV1::RecordCommitment.as_str(),
                AppClaimsDecisionVerbV1::ConfirmCommitment.as_str(),
            ],
            [
                "confirm_claim",
                "reject_claim",
                "record_commitment",
                "confirm_commitment",
            ]
        );
        assert!(claim_proposal(AppClaimsDecisionVerbV1::ConfirmClaim)
            .validate()
            .is_ok());
        assert!(claim_proposal(AppClaimsDecisionVerbV1::RejectClaim)
            .validate()
            .is_ok());
        assert!(claim_proposal(AppClaimsDecisionVerbV1::RecordCommitment)
            .validate()
            .is_ok());
        assert!(commitment_proposal().validate().is_ok());

        let mut mismatched = claim_proposal(AppClaimsDecisionVerbV1::ConfirmClaim);
        mismatched.verb = AppClaimsDecisionVerbV1::ConfirmCommitment;
        mismatched.proposal_digest.clear();
        assert!(matches!(
            mismatched.seal(),
            Err(AppContributionContractError::InvalidField("target"))
        ));

        let mut unknown =
            serde_json::to_value(claim_proposal(AppClaimsDecisionVerbV1::ConfirmClaim))
                .expect("serialized proposal");
        unknown["verb"] = serde_json::json!("supersede_commitment");
        assert!(serde_json::from_value::<AppClaimsDecisionProposalV1>(unknown).is_err());

        let mut generic_port =
            serde_json::to_value(claim_proposal(AppClaimsDecisionVerbV1::ConfirmClaim))
                .expect("serialized proposal");
        generic_port["header"]["contribution_port_id"] = serde_json::json!("claims_decision");
        assert!(serde_json::from_value::<AppClaimsDecisionProposalV1>(generic_port).is_err());
    }

    #[test]
    fn claims_decision_refuses_foreign_sources_stale_heads_and_discarded_input() {
        let base = claim_proposal(AppClaimsDecisionVerbV1::ConfirmClaim);

        let mut foreign_source = base.clone();
        foreign_source.header.source_entity_name = "claim_hypothesis".to_owned();
        foreign_source.proposal_digest.clear();
        assert!(matches!(
            foreign_source.seal(),
            Err(AppContributionContractError::InvalidField("header"))
        ));

        let mut missing_source_head = base.clone();
        missing_source_head.header.source_record_revision = 0;
        missing_source_head.proposal_digest.clear();
        assert!(matches!(
            missing_source_head.seal(),
            Err(AppContributionContractError::InvalidField("header"))
        ));

        let mut zero_revision = base.clone();
        zero_revision.target = AppClaimsDecisionTargetV1::Claim {
            claim_id: "claim:1".to_owned(),
            expected_revision: 0,
        };
        zero_revision.proposal_digest.clear();
        assert!(matches!(
            zero_revision.seal(),
            Err(AppContributionContractError::InvalidField(
                "target.expected_revision"
            ))
        ));

        let mut unsafe_target = base.clone();
        unsafe_target.target = AppClaimsDecisionTargetV1::Claim {
            claim_id: "../claim:1".to_owned(),
            expected_revision: 1,
        };
        unsafe_target.proposal_digest.clear();
        assert!(matches!(
            unsafe_target.seal(),
            Err(AppContributionContractError::InvalidField("target.id"))
        ));

        let mut unnamed = base.clone();
        unnamed.by = "  ".to_owned();
        unnamed.proposal_digest.clear();
        assert!(matches!(
            unnamed.seal(),
            Err(AppContributionContractError::InvalidField("by"))
        ));

        let mut ambiguous_actor = base.clone();
        ambiguous_actor.by = "owner @example.com".to_owned();
        ambiguous_actor.proposal_digest.clear();
        assert!(matches!(
            ambiguous_actor.seal(),
            Err(AppContributionContractError::InvalidField("by"))
        ));

        let mut oversized_note = base;
        oversized_note.note = Some("n".repeat(APP_CLAIMS_DECISION_MAX_NOTE_BYTES + 1));
        oversized_note.proposal_digest.clear();
        assert!(matches!(
            oversized_note.seal(),
            Err(AppContributionContractError::InvalidField("note"))
        ));

        let mut discarded_note = commitment_proposal();
        discarded_note.note = Some("this transition has nowhere to store a note".to_owned());
        discarded_note.proposal_digest.clear();
        assert!(matches!(
            discarded_note.seal(),
            Err(AppContributionContractError::InvalidField("note"))
        ));
    }

    #[test]
    fn owner_signature_binds_command_source_actor_and_pairing_generation() {
        use ring::rand::SystemRandom;

        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .expect("generated desktop identity");
        let key_pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .expect("parsed desktop identity");
        assert!(matches!(
            AppClaimsDecisionOwnerReviewV1::mint(
                0,
                "desktop-key:1".to_owned(),
                digest("desktop-identity"),
                claim_proposal(AppClaimsDecisionVerbV1::ConfirmClaim),
            ),
            Err(AppContributionContractError::InvalidField(
                "owner_review.desktop_pairing_generation"
            ))
        ));
        let review = AppClaimsDecisionOwnerReviewV1::mint(
            7,
            "desktop-key:1".to_owned(),
            digest("desktop-identity"),
            claim_proposal(AppClaimsDecisionVerbV1::ConfirmClaim),
        )
        .expect("minted review");
        let unsigned = AppClaimsDecisionOwnerDecisionEnvelopeV1::prepare(
            review,
            AppClaimsDecisionOwnerDecisionV1::Accept,
        )
        .expect("prepared decision");
        let signature = key_pair.sign(&unsigned.signing_bytes().expect("signing bytes"));
        let signed = unsigned
            .with_signature_hex(hex_lower(signature.as_ref()))
            .expect("sealed signature");
        signed
            .verify_signature(&hex_lower(key_pair.public_key().as_ref()))
            .expect("verified signature");

        let mut changed_verb = signed.clone();
        changed_verb.review.proposal.verb = AppClaimsDecisionVerbV1::RejectClaim;
        let mut changed_actor = signed.clone();
        changed_actor.review.proposal.by = "actor:another-owner-fingerprint".to_owned();
        let mut changed_revision = signed.clone();
        changed_revision.review.proposal.target = AppClaimsDecisionTargetV1::Claim {
            claim_id: "claim:1".to_owned(),
            expected_revision: 2,
        };
        let mut changed_note = signed.clone();
        changed_note.review.proposal.note = Some("different note".to_owned());
        let mut changed_source = signed.clone();
        changed_source.review.proposal.header.source_record_digest = digest("other-source");
        let mut changed_pairing_generation = signed.clone();
        changed_pairing_generation.review.desktop_pairing_generation = 8;
        for tampered in [
            changed_verb,
            changed_actor,
            changed_revision,
            changed_note,
            changed_source,
            changed_pairing_generation,
        ] {
            assert!(tampered
                .verify_signature(&hex_lower(key_pair.public_key().as_ref()))
                .is_err());
        }
    }

    #[test]
    fn canonical_decision_id_is_stable_across_equivalent_review_envelopes() {
        let first_proposal = claim_proposal(AppClaimsDecisionVerbV1::ConfirmClaim);
        let mut rerendered_proposal = first_proposal.clone();
        rerendered_proposal.header.proposal_id = "claims-decision:rerendered".to_owned();
        rerendered_proposal.header.proposal_revision = 2;
        rerendered_proposal.header.dedupe_key = "review-render:2".to_owned();
        rerendered_proposal.proposal_digest.clear();
        let rerendered_proposal = rerendered_proposal
            .seal()
            .expect("sealed equivalent rerender");

        let first_review = AppClaimsDecisionOwnerReviewV1::mint(
            7,
            "desktop-key:1".to_owned(),
            digest("desktop-identity"),
            first_proposal,
        )
        .expect("first review");
        let rerendered_review = AppClaimsDecisionOwnerReviewV1::mint(
            7,
            "desktop-key:1".to_owned(),
            digest("desktop-identity"),
            rerendered_proposal,
        )
        .expect("rerendered review");
        assert_ne!(first_review.review_id, rerendered_review.review_id);

        let first = AppClaimsDecisionOwnerDecisionEnvelopeV1::prepare(
            first_review,
            AppClaimsDecisionOwnerDecisionV1::Accept,
        )
        .expect("first decision");
        let rerendered = AppClaimsDecisionOwnerDecisionEnvelopeV1::prepare(
            rerendered_review.clone(),
            AppClaimsDecisionOwnerDecisionV1::Accept,
        )
        .expect("equivalent decision");
        assert_eq!(first.decision_id, rerendered.decision_id);

        let declined = AppClaimsDecisionOwnerDecisionEnvelopeV1::prepare(
            rerendered_review,
            AppClaimsDecisionOwnerDecisionV1::Reject,
        )
        .expect("declined decision");
        assert_ne!(first.decision_id, declined.decision_id);
    }
}
