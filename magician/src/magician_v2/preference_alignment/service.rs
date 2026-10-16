//! The one entry point admission calls: classify the act, decide whether the
//! feature runs for this owner, collect the inventory, judge, and say what
//! admission may do (plan §9.2 steps 2–3).
//!
//! Modes (`preference_alignment.mode`, see `crate::config`):
//!
//! - `off` — nothing runs; no model call, no inventory read.
//! - `shadow` — the full check runs and is recorded, content-free, but the
//!   answer never affects the act. Canary scopes listed in `enforce_scopes`
//!   enforce instead.
//! - `enforce` — the answer governs admission.
//!
//! The configuration is reloadable: [`install_config`] publishes each accepted
//! config, and a rejected reload never reaches it, so the last accepted mode
//! stays in force.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::admission::{
    append_alignment_log, AdmissionRecord, AdmissionTicket, DecisionOrigin,
    ADMISSION_RECORD_SCHEMA_VERSION,
};
use super::classify::{classify_action, AlignmentScope};
use super::collect::{action_facts, build_inputs, CheckRequest};
use super::ask::AskIntention;
use super::contract::HostMatchedException;
use super::intention::{CoverageStatus, StandingGeneration};
use super::inventory::{collect_inventory, InventoryRequest};
use super::policy::read_policy;
use super::judge::{
    judge_batches_with, AlignmentDisposition, JudgeBackend, JudgeOutcome, ScriptedJudge,
    UnresolvedCause,
};
use super::sources::taste_profile::DEFAULT_TASTE_PROFILE_ADMISSION_TIMEOUT;
use crate::config::{PreferenceAlignmentConfig, PreferenceAlignmentJudge, PreferenceAlignmentMode};
use crate::magician_v2::agents::AgentMemoryResolver;
use crate::magician_v2::execution::actions::ExecutableAction;
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

/// Version of the reviewed default intervention policy (P0 contracts §8.2).
pub const DEFAULT_POLICY_VERSION: &str = "preference_alignment_policy.v1";

static INSTALLED: RwLock<Option<Arc<PreferenceAlignmentConfig>>> = RwLock::new(None);

/// Publish an accepted configuration. Called after validation succeeds, on
/// startup and on every accepted reload.
pub fn install_config(config: PreferenceAlignmentConfig) {
    let mut slot = INSTALLED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(Arc::new(config));
}

/// The configuration in force. Nothing installed means `off`.
pub fn current_config() -> Arc<PreferenceAlignmentConfig> {
    INSTALLED
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(|| Arc::new(PreferenceAlignmentConfig::default()))
}

/// Alignment settings one executor set uses instead of the installed ones
/// (`ActionExecutors::preference_alignment_override`). Provider-free lanes
/// only; production carries none.
#[derive(Clone)]
pub struct AlignmentOverride {
    pub config: PreferenceAlignmentConfig,
    /// Replaces the routed judge when set.
    pub judge: Option<Arc<dyn ScriptedJudge>>,
}

impl std::fmt::Debug for AlignmentOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlignmentOverride")
            .field("config", &self.config)
            .field("judge", &self.judge.as_ref().map(|_| "scripted"))
            .finish()
    }
}

/// The configuration and scripted judge in force for one executor set: its
/// override when it carries one, else the installed configuration and no
/// scripted judge.
pub fn effective_alignment(
    alignment_override: Option<&AlignmentOverride>,
) -> (Arc<PreferenceAlignmentConfig>, Option<Arc<dyn ScriptedJudge>>) {
    match alignment_override {
        Some(alignment) => (Arc::new(alignment.config.clone()), alignment.judge.clone()),
        None => (current_config(), None),
    }
}

/// What admission may do with this act, from alignment's side only. Every
/// other gate still applies to every variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "admission", rename_all = "snake_case")]
pub enum AlignmentAdmission {
    /// The feature is off for this owner, or the act is steered only.
    NotChecked,
    /// Checked (or shadowed) and nothing stops the act.
    Proceed,
    /// Proceed and show the owner this nudge.
    ProceedWithNudge { outcome: JudgeOutcome },
    /// Hold the act and ask the owner.
    Hold { outcome: JudgeOutcome },
    /// The current owner turn cancelled the work.
    Cancel { outcome: JudgeOutcome },
    /// The check could not finish: hold and offer recovery.
    Unresolved { cause: UnresolvedCause },
}

impl AlignmentAdmission {
    /// Whether the act may continue to the remaining gates now.
    pub fn may_continue(&self) -> bool {
        matches!(
            self,
            Self::NotChecked | Self::Proceed | Self::ProceedWithNudge { .. }
        )
    }
}

/// One alignment check, with what it was bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionCheck {
    pub mode: String,
    pub scope: AlignmentScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<StandingGeneration>,
    /// Why the act may continue, when it may. `None` for an act that was not
    /// checked or may not continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_origin: Option<DecisionOrigin>,
    pub admission: AlignmentAdmission,
    /// The owner policy the check ran under (`policy::OwnerPolicy::effective`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<String>,
    /// The intentions the decision cited (and the one it offered to
    /// remember), for the owner-private detail of a hold. Never logged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cited: Vec<AskIntention>,
}

impl ActionCheck {
    /// The admission ticket for a checked act that may continue, for the
    /// dispatch to fence and register. `None` when the act was not checked
    /// or may not continue.
    pub fn ticket(&self, request: &CheckRequest) -> Option<AdmissionTicket> {
        let origin = self.decision_origin?;
        let binding_id = self.binding_id.clone()?;
        if !self.admission.may_continue() {
            return None;
        }
        Some(AdmissionTicket {
            principal: request.principal.clone(),
            workspace: request.workspace.clone(),
            record: AdmissionRecord {
                schema_version: ADMISSION_RECORD_SCHEMA_VERSION.to_string(),
                binding_id,
                decision_origin: origin,
                generation: self.generation.clone(),
                mode: self.mode.clone(),
                policy_version: self
                    .policy_version
                    .clone()
                    .unwrap_or_else(|| DEFAULT_POLICY_VERSION.to_string()),
                execution_id: request.execution_id.clone(),
                admitted_at_ms: Utc::now().timestamp_millis(),
            },
            enforced: origin != DecisionOrigin::Shadow,
        })
    }
}

/// The ticket for an act the owner answered for directly (an alignment
/// receipt). It carries no generation: the owner's answer is about this exact
/// act, so a later change in standing intentions does not void it.
pub fn owner_answer_ticket(
    config: &PreferenceAlignmentConfig,
    request: &CheckRequest,
    binding_id: String,
    origin: DecisionOrigin,
) -> AdmissionTicket {
    AdmissionTicket {
        principal: request.principal.clone(),
        workspace: request.workspace.clone(),
        record: AdmissionRecord {
            schema_version: ADMISSION_RECORD_SCHEMA_VERSION.to_string(),
            binding_id,
            decision_origin: origin,
            generation: None,
            mode: config
                .mode_for_scope(&request.principal, &request.workspace)
                .as_str()
                .to_string(),
            policy_version: DEFAULT_POLICY_VERSION.to_string(),
            execution_id: request.execution_id.clone(),
            admitted_at_ms: Utc::now().timestamp_millis(),
        },
        enforced: true,
    }
}

/// Judge calls spent per task (owner, workspace, execution), process-local.
/// A restart forgets the count, so a task can spend its budget once more
/// after one; the budget bounds a runaway loop, not a lifetime total.
static JUDGE_CALLS_PER_TASK: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(String, String, String), u32>>,
> = std::sync::LazyLock::new(Default::default);

/// Most tasks whose judge-call counts are kept. Past it the map is cleared:
/// forgetting counts only lets tasks spend again, never blocks one wrongly.
const MAX_COUNTED_TASKS: usize = 4_096;

/// Reserve `calls` judge calls for the request's task. `false` when that
/// would pass `max`. Requests that belong to no task are not counted.
fn reserve_judge_calls(request: &CheckRequest, calls: usize, max: u32) -> bool {
    let Some(execution_id) = request.execution_id.as_deref().filter(|id| !id.is_empty()) else {
        return true;
    };
    if calls == 0 {
        return true;
    }
    let calls = u32::try_from(calls).unwrap_or(u32::MAX);
    let mut counts = JUDGE_CALLS_PER_TASK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if counts.len() >= MAX_COUNTED_TASKS {
        counts.clear();
    }
    let spent = counts
        .entry((
            request.principal.clone(),
            request.workspace.clone(),
            execution_id.to_string(),
        ))
        .or_default();
    if spent.saturating_add(calls) > max {
        return false;
    }
    *spent += calls;
    true
}

/// Process-wide handles a check needs. All borrowed; nothing is cached here.
pub struct AlignmentEnvironment<'a> {
    pub config: &'a PreferenceAlignmentConfig,
    pub router: Option<&'a OperationLlmRouter>,
    /// A provider-free judge that replaces the router when set. Only the
    /// journey eval lane and tests set it.
    pub scripted: Option<&'a dyn ScriptedJudge>,
    pub resolver: &'a AgentMemoryResolver,
    pub taste_loader: Option<&'a crate::magician_v2::taste_profile::TasteProfileLoader>,
    pub taste_capture: Option<&'a crate::magician_v2::taste_capture::TasteCaptureService>,
}

/// Run the alignment check for one proposed act.
pub async fn check_action(
    environment: &AlignmentEnvironment<'_>,
    request: &CheckRequest,
    action: &ExecutableAction,
    host_matched_exceptions: &[HostMatchedException],
) -> ActionCheck {
    let config = environment.config;
    let mode = config.mode_for_scope(&request.principal, &request.workspace);
    let scope = classify_action(action);
    let not_checked = |scope: AlignmentScope| ActionCheck {
        mode: mode.as_str().to_string(),
        scope,
        binding_id: None,
        generation: None,
        decision_origin: None,
        admission: AlignmentAdmission::NotChecked,
        policy_version: None,
        cited: Vec::new(),
    };
    if mode == PreferenceAlignmentMode::Off || !scope.is_checked() {
        return not_checked(scope);
    }

    let Some(facts) = action_facts(action, &scope) else {
        // A checked act that cannot be bound cannot carry a receipt.
        return finish(
            mode,
            scope,
            None,
            None,
            None,
            AlignmentAdmission::Unresolved {
                cause: UnresolvedCause::Malformed("the act cannot be bound".to_string()),
            },
        );
    };
    let binding_id = facts.binding_id.clone();

    let inventory = collect_inventory(
        environment.resolver,
        InventoryRequest {
            principal: &request.principal,
            workspace: &request.workspace,
            taste_loader: environment.taste_loader,
            taste_capture: environment.taste_capture,
            taste_timeout: DEFAULT_TASTE_PROFILE_ADMISSION_TIMEOUT,
            now: Utc::now(),
        },
    )
    .await;
    let generation = inventory.generation.clone();
    let coverage_status = inventory.coverage.status;
    // The owner's explicit intervention policy (P7.2). An unreadable one is
    // not silently replaced by the defaults, which may be weaker than what
    // the owner chose: the check is unresolved and the owner can recover.
    let owner_policy = match read_policy(environment.resolver, &request.principal, &request.workspace)
        .await
    {
        Ok(policy) => policy,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "[PREFERENCE-ALIGNMENT] owner intervention policy unreadable; check unresolved"
            );
            return finish(
                mode,
                scope,
                Some(binding_id),
                Some(generation),
                None,
                AlignmentAdmission::Unresolved {
                    cause: UnresolvedCause::Malformed(
                        "the owner's intervention policy could not be read".to_string(),
                    ),
                },
            );
        },
    };
    let mut inventory = inventory;
    owner_policy.apply(&mut inventory.intentions);
    let policy = owner_policy.effective();
    let inputs = build_inputs(
        inventory,
        request,
        Some(facts),
        &policy,
        host_matched_exceptions,
        config.budgets.max_standing_batch,
    );
    let budget = Duration::from_millis(config.budgets.foreground_check_ms);
    let backend = match (environment.scripted, environment.router) {
        (Some(scripted), _) => Some(JudgeBackend::Scripted(scripted)),
        // The typed path (owner decision 2026-10-11), the generative judge
        // as its fallback.
        (None, router) if config.judge() == PreferenceAlignmentJudge::Engine => {
            Some(JudgeBackend::Engine {
                fallback: router,
                bars: config.composer,
            })
        },
        (None, Some(router)) => Some(JudgeBackend::Router(router)),
        (None, None) => None,
    };
    // One judge call per batch, counted against the task. A spent budget is
    // unresolved, never proceed: the owner can waive it or cancel.
    let outcome = if reserve_judge_calls(
        request,
        inputs.len(),
        config.budgets.max_judge_calls_per_task,
    ) {
        judge_batches_with(backend, &inputs, budget).await
    } else {
        tracing::warn!(
            execution_id = ?request.execution_id,
            "[PREFERENCE-ALIGNMENT] the task spent its judge-call budget; check unresolved"
        );
        JudgeOutcome::Unresolved {
            cause: UnresolvedCause::BudgetSpent,
            latency_ms: 0,
        }
    };
    let log_entry = record(&mode, &scope, coverage_status, inputs.len(), &outcome);
    let origin = match &outcome {
        JudgeOutcome::NothingToCompare => DecisionOrigin::NothingToCompare,
        _ => DecisionOrigin::ModelAction,
    };
    let outcome_for_citations = cited_ids(&outcome);

    let admission = match outcome.disposition() {
        AlignmentDisposition::Proceed => AlignmentAdmission::Proceed,
        AlignmentDisposition::ProceedWithNudge => AlignmentAdmission::ProceedWithNudge { outcome },
        AlignmentDisposition::Hold => AlignmentAdmission::Hold { outcome },
        AlignmentDisposition::Cancel => AlignmentAdmission::Cancel { outcome },
        AlignmentDisposition::Unresolved => AlignmentAdmission::Unresolved {
            cause: match outcome {
                JudgeOutcome::Unresolved { cause, .. } => cause,
                // A decided proceed over partial coverage.
                _ => UnresolvedCause::Malformed("coverage incomplete".to_string()),
            },
        },
    };
    let mut check = finish(
        mode,
        scope,
        Some(binding_id),
        Some(generation),
        Some(origin),
        admission,
    );
    check.policy_version = Some(policy.version.clone());
    check.cited = cited_intentions(&inputs, &outcome_for_citations);
    let mut log_entry = log_entry;
    log_entry["kind"] = serde_json::json!("check");
    log_entry["binding_id"] = serde_json::json!(check.binding_id);
    log_entry["generation"] = serde_json::json!(check.generation);
    log_entry["execution_id"] = serde_json::json!(request.execution_id);
    log_entry["admission"] = serde_json::json!(admission_name(&check.admission));
    // Which intentions, at which revisions, this check judged: ids and
    // content digests only, so the line stays content-free and any decision
    // can be traced to exactly what it saw.
    log_entry["judged"] = serde_json::json!(inputs
        .iter()
        .flat_map(|input| input.intentions.iter())
        .map(|intention| serde_json::json!({"id": intention.id, "revision": intention.revision}))
        .collect::<Vec<_>>());
    log_entry["policy_version"] = serde_json::json!(check.policy_version);
    log_entry["at_ms"] = serde_json::json!(Utc::now().timestamp_millis());
    append_alignment_log(
        environment.resolver,
        &request.principal,
        &request.workspace,
        &log_entry,
    )
    .await;
    check
}

/// The ids a decided outcome cites: its supporting intentions and the one it
/// offered to remember.
fn cited_ids(outcome: &JudgeOutcome) -> Vec<String> {
    let JudgeOutcome::Decided { decision, .. } = outcome else {
        return Vec::new();
    };
    let mut ids = decision.decision.supporting_intentions.clone();
    if let Some(offer) = &decision.decision.remember_offer {
        if !ids.contains(offer) {
            ids.push(offer.clone());
        }
    }
    ids
}

/// The cited intentions as the judge saw them, in citation order.
fn cited_intentions(inputs: &[super::contract::AlignmentInput], ids: &[String]) -> Vec<AskIntention> {
    ids.iter()
        .filter_map(|id| {
            inputs
                .iter()
                .flat_map(|input| input.intentions.iter())
                .find(|intention| &intention.id == id)
                .map(AskIntention::from)
        })
        .collect()
}

fn admission_name(admission: &AlignmentAdmission) -> &'static str {
    match admission {
        AlignmentAdmission::NotChecked => "not_checked",
        AlignmentAdmission::Proceed => "proceed",
        AlignmentAdmission::ProceedWithNudge { .. } => "proceed_with_nudge",
        AlignmentAdmission::Hold { .. } => "hold",
        AlignmentAdmission::Cancel { .. } => "cancel",
        AlignmentAdmission::Unresolved { .. } => "unresolved",
    }
}

/// Shadow never changes what happens to the act: whatever the judge said, the
/// act proceeds and the answer is only recorded.
fn finish(
    mode: PreferenceAlignmentMode,
    scope: AlignmentScope,
    binding_id: Option<String>,
    generation: Option<StandingGeneration>,
    origin: Option<DecisionOrigin>,
    admission: AlignmentAdmission,
) -> ActionCheck {
    let (admission, decision_origin) = if mode == PreferenceAlignmentMode::Shadow {
        (AlignmentAdmission::Proceed, Some(DecisionOrigin::Shadow))
    } else if admission.may_continue() {
        (admission, origin)
    } else {
        (admission, None)
    };
    ActionCheck {
        mode: mode.as_str().to_string(),
        scope,
        binding_id,
        generation,
        decision_origin,
        admission,
        policy_version: None,
        cited: Vec::new(),
    }
}

/// Content-free record of one check (plan §12.1): mode, classes, coverage,
/// batch count, disposition, failure stage and latency. No intentions, no
/// arguments, no explanation text. Traced, and returned as the base of the
/// durable log line.
fn record(
    mode: &PreferenceAlignmentMode,
    scope: &AlignmentScope,
    coverage: CoverageStatus,
    batches: usize,
    outcome: &JudgeOutcome,
) -> serde_json::Value {
    let (consequence_class, outward_class) = match scope {
        AlignmentScope::Checked {
            consequence_class,
            outward_class,
            ..
        } => (
            consequence_class.as_str(),
            outward_class.as_deref().unwrap_or("none"),
        ),
        AlignmentScope::SteeredOnly => ("private_local", "none"),
    };
    let (origin, cause, latency_ms, downgraded) = match outcome {
        JudgeOutcome::NothingToCompare => ("nothing_to_compare", "none".to_string(), 0, false),
        JudgeOutcome::Decided {
            decision,
            latency_ms,
            ..
        } => (
            "model_action",
            "none".to_string(),
            *latency_ms,
            decision.downgrade.is_some(),
        ),
        JudgeOutcome::Unresolved { cause, latency_ms } => (
            "unresolved",
            serde_json::to_value(cause)
                .ok()
                .and_then(|value| {
                    value
                        .get("cause")
                        .and_then(|c| c.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "unknown".to_string()),
            *latency_ms,
            false,
        ),
    };
    tracing::info!(
        target: "preference_alignment",
        mode = mode.as_str(),
        consequence_class,
        outward_class,
        coverage = ?coverage,
        batches,
        origin,
        disposition = ?outcome.disposition(),
        cause = %cause,
        latency_ms,
        downgraded,
        "preference alignment check"
    );
    serde_json::json!({
        "mode": mode.as_str(),
        "consequence_class": consequence_class,
        "outward_class": outward_class,
        "coverage": coverage,
        "batches": batches,
        "origin": origin,
        "disposition": outcome.disposition(),
        "cause": cause,
        "latency_ms": latency_ms,
        "downgraded": downgraded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_spends_its_judge_budget_once_and_then_is_unresolved() {
        let task = CheckRequest::for_run("owner", "default", Some("exec-budget"), "t", None, 0);
        assert!(reserve_judge_calls(&task, 3, 4));
        assert!(!reserve_judge_calls(&task, 2, 4), "over the budget");
        assert!(reserve_judge_calls(&task, 1, 4), "exactly at the budget");
        assert!(!reserve_judge_calls(&task, 1, 4));
        // Another task, and work that belongs to no task, are not charged.
        let other = CheckRequest::for_run("owner", "default", Some("exec-other"), "t", None, 0);
        assert!(reserve_judge_calls(&other, 4, 4));
        let untracked = CheckRequest::for_run("owner", "default", None, "t", None, 0);
        assert!(reserve_judge_calls(&untracked, 100, 4));
    }


    #[test]
    fn nothing_installed_reads_as_off() {
        // Other tests may install; only assert the default shape here.
        assert_eq!(
            PreferenceAlignmentConfig::default().mode(),
            PreferenceAlignmentMode::Off
        );
    }

    #[test]
    fn shadow_never_holds() {
        let check = finish(
            PreferenceAlignmentMode::Shadow,
            AlignmentScope::SteeredOnly,
            None,
            None,
            None,
            AlignmentAdmission::Unresolved {
                cause: UnresolvedCause::Timeout,
            },
        );
        assert_eq!(check.admission, AlignmentAdmission::Proceed);
        assert!(check.admission.may_continue());
    }

    #[test]
    fn enforce_keeps_the_hold() {
        let check = finish(
            PreferenceAlignmentMode::Enforce,
            AlignmentScope::SteeredOnly,
            None,
            None,
            None,
            AlignmentAdmission::Unresolved {
                cause: UnresolvedCause::Timeout,
            },
        );
        assert!(!check.admission.may_continue());
    }

    #[tokio::test]
    async fn off_and_steered_acts_never_read_memory_or_call_a_model() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let config = PreferenceAlignmentConfig {
            mode: "enforce".to_string(),
            ..PreferenceAlignmentConfig::default()
        };
        let environment = AlignmentEnvironment {
            config: &config,
            router: None,
            scripted: None,
            resolver: &resolver,
            taste_loader: None,
            taste_capture: None,
        };
        let request = CheckRequest {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            execution_id: None,
            request_text: "look something up".to_string(),
            request_revision: 1,
            owner_spans: Vec::new(),
            pending_gates: Vec::new(),
        };
        let read = ExecutableAction::Http(crate::magician_v2::execution::actions::HttpAction::get(
            "https://example.invalid/",
        ));
        let check = check_action(&environment, &request, &read, &[]).await;
        assert_eq!(check.admission, AlignmentAdmission::NotChecked);
    }
}
