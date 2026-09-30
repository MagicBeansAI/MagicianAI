//! # Policy Engine
//!
//! Evaluates staleness, expiration, retention eligibility, protection
//! constraints, and exposure rights for artifacts in the lifecycle catalog.
//!
//! The engine uses a 5-layer precedence model (Section 7 of the lifecycle
//! plan) to produce deterministic, auditable lifecycle decisions:
//!
//! 1. **Legal/administrative hold** — blocks all deletion
//! 2. **Explicit artifact override** — per-artifact protection
//! 3. **Domain policy** — pipeline/workflow/execution/episode defaults
//! 4. **Artifact-type policy** — (reserved for future use)
//! 5. **Global default** — fallback
//!
//! All evaluations are pure functions: same input always produces the same
//! output with no side effects.

use super::types::{
    ArtifactDomain, ArtifactMetadata, ExposureClass, FreshnessClass, LifecycleDecision,
    LifecycleState, PolicyBindings, PolicyPrecedence, ProtectionFlags, RetentionClass,
    TransitionReason,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Scope Events
// ---------------------------------------------------------------------------

/// Events that signal the closure of a lifecycle scope boundary.
///
/// When a scope closes, artifacts whose retention class matches that scope
/// become eligible for expiration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeEvent {
    /// A task/thread-level unit of work has been closed.
    TaskClosed { task_id: String },
    /// A thread has been closed.
    ThreadClosed { thread_id: String },
    /// A workflow run has completed.
    RunCompleted { run_id: String },
    /// A goal has been completed (or abandoned).
    GoalCompleted { goal_id: String },
}

// ---------------------------------------------------------------------------
// Default threshold for RevalidateOnResume (seconds)
// ---------------------------------------------------------------------------

/// Fallback stale threshold in seconds used for `RevalidateOnResume` freshness
/// when no domain-specific stale window is available.
const REVALIDATE_ON_RESUME_DEFAULT_STALE_SECONDS: u64 = 3600;

// ---------------------------------------------------------------------------
// Policy Engine
// ---------------------------------------------------------------------------

/// The policy engine evaluates artifact metadata against layered policy
/// bindings to produce deterministic lifecycle decisions.
///
/// It holds domain-level defaults and a global fallback. Per-artifact
/// overrides and protection flags are read directly from the artifact's
/// metadata.
#[derive(Debug, Clone)]
pub struct PolicyEngine {
    /// Default policy bindings per domain.
    pub domain_defaults: HashMap<ArtifactDomain, PolicyBindings>,
    /// Global fallback policy when no domain default exists.
    pub global_default: PolicyBindings,
    /// Policy engine version — propagated into every LifecycleDecision.
    policy_version: u32,
}

impl PolicyEngine {
    /// Construct a new `PolicyEngine` with the canonical domain defaults
    /// specified in the lifecycle plan.
    pub fn new() -> Self {
        let mut domain_defaults = HashMap::new();

        // Pipeline: task-scoped, revalidate on resume, internal sanitized
        domain_defaults.insert(
            ArtifactDomain::Pipeline,
            PolicyBindings {
                retention_class: RetentionClass::TaskLifetime,
                freshness_class: FreshnessClass::RevalidateOnResume,
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
        );

        // Workflow: run-scoped, 5-minute fresh / 10-minute stale window
        domain_defaults.insert(
            ArtifactDomain::Workflow,
            PolicyBindings {
                retention_class: RetentionClass::RunLifetime,
                freshness_class: FreshnessClass::Window {
                    fresh_seconds: 300,
                    stale_seconds: 600,
                },
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
        );

        // Execution: goal-scoped, evergreen, internal sanitized
        domain_defaults.insert(
            ArtifactDomain::Execution,
            PolicyBindings {
                retention_class: RetentionClass::GoalLifetime,
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
        );

        // Episode: permanent, evergreen, GAUI sanitized
        domain_defaults.insert(
            ArtifactDomain::Episode,
            PolicyBindings {
                retention_class: RetentionClass::Permanent,
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: ExposureClass::GauiSanitized,
                protection_flags: ProtectionFlags::default(),
            },
        );

        // Durable: permanent, evergreen, internal sanitized
        domain_defaults.insert(
            ArtifactDomain::Durable,
            PolicyBindings {
                retention_class: RetentionClass::Permanent,
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
        );

        // Global fallback: goal-scoped, evergreen, internal sanitized
        let global_default = PolicyBindings {
            retention_class: RetentionClass::GoalLifetime,
            freshness_class: FreshnessClass::Evergreen,
            exposure_class: ExposureClass::InternalSanitized,
            protection_flags: ProtectionFlags::default(),
        };

        Self {
            domain_defaults,
            global_default,
            policy_version: 1,
        }
    }

    // -----------------------------------------------------------------------
    // Core evaluation
    // -----------------------------------------------------------------------

    /// Evaluate a single artifact's metadata against the policy layers and
    /// return a deterministic lifecycle decision.
    ///
    /// Evaluation order:
    /// 1. Protection flags (legal hold, explicit override)
    /// 2. Freshness evaluation (Window, RevalidateOnResume, Evergreen)
    /// 3. Retention evaluation (TTL-based expiration)
    /// 4. Reference-blocked deletion guard
    pub fn evaluate(&self, metadata: &ArtifactMetadata, now: DateTime<Utc>) -> LifecycleDecision {
        let (effective_policy, precedence) = self.resolve_effective_policy(metadata);

        tracing::debug!(
            artifact_uid = %metadata.artifact_uid,
            domain = %metadata.domain,
            current_state = %metadata.lifecycle_state,
            precedence = ?precedence,
            "evaluating artifact lifecycle policy"
        );

        // ----- 1. Protection flags → block deletion -----
        if metadata.policy.protection_flags.legal_hold {
            tracing::debug!(
                artifact_uid = %metadata.artifact_uid,
                "legal hold active — deletion blocked"
            );
            return LifecycleDecision {
                artifact_uid: metadata.artifact_uid.clone(),
                current_state: metadata.lifecycle_state,
                recommended_state: metadata.lifecycle_state,
                reason: TransitionReason::OperatorAction {
                    reason: "legal hold active".to_string(),
                },
                decided_by: PolicyPrecedence::LegalHold,
                deletion_blocked: true,
                explanation: "Artifact is under legal/administrative hold; \
                              deletion is blocked regardless of other policy."
                    .to_string(),
                policy_version: self.policy_version,
            };
        }

        if metadata.policy.protection_flags.explicit_override {
            tracing::debug!(
                artifact_uid = %metadata.artifact_uid,
                "explicit override active — deletion blocked"
            );
            return LifecycleDecision {
                artifact_uid: metadata.artifact_uid.clone(),
                current_state: metadata.lifecycle_state,
                recommended_state: metadata.lifecycle_state,
                reason: TransitionReason::OperatorAction {
                    reason: "explicit override active".to_string(),
                },
                decided_by: PolicyPrecedence::ExplicitOverride,
                deletion_blocked: true,
                explanation: "Artifact has an explicit protection override; \
                              deletion is blocked."
                    .to_string(),
                policy_version: self.policy_version,
            };
        }

        // Already deleted — nothing to do.
        if metadata.lifecycle_state == LifecycleState::Deleted {
            return LifecycleDecision {
                artifact_uid: metadata.artifact_uid.clone(),
                current_state: LifecycleState::Deleted,
                recommended_state: LifecycleState::Deleted,
                reason: TransitionReason::GracePeriodCompleted,
                decided_by: precedence,
                deletion_blocked: false,
                explanation: "Artifact is already deleted; no further action.".to_string(),
                policy_version: self.policy_version,
            };
        }

        // ----- 2. Freshness evaluation -----
        let elapsed_since_produced = now
            .signed_duration_since(metadata.producer.produced_at)
            .num_seconds()
            .max(0) as u64;

        let freshness_result = self.evaluate_freshness(
            &effective_policy.freshness_class,
            metadata,
            elapsed_since_produced,
            now,
        );

        // ----- 3. Retention evaluation (TTL) -----
        let retention_result =
            self.evaluate_retention(&effective_policy.retention_class, elapsed_since_produced);

        // Combine: retention expiry takes priority (it is a hard deadline),
        // then freshness.
        let (recommended_state, reason) = match retention_result {
            Some((state, reason)) => (state, reason),
            None => freshness_result,
        };

        // ----- 4. Reference-blocked deletion guard -----
        let deletion_blocked = if recommended_state == LifecycleState::DeletePending
            || metadata.lifecycle_state == LifecycleState::DeletePending
        {
            !metadata.references.is_empty()
        } else {
            false
        };

        let final_state =
            if deletion_blocked && metadata.lifecycle_state == LifecycleState::DeletePending {
                // Cannot progress from DeletePending while references exist.
                LifecycleState::DeletePending
            } else {
                recommended_state
            };

        let explanation = self.build_explanation(
            metadata,
            &effective_policy,
            precedence,
            final_state,
            deletion_blocked,
        );

        tracing::debug!(
            artifact_uid = %metadata.artifact_uid,
            recommended_state = %final_state,
            deletion_blocked,
            precedence = ?precedence,
            "policy evaluation complete"
        );

        LifecycleDecision {
            artifact_uid: metadata.artifact_uid.clone(),
            current_state: metadata.lifecycle_state,
            recommended_state: final_state,
            reason,
            decided_by: precedence,
            deletion_blocked,
            explanation,
            policy_version: self.policy_version,
        }
    }

    /// Evaluate a batch of artifacts, returning one decision per artifact.
    pub fn evaluate_batch(
        &self,
        artifacts: &[&ArtifactMetadata],
        now: DateTime<Utc>,
    ) -> Vec<LifecycleDecision> {
        artifacts
            .iter()
            .map(|meta| self.evaluate(meta, now))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Policy resolution
    // -----------------------------------------------------------------------

    /// Walk the 5-layer precedence model and return the effective policy
    /// bindings together with the layer that produced them.
    ///
    /// Precedence (first match wins):
    /// 1. Legal hold → artifact's own policy at LegalHold precedence
    /// 2. Explicit override → artifact's own policy at ExplicitOverride
    /// 3. Domain default → domain_defaults map at DomainPolicy
    /// 4. (TypePolicy reserved — falls through to global)
    /// 5. Global default → global_default at GlobalDefault
    pub fn resolve_effective_policy(
        &self,
        metadata: &ArtifactMetadata,
    ) -> (PolicyBindings, PolicyPrecedence) {
        // Layer 1: Legal hold
        if metadata.policy.protection_flags.legal_hold {
            return (metadata.policy.clone(), PolicyPrecedence::LegalHold);
        }

        // Layer 2: Explicit override
        if metadata.policy.protection_flags.explicit_override {
            return (metadata.policy.clone(), PolicyPrecedence::ExplicitOverride);
        }

        // Layer 3: Domain default
        if let Some(domain_policy) = self.domain_defaults.get(&metadata.domain) {
            return (domain_policy.clone(), PolicyPrecedence::DomainPolicy);
        }

        // Layer 5: Global default (layer 4 / TypePolicy is reserved)
        (self.global_default.clone(), PolicyPrecedence::GlobalDefault)
    }

    // -----------------------------------------------------------------------
    // Scope expiry
    // -----------------------------------------------------------------------

    /// Determine whether an artifact's retention scope has expired given a
    /// scope closure event.
    ///
    /// Returns `true` when the artifact should be considered expired because
    /// the scope it depends on has closed:
    /// - `TaskLifetime` expires on `TaskClosed` or `ThreadClosed`
    /// - `RunLifetime` expires on `RunCompleted`
    /// - `GoalLifetime` expires on `GoalCompleted`
    /// - `Permanent` and `Ttl` are unaffected by scope events.
    pub fn is_scope_expired(&self, metadata: &ArtifactMetadata, scope_event: &ScopeEvent) -> bool {
        let (effective_policy, _) = self.resolve_effective_policy(metadata);
        match (&effective_policy.retention_class, scope_event) {
            (RetentionClass::TaskLifetime, ScopeEvent::TaskClosed { .. }) => true,
            (RetentionClass::TaskLifetime, ScopeEvent::ThreadClosed { .. }) => true,
            (RetentionClass::RunLifetime, ScopeEvent::RunCompleted { .. }) => true,
            (RetentionClass::GoalLifetime, ScopeEvent::GoalCompleted { .. }) => true,
            _ => false,
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Evaluate freshness and return the recommended state and reason.
    fn evaluate_freshness(
        &self,
        freshness_class: &FreshnessClass,
        metadata: &ArtifactMetadata,
        elapsed_since_produced: u64,
        now: DateTime<Utc>,
    ) -> (LifecycleState, TransitionReason) {
        match freshness_class {
            FreshnessClass::Window {
                fresh_seconds,
                stale_seconds,
            } => {
                if elapsed_since_produced > *stale_seconds {
                    (
                        LifecycleState::Expired,
                        TransitionReason::StaleWindowExpired,
                    )
                } else if elapsed_since_produced > *fresh_seconds {
                    (LifecycleState::Stale, TransitionReason::FreshnessExpired)
                } else {
                    (LifecycleState::Active, TransitionReason::Revalidated)
                }
            },

            FreshnessClass::RevalidateOnResume => {
                // Use the domain-specific stale window if the domain default
                // defines a Window freshness, otherwise fall back to the
                // global constant.
                let stale_threshold = self
                    .domain_defaults
                    .get(&metadata.domain)
                    .and_then(|p| match &p.freshness_class {
                        FreshnessClass::Window { stale_seconds, .. } => Some(*stale_seconds),
                        _ => None,
                    })
                    .unwrap_or(REVALIDATE_ON_RESUME_DEFAULT_STALE_SECONDS);

                let elapsed_since_validated = now
                    .signed_duration_since(metadata.last_validated_at)
                    .num_seconds()
                    .max(0) as u64;

                if elapsed_since_validated > stale_threshold {
                    (LifecycleState::Stale, TransitionReason::FreshnessExpired)
                } else {
                    (LifecycleState::Active, TransitionReason::Revalidated)
                }
            },

            FreshnessClass::Evergreen => (LifecycleState::Active, TransitionReason::Revalidated),
        }
    }

    /// Evaluate TTL-based retention. Returns `Some` if a TTL class drives
    /// the artifact to `Expired`, otherwise `None` to let other evaluations
    /// take effect. Scope-based retention (TaskLifetime, RunLifetime, etc.)
    /// is handled by the scheduler reacting to scope events.
    fn evaluate_retention(
        &self,
        retention_class: &RetentionClass,
        elapsed_since_produced: u64,
    ) -> Option<(LifecycleState, TransitionReason)> {
        match retention_class {
            RetentionClass::Ttl { seconds } => {
                if elapsed_since_produced > *seconds {
                    Some((
                        LifecycleState::Expired,
                        TransitionReason::ScopeBoundary {
                            event: format!("TTL of {} seconds exceeded", seconds),
                        },
                    ))
                } else {
                    None
                }
            },
            // Scope-based and permanent retention are not evaluated here.
            _ => None,
        }
    }

    /// Build a human-readable explanation of the evaluation outcome.
    fn build_explanation(
        &self,
        metadata: &ArtifactMetadata,
        effective_policy: &PolicyBindings,
        precedence: PolicyPrecedence,
        recommended_state: LifecycleState,
        deletion_blocked: bool,
    ) -> String {
        let mut parts = Vec::new();

        parts.push(format!(
            "Artifact {} (domain={}, current_state={}) evaluated at {:?} precedence.",
            metadata.artifact_uid, metadata.domain, metadata.lifecycle_state, precedence,
        ));

        parts.push(format!(
            "Effective policy: retention={:?}, freshness={:?}, exposure={:?}.",
            effective_policy.retention_class,
            effective_policy.freshness_class,
            effective_policy.exposure_class,
        ));

        parts.push(format!("Recommended state: {}.", recommended_state));

        if deletion_blocked {
            parts.push(format!(
                "Deletion blocked: {} active reference(s).",
                metadata.references.len()
            ));
        }

        parts.join(" ")
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifacts::types::{
        ArtifactReference, OwnershipScope, PhysicalLocator, ProducerInfo,
    };
    use chrono::Duration;
    use std::collections::VecDeque;

    // -----------------------------------------------------------------------
    // Test helper: build metadata with sensible defaults
    // -----------------------------------------------------------------------

    fn make_metadata(
        uid: &str,
        domain: ArtifactDomain,
        produced_at: DateTime<Utc>,
        policy: PolicyBindings,
        state: LifecycleState,
    ) -> ArtifactMetadata {
        ArtifactMetadata {
            artifact_uid: uid.to_string(),
            domain,
            artifact_type: None,
            physical_locator: PhysicalLocator::InMemory {
                key: uid.to_string(),
            },
            route_target: None,
            ownership: OwnershipScope::default(),
            producer: ProducerInfo {
                producer_agent_id: "test-agent".to_string(),
                producer_stage: None,
                produced_at,
            },
            policy,
            lifecycle_state: state,
            last_validated_at: produced_at,
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: VecDeque::new(),
        }
    }

    fn default_policy() -> PolicyBindings {
        PolicyBindings::default()
    }

    // -----------------------------------------------------------------------
    // Freshness window: active → stale → expired
    // -----------------------------------------------------------------------

    #[test]
    fn freshness_window_active_within_fresh_period() {
        let engine = PolicyEngine::new();
        let now = Utc::now();
        let produced_at = now - Duration::seconds(100); // 100s ago

        let policy = PolicyBindings {
            freshness_class: FreshnessClass::Window {
                fresh_seconds: 300,
                stale_seconds: 600,
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "fw-active",
            ArtifactDomain::Workflow,
            produced_at,
            policy,
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        assert_eq!(decision.recommended_state, LifecycleState::Active);
        assert!(!decision.deletion_blocked);
    }

    #[test]
    fn freshness_window_stale_after_fresh_period() {
        let engine = PolicyEngine::new();
        let now = Utc::now();
        let produced_at = now - Duration::seconds(400); // 400s > 300s fresh

        let policy = PolicyBindings {
            freshness_class: FreshnessClass::Window {
                fresh_seconds: 300,
                stale_seconds: 600,
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "fw-stale",
            ArtifactDomain::Workflow,
            produced_at,
            policy,
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        assert_eq!(decision.recommended_state, LifecycleState::Stale);
        assert!(matches!(
            decision.reason,
            TransitionReason::FreshnessExpired
        ));
    }

    #[test]
    fn freshness_window_expired_after_stale_period() {
        let engine = PolicyEngine::new();
        let now = Utc::now();
        let produced_at = now - Duration::seconds(700); // 700s > 600s stale

        let policy = PolicyBindings {
            freshness_class: FreshnessClass::Window {
                fresh_seconds: 300,
                stale_seconds: 600,
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "fw-expired",
            ArtifactDomain::Workflow,
            produced_at,
            policy,
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        assert_eq!(decision.recommended_state, LifecycleState::Expired);
        assert!(matches!(
            decision.reason,
            TransitionReason::StaleWindowExpired
        ));
    }

    // -----------------------------------------------------------------------
    // TTL-based retention expiry
    // -----------------------------------------------------------------------

    #[test]
    fn ttl_retention_active_within_ttl() {
        // Build an engine whose Execution domain uses TTL retention so
        // the effective policy picks it up through the domain layer.
        let mut engine = PolicyEngine::new();
        engine.domain_defaults.insert(
            ArtifactDomain::Execution,
            PolicyBindings {
                retention_class: RetentionClass::Ttl { seconds: 3600 },
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
        );
        let now = Utc::now();
        let produced_at = now - Duration::seconds(1800); // 30 min

        let meta = make_metadata(
            "ttl-ok",
            ArtifactDomain::Execution,
            produced_at,
            default_policy(),
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        assert_eq!(decision.recommended_state, LifecycleState::Active);
    }

    #[test]
    fn ttl_retention_expired_beyond_ttl() {
        let mut engine = PolicyEngine::new();
        engine.domain_defaults.insert(
            ArtifactDomain::Execution,
            PolicyBindings {
                retention_class: RetentionClass::Ttl { seconds: 3600 },
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
        );
        let now = Utc::now();
        let produced_at = now - Duration::seconds(7200); // 2 hours > 1 hour TTL

        let meta = make_metadata(
            "ttl-expired",
            ArtifactDomain::Execution,
            produced_at,
            default_policy(),
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        assert_eq!(decision.recommended_state, LifecycleState::Expired);
    }

    // -----------------------------------------------------------------------
    // TTL takes precedence over freshness
    // -----------------------------------------------------------------------

    #[test]
    fn ttl_overrides_freshness_window() {
        // Configure the domain default with both a TTL retention and a
        // freshness window so we can verify TTL expiry takes priority.
        let mut engine = PolicyEngine::new();
        engine.domain_defaults.insert(
            ArtifactDomain::Workflow,
            PolicyBindings {
                retention_class: RetentionClass::Ttl { seconds: 400 },
                freshness_class: FreshnessClass::Window {
                    fresh_seconds: 300,
                    stale_seconds: 600,
                },
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
        );
        let now = Utc::now();
        let produced_at = now - Duration::seconds(500);

        // Freshness says: stale (500 > 300). TTL says: expired (500 > 400).
        let meta = make_metadata(
            "ttl-over-fresh",
            ArtifactDomain::Workflow,
            produced_at,
            default_policy(),
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        // TTL expiry should win because retention is evaluated second and
        // takes priority when it returns Some.
        assert_eq!(decision.recommended_state, LifecycleState::Expired);
    }

    // -----------------------------------------------------------------------
    // Legal hold blocks deletion
    // -----------------------------------------------------------------------

    #[test]
    fn legal_hold_blocks_deletion() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let policy = PolicyBindings {
            protection_flags: ProtectionFlags {
                legal_hold: true,
                ..ProtectionFlags::default()
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "lh-1",
            ArtifactDomain::Episode,
            now,
            policy,
            LifecycleState::DeletePending,
        );
        let decision = engine.evaluate(&meta, now);

        assert!(decision.deletion_blocked);
        assert_eq!(decision.decided_by, PolicyPrecedence::LegalHold);
        // Should keep current state, not transition.
        assert_eq!(decision.recommended_state, LifecycleState::DeletePending);
    }

    #[test]
    fn legal_hold_preserves_active_state() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let policy = PolicyBindings {
            protection_flags: ProtectionFlags {
                legal_hold: true,
                ..ProtectionFlags::default()
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "lh-active",
            ArtifactDomain::Pipeline,
            now,
            policy,
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        assert!(decision.deletion_blocked);
        assert_eq!(decision.recommended_state, LifecycleState::Active);
    }

    // -----------------------------------------------------------------------
    // Explicit override blocks deletion
    // -----------------------------------------------------------------------

    #[test]
    fn explicit_override_blocks_deletion() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let policy = PolicyBindings {
            protection_flags: ProtectionFlags {
                explicit_override: true,
                ..ProtectionFlags::default()
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "eo-1",
            ArtifactDomain::Workflow,
            now,
            policy,
            LifecycleState::Expired,
        );
        let decision = engine.evaluate(&meta, now);

        assert!(decision.deletion_blocked);
        assert_eq!(decision.decided_by, PolicyPrecedence::ExplicitOverride);
        assert_eq!(decision.recommended_state, LifecycleState::Expired);
    }

    // -----------------------------------------------------------------------
    // Reference-blocked deletion
    // -----------------------------------------------------------------------

    #[test]
    fn references_block_deletion_from_delete_pending() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let policy = PolicyBindings {
            freshness_class: FreshnessClass::Evergreen,
            ..default_policy()
        };

        let mut meta = make_metadata(
            "ref-block",
            ArtifactDomain::Execution,
            now,
            policy,
            LifecycleState::DeletePending,
        );
        meta.references.push(ArtifactReference {
            referrer_id: "goal-cycle-7".to_string(),
            referrer_type: "goal_cycle".to_string(),
            established_at: now,
        });

        let decision = engine.evaluate(&meta, now);

        assert!(decision.deletion_blocked);
        assert_eq!(decision.recommended_state, LifecycleState::DeletePending);
    }

    #[test]
    fn no_references_allows_delete_pending() {
        let engine = PolicyEngine::new();
        let now = Utc::now();
        let produced_at = now - Duration::seconds(5000);

        let policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            ..default_policy()
        };

        let meta = make_metadata(
            "no-ref",
            ArtifactDomain::Execution,
            produced_at,
            policy,
            LifecycleState::DeletePending,
        );

        let decision = engine.evaluate(&meta, now);

        assert!(!decision.deletion_blocked);
    }

    // -----------------------------------------------------------------------
    // Domain default resolution
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_pipeline_domain_default() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let meta = make_metadata(
            "pipe-1",
            ArtifactDomain::Pipeline,
            now,
            default_policy(),
            LifecycleState::Active,
        );
        let (policy, precedence) = engine.resolve_effective_policy(&meta);

        assert_eq!(precedence, PolicyPrecedence::DomainPolicy);
        assert_eq!(policy.retention_class, RetentionClass::TaskLifetime);
        assert_eq!(policy.freshness_class, FreshnessClass::RevalidateOnResume);
    }

    #[test]
    fn resolve_workflow_domain_default() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let meta = make_metadata(
            "wf-1",
            ArtifactDomain::Workflow,
            now,
            default_policy(),
            LifecycleState::Active,
        );
        let (policy, precedence) = engine.resolve_effective_policy(&meta);

        assert_eq!(precedence, PolicyPrecedence::DomainPolicy);
        assert_eq!(policy.retention_class, RetentionClass::RunLifetime);
        assert_eq!(
            policy.freshness_class,
            FreshnessClass::Window {
                fresh_seconds: 300,
                stale_seconds: 600,
            }
        );
    }

    #[test]
    fn resolve_execution_domain_default() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let meta = make_metadata(
            "exec-1",
            ArtifactDomain::Execution,
            now,
            default_policy(),
            LifecycleState::Active,
        );
        let (policy, precedence) = engine.resolve_effective_policy(&meta);

        assert_eq!(precedence, PolicyPrecedence::DomainPolicy);
        assert_eq!(policy.retention_class, RetentionClass::GoalLifetime);
        assert_eq!(policy.freshness_class, FreshnessClass::Evergreen);
    }

    #[test]
    fn resolve_episode_domain_default() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let meta = make_metadata(
            "ep-1",
            ArtifactDomain::Episode,
            now,
            default_policy(),
            LifecycleState::Active,
        );
        let (policy, precedence) = engine.resolve_effective_policy(&meta);

        assert_eq!(precedence, PolicyPrecedence::DomainPolicy);
        assert_eq!(policy.retention_class, RetentionClass::Permanent);
        assert_eq!(policy.exposure_class, ExposureClass::GauiSanitized);
    }

    #[test]
    fn resolve_falls_through_to_global_when_no_domain() {
        let engine = PolicyEngine {
            domain_defaults: HashMap::new(), // empty
            global_default: PolicyEngine::new().global_default.clone(),
            policy_version: 1,
        };
        let now = Utc::now();

        let meta = make_metadata(
            "fallback-1",
            ArtifactDomain::Pipeline,
            now,
            default_policy(),
            LifecycleState::Active,
        );
        let (policy, precedence) = engine.resolve_effective_policy(&meta);

        assert_eq!(precedence, PolicyPrecedence::GlobalDefault);
        assert_eq!(policy.retention_class, RetentionClass::GoalLifetime);
    }

    // -----------------------------------------------------------------------
    // Precedence ordering
    // -----------------------------------------------------------------------

    #[test]
    fn legal_hold_takes_highest_precedence() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let policy = PolicyBindings {
            protection_flags: ProtectionFlags {
                legal_hold: true,
                explicit_override: true,
                ..ProtectionFlags::default()
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "prec-legal",
            ArtifactDomain::Pipeline,
            now,
            policy,
            LifecycleState::Active,
        );
        let (_, precedence) = engine.resolve_effective_policy(&meta);

        assert_eq!(precedence, PolicyPrecedence::LegalHold);
    }

    #[test]
    fn explicit_override_precedes_domain() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let policy = PolicyBindings {
            protection_flags: ProtectionFlags {
                explicit_override: true,
                ..ProtectionFlags::default()
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "prec-override",
            ArtifactDomain::Workflow,
            now,
            policy,
            LifecycleState::Active,
        );
        let (_, precedence) = engine.resolve_effective_policy(&meta);

        assert_eq!(precedence, PolicyPrecedence::ExplicitOverride);
    }

    #[test]
    fn domain_precedes_global() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let meta = make_metadata(
            "prec-domain",
            ArtifactDomain::Episode,
            now,
            default_policy(),
            LifecycleState::Active,
        );
        let (_, precedence) = engine.resolve_effective_policy(&meta);

        assert_eq!(precedence, PolicyPrecedence::DomainPolicy);
    }

    // -----------------------------------------------------------------------
    // Scope event expiry
    // -----------------------------------------------------------------------

    #[test]
    fn task_close_expires_task_lifetime_artifact() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        // Pipeline domain defaults to TaskLifetime retention.
        let meta = make_metadata(
            "scope-task",
            ArtifactDomain::Pipeline,
            now,
            default_policy(),
            LifecycleState::Active,
        );

        let event = ScopeEvent::TaskClosed {
            task_id: "task-42".to_string(),
        };

        assert!(engine.is_scope_expired(&meta, &event));
    }

    #[test]
    fn thread_close_expires_task_lifetime_artifact() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let meta = make_metadata(
            "scope-thread",
            ArtifactDomain::Pipeline,
            now,
            default_policy(),
            LifecycleState::Active,
        );

        let event = ScopeEvent::ThreadClosed {
            thread_id: "thread-7".to_string(),
        };

        assert!(engine.is_scope_expired(&meta, &event));
    }

    #[test]
    fn run_completed_expires_run_lifetime_artifact() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        // Workflow domain defaults to RunLifetime retention.
        let meta = make_metadata(
            "scope-run",
            ArtifactDomain::Workflow,
            now,
            default_policy(),
            LifecycleState::Active,
        );

        let event = ScopeEvent::RunCompleted {
            run_id: "run-99".to_string(),
        };

        assert!(engine.is_scope_expired(&meta, &event));
    }

    #[test]
    fn goal_completed_expires_goal_lifetime_artifact() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        // Execution domain defaults to GoalLifetime retention.
        let meta = make_metadata(
            "scope-goal",
            ArtifactDomain::Execution,
            now,
            default_policy(),
            LifecycleState::Active,
        );

        let event = ScopeEvent::GoalCompleted {
            goal_id: "goal-5".to_string(),
        };

        assert!(engine.is_scope_expired(&meta, &event));
    }

    #[test]
    fn permanent_artifacts_not_expired_by_any_scope_event() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        // Episode domain defaults to Permanent retention.
        let meta = make_metadata(
            "scope-perm",
            ArtifactDomain::Episode,
            now,
            default_policy(),
            LifecycleState::Active,
        );

        assert!(!engine.is_scope_expired(
            &meta,
            &ScopeEvent::TaskClosed {
                task_id: "t".to_string()
            }
        ));
        assert!(!engine.is_scope_expired(
            &meta,
            &ScopeEvent::ThreadClosed {
                thread_id: "t".to_string()
            }
        ));
        assert!(!engine.is_scope_expired(
            &meta,
            &ScopeEvent::RunCompleted {
                run_id: "r".to_string()
            }
        ));
        assert!(!engine.is_scope_expired(
            &meta,
            &ScopeEvent::GoalCompleted {
                goal_id: "g".to_string()
            }
        ));
    }

    #[test]
    fn mismatched_scope_event_does_not_expire() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        // Pipeline (TaskLifetime) should NOT expire on RunCompleted.
        let meta = make_metadata(
            "scope-mismatch",
            ArtifactDomain::Pipeline,
            now,
            default_policy(),
            LifecycleState::Active,
        );

        let event = ScopeEvent::RunCompleted {
            run_id: "run-1".to_string(),
        };

        assert!(!engine.is_scope_expired(&meta, &event));
    }

    // -----------------------------------------------------------------------
    // Batch evaluation
    // -----------------------------------------------------------------------

    #[test]
    fn batch_evaluation_returns_one_decision_per_artifact() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let m1 = make_metadata(
            "batch-1",
            ArtifactDomain::Pipeline,
            now,
            default_policy(),
            LifecycleState::Active,
        );
        let m2 = make_metadata(
            "batch-2",
            ArtifactDomain::Workflow,
            now,
            default_policy(),
            LifecycleState::Active,
        );
        let m3 = make_metadata(
            "batch-3",
            ArtifactDomain::Episode,
            now,
            default_policy(),
            LifecycleState::Active,
        );

        let decisions = engine.evaluate_batch(&[&m1, &m2, &m3], now);

        assert_eq!(decisions.len(), 3);
        assert_eq!(decisions[0].artifact_uid, "batch-1");
        assert_eq!(decisions[1].artifact_uid, "batch-2");
        assert_eq!(decisions[2].artifact_uid, "batch-3");
    }

    #[test]
    fn batch_evaluation_empty_input() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let decisions = engine.evaluate_batch(&[], now);
        assert!(decisions.is_empty());
    }

    // -----------------------------------------------------------------------
    // RevalidateOnResume freshness
    // -----------------------------------------------------------------------

    #[test]
    fn revalidate_on_resume_active_when_recently_validated() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        // Pipeline uses RevalidateOnResume. Default stale threshold is 3600s.
        let produced_at = now - Duration::seconds(5000);
        let mut meta = make_metadata(
            "ror-ok",
            ArtifactDomain::Pipeline,
            produced_at,
            default_policy(),
            LifecycleState::Active,
        );
        // Validated 10 seconds ago — should be fresh.
        meta.last_validated_at = now - Duration::seconds(10);

        let decision = engine.evaluate(&meta, now);
        assert_eq!(decision.recommended_state, LifecycleState::Active);
    }

    #[test]
    fn revalidate_on_resume_stale_when_validation_old() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let produced_at = now - Duration::seconds(10000);
        let mut meta = make_metadata(
            "ror-stale",
            ArtifactDomain::Pipeline,
            produced_at,
            default_policy(),
            LifecycleState::Active,
        );
        // Validated 4000 seconds ago — exceeds 3600s threshold.
        meta.last_validated_at = now - Duration::seconds(4000);

        let decision = engine.evaluate(&meta, now);
        assert_eq!(decision.recommended_state, LifecycleState::Stale);
    }

    // -----------------------------------------------------------------------
    // Evergreen freshness
    // -----------------------------------------------------------------------

    #[test]
    fn evergreen_always_active() {
        let engine = PolicyEngine::new();
        let now = Utc::now();
        let produced_at = now - Duration::seconds(1_000_000); // very old

        let policy = PolicyBindings {
            freshness_class: FreshnessClass::Evergreen,
            ..default_policy()
        };

        let meta = make_metadata(
            "ever-1",
            ArtifactDomain::Execution,
            produced_at,
            policy,
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        assert_eq!(decision.recommended_state, LifecycleState::Active);
    }

    // -----------------------------------------------------------------------
    // Already-deleted artifacts
    // -----------------------------------------------------------------------

    #[test]
    fn deleted_artifact_stays_deleted() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let meta = make_metadata(
            "del-1",
            ArtifactDomain::Episode,
            now,
            default_policy(),
            LifecycleState::Deleted,
        );
        let decision = engine.evaluate(&meta, now);

        assert_eq!(decision.recommended_state, LifecycleState::Deleted);
        assert!(!decision.deletion_blocked);
    }

    // -----------------------------------------------------------------------
    // Default constructor
    // -----------------------------------------------------------------------

    #[test]
    fn default_impl_matches_new() {
        let from_new = PolicyEngine::new();
        let from_default = PolicyEngine::default();

        // Same number of domain defaults.
        assert_eq!(
            from_new.domain_defaults.len(),
            from_default.domain_defaults.len()
        );

        // Spot-check one domain.
        let new_wf = &from_new.domain_defaults[&ArtifactDomain::Workflow];
        let def_wf = &from_default.domain_defaults[&ArtifactDomain::Workflow];
        assert_eq!(new_wf.retention_class, def_wf.retention_class);
    }

    // -----------------------------------------------------------------------
    // Explanation field is populated
    // -----------------------------------------------------------------------

    #[test]
    fn explanation_is_non_empty() {
        let engine = PolicyEngine::new();
        let now = Utc::now();

        let meta = make_metadata(
            "expl-1",
            ArtifactDomain::Episode,
            now,
            default_policy(),
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        assert!(!decision.explanation.is_empty());
        assert!(decision.explanation.contains("expl-1"));
    }

    // -----------------------------------------------------------------------
    // Combined scenario: TTL expired but legal hold blocks
    // -----------------------------------------------------------------------

    #[test]
    fn legal_hold_trumps_ttl_expiry() {
        let engine = PolicyEngine::new();
        let now = Utc::now();
        let produced_at = now - Duration::seconds(10000);

        let policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 100 },
            freshness_class: FreshnessClass::Evergreen,
            protection_flags: ProtectionFlags {
                legal_hold: true,
                ..ProtectionFlags::default()
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "lh-ttl",
            ArtifactDomain::Execution,
            produced_at,
            policy,
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        // Legal hold wins: state preserved, deletion blocked.
        assert!(decision.deletion_blocked);
        assert_eq!(decision.decided_by, PolicyPrecedence::LegalHold);
        assert_eq!(decision.recommended_state, LifecycleState::Active);
    }

    // -----------------------------------------------------------------------
    // Freshness window boundary conditions
    // -----------------------------------------------------------------------

    #[test]
    fn freshness_window_exactly_at_fresh_boundary() {
        let engine = PolicyEngine::new();
        let now = Utc::now();
        // Exactly at the fresh_seconds boundary — should still be active
        // because the comparison is strict greater-than.
        let produced_at = now - Duration::seconds(300);

        let policy = PolicyBindings {
            freshness_class: FreshnessClass::Window {
                fresh_seconds: 300,
                stale_seconds: 600,
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "boundary-fresh",
            ArtifactDomain::Workflow,
            produced_at,
            policy,
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        // elapsed == fresh_seconds, not > fresh_seconds, so still active.
        assert_eq!(decision.recommended_state, LifecycleState::Active);
    }

    #[test]
    fn freshness_window_exactly_at_stale_boundary() {
        let engine = PolicyEngine::new();
        let now = Utc::now();
        let produced_at = now - Duration::seconds(600);

        let policy = PolicyBindings {
            freshness_class: FreshnessClass::Window {
                fresh_seconds: 300,
                stale_seconds: 600,
            },
            ..default_policy()
        };

        let meta = make_metadata(
            "boundary-stale",
            ArtifactDomain::Workflow,
            produced_at,
            policy,
            LifecycleState::Active,
        );
        let decision = engine.evaluate(&meta, now);

        // elapsed == stale_seconds, not > stale_seconds, so still stale.
        assert_eq!(decision.recommended_state, LifecycleState::Stale);
    }

    // -----------------------------------------------------------------------
    // Scope event with explicit TTL retention (not scope-based)
    // -----------------------------------------------------------------------

    #[test]
    fn ttl_artifact_not_expired_by_scope_events() {
        let engine = PolicyEngine {
            domain_defaults: HashMap::new(),
            global_default: PolicyBindings {
                retention_class: RetentionClass::Ttl { seconds: 3600 },
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
            policy_version: 1,
        };
        let now = Utc::now();

        let policy = PolicyBindings {
            retention_class: RetentionClass::Ttl { seconds: 3600 },
            ..default_policy()
        };

        let meta = make_metadata(
            "ttl-scope",
            ArtifactDomain::Pipeline,
            now,
            policy,
            LifecycleState::Active,
        );

        // No scope event should expire a TTL-based artifact.
        assert!(!engine.is_scope_expired(
            &meta,
            &ScopeEvent::TaskClosed {
                task_id: "t".to_string()
            }
        ));
        assert!(!engine.is_scope_expired(
            &meta,
            &ScopeEvent::GoalCompleted {
                goal_id: "g".to_string()
            }
        ));
    }
}
