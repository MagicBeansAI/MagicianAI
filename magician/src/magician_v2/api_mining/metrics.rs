//! Atomic counter modules for API Mining router decisions and projection
//! pipeline events. Both metric structs share the same shape: lock-free
//! `AtomicU64` per counter, aggregated via a `snapshot()` method that
//! returns a serializable view.
//!
//! These exist to give operators (and Forge tiles) a single place to
//! ask "how is mining behaving in production?" without parsing log
//! lines. The router calls `RouterMetrics::record(outcome)` at every
//! decision point; the projection pipeline calls
//! `ProjectionMetrics::record(event)` at every ingest/serve/migrate
//! step. Snapshots are cheap and lossless — readers see a consistent
//! point-in-time view even under concurrent writes (each counter is
//! independent; cross-counter "atomic snapshot" is not promised, but
//! is not needed for monotonic-counter aggregation).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/// What happened on a single router decision.
///
/// Mirrors the `PassThroughKind` typed enum on `RouteDecision`, plus
/// the `Replayed` / `RefusedNavigate` outcomes that aren't expressible
/// as a `PassThrough` variant. Executor wiring records exactly one
/// outcome per browser action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterOutcome {
    /// Router picked an API-replay path; the request fired against
    /// the origin and (assuming success) avoided the browser entirely.
    Replayed,
    /// Router considered replay but refused — typically because the
    /// captured session lacked auth state needed to make the request
    /// safely. Distinct from `PassThroughSessionNotReady` in that
    /// `RefusedNavigate` covers the case where we *would have* replayed
    /// but chose not to fire the network request at all.
    RefusedNavigate,
    /// Router was disabled (`enable_replay: false` or no registry).
    PassThroughRouterDisabled,
    /// Browser action isn't bound to any learned action signature.
    PassThroughNoBinding,
    /// No registered capability matches the request (URL / method / body).
    PassThroughNoCapability,
    /// Capability matched but confidence is below the per-method floor.
    PassThroughLowConfidence,
    /// Capability matched but replay machinery can't build a request
    /// (unknown side-effects, malformed template, etc.).
    PassThroughNotReplayable,
    /// Session context isn't ready (cookies/auth missing).
    PassThroughSessionNotReady,
    /// Router code panicked; failsafe pass-through fired.
    PassThroughRouterPanic,
    /// Catch-all for pass-through paths not covered by typed variants.
    PassThroughOther,
}

/// Lock-free per-scope counter bag for router outcomes.
///
/// Kept default-constructible so a router built without explicit
/// metrics still has somewhere to write. `Arc<RouterMetrics>` is the
/// expected shared shape — multiple router clones share one counter
/// pool, snapshots reflect everything written so far.
#[derive(Debug, Default)]
pub struct RouterMetrics {
    replayed: AtomicU64,
    refused_navigate: AtomicU64,
    pass_through_router_disabled: AtomicU64,
    pass_through_no_binding: AtomicU64,
    pass_through_no_capability: AtomicU64,
    pass_through_low_confidence: AtomicU64,
    pass_through_not_replayable: AtomicU64,
    pass_through_session_not_ready: AtomicU64,
    pass_through_router_panic: AtomicU64,
    pass_through_other: AtomicU64,
}

impl RouterMetrics {
    /// Record exactly one outcome. Cheap: a single relaxed atomic
    /// increment. Safe to call from hot paths under heavy concurrency.
    pub fn record(&self, outcome: RouterOutcome) {
        let counter = match outcome {
            RouterOutcome::Replayed => &self.replayed,
            RouterOutcome::RefusedNavigate => &self.refused_navigate,
            RouterOutcome::PassThroughRouterDisabled => &self.pass_through_router_disabled,
            RouterOutcome::PassThroughNoBinding => &self.pass_through_no_binding,
            RouterOutcome::PassThroughNoCapability => &self.pass_through_no_capability,
            RouterOutcome::PassThroughLowConfidence => &self.pass_through_low_confidence,
            RouterOutcome::PassThroughNotReplayable => &self.pass_through_not_replayable,
            RouterOutcome::PassThroughSessionNotReady => &self.pass_through_session_not_ready,
            RouterOutcome::PassThroughRouterPanic => &self.pass_through_router_panic,
            RouterOutcome::PassThroughOther => &self.pass_through_other,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Read all counters into a single serializable snapshot. Each
    /// counter is read independently; cross-counter atomicity is not
    /// promised but isn't needed for monotonic aggregation.
    pub fn snapshot(&self) -> RouterMetricsSnapshot {
        RouterMetricsSnapshot {
            replayed: self.replayed.load(Ordering::Relaxed),
            refused_navigate: self.refused_navigate.load(Ordering::Relaxed),
            pass_through_router_disabled: self.pass_through_router_disabled.load(Ordering::Relaxed),
            pass_through_no_binding: self.pass_through_no_binding.load(Ordering::Relaxed),
            pass_through_no_capability: self.pass_through_no_capability.load(Ordering::Relaxed),
            pass_through_low_confidence: self.pass_through_low_confidence.load(Ordering::Relaxed),
            pass_through_not_replayable: self.pass_through_not_replayable.load(Ordering::Relaxed),
            pass_through_session_not_ready: self
                .pass_through_session_not_ready
                .load(Ordering::Relaxed),
            pass_through_router_panic: self.pass_through_router_panic.load(Ordering::Relaxed),
            pass_through_other: self.pass_through_other.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time view of router counters. JSON-serializable for the
/// Forge tile + HTTP metrics endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RouterMetricsSnapshot {
    pub replayed: u64,
    pub refused_navigate: u64,
    pub pass_through_router_disabled: u64,
    pub pass_through_no_binding: u64,
    pub pass_through_no_capability: u64,
    pub pass_through_low_confidence: u64,
    pub pass_through_not_replayable: u64,
    pub pass_through_session_not_ready: u64,
    pub pass_through_router_panic: u64,
    pub pass_through_other: u64,
}

/// What happened on a single projection-pipeline event.
///
/// Projection lifecycle: a capability's first JSON response creates a
/// pending projection (`PendingCreated`); an operator approves it
/// (`Approved`); subsequent matching responses ingest rows
/// (`RowsIngested`) and the agent's `query_known_resource` reads
/// produce `RowsServedFromStore`. Schema drift adds new columns
/// (`SchemaConverged`); a hard schema break (column drop, type
/// change) records `MigrationRejected` and tells the operator to
/// `purge_rows` before retrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionEvent {
    /// New pending projection materialized from a first response sample.
    PendingCreated,
    /// Operator approved a pending projection; rows can now ingest.
    Approved,
    /// One or more rows ingested into an approved projection.
    RowsIngested,
    /// Agent `query_known_resource` returned rows from the projection
    /// (no network hit).
    RowsServedFromStore,
    /// Stale rows served (past `ttl_seconds`); caller decides whether
    /// to refresh.
    StaleServed,
    /// New columns appeared in a later response sample; additive
    /// migration applied (`ALTER TABLE ADD COLUMN`).
    SchemaConverged,
    /// Schema change rejected as non-additive (column drop or type
    /// change); operator must explicitly `purge_rows` first.
    MigrationRejected,
    /// Operator purged rows for a projection. Counts the action; the
    /// row count itself is recorded on disk per-projection.
    RowsPurged,
}

/// Lock-free per-scope counter bag for projection pipeline events.
#[derive(Debug, Default)]
pub struct ProjectionMetrics {
    pending_created: AtomicU64,
    approved: AtomicU64,
    rows_ingested: AtomicU64,
    rows_served_from_store: AtomicU64,
    stale_served: AtomicU64,
    schema_converged: AtomicU64,
    migration_rejected: AtomicU64,
    rows_purged: AtomicU64,
}

impl ProjectionMetrics {
    /// Record exactly one event. Cheap atomic increment.
    pub fn record(&self, event: ProjectionEvent) {
        let counter = match event {
            ProjectionEvent::PendingCreated => &self.pending_created,
            ProjectionEvent::Approved => &self.approved,
            ProjectionEvent::RowsIngested => &self.rows_ingested,
            ProjectionEvent::RowsServedFromStore => &self.rows_served_from_store,
            ProjectionEvent::StaleServed => &self.stale_served,
            ProjectionEvent::SchemaConverged => &self.schema_converged,
            ProjectionEvent::MigrationRejected => &self.migration_rejected,
            ProjectionEvent::RowsPurged => &self.rows_purged,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Read all counters into a single serializable snapshot.
    pub fn snapshot(&self) -> ProjectionMetricsSnapshot {
        ProjectionMetricsSnapshot {
            pending_created: self.pending_created.load(Ordering::Relaxed),
            approved: self.approved.load(Ordering::Relaxed),
            rows_ingested: self.rows_ingested.load(Ordering::Relaxed),
            rows_served_from_store: self.rows_served_from_store.load(Ordering::Relaxed),
            stale_served: self.stale_served.load(Ordering::Relaxed),
            schema_converged: self.schema_converged.load(Ordering::Relaxed),
            migration_rejected: self.migration_rejected.load(Ordering::Relaxed),
            rows_purged: self.rows_purged.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time view of projection counters. JSON-serializable.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectionMetricsSnapshot {
    pub pending_created: u64,
    pub approved: u64,
    pub rows_ingested: u64,
    pub rows_served_from_store: u64,
    pub stale_served: u64,
    pub schema_converged: u64,
    pub migration_rejected: u64,
    pub rows_purged: u64,
}

/// Lock-free per-scope counter bag for passive XHR/Fetch validation.
///
/// These counters are separate from active router/replay counters because
/// passive validation never replaces browser execution. It only compares a
/// browser-observed response with a replayed request and uses the result as
/// evidence for capability promotion or demotion.
#[derive(Debug, Default)]
pub struct PassiveValidationMetrics {
    observed_xhr_fetch: AtomicU64,
    matched_capabilities: AtomicU64,
    validations_fired: AtomicU64,
    validations_passed: AtomicU64,
    validations_failed: AtomicU64,
    skipped_non_xhr_fetch: AtomicU64,
    skipped_not_first_party: AtomicU64,
    skipped_no_match: AtomicU64,
    skipped_policy: AtomicU64,
    skipped_missing_response: AtomicU64,
    errors: AtomicU64,
    promoted_to_validated: AtomicU64,
    promoted_to_trusted: AtomicU64,
    budget_exhausted: AtomicU64,
}

impl PassiveValidationMetrics {
    pub fn record_observed_xhr_fetch(&self) {
        self.observed_xhr_fetch.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_matched_capability(&self) {
        self.matched_capabilities.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_validation_fired(&self) {
        self.validations_fired.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_validation_passed(&self) {
        self.validations_passed.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_validation_failed(&self) {
        self.validations_failed.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_skipped_non_xhr_fetch(&self) {
        self.skipped_non_xhr_fetch.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_skipped_not_first_party(&self) {
        self.skipped_not_first_party.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_skipped_no_match(&self) {
        self.skipped_no_match.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_skipped_policy(&self) {
        self.skipped_policy.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_skipped_missing_response(&self) {
        self.skipped_missing_response
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_promoted_to_validated(&self) {
        self.promoted_to_validated.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_promoted_to_trusted(&self) {
        self.promoted_to_trusted.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_budget_exhausted(&self) {
        self.budget_exhausted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> PassiveValidationMetricsSnapshot {
        PassiveValidationMetricsSnapshot {
            observed_xhr_fetch: self.observed_xhr_fetch.load(Ordering::Relaxed),
            matched_capabilities: self.matched_capabilities.load(Ordering::Relaxed),
            validations_fired: self.validations_fired.load(Ordering::Relaxed),
            validations_passed: self.validations_passed.load(Ordering::Relaxed),
            validations_failed: self.validations_failed.load(Ordering::Relaxed),
            skipped_non_xhr_fetch: self.skipped_non_xhr_fetch.load(Ordering::Relaxed),
            skipped_not_first_party: self.skipped_not_first_party.load(Ordering::Relaxed),
            skipped_no_match: self.skipped_no_match.load(Ordering::Relaxed),
            skipped_policy: self.skipped_policy.load(Ordering::Relaxed),
            skipped_missing_response: self.skipped_missing_response.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            promoted_to_validated: self.promoted_to_validated.load(Ordering::Relaxed),
            promoted_to_trusted: self.promoted_to_trusted.load(Ordering::Relaxed),
            budget_exhausted: self.budget_exhausted.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time view of passive validation counters. JSON-serializable.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PassiveValidationMetricsSnapshot {
    pub observed_xhr_fetch: u64,
    pub matched_capabilities: u64,
    pub validations_fired: u64,
    pub validations_passed: u64,
    pub validations_failed: u64,
    pub skipped_non_xhr_fetch: u64,
    pub skipped_not_first_party: u64,
    pub skipped_no_match: u64,
    pub skipped_policy: u64,
    pub skipped_missing_response: u64,
    pub errors: u64,
    pub promoted_to_validated: u64,
    pub promoted_to_trusted: u64,
    pub budget_exhausted: u64,
}

/// Lock-free per-scope counter bag for Phase 1 sequence capture.
///
/// Three counters:
/// - `started`: `SequenceRecorder::start` lazy-init fired for this scope.
/// - `finalized`: a non-empty sequence was finalized and persisted to disk.
/// - `with_browser_only_steps`: the finalized sequence had >=1 step with
///   `executed_via=Browser` AND no correlated `capability_id` — these are
///   the steps that block fully browserless replay in Phase 3.
#[derive(Debug, Default)]
pub struct SequenceMetrics {
    started: AtomicU64,
    finalized: AtomicU64,
    with_browser_only_steps: AtomicU64,
}

impl SequenceMetrics {
    pub fn record_started(&self) {
        self.started.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_finalized(&self) {
        self.finalized.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_finalized_with_browser_only(&self) {
        self.finalized.fetch_add(1, Ordering::Relaxed);
        self.with_browser_only_steps.fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> SequenceMetricsSnapshot {
        SequenceMetricsSnapshot {
            started: self.started.load(Ordering::Relaxed),
            finalized: self.finalized.load(Ordering::Relaxed),
            with_browser_only_steps: self.with_browser_only_steps.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time view of sequence capture counters. JSON-serializable.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SequenceMetricsSnapshot {
    pub started: u64,
    pub finalized: u64,
    pub with_browser_only_steps: u64,
}

/// Lock-free per-scope counter bag for Phase 2 workflow compilation.
///
/// Four counters:
/// - `compile_started`: `compile_from_sequences` invoked for this scope.
/// - `compile_succeeded`: LLM produced a valid `WorkflowGraph` and
///   `WorkflowStore::save` succeeded.
/// - `compile_failed`: save failed OR LLM call + retry both failed AND
///   the draft fallback also failed to produce a usable graph.
/// - `compile_fell_back_to_draft`: LLM/validation failed twice; the
///   draft fallback produced the graph. Counted separately from
///   `compile_succeeded` so operators can spot LLM degradation.
#[derive(Debug, Default)]
pub struct WorkflowMetrics {
    compile_started: AtomicU64,
    compile_succeeded: AtomicU64,
    compile_failed: AtomicU64,
    compile_fell_back_to_draft: AtomicU64,
}

impl WorkflowMetrics {
    pub fn record_compile_started(&self) {
        self.compile_started.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_compile_succeeded(&self) {
        self.compile_succeeded.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_compile_failed(&self) {
        self.compile_failed.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_compile_fell_back_to_draft(&self) {
        self.compile_fell_back_to_draft
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> WorkflowMetricsSnapshot {
        WorkflowMetricsSnapshot {
            compile_started: self.compile_started.load(Ordering::Relaxed),
            compile_succeeded: self.compile_succeeded.load(Ordering::Relaxed),
            compile_failed: self.compile_failed.load(Ordering::Relaxed),
            compile_fell_back_to_draft: self.compile_fell_back_to_draft.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time view of workflow compilation counters. JSON-serializable.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkflowMetricsSnapshot {
    pub compile_started: u64,
    pub compile_succeeded: u64,
    pub compile_failed: u64,
    pub compile_fell_back_to_draft: u64,
}

/// Lock-free per-scope counter bag for Phase 3 workflow replay.
///
/// Counters:
/// - `replay_started`: `WorkflowReplayEngine::replay` invoked.
/// - `replay_succeeded`: full graph completed without error.
/// - `replay_failed_browser_only`: pre-flight rejected because the graph
///   contains an `browser_only: true` step (API-only path can't run it).
/// - `replay_failed_param_resolution`: a `ParamSource` could not be
///   resolved (unknown data_flow_id, missing prior response, missing
///   user_input, JSONPath miss).
/// - `replay_failed_http`: a step returned non-2xx, network error, or
///   missing capability.
/// - `replay_promoted_to_candidate` / `_validated` / `_trusted`: maturity
///   ladder transition counters.
#[derive(Debug, Default)]
pub struct ReplayMetrics {
    replay_started: AtomicU64,
    replay_succeeded: AtomicU64,
    replay_failed_browser_only: AtomicU64,
    replay_failed_param_resolution: AtomicU64,
    replay_failed_http: AtomicU64,
    replay_failed_timeout: AtomicU64,
    replay_promoted_to_candidate: AtomicU64,
    replay_promoted_to_validated: AtomicU64,
    replay_promoted_to_trusted: AtomicU64,
}

impl ReplayMetrics {
    pub fn record_replay_started(&self) {
        self.replay_started.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_replay_succeeded(&self) {
        self.replay_succeeded.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_failed_browser_only(&self) {
        self.replay_failed_browser_only
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_failed_param_resolution(&self) {
        self.replay_failed_param_resolution
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_failed_http(&self) {
        self.replay_failed_http.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_failed_timeout(&self) {
        self.replay_failed_timeout.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_promoted_to_candidate(&self) {
        self.replay_promoted_to_candidate
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_promoted_to_validated(&self) {
        self.replay_promoted_to_validated
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_promoted_to_trusted(&self) {
        self.replay_promoted_to_trusted
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> ReplayMetricsSnapshot {
        ReplayMetricsSnapshot {
            replay_started: self.replay_started.load(Ordering::Relaxed),
            replay_succeeded: self.replay_succeeded.load(Ordering::Relaxed),
            replay_failed_browser_only: self.replay_failed_browser_only.load(Ordering::Relaxed),
            replay_failed_param_resolution: self
                .replay_failed_param_resolution
                .load(Ordering::Relaxed),
            replay_failed_http: self.replay_failed_http.load(Ordering::Relaxed),
            replay_failed_timeout: self.replay_failed_timeout.load(Ordering::Relaxed),
            replay_promoted_to_candidate: self.replay_promoted_to_candidate.load(Ordering::Relaxed),
            replay_promoted_to_validated: self.replay_promoted_to_validated.load(Ordering::Relaxed),
            replay_promoted_to_trusted: self.replay_promoted_to_trusted.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time view of workflow replay counters. JSON-serializable.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReplayMetricsSnapshot {
    pub replay_started: u64,
    pub replay_succeeded: u64,
    pub replay_failed_browser_only: u64,
    pub replay_failed_param_resolution: u64,
    pub replay_failed_http: u64,
    pub replay_failed_timeout: u64,
    pub replay_promoted_to_candidate: u64,
    pub replay_promoted_to_validated: u64,
    pub replay_promoted_to_trusted: u64,
}

/// Lock-free counters for the task-recipe rail.
#[derive(Debug, Default)]
pub struct RecipeMetrics {
    lookup_hit: AtomicU64,
    lookup_miss: AtomicU64,
    replay_started: AtomicU64,
    replay_succeeded: AtomicU64,
    replay_failed_auth: AtomicU64,
    replay_failed_anti_bot: AtomicU64,
    replay_failed_schema_drift: AtomicU64,
    replay_failed_http: AtomicU64,
    replay_failed_network: AtomicU64,
    replay_failed_policy: AtomicU64,
    replay_failed_input: AtomicU64,
    fallback_handoffs: AtomicU64,
    grants_created: AtomicU64,
    grants_used: AtomicU64,
    approvals_denied: AtomicU64,
    auth_heals: AtomicU64,
}

impl RecipeMetrics {
    pub fn record_lookup_hit(&self) {
        self.lookup_hit.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_lookup_miss(&self) {
        self.lookup_miss.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_replay_started(&self) {
        self.replay_started.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_replay_succeeded(&self) {
        self.replay_succeeded.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_fallback_handoff(&self) {
        self.fallback_handoffs.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_grant_created(&self) {
        self.grants_created.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_grant_used(&self) {
        self.grants_used.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_approval_denied(&self) {
        self.approvals_denied.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_auth_heal(&self) {
        self.auth_heals.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_replay_failed(
        &self,
        class: Option<crate::magician_v2::api_mining::recipe_runner::FailureClass>,
    ) {
        use crate::magician_v2::api_mining::recipe_runner::FailureClass;
        let counter = match class {
            Some(FailureClass::Auth) => &self.replay_failed_auth,
            Some(FailureClass::AntiBot) => &self.replay_failed_anti_bot,
            Some(FailureClass::SchemaDrift) => &self.replay_failed_schema_drift,
            Some(FailureClass::Network) => &self.replay_failed_network,
            Some(FailureClass::PolicyBlocked) => &self.replay_failed_policy,
            Some(FailureClass::InputMissing) => &self.replay_failed_input,
            Some(FailureClass::Http) | None => &self.replay_failed_http,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> RecipeMetricsSnapshot {
        RecipeMetricsSnapshot {
            lookup_hit: self.lookup_hit.load(Ordering::Relaxed),
            lookup_miss: self.lookup_miss.load(Ordering::Relaxed),
            replay_started: self.replay_started.load(Ordering::Relaxed),
            replay_succeeded: self.replay_succeeded.load(Ordering::Relaxed),
            replay_failed_auth: self.replay_failed_auth.load(Ordering::Relaxed),
            replay_failed_anti_bot: self.replay_failed_anti_bot.load(Ordering::Relaxed),
            replay_failed_schema_drift: self.replay_failed_schema_drift.load(Ordering::Relaxed),
            replay_failed_http: self.replay_failed_http.load(Ordering::Relaxed),
            replay_failed_network: self.replay_failed_network.load(Ordering::Relaxed),
            replay_failed_policy: self.replay_failed_policy.load(Ordering::Relaxed),
            replay_failed_input: self.replay_failed_input.load(Ordering::Relaxed),
            fallback_handoffs: self.fallback_handoffs.load(Ordering::Relaxed),
            grants_created: self.grants_created.load(Ordering::Relaxed),
            grants_used: self.grants_used.load(Ordering::Relaxed),
            approvals_denied: self.approvals_denied.load(Ordering::Relaxed),
            auth_heals: self.auth_heals.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RecipeMetricsSnapshot {
    pub lookup_hit: u64,
    pub lookup_miss: u64,
    pub replay_started: u64,
    pub replay_succeeded: u64,
    pub replay_failed_auth: u64,
    pub replay_failed_anti_bot: u64,
    pub replay_failed_schema_drift: u64,
    pub replay_failed_http: u64,
    pub replay_failed_network: u64,
    pub replay_failed_policy: u64,
    pub replay_failed_input: u64,
    pub fallback_handoffs: u64,
    pub grants_created: u64,
    pub grants_used: u64,
    pub approvals_denied: u64,
    pub auth_heals: u64,
}

/// Per-(principal, workspace) shared metrics handles.
///
/// Routers and projection pipelines constructed for the same scope
/// resolve to the same `Arc<RouterMetrics>` + `Arc<ProjectionMetrics>`
/// via this registry. HTTP endpoints (`GET /router-metrics`,
/// `/projection-metrics`) look up the registry by scope to return a
/// snapshot. Counters live for the process lifetime; restart resets
/// to zero. Per-scope, not cross-scope, so noise from one workspace
/// doesn't pollute another.
#[derive(Default)]
struct ScopedMetrics {
    router: Arc<RouterMetrics>,
    projection: Arc<ProjectionMetrics>,
    passive_validation: Arc<PassiveValidationMetrics>,
    sequence: Arc<SequenceMetrics>,
    workflow: Arc<WorkflowMetrics>,
    replay: Arc<ReplayMetrics>,
    recipe: Arc<RecipeMetrics>,
}

static SCOPED_METRICS_REGISTRY: OnceLock<RwLock<HashMap<(String, String), ScopedMetrics>>> =
    OnceLock::new();

fn registry() -> &'static RwLock<HashMap<(String, String), ScopedMetrics>> {
    SCOPED_METRICS_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Resolve (or create) the shared `Arc<RouterMetrics>` for a scope.
/// Called by the router factory on construction; subsequent calls
/// with the same scope return the same `Arc`, so every router for
/// (principal, workspace) shares one counter pool.
pub fn router_metrics_for_scope(principal: &str, workspace: &str) -> Arc<RouterMetrics> {
    let key = (principal.to_string(), workspace.to_string());
    {
        let read = registry()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = read.get(&key) {
            return Arc::clone(&entry.router);
        }
    }
    let mut write = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = write.entry(key).or_default();
    Arc::clone(&entry.router)
}

/// Resolve (or create) the shared `Arc<ProjectionMetrics>` for a
/// scope. Mirrors `router_metrics_for_scope`.
pub fn projection_metrics_for_scope(principal: &str, workspace: &str) -> Arc<ProjectionMetrics> {
    let key = (principal.to_string(), workspace.to_string());
    {
        let read = registry()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = read.get(&key) {
            return Arc::clone(&entry.projection);
        }
    }
    let mut write = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = write.entry(key).or_default();
    Arc::clone(&entry.projection)
}

/// Snapshot the router counters for a scope. Returns the default
/// (all-zero) snapshot if no router has been constructed for this
/// scope yet, so the HTTP endpoint can answer cleanly on cold scopes.
pub fn router_snapshot_for_scope(principal: &str, workspace: &str) -> RouterMetricsSnapshot {
    router_metrics_for_scope(principal, workspace).snapshot()
}

/// Snapshot the projection counters for a scope. See `router_snapshot_for_scope`.
pub fn projection_snapshot_for_scope(
    principal: &str,
    workspace: &str,
) -> ProjectionMetricsSnapshot {
    projection_metrics_for_scope(principal, workspace).snapshot()
}

/// Resolve (or create) the shared `Arc<PassiveValidationMetrics>` for a scope.
/// Mirrors `router_metrics_for_scope` / `projection_metrics_for_scope`.
pub fn passive_validation_metrics_for_scope(
    principal: &str,
    workspace: &str,
) -> Arc<PassiveValidationMetrics> {
    let key = (principal.to_string(), workspace.to_string());
    {
        let read = registry()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = read.get(&key) {
            return Arc::clone(&entry.passive_validation);
        }
    }
    let mut write = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = write.entry(key).or_default();
    Arc::clone(&entry.passive_validation)
}

/// Snapshot passive validation counters for a scope.
pub fn passive_validation_snapshot_for_scope(
    principal: &str,
    workspace: &str,
) -> PassiveValidationMetricsSnapshot {
    passive_validation_metrics_for_scope(principal, workspace).snapshot()
}

/// Resolve (or create) the shared `Arc<SequenceMetrics>` for a scope.
/// Mirrors `router_metrics_for_scope` / `projection_metrics_for_scope`.
pub fn sequence_metrics_for_scope(principal: &str, workspace: &str) -> Arc<SequenceMetrics> {
    let key = (principal.to_string(), workspace.to_string());
    {
        let read = registry()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = read.get(&key) {
            return Arc::clone(&entry.sequence);
        }
    }
    let mut write = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = write.entry(key).or_default();
    Arc::clone(&entry.sequence)
}

/// Snapshot the sequence counters for a scope. See `router_snapshot_for_scope`.
pub fn sequence_snapshot_for_scope(principal: &str, workspace: &str) -> SequenceMetricsSnapshot {
    sequence_metrics_for_scope(principal, workspace).snapshot()
}

/// Resolve (or create) the shared `Arc<WorkflowMetrics>` for a scope.
/// Mirrors `sequence_metrics_for_scope`.
pub fn workflow_metrics_for_scope(principal: &str, workspace: &str) -> Arc<WorkflowMetrics> {
    let key = (principal.to_string(), workspace.to_string());
    {
        let read = registry()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = read.get(&key) {
            return Arc::clone(&entry.workflow);
        }
    }
    let mut write = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = write.entry(key).or_default();
    Arc::clone(&entry.workflow)
}

/// Snapshot the workflow compilation counters for a scope.
pub fn workflow_snapshot_for_scope(principal: &str, workspace: &str) -> WorkflowMetricsSnapshot {
    workflow_metrics_for_scope(principal, workspace).snapshot()
}

/// Resolve (or create) the shared `Arc<ReplayMetrics>` for a scope.
/// Mirrors `workflow_metrics_for_scope`.
pub fn replay_metrics_for_scope(principal: &str, workspace: &str) -> Arc<ReplayMetrics> {
    let key = (principal.to_string(), workspace.to_string());
    {
        let read = registry()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = read.get(&key) {
            return Arc::clone(&entry.replay);
        }
    }
    let mut write = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = write.entry(key).or_default();
    Arc::clone(&entry.replay)
}

/// Snapshot the workflow replay counters for a scope.
pub fn replay_snapshot_for_scope(principal: &str, workspace: &str) -> ReplayMetricsSnapshot {
    replay_metrics_for_scope(principal, workspace).snapshot()
}

pub fn recipe_metrics_for_scope(principal: &str, workspace: &str) -> Arc<RecipeMetrics> {
    let key = (principal.to_owned(), workspace.to_owned());
    {
        let read = registry()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = read.get(&key) {
            return Arc::clone(&entry.recipe);
        }
    }
    let mut write = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(&write.entry(key).or_default().recipe)
}

pub fn recipe_snapshot_for_scope(principal: &str, workspace: &str) -> RecipeMetricsSnapshot {
    recipe_metrics_for_scope(principal, workspace).snapshot()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn router_counter_aggregates_each_outcome() {
        let m = RouterMetrics::default();
        m.record(RouterOutcome::Replayed);
        m.record(RouterOutcome::Replayed);
        m.record(RouterOutcome::PassThroughNoBinding);
        let snap = m.snapshot();
        assert_eq!(snap.replayed, 2);
        assert_eq!(snap.pass_through_no_binding, 1);
        assert_eq!(snap.refused_navigate, 0);
        assert_eq!(snap.pass_through_router_disabled, 0);
    }

    #[test]
    fn projection_counter_records_ingest_and_serve() {
        let m = ProjectionMetrics::default();
        m.record(ProjectionEvent::PendingCreated);
        m.record(ProjectionEvent::Approved);
        m.record(ProjectionEvent::RowsIngested);
        m.record(ProjectionEvent::RowsIngested);
        m.record(ProjectionEvent::RowsServedFromStore);
        let snap = m.snapshot();
        assert_eq!(snap.pending_created, 1);
        assert_eq!(snap.approved, 1);
        assert_eq!(snap.rows_ingested, 2);
        assert_eq!(snap.rows_served_from_store, 1);
        assert_eq!(snap.stale_served, 0);
        assert_eq!(snap.schema_converged, 0);
    }

    #[test]
    fn passive_validation_metrics_track_observe_validate_and_promote_counts() {
        let metrics = PassiveValidationMetrics::default();
        metrics.record_observed_xhr_fetch();
        metrics.record_matched_capability();
        metrics.record_validation_fired();
        metrics.record_validation_passed();
        metrics.record_validation_failed();
        metrics.record_skipped_policy();
        metrics.record_error();
        metrics.record_promoted_to_validated();
        metrics.record_budget_exhausted();

        let snap = metrics.snapshot();
        assert_eq!(snap.observed_xhr_fetch, 1);
        assert_eq!(snap.matched_capabilities, 1);
        assert_eq!(snap.validations_fired, 1);
        assert_eq!(snap.validations_passed, 1);
        assert_eq!(snap.validations_failed, 1);
        assert_eq!(snap.skipped_policy, 1);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.promoted_to_validated, 1);
        assert_eq!(snap.budget_exhausted, 1);
    }

    #[test]
    fn sequence_metrics_track_started_finalized_and_browser_only_counts() {
        let metrics = SequenceMetrics::default();
        metrics.record_started();
        metrics.record_started();
        metrics.record_finalized();
        metrics.record_finalized_with_browser_only();

        let snap = metrics.snapshot();
        assert_eq!(snap.started, 2);
        assert_eq!(snap.finalized, 2);
        assert_eq!(snap.with_browser_only_steps, 1);
    }

    #[test]
    fn replay_metrics_track_started_succeeded_failures_and_promotions() {
        let metrics = ReplayMetrics::default();
        metrics.record_replay_started();
        metrics.record_replay_started();
        metrics.record_replay_started();
        metrics.record_replay_succeeded();
        metrics.record_failed_browser_only();
        metrics.record_failed_param_resolution();
        metrics.record_failed_http();
        metrics.record_failed_timeout();
        metrics.record_promoted_to_candidate();
        metrics.record_promoted_to_validated();
        metrics.record_promoted_to_trusted();

        let snap = metrics.snapshot();
        assert_eq!(snap.replay_started, 3);
        assert_eq!(snap.replay_succeeded, 1);
        assert_eq!(snap.replay_failed_browser_only, 1);
        assert_eq!(snap.replay_failed_param_resolution, 1);
        assert_eq!(snap.replay_failed_http, 1);
        assert_eq!(snap.replay_failed_timeout, 1);
        assert_eq!(snap.replay_promoted_to_candidate, 1);
        assert_eq!(snap.replay_promoted_to_validated, 1);
        assert_eq!(snap.replay_promoted_to_trusted, 1);
    }

    #[test]
    fn workflow_metrics_track_compile_started_succeeded_failed_and_fell_back() {
        let metrics = WorkflowMetrics::default();
        metrics.record_compile_started();
        metrics.record_compile_started();
        metrics.record_compile_started();
        metrics.record_compile_succeeded();
        metrics.record_compile_failed();
        metrics.record_compile_fell_back_to_draft();

        let snap = metrics.snapshot();
        assert_eq!(snap.compile_started, 3);
        assert_eq!(snap.compile_succeeded, 1);
        assert_eq!(snap.compile_failed, 1);
        assert_eq!(snap.compile_fell_back_to_draft, 1);
    }

    #[test]
    fn router_metrics_snapshot_is_lossless_under_concurrency() {
        // Spin a few threads each hammering one counter; the snapshot
        // must reflect the sum exactly (monotonic counters give us
        // this without cross-counter coordination).
        use std::sync::Arc;
        use std::thread;

        let m = Arc::new(RouterMetrics::default());
        let mut handles = Vec::new();
        for _ in 0..4 {
            let m = Arc::clone(&m);
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    m.record(RouterOutcome::Replayed);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(m.snapshot().replayed, 4 * 1000);
    }
}
