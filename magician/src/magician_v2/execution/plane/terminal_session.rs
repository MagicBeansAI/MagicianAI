//! Runless T3 session context (plane Task 11).
//!
//! A terminal acting directly has no Magician run. Live `Arc` handles
//! (sandbox widening, browser session flags) belong to a running
//! execution and are **refused** here, with a route to `run_task`.
//! Everything else uses the same `execute_action` path as a T1 grant,
//! once process-global runless executors are installed at boot.

use crate::magician_v2::execution::agentic::{ActionExecutors, AgenticContext};
use crate::magician_v2::execution::plane::grant::PlaneGrant;
use once_cell::sync::Lazy;
use std::sync::{Arc, RwLock};

static RUNLESS_EXECUTORS: Lazy<RwLock<Option<Arc<ActionExecutors>>>> =
    Lazy::new(|| RwLock::new(None));

/// Install the process-global executors used by runless (T3) grants.
pub fn install_runless_executors(executors: Arc<ActionExecutors>) {
    *RUNLESS_EXECUTORS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(executors);
}

/// Empty the process-global slot and return what it held. Boot never needs
/// this; a test that installs its own executors uses it to put the previous
/// occupant back so the slot's state does not leak into another test.
pub fn take_runless_executors() -> Option<Arc<ActionExecutors>> {
    RUNLESS_EXECUTORS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// Process-global executors for T3, if boot installed them.
pub fn runless_executors() -> Option<Arc<ActionExecutors>> {
    RUNLESS_EXECUTORS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The runless executors scoped to one grant: the scope's registry and
/// app-origin map resolved for the grant's principal and workspace, narrowed
/// to `allowed_tools`, with the grant's identity seeded, on slots no other
/// grant shares. Falls back to the shared, unscoped executors (today's
/// behaviour) when the grant names no scope, no scope resolver is installed,
/// or resolution fails, and says so at warn level, so a misconfigured runtime
/// degrades to visible refusals rather than a panic.
pub fn scoped_runless_executors(
    ctx: &AgenticContext,
    allowed_tools: &[String],
) -> Option<Arc<ActionExecutors>> {
    let shared = runless_executors()?;
    let (Some(principal), Some(workspace)) = (ctx.principal.as_deref(), ctx.workspace.as_deref())
    else {
        tracing::warn!("runless plane grant names no principal/workspace; dispatch stays unscoped");
        return Some(shared);
    };
    let Some(resolver) = shared.capability_scope_resolver.as_ref() else {
        tracing::warn!(
            principal,
            workspace,
            "runless plane executors have no scope resolver; dispatch stays unscoped"
        );
        return Some(shared);
    };
    match resolver.capability_snapshot_for_scope(principal, workspace) {
        Ok(snapshot) => Some(Arc::new(shared.for_runless_scope(
            ctx,
            &snapshot,
            allowed_tools,
        ))),
        Err(error) => {
            tracing::warn!(
                principal,
                workspace,
                error = %error,
                "runless plane scope resolution failed; dispatch stays unscoped"
            );
            Some(shared)
        },
    }
}

/// Open a runless plane session from an already-stamped grant.
///
/// Does not start a Magician execution. Features that need one fail at
/// dispatch with a refusal that names `run_task`.
pub struct TerminalSession {
    grant: PlaneGrant,
}

impl TerminalSession {
    pub fn open(mut grant: PlaneGrant) -> Self {
        if grant.executors.is_none() {
            grant.executors = scoped_runless_executors(&grant.ctx, &grant.allowed_tools);
        }
        grant.live_harness_turn = false;
        Self { grant }
    }

    pub fn session_id(&self) -> &str {
        &self.grant.session_id
    }

    pub fn grant(&self) -> &PlaneGrant {
        &self.grant
    }
}

/// If a runless refusal is a missing live-execution capability, name `run_task`.
pub fn decorate_runless_refusal(
    grant: &PlaneGrant,
    mut result: serde_json::Value,
) -> serde_json::Value {
    let runless = grant.ctx.execution_id.as_deref().is_none_or(|id| {
        id.starts_with("plane-terminal-") || grant.executors.is_some() && !grant.live_harness_turn
    });
    if !runless {
        return result;
    }
    if result.get("isError") != Some(&serde_json::json!(true)) {
        return result;
    }
    let Some(text) = result
        .pointer("/content/0/text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    else {
        return result;
    };
    let lower = text.to_ascii_lowercase();
    let needs_run = [
        "outside",
        "root",
        "sandbox",
        "browser session",
        "live execution",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if needs_run && !lower.contains("run_task") {
        if let Some(slot) = result.pointer_mut("/content/0/text") {
            *slot = serde_json::Value::String(format!(
                "{text} Start a Magician run with `run_task` to get a live execution that can widen the sandbox or hold browser session state."
            ));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::plane::grant::PlaneGrant;

    #[test]
    fn two_terminal_sessions_do_not_share_state() {
        let a = TerminalSession::open(PlaneGrant::for_test("term-a"));
        let b = TerminalSession::open(PlaneGrant::for_test("term-b"));
        assert_ne!(a.session_id(), b.session_id());
        assert_ne!(
            a.grant().ctx.execution_id.as_deref(),
            b.grant().ctx.execution_id.as_deref()
        );
    }

    #[test]
    fn an_out_of_root_refusal_names_run_task() {
        let grant = PlaneGrant::for_terminal(
            crate::magician_v2::execution::agentic::AgenticContext::default(),
            "plane-terminal-1".to_string(),
            Vec::new(),
            Arc::new(crate::magician_v2::execution::flat_loop::ToolIndex::default()),
        );
        let decorated = decorate_runless_refusal(
            &grant,
            serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": "path is outside the workspace root"}]
            }),
        );
        let text = decorated["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("outside") || text.contains("root"), "{text}");
        assert!(text.contains("run_task"), "{text}");
    }
}
