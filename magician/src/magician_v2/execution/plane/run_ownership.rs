//! Delegated runs from the plane — Variant 2's start (plane Task 6b).
//!
//! `run_task` from the plane is a **launch**, not an ordinary tool effect:
//! the authority model is [`PlaneRunAuthority`] on the grant, and the launch
//! carries attenuation onto the new run — the denied families a plane grant
//! can never reach (inherited by spawned children), the grant's allowlist
//! intersected into the run's catalog, and the grant's harness as the run's
//! engine, with `magician` as a deliberate pin and never a silent fallback.
//! The attenuation persists beside the execution's routing overrides before
//! the run can dispatch, so it survives restarts and recovery, and a missing
//! payload under the attenuation marker fails closed at dispatch.

use serde_json::{json, Value};

/// Input authority follows verified parent edges beneath the launched run, not
/// task membership or a self-reported root ID. Separate runs of one task must
/// never acquire one another's pending questions.
pub fn input_execution_ids(
    root: &str,
    edges: &[(String, Option<String>)],
) -> Result<std::collections::BTreeSet<String>, &'static str> {
    if edges.len() > 4096 {
        return Err("execution input tree is too large");
    }
    let mut children = std::collections::HashMap::<&str, Vec<&str>>::new();
    let mut unique = std::collections::HashSet::new();
    for (id, parent) in edges {
        if !unique.insert(id) {
            return Err("execution input tree has duplicate IDs");
        }
        if let Some(parent) = parent {
            children.entry(parent).or_default().push(id);
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([root]);
    while let Some(id) = queue.pop_front() {
        if ids.insert(id.to_string()) {
            queue.extend(children.get(id).into_iter().flatten().copied());
        }
    }
    Ok(ids)
}

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

use once_cell::sync::Lazy;

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::auth::sessions::NEVER_ON_THE_PLANE;
use crate::magician_v2::execution::agentic::types::PlaneDelegationAttenuation;
use crate::magician_v2::execution::plane::grant::{PlaneGrant, PlaneRunAuthority};
use crate::magician_v2::execution::plane::turn_engine::{resolve_turn_engine, TurnEngine};

/// Grant-token-keyed ledger of executions this grant started, for the
/// authority's concurrent-run ceiling. Entries prune against the execution
/// store at each launch: a missing or terminal execution stops counting.
static GRANT_LAUNCHES: Lazy<StdMutex<HashMap<String, Vec<(String, String)>>>> =
    Lazy::new(|| StdMutex::new(HashMap::new()));

/// Handle a plane `tools/call` for `run_task`.
///
/// Callers must already have verified `grant.permits("run_task")` and catalog
/// callability (the dispatch path does both before routing here).
pub(crate) async fn plane_run_task(grant: &PlaneGrant, arguments: &Value) -> Value {
    let Some(authority) = grant.run_authority.as_ref() else {
        return mcp_error(
            "run_task requires run authority on the grant; this grant may only act directly \
             (Direct tools). Mint a grant with run authority to delegate runs.",
        );
    };
    if let Some(refusal) = refuse_unlaunchable_engine(&authority.harness_engine) {
        return refusal;
    }
    let Some(task_id) = arguments
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return mcp_error("run_task requires a non-empty `task_id`");
    };
    let (Some(principal), Some(workspace)) = (
        grant
            .ctx
            .principal
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        grant
            .ctx
            .workspace
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ) else {
        return mcp_error("run_task requires the grant to carry principal and workspace scope");
    };
    let Some(service) = grant
        .executors
        .as_ref()
        .and_then(|executors| executors.artifact_v2_service.clone())
    else {
        return mcp_error(
            "run_task cannot reach the execution service from this grant's executors; the \
             runless executors must be installed with the ArtifactV2 service",
        );
    };
    let attenuation = attenuation_from(grant, authority);
    let scope = ScopeRef::system_internal_unauthenticated(principal, workspace);
    if let Some(refusal) =
        refuse_over_concurrency(&service, &scope, grant, authority, task_id).await
    {
        return refusal;
    }
    match service
        .start_plane_delegated_execution(scope, task_id.to_string(), None, attenuation)
        .await
    {
        Ok((task, execution)) => {
            GRANT_LAUNCHES
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(grant.session_id.clone())
                .or_default()
                .push((
                    task.manifest.task_id.clone(),
                    execution.state.execution_id.clone(),
                ));
            json!({
            "isError": false,
            "content": [{"type": "text", "text": format!(
                "started task {} execution {}",
                task.manifest.task_id,
                execution.state.execution_id,
            )}],
            "task_id": task.manifest.task_id,
            "execution_id": execution.state.execution_id,
            "task_status": task.state.status,
                "execution_status": execution.state.status,
            })
        },
        Err(error) => mcp_error(format!("run_task failed: {error}")),
    }
}

/// Enforce the authority's `max_concurrent_runs`: prune this grant's ledger
/// against the execution store (missing or terminal executions stop
/// counting), then refuse when the grant already holds the limit. A limit of
/// zero refuses every launch.
async fn refuse_over_concurrency(
    service: &crate::magician_v2::artifact_v2::service::ArtifactV2Service,
    scope: &ScopeRef,
    grant: &PlaneGrant,
    authority: &PlaneRunAuthority,
    task_id: &str,
) -> Option<Value> {
    if authority.max_concurrent_runs == 0 {
        return Some(mcp_error(
            "run_task refused: this grant's concurrent-run ceiling is zero",
        ));
    }
    // Storage reads may suspend. Never hold the process-wide synchronous
    // ledger mutex across them: other grants must still record launches.
    let observed = GRANT_LAUNCHES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&grant.session_id)
        .cloned()
        .unwrap_or_default();
    let mut inactive = Vec::new();
    for (launched_task, launched_execution) in &observed {
        if !service
            .execution_is_active(scope, launched_task, launched_execution)
            .await
        {
            inactive.push((launched_task.clone(), launched_execution.clone()));
        }
    }
    let mut ledger = GRANT_LAUNCHES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = ledger.entry(grant.session_id.clone()).or_default();
    // Remove only entries proven inactive; preserve launches recorded while
    // the store reads were in flight.
    entry.retain(|launch| !inactive.contains(launch));
    if entry.len() >= authority.max_concurrent_runs {
        return Some(mcp_error(format!(
            "run_task refused for task `{task_id}`: this grant already has {} active              execution(s) and allows {max}",
            entry.len(),
            max = authority.max_concurrent_runs,
        )));
    }
    None
}

/// The attenuation a delegated run dispatches under: the families a plane
/// grant can never reach, the grant's own allowlist as the run's narrowing,
/// and the authority's engine choice.
///
/// `run_task` is denied on the run itself, beyond [`NEVER_ON_THE_PLANE`]:
/// the loop-side `run_task` handler launches through the plain
/// `start_execution` entry with no attenuation, so a plane-started run that
/// could call it would hand an unattenuated sibling the agent's whole
/// surface — hole 2 again, one level down. Sub-work still delegates through
/// `spawn_sub_goal` / `DelegateToAgent`, which inherit
/// `denied_capability_names` and therefore this denial.
fn attenuation_from(
    grant: &PlaneGrant,
    authority: &PlaneRunAuthority,
) -> PlaneDelegationAttenuation {
    let mut denied: Vec<String> = NEVER_ON_THE_PLANE
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    denied.push("run_task".to_string());
    PlaneDelegationAttenuation {
        denied,
        allowed: (!grant.allowed_tools.is_empty()).then(|| grant.allowed_tools.clone()),
        harness_engine: Some(
            authority
                .harness_engine
                .trim()
                .is_empty()
                .then(|| "magician".to_string())
                .unwrap_or_else(|| authority.harness_engine.trim().to_string()),
        ),
        max_usd: authority.max_usd,
        max_wall_clock_secs: authority.max_wall_clock.map(|duration| duration.as_secs()),
    }
}

/// `Some(refusal)` when the authority names an engine this build cannot
/// launch. `""` and `"magician"` are the deliberate forced pin; any name that
/// does not resolve to a real harness (for example OpenCode, whose `run`
/// hangs) must fail at the door, not at spawn time.
fn refuse_unlaunchable_engine(named: &str) -> Option<Value> {
    match named.trim() {
        "" | "magician" => None,
        other => match resolve_turn_engine(Some(other)) {
            TurnEngine::Harness(_) => None,
            TurnEngine::MagicianDecision => Some(mcp_error(format!(
                "run_task cannot launch harness `{other}` on this build; name a launchable \
                 engine or pin `magician` deliberately"
            ))),
        },
    }
}

fn mcp_error(text: impl Into<String>) -> Value {
    json!({
        "isError": true,
        "content": [{"type": "text", "text": text.into()}],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(engine: &str) -> PlaneRunAuthority {
        PlaneRunAuthority {
            allowed_agents: Vec::new(),
            harness_engine: engine.to_string(),
            max_usd: None,
            max_wall_clock: None,
            max_concurrent_runs: 1,
        }
    }

    fn args() -> Value {
        json!({"task_id": "task-1"})
    }

    /// T3 still exists: a grant without run authority refuses, naming it.
    #[tokio::test]
    async fn a_grant_without_run_authority_cannot_start_one() {
        let grant = PlaneGrant::for_test("exec-noauth");
        let out = plane_run_task(&grant, &args()).await;
        assert_eq!(out["isError"], json!(true));
        assert!(
            out["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("run authority"),
            "{}",
            out
        );
    }

    /// OpenCode's `run` hang is a launch failure. Inherit must not pretend.
    #[tokio::test]
    async fn run_task_refuses_a_harness_magician_cannot_launch() {
        let grant = PlaneGrant::for_test("exec-opencode").with_run_authority(authority("opencode"));
        let out = plane_run_task(&grant, &args()).await;
        let text = out["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("cannot launch"), "{text}");
    }

    /// The forced pin is a deliberate choice, not a launch failure.
    #[tokio::test]
    async fn the_magician_pin_is_not_a_launch_refusal() {
        let grant = PlaneGrant::for_test("exec-pin").with_run_authority(authority("magician"));
        let out = plane_run_task(&grant, &args()).await;
        let text = out["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            !text.contains("cannot launch"),
            "the forced pin must pass the door: {text}"
        );
    }

    #[tokio::test]
    async fn a_missing_task_id_is_refused() {
        let grant =
            PlaneGrant::for_test("exec-notask").with_run_authority(authority("claude_code"));
        let out = plane_run_task(&grant, &json!({})).await;
        assert_eq!(out["isError"], json!(true));
    }

    /// The default test grant has no executors, so a would-be launch reports
    /// the honest unwired error rather than pretending to start.
    #[tokio::test]
    async fn authority_without_a_reachable_service_reports_unwired() {
        let grant =
            PlaneGrant::for_test("exec-unwired").with_run_authority(authority("claude_code"));
        let out = plane_run_task(&grant, &args()).await;
        let text = out["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("execution service"), "{text}");
    }

    /// Hole 2, at the source: the attenuation a narrow grant produces.
    #[test]
    fn a_narrow_grant_narrows_the_attenuation_it_launches_with() {
        let mut grant =
            PlaneGrant::for_test("exec-narrow").with_run_authority(authority("claude_code"));
        grant.allowed_tools = vec!["create_task".into(), "run_task".into(), "read_file".into()];
        let attenuation = attenuation_from(&grant, grant.run_authority.as_ref().unwrap());
        for denied in [
            "run_coding_task",
            "apply_code_proposal",
            "interactive_process",
            "run_task",
        ] {
            assert!(
                attenuation.denied.iter().any(|name| name == denied),
                "denied set missing {denied}"
            );
        }
        assert_eq!(
            attenuation.allowed.as_deref(),
            Some(
                [
                    "create_task".to_string(),
                    "run_task".to_string(),
                    "read_file".to_string()
                ]
                .as_slice()
            )
        );
        assert_eq!(attenuation.harness_engine.as_deref(), Some("claude_code"));
    }

    /// An empty grant allowlist is no narrowing — the agent's surface minus
    /// the denied families, the pre-6b meaning.
    #[test]
    fn an_empty_grant_allowlist_does_not_narrow_the_run() {
        let grant = PlaneGrant::for_test("exec-wide").with_run_authority(authority("  "));
        let attenuation = attenuation_from(&grant, grant.run_authority.as_ref().unwrap());
        assert_eq!(attenuation.allowed, None);
        assert_eq!(
            attenuation.harness_engine.as_deref(),
            Some("magician"),
            "a blank engine names the forced pin, never a silent harness"
        );
    }
}
