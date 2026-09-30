//! Sanitized JSON reports for the operator workflow.

use serde::Serialize;

use super::preconditions::{GateStatus, PreconditionReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationOperation {
    Inventory,
    Status,
    Plan,
    Export,
    Import,
    Verify,
    Checkpoint,
    Resume,
    Cancel,
    Cutover,
    Rollback,
}

#[derive(Debug, Clone, Serialize)]
pub struct QualifiedOperation {
    pub operation: ActivationOperation,
    pub qualified: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivationReport {
    pub ok: bool,
    pub operation: ActivationOperation,
    pub dry_run: bool,
    pub principal: String,
    pub workspace: String,
    pub source_profile: String,
    pub target_profile: String,
    pub gates: GateStatus,
    pub qualified: Vec<QualifiedOperation>,
    pub blocking: Vec<String>,
    pub migration_id: Option<String>,
    pub phase: Option<String>,
    pub local_canonical: bool,
    pub backend_unavailable: bool,
    pub detail: serde_json::Value,
}

impl ActivationReport {
    pub fn from_preconditions(
        operation: ActivationOperation,
        principal: &str,
        workspace: &str,
        pre: &PreconditionReport,
        source: &str,
        target: &str,
    ) -> Self {
        let inventory_ok = pre.tier1_remote_ready && pre.tier2_matrix_complete;
        let plan_ok = inventory_ok && pre.source_target_ok;
        let cutover_ok = pre.cutover_allowed();
        let qualified = vec![
            op(
                ActivationOperation::Inventory,
                inventory_ok,
                "catalog projection",
            ),
            op(ActivationOperation::Status, true, "read-only"),
            op(
                ActivationOperation::Plan,
                plan_ok,
                if plan_ok {
                    "readiness-qualified dry-run"
                } else {
                    "plan blocked"
                },
            ),
            op(
                ActivationOperation::Export,
                plan_ok,
                "bytes stay local until cutover",
            ),
            op(
                ActivationOperation::Import,
                plan_ok,
                "target is a fenced staging root",
            ),
            op(
                ActivationOperation::Verify,
                plan_ok,
                "failed verify cannot cut over",
            ),
            op(ActivationOperation::Checkpoint, plan_ok, "resume watermark"),
            op(ActivationOperation::Resume, plan_ok, "idempotent rerun"),
            op(
                ActivationOperation::Cancel,
                plan_ok,
                "stop without local mutation",
            ),
            op(
                ActivationOperation::Cutover,
                cutover_ok,
                if cutover_ok {
                    "fenced canonical transition"
                } else {
                    "gate_or_precondition_blocked"
                },
            ),
            op(
                ActivationOperation::Rollback,
                cutover_ok,
                if cutover_ok {
                    "fenced rollback"
                } else {
                    "gate_or_precondition_blocked"
                },
            ),
        ];
        Self {
            ok: matches!(
                operation,
                ActivationOperation::Inventory | ActivationOperation::Status
            ) || (plan_ok
                && !matches!(
                    operation,
                    ActivationOperation::Cutover | ActivationOperation::Rollback
                )),
            operation,
            dry_run: matches!(
                operation,
                ActivationOperation::Plan | ActivationOperation::Inventory
            ),
            principal: principal.into(),
            workspace: workspace.into(),
            source_profile: source.into(),
            target_profile: target.into(),
            gates: pre.gates.clone(),
            qualified,
            blocking: pre.blocking.clone(),
            migration_id: None,
            phase: None,
            local_canonical: true,
            backend_unavailable: !pre.remote_health,
            detail: serde_json::json!({}),
        }
    }

    pub fn redact(mut self) -> Self {
        self.detail = magician_storage_migration::sanitize_json(self.detail);
        self
    }
}

fn op(operation: ActivationOperation, qualified: bool, reason: &str) -> QualifiedOperation {
    QualifiedOperation {
        operation,
        qualified,
        reason: reason.into(),
    }
}
