//! Activation preconditions from storage plan Task 19.

use serde::Serialize;

use crate::magician_v2::gate3::GATE3_CLOSED as GATE3_ACCEPTED;
use crate::magician_v2::track_a_acceptance::{load_catalog, load_support_matrix};

/// Decision Gate 3 is closed by Task 18A.
pub const GATE3_CLOSED: bool = GATE3_ACCEPTED;
pub const GATE1_CLOSED: bool = true;
pub const GATE2_CLOSED: bool = true;
pub const DEFAULT_BACKUP_MAX_AGE_SECS: u64 = 24 * 60 * 60;
pub const MIN_ROLLBACK_RETAIN_DAYS: u32 = 7;

#[derive(Debug, Clone, Serialize)]
pub struct GateStatus {
    pub gate1_closed: bool,
    pub gate2_closed: bool,
    pub gate3_closed: bool,
}

#[derive(Debug, Clone)]
pub struct ActivationContext {
    pub source_profile: String,
    pub target_profile: String,
    pub backup_age_secs: Option<u64>,
    pub remote_health: bool,
    pub other_migration_lease: bool,
    pub rollback_retain_days: u32,
}

impl Default for ActivationContext {
    fn default() -> Self {
        Self {
            source_profile: "local_embedded".into(),
            target_profile: "remote_durable".into(),
            backup_age_secs: None,
            remote_health: false,
            other_migration_lease: false,
            rollback_retain_days: MIN_ROLLBACK_RETAIN_DAYS,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PreconditionReport {
    pub gates: GateStatus,
    pub tier1_remote_ready: bool,
    pub tier2_matrix_complete: bool,
    pub source_target_ok: bool,
    pub backup_recent: bool,
    pub remote_health: bool,
    pub no_other_lease: bool,
    pub rollback_retention: bool,
    pub blocking: Vec<String>,
}

impl PreconditionReport {
    pub fn cutover_allowed(&self) -> bool {
        self.blocking.is_empty()
    }
}

pub fn evaluate_preconditions(ctx: &ActivationContext) -> anyhow::Result<PreconditionReport> {
    let catalog = load_catalog()?;
    let matrix = load_support_matrix()?;
    let mut blocking = Vec::new();
    let gates = GateStatus {
        gate1_closed: GATE1_CLOSED,
        gate2_closed: GATE2_CLOSED,
        gate3_closed: GATE3_CLOSED,
    };
    if !gates.gate1_closed {
        blocking.push("decision_gate_1_open".into());
    }
    if !gates.gate2_closed {
        blocking.push("decision_gate_2_open".into());
    }
    if !gates.gate3_closed {
        blocking.push("decision_gate_3_open".into());
    }

    let mut tier1_ok = true;
    let mut required_matrix = 0u64;
    for owner in &catalog.owners {
        if owner.is_tier1() && owner.readiness.state != "remote_ready" {
            tier1_ok = false;
            blocking.push(format!("tier1_not_ready:{}", owner.id));
        }
        if owner.is_tier2() || owner.is_device_local() {
            required_matrix += 1;
        }
    }
    let matrix_ok = matrix.owners.len() as u64 == required_matrix;
    if !matrix_ok {
        blocking.push("tier2_matrix_incomplete".into());
    }
    if !tier1_ok {
        blocking.push("tier1_incomplete".into());
    }

    let source_target_ok = ctx.source_profile == "local_embedded"
        && ctx.target_profile == "remote_durable"
        && ctx.source_profile != ctx.target_profile;
    if !source_target_ok {
        blocking.push("source_target_mismatch".into());
    }

    let backup_recent = ctx
        .backup_age_secs
        .is_some_and(|age| age <= DEFAULT_BACKUP_MAX_AGE_SECS);
    if !backup_recent {
        blocking.push("backup_stale_or_missing".into());
    }
    if !ctx.remote_health {
        blocking.push("remote_health_unavailable".into());
    }
    if ctx.other_migration_lease {
        blocking.push("migration_lease_held".into());
    }
    let rollback_retention = ctx.rollback_retain_days >= MIN_ROLLBACK_RETAIN_DAYS;
    if !rollback_retention {
        blocking.push("rollback_retention_too_short".into());
    }

    Ok(PreconditionReport {
        gates,
        tier1_remote_ready: tier1_ok,
        tier2_matrix_complete: matrix_ok,
        source_target_ok,
        backup_recent,
        remote_health: ctx.remote_health,
        no_other_lease: !ctx.other_migration_lease,
        rollback_retention,
        blocking,
    })
}
