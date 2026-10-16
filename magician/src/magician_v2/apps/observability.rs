//! Payload-minimal observability vocabulary for the Apps platform.
//!
//! This module is the only Apps lifecycle-trace emitter. Its builder accepts
//! closed enums and bounded contract types, so callers cannot attach payloads,
//! raw errors, credentials, principal/workspace names, provider/device details
//! or task/execution internals. High-cardinality identifiers and digests are
//! trace fields only; the module deliberately exposes no metrics API.

use magician_app_contract::AppPublicOperationId;
use tracing::Level;

use super::models::{AppDigest, AppName, AppReference};

/// Stable Apps trace target used by filtering and conformance tests.
pub const APP_TRACE_TARGET: &str = "magician.apps";

/// Exact attributes that the shared emitter can write. All are part of the
/// normative truth-baseline vocabulary.
pub const APP_TRACE_ATTRIBUTE_KEYS: [&str; 14] = [
    "app.operation",
    "app.outcome",
    "app.correlation_id",
    "app.causation_id",
    "app.action_id",
    "app.workflow_id",
    "app.run_ref",
    "app.child_id",
    "app.mapping_digest",
    "app.resource_permit_ref",
    "app.receipt_kind",
    "app.receipt_ref",
    "app.retry_class",
    "app.effect_state",
];

/// The normative bounded stage vocabulary from the Apps truth baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTraceStage {
    Request,
    Resolve,
    Policy,
    Admit,
    Execute,
    Effect,
    Settle,
    Publish,
    Reconcile,
}

impl AppTraceStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "app.request",
            Self::Resolve => "app.resolve",
            Self::Policy => "app.policy",
            Self::Admit => "app.admit",
            Self::Execute => "app.execute",
            Self::Effect => "app.effect",
            Self::Settle => "app.settle",
            Self::Publish => "app.publish",
            Self::Reconcile => "app.reconcile",
        }
    }
}

/// Stable operation vocabulary shared by the public edge and runtime owners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTraceOperation {
    ContractCapabilities,
    QueryData,
    MutateData,
    LaunchAction,
    GetActionRun,
    ComposeActionRun,
    CancelActionRun,
    ReadEntityChanges,
    WorkflowLaunch,
    WorkflowRecovery,
    WorkflowTerminal,
    RecordComposition,
    ActionResultComposition,
    EffectSettlement,
    WorkerReconciliation,
}

impl AppTraceOperation {
    /// Exhaustive bridge from the canonical supported-public inventory. A new
    /// public operation cannot compile until its observability name is fixed.
    pub const fn from_supported_public(operation: AppPublicOperationId) -> Self {
        match operation {
            AppPublicOperationId::ContractCapabilities => Self::ContractCapabilities,
            AppPublicOperationId::QueryData => Self::QueryData,
            AppPublicOperationId::MutateData => Self::MutateData,
            AppPublicOperationId::LaunchAction => Self::LaunchAction,
            AppPublicOperationId::GetActionRun => Self::GetActionRun,
            AppPublicOperationId::ComposeActionRun => Self::ComposeActionRun,
            AppPublicOperationId::CancelActionRun => Self::CancelActionRun,
            AppPublicOperationId::ReadEntityChanges => Self::ReadEntityChanges,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContractCapabilities => "contract_capabilities",
            Self::QueryData => "query_data",
            Self::MutateData => "mutate_data",
            Self::LaunchAction => "launch_action",
            Self::GetActionRun => "get_action_run",
            Self::ComposeActionRun => "compose_action_run",
            Self::CancelActionRun => "cancel_action_run",
            Self::ReadEntityChanges => "read_entity_changes",
            Self::WorkflowLaunch => "workflow_launch",
            Self::WorkflowRecovery => "workflow_recovery",
            Self::WorkflowTerminal => "workflow_terminal",
            Self::RecordComposition => "record_composition",
            Self::ActionResultComposition => "action_result_composition",
            Self::EffectSettlement => "effect_settlement",
            Self::WorkerReconciliation => "worker_reconciliation",
        }
    }
}

/// Terminal decision/status values. These are suitable for bounded metric
/// dimensions, although this module emits traces only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTraceOutcome {
    Allowed,
    Denied,
    Completed,
    Failed,
    Cancelled,
    Uncertain,
    Unavailable,
}

impl AppTraceOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Uncertain => "uncertain",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTraceEffectState {
    NotStarted,
    Committed,
    SettledNoEffect,
    Uncertain,
}

impl AppTraceEffectState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Committed => "committed",
            Self::SettledNoEffect => "settled_no_effect",
            Self::Uncertain => "uncertain",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTraceRetryClass {
    None,
    SameInput,
    Refresh,
    Reauthorize,
    UserAction,
    ReconcileUncertain,
}

impl AppTraceRetryClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SameInput => "same_input",
            Self::Refresh => "refresh",
            Self::Reauthorize => "reauthorize",
            Self::UserAction => "user_action",
            Self::ReconcileUncertain => "reconcile_uncertain",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTraceReceiptKind {
    Effect,
    Transfer,
    Mutation,
}

impl AppTraceReceiptKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Effect => "effect",
            Self::Transfer => "transfer",
            Self::Mutation => "mutation",
        }
    }
}

/// One payload-minimal event. All fields are private; builders intentionally
/// accept only bounded app contract types and closed vocabularies.
#[derive(Debug, Clone, Copy)]
pub struct AppTraceEvent<'a> {
    stage: AppTraceStage,
    operation: AppTraceOperation,
    outcome: Option<AppTraceOutcome>,
    correlation_id: Option<&'a AppReference>,
    causation_id: Option<&'a AppReference>,
    action_id: Option<&'a AppName>,
    workflow_id: Option<&'a AppName>,
    run_ref: Option<&'a AppReference>,
    child_id: Option<&'a AppReference>,
    mapping_digest: Option<&'a AppDigest>,
    resource_permit_ref: Option<&'a AppReference>,
    receipt_kind: Option<AppTraceReceiptKind>,
    receipt_ref: Option<&'a AppReference>,
    retry_class: Option<AppTraceRetryClass>,
    effect_state: Option<AppTraceEffectState>,
}

impl<'a> AppTraceEvent<'a> {
    pub const fn new(stage: AppTraceStage, operation: AppTraceOperation) -> Self {
        Self {
            stage,
            operation,
            outcome: None,
            correlation_id: None,
            causation_id: None,
            action_id: None,
            workflow_id: None,
            run_ref: None,
            child_id: None,
            mapping_digest: None,
            resource_permit_ref: None,
            receipt_kind: None,
            receipt_ref: None,
            retry_class: None,
            effect_state: None,
        }
    }

    pub const fn outcome(mut self, outcome: AppTraceOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    pub const fn correlation_id(mut self, correlation_id: &'a AppReference) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    pub const fn causation_id(mut self, causation_id: &'a AppReference) -> Self {
        self.causation_id = Some(causation_id);
        self
    }

    pub const fn action_id(mut self, action_id: &'a AppName) -> Self {
        self.action_id = Some(action_id);
        self
    }

    pub const fn workflow_id(mut self, workflow_id: &'a AppName) -> Self {
        self.workflow_id = Some(workflow_id);
        self
    }

    pub const fn run_ref(mut self, run_ref: &'a AppReference) -> Self {
        self.run_ref = Some(run_ref);
        self
    }

    pub const fn child_id(mut self, child_id: &'a AppReference) -> Self {
        self.child_id = Some(child_id);
        self
    }

    pub const fn mapping_digest(mut self, mapping_digest: &'a AppDigest) -> Self {
        self.mapping_digest = Some(mapping_digest);
        self
    }

    pub const fn resource_permit_ref(mut self, resource_permit_ref: &'a AppReference) -> Self {
        self.resource_permit_ref = Some(resource_permit_ref);
        self
    }

    pub const fn receipt(
        mut self,
        receipt_kind: AppTraceReceiptKind,
        receipt_ref: &'a AppReference,
    ) -> Self {
        self.receipt_kind = Some(receipt_kind);
        self.receipt_ref = Some(receipt_ref);
        self
    }

    pub const fn retry_class(mut self, retry_class: AppTraceRetryClass) -> Self {
        self.retry_class = Some(retry_class);
        self
    }

    pub const fn effect_state(mut self, effect_state: AppTraceEffectState) -> Self {
        self.effect_state = Some(effect_state);
        self
    }

    /// Emit one fixed-name event. Empty optional fields mean that the boundary
    /// did not own that correlation fact; they never stand in for redacted
    /// payloads.
    pub fn emit(self) {
        // **The happy path of the highest-volume stage is DEBUG.** One
        // `effect_settlement … completed` line is emitted per effect, and an
        // ambient app round settles one effect per rostered member — 143 INFO
        // lines every five minutes from one app, 41% of the runtime log. A
        // settlement that failed, was denied or cancelled, and every other
        // stage (launch, admit, publish, terminal) stays at INFO, so a run is
        // still traceable end to end without the per-member chorus.
        let routine_settlement = matches!(self.stage, AppTraceStage::Settle)
            && self.operation.as_str() == "effect_settlement"
            && matches!(self.outcome, Some(AppTraceOutcome::Completed));
        macro_rules! emit_stage {
            ($stage:literal) => {{
                if routine_settlement {
                    emit_stage!(@level Level::DEBUG, $stage)
                } else {
                    emit_stage!(@level Level::INFO, $stage)
                }
            }};
            (@level $level:expr, $stage:literal) => {{
                tracing::event!(
                    target: APP_TRACE_TARGET,
                    $level,
                    app.operation = self.operation.as_str(),
                    app.outcome = self.outcome.map(AppTraceOutcome::as_str).unwrap_or(""),
                    app.correlation_id = self
                        .correlation_id
                        .map(AppReference::as_str)
                        .unwrap_or(""),
                    app.causation_id = self
                        .causation_id
                        .map(AppReference::as_str)
                        .unwrap_or(""),
                    app.action_id = self.action_id.map(AppName::as_str).unwrap_or(""),
                    app.workflow_id = self.workflow_id.map(AppName::as_str).unwrap_or(""),
                    app.run_ref = self.run_ref.map(AppReference::as_str).unwrap_or(""),
                    app.child_id = self.child_id.map(AppReference::as_str).unwrap_or(""),
                    app.mapping_digest = self
                        .mapping_digest
                        .map(AppDigest::as_str)
                        .unwrap_or(""),
                    app.resource_permit_ref = self
                        .resource_permit_ref
                        .map(AppReference::as_str)
                        .unwrap_or(""),
                    app.receipt_kind = self
                        .receipt_kind
                        .map(AppTraceReceiptKind::as_str)
                        .unwrap_or(""),
                    app.receipt_ref = self.receipt_ref.map(AppReference::as_str).unwrap_or(""),
                    app.retry_class = self
                        .retry_class
                        .map(AppTraceRetryClass::as_str)
                        .unwrap_or(""),
                    app.effect_state = self
                        .effect_state
                        .map(AppTraceEffectState::as_str)
                        .unwrap_or(""),
                    $stage
                );
            }};
        }
        match self.stage {
            AppTraceStage::Request => emit_stage!("app.request"),
            AppTraceStage::Resolve => emit_stage!("app.resolve"),
            AppTraceStage::Policy => emit_stage!("app.policy"),
            AppTraceStage::Admit => emit_stage!("app.admit"),
            AppTraceStage::Execute => emit_stage!("app.execute"),
            AppTraceStage::Effect => emit_stage!("app.effect"),
            AppTraceStage::Settle => emit_stage!("app.settle"),
            AppTraceStage::Publish => emit_stage!("app.publish"),
            AppTraceStage::Reconcile => emit_stage!("app.reconcile"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_and_outcome_vocabularies_match_the_truth_baseline() {
        assert_eq!(
            [
                AppTraceStage::Request,
                AppTraceStage::Resolve,
                AppTraceStage::Policy,
                AppTraceStage::Admit,
                AppTraceStage::Execute,
                AppTraceStage::Effect,
                AppTraceStage::Settle,
                AppTraceStage::Publish,
                AppTraceStage::Reconcile,
            ]
            .map(AppTraceStage::as_str),
            [
                "app.request",
                "app.resolve",
                "app.policy",
                "app.admit",
                "app.execute",
                "app.effect",
                "app.settle",
                "app.publish",
                "app.reconcile",
            ]
        );
        assert_eq!(
            [
                AppTraceOutcome::Allowed,
                AppTraceOutcome::Denied,
                AppTraceOutcome::Completed,
                AppTraceOutcome::Failed,
                AppTraceOutcome::Cancelled,
                AppTraceOutcome::Uncertain,
                AppTraceOutcome::Unavailable,
            ]
            .map(AppTraceOutcome::as_str),
            [
                "allowed",
                "denied",
                "completed",
                "failed",
                "cancelled",
                "uncertain",
                "unavailable",
            ]
        );
    }

    #[test]
    fn supported_public_operation_names_are_closed_and_canonical() {
        assert_eq!(
            [
                AppTraceOperation::ContractCapabilities,
                AppTraceOperation::QueryData,
                AppTraceOperation::MutateData,
                AppTraceOperation::LaunchAction,
                AppTraceOperation::GetActionRun,
                AppTraceOperation::ComposeActionRun,
                AppTraceOperation::CancelActionRun,
                AppTraceOperation::ReadEntityChanges,
            ]
            .map(AppTraceOperation::as_str),
            [
                "contract_capabilities",
                "query_data",
                "mutate_data",
                "launch_action",
                "get_action_run",
                "compose_action_run",
                "cancel_action_run",
                "read_entity_changes",
            ]
        );
        for operation in magician_app_contract::SUPPORTED_PUBLIC_APP_OPERATIONS {
            assert_eq!(
                AppTraceOperation::from_supported_public(operation.id).as_str(),
                operation.id.as_str()
            );
        }
    }

    #[test]
    fn emitter_attribute_keys_are_the_payload_minimal_allowlist_subset() {
        assert_eq!(APP_TRACE_ATTRIBUTE_KEYS.len(), 14);
        for key in APP_TRACE_ATTRIBUTE_KEYS {
            assert!(key.starts_with("app."));
            assert!(!key.contains("payload"));
            assert!(!key.contains("error_message"));
            assert!(!key.contains("principal"));
            assert!(!key.contains("workspace"));
            assert!(!key.contains("provider"));
            assert!(!key.contains("device"));
            assert!(!key.contains("task_id"));
            assert!(!key.contains("execution_id"));
        }
    }
}
