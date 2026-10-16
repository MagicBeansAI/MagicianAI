//! # Artifact Lifecycle Types
//!
//! Canonical types for the artifact lifecycle control plane. These define the
//! metadata contract, lifecycle states, policy bindings, and domain taxonomy
//! that all lifecycle components share.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fmt;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Global unique identifier for an artifact in the lifecycle catalog.
pub type ArtifactUid = String;

/// The domain an artifact belongs to, determining which adapter handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDomain {
    /// Pipeline coordination artifacts (query analysis, plan graphs, etc.)
    Pipeline,
    /// Workflow step artifacts (step outputs with provenance)
    Workflow,
    /// Agentic execution artifacts (browser/tool outputs)
    Execution,
    /// Episode memory artifacts (structured outputs from episodes)
    Episode,
    /// Durable artifacts that persist across runs
    Durable,
}

impl fmt::Display for ArtifactDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactDomain::Pipeline => write!(f, "pipeline"),
            ArtifactDomain::Workflow => write!(f, "workflow"),
            ArtifactDomain::Execution => write!(f, "execution"),
            ArtifactDomain::Episode => write!(f, "episode"),
            ArtifactDomain::Durable => write!(f, "durable"),
        }
    }
}

/// Locator describing where the artifact payload physically lives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum PhysicalLocator {
    /// Pipeline artifact store file
    PipelineStore {
        chain_id: String,
        artifact_id: String,
    },
    /// Workflow run file
    WorkflowRun {
        instance_id: String,
        step_name: String,
        artifact_name: String,
    },
    /// Episode file on disk
    EpisodeFile {
        agent_id: String,
        episode_id: String,
    },
    /// In-memory only (not persisted outside catalog)
    InMemory { key: String },
    /// Durable artifact store file (persists across runs)
    DurableStore { namespace: String, name: String },
}

// ---------------------------------------------------------------------------
// Lifecycle State Machine
// ---------------------------------------------------------------------------

/// Lifecycle state of an artifact.
///
/// Transitions:
/// - `Active` → `Stale` (freshness window expires)
/// - `Stale` → `Active` (revalidated)
/// - `Stale` → `Expired` (stale window expires)
/// - `Active`/`Stale` → `Quarantined` (integrity failure)
/// - `Expired` → `DeletePending` (sweeper selects)
/// - `Quarantined` → `DeletePending` (operator action)
/// - `DeletePending` → `Deleted` (reference check passes + grace period)
/// - Any → `Active` (explicit revalidation, except Deleted)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// Eligible for normal consumption.
    Active,
    /// Readable for diagnostics but not valid for planning decisions
    /// unless revalidated.
    Stale,
    /// Not eligible for planning consumption; queued for cleanup unless
    /// protected by references.
    Expired,
    /// Integrity or sanitization concern; hidden from normal consumers.
    Quarantined,
    /// Replaced by a newer version of the same logical artifact.
    /// Not eligible for consumption or display, but retained for audit.
    Superseded,
    /// Selected by sweeper, awaiting grace period and reference check.
    DeletePending,
    /// Tombstoned or physically removed per policy.
    Deleted,
}

impl LifecycleState {
    /// Whether this state allows normal read access.
    pub fn is_readable(&self) -> bool {
        matches!(self, LifecycleState::Active | LifecycleState::Stale)
    }

    /// Whether this state is terminal (no further transitions expected).
    pub fn is_terminal(&self) -> bool {
        matches!(self, LifecycleState::Deleted)
    }

    /// Whether this artifact is eligible for planning/execution consumption.
    pub fn is_consumable(&self) -> bool {
        matches!(self, LifecycleState::Active)
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LifecycleState::Active => write!(f, "active"),
            LifecycleState::Stale => write!(f, "stale"),
            LifecycleState::Expired => write!(f, "expired"),
            LifecycleState::Quarantined => write!(f, "quarantined"),
            LifecycleState::Superseded => write!(f, "superseded"),
            LifecycleState::DeletePending => write!(f, "delete_pending"),
            LifecycleState::Deleted => write!(f, "deleted"),
        }
    }
}

/// Reason a lifecycle transition occurred, for audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionReason {
    /// Freshness window expired (active → stale).
    FreshnessExpired,
    /// Stale window expired (stale → expired).
    StaleWindowExpired,
    /// Explicitly revalidated by caller.
    Revalidated,
    /// Integrity check failed (deserialization, sanitization).
    IntegrityFailure { detail: String },
    /// Scope boundary crossed (task switch, thread close).
    ScopeBoundary { event: String },
    /// Resume mode triggered purge.
    ResumeMode { mode: String },
    /// Selected by lifecycle sweeper.
    SweeperSelected,
    /// Grace period completed and references cleared.
    GracePeriodCompleted,
    /// Operator or admin action.
    OperatorAction { reason: String },
    /// Replaced by a newer publication of the same logical surface.
    Superseded,
    /// Initial registration.
    Registered,
}

// ---------------------------------------------------------------------------
// Policy Bindings
// ---------------------------------------------------------------------------

/// Retention class determining how long an artifact is kept.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    /// Retained for the duration of the current task/thread.
    TaskLifetime,
    /// Retained for the duration of the workflow run.
    RunLifetime,
    /// Retained for the duration of the goal's execution.
    GoalLifetime,
    /// Explicit time-to-live in seconds.
    Ttl { seconds: u64 },
    /// Retained indefinitely (until explicit cleanup).
    Permanent,
}

/// Freshness class determining when an artifact becomes stale.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessClass {
    /// Fresh for a duration in seconds from production time.
    Window {
        fresh_seconds: u64,
        stale_seconds: u64,
    },
    /// Requires revalidation on resume.
    RevalidateOnResume,
    /// Always considered fresh (no staleness).
    Evergreen,
}

/// Exposure class determining who can read this artifact and in what form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExposureClass {
    /// Raw internal access, no sanitization.
    InternalRaw,
    /// Internal access with sanitization applied.
    InternalSanitized,
    /// GAUI access with sanitization and redaction.
    GauiSanitized,
    /// External API access with full redaction.
    ExternalRedacted,
}

/// Protection flags that prevent deletion regardless of policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProtectionFlags {
    /// Legal or administrative hold — highest priority, blocks all deletion.
    #[serde(default)]
    pub legal_hold: bool,
    /// Explicit override preventing deletion.
    #[serde(default)]
    pub explicit_override: bool,
    /// Protected for replan/task-switch safeguards.
    #[serde(default)]
    pub replan_safe: bool,
}

impl ProtectionFlags {
    /// Whether any protection flag is set.
    pub fn is_protected(&self) -> bool {
        self.legal_hold || self.explicit_override || self.replan_safe
    }
}

/// Complete policy bindings for an artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyBindings {
    pub retention_class: RetentionClass,
    pub freshness_class: FreshnessClass,
    pub exposure_class: ExposureClass,
    pub protection_flags: ProtectionFlags,
}

impl Default for PolicyBindings {
    fn default() -> Self {
        Self {
            retention_class: RetentionClass::GoalLifetime,
            freshness_class: FreshnessClass::Evergreen,
            exposure_class: ExposureClass::InternalSanitized,
            protection_flags: ProtectionFlags::default(),
        }
    }
}

/// Policy precedence layers, evaluated top-to-bottom (first match wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyPrecedence {
    /// Legal/administrative hold — highest priority.
    LegalHold = 0,
    /// Explicit per-artifact override.
    ExplicitOverride = 1,
    /// Domain-level policy (pipeline/workflow/execution/episode).
    DomainPolicy = 2,
    /// Artifact-type policy.
    TypePolicy = 3,
    /// Global default — lowest priority.
    GlobalDefault = 4,
}

// ---------------------------------------------------------------------------
// Ownership Scope
// ---------------------------------------------------------------------------

/// Ownership scope identifying where an artifact belongs in the system.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OwnershipScope {
    /// Execution that owns this artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// Task identity (optional, for future task model unification).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Workflow instance when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance_id: Option<String>,
    /// Run ID for run-level freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Cycle ID for cycle-level scoping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<String>,
}

/// Producer information for an artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProducerInfo {
    /// Agent that produced this artifact.
    pub producer_agent_id: String,
    /// Pipeline/workflow stage that produced this artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_stage: Option<String>,
    /// When this artifact was produced.
    pub produced_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Reference Tracking
// ---------------------------------------------------------------------------

/// A reference from another entity to this artifact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactReference {
    /// What entity holds this reference.
    pub referrer_id: String,
    /// Type of referrer (e.g., "goal_cycle", "workflow_step", "plan_graph").
    pub referrer_type: String,
    /// When this reference was established.
    pub established_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Render Hints
// ---------------------------------------------------------------------------

/// Hints for auto-surface publication, controlling how an artifact is rendered
/// and grouped when assembled into a surface.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RenderHints {
    /// Group key for surface assembly — artifacts with the same group key are
    /// assembled into a single surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_group: Option<String>,
    /// Display priority within the surface (lower = higher priority).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_priority: Option<u32>,
    /// Title for this section when rendered on a surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_title: Option<String>,
    /// Section kind override (e.g., "summary", "data_table", "chart").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_section: Option<String>,
    /// Freshness TTL in seconds — how long this artifact's rendered section
    /// remains fresh before the surface should be refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness_ttl_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// Canonical Artifact Metadata
// ---------------------------------------------------------------------------

/// Full lifecycle metadata for an artifact registered in the catalog.
///
/// This is the canonical contract from Section 6 of the lifecycle plan.
/// Every artifact registered in the catalog must include all required fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactMetadata {
    // --- Identity ---
    /// Global unique identifier.
    pub artifact_uid: ArtifactUid,
    /// Which domain this artifact belongs to.
    pub domain: ArtifactDomain,
    /// Logical artifact kind within the domain when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    /// Where the artifact payload physically lives.
    pub physical_locator: PhysicalLocator,
    /// Optional route binding for route-published artifacts such as surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_target: Option<String>,

    // --- Ownership ---
    pub ownership: OwnershipScope,

    // --- Producer ---
    pub producer: ProducerInfo,

    // --- Policy ---
    pub policy: PolicyBindings,

    // --- State ---
    /// Current lifecycle state.
    pub lifecycle_state: LifecycleState,
    /// When lifecycle state was last validated/transitioned.
    pub last_validated_at: DateTime<Utc>,
    /// Computed expiry time (materialized from policy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    // --- References ---
    /// Active references preventing deletion.
    #[serde(default)]
    pub references: Vec<ArtifactReference>,

    // --- Render Hints ---
    /// Optional hints for auto-surface publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_hints: Option<RenderHints>,

    // --- Audit ---
    /// History of lifecycle transitions for this artifact.
    /// Capped at 50 entries; oldest evicted first (O(1) via VecDeque).
    #[serde(default)]
    pub transition_log: VecDeque<LifecycleTransition>,
}

impl ArtifactMetadata {
    /// Number of active (non-released) references.
    pub fn ref_count(&self) -> usize {
        self.references.len()
    }

    /// Whether this artifact is protected from deletion.
    pub fn is_protected(&self) -> bool {
        self.policy.protection_flags.is_protected() || !self.references.is_empty()
    }

    /// Whether this artifact is eligible for normal consumption.
    pub fn is_consumable(&self) -> bool {
        self.lifecycle_state.is_consumable()
    }

    /// Add a reference from another entity.
    pub fn add_reference(&mut self, reference: ArtifactReference) {
        // Deduplicate by referrer_id only — timestamps differ across calls
        // but the logical reference is the same entity.
        if !self
            .references
            .iter()
            .any(|r| r.referrer_id == reference.referrer_id)
        {
            self.references.push(reference);
        }
    }

    /// Remove a reference by referrer_id.
    pub fn release_reference(&mut self, referrer_id: &str) {
        self.references.retain(|r| r.referrer_id != referrer_id);
    }

    /// Record a lifecycle state transition.
    ///
    /// The transition log is capped at 50 entries to prevent unbounded growth.
    /// When the cap is reached, the oldest entry is removed.
    pub fn transition_to(&mut self, new_state: LifecycleState, reason: TransitionReason) {
        const MAX_TRANSITION_LOG: usize = 50;

        let now = Utc::now();
        self.transition_log.push_back(LifecycleTransition {
            from_state: self.lifecycle_state,
            to_state: new_state,
            reason,
            transitioned_at: now,
        });
        if self.transition_log.len() > MAX_TRANSITION_LOG {
            self.transition_log.pop_front();
        }
        self.lifecycle_state = new_state;
        self.last_validated_at = now;
    }
}

/// Record of a single lifecycle state transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleTransition {
    pub from_state: LifecycleState,
    pub to_state: LifecycleState,
    pub reason: TransitionReason,
    pub transitioned_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Lifecycle Decision
// ---------------------------------------------------------------------------

/// Result of a policy engine evaluation for a single artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleDecision {
    pub artifact_uid: ArtifactUid,
    pub current_state: LifecycleState,
    pub recommended_state: LifecycleState,
    pub reason: TransitionReason,
    /// Which policy layer produced this decision.
    pub decided_by: PolicyPrecedence,
    /// Whether deletion is blocked by references or protection.
    pub deletion_blocked: bool,
    /// Human-readable explanation.
    pub explanation: String,
    /// Policy engine version that produced this decision.
    pub policy_version: u32,
}

// ---------------------------------------------------------------------------
// Catalog Query
// ---------------------------------------------------------------------------

/// Filter criteria for querying the artifact catalog.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<ArtifactDomain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<String>,
    #[serde(default)]
    pub lifecycle_states: HashSet<LifecycleState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_after: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_before: Option<DateTime<Utc>>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

// ---------------------------------------------------------------------------
// Catalog Stats
// ---------------------------------------------------------------------------

/// Aggregate statistics for lifecycle reporting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogStats {
    pub total_artifacts: usize,
    pub by_domain: std::collections::HashMap<String, usize>,
    pub by_state: std::collections::HashMap<String, usize>,
    pub protected_count: usize,
    pub referenced_count: usize,
    pub cleanup_candidates: usize,
}

// ---------------------------------------------------------------------------
// Cutover Fence
// ---------------------------------------------------------------------------

/// The timestamp fence separating unmanaged historical data from
/// lifecycle-managed data (Section 16 of the lifecycle plan).
///
/// Only artifacts with `produced_at >= cutover_utc` are registered in the
/// catalog and eligible for lifecycle management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CutoverFence {
    /// The cutover timestamp. Artifacts before this are pre-cutover.
    pub cutover_utc: DateTime<Utc>,
}

impl CutoverFence {
    /// Create a new cutover fence at the given timestamp.
    pub fn new(cutover_utc: DateTime<Utc>) -> Self {
        Self { cutover_utc }
    }

    /// Whether an artifact is post-cutover (eligible for lifecycle management).
    pub fn is_post_cutover(&self, produced_at: &DateTime<Utc>) -> bool {
        produced_at >= &self.cutover_utc
    }
}

impl Default for CutoverFence {
    /// Default cutover is "now" — only newly created artifacts are managed.
    fn default() -> Self {
        Self {
            cutover_utc: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Sweep Configuration
// ---------------------------------------------------------------------------

/// Configuration for the lifecycle sweeper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepConfig {
    /// Maximum artifacts to delete per sweep run.
    pub max_deletes_per_sweep: usize,
    /// Grace period in seconds before a DeletePending artifact is hard-deleted.
    pub grace_period_seconds: u64,
    /// Whether to run in dry-run mode (no actual deletions).
    pub dry_run: bool,
    /// Per-domain kill switches. If a domain is present and true, sweeps skip it.
    #[serde(default)]
    pub domain_kill_switches: std::collections::HashMap<String, bool>,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            max_deletes_per_sweep: 100,
            grace_period_seconds: 3600, // 1 hour
            dry_run: false,
            domain_kill_switches: std::collections::HashMap::new(),
        }
    }
}

/// Result of a sweep run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SweepResult {
    pub candidates_evaluated: usize,
    pub transitions_applied: usize,
    pub deletions_performed: usize,
    pub deletions_blocked: usize,
    pub errors: Vec<String>,
    pub dry_run: bool,
    /// UIDs that failed deletion and should be retried on next sweep.
    #[serde(default)]
    pub retry_candidates: Vec<String>,
}

// ---------------------------------------------------------------------------
// Sanitization Types
// ---------------------------------------------------------------------------

/// Target surface for artifact projection/sanitization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionSurface {
    /// Internal raw — no sanitization.
    InternalRaw,
    /// Internal with sanitization applied.
    InternalSanitized,
    /// GAUI — sanitized, size-limited, secrets stripped.
    Gaui,
    /// External — full redaction.
    External,
}

/// A sanitized projection of an artifact for a specific surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactProjection {
    pub artifact_uid: ArtifactUid,
    pub domain: ArtifactDomain,
    pub surface: ProjectionSurface,
    pub producer_agent_id: String,
    pub produced_at: DateTime<Utc>,
    pub lifecycle_state: LifecycleState,
    /// Sanitized content (may be truncated, redacted, or transformed).
    pub content: serde_json::Value,
    /// Fields that were redacted during projection.
    #[serde(default)]
    pub redacted_fields: Vec<String>,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_state_properties() {
        assert!(LifecycleState::Active.is_readable());
        assert!(LifecycleState::Active.is_consumable());
        assert!(LifecycleState::Stale.is_readable());
        assert!(!LifecycleState::Stale.is_consumable());
        assert!(!LifecycleState::Expired.is_readable());
        assert!(!LifecycleState::Quarantined.is_readable());
        assert!(LifecycleState::Deleted.is_terminal());
        assert!(!LifecycleState::Active.is_terminal());
    }

    #[test]
    fn protection_flags_composite() {
        let mut flags = ProtectionFlags::default();
        assert!(!flags.is_protected());
        flags.replan_safe = true;
        assert!(flags.is_protected());
    }

    #[test]
    fn metadata_reference_management() {
        let mut meta = ArtifactMetadata {
            artifact_uid: "test-uid".to_string(),
            domain: ArtifactDomain::Episode,
            artifact_type: None,
            physical_locator: PhysicalLocator::InMemory {
                key: "k".to_string(),
            },
            route_target: None,
            ownership: OwnershipScope::default(),
            producer: ProducerInfo {
                producer_agent_id: "agent-1".to_string(),
                producer_stage: None,
                produced_at: Utc::now(),
            },
            policy: PolicyBindings::default(),
            lifecycle_state: LifecycleState::Active,
            last_validated_at: Utc::now(),
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: VecDeque::new(),
        };

        assert_eq!(meta.ref_count(), 0);
        assert!(!meta.is_protected());

        let reference = ArtifactReference {
            referrer_id: "cycle-1".to_string(),
            referrer_type: "goal_cycle".to_string(),
            established_at: Utc::now(),
        };
        meta.add_reference(reference.clone());
        assert_eq!(meta.ref_count(), 1);
        assert!(meta.is_protected());

        // Duplicate add is idempotent
        meta.add_reference(reference);
        assert_eq!(meta.ref_count(), 1);

        meta.release_reference("cycle-1");
        assert_eq!(meta.ref_count(), 0);
    }

    #[test]
    fn metadata_transition_records_history() {
        let mut meta = ArtifactMetadata {
            artifact_uid: "test-uid".to_string(),
            domain: ArtifactDomain::Pipeline,
            artifact_type: None,
            physical_locator: PhysicalLocator::InMemory {
                key: "k".to_string(),
            },
            route_target: None,
            ownership: OwnershipScope::default(),
            producer: ProducerInfo {
                producer_agent_id: "agent-1".to_string(),
                producer_stage: None,
                produced_at: Utc::now(),
            },
            policy: PolicyBindings::default(),
            lifecycle_state: LifecycleState::Active,
            last_validated_at: Utc::now(),
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: VecDeque::new(),
        };

        meta.transition_to(LifecycleState::Stale, TransitionReason::FreshnessExpired);
        assert_eq!(meta.lifecycle_state, LifecycleState::Stale);
        assert_eq!(meta.transition_log.len(), 1);
        assert_eq!(meta.transition_log[0].from_state, LifecycleState::Active);
        assert_eq!(meta.transition_log[0].to_state, LifecycleState::Stale);
    }

    #[test]
    fn transition_log_capped_at_50() {
        let mut meta = ArtifactMetadata {
            artifact_uid: "cap-test".to_string(),
            domain: ArtifactDomain::Pipeline,
            artifact_type: None,
            physical_locator: PhysicalLocator::InMemory {
                key: "k".to_string(),
            },
            route_target: None,
            ownership: OwnershipScope::default(),
            producer: ProducerInfo {
                producer_agent_id: "agent-1".to_string(),
                producer_stage: None,
                produced_at: Utc::now(),
            },
            policy: PolicyBindings::default(),
            lifecycle_state: LifecycleState::Active,
            last_validated_at: Utc::now(),
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: VecDeque::new(),
        };

        // Perform 60 transitions — log should cap at 50.
        for _ in 0..60 {
            meta.transition_to(LifecycleState::Stale, TransitionReason::FreshnessExpired);
            meta.transition_to(LifecycleState::Active, TransitionReason::Revalidated);
        }

        assert_eq!(meta.transition_log.len(), 50);
        // The oldest entries should have been evicted; latest should be present.
        assert_eq!(
            meta.transition_log.back().unwrap().to_state,
            LifecycleState::Active
        );
    }

    #[test]
    fn cutover_fence_filters_correctly() {
        let fence = CutoverFence::new(
            DateTime::parse_from_rfc3339("2026-02-27T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        let before = DateTime::parse_from_rfc3339("2026-02-26T23:59:59Z")
            .unwrap()
            .with_timezone(&Utc);
        let after = DateTime::parse_from_rfc3339("2026-02-27T00:00:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let exactly = DateTime::parse_from_rfc3339("2026-02-27T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert!(!fence.is_post_cutover(&before));
        assert!(fence.is_post_cutover(&after));
        assert!(fence.is_post_cutover(&exactly));
    }

    #[test]
    fn policy_bindings_default_values() {
        let bindings = PolicyBindings::default();
        assert_eq!(bindings.retention_class, RetentionClass::GoalLifetime);
        assert_eq!(bindings.freshness_class, FreshnessClass::Evergreen);
        assert_eq!(bindings.exposure_class, ExposureClass::InternalSanitized);
        assert!(!bindings.protection_flags.is_protected());
    }

    #[test]
    fn artifact_metadata_serialization_roundtrip() {
        let meta = ArtifactMetadata {
            artifact_uid: "uid-123".to_string(),
            domain: ArtifactDomain::Workflow,
            artifact_type: Some("data_bundle".to_string()),
            physical_locator: PhysicalLocator::WorkflowRun {
                instance_id: "wf-1".to_string(),
                step_name: "step-a".to_string(),
                artifact_name: "output".to_string(),
            },
            route_target: Some("/briefing".to_string()),
            ownership: OwnershipScope {
                execution_id: Some("exec-1".to_string()),
                workflow_instance_id: Some("wf-1".to_string()),
                ..Default::default()
            },
            producer: ProducerInfo {
                producer_agent_id: "agent-1".to_string(),
                producer_stage: Some("extract".to_string()),
                produced_at: Utc::now(),
            },
            policy: PolicyBindings {
                retention_class: RetentionClass::Ttl { seconds: 3600 },
                freshness_class: FreshnessClass::Window {
                    fresh_seconds: 300,
                    stale_seconds: 600,
                },
                exposure_class: ExposureClass::GauiSanitized,
                protection_flags: ProtectionFlags::default(),
            },
            lifecycle_state: LifecycleState::Active,
            last_validated_at: Utc::now(),
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: VecDeque::new(),
        };

        let json = serde_json::to_string(&meta).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&json).unwrap();
        let restored: ArtifactMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(
            payload["ownership"]["execution_id"].as_str(),
            Some("exec-1")
        );
        assert!(payload["ownership"].get("thread_id").is_none());
        assert_eq!(restored.artifact_uid, "uid-123");
        assert_eq!(restored.domain, ArtifactDomain::Workflow);
        assert_eq!(restored.artifact_type.as_deref(), Some("data_bundle"));
        assert_eq!(restored.route_target.as_deref(), Some("/briefing"));
        assert_eq!(restored.lifecycle_state, LifecycleState::Active);
    }
}
