//! Surface-neutral, deadline-aware retrieval of prompt context.
//!
//! The coordinator in this module deliberately knows nothing about Chat,
//! realtime providers, or task execution. Callers provide independently
//! cancellable stages and receive every valid contribution that completed
//! before one absolute turn deadline. A slow or failed sibling can therefore
//! no longer erase already-retrieved evidence.

use async_trait::async_trait;
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    future::Future,
    panic::AssertUnwindSafe,
    sync::{Arc, OnceLock},
    time::Duration,
};
use thiserror::Error;
use tokio::{sync::mpsc, task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::magician_v2::agents::{FeatureMode, InvocationSurface};

/// Version of the coordinator contract included in reuse keys.
pub const CONTEXT_RETRIEVAL_CONTRACT_VERSION: &str = "staged_context.v1";

const DEFAULT_MAX_CONTRIBUTIONS: usize = 64;
const DEFAULT_MAX_TOTAL_BYTES: usize = 64 * 1024;
const MAX_STAGE_NAME_BYTES: usize = 128;
const MAX_ERROR_CODE_BYTES: usize = 96;

/// Authenticated identity of the turn that owns a retrieval operation.
///
/// `binding_id` is the surface-owned session, call, or execution binding. It
/// prevents otherwise identical queries from reusing session-local evidence
/// across unrelated conversations.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextRetrievalRequest {
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub surface: InvocationSurface,
    pub feature_mode: FeatureMode,
    pub binding_id: String,
    pub turn_id: String,
    pub turn_generation: u64,
    pub relevance_query: String,
    pub authority_revision: String,
    pub retrieval_contract_version: String,
    /// Source revisions that affect retrieval semantics, such as memory,
    /// procedure, model, configuration, or index revisions.
    pub source_revisions: BTreeMap<String, String>,
}

impl fmt::Debug for ContextRetrievalRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextRetrievalRequest")
            .field("principal", &self.principal)
            .field("workspace", &self.workspace)
            .field("agent_id", &self.agent_id)
            .field("surface", &self.surface)
            .field("feature_mode", &self.feature_mode)
            .field("binding_id", &self.binding_id)
            .field("turn_id", &self.turn_id)
            .field("turn_generation", &self.turn_generation)
            .field(
                "relevance_query_digest",
                &relevance_query_digest(&self.relevance_query),
            )
            .field("authority_revision", &self.authority_revision)
            .field(
                "retrieval_contract_version",
                &self.retrieval_contract_version,
            )
            .field("source_revisions", &self.source_revisions)
            .finish()
    }
}

impl ContextRetrievalRequest {
    /// Construct a request with the current retrieval contract version.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        agent_id: impl Into<String>,
        surface: InvocationSurface,
        feature_mode: FeatureMode,
        binding_id: impl Into<String>,
        turn_id: impl Into<String>,
        turn_generation: u64,
        relevance_query: impl Into<String>,
        authority_revision: impl Into<String>,
        source_revisions: BTreeMap<String, String>,
    ) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
            agent_id: agent_id.into(),
            surface,
            feature_mode,
            binding_id: binding_id.into(),
            turn_id: turn_id.into(),
            turn_generation,
            relevance_query: relevance_query.into(),
            authority_revision: authority_revision.into(),
            retrieval_contract_version: CONTEXT_RETRIEVAL_CONTRACT_VERSION.to_string(),
            source_revisions,
        }
    }

    fn validate(&self) -> Result<(), ContextCoordinatorError> {
        for (field, value) in [
            ("principal", self.principal.as_str()),
            ("workspace", self.workspace.as_str()),
            ("agent_id", self.agent_id.as_str()),
            ("binding_id", self.binding_id.as_str()),
            ("turn_id", self.turn_id.as_str()),
            ("authority_revision", self.authority_revision.as_str()),
            (
                "retrieval_contract_version",
                self.retrieval_contract_version.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                return Err(ContextCoordinatorError::InvalidRequest { field });
            }
        }
        for (source, revision) in &self.source_revisions {
            if source.trim().is_empty() || revision.trim().is_empty() {
                return Err(ContextCoordinatorError::InvalidRequest {
                    field: "source_revisions",
                });
            }
        }
        Ok(())
    }

    pub fn turn_binding(&self) -> ContextTurnBinding {
        ContextTurnBinding {
            binding_id: self.binding_id.clone(),
            turn_id: self.turn_id.clone(),
            turn_generation: self.turn_generation,
            relevance_query_digest: relevance_query_digest(&self.relevance_query),
        }
    }
}

/// Absolute-deadline and output-bound policy for one coordinator run.
#[derive(Debug, Clone)]
pub struct ContextRetrievalPolicy {
    pub deadline: Instant,
    pub max_contributions: usize,
    pub max_total_bytes: usize,
}

impl ContextRetrievalPolicy {
    pub fn until(deadline: Instant) -> Self {
        Self {
            deadline,
            max_contributions: DEFAULT_MAX_CONTRIBUTIONS,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
        }
    }

    /// Convenience constructor. The coordinator still receives and enforces
    /// the absolute `deadline` stored in the returned policy.
    pub fn with_budget(budget: Duration) -> Self {
        Self::until(Instant::now() + budget)
    }

    pub fn with_bounds(mut self, max_contributions: usize, max_total_bytes: usize) -> Self {
        self.max_contributions = max_contributions;
        self.max_total_bytes = max_total_bytes;
        self
    }

    fn validate(&self) -> Result<(), ContextCoordinatorError> {
        if self.max_contributions == 0 {
            return Err(ContextCoordinatorError::InvalidPolicy {
                field: "max_contributions",
            });
        }
        if self.max_total_bytes == 0 {
            return Err(ContextCoordinatorError::InvalidPolicy {
                field: "max_total_bytes",
            });
        }
        Ok(())
    }
}

/// Stable semantic class of a retrieval stage. Variant order is intentionally
/// the default merge order and must not be changed without a contract version
/// bump.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ContextStageKind {
    TurnLocalState,
    FastMemory,
    HybridMemory,
    ReusableProcedures,
    OptionalCheckpoint,
    Extension,
}

/// Stable stage identity and deterministic ordering within its semantic kind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContextStageDescriptor {
    pub kind: ContextStageKind,
    pub order_within_kind: u16,
    pub name: String,
}

impl ContextStageDescriptor {
    pub fn new(kind: ContextStageKind, order_within_kind: u16, name: impl Into<String>) -> Self {
        Self {
            kind,
            order_within_kind,
            name: name.into(),
        }
    }

    fn validate(&self) -> Result<(), ContextCoordinatorError> {
        if self.name.trim().is_empty() || self.name.len() > MAX_STAGE_NAME_BYTES {
            return Err(ContextCoordinatorError::InvalidStage {
                stage: self.name.clone(),
                reason: "stage name must be non-empty and bounded",
            });
        }
        Ok(())
    }
}

/// Canonical identity used to deduplicate the same evidence returned by
/// multiple retrieval strategies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContextEvidenceKey {
    pub source_identity: String,
    pub revision: String,
}

impl ContextEvidenceKey {
    pub fn new(source_identity: impl Into<String>, revision: impl Into<String>) -> Self {
        Self {
            source_identity: source_identity.into(),
            revision: revision.into(),
        }
    }

    fn is_valid(&self) -> bool {
        !self.source_identity.trim().is_empty() && !self.revision.trim().is_empty()
    }
}

/// Producer-declared completeness. The coordinator prefers a complete
/// authorized representation over an excerpt of the same evidence revision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ContextEvidenceCompleteness {
    IdentifierOnly,
    Excerpt,
    Complete,
}

/// A contribution before the coordinator stamps turn and stage identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextContributionDraft {
    pub evidence_key: ContextEvidenceKey,
    pub text: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub candidate_count: usize,
    pub retrieval_backend: String,
    pub completeness: ContextEvidenceCompleteness,
    /// Lower values are more relevant and are retained first at a budget.
    #[serde(default)]
    pub rank: u32,
}

impl ContextContributionDraft {
    fn validate(&self) -> Result<(), ContextStageError> {
        if !self.evidence_key.is_valid() {
            return Err(ContextStageError::new(
                "invalid_evidence_key",
                false,
                "evidence identity and revision must be non-empty",
            ));
        }
        if self.text.trim().is_empty() {
            return Err(ContextStageError::new(
                "empty_contribution",
                false,
                "context contribution text must be non-empty",
            ));
        }
        if self.retrieval_backend.trim().is_empty() {
            return Err(ContextStageError::new(
                "missing_retrieval_backend",
                false,
                "retrieval backend must be identified",
            ));
        }
        if self
            .evidence_refs
            .iter()
            .any(|reference| reference.trim().is_empty())
        {
            return Err(ContextStageError::new(
                "invalid_evidence_ref",
                false,
                "evidence references must be non-empty",
            ));
        }
        Ok(())
    }
}

/// A validated, turn-bound context contribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextContribution {
    pub turn: ContextTurnBinding,
    pub stage: ContextStageDescriptor,
    pub evidence_key: ContextEvidenceKey,
    pub text: String,
    pub evidence_refs: Vec<String>,
    pub candidate_count: usize,
    pub retrieval_backend: String,
    pub completeness: ContextEvidenceCompleteness,
    pub rank: u32,
    pub completed_at_ms: f64,
}

impl ContextContribution {
    /// Serialized byte size used for atomic admission. The coordinator omits
    /// this entire contribution when it does not fit; it never slices `text`.
    pub fn estimated_size_bytes(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |serialized| serialized.len())
    }
}

/// Turn fence stamped onto every contribution and outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextTurnBinding {
    pub binding_id: String,
    pub turn_id: String,
    pub turn_generation: u64,
    pub relevance_query_digest: String,
}

impl ContextTurnBinding {
    pub fn matches_request(&self, request: &ContextRetrievalRequest) -> bool {
        self == &request.turn_binding()
    }
}

/// Content-free, exact cache/reuse key. The raw relevance query is never
/// retained in this key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextReuseKey {
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub surface: InvocationSurface,
    pub feature_mode: FeatureMode,
    pub binding_id: String,
    pub authority_revision: String,
    pub relevance_query_digest: String,
    pub retrieval_contract_version: String,
    pub source_revisions: BTreeMap<String, String>,
    /// Output bounds are part of the key so a small interactive result cannot
    /// be reused as though it were a larger task-grade result.
    pub max_contributions: usize,
    pub max_total_bytes: usize,
}

impl ContextReuseKey {
    pub fn for_request(
        request: &ContextRetrievalRequest,
        policy: &ContextRetrievalPolicy,
    ) -> Result<Self, ContextCoordinatorError> {
        request.validate()?;
        policy.validate()?;
        Ok(Self {
            principal: request.principal.clone(),
            workspace: request.workspace.clone(),
            agent_id: request.agent_id.clone(),
            surface: request.surface,
            feature_mode: request.feature_mode,
            binding_id: request.binding_id.clone(),
            authority_revision: request.authority_revision.clone(),
            relevance_query_digest: relevance_query_digest(&request.relevance_query),
            retrieval_contract_version: request.retrieval_contract_version.clone(),
            source_revisions: request.source_revisions.clone(),
            max_contributions: policy.max_contributions,
            max_total_bytes: policy.max_total_bytes,
        })
    }

    pub fn matches_request(&self, request: &ContextRetrievalRequest) -> bool {
        request.validate().is_ok()
            && self.principal == request.principal
            && self.workspace == request.workspace
            && self.agent_id == request.agent_id
            && self.surface == request.surface
            && self.feature_mode == request.feature_mode
            && self.binding_id == request.binding_id
            && self.authority_revision == request.authority_revision
            && self.relevance_query_digest == relevance_query_digest(&request.relevance_query)
            && self.retrieval_contract_version == request.retrieval_contract_version
            && self.source_revisions == request.source_revisions
    }

    pub fn matches_policy(&self, policy: &ContextRetrievalPolicy) -> bool {
        policy.validate().is_ok()
            && self.max_contributions == policy.max_contributions
            && self.max_total_bytes == policy.max_total_bytes
    }

    /// Stable internal fingerprint suitable for a bounded cache key. It is
    /// not an authorization token and callers must still recheck the key's
    /// structured scope and current revisions before reuse.
    pub fn fingerprint(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("ContextReuseKey serialization is infallible");
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"magician.staged-context.reuse-key.v1\0");
        hasher.update(&bytes);
        hasher.finalize().to_hex().to_string()
    }
}

fn relevance_query_digest(query: &str) -> String {
    // A plain content hash would be a dictionary oracle for low-entropy
    // private queries (names, dates, relationships). Reuse is intentionally
    // process-local and short-lived, so key the digest with an ephemeral
    // process secret rather than persisting a bearer-like stable hash.
    static DIGEST_KEY: OnceLock<[u8; 32]> = OnceLock::new();
    let key = DIGEST_KEY.get_or_init(|| {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key
    });
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(b"magician.staged-context.relevance-query.v1\0");
    hasher.update(query.as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextStageState {
    Completed,
    Empty,
    TimedOut,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextStageStatus {
    pub stage: ContextStageDescriptor,
    pub state: ContextStageState,
    pub elapsed_ms: f64,
    pub candidate_count: usize,
    pub produced_contribution_count: usize,
    pub accepted_contribution_count: usize,
    pub discarded_duplicate_count: usize,
    pub omitted_by_budget_count: usize,
    pub error_code: Option<String>,
    pub error_retryable: Option<bool>,
}

impl ContextStageStatus {
    fn unfinished(
        stage: ContextStageDescriptor,
        state: ContextStageState,
        elapsed_ms: f64,
    ) -> Self {
        Self {
            stage,
            state,
            elapsed_ms,
            candidate_count: 0,
            produced_contribution_count: 0,
            accepted_contribution_count: 0,
            discarded_duplicate_count: 0,
            omitted_by_budget_count: 0,
            error_code: None,
            error_retryable: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextRetrievalState {
    Ready,
    Partial,
    Empty,
    TimedOut,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextRetrievalOutcome {
    pub state: ContextRetrievalState,
    pub turn: ContextTurnBinding,
    pub reuse_key: ContextReuseKey,
    pub contributions: Vec<ContextContribution>,
    pub stage_statuses: Vec<ContextStageStatus>,
    pub elapsed_ms: f64,
    pub deadline_reached: bool,
    pub cancelled: bool,
    pub deduplicated_evidence_count: usize,
    /// Number of evidence identities rejected because the same source
    /// identity/revision carried contradictory representations.
    pub evidence_conflict_count: usize,
    pub omitted_by_budget_count: usize,
}

impl ContextRetrievalOutcome {
    pub fn is_bound_to(&self, request: &ContextRetrievalRequest) -> bool {
        self.turn.matches_request(request) && self.reuse_key.matches_request(request)
    }

    /// Only complete, current outcomes may be cached as a whole. In
    /// particular, a timeout/error is never cached as an empty result.
    pub fn is_complete_for_reuse(&self) -> bool {
        !self.cancelled
            && !self.deadline_reached
            && self.stage_statuses.iter().all(|status| {
                matches!(
                    status.state,
                    ContextStageState::Completed | ContextStageState::Empty
                )
            })
    }

    /// One-call guard for cache consumers. Reuse is allowed only when the
    /// outcome is complete and every turn scope, authority/query/source
    /// revision, and output bound still matches.
    pub fn can_reuse_for(
        &self,
        request: &ContextRetrievalRequest,
        policy: &ContextRetrievalPolicy,
    ) -> bool {
        self.is_complete_for_reuse()
            && self.is_bound_to(request)
            && self.reuse_key.matches_policy(policy)
    }
}

/// Stage execution context. Every stage sees the same absolute deadline.
#[derive(Clone)]
pub struct ContextStageExecutionContext {
    pub request: Arc<ContextRetrievalRequest>,
    pub deadline: Instant,
    pub cancellation: CancellationToken,
}

impl ContextStageExecutionContext {
    pub fn remaining(&self) -> Duration {
        if self.cancellation.is_cancelled() {
            Duration::ZERO
        } else {
            self.deadline.saturating_duration_since(Instant::now())
        }
    }

    pub fn should_stop(&self) -> bool {
        self.cancellation.is_cancelled() || Instant::now() >= self.deadline
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("context stage failed ({code})")]
pub struct ContextStageError {
    pub code: String,
    pub retryable: bool,
    /// Diagnostic detail for restricted logs. It is intentionally not copied
    /// into the ordinary outcome or metrics contract.
    pub detail: String,
}

impl ContextStageError {
    pub fn new(code: impl Into<String>, retryable: bool, detail: impl Into<String>) -> Self {
        let code = code.into();
        let code = code.trim();
        let code = if code.is_empty()
            || code.len() > MAX_ERROR_CODE_BYTES
            || !code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            "invalid_stage_error_code".to_string()
        } else {
            code.to_string()
        };
        Self {
            code,
            retryable,
            detail: detail.into(),
        }
    }
}

#[async_trait]
pub trait ContextRetrievalStage: Send + Sync {
    fn descriptor(&self) -> ContextStageDescriptor;

    /// Retrieve context without mutating provider/session state. Implementors
    /// must honor `context.cancellation`, must not detach unowned work, and
    /// must return only evidence already authorized for `context.request`.
    /// The coordinator will additionally abort the owning task at the turn
    /// fence, but cooperative cancellation is required for external I/O.
    async fn retrieve(
        &self,
        context: ContextStageExecutionContext,
    ) -> Result<Vec<ContextContributionDraft>, ContextStageError>;
}

/// Zero-boilerplate adapter for existing Chat, voice, and task retrieval
/// futures. Captured services should normally be `Arc`s cloned into the async
/// block returned by the closure.
pub struct FnContextRetrievalStage<F> {
    descriptor: ContextStageDescriptor,
    retrieve: F,
}

impl<F> FnContextRetrievalStage<F> {
    pub fn new(descriptor: ContextStageDescriptor, retrieve: F) -> Self {
        Self {
            descriptor,
            retrieve,
        }
    }
}

#[async_trait]
impl<F, Fut> ContextRetrievalStage for FnContextRetrievalStage<F>
where
    F: Fn(ContextStageExecutionContext) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Vec<ContextContributionDraft>, ContextStageError>> + Send,
{
    fn descriptor(&self) -> ContextStageDescriptor {
        self.descriptor.clone()
    }

    async fn retrieve(
        &self,
        context: ContextStageExecutionContext,
    ) -> Result<Vec<ContextContributionDraft>, ContextStageError> {
        (self.retrieve)(context).await
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextCoordinatorError {
    #[error("invalid context retrieval request field: {field}")]
    InvalidRequest { field: &'static str },
    #[error("invalid context retrieval policy field: {field}")]
    InvalidPolicy { field: &'static str },
    #[error("invalid context stage `{stage}`: {reason}")]
    InvalidStage { stage: String, reason: &'static str },
    #[error("duplicate context stage descriptor: {stage}")]
    DuplicateStage { stage: String },
}

/// Stateless reusable coordinator.
#[derive(Debug, Default, Clone, Copy)]
pub struct StagedContextCoordinator;

impl StagedContextCoordinator {
    pub async fn retrieve(
        &self,
        request: ContextRetrievalRequest,
        policy: ContextRetrievalPolicy,
        cancellation: CancellationToken,
        stages: Vec<Arc<dyn ContextRetrievalStage>>,
    ) -> Result<ContextRetrievalOutcome, ContextCoordinatorError> {
        retrieve_turn_context(request, policy, cancellation, stages).await
    }
}

/// Borrow-friendly two-stage adapter for existing retrieval futures. This is
/// the production migration seam for Chat/voice/task call sites whose futures
/// borrow their owning service and therefore cannot be moved into a `'static`
/// `ContextRetrievalStage`. It preserves the same absolute-deadline and
/// sibling-survival semantics as [`StagedContextCoordinator`].
#[derive(Debug, Clone, PartialEq)]
pub struct StagedPairOutcome<A, B> {
    pub first: Option<A>,
    pub second: Option<B>,
    pub first_status: ContextStageState,
    pub second_status: ContextStageState,
    pub first_elapsed_ms: f64,
    pub second_elapsed_ms: f64,
    pub first_error_code: Option<String>,
    pub second_error_code: Option<String>,
    pub first_error_retryable: Option<bool>,
    pub second_error_retryable: Option<bool>,
    pub elapsed_ms: f64,
    pub deadline_reached: bool,
    pub cancelled: bool,
}

/// Borrow-friendly three-stage variant used by production prompt assembly:
/// fast immutable memory, hybrid memory, and reusable procedures all begin
/// under the same absolute deadline. Values remain heterogeneous so existing
/// renderers do not need to erase their typed results into strings.
#[derive(Debug, Clone, PartialEq)]
pub struct StagedTripleOutcome<A, B, C> {
    pub first: Option<A>,
    pub second: Option<B>,
    pub third: Option<C>,
    pub first_status: ContextStageState,
    pub second_status: ContextStageState,
    pub third_status: ContextStageState,
    pub first_elapsed_ms: f64,
    pub second_elapsed_ms: f64,
    pub third_elapsed_ms: f64,
    pub first_error_code: Option<String>,
    pub second_error_code: Option<String>,
    pub third_error_code: Option<String>,
    pub first_error_retryable: Option<bool>,
    pub second_error_retryable: Option<bool>,
    pub third_error_retryable: Option<bool>,
    pub elapsed_ms: f64,
    pub deadline_reached: bool,
    pub cancelled: bool,
}

/// Typed staged values describe their canonical evidence without giving up
/// the borrow-friendly value consumed by the surface renderer. This lets the
/// production seam enforce the same binding, ordering, deduplication, and
/// atomic budget rules as the object-safe coordinator.
pub trait ContextContributionSource {
    fn context_contributions(&self) -> Vec<ContextContributionDraft>;
}

impl ContextContributionSource for &str {
    fn context_contributions(&self) -> Vec<ContextContributionDraft> {
        string_context_contribution(self)
    }
}

impl ContextContributionSource for String {
    fn context_contributions(&self) -> Vec<ContextContributionDraft> {
        string_context_contribution(self)
    }
}

fn string_context_contribution(text: &str) -> Vec<ContextContributionDraft> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let revision = blake3::hash(text.as_bytes()).to_hex().to_string();
    vec![ContextContributionDraft {
        evidence_key: ContextEvidenceKey::new("borrowed-stage-text", revision),
        text: text.to_string(),
        evidence_refs: Vec::new(),
        candidate_count: 1,
        retrieval_backend: "borrowed_stage".to_string(),
        completeness: ContextEvidenceCompleteness::Complete,
        rank: 0,
    }]
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundStagedTripleOutcome<A, B, C> {
    pub first: Option<A>,
    pub second: Option<B>,
    pub third: Option<C>,
    pub first_status: ContextStageState,
    pub second_status: ContextStageState,
    pub third_status: ContextStageState,
    pub first_elapsed_ms: f64,
    pub second_elapsed_ms: f64,
    pub third_elapsed_ms: f64,
    pub elapsed_ms: f64,
    pub deadline_reached: bool,
    pub cancelled: bool,
    /// Canonical turn/reuse binding and deterministic accepted evidence for
    /// this exact production checkpoint.
    pub canonical: ContextRetrievalOutcome,
}

impl<A, B, C> BoundStagedTripleOutcome<A, B, C> {
    pub fn stage_contributed(&self, descriptor: &ContextStageDescriptor) -> bool {
        self.canonical
            .stage_statuses
            .iter()
            .any(|status| &status.stage == descriptor && status.accepted_contribution_count > 0)
    }
}

fn canonical_stage_value_is_usable(
    outcome: &ContextRetrievalOutcome,
    descriptor: &ContextStageDescriptor,
) -> bool {
    outcome.stage_statuses.iter().any(|status| {
        &status.stage == descriptor
            && status.state == ContextStageState::Completed
            && status.omitted_by_budget_count == 0
            && status.accepted_contribution_count > 0
    })
}

fn canonical_stage_state(
    outcome: &ContextRetrievalOutcome,
    descriptor: &ContextStageDescriptor,
) -> ContextStageState {
    outcome
        .stage_statuses
        .iter()
        .find(|status| &status.stage == descriptor)
        .map(|status| status.state.clone())
        .unwrap_or(ContextStageState::Error)
}

/// Typed completion from a borrow-friendly stage. It lets the lightweight
/// adapter preserve an independently empty or failed sibling without forcing
/// current service-borrowing futures into boxed `'static` stage objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedValue<T> {
    Completed(T),
    Empty,
    Error { code: String, retryable: bool },
}

impl<T> StagedValue<T> {
    pub fn error(code: impl Into<String>, retryable: bool) -> Self {
        let error = ContextStageError::new(code, retryable, "staged retrieval error");
        Self::Error {
            code: error.code,
            retryable: error.retryable,
        }
    }
}

/// Convert an existing future output into an explicit staged completion.
/// `Option<T>` is supported directly so current Chat memory/procedure futures
/// gain truthful `empty` status without bespoke wrappers.
pub trait IntoStagedValue {
    type Value;

    fn into_staged_value(self) -> StagedValue<Self::Value>;
}

impl<T> IntoStagedValue for Option<T> {
    type Value = T;

    fn into_staged_value(self) -> StagedValue<Self::Value> {
        match self {
            Some(value) => StagedValue::Completed(value),
            None => StagedValue::Empty,
        }
    }
}

impl<T> IntoStagedValue for StagedValue<T> {
    type Value = T;

    fn into_staged_value(self) -> StagedValue<Self::Value> {
        self
    }
}

/// Fixed product checkpoint labels for content-free observability. Free-form
/// labels are intentionally excluded so user text cannot enter ordinary logs.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextRetrievalCheckpoint {
    ChatTurn,
    RealtimeVoiceTurn,
    AutonomousTaskCheckpoint,
}

impl ContextRetrievalCheckpoint {
    pub fn surface(self) -> InvocationSurface {
        match self {
            Self::ChatTurn => InvocationSurface::Chat,
            Self::RealtimeVoiceTurn => InvocationSurface::RealtimeVoice,
            Self::AutonomousTaskCheckpoint => InvocationSurface::Task,
        }
    }
}

/// One content-free stage row emitted for every staged context attempt.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ContextStageObservation {
    pub stage: ContextStageKind,
    pub status: ContextStageState,
    pub elapsed_ms: f64,
    pub error_code: Option<String>,
    pub error_retryable: Option<bool>,
    pub candidate_count: usize,
    pub produced_contribution_count: usize,
    pub accepted_contribution_count: usize,
    pub discarded_duplicate_count: usize,
    pub omitted_by_budget_count: usize,
}

/// Unified outcome row shared by Chat, realtime voice, and autonomous task
/// checkpoints. It deliberately contains no query, context text, evidence
/// reference, session identifier, or user-controlled label.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ContextRetrievalObservation {
    pub schema_version: &'static str,
    pub checkpoint: ContextRetrievalCheckpoint,
    pub surface: InvocationSurface,
    pub feature_mode: FeatureMode,
    pub outcome: ContextRetrievalState,
    pub elapsed_ms: f64,
    pub budget_ms: u64,
    pub deadline_reached: bool,
    pub cancelled: bool,
    pub completed_stage_count: usize,
    pub empty_stage_count: usize,
    pub timed_out_stage_count: usize,
    pub error_stage_count: usize,
    pub cancelled_stage_count: usize,
    pub contribution_count: usize,
    pub contribution_bytes: usize,
    pub deduplicated_evidence_count: usize,
    pub evidence_conflict_count: usize,
    pub omitted_by_budget_count: usize,
    pub retrieval_backends: Vec<String>,
    pub stages: Vec<ContextStageObservation>,
}

impl ContextRetrievalObservation {
    /// Content-free outcome for a turn fence reached before retrieval stages
    /// could start (for example realtime session/history setup consuming the
    /// complete absolute deadline).
    pub fn setup_timed_out(
        checkpoint: ContextRetrievalCheckpoint,
        budget_ms: u64,
        elapsed_ms: f64,
    ) -> Self {
        Self::pre_stage_fence(
            checkpoint,
            budget_ms,
            elapsed_ms,
            ContextStageState::TimedOut,
            true,
            false,
        )
    }

    /// Content-free outcome for a turn cancelled before retrieval stages
    /// could start.
    pub fn setup_cancelled(
        checkpoint: ContextRetrievalCheckpoint,
        budget_ms: u64,
        elapsed_ms: f64,
    ) -> Self {
        Self::pre_stage_fence(
            checkpoint,
            budget_ms,
            elapsed_ms,
            ContextStageState::Cancelled,
            false,
            true,
        )
    }

    fn pre_stage_fence(
        checkpoint: ContextRetrievalCheckpoint,
        budget_ms: u64,
        elapsed_ms: f64,
        state: ContextStageState,
        deadline_reached: bool,
        cancelled: bool,
    ) -> Self {
        Self::new(
            checkpoint,
            budget_ms,
            elapsed_ms,
            deadline_reached,
            cancelled,
            [
                ContextStageKind::FastMemory,
                ContextStageKind::HybridMemory,
                ContextStageKind::ReusableProcedures,
            ]
            .into_iter()
            .map(|stage| ContextStageObservation {
                stage,
                status: state,
                elapsed_ms,
                error_code: None,
                error_retryable: None,
                candidate_count: 0,
                produced_contribution_count: 0,
                accepted_contribution_count: 0,
                discarded_duplicate_count: 0,
                omitted_by_budget_count: 0,
            })
            .collect(),
        )
    }

    pub fn from_outcome(
        checkpoint: ContextRetrievalCheckpoint,
        budget_ms: u64,
        outcome: &ContextRetrievalOutcome,
    ) -> Self {
        let mut observation = Self::new(
            checkpoint,
            budget_ms,
            outcome.elapsed_ms,
            outcome.deadline_reached,
            outcome.cancelled,
            outcome
                .stage_statuses
                .iter()
                .map(|status| ContextStageObservation {
                    stage: status.stage.kind,
                    status: status.state.clone(),
                    elapsed_ms: status.elapsed_ms,
                    error_code: status.error_code.clone(),
                    error_retryable: status.error_retryable,
                    candidate_count: status.candidate_count,
                    produced_contribution_count: status.produced_contribution_count,
                    accepted_contribution_count: status.accepted_contribution_count,
                    discarded_duplicate_count: status.discarded_duplicate_count,
                    omitted_by_budget_count: status.omitted_by_budget_count,
                })
                .collect(),
        );
        observation.contribution_count = outcome.contributions.len();
        observation.contribution_bytes = outcome
            .contributions
            .iter()
            .map(ContextContribution::estimated_size_bytes)
            .fold(0usize, usize::saturating_add);
        observation.deduplicated_evidence_count = outcome.deduplicated_evidence_count;
        observation.evidence_conflict_count = outcome.evidence_conflict_count;
        observation.omitted_by_budget_count = outcome.omitted_by_budget_count;
        observation.surface = outcome.reuse_key.surface;
        observation.feature_mode = outcome.reuse_key.feature_mode;
        observation.retrieval_backends = outcome
            .contributions
            .iter()
            .map(|contribution| bounded_backend_label(&contribution.retrieval_backend))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        observation
    }

    pub fn from_pair<A, B>(
        checkpoint: ContextRetrievalCheckpoint,
        budget_ms: u64,
        outcome: &StagedPairOutcome<A, B>,
    ) -> Self {
        Self::new(
            checkpoint,
            budget_ms,
            outcome.elapsed_ms,
            outcome.deadline_reached,
            outcome.cancelled,
            vec![
                ContextStageObservation {
                    stage: ContextStageKind::HybridMemory,
                    status: outcome.first_status.clone(),
                    elapsed_ms: outcome.first_elapsed_ms,
                    error_code: outcome.first_error_code.clone(),
                    error_retryable: outcome.first_error_retryable,
                    candidate_count: 0,
                    produced_contribution_count: 0,
                    accepted_contribution_count: 0,
                    discarded_duplicate_count: 0,
                    omitted_by_budget_count: 0,
                },
                ContextStageObservation {
                    stage: ContextStageKind::ReusableProcedures,
                    status: outcome.second_status.clone(),
                    elapsed_ms: outcome.second_elapsed_ms,
                    error_code: outcome.second_error_code.clone(),
                    error_retryable: outcome.second_error_retryable,
                    candidate_count: 0,
                    produced_contribution_count: 0,
                    accepted_contribution_count: 0,
                    discarded_duplicate_count: 0,
                    omitted_by_budget_count: 0,
                },
            ],
        )
    }

    pub fn from_triple<A, B, C>(
        checkpoint: ContextRetrievalCheckpoint,
        budget_ms: u64,
        outcome: &StagedTripleOutcome<A, B, C>,
    ) -> Self {
        Self::new(
            checkpoint,
            budget_ms,
            outcome.elapsed_ms,
            outcome.deadline_reached,
            outcome.cancelled,
            vec![
                ContextStageObservation {
                    stage: ContextStageKind::FastMemory,
                    status: outcome.first_status.clone(),
                    elapsed_ms: outcome.first_elapsed_ms,
                    error_code: outcome.first_error_code.clone(),
                    error_retryable: outcome.first_error_retryable,
                    candidate_count: 0,
                    produced_contribution_count: 0,
                    accepted_contribution_count: 0,
                    discarded_duplicate_count: 0,
                    omitted_by_budget_count: 0,
                },
                ContextStageObservation {
                    stage: ContextStageKind::HybridMemory,
                    status: outcome.second_status.clone(),
                    elapsed_ms: outcome.second_elapsed_ms,
                    error_code: outcome.second_error_code.clone(),
                    error_retryable: outcome.second_error_retryable,
                    candidate_count: 0,
                    produced_contribution_count: 0,
                    accepted_contribution_count: 0,
                    discarded_duplicate_count: 0,
                    omitted_by_budget_count: 0,
                },
                ContextStageObservation {
                    stage: ContextStageKind::ReusableProcedures,
                    status: outcome.third_status.clone(),
                    elapsed_ms: outcome.third_elapsed_ms,
                    error_code: outcome.third_error_code.clone(),
                    error_retryable: outcome.third_error_retryable,
                    candidate_count: 0,
                    produced_contribution_count: 0,
                    accepted_contribution_count: 0,
                    discarded_duplicate_count: 0,
                    omitted_by_budget_count: 0,
                },
            ],
        )
    }

    fn new(
        checkpoint: ContextRetrievalCheckpoint,
        budget_ms: u64,
        elapsed_ms: f64,
        deadline_reached: bool,
        cancelled: bool,
        stages: Vec<ContextStageObservation>,
    ) -> Self {
        let completed_stage_count = count_stage_state(&stages, ContextStageState::Completed);
        let empty_stage_count = count_stage_state(&stages, ContextStageState::Empty);
        let timed_out_stage_count = count_stage_state(&stages, ContextStageState::TimedOut);
        let error_stage_count = count_stage_state(&stages, ContextStageState::Error);
        let cancelled_stage_count = count_stage_state(&stages, ContextStageState::Cancelled);
        let outcome = classify_observed_context_outcome(cancelled, &stages);
        Self {
            schema_version: "context_retrieval_observation.v1",
            checkpoint,
            surface: checkpoint.surface(),
            feature_mode: FeatureMode::None,
            outcome,
            elapsed_ms,
            budget_ms,
            deadline_reached,
            cancelled,
            completed_stage_count,
            empty_stage_count,
            timed_out_stage_count,
            error_stage_count,
            cancelled_stage_count,
            contribution_count: 0,
            contribution_bytes: 0,
            deduplicated_evidence_count: 0,
            evidence_conflict_count: 0,
            omitted_by_budget_count: 0,
            retrieval_backends: Vec::new(),
            stages,
        }
    }

    /// Emit one unified outcome row followed by independent stage rows. Only
    /// fixed enums, timings, counts, and bounded error codes are recorded.
    pub fn emit(&self) {
        tracing::info!(
            event = "context_retrieval.outcome",
            schema_version = self.schema_version,
            checkpoint = ?self.checkpoint,
            surface = self.surface.as_str(),
            feature_mode = self.feature_mode.as_str(),
            outcome = ?self.outcome,
            elapsed_ms = self.elapsed_ms,
            budget_ms = self.budget_ms,
            deadline_reached = self.deadline_reached,
            cancelled = self.cancelled,
            completed_stage_count = self.completed_stage_count,
            empty_stage_count = self.empty_stage_count,
            timed_out_stage_count = self.timed_out_stage_count,
            error_stage_count = self.error_stage_count,
            cancelled_stage_count = self.cancelled_stage_count,
            contribution_count = self.contribution_count,
            contribution_bytes = self.contribution_bytes,
            deduplicated_evidence_count = self.deduplicated_evidence_count,
            evidence_conflict_count = self.evidence_conflict_count,
            omitted_by_budget_count = self.omitted_by_budget_count,
            retrieval_backends = ?self.retrieval_backends,
            "Staged context retrieval outcome"
        );
        for stage in &self.stages {
            tracing::info!(
                event = "context_retrieval.stage",
                schema_version = self.schema_version,
                checkpoint = ?self.checkpoint,
                surface = self.surface.as_str(),
                feature_mode = self.feature_mode.as_str(),
                stage = ?stage.stage,
                status = ?stage.status,
                elapsed_ms = stage.elapsed_ms,
                error_code = ?stage.error_code,
                error_retryable = ?stage.error_retryable,
                candidate_count = stage.candidate_count,
                produced_contribution_count = stage.produced_contribution_count,
                accepted_contribution_count = stage.accepted_contribution_count,
                discarded_duplicate_count = stage.discarded_duplicate_count,
                omitted_by_budget_count = stage.omitted_by_budget_count,
                "Staged context retrieval stage"
            );
        }
    }
}

fn bounded_backend_label(value: &str) -> String {
    let trimmed = value.trim();
    if !trimmed.is_empty()
        && trimmed.len() <= 64
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'.'))
    {
        trimmed.to_string()
    } else {
        "other".to_string()
    }
}

fn count_stage_state(stages: &[ContextStageObservation], state: ContextStageState) -> usize {
    stages.iter().filter(|stage| stage.status == state).count()
}

fn classify_observed_context_outcome(
    cancelled: bool,
    stages: &[ContextStageObservation],
) -> ContextRetrievalState {
    if cancelled {
        return ContextRetrievalState::Cancelled;
    }
    let completed = stages
        .iter()
        .any(|stage| stage.status == ContextStageState::Completed);
    let interrupted = stages.iter().any(|stage| {
        matches!(
            stage.status,
            ContextStageState::TimedOut | ContextStageState::Error | ContextStageState::Cancelled
        )
    });
    if completed {
        return if interrupted {
            ContextRetrievalState::Partial
        } else {
            ContextRetrievalState::Ready
        };
    }
    if stages
        .iter()
        .any(|stage| stage.status == ContextStageState::TimedOut)
    {
        ContextRetrievalState::TimedOut
    } else if stages
        .iter()
        .any(|stage| stage.status == ContextStageState::Error)
    {
        ContextRetrievalState::Failed
    } else {
        ContextRetrievalState::Empty
    }
}

pub async fn retrieve_staged_pair<AF, BF, AO, BO>(
    first: AF,
    second: BF,
    deadline: Instant,
    cancellation: CancellationToken,
) -> StagedPairOutcome<AO::Value, BO::Value>
where
    AF: Future<Output = AO>,
    BF: Future<Output = BO>,
    AO: IntoStagedValue,
    BO: IntoStagedValue,
{
    let started = Instant::now();
    let first_started = Instant::now();
    let second_started = Instant::now();
    tokio::pin!(first);
    tokio::pin!(second);
    let deadline_sleep = tokio::time::sleep_until(deadline);
    tokio::pin!(deadline_sleep);

    let mut first_value = None;
    let mut second_value = None;
    let mut first_elapsed_ms = 0.0;
    let mut second_elapsed_ms = 0.0;
    let mut first_status = None;
    let mut second_status = None;
    let mut first_error_code = None;
    let mut second_error_code = None;
    let mut first_error_retryable = None;
    let mut second_error_retryable = None;
    // Always poll the supplied futures once through the biased select. An
    // already-ready value belongs to this checkpoint even when the deadline
    // timer is also ready; cancellation remains the only higher-priority
    // fence.
    let mut deadline_reached = false;
    let mut cancelled = cancellation.is_cancelled();

    while !cancelled && !deadline_reached && (first_status.is_none() || second_status.is_none()) {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                cancelled = true;
            }
            value = &mut first, if first_status.is_none() => {
                first_elapsed_ms = duration_ms(first_started.elapsed());
                match value.into_staged_value() {
                    StagedValue::Completed(value) => {
                        first_value = Some(value);
                        first_status = Some(ContextStageState::Completed);
                    },
                    StagedValue::Empty => first_status = Some(ContextStageState::Empty),
                    StagedValue::Error { code, retryable } => {
                        first_status = Some(ContextStageState::Error);
                        first_error_code = Some(code);
                        first_error_retryable = Some(retryable);
                    },
                }
            }
            value = &mut second, if second_status.is_none() => {
                second_elapsed_ms = duration_ms(second_started.elapsed());
                match value.into_staged_value() {
                    StagedValue::Completed(value) => {
                        second_value = Some(value);
                        second_status = Some(ContextStageState::Completed);
                    },
                    StagedValue::Empty => second_status = Some(ContextStageState::Empty),
                    StagedValue::Error { code, retryable } => {
                        second_status = Some(ContextStageState::Error);
                        second_error_code = Some(code);
                        second_error_retryable = Some(retryable);
                    },
                }
            }
            _ = &mut deadline_sleep => {
                deadline_reached = true;
            }
        }
    }
    cancelled |= cancellation.is_cancelled();

    let unfinished = if cancelled {
        ContextStageState::Cancelled
    } else {
        ContextStageState::TimedOut
    };
    if cancelled {
        first_value = None;
        second_value = None;
    }
    StagedPairOutcome {
        first: first_value,
        second: second_value,
        first_status: first_status.unwrap_or_else(|| unfinished.clone()),
        second_status: second_status.unwrap_or(unfinished),
        first_elapsed_ms,
        second_elapsed_ms,
        first_error_code,
        second_error_code,
        first_error_retryable,
        second_error_retryable,
        elapsed_ms: duration_ms(started.elapsed()),
        deadline_reached,
        cancelled,
    }
}

pub async fn retrieve_staged_triple<AF, BF, CF, AO, BO, CO>(
    first: AF,
    second: BF,
    third: CF,
    deadline: Instant,
    cancellation: CancellationToken,
) -> StagedTripleOutcome<AO::Value, BO::Value, CO::Value>
where
    AF: Future<Output = AO>,
    BF: Future<Output = BO>,
    CF: Future<Output = CO>,
    AO: IntoStagedValue,
    BO: IntoStagedValue,
    CO: IntoStagedValue,
{
    let started = Instant::now();
    let first_started = Instant::now();
    let second_started = Instant::now();
    let third_started = Instant::now();
    tokio::pin!(first);
    tokio::pin!(second);
    tokio::pin!(third);
    let deadline_sleep = tokio::time::sleep_until(deadline);
    tokio::pin!(deadline_sleep);

    let mut first_value = None;
    let mut second_value = None;
    let mut third_value = None;
    let mut first_elapsed_ms = 0.0;
    let mut second_elapsed_ms = 0.0;
    let mut third_elapsed_ms = 0.0;
    let mut first_status = None;
    let mut second_status = None;
    let mut third_status = None;
    let mut first_error_code = None;
    let mut second_error_code = None;
    let mut third_error_code = None;
    let mut first_error_retryable = None;
    let mut second_error_retryable = None;
    let mut third_error_retryable = None;
    // Always poll the supplied futures once through the biased select. An
    // already-ready value belongs to this checkpoint even when the deadline
    // timer is also ready; cancellation remains the only higher-priority
    // fence.
    let mut deadline_reached = false;
    let mut cancelled = cancellation.is_cancelled();

    while !cancelled
        && !deadline_reached
        && (first_status.is_none() || second_status.is_none() || third_status.is_none())
    {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => cancelled = true,
            value = &mut first, if first_status.is_none() => {
                first_elapsed_ms = duration_ms(first_started.elapsed());
                match value.into_staged_value() {
                    StagedValue::Completed(value) => {
                        first_value = Some(value);
                        first_status = Some(ContextStageState::Completed);
                    },
                    StagedValue::Empty => first_status = Some(ContextStageState::Empty),
                    StagedValue::Error { code, retryable } => {
                        first_status = Some(ContextStageState::Error);
                        first_error_code = Some(code);
                        first_error_retryable = Some(retryable);
                    },
                }
            }
            value = &mut second, if second_status.is_none() => {
                second_elapsed_ms = duration_ms(second_started.elapsed());
                match value.into_staged_value() {
                    StagedValue::Completed(value) => {
                        second_value = Some(value);
                        second_status = Some(ContextStageState::Completed);
                    },
                    StagedValue::Empty => second_status = Some(ContextStageState::Empty),
                    StagedValue::Error { code, retryable } => {
                        second_status = Some(ContextStageState::Error);
                        second_error_code = Some(code);
                        second_error_retryable = Some(retryable);
                    },
                }
            }
            value = &mut third, if third_status.is_none() => {
                third_elapsed_ms = duration_ms(third_started.elapsed());
                match value.into_staged_value() {
                    StagedValue::Completed(value) => {
                        third_value = Some(value);
                        third_status = Some(ContextStageState::Completed);
                    },
                    StagedValue::Empty => third_status = Some(ContextStageState::Empty),
                    StagedValue::Error { code, retryable } => {
                        third_status = Some(ContextStageState::Error);
                        third_error_code = Some(code);
                        third_error_retryable = Some(retryable);
                    },
                }
            }
            _ = &mut deadline_sleep => deadline_reached = true,
        }
    }
    cancelled |= cancellation.is_cancelled();

    let unfinished = if cancelled {
        ContextStageState::Cancelled
    } else {
        ContextStageState::TimedOut
    };
    // Once the turn is cancelled no completed value remains eligible for a
    // provider prompt. Statuses stay truthful for metrics, but values are
    // discarded so callers cannot accidentally inject them into a newer turn.
    if cancelled {
        first_value = None;
        second_value = None;
        third_value = None;
    }
    StagedTripleOutcome {
        first: first_value,
        second: second_value,
        third: third_value,
        first_status: first_status.unwrap_or_else(|| unfinished.clone()),
        second_status: second_status.unwrap_or_else(|| unfinished.clone()),
        third_status: third_status.unwrap_or(unfinished),
        first_elapsed_ms,
        second_elapsed_ms,
        third_elapsed_ms,
        first_error_code,
        second_error_code,
        third_error_code,
        first_error_retryable,
        second_error_retryable,
        third_error_retryable,
        elapsed_ms: duration_ms(started.elapsed()),
        deadline_reached,
        cancelled,
    }
}

/// Production borrow-friendly coordinator. Unlike the low-level timing
/// adapter, this validates the authenticated request, stamps every completed
/// contribution with the exact turn/query binding, applies deterministic
/// evidence deduplication and atomic output budgets, and exposes the exact
/// complete-only reuse key. It intentionally does not cache values itself.
#[allow(clippy::too_many_arguments)]
pub async fn retrieve_bound_staged_triple<AF, BF, CF, AO, BO, CO>(
    request: ContextRetrievalRequest,
    policy: ContextRetrievalPolicy,
    cancellation: CancellationToken,
    first_stage: ContextStageDescriptor,
    first: AF,
    second_stage: ContextStageDescriptor,
    second: BF,
    third_stage: ContextStageDescriptor,
    third: CF,
) -> Result<BoundStagedTripleOutcome<AO::Value, BO::Value, CO::Value>, ContextCoordinatorError>
where
    AF: Future<Output = AO>,
    BF: Future<Output = BO>,
    CF: Future<Output = CO>,
    AO: IntoStagedValue,
    BO: IntoStagedValue,
    CO: IntoStagedValue,
    AO::Value: ContextContributionSource,
    BO::Value: ContextContributionSource,
    CO::Value: ContextContributionSource,
{
    request.validate()?;
    policy.validate()?;
    let mut unique = BTreeSet::new();
    for descriptor in [&first_stage, &second_stage, &third_stage] {
        descriptor.validate()?;
        if !unique.insert(descriptor.clone()) {
            return Err(ContextCoordinatorError::DuplicateStage {
                stage: descriptor.name.clone(),
            });
        }
    }

    let turn = request.turn_binding();
    let reuse_key = ContextReuseKey::for_request(&request, &policy)?;
    let staged = retrieve_staged_triple(first, second, third, policy.deadline, cancellation).await;
    let mut statuses = BTreeMap::new();
    let mut candidates = Vec::new();
    record_borrowed_stage(
        first_stage.clone(),
        staged.first.as_ref(),
        staged.first_status.clone(),
        staged.first_elapsed_ms,
        staged.first_error_code.clone(),
        staged.first_error_retryable,
        &turn,
        &mut statuses,
        &mut candidates,
    );
    record_borrowed_stage(
        second_stage.clone(),
        staged.second.as_ref(),
        staged.second_status.clone(),
        staged.second_elapsed_ms,
        staged.second_error_code.clone(),
        staged.second_error_retryable,
        &turn,
        &mut statuses,
        &mut candidates,
    );
    record_borrowed_stage(
        third_stage.clone(),
        staged.third.as_ref(),
        staged.third_status.clone(),
        staged.third_elapsed_ms,
        staged.third_error_code.clone(),
        staged.third_error_retryable,
        &turn,
        &mut statuses,
        &mut candidates,
    );

    let (
        contributions,
        deduplicated_evidence_count,
        omitted_by_budget_count,
        evidence_conflict_count,
    ) = if staged.cancelled {
        (Vec::new(), 0, 0, 0)
    } else {
        merge_candidates(candidates, &policy, &mut statuses)
    };
    let stage_statuses = statuses.into_values().collect::<Vec<_>>();
    let state = overall_state(
        staged.cancelled,
        &contributions,
        &stage_statuses,
        omitted_by_budget_count,
    );
    let canonical = ContextRetrievalOutcome {
        state,
        turn,
        reuse_key,
        contributions,
        stage_statuses,
        elapsed_ms: staged.elapsed_ms,
        deadline_reached: staged.deadline_reached,
        cancelled: staged.cancelled,
        deduplicated_evidence_count,
        evidence_conflict_count,
        omitted_by_budget_count,
    };

    let first_usable = canonical_stage_value_is_usable(&canonical, &first_stage);
    let second_usable = canonical_stage_value_is_usable(&canonical, &second_stage);
    let third_usable = canonical_stage_value_is_usable(&canonical, &third_stage);
    let first_status = canonical_stage_state(&canonical, &first_stage);
    let second_status = canonical_stage_state(&canonical, &second_stage);
    let third_status = canonical_stage_state(&canonical, &third_stage);
    Ok(BoundStagedTripleOutcome {
        first: staged.first.filter(|_| first_usable),
        second: staged.second.filter(|_| second_usable),
        third: staged.third.filter(|_| third_usable),
        first_status,
        second_status,
        third_status,
        first_elapsed_ms: staged.first_elapsed_ms,
        second_elapsed_ms: staged.second_elapsed_ms,
        third_elapsed_ms: staged.third_elapsed_ms,
        elapsed_ms: staged.elapsed_ms,
        deadline_reached: staged.deadline_reached,
        cancelled: staged.cancelled,
        canonical,
    })
}

#[allow(clippy::too_many_arguments)]
fn record_borrowed_stage<T: ContextContributionSource>(
    descriptor: ContextStageDescriptor,
    value: Option<&T>,
    state: ContextStageState,
    elapsed_ms: f64,
    error_code: Option<String>,
    error_retryable: Option<bool>,
    turn: &ContextTurnBinding,
    statuses: &mut BTreeMap<ContextStageDescriptor, ContextStageStatus>,
    candidates: &mut Vec<CandidateContribution>,
) {
    let mut drafts = value
        .map(ContextContributionSource::context_contributions)
        .unwrap_or_default();
    if matches!(state, ContextStageState::Completed) && drafts.is_empty() {
        statuses.insert(
            descriptor.clone(),
            ContextStageStatus {
                stage: descriptor,
                state: ContextStageState::Error,
                elapsed_ms,
                candidate_count: 0,
                produced_contribution_count: 0,
                accepted_contribution_count: 0,
                discarded_duplicate_count: 0,
                omitted_by_budget_count: 0,
                error_code: Some("completed_stage_without_contribution".to_string()),
                error_retryable: Some(false),
            },
        );
        return;
    }
    if let Some(error) = drafts.iter().find_map(|draft| draft.validate().err()) {
        statuses.insert(
            descriptor.clone(),
            ContextStageStatus {
                stage: descriptor,
                state: ContextStageState::Error,
                elapsed_ms,
                candidate_count: 0,
                produced_contribution_count: 0,
                accepted_contribution_count: 0,
                discarded_duplicate_count: 0,
                omitted_by_budget_count: 0,
                error_code: Some(error.code),
                error_retryable: Some(error.retryable),
            },
        );
        return;
    }
    for draft in &mut drafts {
        draft.evidence_refs.sort();
        draft.evidence_refs.dedup();
    }
    let candidate_count = drafts
        .iter()
        .map(|draft| draft.candidate_count)
        .fold(0usize, usize::saturating_add);
    let produced_contribution_count = drafts.len();
    statuses.insert(
        descriptor.clone(),
        ContextStageStatus {
            stage: descriptor.clone(),
            state,
            elapsed_ms,
            candidate_count,
            produced_contribution_count,
            accepted_contribution_count: 0,
            discarded_duplicate_count: 0,
            omitted_by_budget_count: 0,
            error_code,
            error_retryable,
        },
    );
    if !matches!(state, ContextStageState::Completed) {
        return;
    }
    candidates.extend(drafts.into_iter().map(|draft| CandidateContribution {
        contribution: ContextContribution {
            turn: turn.clone(),
            stage: descriptor.clone(),
            evidence_key: draft.evidence_key,
            text: draft.text,
            evidence_refs: draft.evidence_refs,
            candidate_count: draft.candidate_count,
            retrieval_backend: draft.retrieval_backend,
            completeness: draft.completeness,
            rank: draft.rank,
            completed_at_ms: elapsed_ms,
        },
    }));
}

/// Run all stages concurrently under one absolute deadline.
pub async fn retrieve_turn_context(
    request: ContextRetrievalRequest,
    policy: ContextRetrievalPolicy,
    cancellation: CancellationToken,
    stages: Vec<Arc<dyn ContextRetrievalStage>>,
) -> Result<ContextRetrievalOutcome, ContextCoordinatorError> {
    request.validate()?;
    policy.validate()?;

    let mut descriptors = BTreeSet::new();
    let mut stage_names = BTreeSet::new();
    let mut ordered_stages = Vec::with_capacity(stages.len());
    for stage in stages {
        let descriptor = stage.descriptor();
        descriptor.validate()?;
        if !stage_names.insert(descriptor.name.clone()) || !descriptors.insert(descriptor.clone()) {
            return Err(ContextCoordinatorError::DuplicateStage {
                stage: descriptor.name,
            });
        }
        ordered_stages.push((descriptor, stage));
    }
    ordered_stages.sort_by(|left, right| left.0.cmp(&right.0));

    let started = Instant::now();
    let turn = request.turn_binding();
    let reuse_key = ContextReuseKey::for_request(&request, &policy)?;
    let request = Arc::new(request);
    let operation_cancellation = cancellation.child_token();
    let (completion_tx, mut completion_rx) = mpsc::unbounded_channel();
    let mut tasks = JoinSet::new();
    let mut pending = descriptors;

    let already_cancelled = cancellation.is_cancelled();
    let deadline_already_reached = Instant::now() >= policy.deadline;
    if !already_cancelled && !deadline_already_reached {
        for (descriptor, stage) in ordered_stages {
            let context = ContextStageExecutionContext {
                request: Arc::clone(&request),
                deadline: policy.deadline,
                cancellation: operation_cancellation.child_token(),
            };
            let completion_tx = completion_tx.clone();
            tasks.spawn(async move {
                let stage_started = Instant::now();
                let result = if context.should_stop() {
                    Err(ContextStageError::new(
                        "stage_not_started_after_fence",
                        true,
                        "turn cancellation or deadline was reached before stage start",
                    ))
                } else {
                    AssertUnwindSafe(stage.retrieve(context))
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|_| {
                            Err(ContextStageError::new(
                                "stage_panicked",
                                false,
                                "retrieval stage panicked",
                            ))
                        })
                };
                let completed_at = Instant::now();
                let _ = completion_tx.send(CompletedStage {
                    descriptor,
                    result,
                    elapsed: stage_started.elapsed(),
                    completed_at,
                });
            });
        }
    }
    drop(completion_tx);

    let mut statuses = BTreeMap::<ContextStageDescriptor, ContextStageStatus>::new();
    let mut candidates = Vec::<CandidateContribution>::new();
    let mut deadline_reached = deadline_already_reached;
    let mut cancelled = already_cancelled;

    if !cancelled && !deadline_reached && !pending.is_empty() {
        let deadline_sleep = tokio::time::sleep_until(policy.deadline);
        tokio::pin!(deadline_sleep);
        loop {
            if pending.is_empty() {
                break;
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    cancelled = true;
                    drain_ready_completions(
                        &mut completion_rx,
                        policy.deadline,
                        started,
                        &turn,
                        &mut pending,
                        &mut statuses,
                        &mut candidates,
                    );
                    break;
                }
                _ = &mut deadline_sleep => {
                    deadline_reached = true;
                    drain_ready_completions(
                        &mut completion_rx,
                        policy.deadline,
                        started,
                        &turn,
                        &mut pending,
                        &mut statuses,
                        &mut candidates,
                    );
                    break;
                }
                completion = completion_rx.recv() => {
                    let Some(completion) = completion else {
                        break;
                    };
                    record_completion(
                        completion,
                        policy.deadline,
                        started,
                        &turn,
                        &mut pending,
                        &mut statuses,
                        &mut candidates,
                    );
                }
            }
        }
    }

    operation_cancellation.cancel();
    tasks.abort_all();

    let unfinished_state = if cancelled {
        ContextStageState::Cancelled
    } else if deadline_reached {
        ContextStageState::TimedOut
    } else {
        ContextStageState::Error
    };
    let unfinished_code = if matches!(unfinished_state, ContextStageState::Error) {
        Some("stage_task_ended_without_result".to_string())
    } else {
        None
    };
    let elapsed_ms = duration_ms(started.elapsed());
    for descriptor in pending {
        let mut status = ContextStageStatus::unfinished(
            descriptor.clone(),
            unfinished_state.clone(),
            elapsed_ms,
        );
        status.error_code = unfinished_code.clone();
        status.error_retryable = unfinished_code.as_ref().map(|_| false);
        statuses.insert(descriptor, status);
    }

    // Cancelled turns expose no context, even if a stage completed just before
    // cancellation was observed. This prevents a caller from accidentally
    // injecting stale evidence into a replacement turn.
    let (
        contributions,
        deduplicated_evidence_count,
        omitted_by_budget_count,
        evidence_conflict_count,
    ) = if cancelled {
        (Vec::new(), 0, 0, 0)
    } else {
        merge_candidates(candidates, &policy, &mut statuses)
    };

    let stage_statuses = statuses.into_values().collect::<Vec<_>>();
    let state = overall_state(
        cancelled,
        &contributions,
        &stage_statuses,
        omitted_by_budget_count,
    );

    Ok(ContextRetrievalOutcome {
        state,
        turn,
        reuse_key,
        contributions,
        stage_statuses,
        elapsed_ms: duration_ms(started.elapsed()),
        deadline_reached,
        cancelled,
        deduplicated_evidence_count,
        evidence_conflict_count,
        omitted_by_budget_count,
    })
}

struct CompletedStage {
    descriptor: ContextStageDescriptor,
    result: Result<Vec<ContextContributionDraft>, ContextStageError>,
    elapsed: Duration,
    completed_at: Instant,
}

struct CandidateContribution {
    contribution: ContextContribution,
}

fn drain_ready_completions(
    completion_rx: &mut mpsc::UnboundedReceiver<CompletedStage>,
    deadline: Instant,
    started: Instant,
    turn: &ContextTurnBinding,
    pending: &mut BTreeSet<ContextStageDescriptor>,
    statuses: &mut BTreeMap<ContextStageDescriptor, ContextStageStatus>,
    candidates: &mut Vec<CandidateContribution>,
) {
    while let Ok(completion) = completion_rx.try_recv() {
        record_completion(
            completion, deadline, started, turn, pending, statuses, candidates,
        );
    }
}

fn record_completion(
    completion: CompletedStage,
    deadline: Instant,
    started: Instant,
    turn: &ContextTurnBinding,
    pending: &mut BTreeSet<ContextStageDescriptor>,
    statuses: &mut BTreeMap<ContextStageDescriptor, ContextStageStatus>,
    candidates: &mut Vec<CandidateContribution>,
) {
    if !pending.remove(&completion.descriptor) {
        return;
    }
    if completion.completed_at > deadline {
        statuses.insert(
            completion.descriptor.clone(),
            ContextStageStatus::unfinished(
                completion.descriptor,
                ContextStageState::TimedOut,
                duration_ms(completion.elapsed),
            ),
        );
        return;
    }

    match completion.result {
        Ok(mut drafts) => {
            if let Some(error) = drafts.iter().find_map(|draft| draft.validate().err()) {
                let mut status = ContextStageStatus::unfinished(
                    completion.descriptor.clone(),
                    ContextStageState::Error,
                    duration_ms(completion.elapsed),
                );
                status.error_code = Some(error.code);
                status.error_retryable = Some(error.retryable);
                statuses.insert(completion.descriptor, status);
                return;
            }

            for draft in &mut drafts {
                draft.evidence_refs.sort();
                draft.evidence_refs.dedup();
            }
            let candidate_count = drafts
                .iter()
                .map(|draft| draft.candidate_count)
                .fold(0usize, usize::saturating_add);
            let produced_contribution_count = drafts.len();
            let state = if drafts.is_empty() {
                ContextStageState::Empty
            } else {
                ContextStageState::Completed
            };
            statuses.insert(
                completion.descriptor.clone(),
                ContextStageStatus {
                    stage: completion.descriptor.clone(),
                    state,
                    elapsed_ms: duration_ms(completion.elapsed),
                    candidate_count,
                    produced_contribution_count,
                    accepted_contribution_count: 0,
                    discarded_duplicate_count: 0,
                    omitted_by_budget_count: 0,
                    error_code: None,
                    error_retryable: None,
                },
            );
            candidates.extend(drafts.into_iter().map(|draft| CandidateContribution {
                contribution: ContextContribution {
                    turn: turn.clone(),
                    stage: completion.descriptor.clone(),
                    evidence_key: draft.evidence_key,
                    text: draft.text,
                    evidence_refs: draft.evidence_refs,
                    candidate_count: draft.candidate_count,
                    retrieval_backend: draft.retrieval_backend,
                    completeness: draft.completeness,
                    rank: draft.rank,
                    completed_at_ms: duration_ms(
                        completion.completed_at.saturating_duration_since(started),
                    ),
                },
            }));
        },
        Err(error) => {
            let mut status = ContextStageStatus::unfinished(
                completion.descriptor.clone(),
                ContextStageState::Error,
                duration_ms(completion.elapsed),
            );
            status.error_code = Some(error.code);
            status.error_retryable = Some(error.retryable);
            statuses.insert(completion.descriptor, status);
        },
    }
}

fn merge_candidates(
    mut candidates: Vec<CandidateContribution>,
    policy: &ContextRetrievalPolicy,
    statuses: &mut BTreeMap<ContextStageDescriptor, ContextStageStatus>,
) -> (Vec<ContextContribution>, usize, usize, usize) {
    // Group identical evidence first, then put the deterministic winner first:
    // producer-declared completeness, rank, and stable stage order. Text size
    // is deliberately not a quality signal: a longer contradictory value is
    // not more authoritative.
    candidates.sort_by(|left, right| {
        left.contribution
            .evidence_key
            .cmp(&right.contribution.evidence_key)
            .then_with(|| {
                right
                    .contribution
                    .completeness
                    .cmp(&left.contribution.completeness)
            })
            .then_with(|| left.contribution.rank.cmp(&right.contribution.rank))
            .then_with(|| left.contribution.stage.cmp(&right.contribution.stage))
            .then_with(|| left.contribution.text.cmp(&right.contribution.text))
    });

    let mut winners = Vec::new();
    let mut deduplicated = 0;
    let mut evidence_conflicts = 0;
    while !candidates.is_empty() {
        let key = candidates[0].contribution.evidence_key.clone();
        let group_len = candidates
            .iter()
            .take_while(|candidate| candidate.contribution.evidence_key == key)
            .count();
        let mut group = candidates.drain(..group_len).collect::<Vec<_>>();
        let winning_completeness = group[0].contribution.completeness;
        let same_completeness = group
            .iter()
            .take_while(|candidate| candidate.contribution.completeness == winning_completeness)
            .collect::<Vec<_>>();
        let has_conflict = same_completeness
            .iter()
            .skip(1)
            .any(|candidate| candidate.contribution.text != same_completeness[0].contribution.text);
        if has_conflict {
            evidence_conflicts += 1;
            for candidate in group {
                if let Some(status) = statuses.get_mut(&candidate.contribution.stage) {
                    status.state = ContextStageState::Error;
                    status.error_code = Some("evidence_revision_conflict".to_string());
                    status.error_retryable = Some(false);
                }
            }
            continue;
        }

        let mut winner = group.remove(0).contribution;
        // Different authorized references to the same declared content are
        // complementary provenance, not contradictory evidence. Preserve the
        // union deterministically instead of selecting whichever happened to
        // have the longest serialized representation.
        winner.evidence_refs.extend(
            group
                .iter()
                .flat_map(|duplicate| duplicate.contribution.evidence_refs.iter().cloned()),
        );
        winner.evidence_refs.sort();
        winner.evidence_refs.dedup();
        for duplicate in group {
            let duplicate = duplicate.contribution;
            if let Some(status) = statuses.get_mut(&duplicate.stage) {
                status.discarded_duplicate_count += 1;
            }
            deduplicated += 1;
        }
        winners.push(winner);
    }

    winners.sort_by(|left, right| {
        left.stage
            .cmp(&right.stage)
            .then_with(|| left.rank.cmp(&right.rank))
            .then_with(|| left.evidence_key.cmp(&right.evidence_key))
            .then_with(|| left.text.cmp(&right.text))
    });

    let mut accepted = Vec::new();
    // Account for the surrounding JSON array. Each subsequent value also
    // needs one comma byte.
    let mut used_bytes = 2usize;
    let mut omitted = 0usize;
    for contribution in winners {
        let contribution_bytes = contribution.estimated_size_bytes();
        let fits_count = accepted.len() < policy.max_contributions;
        let separator_bytes = usize::from(!accepted.is_empty());
        let fits_bytes = used_bytes
            .checked_add(separator_bytes)
            .and_then(|next| next.checked_add(contribution_bytes))
            .is_some_and(|next| next <= policy.max_total_bytes);
        if fits_count && fits_bytes {
            used_bytes += separator_bytes + contribution_bytes;
            if let Some(status) = statuses.get_mut(&contribution.stage) {
                status.accepted_contribution_count += 1;
            }
            accepted.push(contribution);
        } else {
            omitted += 1;
            if let Some(status) = statuses.get_mut(&contribution.stage) {
                status.omitted_by_budget_count += 1;
            }
        }
    }

    (accepted, deduplicated, omitted, evidence_conflicts)
}

fn overall_state(
    cancelled: bool,
    contributions: &[ContextContribution],
    statuses: &[ContextStageStatus],
    omitted_by_budget_count: usize,
) -> ContextRetrievalState {
    if cancelled {
        return ContextRetrievalState::Cancelled;
    }
    let has_incomplete = statuses.iter().any(|status| {
        matches!(
            status.state,
            ContextStageState::TimedOut | ContextStageState::Error | ContextStageState::Cancelled
        )
    });
    if !contributions.is_empty() {
        return if has_incomplete || omitted_by_budget_count > 0 {
            ContextRetrievalState::Partial
        } else {
            ContextRetrievalState::Ready
        };
    }
    if omitted_by_budget_count > 0 {
        return ContextRetrievalState::Partial;
    }
    if statuses
        .iter()
        .any(|status| status.state == ContextStageState::TimedOut)
    {
        ContextRetrievalState::TimedOut
    } else if statuses
        .iter()
        .any(|status| status.state == ContextStageState::Error)
    {
        ContextRetrievalState::Failed
    } else {
        ContextRetrievalState::Empty
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    use async_trait::async_trait;
    use tokio::sync::Barrier;

    use super::*;

    #[derive(Clone)]
    enum TestResult {
        Contributions(Vec<ContextContributionDraft>),
        Error(ContextStageError),
        Panic,
    }

    struct TestStage {
        descriptor: ContextStageDescriptor,
        delay: Duration,
        result: TestResult,
        starts: Option<Arc<Barrier>>,
        invocation_count: Arc<AtomicUsize>,
        dropped: Option<Arc<AtomicBool>>,
        observe_cancellation: bool,
    }

    impl TestStage {
        fn new(kind: ContextStageKind, name: &str, delay: Duration, result: TestResult) -> Self {
            Self {
                descriptor: ContextStageDescriptor::new(kind, 0, name),
                delay,
                result,
                starts: None,
                invocation_count: Arc::new(AtomicUsize::new(0)),
                dropped: None,
                observe_cancellation: true,
            }
        }
    }

    struct DropSignal(Option<Arc<AtomicBool>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(dropped) = &self.0 {
                dropped.store(true, Ordering::SeqCst);
            }
        }
    }

    #[async_trait]
    impl ContextRetrievalStage for TestStage {
        fn descriptor(&self) -> ContextStageDescriptor {
            self.descriptor.clone()
        }

        async fn retrieve(
            &self,
            context: ContextStageExecutionContext,
        ) -> Result<Vec<ContextContributionDraft>, ContextStageError> {
            self.invocation_count.fetch_add(1, Ordering::SeqCst);
            let _drop_signal = DropSignal(self.dropped.clone());
            if let Some(starts) = &self.starts {
                starts.wait().await;
            }
            if self.observe_cancellation {
                tokio::select! {
                    _ = context.cancellation.cancelled() => {
                        return Err(ContextStageError::new("cancelled", true, "cancelled"));
                    }
                    _ = tokio::time::sleep(self.delay) => {}
                }
            } else {
                tokio::time::sleep(self.delay).await;
            }
            match &self.result {
                TestResult::Contributions(contributions) => Ok(contributions.clone()),
                TestResult::Error(error) => Err(error.clone()),
                TestResult::Panic => panic!("scripted stage panic"),
            }
        }
    }

    fn request(query: &str) -> ContextRetrievalRequest {
        ContextRetrievalRequest::new(
            "owner",
            "default",
            "personal-assistant",
            InvocationSurface::Chat,
            FeatureMode::None,
            "session-1",
            "turn-9",
            9,
            query,
            "authority-r7",
            BTreeMap::from([
                ("memory".to_string(), "memory-r2".to_string()),
                ("procedures".to_string(), "procedures-r4".to_string()),
            ]),
        )
    }

    fn draft(
        identity: &str,
        text: &str,
        completeness: ContextEvidenceCompleteness,
        rank: u32,
    ) -> ContextContributionDraft {
        ContextContributionDraft {
            evidence_key: ContextEvidenceKey::new(identity, "r1"),
            text: text.to_string(),
            evidence_refs: vec![format!("ref:{identity}")],
            candidate_count: 1,
            retrieval_backend: "test".to_string(),
            completeness,
            rank,
        }
    }

    fn stage_arc(stage: TestStage) -> Arc<dyn ContextRetrievalStage> {
        Arc::new(stage)
    }

    fn status<'a>(outcome: &'a ContextRetrievalOutcome, name: &str) -> &'a ContextStageStatus {
        outcome
            .stage_statuses
            .iter()
            .find(|status| status.stage.name == name)
            .expect("stage status")
    }

    #[tokio::test]
    async fn completed_stage_survives_slow_sibling_deadline() {
        let fast = TestStage::new(
            ContextStageKind::HybridMemory,
            "hybrid_memory",
            Duration::from_millis(2),
            TestResult::Contributions(vec![draft(
                "memory:fact",
                "The exact fact",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );
        let slow = TestStage::new(
            ContextStageKind::ReusableProcedures,
            "procedures",
            Duration::from_secs(10),
            TestResult::Contributions(vec![draft(
                "procedure:slow",
                "Slow procedure",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );

        let outcome = retrieve_turn_context(
            request("exact fact"),
            ContextRetrievalPolicy::with_budget(Duration::from_millis(250)),
            CancellationToken::new(),
            vec![stage_arc(slow), stage_arc(fast)],
        )
        .await
        .unwrap();

        assert_eq!(outcome.state, ContextRetrievalState::Partial);
        assert!(outcome.deadline_reached);
        assert_eq!(outcome.contributions.len(), 1);
        assert_eq!(outcome.contributions[0].text, "The exact fact");
        assert_eq!(
            status(&outcome, "hybrid_memory").state,
            ContextStageState::Completed
        );
        assert_eq!(
            status(&outcome, "procedures").state,
            ContextStageState::TimedOut
        );
        assert!(!outcome.is_complete_for_reuse());
    }

    #[tokio::test]
    async fn stages_start_concurrently_and_merge_by_semantic_order() {
        let starts = Arc::new(Barrier::new(3));
        let mut local = TestStage::new(
            ContextStageKind::TurnLocalState,
            "turn_local",
            Duration::from_millis(15),
            TestResult::Contributions(vec![draft(
                "local:1",
                "local",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );
        local.starts = Some(Arc::clone(&starts));
        let mut memory = TestStage::new(
            ContextStageKind::FastMemory,
            "fast_memory",
            Duration::from_millis(1),
            TestResult::Contributions(vec![draft(
                "memory:1",
                "memory",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );
        memory.starts = Some(Arc::clone(&starts));

        let run = tokio::spawn(retrieve_turn_context(
            request("concurrent"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
            vec![stage_arc(memory), stage_arc(local)],
        ));
        tokio::time::timeout(Duration::from_secs(1), starts.wait())
            .await
            .expect("both stages reached the barrier");
        let outcome = run.await.unwrap().unwrap();

        assert_eq!(outcome.state, ContextRetrievalState::Ready);
        assert_eq!(outcome.contributions[0].stage.name, "turn_local");
        assert_eq!(outcome.contributions[1].stage.name, "fast_memory");
    }

    #[tokio::test]
    async fn closure_stage_adapts_existing_retrieval_future() {
        let observed_generation = Arc::new(AtomicUsize::new(0));
        let observed_generation_for_stage = Arc::clone(&observed_generation);
        let stage = FnContextRetrievalStage::new(
            ContextStageDescriptor::new(ContextStageKind::FastMemory, 0, "closure_memory"),
            move |context: ContextStageExecutionContext| {
                let observed_generation = Arc::clone(&observed_generation_for_stage);
                async move {
                    observed_generation
                        .store(context.request.turn_generation as usize, Ordering::SeqCst);
                    assert!(!context.should_stop());
                    Ok(vec![draft(
                        "memory:closure",
                        "adapted result",
                        ContextEvidenceCompleteness::Complete,
                        0,
                    )])
                }
            },
        );

        let outcome = retrieve_turn_context(
            request("closure"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
            vec![Arc::new(stage)],
        )
        .await
        .unwrap();

        assert_eq!(observed_generation.load(Ordering::SeqCst), 9);
        assert_eq!(outcome.contributions[0].text, "adapted result");
    }

    #[tokio::test]
    async fn duplicate_evidence_prefers_complete_representation() {
        let excerpt = TestStage::new(
            ContextStageKind::FastMemory,
            "fast_memory",
            Duration::ZERO,
            TestResult::Contributions(vec![draft(
                "memory:shared",
                "partial",
                ContextEvidenceCompleteness::Excerpt,
                0,
            )]),
        );
        let complete = TestStage::new(
            ContextStageKind::HybridMemory,
            "hybrid_memory",
            Duration::from_millis(2),
            TestResult::Contributions(vec![draft(
                "memory:shared",
                "complete authorized fact",
                ContextEvidenceCompleteness::Complete,
                3,
            )]),
        );

        let outcome = retrieve_turn_context(
            request("shared"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
            vec![stage_arc(excerpt), stage_arc(complete)],
        )
        .await
        .unwrap();

        assert_eq!(outcome.contributions.len(), 1);
        assert_eq!(outcome.contributions[0].text, "complete authorized fact");
        assert_eq!(outcome.deduplicated_evidence_count, 1);
        assert_eq!(status(&outcome, "fast_memory").discarded_duplicate_count, 1);
    }

    #[tokio::test]
    async fn identical_evidence_merges_provenance_without_treating_refs_as_content() {
        let mut fast_draft = draft(
            "memory:shared",
            "birthday is 14 September",
            ContextEvidenceCompleteness::Complete,
            0,
        );
        fast_draft.evidence_refs = vec!["memory://fast/fact-1".to_string()];
        let mut hybrid_draft = fast_draft.clone();
        hybrid_draft.evidence_refs = vec!["memory://hybrid/fact-1".to_string()];
        let fast = TestStage::new(
            ContextStageKind::FastMemory,
            "fast_memory",
            Duration::ZERO,
            TestResult::Contributions(vec![fast_draft]),
        );
        let hybrid = TestStage::new(
            ContextStageKind::HybridMemory,
            "hybrid_memory",
            Duration::ZERO,
            TestResult::Contributions(vec![hybrid_draft]),
        );

        let outcome = retrieve_turn_context(
            request("shared provenance"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
            vec![stage_arc(hybrid), stage_arc(fast)],
        )
        .await
        .unwrap();

        assert_eq!(outcome.state, ContextRetrievalState::Ready);
        assert_eq!(outcome.evidence_conflict_count, 0);
        assert_eq!(outcome.deduplicated_evidence_count, 1);
        assert_eq!(outcome.contributions.len(), 1);
        assert_eq!(
            outcome.contributions[0].evidence_refs,
            vec![
                "memory://fast/fact-1".to_string(),
                "memory://hybrid/fact-1".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn contradictory_complete_evidence_for_one_revision_fails_visibly() {
        let first = TestStage::new(
            ContextStageKind::FastMemory,
            "fast_memory",
            Duration::ZERO,
            TestResult::Contributions(vec![draft(
                "memory:shared",
                "birthday is 14 September",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );
        let second = TestStage::new(
            ContextStageKind::HybridMemory,
            "hybrid_memory",
            Duration::ZERO,
            TestResult::Contributions(vec![draft(
                "memory:shared",
                "birthday is 15 September",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );

        let outcome = retrieve_turn_context(
            request("shared conflict"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
            vec![stage_arc(first), stage_arc(second)],
        )
        .await
        .unwrap();

        assert!(outcome.contributions.is_empty());
        assert_eq!(outcome.state, ContextRetrievalState::Failed);
        assert_eq!(outcome.evidence_conflict_count, 1);
        for stage_name in ["fast_memory", "hybrid_memory"] {
            let stage = status(&outcome, stage_name);
            assert_eq!(stage.state, ContextStageState::Error);
            assert_eq!(
                stage.error_code.as_deref(),
                Some("evidence_revision_conflict")
            );
        }
        let observation = ContextRetrievalObservation::from_outcome(
            ContextRetrievalCheckpoint::ChatTurn,
            1_000,
            &outcome,
        );
        assert_eq!(observation.evidence_conflict_count, 1);
    }

    #[tokio::test]
    async fn merge_is_byte_stable_when_completion_order_changes() {
        async fn run(first_delay: u64, second_delay: u64) -> Vec<ContextContribution> {
            let first = TestStage::new(
                ContextStageKind::HybridMemory,
                "memory_b",
                Duration::from_millis(first_delay),
                TestResult::Contributions(vec![draft(
                    "memory:b",
                    "B",
                    ContextEvidenceCompleteness::Complete,
                    1,
                )]),
            );
            let second = TestStage::new(
                ContextStageKind::HybridMemory,
                "memory_a",
                Duration::from_millis(second_delay),
                TestResult::Contributions(vec![draft(
                    "memory:a",
                    "A",
                    ContextEvidenceCompleteness::Complete,
                    0,
                )]),
            );
            retrieve_turn_context(
                request("stable"),
                ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
                CancellationToken::new(),
                vec![stage_arc(first), stage_arc(second)],
            )
            .await
            .unwrap()
            .contributions
        }

        let left = run(1, 10).await;
        let right = run(10, 1).await;
        let strip_timing = |mut values: Vec<ContextContribution>| {
            for value in &mut values {
                value.completed_at_ms = 0.0;
            }
            serde_json::to_vec(&values).unwrap()
        };
        assert_eq!(strip_timing(left), strip_timing(right));
    }

    #[tokio::test]
    async fn cancellation_discards_completed_context_and_aborts_siblings() {
        let dropped = Arc::new(AtomicBool::new(false));
        let fast = TestStage::new(
            ContextStageKind::FastMemory,
            "fast",
            Duration::ZERO,
            TestResult::Contributions(vec![draft(
                "memory:fast",
                "fast",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );
        let mut slow = TestStage::new(
            ContextStageKind::HybridMemory,
            "slow",
            Duration::from_secs(10),
            TestResult::Contributions(vec![draft(
                "memory:slow",
                "slow",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );
        slow.dropped = Some(Arc::clone(&dropped));
        let cancellation = CancellationToken::new();
        let cancellation_for_run = cancellation.clone();
        let run = tokio::spawn(retrieve_turn_context(
            request("cancel"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(5)),
            cancellation_for_run,
            vec![stage_arc(fast), stage_arc(slow)],
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancellation.cancel();
        let outcome = run.await.unwrap().unwrap();

        assert_eq!(outcome.state, ContextRetrievalState::Cancelled);
        assert!(outcome.cancelled);
        assert!(outcome.contributions.is_empty());
        tokio::task::yield_now().await;
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn already_expired_deadline_does_not_start_stages() {
        let stage = TestStage::new(
            ContextStageKind::FastMemory,
            "never_started",
            Duration::ZERO,
            TestResult::Contributions(Vec::new()),
        );
        let invocations = Arc::clone(&stage.invocation_count);
        let outcome = retrieve_turn_context(
            request("expired"),
            ContextRetrievalPolicy::until(Instant::now() - Duration::from_millis(1)),
            CancellationToken::new(),
            vec![stage_arc(stage)],
        )
        .await
        .unwrap();

        assert_eq!(outcome.state, ContextRetrievalState::TimedOut);
        assert_eq!(invocations.load(Ordering::SeqCst), 0);
        assert_eq!(
            status(&outcome, "never_started").state,
            ContextStageState::TimedOut
        );
    }

    #[tokio::test]
    async fn invalid_contribution_isolated_as_stage_error() {
        let invalid = TestStage::new(
            ContextStageKind::FastMemory,
            "invalid",
            Duration::ZERO,
            TestResult::Contributions(vec![draft(
                "memory:invalid",
                "   ",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );
        let valid = TestStage::new(
            ContextStageKind::ReusableProcedures,
            "valid",
            Duration::ZERO,
            TestResult::Contributions(vec![draft(
                "procedure:valid",
                "usable procedure",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );
        let outcome = retrieve_turn_context(
            request("validation"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
            vec![stage_arc(invalid), stage_arc(valid)],
        )
        .await
        .unwrap();

        assert_eq!(outcome.state, ContextRetrievalState::Partial);
        assert_eq!(outcome.contributions.len(), 1);
        assert_eq!(status(&outcome, "invalid").state, ContextStageState::Error);
        assert_eq!(
            status(&outcome, "invalid").error_code.as_deref(),
            Some("empty_contribution")
        );
    }

    #[tokio::test]
    async fn stage_error_and_panic_do_not_erase_successful_sibling() {
        let error = TestStage::new(
            ContextStageKind::FastMemory,
            "error",
            Duration::ZERO,
            TestResult::Error(ContextStageError::new(
                "backend_unavailable",
                true,
                "offline",
            )),
        );
        let panic = TestStage::new(
            ContextStageKind::HybridMemory,
            "panic",
            Duration::ZERO,
            TestResult::Panic,
        );
        let valid = TestStage::new(
            ContextStageKind::OptionalCheckpoint,
            "checkpoint",
            Duration::ZERO,
            TestResult::Contributions(vec![draft(
                "checkpoint:1",
                "checkpoint",
                ContextEvidenceCompleteness::Complete,
                0,
            )]),
        );

        let outcome = retrieve_turn_context(
            request("isolation"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
            vec![stage_arc(error), stage_arc(panic), stage_arc(valid)],
        )
        .await
        .unwrap();

        assert_eq!(outcome.state, ContextRetrievalState::Partial);
        assert_eq!(outcome.contributions[0].text, "checkpoint");
        assert_eq!(
            status(&outcome, "error").error_code.as_deref(),
            Some("backend_unavailable")
        );
        assert_eq!(
            status(&outcome, "panic").error_code.as_deref(),
            Some("stage_panicked")
        );
    }

    #[tokio::test]
    async fn contribution_budget_omits_whole_records_without_slicing_text() {
        let first = draft(
            "memory:first",
            "FIRST-COMPLETE-VALUE",
            ContextEvidenceCompleteness::Complete,
            0,
        );
        let second = draft(
            "memory:second",
            "SECOND-COMPLETE-VALUE",
            ContextEvidenceCompleteness::Complete,
            1,
        );
        let stage = TestStage::new(
            ContextStageKind::HybridMemory,
            "memory",
            Duration::ZERO,
            TestResult::Contributions(vec![first, second]),
        );
        let outcome = retrieve_turn_context(
            request("budget"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)).with_bounds(1, 1_000),
            CancellationToken::new(),
            vec![stage_arc(stage)],
        )
        .await
        .unwrap();

        assert_eq!(outcome.contributions.len(), 1);
        assert_eq!(outcome.contributions[0].text, "FIRST-COMPLETE-VALUE");
        assert_eq!(outcome.omitted_by_budget_count, 1);
        assert_eq!(status(&outcome, "memory").omitted_by_budget_count, 1);
    }

    #[tokio::test]
    async fn empty_stages_are_truthful_and_reusable() {
        let empty = TestStage::new(
            ContextStageKind::FastMemory,
            "empty",
            Duration::ZERO,
            TestResult::Contributions(Vec::new()),
        );
        let request = request("nothing");
        let policy = ContextRetrievalPolicy::with_budget(Duration::from_secs(1));
        let outcome = retrieve_turn_context(
            request.clone(),
            policy.clone(),
            CancellationToken::new(),
            vec![stage_arc(empty)],
        )
        .await
        .unwrap();

        assert_eq!(outcome.state, ContextRetrievalState::Empty);
        assert_eq!(status(&outcome, "empty").state, ContextStageState::Empty);
        assert!(outcome.is_complete_for_reuse());
        assert!(outcome.can_reuse_for(&request, &policy));

        let mut changed_revision = request;
        changed_revision.authority_revision = "authority-r8".to_string();
        assert!(!outcome.can_reuse_for(&changed_revision, &policy));
    }

    #[tokio::test]
    async fn reuse_key_is_exact_scope_query_and_revision_bound() {
        let base = request("Where is the contract?");
        let policy = ContextRetrievalPolicy::with_budget(Duration::from_secs(1));
        let key = ContextReuseKey::for_request(&base, &policy).unwrap();
        assert!(key.matches_request(&base));
        assert!(key.matches_policy(&policy));
        assert_eq!(
            key.fingerprint(),
            ContextReuseKey::for_request(&base, &policy)
                .unwrap()
                .fingerprint()
        );

        let mut different_query = base.clone();
        different_query.relevance_query.push(' ');
        assert!(!key.matches_request(&different_query));

        let mut different_scope = base.clone();
        different_scope.workspace = "other".to_string();
        assert!(!key.matches_request(&different_scope));

        let mut different_revision = base.clone();
        different_revision
            .source_revisions
            .insert("memory".to_string(), "memory-r3".to_string());
        assert!(!key.matches_request(&different_revision));

        let mut different_binding = base;
        different_binding.binding_id = "session-2".to_string();
        assert!(!key.matches_request(&different_binding));

        let different_bounds =
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)).with_bounds(8, 8_192);
        assert!(!key.matches_policy(&different_bounds));
        assert_ne!(
            key.fingerprint(),
            ContextReuseKey::for_request(&different_binding, &different_bounds)
                .unwrap()
                .fingerprint()
        );
    }

    #[test]
    fn request_debug_and_reuse_key_do_not_expose_query_text() {
        let request = request("private low entropy family fact");
        let policy = ContextRetrievalPolicy::with_budget(Duration::from_secs(1));
        let key = ContextReuseKey::for_request(&request, &policy).unwrap();

        let debug = format!("{request:?}");
        let serialized_key = serde_json::to_string(&key).unwrap();
        assert!(!debug.contains("private low entropy family fact"));
        assert!(!serialized_key.contains("private low entropy family fact"));
        assert_eq!(key.relevance_query_digest.len(), 64);
    }

    #[test]
    fn stage_error_codes_cannot_smuggle_user_content_into_metrics() {
        let error = ContextStageError::new(
            "provider_error\nprivate query: spouse birthday",
            true,
            "restricted detail remains available only to restricted logs",
        );

        assert_eq!(error.code, "invalid_stage_error_code");
        assert!(error.retryable);
        assert!(!error.code.contains("spouse"));
        assert_eq!(
            ContextStageError::new(" hybrid_timeout-2 ", false, "detail").code,
            "hybrid_timeout-2"
        );
    }

    #[tokio::test]
    async fn turn_binding_rejects_new_generation_or_query() {
        let base = request("first query");
        let binding = base.turn_binding();
        assert!(binding.matches_request(&base));

        let mut next_turn = base.clone();
        next_turn.turn_generation += 1;
        next_turn.turn_id = "turn-10".to_string();
        assert!(!binding.matches_request(&next_turn));

        let mut changed_query = base;
        changed_query.relevance_query = "second query".to_string();
        assert!(!binding.matches_request(&changed_query));
    }

    #[tokio::test]
    async fn duplicate_stage_descriptors_fail_before_any_stage_starts() {
        let first = TestStage::new(
            ContextStageKind::FastMemory,
            "duplicate",
            Duration::ZERO,
            TestResult::Contributions(Vec::new()),
        );
        let first_invocations = Arc::clone(&first.invocation_count);
        let second = TestStage::new(
            ContextStageKind::FastMemory,
            "duplicate",
            Duration::ZERO,
            TestResult::Contributions(Vec::new()),
        );
        let second_invocations = Arc::clone(&second.invocation_count);

        let error = retrieve_turn_context(
            request("duplicate"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
            vec![stage_arc(first), stage_arc(second)],
        )
        .await
        .unwrap_err();

        assert_eq!(
            error,
            ContextCoordinatorError::DuplicateStage {
                stage: "duplicate".to_string()
            }
        );
        assert_eq!(first_invocations.load(Ordering::SeqCst), 0);
        assert_eq!(second_invocations.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancelled_before_start_does_not_invoke_stages() {
        let stage = TestStage::new(
            ContextStageKind::FastMemory,
            "cancelled",
            Duration::ZERO,
            TestResult::Contributions(Vec::new()),
        );
        let invocations = Arc::clone(&stage.invocation_count);
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let outcome = retrieve_turn_context(
            request("cancelled"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            cancellation,
            vec![stage_arc(stage)],
        )
        .await
        .unwrap();

        assert_eq!(outcome.state, ContextRetrievalState::Cancelled);
        assert_eq!(invocations.load(Ordering::SeqCst), 0);
        assert!(outcome.contributions.is_empty());
    }

    #[tokio::test]
    async fn staged_triple_retains_fast_memory_when_siblings_reach_absolute_deadline() {
        let deadline = Instant::now() + Duration::from_millis(250);
        let outcome = retrieve_staged_triple(
            async { Some("fast exact fact") },
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Some("late hybrid fact")
            },
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Some("late procedure")
            },
            deadline,
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome.first, Some("fast exact fact"));
        assert!(outcome.second.is_none());
        assert!(outcome.third.is_none());
        assert_eq!(outcome.first_status, ContextStageState::Completed);
        assert_eq!(outcome.second_status, ContextStageState::TimedOut);
        assert_eq!(outcome.third_status, ContextStageState::TimedOut);
        assert!(outcome.deadline_reached);
        assert!(!outcome.cancelled);
    }

    #[tokio::test]
    async fn staged_triple_preserves_independent_error_empty_and_success_statuses() {
        let outcome = retrieve_staged_triple(
            async { StagedValue::<&'static str>::error("fast_snapshot_unavailable", true) },
            async { StagedValue::<&'static str>::Empty },
            async { StagedValue::Completed("usable procedure") },
            Instant::now() + Duration::from_secs(1),
            CancellationToken::new(),
        )
        .await;

        assert!(outcome.first.is_none());
        assert!(outcome.second.is_none());
        assert_eq!(outcome.third, Some("usable procedure"));
        assert_eq!(outcome.first_status, ContextStageState::Error);
        assert_eq!(outcome.second_status, ContextStageState::Empty);
        assert_eq!(outcome.third_status, ContextStageState::Completed);
        assert_eq!(
            outcome.first_error_code.as_deref(),
            Some("fast_snapshot_unavailable")
        );
        assert_eq!(outcome.first_error_retryable, Some(true));
        assert!(!outcome.deadline_reached);
    }

    #[tokio::test]
    async fn staged_triple_cancellation_discards_completed_value_and_late_siblings() {
        let (release_fast_tx, release_fast_rx) = tokio::sync::oneshot::channel();
        let (fast_completed_tx, fast_completed_rx) = tokio::sync::oneshot::channel();
        let cancellation = CancellationToken::new();
        let run_cancellation = cancellation.clone();
        let run = tokio::spawn(async move {
            retrieve_staged_triple(
                async move {
                    release_fast_rx.await.expect("release fast stage");
                    let _ = fast_completed_tx.send(());
                    Some("stale fast fact")
                },
                std::future::pending::<Option<&'static str>>(),
                std::future::pending::<Option<&'static str>>(),
                Instant::now() + Duration::from_secs(10),
                run_cancellation,
            )
            .await
        });

        release_fast_tx.send(()).expect("release fast stage");
        fast_completed_rx.await.expect("fast stage completed");
        tokio::task::yield_now().await;
        cancellation.cancel();
        let outcome = run.await.unwrap();

        assert!(outcome.cancelled);
        assert!(!outcome.deadline_reached);
        assert!(outcome.first.is_none());
        assert!(outcome.second.is_none());
        assert!(outcome.third.is_none());
        assert_eq!(outcome.first_status, ContextStageState::Completed);
        assert_eq!(outcome.second_status, ContextStageState::Cancelled);
        assert_eq!(outcome.third_status, ContextStageState::Cancelled);
    }

    #[tokio::test]
    async fn staged_pair_cancellation_also_discards_checkpoint_context() {
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        let cancellation = CancellationToken::new();
        let run_cancellation = cancellation.clone();
        let run = tokio::spawn(async move {
            retrieve_staged_pair(
                async move {
                    let _ = completed_tx.send(());
                    Some("completed memory")
                },
                std::future::pending::<Option<&'static str>>(),
                Instant::now() + Duration::from_secs(10),
                run_cancellation,
            )
            .await
        });

        completed_rx.await.expect("memory stage completed");
        tokio::task::yield_now().await;
        cancellation.cancel();
        let outcome = run.await.unwrap();

        assert!(outcome.cancelled);
        assert!(outcome.first.is_none());
        assert!(outcome.second.is_none());
        assert_eq!(outcome.first_status, ContextStageState::Completed);
        assert_eq!(outcome.second_status, ContextStageState::Cancelled);
    }

    #[tokio::test]
    async fn staged_pair_drains_results_ready_at_the_absolute_deadline() {
        let deadline = Instant::now();
        let outcome = retrieve_staged_pair(
            async { Some("first") },
            async { Some("second") },
            deadline,
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome.first, Some("first"));
        assert_eq!(outcome.second, Some("second"));
        assert_eq!(outcome.first_status, ContextStageState::Completed);
        assert_eq!(outcome.second_status, ContextStageState::Completed);
        assert!(!outcome.deadline_reached);
        assert!(!outcome.cancelled);
    }

    #[tokio::test]
    async fn staged_pair_keeps_completed_task_context_when_sibling_errors() {
        let outcome = retrieve_staged_pair(
            async { StagedValue::Completed("memory") },
            async { StagedValue::<&'static str>::error("procedure_backend_failed", true) },
            Instant::now() + Duration::from_secs(1),
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome.first, Some("memory"));
        assert!(outcome.second.is_none());
        assert_eq!(outcome.first_status, ContextStageState::Completed);
        assert_eq!(outcome.second_status, ContextStageState::Error);
        assert_eq!(
            outcome.second_error_code.as_deref(),
            Some("procedure_backend_failed")
        );
        assert_eq!(outcome.second_error_retryable, Some(true));
        let observation = ContextRetrievalObservation::from_pair(
            ContextRetrievalCheckpoint::AutonomousTaskCheckpoint,
            1_000,
            &outcome,
        );
        assert_eq!(observation.outcome, ContextRetrievalState::Partial);
        assert_eq!(observation.error_stage_count, 1);
    }

    #[tokio::test]
    async fn staged_triple_drains_results_ready_at_the_absolute_deadline() {
        let deadline = Instant::now();
        let outcome = retrieve_staged_triple(
            async { Some("first") },
            async { Some("second") },
            async { Some("third") },
            deadline,
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome.first, Some("first"));
        assert_eq!(outcome.second, Some("second"));
        assert_eq!(outcome.third, Some("third"));
        assert_eq!(outcome.first_status, ContextStageState::Completed);
        assert_eq!(outcome.second_status, ContextStageState::Completed);
        assert_eq!(outcome.third_status, ContextStageState::Completed);
        assert!(!outcome.deadline_reached);
        assert!(!outcome.cancelled);
    }

    #[tokio::test]
    async fn staged_triple_output_slots_are_deterministic_not_completion_ordered() {
        let outcome = retrieve_staged_triple(
            async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Some("first")
            },
            async {
                tokio::time::sleep(Duration::from_millis(1)).await;
                Some("second")
            },
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Some("third")
            },
            Instant::now() + Duration::from_secs(1),
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome.first, Some("first"));
        assert_eq!(outcome.second, Some("second"));
        assert_eq!(outcome.third, Some("third"));
        assert_eq!(outcome.first_status, ContextStageState::Completed);
        assert_eq!(outcome.second_status, ContextStageState::Completed);
        assert_eq!(outcome.third_status, ContextStageState::Completed);
        assert!(outcome.second_elapsed_ms < outcome.first_elapsed_ms);
    }

    #[test]
    fn staged_observation_contains_only_fixed_metadata_and_classifies_partial() {
        let outcome = StagedTripleOutcome {
            first: Some("PRIVATE-CONTEXT-MUST-NOT-APPEAR"),
            second: None::<&str>,
            third: None::<&str>,
            first_status: ContextStageState::Completed,
            second_status: ContextStageState::TimedOut,
            third_status: ContextStageState::Error,
            first_elapsed_ms: 4.0,
            second_elapsed_ms: 20.0,
            third_elapsed_ms: 8.0,
            first_error_code: None,
            second_error_code: None,
            third_error_code: Some("procedure_backend_unavailable".to_string()),
            first_error_retryable: None,
            second_error_retryable: None,
            third_error_retryable: Some(true),
            elapsed_ms: 20.0,
            deadline_reached: true,
            cancelled: false,
        };
        let observation = ContextRetrievalObservation::from_triple(
            ContextRetrievalCheckpoint::RealtimeVoiceTurn,
            20,
            &outcome,
        );
        let serialized = serde_json::to_string(&observation).unwrap();

        assert_eq!(observation.outcome, ContextRetrievalState::Partial);
        assert_eq!(observation.completed_stage_count, 1);
        assert_eq!(observation.timed_out_stage_count, 1);
        assert_eq!(observation.error_stage_count, 1);
        assert!(!serialized.contains("PRIVATE-CONTEXT-MUST-NOT-APPEAR"));
        assert!(!serialized.contains("relevance_query"));
        assert!(!serialized.contains("evidence_refs"));
        assert!(!serialized.contains("\"text\""));
    }

    #[test]
    fn observation_checkpoints_have_fixed_surface_mapping() {
        assert_eq!(
            ContextRetrievalCheckpoint::ChatTurn.surface(),
            InvocationSurface::Chat
        );
        assert_eq!(
            ContextRetrievalCheckpoint::RealtimeVoiceTurn.surface(),
            InvocationSurface::RealtimeVoice
        );
        assert_eq!(
            ContextRetrievalCheckpoint::AutonomousTaskCheckpoint.surface(),
            InvocationSurface::Task
        );
    }

    #[test]
    fn pre_stage_fences_emit_truthful_timeout_and_cancellation_rows() {
        let timed_out = ContextRetrievalObservation::setup_timed_out(
            ContextRetrievalCheckpoint::RealtimeVoiceTurn,
            300,
            301.0,
        );
        assert_eq!(timed_out.outcome, ContextRetrievalState::TimedOut);
        assert!(timed_out.deadline_reached);
        assert!(!timed_out.cancelled);
        assert_eq!(timed_out.timed_out_stage_count, 3);
        assert_eq!(timed_out.stages.len(), 3);

        let cancelled = ContextRetrievalObservation::setup_cancelled(
            ContextRetrievalCheckpoint::RealtimeVoiceTurn,
            300,
            12.0,
        );
        assert_eq!(cancelled.outcome, ContextRetrievalState::Cancelled);
        assert!(!cancelled.deadline_reached);
        assert!(cancelled.cancelled);
        assert_eq!(cancelled.cancelled_stage_count, 3);
    }

    #[tokio::test]
    async fn canonical_observation_preserves_authenticated_feature_surface() {
        let mut request = request("feature-bound context");
        request.surface = InvocationSurface::Tutor;
        request.feature_mode = FeatureMode::Tutor;
        let outcome = retrieve_bound_staged_triple(
            request,
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
            ContextStageDescriptor::new(ContextStageKind::FastMemory, 0, "fast"),
            async { Some("fast evidence") },
            ContextStageDescriptor::new(ContextStageKind::HybridMemory, 0, "hybrid"),
            async { None::<&'static str> },
            ContextStageDescriptor::new(ContextStageKind::ReusableProcedures, 0, "procedures"),
            async { None::<&'static str> },
        )
        .await
        .unwrap();

        let observation = ContextRetrievalObservation::from_outcome(
            ContextRetrievalCheckpoint::AutonomousTaskCheckpoint,
            1_000,
            &outcome.canonical,
        );
        assert_eq!(observation.surface, InvocationSurface::Tutor);
        assert_eq!(observation.feature_mode, FeatureMode::Tutor);
    }

    #[tokio::test]
    async fn bound_borrowed_production_seam_stamps_deduplicates_and_fences_reuse() {
        let request = request("same exact turn query");
        let policy = ContextRetrievalPolicy::with_budget(Duration::from_secs(1));
        let fast = ContextStageDescriptor::new(ContextStageKind::FastMemory, 0, "fast");
        let hybrid = ContextStageDescriptor::new(ContextStageKind::HybridMemory, 0, "hybrid");
        let procedures =
            ContextStageDescriptor::new(ContextStageKind::ReusableProcedures, 0, "procedures");
        let outcome = retrieve_bound_staged_triple(
            request.clone(),
            policy.clone(),
            CancellationToken::new(),
            fast.clone(),
            async { Some("shared memory") },
            hybrid.clone(),
            async { Some("shared memory") },
            procedures.clone(),
            async { Some("procedure evidence") },
        )
        .await
        .unwrap();

        assert!(outcome.canonical.is_bound_to(&request));
        assert!(outcome.canonical.can_reuse_for(&request, &policy));
        assert_eq!(outcome.canonical.deduplicated_evidence_count, 1);
        assert_eq!(outcome.first, Some("shared memory"));
        assert!(
            outcome.second.is_none(),
            "duplicate hybrid value is not injected twice"
        );
        assert_eq!(outcome.third, Some("procedure evidence"));
        assert!(outcome
            .canonical
            .contributions
            .iter()
            .all(|contribution| { contribution.turn == request.turn_binding() }));
        let observation = ContextRetrievalObservation::from_outcome(
            ContextRetrievalCheckpoint::ChatTurn,
            1_000,
            &outcome.canonical,
        );
        assert_eq!(observation.contribution_count, 2);
        assert!(observation.contribution_bytes > 0);
        assert_eq!(observation.deduplicated_evidence_count, 1);
        assert_eq!(observation.omitted_by_budget_count, 0);
        assert_eq!(observation.retrieval_backends, vec!["borrowed_stage"]);
        assert_eq!(
            observation
                .stages
                .iter()
                .map(|stage| stage.produced_contribution_count)
                .sum::<usize>(),
            3
        );

        let mut newer_turn = request;
        newer_turn.turn_generation += 1;
        newer_turn.turn_id = "turn-new".to_string();
        newer_turn.relevance_query = "different query".to_string();
        assert!(!outcome.canonical.can_reuse_for(&newer_turn, &policy));
    }
}
