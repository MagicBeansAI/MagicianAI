//! Canonical app resource-authority contract and provider-free replay kernel.
//!
//! This module is deliberately dormant in Phase 0. It does not persist a
//! ledger, register a route, dispatch work, or replace any existing runtime
//! meter. It freezes the identity, reservation, observation and settlement
//! semantics that the Phase-4B durable authority must implement. Existing
//! token, LLM-cost, tool, browser, storage and VibeDev active-time owners are
//! observation/adopter seams; none is independently allowed to answer
//! "remaining app budget".

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    authority::{AuthenticatedAppScope, ResolvedAppAuthority},
    models::{
        decode_app_contract, AppContractError, AppContractLimits, AppDigest, AppInstallationId,
        AppName, AppReference, AppRevision, ValidateAppContract,
    },
    records::{AppResourceBehaviorIdentity, AppResourceCeiling, AppRunBinding, AppScope},
};

const HARD_MAX_TREE_NODES: u32 = 4_096;
const HARD_MAX_TREE_DEPTH: u16 = 32;
pub const HARD_MAX_JOURNAL_EVENTS: u32 = 32_768;
const HARD_MAX_RESERVATIONS: u32 = 8_192;
const HARD_MAX_ACTIVE_INTERVALS: u32 = 32_768;
const HARD_MAX_CAPABILITY_FAMILIES: u16 = 256;
const HARD_MAX_OBSERVATION_SOURCES: usize = 16;

/// Durable identity for one complete app execution tree.
///
/// This is a stored claim, not current authority. Replaying it requires a
/// freshly server-minted [`CurrentAppResourceAuthority`] with exact identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceTreeIdentity {
    pub scope: AppScope,
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub package_revision_ref: AppReference,
    pub grant_revision: AppRevision,
    pub schema_revision: AppRevision,
    pub authority_digest: AppDigest,
    /// Scheduler-sealed behavior dimension. `None` keeps the legacy ordinary
    /// app-task identity and installation-wide accounting semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_resource_identity: Option<AppResourceBehaviorIdentity>,
    pub root_execution_id: AppReference,
    pub budget_ledger_ref: AppReference,
    pub installation_period_ref: AppReference,
}

impl ValidateAppContract for AppResourceTreeIdentity {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.installation_generation == 0 {
            return Err(AppContractError::invalid(
                "installation_generation",
                "must be greater than zero",
            ));
        }
        if let Some(identity) = self.behavior_resource_identity.as_ref() {
            identity.validate_app_contract(_limits)?;
        }
        Ok(())
    }
}

/// Fixed implementation-safety and scheduler policy applied by the canonical
/// authority. Values can narrow the hard implementation caps but never widen
/// them. This policy is server configuration, not an app manifest field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceEnforcementPolicy {
    pub max_tree_nodes: u32,
    pub max_tree_depth: u16,
    pub max_journal_events: u32,
    pub max_reservations: u32,
    pub max_active_intervals: u32,
    pub max_capability_families: u16,
    pub max_no_progress_seconds: u64,
    pub max_package_bytes: u64,
    pub max_background_starts_per_period: u64,
    pub scheduler_capacity: u16,
    pub foreground_reserved_slots: u16,
}

impl AppResourceEnforcementPolicy {
    /// Validate server-owned implementation and scheduler limits without an
    /// app ceiling. Configuration loading uses this exact boundary so it
    /// cannot accept values that the execution authority would later reject.
    /// Per-app lifetime compatibility remains part of admission below.
    pub fn validate_server_configuration(self) -> Result<Self, AppResourceContractError> {
        if self.max_tree_nodes == 0 || self.max_tree_nodes > HARD_MAX_TREE_NODES {
            return Err(AppResourceContractError::InvalidPolicy(
                "max_tree_nodes must be positive and no greater than the hard cap",
            ));
        }
        if self.max_tree_depth == 0 || self.max_tree_depth > HARD_MAX_TREE_DEPTH {
            return Err(AppResourceContractError::InvalidPolicy(
                "max_tree_depth must be positive and no greater than the hard cap",
            ));
        }
        if self.max_journal_events == 0 || self.max_journal_events > HARD_MAX_JOURNAL_EVENTS {
            return Err(AppResourceContractError::InvalidPolicy(
                "max_journal_events must be positive and no greater than the hard cap",
            ));
        }
        if self.max_reservations == 0 || self.max_reservations > HARD_MAX_RESERVATIONS {
            return Err(AppResourceContractError::InvalidPolicy(
                "max_reservations must be positive and no greater than the hard cap",
            ));
        }
        if self.max_active_intervals == 0 || self.max_active_intervals > HARD_MAX_ACTIVE_INTERVALS {
            return Err(AppResourceContractError::InvalidPolicy(
                "max_active_intervals must be positive and no greater than the hard cap",
            ));
        }
        if self.max_capability_families == 0
            || self.max_capability_families > HARD_MAX_CAPABILITY_FAMILIES
        {
            return Err(AppResourceContractError::InvalidPolicy(
                "max_capability_families must be positive and no greater than the hard cap",
            ));
        }
        if self.max_no_progress_seconds == 0 {
            return Err(AppResourceContractError::InvalidPolicy(
                "max_no_progress_seconds must be positive",
            ));
        }
        if self.max_package_bytes == 0 {
            return Err(AppResourceContractError::InvalidPolicy(
                "max_package_bytes must be positive",
            ));
        }
        if self.scheduler_capacity == 0 || self.foreground_reserved_slots > self.scheduler_capacity
        {
            return Err(AppResourceContractError::InvalidPolicy(
                "foreground_reserved_slots must fit within positive scheduler capacity",
            ));
        }
        Ok(self)
    }

    fn validate(self, ceiling: &AppResourceCeiling) -> Result<(), AppResourceContractError> {
        self.validate_server_configuration()?;
        if ceiling.max_lifetime_seconds != 0
            && self.max_no_progress_seconds > ceiling.max_lifetime_seconds
        {
            return Err(AppResourceContractError::InvalidPolicy(
                "max_no_progress_seconds must be within max_lifetime_seconds",
            ));
        }
        Ok(())
    }
}

/// Installation-period and scheduler state external to this root.
///
/// The durable Phase-4B authority must compute these values atomically while
/// excluding `budget_ledger_ref`, then refresh the period revision for later
/// decisions. That prevents both double-counting this tree and ignoring spend
/// from concurrent trees.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourcePeriodBaseline {
    scope: AppScope,
    installation_id: AppInstallationId,
    installation_generation: u64,
    excluded_budget_ledger_ref: AppReference,
    period_ref: AppReference,
    period_revision: AppRevision,
    period_ends_at_elapsed_ms: u64,
    tokens_excluding_root: u64,
    outstanding_tokens_excluding_root: u64,
    cost_microusd_excluding_root: u64,
    outstanding_cost_microusd_excluding_root: u64,
    /// Behavior-local monthly totals are an additional ceiling dimension.
    /// Installation totals above remain authoritative for the app-wide grant.
    behavior_period: Option<AppResourceBehaviorPeriodBaseline>,
    background_starts_excluding_root: u64,
    installation_foreground_runs_excluding_root: u16,
    installation_background_runs_excluding_root: u16,
    scheduler_foreground_runs_excluding_root: u16,
    scheduler_background_runs_excluding_root: u16,
    package_bytes: u64,
}

/// Trusted store projection for the exact behavior discriminator carried by
/// the root binding. It is non-deserializable and cannot be minted by package
/// or journal bytes.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppResourceBehaviorPeriodBaseline {
    ledger_dimension_digest: AppDigest,
    tokens_excluding_root: u64,
    outstanding_tokens_excluding_root: u64,
    cost_microusd_excluding_root: u64,
    outstanding_cost_microusd_excluding_root: u64,
}

impl AppResourceBehaviorPeriodBaseline {
    pub(crate) fn from_resource_store(
        identity: &AppResourceBehaviorIdentity,
        tokens_excluding_root: u64,
        outstanding_tokens_excluding_root: u64,
        cost_microusd_excluding_root: u64,
        outstanding_cost_microusd_excluding_root: u64,
    ) -> Self {
        Self {
            ledger_dimension_digest: identity.ledger_dimension_digest().clone(),
            tokens_excluding_root,
            outstanding_tokens_excluding_root,
            cost_microusd_excluding_root,
            outstanding_cost_microusd_excluding_root,
        }
    }
}

impl AppResourcePeriodBaseline {
    /// Trusted resource-store adapter seam. This does not authenticate or read
    /// a store by itself, and it is intentionally unavailable outside the
    /// crate and has no transport deserializer.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_resource_store(
        scope: AppScope,
        installation_id: AppInstallationId,
        installation_generation: u64,
        excluded_budget_ledger_ref: AppReference,
        period_ref: AppReference,
        period_revision: AppRevision,
        period_ends_at_elapsed_ms: u64,
        tokens_excluding_root: u64,
        outstanding_tokens_excluding_root: u64,
        cost_microusd_excluding_root: u64,
        outstanding_cost_microusd_excluding_root: u64,
        behavior_period: Option<AppResourceBehaviorPeriodBaseline>,
        background_starts_excluding_root: u64,
        installation_foreground_runs_excluding_root: u16,
        installation_background_runs_excluding_root: u16,
        scheduler_foreground_runs_excluding_root: u16,
        scheduler_background_runs_excluding_root: u16,
        package_bytes: u64,
    ) -> Self {
        Self {
            scope,
            installation_id,
            installation_generation,
            excluded_budget_ledger_ref,
            period_ref,
            period_revision,
            period_ends_at_elapsed_ms,
            tokens_excluding_root,
            outstanding_tokens_excluding_root,
            cost_microusd_excluding_root,
            outstanding_cost_microusd_excluding_root,
            behavior_period,
            background_starts_excluding_root,
            installation_foreground_runs_excluding_root,
            installation_background_runs_excluding_root,
            scheduler_foreground_runs_excluding_root,
            scheduler_background_runs_excluding_root,
            package_bytes,
        }
    }
}

/// Current server-owned evidence required to evaluate or admit any event.
///
/// Private fields plus the absence of `Deserialize` prevent a package, client
/// or recovered journal from minting remaining budget. It is a fence, not a
/// bearer token: consequential adapters must still re-resolve current app and
/// resource-store state at their boundary.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentAppResourceAuthority {
    identity: AppResourceTreeIdentity,
    ceiling: AppResourceCeiling,
    policy: AppResourceEnforcementPolicy,
    baseline: AppResourcePeriodBaseline,
}

impl CurrentAppResourceAuthority {
    pub fn from_current_authority(
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        binding: &AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        baseline: AppResourcePeriodBaseline,
        now: DateTime<Utc>,
    ) -> Result<Self, AppResourceContractError> {
        ensure_current_authority_freshness(authenticated_scope, &resolved.resolved_at, &now)?;
        Self::from_authority_snapshot(
            authenticated_scope,
            resolved,
            binding,
            policy,
            baseline,
            None,
        )
    }

    /// Rebuild the evaluator for settlement/closure of work that already owns
    /// a process-minted root lease (and, for settlement, an operation permit).
    /// A lifecycle revocation after dispatch must stop new reservations, but it
    /// must not make already-observed spend disappear or strand the tree open.
    /// The accepted immutable tree identity replaces freshness as the fence;
    /// authenticated scope, the canonical effective-authority digest and every
    /// durable identity component are still checked exactly.
    pub fn from_accepted_execution(
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        binding: &AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        baseline: AppResourcePeriodBaseline,
        accepted_identity: &AppResourceTreeIdentity,
    ) -> Result<Self, AppResourceContractError> {
        Self::from_authority_snapshot(
            authenticated_scope,
            resolved,
            binding,
            policy,
            baseline,
            Some(accepted_identity),
        )
    }

    fn from_authority_snapshot(
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppAuthority,
        binding: &AppRunBinding,
        policy: AppResourceEnforcementPolicy,
        baseline: AppResourcePeriodBaseline,
        accepted_identity: Option<&AppResourceTreeIdentity>,
    ) -> Result<Self, AppResourceContractError> {
        binding
            .validate_app_contract(&AppContractLimits::default())
            .map_err(|error| AppResourceContractError::InvalidBinding(error.to_string()))?;
        policy.validate(&resolved.effective_resources)?;
        let canonical_authority_digest = resolved
            .canonical_authority_digest()
            .map_err(|error| AppResourceContractError::InvalidBinding(error.to_string()))?;
        if canonical_authority_digest != resolved.authority_digest {
            return Err(AppResourceContractError::IdentityMismatch(
                "authority_digest",
            ));
        }

        if &binding.scope != authenticated_scope.scope() {
            return Err(AppResourceContractError::IdentityMismatch("scope"));
        }
        if authenticated_scope.scope_binding_ref() != &resolved.scope_binding_ref
            || authenticated_scope.actor_ref() != &resolved.actor_ref
            || authenticated_scope.session_ref() != &resolved.session_ref
            || authenticated_scope.authentication() != resolved.authentication
            || authenticated_scope.authentication_revision() != resolved.authentication_revision
        {
            return Err(AppResourceContractError::IdentityMismatch(
                "authenticated_session",
            ));
        }
        if binding.installation_id != resolved.installation_id {
            return Err(AppResourceContractError::IdentityMismatch(
                "installation_id",
            ));
        }
        if binding.package_revision_ref != resolved.package_revision_ref {
            return Err(AppResourceContractError::IdentityMismatch(
                "package_revision_ref",
            ));
        }
        if binding.schema_revision != resolved.schema_revision {
            return Err(AppResourceContractError::IdentityMismatch(
                "schema_revision",
            ));
        }
        if binding.authority_digest != resolved.authority_digest {
            return Err(AppResourceContractError::IdentityMismatch(
                "authority_digest",
            ));
        }
        if baseline.scope != binding.scope
            || baseline.installation_id != binding.installation_id
            || baseline.installation_generation != resolved.installation_generation
            || baseline.excluded_budget_ledger_ref != binding.budget_ledger_ref
        {
            return Err(AppResourceContractError::IdentityMismatch(
                "installation_period",
            ));
        }
        if baseline
            .behavior_period
            .as_ref()
            .map(|behavior| &behavior.ledger_dimension_digest)
            != binding
                .behavior_resource_identity
                .as_ref()
                .map(AppResourceBehaviorIdentity::ledger_dimension_digest)
        {
            return Err(AppResourceContractError::IdentityMismatch(
                "behavior_resource_identity",
            ));
        }

        let identity = AppResourceTreeIdentity {
            scope: binding.scope.clone(),
            installation_id: binding.installation_id.clone(),
            installation_generation: resolved.installation_generation,
            package_revision_ref: binding.package_revision_ref.clone(),
            grant_revision: resolved.grant_revision,
            schema_revision: binding.schema_revision,
            authority_digest: binding.authority_digest.clone(),
            behavior_resource_identity: binding.behavior_resource_identity.clone(),
            root_execution_id: binding.execution_id.clone(),
            budget_ledger_ref: binding.budget_ledger_ref.clone(),
            installation_period_ref: baseline.period_ref.clone(),
        };
        if accepted_identity.is_some_and(|accepted| accepted != &identity) {
            return Err(AppResourceContractError::IdentityMismatch(
                "accepted_tree_identity",
            ));
        }

        Ok(Self {
            identity,
            ceiling: resolved.effective_resources.clone(),
            policy,
            baseline,
        })
    }

    pub fn identity(&self) -> &AppResourceTreeIdentity {
        &self.identity
    }

    pub fn period_ref(&self) -> &AppReference {
        &self.baseline.period_ref
    }

    pub fn period_revision(&self) -> AppRevision {
        self.baseline.period_revision
    }
}

fn ensure_current_authority_freshness(
    authenticated_scope: &AuthenticatedAppScope,
    resolved_at: &DateTime<Utc>,
    now: &DateTime<Utc>,
) -> Result<(), AppResourceContractError> {
    authenticated_scope
        .ensure_live_at(now)
        .map_err(|_| AppResourceContractError::IdentityMismatch("authentication_window"))?;
    if resolved_at != now {
        return Err(AppResourceContractError::StaleCurrentAuthority);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppResourceExecutionLane {
    Foreground,
    Background,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppResourceNodeKind {
    Root,
    DelegatedChild,
    Retry,
    Resume,
    Repair,
    ToolCall,
    BrowserOrNetwork,
    Synthesis,
    Reflection,
}

/// Additive resource dimensions. Active time is deliberately absent: it is
/// computed as a union of separately bounded intervals rather than summed.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceQuantity {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub cost_microusd: u64,
    pub paid_tool_invocations: u64,
    pub browser_network_actions: u64,
    pub records: u64,
    pub payload_bytes: u64,
    pub attachment_bytes: u64,
}

impl AppResourceQuantity {
    fn validate(self, field: &'static str) -> Result<(), AppResourceContractError> {
        if self.cached_input_tokens > self.input_tokens {
            return Err(AppResourceContractError::InvalidQuantity {
                field,
                message: "cached input tokens cannot exceed total input tokens",
            });
        }
        Ok(())
    }

    fn checked_add(self, other: Self) -> Result<Self, AppResourceContractError> {
        macro_rules! sum {
            ($field:ident) => {
                self.$field.checked_add(other.$field).ok_or(
                    AppResourceContractError::ArithmeticOverflow(stringify!($field)),
                )?
            };
        }
        Ok(Self {
            input_tokens: sum!(input_tokens),
            cached_input_tokens: sum!(cached_input_tokens),
            output_tokens: sum!(output_tokens),
            cost_microusd: sum!(cost_microusd),
            paid_tool_invocations: sum!(paid_tool_invocations),
            browser_network_actions: sum!(browser_network_actions),
            records: sum!(records),
            payload_bytes: sum!(payload_bytes),
            attachment_bytes: sum!(attachment_bytes),
        })
    }

    pub fn first_component_exceeding(self, upper: Self) -> Option<&'static str> {
        macro_rules! check {
            ($field:ident) => {
                if self.$field > upper.$field {
                    return Some(stringify!($field));
                }
            };
        }
        check!(input_tokens);
        check!(cached_input_tokens);
        check!(output_tokens);
        check!(cost_microusd);
        check!(paid_tool_invocations);
        check!(browser_network_actions);
        check!(records);
        check!(payload_bytes);
        check!(attachment_bytes);
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCapabilityResourceQuantity {
    pub capability_family: AppName,
    pub paid_invocations: u64,
    pub cost_microusd: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppActiveInterval {
    pub start_elapsed_ms: u64,
    pub end_elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppResourceSettlementOutcome {
    /// A provider/tool/store observation is final. Actual usage is charged and
    /// must remain within the exact pre-dispatch reservation upper bound.
    Committed,
    /// Trusted recovery evidence proves the consequential operation never
    /// dispatched. The reservation can be released without a charge.
    ProvenUnspent,
    /// The external outcome cannot yet be proven. The full reservation remains
    /// held and blocks terminal settlement; expiry alone never releases it.
    OutcomeUncertain,
}

/// Domain owners that can supply facts to the canonical normalizer. These are
/// observation sources only; none can authorize a new reservation or
/// independently calculate remaining app budget.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppResourceObservationSource {
    ExecutionTokenMeter,
    LlmTaskLedger,
    CodingTaskActiveTime,
    ToolRuntime,
    BrowserRuntime,
    AppStoreTransaction,
    AttachmentStore,
    PackageStore,
    WorkflowNoEffect,
    CrashReconciler,
}

/// Trusted current evidence that a consequential reservation never
/// dispatched. Durable journal JSON cannot manufacture this value; the crash
/// reconciler must mint it and the assessment consumes it by exact reservation
/// and observation identity.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrustedAppResourceRecoveryEvidence {
    reservation_id: AppReference,
    observation_id: AppReference,
    reconciliation_ref: AppReference,
    reconciliation_revision: AppRevision,
    reconciled_at_elapsed_ms: u64,
}

impl TrustedAppResourceRecoveryEvidence {
    pub fn from_crash_reconciler(
        reservation_id: AppReference,
        observation_id: AppReference,
        reconciliation_ref: AppReference,
        reconciliation_revision: AppRevision,
        reconciled_at_elapsed_ms: u64,
    ) -> Self {
        Self {
            reservation_id,
            observation_id,
            reconciliation_ref,
            reconciliation_revision,
            reconciled_at_elapsed_ms,
        }
    }
}

/// A flat event journal keeps hostile/recovered topology destruction off the
/// native call stack. Phase 4B persists these semantics through its canonical
/// store; this Phase-0 value is only a contract fixture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceJournal {
    pub identity: AppResourceTreeIdentity,
    pub evaluated_at_elapsed_ms: u64,
    pub events: Vec<AppResourceJournalEvent>,
}

impl ValidateAppContract for AppResourceJournal {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.identity.installation_generation == 0 {
            return Err(AppContractError::invalid(
                "resource.identity.installation_generation",
                "must be greater than zero",
            ));
        }
        self.identity.validate_app_contract(_limits)?;
        if self.events.is_empty() || self.events.len() > HARD_MAX_JOURNAL_EVENTS as usize {
            return Err(AppContractError::invalid(
                "resource.events",
                "must contain a root and remain within the hard journal ceiling",
            ));
        }

        let mut interval_count = 0_usize;
        for (index, event) in self.events.iter().enumerate() {
            let expected = (index as u64).checked_add(1).ok_or_else(|| {
                AppContractError::invalid("resource.sequence", "sequence overflow")
            })?;
            if event.sequence() != expected {
                return Err(AppContractError::invalid(
                    "resource.sequence",
                    "must be contiguous and start at one",
                ));
            }
            if event.at_elapsed_ms() > self.evaluated_at_elapsed_ms {
                return Err(AppContractError::invalid(
                    "resource.event.at_elapsed_ms",
                    "cannot be after evaluated_at_elapsed_ms",
                ));
            }
            match event {
                AppResourceJournalEvent::Reserved {
                    requested,
                    capability_requests,
                    ..
                } => {
                    requested
                        .validate("resource.reservation.requested")
                        .map_err(|error| {
                            AppContractError::invalid(
                                "resource.reservation.requested",
                                error.to_string(),
                            )
                        })?;
                    if capability_requests.len() > HARD_MAX_CAPABILITY_FAMILIES as usize {
                        return Err(AppContractError::invalid(
                            "resource.reservation.capability_requests",
                            "exceeds the hard capability-family ceiling",
                        ));
                    }
                },
                AppResourceJournalEvent::Settled {
                    outcome,
                    actual,
                    observation_sources,
                    capability_usage,
                    active_intervals,
                    effect_binding_digest,
                    effect_result,
                    ..
                } => {
                    validate_observation_sources(observation_sources).map_err(|error| {
                        AppContractError::invalid(
                            "resource.settlement.observation_sources",
                            error.to_string(),
                        )
                    })?;
                    validate_settlement_evidence(*outcome, observation_sources).map_err(
                        |error| {
                            AppContractError::invalid(
                                "resource.settlement.observation_sources",
                                error.to_string(),
                            )
                        },
                    )?;
                    if effect_binding_digest.is_some()
                        && !valid_effect_settlement_owner(
                            *outcome,
                            observation_sources,
                            effect_result.is_some(),
                        )
                    {
                        return Err(AppContractError::invalid(
                            "resource.settlement.effect_binding_digest",
                            "requires a physical observation owner or committed crash cleanup without a result",
                        ));
                    }
                    if effect_result.as_ref().is_some_and(|result| {
                        effect_binding_digest.is_none()
                            || *outcome != AppResourceSettlementOutcome::Committed
                            || result.result_bytes > 16 * 1024 * 1024
                    }) {
                        return Err(AppContractError::invalid(
                            "resource.settlement.effect_result",
                            "requires a bounded committed effect settlement",
                        ));
                    }
                    actual
                        .validate("resource.settlement.actual")
                        .map_err(|error| {
                            AppContractError::invalid(
                                "resource.settlement.actual",
                                error.to_string(),
                            )
                        })?;
                    if capability_usage.len() > HARD_MAX_CAPABILITY_FAMILIES as usize {
                        return Err(AppContractError::invalid(
                            "resource.settlement.capability_usage",
                            "exceeds the hard capability-family ceiling",
                        ));
                    }
                    interval_count = interval_count
                        .checked_add(active_intervals.len())
                        .ok_or_else(|| {
                            AppContractError::invalid(
                                "resource.active_intervals",
                                "interval count overflow",
                            )
                        })?;
                    if interval_count > HARD_MAX_ACTIVE_INTERVALS as usize {
                        return Err(AppContractError::invalid(
                            "resource.active_intervals",
                            "exceeds the hard active-interval ceiling",
                        ));
                    }
                    if active_intervals.iter().any(|interval| {
                        interval.start_elapsed_ms >= interval.end_elapsed_ms
                            || interval.end_elapsed_ms > event.at_elapsed_ms()
                    }) {
                        return Err(AppContractError::invalid(
                            "resource.active_intervals",
                            "must be non-empty and end no later than their observation",
                        ));
                    }
                },
                _ => {},
            }
        }
        Ok(())
    }
}

/// Hostile/corrupt stored journals pass the shared iterative JSON byte, depth
/// and node preflight before Serde constructs the flat value. Current authority
/// and lower policy ceilings are still required separately during assessment.
pub fn decode_app_resource_journal(bytes: &[u8]) -> Result<AppResourceJournal, AppContractError> {
    decode_app_contract(bytes, &AppContractLimits::default())
}

/// Bounded raw provider-result identity retained by the canonical resource
/// settlement. Content bytes remain with the disclosure/retention owner; this
/// digest + exact encoded length is sufficient to bind replay after a crash
/// between resource settlement and the later labeled-result checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceEffectResult {
    pub result_digest: AppDigest,
    pub result_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppEffectDispatchAbortReason {
    Cancelled,
    AdmissionExpired,
    RevalidationFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppResourceJournalEvent {
    NodeOpened {
        sequence: u64,
        node_id: AppReference,
        execution_ref: AppReference,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_node_id: Option<AppReference>,
        node_kind: AppResourceNodeKind,
        lane: AppResourceExecutionLane,
        at_elapsed_ms: u64,
    },
    ProgressObserved {
        sequence: u64,
        node_id: AppReference,
        progress_id: AppReference,
        at_elapsed_ms: u64,
    },
    Reserved {
        sequence: u64,
        node_id: AppReference,
        reservation_id: AppReference,
        operation_key: AppReference,
        requested: AppResourceQuantity,
        capability_requests: Vec<AppCapabilityResourceQuantity>,
        at_elapsed_ms: u64,
        expires_at_elapsed_ms: u64,
    },
    /// Durable boundary proving that an exact effect identity crossed its
    /// final pre-I/O fences. A crash after this event can no longer release
    /// the reservation as proven-unspent; recovery must settle it
    /// conservatively as post-I/O/uncertain work. The binding digest is an
    /// opaque correlation key to this resource contract: journal replay only
    /// compares it for byte-exact equality and never reconstructs or assigns
    /// effect-binding schema semantics to it. Schema-aware re-attribution is
    /// owned by the sealed completion intent, which rejects legacy V1 rows.
    ///
    /// # This event must never carry a dispatch attempt id
    ///
    /// It is the obvious place to put one — the loop's `effect_id` names the
    /// attempt, and joining it here would let an operator ask "did attempt X
    /// fire" straight against the journal. It would also break the rail.
    ///
    /// The retry in `workflows.rs` re-issues this event with *byte-identical*
    /// material and treats an `AlreadyPresent` receipt as proof the start
    /// happened. That proof only works because the material is attempt-
    /// INDEPENDENT: two attempts at the same bound effect produce the same
    /// event, so the second recognises the first. An attempt id in the row
    /// would make them differ, and a deliberate retry — the model re-deciding,
    /// same app action, same inputs — would stop matching and either conflict
    /// or append a second start against one reservation. That is precisely the
    /// crash-safety property this event exists to provide.
    ///
    /// So the identity here stays the binding digest, and the join to an
    /// attempt is the loop's to hold: it knows both values at the checkpoint it
    /// already reads. See `docs/components/magician/effect-identity.md`.
    EffectDispatchStarted {
        sequence: u64,
        node_id: AppReference,
        reservation_id: AppReference,
        effect_binding_digest: AppDigest,
        at_elapsed_ms: u64,
    },
    /// A typed local owner proved that no provider poll occurred after a
    /// successful dispatch-start checkpoint. This is the only post-start path
    /// that can release the reservation as proven-unspent.
    EffectDispatchAbortedBeforeIo {
        sequence: u64,
        node_id: AppReference,
        reservation_id: AppReference,
        effect_binding_digest: AppDigest,
        reason: AppEffectDispatchAbortReason,
        at_elapsed_ms: u64,
    },
    Settled {
        sequence: u64,
        node_id: AppReference,
        reservation_id: AppReference,
        observation_id: AppReference,
        outcome: AppResourceSettlementOutcome,
        observation_sources: Vec<AppResourceObservationSource>,
        actual: AppResourceQuantity,
        capability_usage: Vec<AppCapabilityResourceQuantity>,
        active_intervals: Vec<AppActiveInterval>,
        /// Common Apps effect identity when this settlement crossed the
        /// primitive/action/physical-target permit. Older and non-tool
        /// resource events remain readable without it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_binding_digest: Option<AppDigest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_result: Option<AppResourceEffectResult>,
        at_elapsed_ms: u64,
    },
    NodeClosed {
        sequence: u64,
        node_id: AppReference,
        at_elapsed_ms: u64,
    },
}

impl AppResourceJournalEvent {
    fn sequence(&self) -> u64 {
        match self {
            Self::NodeOpened { sequence, .. }
            | Self::ProgressObserved { sequence, .. }
            | Self::Reserved { sequence, .. }
            | Self::EffectDispatchStarted { sequence, .. }
            | Self::EffectDispatchAbortedBeforeIo { sequence, .. }
            | Self::Settled { sequence, .. }
            | Self::NodeClosed { sequence, .. } => *sequence,
        }
    }

    fn at_elapsed_ms(&self) -> u64 {
        match self {
            Self::NodeOpened { at_elapsed_ms, .. }
            | Self::ProgressObserved { at_elapsed_ms, .. }
            | Self::Reserved { at_elapsed_ms, .. }
            | Self::EffectDispatchStarted { at_elapsed_ms, .. }
            | Self::EffectDispatchAbortedBeforeIo { at_elapsed_ms, .. }
            | Self::Settled { at_elapsed_ms, .. }
            | Self::NodeClosed { at_elapsed_ms, .. } => *at_elapsed_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppResourceBreach {
    InputTokens,
    OutputTokens,
    Cost,
    MonthlyCost,
    MonthlyTokens,
    PaidToolInvocations,
    ActiveTime,
    Lifetime,
    BrowserNetworkActions,
    Records,
    PayloadBytes,
    AttachmentBytes,
    PackageBytes,
    InstallationPeriod,
    NoProgress,
    UnreconciledExpiredReservation,
    OutcomeUncertain,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceAssessment {
    identity: AppResourceTreeIdentity,
    period_revision: AppRevision,
    committed: AppResourceQuantity,
    outstanding_reserved: AppResourceQuantity,
    capability_usage: BTreeMap<AppName, AppCapabilityResourceQuantity>,
    active_elapsed_ms: u64,
    admitted_active_elapsed_ms: u64,
    lifetime_elapsed_ms: u64,
    evaluated_at_elapsed_ms: u64,
    last_progress_elapsed_ms: u64,
    latest_held_reservation_expiry_elapsed_ms: Option<u64>,
    open_nodes: u32,
    held_reservations: u32,
    uncertain_reservations: u32,
    root_closed: bool,
    breaches: BTreeSet<AppResourceBreach>,
}

impl AppResourceAssessment {
    pub fn terminally_settled(&self) -> bool {
        self.root_closed
            && self.open_nodes == 0
            && self.held_reservations == 0
            && self.uncertain_reservations == 0
    }

    pub fn committed(&self) -> AppResourceQuantity {
        self.committed
    }

    pub fn outstanding_reserved(&self) -> AppResourceQuantity {
        self.outstanding_reserved
    }

    pub fn active_elapsed_ms(&self) -> u64 {
        self.active_elapsed_ms
    }

    pub fn admitted_active_elapsed_ms(&self) -> u64 {
        self.admitted_active_elapsed_ms
    }

    pub fn lifetime_elapsed_ms(&self) -> u64 {
        self.lifetime_elapsed_ms
    }

    pub fn breaches(&self) -> &BTreeSet<AppResourceBreach> {
        &self.breaches
    }

    pub fn period_revision(&self) -> AppRevision {
        self.period_revision
    }

    pub fn evaluated_at_elapsed_ms(&self) -> u64 {
        self.evaluated_at_elapsed_ms
    }

    pub fn last_progress_elapsed_ms(&self) -> u64 {
        self.last_progress_elapsed_ms
    }

    pub fn has_live_held_reservation_at(&self, at_elapsed_ms: u64) -> bool {
        self.latest_held_reservation_expiry_elapsed_ms
            .is_some_and(|expiry| expiry > at_elapsed_ms)
    }

    pub fn open_nodes(&self) -> u32 {
        self.open_nodes
    }

    pub fn held_reservations(&self) -> u32 {
        self.held_reservations
    }

    pub fn uncertain_reservations(&self) -> u32 {
        self.uncertain_reservations
    }

    pub fn root_closed(&self) -> bool {
        self.root_closed
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppResourceContractError {
    #[error("invalid app resource enforcement policy: {0}")]
    InvalidPolicy(&'static str),
    #[error("invalid app run binding: {0}")]
    InvalidBinding(String),
    #[error("app resource identity mismatch: {0}")]
    IdentityMismatch(&'static str),
    #[error("app resource authority was not resolved at the current admission boundary")]
    StaleCurrentAuthority,
    #[error("app resource installation-period snapshot predates the assessed tree")]
    StalePeriodRevision,
    #[error("app resource journal exceeds the {limit} event ceiling")]
    EventLimitExceeded { limit: u32 },
    #[error("app resource journal sequence must be contiguous at {expected}, got {actual}")]
    InvalidSequence { expected: u64, actual: u64 },
    #[error("app resource event occurs outside the evaluated lifetime")]
    EventAfterEvaluation,
    #[error("app resource event time regresses relative to journal order")]
    EventTimeRegression,
    #[error("app resource tree must open exactly one root first")]
    RootInvariant,
    #[error("app resource root is already closed")]
    RootAlreadyClosed,
    #[error("duplicate app resource node `{0}`")]
    DuplicateNode(String),
    #[error("duplicate app resource execution reference `{0}`")]
    DuplicateExecutionRef(String),
    #[error("unknown app resource node `{0}`")]
    UnknownNode(String),
    #[error("app resource node `{0}` has an invalid or closed parent")]
    InvalidParent(String),
    #[error("app resource tree exceeds the {limit} node ceiling")]
    NodeLimitExceeded { limit: u32 },
    #[error("app resource tree exceeds the {limit} level depth ceiling")]
    TreeDepthExceeded { limit: u16 },
    #[error("app resource child lane cannot differ from its root lane")]
    LaneMismatch,
    #[error("app resource node `{0}` is already closed")]
    NodeClosed(String),
    #[error("app resource node `{0}` still has open children")]
    NodeHasOpenChildren(String),
    #[error("app resource event predates its node")]
    EventBeforeNodeOpen,
    #[error("duplicate app resource progress identity `{0}`")]
    DuplicateProgress(String),
    #[error("app resource reservation exceeds the {limit} reservation ceiling")]
    ReservationLimitExceeded { limit: u32 },
    #[error("duplicate app resource reservation identity `{0}`")]
    DuplicateReservation(String),
    #[error("app resource operation key `{0}` was rebound")]
    OperationKeyRebound(String),
    #[error("unknown app resource reservation `{0}`")]
    UnknownReservation(String),
    #[error("app resource reservation belongs to another node")]
    ReservationNodeMismatch,
    #[error("app resource reservation has an invalid expiry")]
    InvalidReservationExpiry,
    #[error("app resource reservation was already terminally settled")]
    ReservationAlreadySettled,
    #[error("app resource effect dispatch identity is duplicated or mismatched")]
    EffectDispatchIdentityMismatch,
    #[error("app resource settlement exceeds its reservation for `{field}`")]
    SettlementExceedsReservation { field: &'static str },
    #[error("app resource settlement exceeds its reservation for capability family `{0}`")]
    CapabilitySettlementExceedsReservation(String),
    #[error("app resource observation identity `{0}` was rebound")]
    ObservationRebound(String),
    #[error("app resource settlement must name a bounded unique observation-source set")]
    InvalidObservationSources,
    #[error("proven-unspent settlement requires exclusive crash-reconciler evidence")]
    UntrustedProvenUnspentEvidence,
    #[error("proven-unspent settlement is missing trusted current recovery evidence")]
    MissingTrustedRecoveryEvidence,
    #[error("trusted app resource recovery evidence is duplicated, stale, extra or mismatched")]
    InvalidRecoveryEvidence,
    #[error("invalid app resource quantity `{field}`: {message}")]
    InvalidQuantity {
        field: &'static str,
        message: &'static str,
    },
    #[error("app resource arithmetic overflow in {0}")]
    ArithmeticOverflow(&'static str),
    #[error("app resource capability families exceed the {limit} family ceiling")]
    CapabilityFamilyLimitExceeded { limit: u16 },
    #[error("app resource capability family `{0}` is duplicated")]
    DuplicateCapabilityFamily(String),
    #[error("app resource capability families must use canonical ascending order")]
    NonCanonicalCapabilityOrder,
    #[error("app resource capability totals do not match the aggregate quantity")]
    CapabilityTotalMismatch,
    #[error("app active interval is empty, reversed, or outside its reservation/node lifetime")]
    InvalidActiveInterval,
    #[error("app active intervals exceed the {limit} interval ceiling")]
    ActiveIntervalLimitExceeded { limit: u32 },
    #[error("uncertain or proven-unspent settlement cannot claim actual usage")]
    NonCommittedUsage,
    #[error("app resource reservation would exceed a hard ceiling before dispatch: {0:?}")]
    ReservationDenied(AppResourceBreach),
    #[error("app foreground admission exceeds installation concurrency")]
    ForegroundConcurrencyDenied,
    #[error("app background admission exceeds installation concurrency")]
    BackgroundConcurrencyDenied,
    #[error("app background admission would consume the scheduler foreground reserve")]
    ForegroundReserveDenied,
    #[error("app foreground admission exceeds scheduler capacity")]
    SchedulerCapacityDenied,
    #[error("app background admission exceeds its period start ceiling")]
    BackgroundFrequencyDenied,
}

#[derive(Debug, Clone)]
struct NodeState {
    depth: u16,
    open: bool,
    parent_node_id: Option<AppReference>,
    open_children: u32,
    opened_at_elapsed_ms: u64,
    closed_at_elapsed_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReservationStatus {
    Held,
    Uncertain,
    Settled,
    Released,
}

#[derive(Debug, Clone)]
struct ReservationState {
    node_id: AppReference,
    operation_key: AppReference,
    requested: AppResourceQuantity,
    capability_requests: Vec<AppCapabilityResourceQuantity>,
    created_at_elapsed_ms: u64,
    effect_binding_digest: Option<AppDigest>,
    status: ReservationStatus,
    expires_at_elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservationFingerprint {
    node_id: AppReference,
    reservation_id: AppReference,
    outcome: AppResourceSettlementOutcome,
    observation_sources: Vec<AppResourceObservationSource>,
    actual: AppResourceQuantity,
    capability_usage: Vec<AppCapabilityResourceQuantity>,
    active_intervals: Vec<AppActiveInterval>,
    effect_binding_digest: Option<AppDigest>,
    effect_result: Option<AppResourceEffectResult>,
    at_elapsed_ms: u64,
}

/// Incremental disjoint interval set. Insertions are iterative and merge only
/// adjacent/overlapping ranges, so replay does not repeatedly sort all prior
/// observations or recurse through an execution tree.
#[derive(Debug, Clone, Default)]
struct ActiveIntervalUnion {
    ranges: BTreeMap<u64, u64>,
    total_millis: u64,
    observed_count: usize,
}

impl ActiveIntervalUnion {
    fn insert(&mut self, interval: AppActiveInterval) -> Result<(), AppResourceContractError> {
        self.observed_count = self.observed_count.checked_add(1).ok_or(
            AppResourceContractError::ArithmeticOverflow("active_interval_count"),
        )?;
        let mut start = interval.start_elapsed_ms;
        let mut end = interval.end_elapsed_ms;

        if let Some((&previous_start, &previous_end)) = self.ranges.range(..=start).next_back() {
            if previous_end >= start {
                self.ranges.remove(&previous_start);
                self.total_millis = self
                    .total_millis
                    .checked_sub(previous_end - previous_start)
                    .ok_or(AppResourceContractError::ArithmeticOverflow(
                        "active_elapsed_ms",
                    ))?;
                start = previous_start;
                end = end.max(previous_end);
            }
        }

        loop {
            let Some((&next_start, &next_end)) = self.ranges.range(start..).next() else {
                break;
            };
            if next_start > end {
                break;
            }
            self.ranges.remove(&next_start);
            self.total_millis = self.total_millis.checked_sub(next_end - next_start).ok_or(
                AppResourceContractError::ArithmeticOverflow("active_elapsed_ms"),
            )?;
            end = end.max(next_end);
        }

        self.ranges.insert(start, end);
        self.total_millis = self.total_millis.checked_add(end - start).ok_or(
            AppResourceContractError::ArithmeticOverflow("active_elapsed_ms"),
        )?;
        Ok(())
    }
}

/// Replay a bounded flat journal under freshly resolved current authority.
///
/// Invalid pre-dispatch reservations fail closed. A committed observation must
/// remain componentwise within its reservation, because the reservation is the
/// hard upper bound accepted before consequential I/O. Providers must reserve
/// a conservative maximum and settle the exact observed usage, releasing the
/// unused remainder.
pub fn assess_app_resource_journal(
    journal: &AppResourceJournal,
    current: &CurrentAppResourceAuthority,
) -> Result<AppResourceAssessment, AppResourceContractError> {
    assess_app_resource_journal_with_recovery(journal, current, &[])
}

pub fn assess_app_resource_journal_with_recovery(
    journal: &AppResourceJournal,
    current: &CurrentAppResourceAuthority,
    recovery_evidence: &[&TrustedAppResourceRecoveryEvidence],
) -> Result<AppResourceAssessment, AppResourceContractError> {
    ensure_identity_matches(&journal.identity, &current.identity)?;
    current.policy.validate(&current.ceiling)?;
    if journal.events.len() > current.policy.max_journal_events as usize {
        return Err(AppResourceContractError::EventLimitExceeded {
            limit: current.policy.max_journal_events,
        });
    }
    validate_static_root_context(current)?;
    if recovery_evidence.len() > journal.events.len() {
        return Err(AppResourceContractError::InvalidRecoveryEvidence);
    }
    let mut recovery_by_observation = BTreeMap::new();
    let mut recovery_refs = BTreeSet::new();
    for evidence in recovery_evidence {
        if evidence.reconciled_at_elapsed_ms > journal.evaluated_at_elapsed_ms
            || !recovery_refs.insert(evidence.reconciliation_ref.clone())
            || recovery_by_observation
                .insert(evidence.observation_id.clone(), *evidence)
                .is_some()
        {
            return Err(AppResourceContractError::InvalidRecoveryEvidence);
        }
    }

    let mut nodes = BTreeMap::<AppReference, NodeState>::new();
    let mut execution_refs = BTreeSet::<AppReference>::new();
    let mut reservations = BTreeMap::<AppReference, ReservationState>::new();
    let mut operation_keys = BTreeMap::<AppReference, AppReference>::new();
    let mut observations = BTreeMap::<AppReference, ObservationFingerprint>::new();
    let mut progress_ids = BTreeMap::<AppReference, (AppReference, u64)>::new();
    let mut held_reservation_expiries = BTreeMap::<u64, u32>::new();
    let mut capability_usage = BTreeMap::<AppName, AppCapabilityResourceQuantity>::new();
    let mut active_intervals = ActiveIntervalUnion::default();
    let mut committed = AppResourceQuantity::default();
    let mut outstanding = AppResourceQuantity::default();
    let mut last_progress_ms = 0_u64;
    let mut root_lane = None;
    let mut root_closed = false;
    let mut root_closed_at_elapsed_ms = None;
    let mut last_event_elapsed_ms = 0_u64;

    for (index, event) in journal.events.iter().enumerate() {
        let expected =
            (index as u64)
                .checked_add(1)
                .ok_or(AppResourceContractError::ArithmeticOverflow(
                    "journal_sequence",
                ))?;
        if event.sequence() != expected {
            return Err(AppResourceContractError::InvalidSequence {
                expected,
                actual: event.sequence(),
            });
        }
        if event.at_elapsed_ms() > journal.evaluated_at_elapsed_ms {
            return Err(AppResourceContractError::EventAfterEvaluation);
        }
        match event {
            AppResourceJournalEvent::NodeOpened {
                node_id,
                execution_ref,
                parent_node_id,
                node_kind,
                lane,
                at_elapsed_ms,
                ..
            } => {
                ensure_monotonic_event_time(*at_elapsed_ms, &mut last_event_elapsed_ms)?;
                ensure_no_progress_before_work_expanding_event(
                    *at_elapsed_ms,
                    last_progress_ms,
                    has_live_held_reservation_at(&held_reservation_expiries, *at_elapsed_ms),
                    current,
                )?;
                if nodes.len() >= current.policy.max_tree_nodes as usize {
                    return Err(AppResourceContractError::NodeLimitExceeded {
                        limit: current.policy.max_tree_nodes,
                    });
                }
                if nodes.contains_key(node_id) {
                    return Err(AppResourceContractError::DuplicateNode(node_id.to_string()));
                }
                if !execution_refs.insert(execution_ref.clone()) {
                    return Err(AppResourceContractError::DuplicateExecutionRef(
                        execution_ref.to_string(),
                    ));
                }

                let (depth, parent_node_id) = if nodes.is_empty() {
                    if *node_kind != AppResourceNodeKind::Root
                        || parent_node_id.is_some()
                        || *at_elapsed_ms != 0
                        || execution_ref != &journal.identity.root_execution_id
                    {
                        return Err(AppResourceContractError::RootInvariant);
                    }
                    root_lane = Some(*lane);
                    (0, None)
                } else {
                    if *node_kind == AppResourceNodeKind::Root {
                        return Err(AppResourceContractError::RootInvariant);
                    }
                    if Some(*lane) != root_lane {
                        return Err(AppResourceContractError::LaneMismatch);
                    }
                    let parent_id = parent_node_id
                        .as_ref()
                        .ok_or(AppResourceContractError::RootInvariant)?;
                    let parent = nodes.get(parent_id).ok_or_else(|| {
                        AppResourceContractError::InvalidParent(parent_id.to_string())
                    })?;
                    if !parent.open {
                        return Err(AppResourceContractError::InvalidParent(
                            parent_id.to_string(),
                        ));
                    }
                    if *at_elapsed_ms < parent.opened_at_elapsed_ms {
                        return Err(AppResourceContractError::EventBeforeNodeOpen);
                    }
                    let depth = parent.depth.checked_add(1).ok_or(
                        AppResourceContractError::TreeDepthExceeded {
                            limit: current.policy.max_tree_depth,
                        },
                    )?;
                    (depth, Some(parent_id.clone()))
                };
                if depth > current.policy.max_tree_depth {
                    return Err(AppResourceContractError::TreeDepthExceeded {
                        limit: current.policy.max_tree_depth,
                    });
                }
                if let Some(parent_id) = &parent_node_id {
                    let parent = nodes.get_mut(parent_id).ok_or_else(|| {
                        AppResourceContractError::InvalidParent(parent_id.to_string())
                    })?;
                    parent.open_children = parent.open_children.checked_add(1).ok_or(
                        AppResourceContractError::ArithmeticOverflow("open_children"),
                    )?;
                }
                nodes.insert(
                    node_id.clone(),
                    NodeState {
                        depth,
                        open: true,
                        parent_node_id,
                        open_children: 0,
                        opened_at_elapsed_ms: *at_elapsed_ms,
                        closed_at_elapsed_ms: None,
                    },
                );
            },
            AppResourceJournalEvent::ProgressObserved {
                node_id,
                progress_id,
                at_elapsed_ms,
                ..
            } => {
                if let Some((existing_node, existing_at)) = progress_ids.get(progress_id) {
                    if existing_node == node_id && existing_at == at_elapsed_ms {
                        continue;
                    }
                    return Err(AppResourceContractError::DuplicateProgress(
                        progress_id.to_string(),
                    ));
                }
                ensure_monotonic_event_time(*at_elapsed_ms, &mut last_event_elapsed_ms)?;
                let node = require_open_node(&nodes, node_id)?;
                if *at_elapsed_ms < node.opened_at_elapsed_ms {
                    return Err(AppResourceContractError::EventBeforeNodeOpen);
                }
                ensure_no_progress_before_work_expanding_event(
                    *at_elapsed_ms,
                    last_progress_ms,
                    has_live_held_reservation_at(&held_reservation_expiries, *at_elapsed_ms),
                    current,
                )?;
                progress_ids.insert(progress_id.clone(), (node_id.clone(), *at_elapsed_ms));
                last_progress_ms = last_progress_ms.max(*at_elapsed_ms);
            },
            AppResourceJournalEvent::Reserved {
                node_id,
                reservation_id,
                operation_key,
                requested,
                capability_requests,
                at_elapsed_ms,
                expires_at_elapsed_ms,
                ..
            } => {
                if *expires_at_elapsed_ms <= *at_elapsed_ms
                    || *expires_at_elapsed_ms
                        > seconds_to_millis(current.ceiling.max_lifetime_seconds)?
                    || *expires_at_elapsed_ms > current.baseline.period_ends_at_elapsed_ms
                {
                    return Err(AppResourceContractError::InvalidReservationExpiry);
                }
                requested.validate("reservation.requested")?;
                validate_capability_breakdown(
                    capability_requests,
                    *requested,
                    current.policy.max_capability_families,
                )?;
                if let Some(existing) = reservations.get(reservation_id) {
                    if existing.node_id == *node_id
                        && existing.operation_key == *operation_key
                        && existing.requested == *requested
                        && existing.capability_requests == *capability_requests
                        && existing.created_at_elapsed_ms == *at_elapsed_ms
                        && existing.expires_at_elapsed_ms == *expires_at_elapsed_ms
                    {
                        continue;
                    }
                    return Err(AppResourceContractError::DuplicateReservation(
                        reservation_id.to_string(),
                    ));
                }
                ensure_monotonic_event_time(*at_elapsed_ms, &mut last_event_elapsed_ms)?;
                let node = require_open_node(&nodes, node_id)?;
                if *at_elapsed_ms < node.opened_at_elapsed_ms {
                    return Err(AppResourceContractError::EventBeforeNodeOpen);
                }
                if reservations.len() >= current.policy.max_reservations as usize {
                    return Err(AppResourceContractError::ReservationLimitExceeded {
                        limit: current.policy.max_reservations,
                    });
                }
                if let Some(bound) = operation_keys.get(operation_key) {
                    if bound != reservation_id {
                        return Err(AppResourceContractError::OperationKeyRebound(
                            operation_key.to_string(),
                        ));
                    }
                }
                let candidate_outstanding = outstanding.checked_add(*requested)?;
                let candidate = committed.checked_add(candidate_outstanding)?;
                let mut admitted_active = active_intervals.clone();
                for reservation in reservations.values().filter(|reservation| {
                    matches!(
                        reservation.status,
                        ReservationStatus::Held | ReservationStatus::Uncertain
                    )
                }) {
                    admitted_active.insert(AppActiveInterval {
                        start_elapsed_ms: reservation.created_at_elapsed_ms,
                        end_elapsed_ms: reservation.expires_at_elapsed_ms,
                    })?;
                }
                admitted_active.insert(AppActiveInterval {
                    start_elapsed_ms: *at_elapsed_ms,
                    end_elapsed_ms: *expires_at_elapsed_ms,
                })?;
                let has_live_held_reservation =
                    has_live_held_reservation_at(&held_reservation_expiries, *at_elapsed_ms);
                ensure_within_tree_dispatch_ceilings(
                    candidate,
                    admitted_active.total_millis,
                    *at_elapsed_ms,
                    last_progress_ms,
                    has_live_held_reservation,
                    current,
                )?;
                outstanding = candidate_outstanding;
                add_held_reservation_expiry(
                    &mut held_reservation_expiries,
                    *expires_at_elapsed_ms,
                )?;
                operation_keys.insert(operation_key.clone(), reservation_id.clone());
                reservations.insert(
                    reservation_id.clone(),
                    ReservationState {
                        node_id: node_id.clone(),
                        operation_key: operation_key.clone(),
                        requested: *requested,
                        capability_requests: capability_requests.clone(),
                        created_at_elapsed_ms: *at_elapsed_ms,
                        effect_binding_digest: None,
                        status: ReservationStatus::Held,
                        expires_at_elapsed_ms: *expires_at_elapsed_ms,
                    },
                );
            },
            AppResourceJournalEvent::EffectDispatchStarted {
                node_id,
                reservation_id,
                effect_binding_digest,
                at_elapsed_ms,
                ..
            } => {
                ensure_monotonic_event_time(*at_elapsed_ms, &mut last_event_elapsed_ms)?;
                let node = require_open_node(&nodes, node_id)?;
                if *at_elapsed_ms < node.opened_at_elapsed_ms {
                    return Err(AppResourceContractError::EventBeforeNodeOpen);
                }
                let reservation = reservations.get_mut(reservation_id).ok_or_else(|| {
                    AppResourceContractError::UnknownReservation(reservation_id.to_string())
                })?;
                if reservation.node_id != *node_id {
                    return Err(AppResourceContractError::ReservationNodeMismatch);
                }
                if reservation.status != ReservationStatus::Held
                    || reservation.effect_binding_digest.is_some()
                {
                    return Err(AppResourceContractError::EffectDispatchIdentityMismatch);
                }
                remove_held_reservation_expiry(
                    &mut held_reservation_expiries,
                    reservation.expires_at_elapsed_ms,
                )?;
                reservation.effect_binding_digest = Some(effect_binding_digest.clone());
                // Crossing the durable dispatch-start edge makes provider
                // outcome uncertain until the exact matching settlement is
                // appended. A crash therefore cannot leave this looking like
                // an ordinary live Held reservation.
                reservation.status = ReservationStatus::Uncertain;
            },
            AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
                node_id,
                reservation_id,
                effect_binding_digest,
                at_elapsed_ms,
                ..
            } => {
                ensure_monotonic_event_time(*at_elapsed_ms, &mut last_event_elapsed_ms)?;
                let node = require_open_node(&nodes, node_id)?;
                if *at_elapsed_ms < node.opened_at_elapsed_ms {
                    return Err(AppResourceContractError::EventBeforeNodeOpen);
                }
                let reservation = reservations.get_mut(reservation_id).ok_or_else(|| {
                    AppResourceContractError::UnknownReservation(reservation_id.to_string())
                })?;
                if reservation.node_id != *node_id
                    || reservation.status != ReservationStatus::Uncertain
                    || reservation.effect_binding_digest.as_ref() != Some(effect_binding_digest)
                {
                    return Err(AppResourceContractError::EffectDispatchIdentityMismatch);
                }
                outstanding = checked_sub(outstanding, reservation.requested)?;
                reservation.status = ReservationStatus::Released;
            },
            AppResourceJournalEvent::Settled {
                node_id,
                reservation_id,
                observation_id,
                outcome,
                observation_sources,
                actual,
                capability_usage: observed_capabilities,
                active_intervals: observed_intervals,
                effect_binding_digest,
                effect_result,
                at_elapsed_ms,
                ..
            } => {
                validate_observation_sources(observation_sources)?;
                validate_settlement_evidence(*outcome, observation_sources)?;
                actual.validate("settlement.actual")?;
                validate_capability_breakdown(
                    observed_capabilities,
                    *actual,
                    current.policy.max_capability_families,
                )?;
                if *outcome != AppResourceSettlementOutcome::Committed
                    && (*actual != AppResourceQuantity::default()
                        || !observed_capabilities.is_empty()
                        || !observed_intervals.is_empty())
                {
                    return Err(AppResourceContractError::NonCommittedUsage);
                }
                validate_active_interval_batch_size(
                    observed_intervals.len(),
                    current.policy.max_active_intervals,
                )?;
                if (effect_binding_digest.is_some()
                    && !valid_effect_settlement_owner(
                        *outcome,
                        observation_sources,
                        effect_result.is_some(),
                    ))
                    || effect_result.as_ref().is_some_and(|result| {
                        *outcome != AppResourceSettlementOutcome::Committed
                            || effect_binding_digest.is_none()
                            || result.result_bytes > 16 * 1024 * 1024
                    })
                {
                    return Err(AppResourceContractError::EffectDispatchIdentityMismatch);
                }

                let fingerprint = ObservationFingerprint {
                    node_id: node_id.clone(),
                    reservation_id: reservation_id.clone(),
                    outcome: *outcome,
                    observation_sources: observation_sources.clone(),
                    actual: *actual,
                    capability_usage: observed_capabilities.clone(),
                    active_intervals: observed_intervals.clone(),
                    effect_binding_digest: effect_binding_digest.clone(),
                    effect_result: effect_result.clone(),
                    at_elapsed_ms: *at_elapsed_ms,
                };
                if let Some(existing) = observations.get(observation_id) {
                    if existing == &fingerprint {
                        continue;
                    }
                    return Err(AppResourceContractError::ObservationRebound(
                        observation_id.to_string(),
                    ));
                }
                ensure_monotonic_event_time(*at_elapsed_ms, &mut last_event_elapsed_ms)?;
                let node = require_node(&nodes, node_id)?;
                if *at_elapsed_ms < node.opened_at_elapsed_ms {
                    return Err(AppResourceContractError::EventBeforeNodeOpen);
                }
                let reservation = reservations.get(reservation_id).ok_or_else(|| {
                    AppResourceContractError::UnknownReservation(reservation_id.to_string())
                })?;
                if reservation.node_id != *node_id {
                    return Err(AppResourceContractError::ReservationNodeMismatch);
                }
                if matches!(
                    reservation.status,
                    ReservationStatus::Settled | ReservationStatus::Released
                ) {
                    return Err(AppResourceContractError::ReservationAlreadySettled);
                }
                if reservation.effect_binding_digest.as_ref() != effect_binding_digest.as_ref()
                    || (*outcome == AppResourceSettlementOutcome::ProvenUnspent
                        && reservation.effect_binding_digest.is_some())
                {
                    return Err(AppResourceContractError::EffectDispatchIdentityMismatch);
                }
                if *outcome == AppResourceSettlementOutcome::Committed {
                    ensure_settlement_within_reservation(
                        *actual,
                        observed_capabilities,
                        reservation.requested,
                        &reservation.capability_requests,
                    )?;
                }
                let reservation_was_held = reservation.status == ReservationStatus::Held;
                validate_active_intervals(
                    observed_intervals,
                    node.opened_at_elapsed_ms
                        .max(reservation.created_at_elapsed_ms),
                    node.closed_at_elapsed_ms
                        .unwrap_or(journal.evaluated_at_elapsed_ms)
                        .min(*at_elapsed_ms)
                        .min(reservation.expires_at_elapsed_ms),
                    active_intervals.observed_count,
                    current.policy.max_active_intervals,
                )?;

                if reservation_was_held {
                    remove_held_reservation_expiry(
                        &mut held_reservation_expiries,
                        reservation.expires_at_elapsed_ms,
                    )?;
                }

                let reservation = reservations.get_mut(reservation_id).ok_or_else(|| {
                    AppResourceContractError::UnknownReservation(reservation_id.to_string())
                })?;

                match outcome {
                    AppResourceSettlementOutcome::Committed => {
                        outstanding = checked_sub(outstanding, reservation.requested)?;
                        committed = committed.checked_add(*actual)?;
                        merge_capability_usage(
                            &mut capability_usage,
                            observed_capabilities,
                            current.policy.max_capability_families,
                        )?;
                        for interval in observed_intervals {
                            active_intervals.insert(*interval)?;
                        }
                        // A final provider/tool result is itself durable forward
                        // progress. Record it atomically with settlement so a
                        // legitimate operation whose bounded provider timeout is
                        // longer than the idle watchdog cannot settle into a
                        // permanent NoProgress breach before a follow-up
                        // ProgressObserved event can be appended.
                        last_progress_ms = last_progress_ms.max(*at_elapsed_ms);
                        reservation.status = ReservationStatus::Settled;
                    },
                    AppResourceSettlementOutcome::ProvenUnspent => {
                        let recovery = recovery_by_observation
                            .remove(observation_id)
                            .ok_or(AppResourceContractError::MissingTrustedRecoveryEvidence)?;
                        if recovery.reservation_id != *reservation_id
                            || recovery.reconciled_at_elapsed_ms != *at_elapsed_ms
                        {
                            return Err(AppResourceContractError::InvalidRecoveryEvidence);
                        }
                        outstanding = checked_sub(outstanding, reservation.requested)?;
                        reservation.status = ReservationStatus::Released;
                    },
                    AppResourceSettlementOutcome::OutcomeUncertain => {
                        reservation.status = ReservationStatus::Uncertain;
                    },
                }
                observations.insert(observation_id.clone(), fingerprint);
            },
            AppResourceJournalEvent::NodeClosed {
                node_id,
                at_elapsed_ms,
                ..
            } => {
                ensure_monotonic_event_time(*at_elapsed_ms, &mut last_event_elapsed_ms)?;
                let node = nodes
                    .get(node_id)
                    .ok_or_else(|| AppResourceContractError::UnknownNode(node_id.to_string()))?;
                if !node.open {
                    return Err(AppResourceContractError::NodeClosed(node_id.to_string()));
                }
                if node.open_children != 0 {
                    return Err(AppResourceContractError::NodeHasOpenChildren(
                        node_id.to_string(),
                    ));
                }
                if *at_elapsed_ms < node.opened_at_elapsed_ms {
                    return Err(AppResourceContractError::EventBeforeNodeOpen);
                }
                let parent_node_id = node.parent_node_id.clone();
                let is_root = node.depth == 0;
                let node = nodes
                    .get_mut(node_id)
                    .ok_or_else(|| AppResourceContractError::UnknownNode(node_id.to_string()))?;
                node.open = false;
                node.closed_at_elapsed_ms = Some(*at_elapsed_ms);
                if let Some(parent_id) = parent_node_id {
                    let parent = nodes.get_mut(&parent_id).ok_or_else(|| {
                        AppResourceContractError::InvalidParent(parent_id.to_string())
                    })?;
                    parent.open_children = parent.open_children.checked_sub(1).ok_or(
                        AppResourceContractError::ArithmeticOverflow("open_children"),
                    )?;
                }
                if is_root {
                    root_closed = true;
                    root_closed_at_elapsed_ms = Some(*at_elapsed_ms);
                }
            },
        }
    }

    if nodes.is_empty() {
        return Err(AppResourceContractError::RootInvariant);
    }
    if !recovery_by_observation.is_empty() {
        return Err(AppResourceContractError::InvalidRecoveryEvidence);
    }

    let active_elapsed_ms = active_intervals.total_millis;
    let mut admitted_active_intervals = active_intervals.clone();
    for reservation in reservations.values().filter(|reservation| {
        matches!(
            reservation.status,
            ReservationStatus::Held | ReservationStatus::Uncertain
        )
    }) {
        admitted_active_intervals.insert(AppActiveInterval {
            start_elapsed_ms: reservation.created_at_elapsed_ms,
            end_elapsed_ms: reservation.expires_at_elapsed_ms,
        })?;
    }
    let admitted_active_elapsed_ms = admitted_active_intervals.total_millis;
    let lifetime_elapsed_ms = root_closed_at_elapsed_ms.unwrap_or(journal.evaluated_at_elapsed_ms);
    let open_nodes = usize_to_u32(
        nodes.values().filter(|node| node.open).count(),
        "open_nodes",
    )?;
    let held_reservations = usize_to_u32(
        reservations
            .values()
            .filter(|reservation| reservation.status == ReservationStatus::Held)
            .count(),
        "held_reservations",
    )?;
    let uncertain_reservations = usize_to_u32(
        reservations
            .values()
            .filter(|reservation| reservation.status == ReservationStatus::Uncertain)
            .count(),
        "uncertain_reservations",
    )?;
    let latest_held_reservation_expiry_elapsed_ms = held_reservation_expiries
        .last_key_value()
        .map(|(expiry, _)| *expiry);
    let mut breaches =
        settled_breaches(committed, active_elapsed_ms, lifetime_elapsed_ms, current)?;

    let no_progress_limit_ms = seconds_to_millis(current.policy.max_no_progress_seconds)?;
    let idle_elapsed_ms = journal
        .evaluated_at_elapsed_ms
        .checked_sub(last_progress_ms)
        .ok_or(AppResourceContractError::EventTimeRegression)?;
    // An admitted operation has its own bounded expiry and may legitimately
    // outlive the shorter idle watchdog (for example, a local model call). Do
    // not declare the tree idle while that exact reservation is still live.
    // Once it expires, both the idle and unreconciled-reservation breakers can
    // fire. Uncertain reservations never suppress the idle breaker.
    let has_live_held_reservation = latest_held_reservation_expiry_elapsed_ms
        .is_some_and(|expiry| expiry > journal.evaluated_at_elapsed_ms);
    if open_nodes > 0 && !has_live_held_reservation && idle_elapsed_ms > no_progress_limit_ms {
        breaches.insert(AppResourceBreach::NoProgress);
    }
    if reservations.values().any(|reservation| {
        matches!(
            reservation.status,
            ReservationStatus::Held | ReservationStatus::Uncertain
        ) && reservation.expires_at_elapsed_ms <= journal.evaluated_at_elapsed_ms
    }) {
        breaches.insert(AppResourceBreach::UnreconciledExpiredReservation);
    }
    if uncertain_reservations > 0 {
        breaches.insert(AppResourceBreach::OutcomeUncertain);
    }
    add_current_period_breaches(&mut breaches, committed.checked_add(outstanding)?, current)?;

    // Read the operation key during replay so the retained state is explicitly
    // part of idempotency evidence rather than an unused convenience field.
    debug_assert!(reservations.iter().all(|(reservation_id, reservation)| {
        operation_keys.get(&reservation.operation_key) == Some(reservation_id)
    }));

    Ok(AppResourceAssessment {
        identity: journal.identity.clone(),
        period_revision: current.baseline.period_revision,
        committed,
        outstanding_reserved: outstanding,
        capability_usage,
        active_elapsed_ms,
        admitted_active_elapsed_ms,
        lifetime_elapsed_ms,
        evaluated_at_elapsed_ms: journal.evaluated_at_elapsed_ms,
        last_progress_elapsed_ms: last_progress_ms,
        latest_held_reservation_expiry_elapsed_ms,
        open_nodes,
        held_reservations,
        uncertain_reservations,
        root_closed,
        breaches,
    })
}

/// Rebuild only the cacheable admitted-active projection from an already
/// decoded journal. This deliberately returns no authority or reservation
/// state; it exists for read-after-write recovery paths whose mutation receipt
/// was lost but whose exact terminal checkpoint is durable.
pub(crate) fn project_admitted_active_elapsed_ms(
    journal: &AppResourceJournal,
) -> Result<u64, AppResourceContractError> {
    let mut active_intervals = ActiveIntervalUnion::default();
    let mut pending = BTreeMap::<AppReference, (AppActiveInterval, Option<AppDigest>)>::new();
    for event in &journal.events {
        match event {
            AppResourceJournalEvent::Reserved {
                reservation_id,
                at_elapsed_ms,
                expires_at_elapsed_ms,
                ..
            } => {
                if at_elapsed_ms >= expires_at_elapsed_ms {
                    return Err(AppResourceContractError::InvalidActiveInterval);
                }
                if pending
                    .insert(
                        reservation_id.clone(),
                        (
                            AppActiveInterval {
                                start_elapsed_ms: *at_elapsed_ms,
                                end_elapsed_ms: *expires_at_elapsed_ms,
                            },
                            None,
                        ),
                    )
                    .is_some()
                {
                    return Err(AppResourceContractError::DuplicateReservation(
                        reservation_id.to_string(),
                    ));
                }
            },
            AppResourceJournalEvent::EffectDispatchStarted {
                reservation_id,
                effect_binding_digest,
                ..
            } => {
                let (_, started_digest) = pending.get_mut(reservation_id).ok_or_else(|| {
                    AppResourceContractError::UnknownReservation(reservation_id.to_string())
                })?;
                if started_digest.is_some() {
                    return Err(AppResourceContractError::EffectDispatchIdentityMismatch);
                }
                *started_digest = Some(effect_binding_digest.clone());
            },
            AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
                reservation_id,
                effect_binding_digest,
                ..
            } => {
                if !pending
                    .get(reservation_id)
                    .is_some_and(|(_, started_digest)| {
                        started_digest.as_ref() == Some(effect_binding_digest)
                    })
                {
                    return Err(AppResourceContractError::EffectDispatchIdentityMismatch);
                }
                if pending.remove(reservation_id).is_none() {
                    return Err(AppResourceContractError::UnknownReservation(
                        reservation_id.to_string(),
                    ));
                }
            },
            AppResourceJournalEvent::Settled {
                reservation_id,
                outcome,
                active_intervals: observed_intervals,
                effect_binding_digest,
                ..
            } => {
                let started_digest = pending
                    .get(reservation_id)
                    .map(|(_, digest)| digest)
                    .ok_or_else(|| {
                        AppResourceContractError::UnknownReservation(reservation_id.to_string())
                    })?;
                if started_digest.as_ref() != effect_binding_digest.as_ref()
                    || (*outcome == AppResourceSettlementOutcome::ProvenUnspent
                        && started_digest.is_some())
                {
                    return Err(AppResourceContractError::EffectDispatchIdentityMismatch);
                }
                match outcome {
                    AppResourceSettlementOutcome::Committed => {
                        pending.remove(reservation_id);
                        for interval in observed_intervals {
                            if interval.start_elapsed_ms >= interval.end_elapsed_ms {
                                return Err(AppResourceContractError::InvalidActiveInterval);
                            }
                            active_intervals.insert(*interval)?;
                        }
                    },
                    AppResourceSettlementOutcome::ProvenUnspent => {
                        pending.remove(reservation_id);
                    },
                    AppResourceSettlementOutcome::OutcomeUncertain => {},
                }
            },
            AppResourceJournalEvent::NodeOpened { .. }
            | AppResourceJournalEvent::ProgressObserved { .. }
            | AppResourceJournalEvent::NodeClosed { .. } => {},
        }
    }
    for (interval, _) in pending.into_values() {
        active_intervals.insert(interval)?;
    }
    Ok(active_intervals.total_millis)
}

fn ensure_identity_matches(
    stored: &AppResourceTreeIdentity,
    current: &AppResourceTreeIdentity,
) -> Result<(), AppResourceContractError> {
    if stored.scope != current.scope {
        return Err(AppResourceContractError::IdentityMismatch("scope"));
    }
    if stored.installation_id != current.installation_id {
        return Err(AppResourceContractError::IdentityMismatch(
            "installation_id",
        ));
    }
    if stored.installation_generation != current.installation_generation {
        return Err(AppResourceContractError::IdentityMismatch(
            "installation_generation",
        ));
    }
    if stored.package_revision_ref != current.package_revision_ref {
        return Err(AppResourceContractError::IdentityMismatch(
            "package_revision_ref",
        ));
    }
    if stored.grant_revision != current.grant_revision {
        return Err(AppResourceContractError::IdentityMismatch("grant_revision"));
    }
    if stored.schema_revision != current.schema_revision {
        return Err(AppResourceContractError::IdentityMismatch(
            "schema_revision",
        ));
    }
    if stored.authority_digest != current.authority_digest {
        return Err(AppResourceContractError::IdentityMismatch(
            "authority_digest",
        ));
    }
    if stored.behavior_resource_identity != current.behavior_resource_identity {
        return Err(AppResourceContractError::IdentityMismatch(
            "behavior_resource_identity",
        ));
    }
    if stored.root_execution_id != current.root_execution_id {
        return Err(AppResourceContractError::IdentityMismatch(
            "root_execution_id",
        ));
    }
    if stored.budget_ledger_ref != current.budget_ledger_ref {
        return Err(AppResourceContractError::IdentityMismatch(
            "budget_ledger_ref",
        ));
    }
    if stored.installation_period_ref != current.installation_period_ref {
        return Err(AppResourceContractError::IdentityMismatch(
            "installation_period_ref",
        ));
    }
    Ok(())
}

fn validate_static_root_context(
    current: &CurrentAppResourceAuthority,
) -> Result<(), AppResourceContractError> {
    if current.baseline.package_bytes > current.policy.max_package_bytes {
        return Err(AppResourceContractError::ReservationDenied(
            AppResourceBreach::PackageBytes,
        ));
    }
    if current.baseline.period_ends_at_elapsed_ms == 0 {
        return Err(AppResourceContractError::ReservationDenied(
            AppResourceBreach::InstallationPeriod,
        ));
    }
    Ok(())
}

/// Evaluate a new root against a freshly read installation-period/scheduler
/// snapshot. This pure verdict is not a reservation: Phase 4B must atomically
/// append root admission under the same period revision before dispatch.
pub fn evaluate_app_resource_root_admission(
    current: &CurrentAppResourceAuthority,
    lane: AppResourceExecutionLane,
) -> Result<(), AppResourceContractError> {
    current.policy.validate(&current.ceiling)?;
    validate_static_root_context(current)?;
    ensure_within_installation_period_ceilings(AppResourceQuantity::default(), current)?;
    admit_lane(lane, current)
}

/// Evaluate one new reservation from a previously assessed tree and a freshly
/// refreshed current authority. The durable implementation must commit the
/// reservation and its installation-period revision atomically; this verdict
/// alone never authorizes dispatch.
pub fn evaluate_app_resource_reservation(
    assessment: &AppResourceAssessment,
    requested: AppResourceQuantity,
    capability_requests: &[AppCapabilityResourceQuantity],
    at_elapsed_ms: u64,
    expires_at_elapsed_ms: u64,
    current: &CurrentAppResourceAuthority,
) -> Result<(), AppResourceContractError> {
    ensure_identity_matches(&assessment.identity, &current.identity)?;
    if current.baseline.period_revision < assessment.period_revision {
        return Err(AppResourceContractError::StalePeriodRevision);
    }
    current.policy.validate(&current.ceiling)?;
    validate_static_root_context(current)?;
    if assessment.root_closed {
        return Err(AppResourceContractError::RootAlreadyClosed);
    }
    if at_elapsed_ms < assessment.evaluated_at_elapsed_ms {
        return Err(AppResourceContractError::EventTimeRegression);
    }
    // Installation-period totals are snapshot-derived. A newer revision can
    // legitimately remove a stale external reservation, so recalculate those
    // two breaches below instead of inheriting their old verdict. Tree-local
    // and reconciliation breaches remain authoritative.
    if let Some(breach) = assessment
        .breaches
        .iter()
        .find(|breach| {
            !matches!(
                breach,
                AppResourceBreach::MonthlyTokens | AppResourceBreach::MonthlyCost
            )
        })
        .copied()
    {
        return Err(AppResourceContractError::ReservationDenied(breach));
    }
    if expires_at_elapsed_ms <= at_elapsed_ms
        || expires_at_elapsed_ms > seconds_to_millis(current.ceiling.max_lifetime_seconds)?
        || expires_at_elapsed_ms > current.baseline.period_ends_at_elapsed_ms
    {
        return Err(AppResourceContractError::InvalidReservationExpiry);
    }
    requested.validate("reservation.requested")?;
    validate_capability_breakdown(
        capability_requests,
        requested,
        current.policy.max_capability_families,
    )?;
    let candidate = assessment
        .committed
        .checked_add(assessment.outstanding_reserved)?
        .checked_add(requested)?;
    ensure_within_tree_dispatch_ceilings(
        candidate,
        assessment.admitted_active_elapsed_ms,
        at_elapsed_ms,
        assessment.last_progress_elapsed_ms,
        assessment.has_live_held_reservation_at(at_elapsed_ms),
        current,
    )?;
    ensure_within_installation_period_ceilings(candidate, current)
}

/// Check the freshly reduced candidate root against the current installation
/// period without applying today's external totals to every historical event.
/// The durable authority calls this only for a newly appended reservation;
/// replay continues to report snapshot-derived breaches without invalidating
/// work that was admitted under an earlier period revision.
pub fn evaluate_app_resource_candidate_period(
    assessment: &AppResourceAssessment,
    current: &CurrentAppResourceAuthority,
) -> Result<(), AppResourceContractError> {
    let candidate = assessment
        .committed
        .checked_add(assessment.outstanding_reserved)?;
    ensure_within_installation_period_ceilings(candidate, current)
}

fn admit_lane(
    lane: AppResourceExecutionLane,
    current: &CurrentAppResourceAuthority,
) -> Result<(), AppResourceContractError> {
    let baseline = &current.baseline;
    match lane {
        AppResourceExecutionLane::Foreground => {
            if baseline.installation_foreground_runs_excluding_root
                >= current.ceiling.max_concurrent_foreground_runs
            {
                return Err(AppResourceContractError::ForegroundConcurrencyDenied);
            }
            let total = u32::from(baseline.scheduler_foreground_runs_excluding_root)
                .checked_add(u32::from(baseline.scheduler_background_runs_excluding_root))
                .and_then(|value| value.checked_add(1))
                .ok_or(AppResourceContractError::ArithmeticOverflow(
                    "scheduler_concurrency",
                ))?;
            if total > u32::from(current.policy.scheduler_capacity) {
                return Err(AppResourceContractError::SchedulerCapacityDenied);
            }
        },
        AppResourceExecutionLane::Background => {
            if baseline.installation_background_runs_excluding_root
                >= current.ceiling.max_concurrent_background_runs
            {
                return Err(AppResourceContractError::BackgroundConcurrencyDenied);
            }
            if baseline.background_starts_excluding_root
                >= current.policy.max_background_starts_per_period
            {
                return Err(AppResourceContractError::BackgroundFrequencyDenied);
            }
            let admitted_total = u32::from(baseline.scheduler_foreground_runs_excluding_root)
                .checked_add(u32::from(baseline.scheduler_background_runs_excluding_root))
                .and_then(|value| value.checked_add(1))
                .ok_or(AppResourceContractError::ArithmeticOverflow(
                    "scheduler_concurrency",
                ))?;
            if admitted_total > u32::from(current.policy.scheduler_capacity) {
                return Err(AppResourceContractError::SchedulerCapacityDenied);
            }
            let background_limit = u32::from(current.policy.scheduler_capacity)
                .checked_sub(u32::from(current.policy.foreground_reserved_slots))
                .ok_or(AppResourceContractError::InvalidPolicy(
                    "foreground reserve exceeds scheduler capacity",
                ))?;
            let background_runs = u32::from(baseline.scheduler_background_runs_excluding_root)
                .checked_add(1)
                .ok_or(AppResourceContractError::ArithmeticOverflow(
                    "scheduler_background_concurrency",
                ))?;
            if background_runs > background_limit {
                return Err(AppResourceContractError::ForegroundReserveDenied);
            }
        },
    }
    Ok(())
}

fn require_node<'a>(
    nodes: &'a BTreeMap<AppReference, NodeState>,
    node_id: &AppReference,
) -> Result<&'a NodeState, AppResourceContractError> {
    nodes
        .get(node_id)
        .ok_or_else(|| AppResourceContractError::UnknownNode(node_id.to_string()))
}

fn ensure_monotonic_event_time(
    at_elapsed_ms: u64,
    last_effective_event_elapsed_ms: &mut u64,
) -> Result<(), AppResourceContractError> {
    if at_elapsed_ms < *last_effective_event_elapsed_ms {
        return Err(AppResourceContractError::EventTimeRegression);
    }
    *last_effective_event_elapsed_ms = at_elapsed_ms;
    Ok(())
}

fn require_open_node<'a>(
    nodes: &'a BTreeMap<AppReference, NodeState>,
    node_id: &AppReference,
) -> Result<&'a NodeState, AppResourceContractError> {
    let node = nodes
        .get(node_id)
        .ok_or_else(|| AppResourceContractError::UnknownNode(node_id.to_string()))?;
    if node.open {
        Ok(node)
    } else {
        Err(AppResourceContractError::NodeClosed(node_id.to_string()))
    }
}

fn validate_capability_breakdown(
    values: &[AppCapabilityResourceQuantity],
    aggregate: AppResourceQuantity,
    max_families: u16,
) -> Result<(), AppResourceContractError> {
    if values.len() > usize::from(max_families) {
        return Err(AppResourceContractError::CapabilityFamilyLimitExceeded {
            limit: max_families,
        });
    }
    for pair in values.windows(2) {
        if pair[0].capability_family == pair[1].capability_family {
            return Err(AppResourceContractError::DuplicateCapabilityFamily(
                pair[0].capability_family.to_string(),
            ));
        }
        if pair[0].capability_family > pair[1].capability_family {
            return Err(AppResourceContractError::NonCanonicalCapabilityOrder);
        }
    }
    let mut invocations = 0_u64;
    let mut cost = 0_u64;
    for value in values {
        invocations = invocations.checked_add(value.paid_invocations).ok_or(
            AppResourceContractError::ArithmeticOverflow("capability_paid_invocations"),
        )?;
        cost = cost.checked_add(value.cost_microusd).ok_or(
            AppResourceContractError::ArithmeticOverflow("capability_cost_microusd"),
        )?;
    }
    if invocations != aggregate.paid_tool_invocations || cost > aggregate.cost_microusd {
        return Err(AppResourceContractError::CapabilityTotalMismatch);
    }
    Ok(())
}

fn ensure_settlement_within_reservation(
    actual: AppResourceQuantity,
    capability_usage: &[AppCapabilityResourceQuantity],
    requested: AppResourceQuantity,
    capability_requests: &[AppCapabilityResourceQuantity],
) -> Result<(), AppResourceContractError> {
    if let Some(field) = actual.first_component_exceeding(requested) {
        return Err(AppResourceContractError::SettlementExceedsReservation { field });
    }

    // Both slices are already validated as unique canonical ascending lists,
    // so a bounded two-pointer walk avoids allocation and also rejects usage
    // for a family which was never admitted.
    let mut requested_index = 0_usize;
    for observed in capability_usage {
        while requested_index < capability_requests.len()
            && capability_requests[requested_index]
                .capability_family
                .as_str()
                < observed.capability_family.as_str()
        {
            requested_index += 1;
        }
        let Some(reserved) = capability_requests.get(requested_index) else {
            return Err(
                AppResourceContractError::CapabilitySettlementExceedsReservation(
                    observed.capability_family.to_string(),
                ),
            );
        };
        if reserved.capability_family.as_str() != observed.capability_family.as_str()
            || observed.paid_invocations > reserved.paid_invocations
            || observed.cost_microusd > reserved.cost_microusd
        {
            return Err(
                AppResourceContractError::CapabilitySettlementExceedsReservation(
                    observed.capability_family.to_string(),
                ),
            );
        }
    }
    Ok(())
}

fn validate_observation_sources(
    sources: &[AppResourceObservationSource],
) -> Result<(), AppResourceContractError> {
    if sources.is_empty() || sources.len() > HARD_MAX_OBSERVATION_SOURCES {
        return Err(AppResourceContractError::InvalidObservationSources);
    }
    if sources.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(AppResourceContractError::InvalidObservationSources);
    }
    Ok(())
}

fn validate_settlement_evidence(
    outcome: AppResourceSettlementOutcome,
    sources: &[AppResourceObservationSource],
) -> Result<(), AppResourceContractError> {
    if outcome == AppResourceSettlementOutcome::ProvenUnspent
        && (sources.len() != 1 || sources[0] != AppResourceObservationSource::CrashReconciler)
    {
        return Err(AppResourceContractError::UntrustedProvenUnspentEvidence);
    }
    Ok(())
}

fn valid_effect_settlement_owner(
    outcome: AppResourceSettlementOutcome,
    sources: &[AppResourceObservationSource],
    has_result: bool,
) -> bool {
    if sources.contains(&AppResourceObservationSource::CrashReconciler) {
        // The durable cleanup owner verifies a move-only crash proof and the
        // exact reserved upper bound before appending this event. It retains
        // the dispatched effect identity but cannot invent a provider result,
        // prove that I/O did not happen, or pose as a physical observer.
        return sources == [AppResourceObservationSource::CrashReconciler]
            && outcome == AppResourceSettlementOutcome::Committed
            && !has_result;
    }
    sources.iter().any(|source| {
        matches!(
            source,
            AppResourceObservationSource::ToolRuntime
                | AppResourceObservationSource::BrowserRuntime
        )
    })
}

fn merge_capability_usage(
    aggregate: &mut BTreeMap<AppName, AppCapabilityResourceQuantity>,
    values: &[AppCapabilityResourceQuantity],
    max_families: u16,
) -> Result<(), AppResourceContractError> {
    let new_families = values
        .iter()
        .filter(|value| !aggregate.contains_key(&value.capability_family))
        .count();
    let candidate_family_count = aggregate.len().checked_add(new_families).ok_or(
        AppResourceContractError::ArithmeticOverflow("capability_family_count"),
    )?;
    if candidate_family_count > usize::from(max_families) {
        return Err(AppResourceContractError::CapabilityFamilyLimitExceeded {
            limit: max_families,
        });
    }
    for value in values {
        let entry = aggregate
            .entry(value.capability_family.clone())
            .or_insert_with(|| AppCapabilityResourceQuantity {
                capability_family: value.capability_family.clone(),
                paid_invocations: 0,
                cost_microusd: 0,
            });
        entry.paid_invocations = entry
            .paid_invocations
            .checked_add(value.paid_invocations)
            .ok_or(AppResourceContractError::ArithmeticOverflow(
                "capability_paid_invocations",
            ))?;
        entry.cost_microusd = entry.cost_microusd.checked_add(value.cost_microusd).ok_or(
            AppResourceContractError::ArithmeticOverflow("capability_cost_microusd"),
        )?;
    }
    Ok(())
}

fn validate_active_intervals(
    intervals: &[AppActiveInterval],
    node_opened_at_elapsed_ms: u64,
    node_ended_at_elapsed_ms: u64,
    existing_count: usize,
    max_intervals: u32,
) -> Result<(), AppResourceContractError> {
    let total = existing_count.checked_add(intervals.len()).ok_or(
        AppResourceContractError::ArithmeticOverflow("active_interval_count"),
    )?;
    if total > max_intervals as usize {
        return Err(AppResourceContractError::ActiveIntervalLimitExceeded {
            limit: max_intervals,
        });
    }
    for interval in intervals {
        if interval.start_elapsed_ms >= interval.end_elapsed_ms
            || interval.start_elapsed_ms < node_opened_at_elapsed_ms
            || interval.end_elapsed_ms > node_ended_at_elapsed_ms
        {
            return Err(AppResourceContractError::InvalidActiveInterval);
        }
    }
    Ok(())
}

fn validate_active_interval_batch_size(
    incoming_count: usize,
    max_intervals: u32,
) -> Result<(), AppResourceContractError> {
    if incoming_count > max_intervals as usize {
        return Err(AppResourceContractError::ActiveIntervalLimitExceeded {
            limit: max_intervals,
        });
    }
    Ok(())
}

fn checked_sub(
    value: AppResourceQuantity,
    other: AppResourceQuantity,
) -> Result<AppResourceQuantity, AppResourceContractError> {
    macro_rules! difference {
        ($field:ident) => {
            value.$field.checked_sub(other.$field).ok_or(
                AppResourceContractError::ArithmeticOverflow(concat!(
                    "outstanding_",
                    stringify!($field)
                )),
            )?
        };
    }
    Ok(AppResourceQuantity {
        input_tokens: difference!(input_tokens),
        cached_input_tokens: difference!(cached_input_tokens),
        output_tokens: difference!(output_tokens),
        cost_microusd: difference!(cost_microusd),
        paid_tool_invocations: difference!(paid_tool_invocations),
        browser_network_actions: difference!(browser_network_actions),
        records: difference!(records),
        payload_bytes: difference!(payload_bytes),
        attachment_bytes: difference!(attachment_bytes),
    })
}

fn ensure_within_tree_dispatch_ceilings(
    candidate: AppResourceQuantity,
    active_elapsed_ms: u64,
    at_elapsed_ms: u64,
    last_progress_ms: u64,
    has_live_held_reservation: bool,
    current: &CurrentAppResourceAuthority,
) -> Result<(), AppResourceContractError> {
    let ceiling = &current.ceiling;
    let checks = [
        (
            candidate.input_tokens > ceiling.max_input_tokens,
            AppResourceBreach::InputTokens,
        ),
        (
            candidate.output_tokens > ceiling.max_output_tokens,
            AppResourceBreach::OutputTokens,
        ),
        (
            candidate.cost_microusd > ceiling.max_cost_microusd,
            AppResourceBreach::Cost,
        ),
        (
            candidate.paid_tool_invocations > ceiling.max_paid_tool_invocations,
            AppResourceBreach::PaidToolInvocations,
        ),
        (
            candidate.browser_network_actions > ceiling.max_browser_network_actions,
            AppResourceBreach::BrowserNetworkActions,
        ),
        (
            candidate.records > ceiling.max_records,
            AppResourceBreach::Records,
        ),
        (
            candidate.payload_bytes > ceiling.max_payload_bytes,
            AppResourceBreach::PayloadBytes,
        ),
        (
            candidate.attachment_bytes > ceiling.max_attachment_bytes,
            AppResourceBreach::AttachmentBytes,
        ),
    ];
    for (exceeded, breach) in checks {
        if exceeded {
            return Err(AppResourceContractError::ReservationDenied(breach));
        }
    }

    if active_elapsed_ms > seconds_to_millis(ceiling.max_active_seconds)? {
        return Err(AppResourceContractError::ReservationDenied(
            AppResourceBreach::ActiveTime,
        ));
    }
    if at_elapsed_ms > seconds_to_millis(ceiling.max_lifetime_seconds)? {
        return Err(AppResourceContractError::ReservationDenied(
            AppResourceBreach::Lifetime,
        ));
    }
    if at_elapsed_ms > current.baseline.period_ends_at_elapsed_ms {
        return Err(AppResourceContractError::ReservationDenied(
            AppResourceBreach::InstallationPeriod,
        ));
    }
    ensure_no_progress_before_work_expanding_event(
        at_elapsed_ms,
        last_progress_ms,
        has_live_held_reservation,
        current,
    )?;

    Ok(())
}

fn ensure_no_progress_before_work_expanding_event(
    at_elapsed_ms: u64,
    last_progress_ms: u64,
    has_live_held_reservation: bool,
    current: &CurrentAppResourceAuthority,
) -> Result<(), AppResourceContractError> {
    let idle_elapsed_ms = at_elapsed_ms
        .checked_sub(last_progress_ms)
        .ok_or(AppResourceContractError::EventTimeRegression)?;
    if !has_live_held_reservation
        && idle_elapsed_ms > seconds_to_millis(current.policy.max_no_progress_seconds)?
    {
        return Err(AppResourceContractError::ReservationDenied(
            AppResourceBreach::NoProgress,
        ));
    }
    Ok(())
}

fn has_live_held_reservation_at(expiries: &BTreeMap<u64, u32>, at_elapsed_ms: u64) -> bool {
    expiries
        .last_key_value()
        .is_some_and(|(expiry, _)| *expiry > at_elapsed_ms)
}

fn add_held_reservation_expiry(
    expiries: &mut BTreeMap<u64, u32>,
    expiry_elapsed_ms: u64,
) -> Result<(), AppResourceContractError> {
    let count = expiries.entry(expiry_elapsed_ms).or_default();
    *count = count
        .checked_add(1)
        .ok_or(AppResourceContractError::ArithmeticOverflow(
            "held_reservation_expiry_count",
        ))?;
    Ok(())
}

fn remove_held_reservation_expiry(
    expiries: &mut BTreeMap<u64, u32>,
    expiry_elapsed_ms: u64,
) -> Result<(), AppResourceContractError> {
    let count = expiries.get_mut(&expiry_elapsed_ms).ok_or(
        AppResourceContractError::ArithmeticOverflow("held_reservation_expiry_count"),
    )?;
    *count = count
        .checked_sub(1)
        .ok_or(AppResourceContractError::ArithmeticOverflow(
            "held_reservation_expiry_count",
        ))?;
    if *count == 0 {
        expiries.remove(&expiry_elapsed_ms);
    }
    Ok(())
}

fn ensure_within_installation_period_ceilings(
    tree_committed_and_reserved: AppResourceQuantity,
    current: &CurrentAppResourceAuthority,
) -> Result<(), AppResourceContractError> {
    let ceiling = &current.ceiling;
    let (period_tokens, period_cost) =
        installation_period_totals(tree_committed_and_reserved, current)?;
    if period_tokens > ceiling.max_monthly_tokens {
        return Err(AppResourceContractError::ReservationDenied(
            AppResourceBreach::MonthlyTokens,
        ));
    }
    if period_cost > ceiling.max_monthly_cost_microusd {
        return Err(AppResourceContractError::ReservationDenied(
            AppResourceBreach::MonthlyCost,
        ));
    }
    if let Some((behavior_tokens, behavior_cost, behavior_identity)) =
        behavior_period_totals(tree_committed_and_reserved, current)?
    {
        if behavior_tokens > behavior_identity.max_monthly_tokens() {
            return Err(AppResourceContractError::ReservationDenied(
                AppResourceBreach::MonthlyTokens,
            ));
        }
        if behavior_cost > behavior_identity.max_monthly_cost_microusd() {
            return Err(AppResourceContractError::ReservationDenied(
                AppResourceBreach::MonthlyCost,
            ));
        }
    }
    Ok(())
}

fn add_current_period_breaches(
    breaches: &mut BTreeSet<AppResourceBreach>,
    tree_committed_and_reserved: AppResourceQuantity,
    current: &CurrentAppResourceAuthority,
) -> Result<(), AppResourceContractError> {
    let (period_tokens, period_cost) =
        installation_period_totals(tree_committed_and_reserved, current)?;
    if period_tokens > current.ceiling.max_monthly_tokens {
        breaches.insert(AppResourceBreach::MonthlyTokens);
    }
    if period_cost > current.ceiling.max_monthly_cost_microusd {
        breaches.insert(AppResourceBreach::MonthlyCost);
    }
    if let Some((behavior_tokens, behavior_cost, behavior_identity)) =
        behavior_period_totals(tree_committed_and_reserved, current)?
    {
        if behavior_tokens > behavior_identity.max_monthly_tokens() {
            breaches.insert(AppResourceBreach::MonthlyTokens);
        }
        if behavior_cost > behavior_identity.max_monthly_cost_microusd() {
            breaches.insert(AppResourceBreach::MonthlyCost);
        }
    }
    Ok(())
}

fn installation_period_totals(
    tree_committed_and_reserved: AppResourceQuantity,
    current: &CurrentAppResourceAuthority,
) -> Result<(u64, u64), AppResourceContractError> {
    let tree_tokens = total_tokens(tree_committed_and_reserved)?;
    let period_tokens = current
        .baseline
        .tokens_excluding_root
        .checked_add(current.baseline.outstanding_tokens_excluding_root)
        .and_then(|value| value.checked_add(tree_tokens))
        .ok_or(AppResourceContractError::ArithmeticOverflow(
            "monthly_tokens",
        ))?;
    let period_cost = current
        .baseline
        .cost_microusd_excluding_root
        .checked_add(current.baseline.outstanding_cost_microusd_excluding_root)
        .and_then(|value| value.checked_add(tree_committed_and_reserved.cost_microusd))
        .ok_or(AppResourceContractError::ArithmeticOverflow(
            "monthly_cost_microusd",
        ))?;
    Ok((period_tokens, period_cost))
}

fn behavior_period_totals(
    tree_committed_and_reserved: AppResourceQuantity,
    current: &CurrentAppResourceAuthority,
) -> Result<Option<(u64, u64, &AppResourceBehaviorIdentity)>, AppResourceContractError> {
    let Some(identity) = current.identity.behavior_resource_identity.as_ref() else {
        return Ok(None);
    };
    let baseline = current.baseline.behavior_period.as_ref().ok_or(
        AppResourceContractError::IdentityMismatch("behavior_period_baseline"),
    )?;
    let tree_tokens = total_tokens(tree_committed_and_reserved)?;
    let period_tokens = baseline
        .tokens_excluding_root
        .checked_add(baseline.outstanding_tokens_excluding_root)
        .and_then(|value| value.checked_add(tree_tokens))
        .ok_or(AppResourceContractError::ArithmeticOverflow(
            "behavior_monthly_tokens",
        ))?;
    let period_cost = baseline
        .cost_microusd_excluding_root
        .checked_add(baseline.outstanding_cost_microusd_excluding_root)
        .and_then(|value| value.checked_add(tree_committed_and_reserved.cost_microusd))
        .ok_or(AppResourceContractError::ArithmeticOverflow(
            "behavior_monthly_cost_microusd",
        ))?;
    Ok(Some((period_tokens, period_cost, identity)))
}

fn settled_breaches(
    committed: AppResourceQuantity,
    active_elapsed_ms: u64,
    lifetime_elapsed_ms: u64,
    current: &CurrentAppResourceAuthority,
) -> Result<BTreeSet<AppResourceBreach>, AppResourceContractError> {
    let mut breaches = BTreeSet::new();
    let ceiling = &current.ceiling;
    let checks = [
        (
            committed.input_tokens > ceiling.max_input_tokens,
            AppResourceBreach::InputTokens,
        ),
        (
            committed.output_tokens > ceiling.max_output_tokens,
            AppResourceBreach::OutputTokens,
        ),
        (
            committed.cost_microusd > ceiling.max_cost_microusd,
            AppResourceBreach::Cost,
        ),
        (
            committed.paid_tool_invocations > ceiling.max_paid_tool_invocations,
            AppResourceBreach::PaidToolInvocations,
        ),
        (
            committed.browser_network_actions > ceiling.max_browser_network_actions,
            AppResourceBreach::BrowserNetworkActions,
        ),
        (
            committed.records > ceiling.max_records,
            AppResourceBreach::Records,
        ),
        (
            committed.payload_bytes > ceiling.max_payload_bytes,
            AppResourceBreach::PayloadBytes,
        ),
        (
            committed.attachment_bytes > ceiling.max_attachment_bytes,
            AppResourceBreach::AttachmentBytes,
        ),
        (
            active_elapsed_ms > seconds_to_millis(ceiling.max_active_seconds)?,
            AppResourceBreach::ActiveTime,
        ),
        (
            lifetime_elapsed_ms > seconds_to_millis(ceiling.max_lifetime_seconds)?,
            AppResourceBreach::Lifetime,
        ),
        (
            lifetime_elapsed_ms > current.baseline.period_ends_at_elapsed_ms,
            AppResourceBreach::InstallationPeriod,
        ),
    ];
    for (exceeded, breach) in checks {
        if exceeded {
            breaches.insert(breach);
        }
    }
    Ok(breaches)
}

fn seconds_to_millis(seconds: u64) -> Result<u64, AppResourceContractError> {
    seconds
        .checked_mul(1_000)
        .ok_or(AppResourceContractError::ArithmeticOverflow(
            "seconds_to_millis",
        ))
}

fn total_tokens(quantity: AppResourceQuantity) -> Result<u64, AppResourceContractError> {
    quantity
        .input_tokens
        .checked_add(quantity.output_tokens)
        .ok_or(AppResourceContractError::ArithmeticOverflow("total_tokens"))
}

fn usize_to_u32(value: usize, field: &'static str) -> Result<u32, AppResourceContractError> {
    u32::try_from(value).map_err(|_| AppResourceContractError::ArithmeticOverflow(field))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).expect("test reference")
    }

    fn name(value: &str) -> AppName {
        AppName::parse(value).expect("test name")
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).expect("test revision")
    }

    fn quantity(cost_microusd: u64) -> AppResourceQuantity {
        AppResourceQuantity {
            cost_microusd,
            ..AppResourceQuantity::default()
        }
    }

    fn ceiling(value: u64) -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: value,
            max_output_tokens: value,
            max_cost_microusd: value,
            max_paid_tool_invocations: value,
            max_active_seconds: value,
            max_lifetime_seconds: value,
            max_browser_network_actions: value,
            max_concurrent_foreground_runs: 4,
            max_concurrent_background_runs: 2,
            max_records: value,
            max_payload_bytes: value,
            max_attachment_bytes: value,
            max_monthly_tokens: value.saturating_mul(2),
            max_monthly_cost_microusd: value,
        }
    }

    fn policy() -> AppResourceEnforcementPolicy {
        AppResourceEnforcementPolicy {
            max_tree_nodes: 64,
            max_tree_depth: 8,
            max_journal_events: 256,
            max_reservations: 64,
            max_active_intervals: 64,
            max_capability_families: 16,
            max_no_progress_seconds: 30,
            max_package_bytes: 1_000,
            max_background_starts_per_period: 20,
            scheduler_capacity: 8,
            foreground_reserved_slots: 2,
        }
    }

    #[test]
    fn server_resource_policy_uses_the_same_hard_caps_as_admission() {
        let valid = policy();
        assert_eq!(
            valid.validate_server_configuration().expect("valid policy"),
            valid
        );

        let mut oversized = valid;
        oversized.max_journal_events = HARD_MAX_JOURNAL_EVENTS + 1;
        assert_eq!(
            oversized.validate_server_configuration(),
            Err(AppResourceContractError::InvalidPolicy(
                "max_journal_events must be positive and no greater than the hard cap"
            ))
        );
    }

    #[test]
    fn app_lifetime_compatibility_remains_an_admission_check() {
        let server_policy = policy();
        server_policy
            .validate_server_configuration()
            .expect("server policy does not depend on one app ceiling");
        let short_lived = ceiling(10);
        assert_eq!(
            server_policy.validate(&short_lived),
            Err(AppResourceContractError::InvalidPolicy(
                "max_no_progress_seconds must be within max_lifetime_seconds"
            ))
        );
    }

    fn current_with(ceiling: AppResourceCeiling) -> CurrentAppResourceAuthority {
        let scope = AppScope {
            principal: reference("principal:test"),
            workspace: reference("workspace:test"),
        };
        let installation_id = AppInstallationId::parse("installation-test").expect("installation");
        CurrentAppResourceAuthority {
            identity: AppResourceTreeIdentity {
                scope: scope.clone(),
                installation_id: installation_id.clone(),
                installation_generation: 3,
                package_revision_ref: reference("package:revision"),
                grant_revision: revision(4),
                schema_revision: revision(5),
                authority_digest: AppDigest::blake3(b"authority"),
                behavior_resource_identity: None,
                root_execution_id: reference("execution:root"),
                budget_ledger_ref: reference("ledger:root"),
                installation_period_ref: reference("period:2026-08"),
            },
            ceiling,
            policy: policy(),
            baseline: AppResourcePeriodBaseline::from_resource_store(
                scope,
                installation_id,
                3,
                reference("ledger:root"),
                reference("period:2026-08"),
                revision(1),
                100_000,
                0,
                0,
                0,
                0,
                None,
                0,
                0,
                0,
                0,
                0,
                100,
            ),
        }
    }

    #[test]
    fn current_resource_authority_requires_same_boundary_resolution_time() {
        let now = Utc
            .with_ymd_and_hms(2026, 8, 15, 8, 0, 0)
            .single()
            .expect("test time");
        let authenticated = AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: reference("principal:test"),
                workspace: reference("workspace:test"),
            },
            crate::magician_v2::apps::models::AppScopeBindingRef::parse("scope-test")
                .expect("scope binding"),
            reference("actor:test"),
            reference("session:test"),
            revision(1),
            now - Duration::minutes(1),
            now + Duration::minutes(1),
        )
        .expect("authenticated scope");
        assert_eq!(
            ensure_current_authority_freshness(&authenticated, &now, &now),
            Ok(())
        );
        assert_eq!(
            ensure_current_authority_freshness(
                &authenticated,
                &(now - Duration::milliseconds(1)),
                &now,
            ),
            Err(AppResourceContractError::StaleCurrentAuthority)
        );

        let expired = AuthenticatedAppScope::from_verified_session(
            authenticated.scope().clone(),
            authenticated.scope_binding_ref().clone(),
            authenticated.actor_ref().clone(),
            authenticated.session_ref().clone(),
            authenticated.authentication_revision(),
            now - Duration::minutes(2),
            now,
        )
        .expect("expired evidence can be represented for rejection");
        assert_eq!(
            ensure_current_authority_freshness(&expired, &now, &now),
            Err(AppResourceContractError::IdentityMismatch(
                "authentication_window"
            ))
        );
    }

    fn open_root(sequence: u64, lane: AppResourceExecutionLane) -> AppResourceJournalEvent {
        AppResourceJournalEvent::NodeOpened {
            sequence,
            node_id: reference("node:root"),
            execution_ref: reference("execution:root"),
            parent_node_id: None,
            node_kind: AppResourceNodeKind::Root,
            lane,
            at_elapsed_ms: 0,
        }
    }

    fn open_child(sequence: u64, id: &str, kind: AppResourceNodeKind) -> AppResourceJournalEvent {
        AppResourceJournalEvent::NodeOpened {
            sequence,
            node_id: reference(id),
            execution_ref: reference(&format!("execution:{id}")),
            parent_node_id: Some(reference("node:root")),
            node_kind: kind,
            lane: AppResourceExecutionLane::Foreground,
            at_elapsed_ms: sequence,
        }
    }

    fn reserve(sequence: u64, node_id: &str, id: &str, cost: u64) -> AppResourceJournalEvent {
        AppResourceJournalEvent::Reserved {
            sequence,
            node_id: reference(node_id),
            reservation_id: reference(id),
            operation_key: reference(&format!("operation:{id}")),
            requested: quantity(cost),
            capability_requests: Vec::new(),
            at_elapsed_ms: sequence,
            expires_at_elapsed_ms: 60_000,
        }
    }

    fn settle(
        sequence: u64,
        node_id: &str,
        reservation_id: &str,
        observation_id: &str,
        cost: u64,
        intervals: Vec<AppActiveInterval>,
    ) -> AppResourceJournalEvent {
        let at_elapsed_ms = intervals
            .iter()
            .map(|interval| interval.end_elapsed_ms)
            .max()
            .unwrap_or(sequence)
            .max(sequence);
        AppResourceJournalEvent::Settled {
            sequence,
            node_id: reference(node_id),
            reservation_id: reference(reservation_id),
            observation_id: reference(observation_id),
            outcome: AppResourceSettlementOutcome::Committed,
            observation_sources: vec![AppResourceObservationSource::LlmTaskLedger],
            actual: quantity(cost),
            capability_usage: Vec::new(),
            active_intervals: intervals,
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms,
        }
    }

    fn journal(
        current: &CurrentAppResourceAuthority,
        evaluated_at_elapsed_ms: u64,
        events: Vec<AppResourceJournalEvent>,
    ) -> AppResourceJournal {
        AppResourceJournal {
            identity: current.identity.clone(),
            evaluated_at_elapsed_ms,
            events,
        }
    }

    #[test]
    fn crash_effect_cleanup_roundtrips_without_inventing_physical_evidence() {
        let current = current_with(ceiling(100));
        let effect = AppDigest::blake3(b"crashed-source-read");
        let before = vec![
            open_root(1, AppResourceExecutionLane::Foreground),
            reserve(2, "node:root", "reservation:read", 0),
            AppResourceJournalEvent::EffectDispatchStarted {
                sequence: 3,
                node_id: reference("node:root"),
                reservation_id: reference("reservation:read"),
                effect_binding_digest: effect.clone(),
                at_elapsed_ms: 3,
            },
        ];
        let settlement = AppResourceJournalEvent::Settled {
            sequence: 4,
            node_id: reference("node:root"),
            reservation_id: reference("reservation:read"),
            observation_id: reference("observation:crash-cleanup"),
            outcome: AppResourceSettlementOutcome::Committed,
            observation_sources: vec![AppResourceObservationSource::CrashReconciler],
            actual: quantity(0),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: Some(effect),
            effect_result: None,
            at_elapsed_ms: 4,
        };
        let mut events = before.clone();
        events.push(settlement.clone());
        events.push(AppResourceJournalEvent::NodeClosed {
            sequence: 5,
            node_id: reference("node:root"),
            at_elapsed_ms: 5,
        });
        let candidate = journal(&current, 5, events);
        let bytes = serde_json::to_vec(&candidate).unwrap();
        let decoded = decode_app_resource_journal(&bytes)
            .expect("crash cleanup must survive durable journal validation");
        assert_eq!(decoded.events[..before.len()], before);
        let assessment = assess_app_resource_journal(&decoded, &current).unwrap();
        assert!(assessment.terminally_settled());
        assert_eq!(assessment.uncertain_reservations(), 0);

        for (outcome, sources, result) in [
            (
                AppResourceSettlementOutcome::OutcomeUncertain,
                vec![AppResourceObservationSource::CrashReconciler],
                None,
            ),
            (
                AppResourceSettlementOutcome::ProvenUnspent,
                vec![AppResourceObservationSource::CrashReconciler],
                None,
            ),
            (
                AppResourceSettlementOutcome::Committed,
                vec![AppResourceObservationSource::CrashReconciler],
                Some(AppResourceEffectResult {
                    result_digest: AppDigest::blake3(b"invented"),
                    result_bytes: 8,
                }),
            ),
            (
                AppResourceSettlementOutcome::Committed,
                vec![
                    AppResourceObservationSource::ToolRuntime,
                    AppResourceObservationSource::CrashReconciler,
                ],
                None,
            ),
        ] {
            let mut invalid = settlement.clone();
            if let AppResourceJournalEvent::Settled {
                outcome: target_outcome,
                observation_sources,
                effect_result,
                ..
            } = &mut invalid
            {
                *target_outcome = outcome;
                *observation_sources = sources;
                *effect_result = result;
            }
            let mut events = before.clone();
            events.push(invalid);
            let invalid = journal(&current, 4, events);
            assert!(decode_app_resource_journal(&serde_json::to_vec(&invalid).unwrap()).is_err());
            assert!(assess_app_resource_journal(&invalid, &current).is_err());
        }
    }

    #[test]
    fn durable_effect_start_turns_crash_window_uncertain_and_binds_settlement() {
        let current = current_with(ceiling(100));
        let effect_digest = AppDigest::blake3(b"effect-binding");
        let started = vec![
            open_root(1, AppResourceExecutionLane::Foreground),
            open_child(2, "node:tool", AppResourceNodeKind::ToolCall),
            reserve(3, "node:tool", "reservation:tool", 1),
            AppResourceJournalEvent::EffectDispatchStarted {
                sequence: 4,
                node_id: reference("node:tool"),
                reservation_id: reference("reservation:tool"),
                effect_binding_digest: effect_digest.clone(),
                at_elapsed_ms: 4,
            },
        ];
        let assessment =
            assess_app_resource_journal(&journal(&current, 4, started.clone()), &current)
                .expect("dispatch-start journal");
        assert_eq!(assessment.held_reservations(), 0);
        assert_eq!(assessment.uncertain_reservations(), 1);

        let mut aborted = started.clone();
        aborted.push(AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
            sequence: 5,
            node_id: reference("node:tool"),
            reservation_id: reference("reservation:tool"),
            effect_binding_digest: effect_digest.clone(),
            reason: AppEffectDispatchAbortReason::AdmissionExpired,
            at_elapsed_ms: 5,
        });
        assert_eq!(
            project_admitted_active_elapsed_ms(&journal(&current, 5, aborted))
                .expect("abort readback active projection"),
            0
        );

        let raw_result = b"canonical-action-result";
        let mut committed = started.clone();
        committed.push(AppResourceJournalEvent::Settled {
            sequence: 5,
            node_id: reference("node:tool"),
            reservation_id: reference("reservation:tool"),
            observation_id: reference("observation:tool-committed"),
            outcome: AppResourceSettlementOutcome::Committed,
            observation_sources: vec![AppResourceObservationSource::ToolRuntime],
            actual: quantity(1),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: Some(effect_digest.clone()),
            effect_result: Some(AppResourceEffectResult {
                result_digest: AppDigest::blake3(raw_result),
                result_bytes: u64::try_from(raw_result.len()).unwrap(),
            }),
            at_elapsed_ms: 5,
        });
        let committed_assessment =
            assess_app_resource_journal(&journal(&current, 5, committed), &current)
                .expect("effect result identity remains in canonical settlement");
        assert_eq!(committed_assessment.uncertain_reservations(), 0);

        let mut mismatched = started;
        mismatched.push(AppResourceJournalEvent::Settled {
            sequence: 5,
            node_id: reference("node:tool"),
            reservation_id: reference("reservation:tool"),
            observation_id: reference("observation:tool"),
            outcome: AppResourceSettlementOutcome::Committed,
            observation_sources: vec![AppResourceObservationSource::ToolRuntime],
            actual: quantity(1),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: Some(AppDigest::blake3(b"other-effect")),
            effect_result: None,
            at_elapsed_ms: 5,
        });
        assert_eq!(
            assess_app_resource_journal(&journal(&current, 5, mismatched), &current),
            Err(AppResourceContractError::EffectDispatchIdentityMismatch)
        );
    }

    #[test]
    fn fragmented_child_meters_would_admit_what_the_root_contract_denies() {
        let current = current_with(ceiling(100));
        let fragments = [quantity(60), quantity(60)];
        assert!(fragments
            .iter()
            .all(|fragment| fragment.cost_microusd <= 100));

        let result = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    open_child(2, "node:first", AppResourceNodeKind::DelegatedChild),
                    reserve(3, "node:first", "reservation:first", 60),
                    settle(
                        4,
                        "node:first",
                        "reservation:first",
                        "observation:first",
                        60,
                        Vec::new(),
                    ),
                    open_child(5, "node:second", AppResourceNodeKind::DelegatedChild),
                    reserve(6, "node:second", "reservation:second", 60),
                ],
            ),
            &current,
        );
        assert_eq!(
            result,
            Err(AppResourceContractError::ReservationDenied(
                AppResourceBreach::Cost
            ))
        );
    }

    #[test]
    fn retry_resume_repair_and_synthesis_share_one_root_ceiling() {
        let current = current_with(ceiling(100));
        let kinds = [
            AppResourceNodeKind::Retry,
            AppResourceNodeKind::Resume,
            AppResourceNodeKind::Repair,
            AppResourceNodeKind::Synthesis,
        ];
        let mut events = vec![open_root(1, AppResourceExecutionLane::Foreground)];
        let mut sequence = 2;
        for (index, kind) in kinds.into_iter().enumerate() {
            let node = format!("node:branch-{index}");
            let reservation = format!("reservation:branch-{index}");
            let observation = format!("observation:branch-{index}");
            events.push(open_child(sequence, &node, kind));
            sequence += 1;
            events.push(reserve(sequence, &node, &reservation, 30));
            sequence += 1;
            if index < 3 {
                events.push(settle(
                    sequence,
                    &node,
                    &reservation,
                    &observation,
                    30,
                    Vec::new(),
                ));
                sequence += 1;
            }
        }
        assert!(matches!(
            assess_app_resource_journal(&journal(&current, 100, events), &current),
            Err(AppResourceContractError::ReservationDenied(_))
        ));
    }

    #[test]
    fn parallel_active_time_is_interval_union_not_child_sum() {
        let current = current_with(ceiling(100));
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                20,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    open_child(2, "node:first", AppResourceNodeKind::DelegatedChild),
                    reserve(3, "node:first", "reservation:first", 1),
                    open_child(4, "node:second", AppResourceNodeKind::DelegatedChild),
                    reserve(5, "node:second", "reservation:second", 1),
                    settle(
                        6,
                        "node:first",
                        "reservation:first",
                        "observation:first",
                        1,
                        vec![AppActiveInterval {
                            start_elapsed_ms: 3,
                            end_elapsed_ms: 11,
                        }],
                    ),
                    settle(
                        7,
                        "node:second",
                        "reservation:second",
                        "observation:second",
                        1,
                        vec![AppActiveInterval {
                            start_elapsed_ms: 6,
                            end_elapsed_ms: 16,
                        }],
                    ),
                ],
            ),
            &current,
        )
        .expect("valid journal");
        assert_eq!(assessment.active_elapsed_ms, 13);
    }

    #[test]
    fn parallel_reservation_windows_are_union_bounded_before_dispatch() {
        let mut active_ceiling = ceiling(100);
        active_ceiling.max_active_seconds = 1;
        let current = current_with(active_ceiling);
        let reservation = |sequence, id, start, end| AppResourceJournalEvent::Reserved {
            sequence,
            node_id: reference("node:root"),
            reservation_id: reference(id),
            operation_key: reference(&format!("operation:{id}")),
            requested: quantity(1),
            capability_requests: Vec::new(),
            at_elapsed_ms: start,
            expires_at_elapsed_ms: end,
        };
        let overlapping = assess_app_resource_journal(
            &journal(
                &current,
                500,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    reservation(2, "reservation:first", 1, 900),
                    reservation(3, "reservation:overlap", 500, 1_000),
                ],
            ),
            &current,
        )
        .expect("overlapping reservations share one active-time window");
        assert_eq!(overlapping.active_elapsed_ms(), 0);
        assert_eq!(overlapping.admitted_active_elapsed_ms(), 999);

        assert_eq!(
            assess_app_resource_journal(
                &journal(
                    &current,
                    700,
                    vec![
                        open_root(1, AppResourceExecutionLane::Foreground),
                        reservation(2, "reservation:first", 1, 700),
                        reservation(3, "reservation:disjoint", 700, 1_500),
                    ],
                ),
                &current,
            ),
            Err(AppResourceContractError::ReservationDenied(
                AppResourceBreach::ActiveTime
            ))
        );
    }

    #[test]
    fn committed_active_interval_cannot_exceed_its_reserved_window() {
        let current = current_with(ceiling(100));
        let reservation = AppResourceJournalEvent::Reserved {
            sequence: 2,
            node_id: reference("node:root"),
            reservation_id: reference("reservation:provider"),
            operation_key: reference("operation:provider"),
            requested: quantity(20),
            capability_requests: Vec::new(),
            at_elapsed_ms: 10,
            expires_at_elapsed_ms: 20,
        };
        let settlement = settle(
            3,
            "node:root",
            "reservation:provider",
            "observation:provider",
            20,
            vec![AppActiveInterval {
                start_elapsed_ms: 10,
                end_elapsed_ms: 21,
            }],
        );
        assert_eq!(
            assess_app_resource_journal(
                &journal(
                    &current,
                    21,
                    vec![
                        open_root(1, AppResourceExecutionLane::Foreground),
                        reservation,
                        settlement,
                    ],
                ),
                &current,
            ),
            Err(AppResourceContractError::InvalidActiveInterval)
        );
    }

    #[test]
    fn uncertain_expired_work_remains_reserved_and_blocks_settlement() {
        let current = current_with(ceiling(100));
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                70_000,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    reserve(2, "node:root", "reservation:external", 40),
                    AppResourceJournalEvent::Settled {
                        sequence: 3,
                        node_id: reference("node:root"),
                        reservation_id: reference("reservation:external"),
                        observation_id: reference("observation:uncertain"),
                        outcome: AppResourceSettlementOutcome::OutcomeUncertain,
                        observation_sources: vec![AppResourceObservationSource::CrashReconciler],
                        actual: AppResourceQuantity::default(),
                        capability_usage: Vec::new(),
                        active_intervals: Vec::new(),
                        effect_binding_digest: None,
                        effect_result: None,
                        at_elapsed_ms: 20,
                    },
                    AppResourceJournalEvent::NodeClosed {
                        sequence: 4,
                        node_id: reference("node:root"),
                        at_elapsed_ms: 30,
                    },
                ],
            ),
            &current,
        )
        .expect("uncertain outcome is retained, not a malformed journal");
        assert_eq!(assessment.outstanding_reserved.cost_microusd, 40);
        assert_eq!(assessment.uncertain_reservations, 1);
        assert!(assessment
            .breaches
            .contains(&AppResourceBreach::UnreconciledExpiredReservation));
        assert!(assessment
            .breaches
            .contains(&AppResourceBreach::OutcomeUncertain));
        assert!(!assessment.terminally_settled());
    }

    #[test]
    fn proven_unspent_requires_exclusive_crash_reconciler_evidence() {
        let current = current_with(ceiling(100));
        let settlement = |source| AppResourceJournalEvent::Settled {
            sequence: 3,
            node_id: reference("node:root"),
            reservation_id: reference("reservation:recovery"),
            observation_id: reference("observation:recovery"),
            outcome: AppResourceSettlementOutcome::ProvenUnspent,
            observation_sources: vec![source],
            actual: AppResourceQuantity::default(),
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms: 3,
        };
        let events = |source| {
            vec![
                open_root(1, AppResourceExecutionLane::Foreground),
                reserve(2, "node:root", "reservation:recovery", 40),
                settlement(source),
            ]
        };

        assert_eq!(
            assess_app_resource_journal(
                &journal(
                    &current,
                    4,
                    events(AppResourceObservationSource::LlmTaskLedger),
                ),
                &current,
            ),
            Err(AppResourceContractError::UntrustedProvenUnspentEvidence)
        );
        assert_eq!(
            assess_app_resource_journal(
                &journal(
                    &current,
                    4,
                    events(AppResourceObservationSource::CrashReconciler),
                ),
                &current,
            ),
            Err(AppResourceContractError::MissingTrustedRecoveryEvidence)
        );
        let recovery = TrustedAppResourceRecoveryEvidence::from_crash_reconciler(
            reference("reservation:recovery"),
            reference("observation:recovery"),
            reference("reconciliation:recovery"),
            revision(1),
            3,
        );
        let assessment = assess_app_resource_journal_with_recovery(
            &journal(
                &current,
                4,
                events(AppResourceObservationSource::CrashReconciler),
            ),
            &current,
            &[&recovery],
        )
        .expect("trusted recovery may release the reservation");
        assert_eq!(
            assessment.outstanding_reserved(),
            AppResourceQuantity::default()
        );
        static_assertions::assert_not_impl_any!(
            TrustedAppResourceRecoveryEvidence: serde::de::DeserializeOwned, Clone
        );
    }

    #[test]
    fn actual_over_reservation_fails_closed_before_it_can_cross_a_hard_ceiling() {
        let current = current_with(ceiling(100));
        assert_eq!(
            assess_app_resource_journal(
                &journal(
                    &current,
                    100,
                    vec![
                        open_root(1, AppResourceExecutionLane::Foreground),
                        reserve(2, "node:root", "reservation:provider", 50),
                        settle(
                            3,
                            "node:root",
                            "reservation:provider",
                            "observation:provider",
                            120,
                            Vec::new(),
                        ),
                    ],
                ),
                &current,
            ),
            Err(AppResourceContractError::SettlementExceedsReservation {
                field: "cost_microusd"
            })
        );
    }

    #[test]
    fn settlement_upper_bound_checks_every_additive_dimension() {
        let upper = AppResourceQuantity {
            input_tokens: 1,
            cached_input_tokens: 1,
            output_tokens: 1,
            cost_microusd: 1,
            paid_tool_invocations: 1,
            browser_network_actions: 1,
            records: 1,
            payload_bytes: 1,
            attachment_bytes: 1,
        };
        let cases = [
            (
                "input_tokens",
                AppResourceQuantity {
                    input_tokens: 2,
                    ..upper
                },
            ),
            (
                "cached_input_tokens",
                AppResourceQuantity {
                    cached_input_tokens: 2,
                    ..upper
                },
            ),
            (
                "output_tokens",
                AppResourceQuantity {
                    output_tokens: 2,
                    ..upper
                },
            ),
            (
                "cost_microusd",
                AppResourceQuantity {
                    cost_microusd: 2,
                    ..upper
                },
            ),
            (
                "paid_tool_invocations",
                AppResourceQuantity {
                    paid_tool_invocations: 2,
                    ..upper
                },
            ),
            (
                "browser_network_actions",
                AppResourceQuantity {
                    browser_network_actions: 2,
                    ..upper
                },
            ),
            (
                "records",
                AppResourceQuantity {
                    records: 2,
                    ..upper
                },
            ),
            (
                "payload_bytes",
                AppResourceQuantity {
                    payload_bytes: 2,
                    ..upper
                },
            ),
            (
                "attachment_bytes",
                AppResourceQuantity {
                    attachment_bytes: 2,
                    ..upper
                },
            ),
        ];
        for (field, actual) in cases {
            assert_eq!(actual.first_component_exceeding(upper), Some(field));
        }
        assert_eq!(upper.first_component_exceeding(upper), None);
    }

    #[test]
    fn settlement_interval_cannot_predate_its_reservation() {
        let current = current_with(ceiling(100));
        let reservation = AppResourceJournalEvent::Reserved {
            sequence: 2,
            node_id: reference("node:root"),
            reservation_id: reference("reservation:provider"),
            operation_key: reference("operation:provider"),
            requested: quantity(20),
            capability_requests: Vec::new(),
            at_elapsed_ms: 10,
            expires_at_elapsed_ms: 60_000,
        };
        let settlement = settle(
            3,
            "node:root",
            "reservation:provider",
            "observation:provider",
            20,
            vec![AppActiveInterval {
                start_elapsed_ms: 5,
                end_elapsed_ms: 12,
            }],
        );

        assert_eq!(
            assess_app_resource_journal(
                &journal(
                    &current,
                    100,
                    vec![
                        open_root(1, AppResourceExecutionLane::Foreground),
                        reservation,
                        settlement,
                    ],
                ),
                &current,
            ),
            Err(AppResourceContractError::InvalidActiveInterval)
        );
    }

    #[test]
    fn settlement_rejects_oversized_interval_batch_before_observation_capture() {
        let mut current = current_with(ceiling(100));
        current.policy.max_active_intervals = 2;
        let intervals = (0..3)
            .map(|offset| AppActiveInterval {
                start_elapsed_ms: 10 + offset * 2,
                end_elapsed_ms: 11 + offset * 2,
            })
            .collect();

        assert_eq!(
            assess_app_resource_journal(
                &journal(
                    &current,
                    100,
                    vec![
                        open_root(1, AppResourceExecutionLane::Foreground),
                        reserve(2, "node:root", "reservation:provider", 20),
                        settle(
                            3,
                            "node:root",
                            "reservation:provider",
                            "observation:provider",
                            20,
                            intervals,
                        ),
                    ],
                ),
                &current,
            ),
            Err(AppResourceContractError::ActiveIntervalLimitExceeded { limit: 2 })
        );
    }

    #[test]
    fn observation_replay_is_idempotent_but_rebinding_fails_closed() {
        let current = current_with(ceiling(100));
        let observation = settle(
            3,
            "node:root",
            "reservation:provider",
            "observation:provider",
            20,
            Vec::new(),
        );
        let mut duplicate = observation.clone();
        if let AppResourceJournalEvent::Settled { sequence, .. } = &mut duplicate {
            *sequence = 5;
        }
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    reserve(2, "node:root", "reservation:provider", 20),
                    observation,
                    AppResourceJournalEvent::ProgressObserved {
                        sequence: 4,
                        node_id: reference("node:root"),
                        progress_id: reference("progress:newer"),
                        at_elapsed_ms: 10,
                    },
                    duplicate,
                ],
            ),
            &current,
        )
        .expect("exact observation replay");
        assert_eq!(assessment.committed.cost_microusd, 20);

        let current = current_with(ceiling(100));
        let result = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    reserve(2, "node:root", "reservation:provider", 20),
                    settle(
                        3,
                        "node:root",
                        "reservation:provider",
                        "observation:provider",
                        20,
                        Vec::new(),
                    ),
                    settle(
                        4,
                        "node:root",
                        "reservation:provider",
                        "observation:provider",
                        19,
                        Vec::new(),
                    ),
                ],
            ),
            &current,
        );
        assert_eq!(
            result,
            Err(AppResourceContractError::ObservationRebound(
                "observation:provider".to_owned()
            ))
        );
    }

    #[test]
    fn reservation_replay_is_idempotent_and_does_not_double_hold() {
        let current = current_with(ceiling(100));
        let original = reserve(2, "node:root", "reservation:provider", 20);
        let mut duplicate = original.clone();
        if let AppResourceJournalEvent::Reserved { sequence, .. } = &mut duplicate {
            *sequence = 4;
        }
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    original,
                    AppResourceJournalEvent::ProgressObserved {
                        sequence: 3,
                        node_id: reference("node:root"),
                        progress_id: reference("progress:newer"),
                        at_elapsed_ms: 3,
                    },
                    duplicate,
                    settle(
                        5,
                        "node:root",
                        "reservation:provider",
                        "observation:provider",
                        20,
                        Vec::new(),
                    ),
                ],
            ),
            &current,
        )
        .expect("exact reservation replay");
        assert_eq!(assessment.committed.cost_microusd, 20);
        assert_eq!(assessment.outstanding_reserved.cost_microusd, 0);

        let current = current_with(ceiling(100));
        let result = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    reserve(2, "node:root", "reservation:provider", 20),
                    reserve(3, "node:root", "reservation:provider", 19),
                ],
            ),
            &current,
        );
        assert_eq!(
            result,
            Err(AppResourceContractError::DuplicateReservation(
                "reservation:provider".to_owned()
            ))
        );
    }

    #[test]
    fn closing_root_stops_lifetime_and_requires_closed_children() {
        let current = current_with(ceiling(100));
        let closed = assess_app_resource_journal(
            &journal(
                &current,
                50_000,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    AppResourceJournalEvent::NodeClosed {
                        sequence: 2,
                        node_id: reference("node:root"),
                        at_elapsed_ms: 1_000,
                    },
                ],
            ),
            &current,
        )
        .expect("closed root");
        assert_eq!(closed.lifetime_elapsed_ms, 1_000);
        assert!(closed.terminally_settled());

        let child_open = assess_app_resource_journal(
            &journal(
                &current,
                10,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    open_child(2, "node:child", AppResourceNodeKind::DelegatedChild),
                    AppResourceJournalEvent::NodeClosed {
                        sequence: 3,
                        node_id: reference("node:root"),
                        at_elapsed_ms: 3,
                    },
                ],
            ),
            &current,
        );
        assert_eq!(
            child_open,
            Err(AppResourceContractError::NodeHasOpenChildren(
                "node:root".to_owned()
            ))
        );
    }

    #[test]
    fn spawning_retry_nodes_does_not_reset_the_no_progress_breaker() {
        let mut current = current_with(ceiling(100));
        current.policy.max_no_progress_seconds = 3;
        let result = assess_app_resource_journal(
            &journal(
                &current,
                5_000,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    AppResourceJournalEvent::NodeOpened {
                        sequence: 2,
                        node_id: reference("node:retry-one"),
                        execution_ref: reference("execution:retry-one"),
                        parent_node_id: Some(reference("node:root")),
                        node_kind: AppResourceNodeKind::Retry,
                        lane: AppResourceExecutionLane::Foreground,
                        at_elapsed_ms: 2_000,
                    },
                    AppResourceJournalEvent::NodeOpened {
                        sequence: 3,
                        node_id: reference("node:retry-two"),
                        execution_ref: reference("execution:retry-two"),
                        parent_node_id: Some(reference("node:root")),
                        node_kind: AppResourceNodeKind::Retry,
                        lane: AppResourceExecutionLane::Foreground,
                        at_elapsed_ms: 4_000,
                    },
                    AppResourceJournalEvent::Reserved {
                        sequence: 4,
                        node_id: reference("node:retry-two"),
                        reservation_id: reference("reservation:late"),
                        operation_key: reference("operation:late"),
                        requested: quantity(1),
                        capability_requests: Vec::new(),
                        at_elapsed_ms: 4_001,
                        expires_at_elapsed_ms: 60_000,
                    },
                ],
            ),
            &current,
        );
        assert_eq!(
            result,
            Err(AppResourceContractError::ReservationDenied(
                AppResourceBreach::NoProgress
            ))
        );
    }

    #[test]
    fn live_reservation_is_governed_by_its_operation_expiry_not_the_idle_watchdog() {
        let current = current_with(ceiling(100));
        let events = vec![
            open_root(1, AppResourceExecutionLane::Foreground),
            reserve(2, "node:root", "reservation:slow-provider", 1),
        ];

        let live =
            assess_app_resource_journal(&journal(&current, 40_000, events.clone()), &current)
                .expect("a bounded live operation may outlive the shorter idle watchdog");
        assert_eq!(live.held_reservations(), 1);
        assert!(!live.breaches().contains(&AppResourceBreach::NoProgress));
        assert!(!live
            .breaches()
            .contains(&AppResourceBreach::UnreconciledExpiredReservation));
        assert_eq!(
            evaluate_app_resource_reservation(&live, quantity(1), &[], 40_001, 59_999, &current,),
            Ok(()),
            "a sibling operation may advance while the first bounded operation is live"
        );

        let expired = assess_app_resource_journal(&journal(&current, 60_000, events), &current)
            .expect("an expired reservation remains assessable for recovery");
        assert!(expired.breaches().contains(&AppResourceBreach::NoProgress));
        assert!(expired
            .breaches()
            .contains(&AppResourceBreach::UnreconciledExpiredReservation));
    }

    #[test]
    fn committed_long_running_operation_atomically_records_progress() {
        let current = current_with(ceiling(100));
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                40_000,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    reserve(2, "node:root", "reservation:slow-provider", 1),
                    settle(
                        3,
                        "node:root",
                        "reservation:slow-provider",
                        "observation:slow-provider",
                        1,
                        vec![AppActiveInterval {
                            start_elapsed_ms: 2,
                            end_elapsed_ms: 40_000,
                        }],
                    ),
                ],
            ),
            &current,
        )
        .expect("a successful bounded provider call is forward progress");

        assert_eq!(assessment.last_progress_elapsed_ms(), 40_000);
        assert_eq!(assessment.held_reservations(), 0);
        assert!(!assessment
            .breaches()
            .contains(&AppResourceBreach::NoProgress));
    }

    #[test]
    fn uncertain_long_running_operation_does_not_manufacture_progress() {
        let current = current_with(ceiling(100));
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                40_000,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    reserve(2, "node:root", "reservation:uncertain-provider", 1),
                    AppResourceJournalEvent::Settled {
                        sequence: 3,
                        node_id: reference("node:root"),
                        reservation_id: reference("reservation:uncertain-provider"),
                        observation_id: reference("observation:uncertain-provider"),
                        outcome: AppResourceSettlementOutcome::OutcomeUncertain,
                        observation_sources: vec![AppResourceObservationSource::LlmTaskLedger],
                        actual: AppResourceQuantity::default(),
                        capability_usage: Vec::new(),
                        active_intervals: Vec::new(),
                        effect_binding_digest: None,
                        effect_result: None,
                        at_elapsed_ms: 40_000,
                    },
                ],
            ),
            &current,
        )
        .expect("uncertain outcomes remain assessable for recovery");

        assert_eq!(assessment.last_progress_elapsed_ms(), 0);
        assert!(assessment
            .breaches()
            .contains(&AppResourceBreach::NoProgress));
        assert!(assessment
            .breaches()
            .contains(&AppResourceBreach::OutcomeUncertain));
    }

    #[test]
    fn quantity_overflow_fails_closed_instead_of_wrapping() {
        let mut broad = ceiling(100);
        broad.max_cost_microusd = u64::MAX;
        broad.max_monthly_cost_microusd = u64::MAX;
        let current = current_with(broad);
        let result = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    reserve(2, "node:root", "reservation:max", u64::MAX),
                    settle(
                        3,
                        "node:root",
                        "reservation:max",
                        "observation:max",
                        u64::MAX,
                        Vec::new(),
                    ),
                    reserve(4, "node:root", "reservation:one", 1),
                    settle(
                        5,
                        "node:root",
                        "reservation:one",
                        "observation:one",
                        1,
                        Vec::new(),
                    ),
                ],
            ),
            &current,
        );
        assert_eq!(
            result,
            Err(AppResourceContractError::ArithmeticOverflow(
                "cost_microusd"
            ))
        );
    }

    #[test]
    fn hostile_or_malformed_journal_json_is_rejected_before_replay() {
        assert!(decode_app_resource_journal(br#"{"unexpected":true}"#).is_err());

        let deep = format!("{}0{}", "[".repeat(40), "]".repeat(40));
        assert!(matches!(
            decode_app_resource_journal(deep.as_bytes()),
            Err(AppContractError::JsonDepthExceeded { .. })
        ));
    }

    #[test]
    fn topology_is_flat_bounded_and_parent_before_child() {
        let mut current = current_with(ceiling(100));
        current.policy.max_tree_depth = 1;
        let events = vec![
            open_root(1, AppResourceExecutionLane::Foreground),
            open_child(2, "node:child", AppResourceNodeKind::DelegatedChild),
            AppResourceJournalEvent::NodeOpened {
                sequence: 3,
                node_id: reference("node:grandchild"),
                execution_ref: reference("execution:grandchild"),
                parent_node_id: Some(reference("node:child")),
                node_kind: AppResourceNodeKind::DelegatedChild,
                lane: AppResourceExecutionLane::Foreground,
                at_elapsed_ms: 3,
            },
        ];
        assert_eq!(
            assess_app_resource_journal(&journal(&current, 100, events), &current),
            Err(AppResourceContractError::TreeDepthExceeded { limit: 1 })
        );
    }

    #[test]
    fn distinct_nodes_cannot_alias_one_execution_reference() {
        let current = current_with(ceiling(100));
        assert_eq!(
            assess_app_resource_journal(
                &journal(
                    &current,
                    100,
                    vec![
                        open_root(1, AppResourceExecutionLane::Foreground),
                        AppResourceJournalEvent::NodeOpened {
                            sequence: 2,
                            node_id: reference("node:alias"),
                            execution_ref: reference("execution:root"),
                            parent_node_id: Some(reference("node:root")),
                            node_kind: AppResourceNodeKind::DelegatedChild,
                            lane: AppResourceExecutionLane::Foreground,
                            at_elapsed_ms: 2,
                        },
                    ],
                ),
                &current,
            ),
            Err(AppResourceContractError::DuplicateExecutionRef(
                "execution:root".to_owned()
            ))
        );
    }

    #[test]
    fn stale_authority_or_ledger_identity_cannot_replay() {
        let current = current_with(ceiling(100));
        let mut stored = current.identity.clone();
        stored.budget_ledger_ref = reference("ledger:forged");
        let journal = AppResourceJournal {
            identity: stored,
            evaluated_at_elapsed_ms: 1,
            events: vec![open_root(1, AppResourceExecutionLane::Foreground)],
        };
        assert_eq!(
            assess_app_resource_journal(&journal, &current),
            Err(AppResourceContractError::IdentityMismatch(
                "budget_ledger_ref"
            ))
        );
    }

    #[test]
    fn background_admission_preserves_foreground_scheduler_slots() {
        let mut current = current_with(ceiling(100));
        current.baseline.scheduler_foreground_runs_excluding_root = 0;
        current.baseline.scheduler_background_runs_excluding_root = 6;
        assert_eq!(
            evaluate_app_resource_root_admission(&current, AppResourceExecutionLane::Background),
            Err(AppResourceContractError::ForegroundReserveDenied)
        );
    }

    #[test]
    fn foreground_occupancy_does_not_shrink_the_background_quota_twice() {
        let mut current = current_with(ceiling(100));
        current.baseline.scheduler_foreground_runs_excluding_root = 3;
        current.baseline.scheduler_background_runs_excluding_root = 3;
        assert_eq!(
            evaluate_app_resource_root_admission(&current, AppResourceExecutionLane::Background),
            Ok(())
        );
    }

    #[test]
    fn historical_replay_does_not_readmit_a_root_against_current_concurrency() {
        let mut current = current_with(ceiling(100));
        current.baseline.installation_foreground_runs_excluding_root =
            current.ceiling.max_concurrent_foreground_runs;

        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![open_root(1, AppResourceExecutionLane::Foreground)],
            ),
            &current,
        )
        .expect("historically admitted root remains replayable");
        assert!(assessment.breaches().is_empty());
        assert_eq!(
            evaluate_app_resource_root_admission(&current, AppResourceExecutionLane::Foreground),
            Err(AppResourceContractError::ForegroundConcurrencyDenied)
        );
    }

    #[test]
    fn replay_reports_all_snapshot_breaches_and_refresh_can_clear_them() {
        let mut current = current_with(ceiling(100));
        current.baseline.tokens_excluding_root = 190;
        current.baseline.cost_microusd_excluding_root = 90;
        let usage = AppResourceQuantity {
            input_tokens: 20,
            cost_microusd: 20,
            ..AppResourceQuantity::default()
        };
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    AppResourceJournalEvent::Reserved {
                        sequence: 2,
                        node_id: reference("node:root"),
                        reservation_id: reference("reservation:period"),
                        operation_key: reference("operation:period"),
                        requested: usage,
                        capability_requests: Vec::new(),
                        at_elapsed_ms: 2,
                        expires_at_elapsed_ms: 60_000,
                    },
                    AppResourceJournalEvent::Settled {
                        sequence: 3,
                        node_id: reference("node:root"),
                        reservation_id: reference("reservation:period"),
                        observation_id: reference("observation:period"),
                        outcome: AppResourceSettlementOutcome::Committed,
                        observation_sources: vec![
                            AppResourceObservationSource::ExecutionTokenMeter,
                            AppResourceObservationSource::LlmTaskLedger,
                        ],
                        actual: usage,
                        capability_usage: Vec::new(),
                        active_intervals: Vec::new(),
                        effect_binding_digest: None,
                        effect_result: None,
                        at_elapsed_ms: 3,
                    },
                ],
            ),
            &current,
        )
        .expect("snapshot overage is reported rather than making replay impossible");
        assert_eq!(
            assessment.breaches(),
            &BTreeSet::from([
                AppResourceBreach::MonthlyCost,
                AppResourceBreach::MonthlyTokens,
            ])
        );

        current.baseline.period_revision = revision(2);
        current.baseline.tokens_excluding_root = 0;
        current.baseline.cost_microusd_excluding_root = 0;
        assert_eq!(
            evaluate_app_resource_reservation(&assessment, quantity(1), &[], 101, 60_000, &current,),
            Ok(())
        );
    }

    #[test]
    fn reservation_evaluation_rejects_older_period_revisions_and_time_travel() {
        let mut current = current_with(ceiling(100));
        current.baseline.period_revision = revision(2);
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![open_root(1, AppResourceExecutionLane::Foreground)],
            ),
            &current,
        )
        .expect("current root assessment");

        assert_eq!(
            evaluate_app_resource_reservation(&assessment, quantity(1), &[], 99, 60_000, &current,),
            Err(AppResourceContractError::EventTimeRegression)
        );

        current.baseline.period_revision = revision(1);
        assert_eq!(
            evaluate_app_resource_reservation(&assessment, quantity(1), &[], 101, 60_000, &current,),
            Err(AppResourceContractError::StalePeriodRevision)
        );
    }

    #[test]
    fn installation_period_tokens_include_other_trees_and_outstanding_work() {
        let mut current = current_with(ceiling(100));
        current.baseline.tokens_excluding_root = 180;
        current.baseline.outstanding_tokens_excluding_root = 10;
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![open_root(1, AppResourceExecutionLane::Foreground)],
            ),
            &current,
        )
        .expect("current root assessment");
        let result = evaluate_app_resource_reservation(
            &assessment,
            AppResourceQuantity {
                input_tokens: 11,
                ..AppResourceQuantity::default()
            },
            &[],
            101,
            60_000,
            &current,
        );
        assert_eq!(
            result,
            Err(AppResourceContractError::ReservationDenied(
                AppResourceBreach::MonthlyTokens
            ))
        );

        let candidate = assess_app_resource_journal(
            &journal(
                &current,
                101,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    AppResourceJournalEvent::Reserved {
                        sequence: 2,
                        node_id: reference("node:root"),
                        reservation_id: reference("reservation:period-candidate"),
                        operation_key: reference("operation:period-candidate"),
                        requested: AppResourceQuantity {
                            input_tokens: 11,
                            ..AppResourceQuantity::default()
                        },
                        capability_requests: Vec::new(),
                        at_elapsed_ms: 101,
                        expires_at_elapsed_ms: 60_000,
                    },
                ],
            ),
            &current,
        )
        .expect("historical replay reports rather than rejects external period totals");
        assert_eq!(
            evaluate_app_resource_candidate_period(&candidate, &current),
            Err(AppResourceContractError::ReservationDenied(
                AppResourceBreach::MonthlyTokens
            ))
        );
    }

    #[test]
    fn cached_input_is_a_subset_not_a_second_monthly_token_charge() {
        let mut current = current_with(ceiling(100));
        current.baseline.tokens_excluding_root = 100;
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![open_root(1, AppResourceExecutionLane::Foreground)],
            ),
            &current,
        )
        .expect("current root assessment");
        assert_eq!(
            evaluate_app_resource_reservation(
                &assessment,
                AppResourceQuantity {
                    input_tokens: 70,
                    cached_input_tokens: 60,
                    output_tokens: 20,
                    ..AppResourceQuantity::default()
                },
                &[],
                101,
                60_000,
                &current,
            ),
            Ok(())
        );
    }

    #[test]
    fn capability_breakdown_is_exact_and_aggregated_by_family() {
        let mut current = current_with(ceiling(100));
        current.ceiling.max_paid_tool_invocations = 5;
        let requested = AppResourceQuantity {
            cost_microusd: 20,
            paid_tool_invocations: 2,
            ..AppResourceQuantity::default()
        };
        let family = AppCapabilityResourceQuantity {
            capability_family: name("browser"),
            paid_invocations: 2,
            cost_microusd: 10,
        };
        let assessment = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    AppResourceJournalEvent::Reserved {
                        sequence: 2,
                        node_id: reference("node:root"),
                        reservation_id: reference("reservation:tool"),
                        operation_key: reference("operation:tool"),
                        requested,
                        capability_requests: vec![family.clone()],
                        at_elapsed_ms: 2,
                        expires_at_elapsed_ms: 60_000,
                    },
                    AppResourceJournalEvent::Settled {
                        sequence: 3,
                        node_id: reference("node:root"),
                        reservation_id: reference("reservation:tool"),
                        observation_id: reference("observation:tool"),
                        outcome: AppResourceSettlementOutcome::Committed,
                        observation_sources: vec![AppResourceObservationSource::ToolRuntime],
                        actual: requested,
                        capability_usage: vec![family],
                        active_intervals: Vec::new(),
                        effect_binding_digest: None,
                        effect_result: None,
                        at_elapsed_ms: 20,
                    },
                ],
            ),
            &current,
        )
        .expect("valid capability accounting");
        assert_eq!(
            assessment.capability_usage[&name("browser")].paid_invocations,
            2
        );
    }

    #[test]
    fn capability_settlement_cannot_spend_another_family_or_exceed_its_reservation() {
        let mut current = current_with(ceiling(100));
        current.ceiling.max_paid_tool_invocations = 5;
        let requested = AppResourceQuantity {
            cost_microusd: 20,
            paid_tool_invocations: 2,
            ..AppResourceQuantity::default()
        };
        let reserved_family = AppCapabilityResourceQuantity {
            capability_family: name("browser"),
            paid_invocations: 2,
            cost_microusd: 10,
        };
        let observed_family = AppCapabilityResourceQuantity {
            capability_family: name("mail"),
            paid_invocations: 2,
            cost_microusd: 10,
        };
        let result = assess_app_resource_journal(
            &journal(
                &current,
                100,
                vec![
                    open_root(1, AppResourceExecutionLane::Foreground),
                    AppResourceJournalEvent::Reserved {
                        sequence: 2,
                        node_id: reference("node:root"),
                        reservation_id: reference("reservation:tool-family"),
                        operation_key: reference("operation:tool-family"),
                        requested,
                        capability_requests: vec![reserved_family],
                        at_elapsed_ms: 2,
                        expires_at_elapsed_ms: 60_000,
                    },
                    AppResourceJournalEvent::Settled {
                        sequence: 3,
                        node_id: reference("node:root"),
                        reservation_id: reference("reservation:tool-family"),
                        observation_id: reference("observation:tool-family"),
                        outcome: AppResourceSettlementOutcome::Committed,
                        observation_sources: vec![AppResourceObservationSource::ToolRuntime],
                        actual: requested,
                        capability_usage: vec![observed_family],
                        active_intervals: Vec::new(),
                        effect_binding_digest: None,
                        effect_result: None,
                        at_elapsed_ms: 20,
                    },
                ],
            ),
            &current,
        );
        assert_eq!(
            result,
            Err(
                AppResourceContractError::CapabilitySettlementExceedsReservation("mail".to_owned())
            )
        );
    }

    #[test]
    fn capability_family_limit_applies_to_the_complete_tree() {
        let mut current = current_with(ceiling(100));
        current.policy.max_capability_families = 1;
        current.ceiling.max_paid_tool_invocations = 2;
        let requested = AppResourceQuantity {
            cost_microusd: 1,
            paid_tool_invocations: 1,
            ..AppResourceQuantity::default()
        };
        let capability = |family: &str| AppCapabilityResourceQuantity {
            capability_family: name(family),
            paid_invocations: 1,
            cost_microusd: 1,
        };
        let reservation =
            |sequence: u64, id: &str, family: &str| AppResourceJournalEvent::Reserved {
                sequence,
                node_id: reference("node:root"),
                reservation_id: reference(&format!("reservation:{id}")),
                operation_key: reference(&format!("operation:{id}")),
                requested,
                capability_requests: vec![capability(family)],
                at_elapsed_ms: sequence,
                expires_at_elapsed_ms: 60_000,
            };
        let settlement = |sequence: u64, id: &str, family: &str| AppResourceJournalEvent::Settled {
            sequence,
            node_id: reference("node:root"),
            reservation_id: reference(&format!("reservation:{id}")),
            observation_id: reference(&format!("observation:{id}")),
            outcome: AppResourceSettlementOutcome::Committed,
            observation_sources: vec![AppResourceObservationSource::ToolRuntime],
            actual: requested,
            capability_usage: vec![capability(family)],
            active_intervals: Vec::new(),
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms: sequence,
        };
        assert_eq!(
            assess_app_resource_journal(
                &journal(
                    &current,
                    100,
                    vec![
                        open_root(1, AppResourceExecutionLane::Foreground),
                        reservation(2, "browser", "browser"),
                        settlement(3, "browser", "browser"),
                        reservation(4, "mail", "mail"),
                        settlement(5, "mail", "mail"),
                    ],
                ),
                &current,
            ),
            Err(AppResourceContractError::CapabilityFamilyLimitExceeded { limit: 1 })
        );
    }
}
