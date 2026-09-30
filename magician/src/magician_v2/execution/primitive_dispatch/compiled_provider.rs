//! Inner-loop dispatcher for packs backed by existing Rust providers.
//!
//! These packs use the same inner LLM loop as CLI-template tools, but each
//! primitive call lowers through the compiled capability provider instead of
//! spawning a subprocess. This keeps tools such as DuckDB, iMessage, and
//! harness trace reads on their existing safe Rust execution paths while still
//! giving the model a focused tool guide and terminal self-loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

use super::exec_ctx::PrimitiveExecCtx;
use super::runner::{PrimitiveDispatcher, PrimitiveToolResult};
use crate::magician_v2::execution::actions::ActionResult;
use crate::magician_v2::execution::capability::{
    CapabilityPackDefinition, CapabilityProvider, CapabilityRegistry, NativeActionSchemaDef,
};
use crate::magician_v2::execution::compiled_dispatch::{
    dispatch_compiled_provider, CompiledDispatchAuthority, CompiledDispatchContext,
};
use crate::magician_v2::execution::ExecutionError;

struct CompiledProviderDispatcher {
    pack_name: String,
    provider_name: String,
    provider: Arc<dyn CapabilityProvider>,
    action_schemas: HashMap<String, NativeActionSchemaDef>,
    timeout_secs: u64,
    session_id: Option<String>,
    provenance: HashMap<String, Value>,
    /// Captured from `exec_ctx.compiled_dispatch_authority` at
    /// dispatcher construction. `Some` → gated compiled-provider calls
    /// charge the spend ledger via reserve/commit/rollback. `None` →
    /// degraded path (execute inner action without ledger bookkeeping).
    authority: Option<CompiledDispatchAuthority>,
    learning_store: Option<crate::magician_v2::learning::LearningStore>,
}

impl CompiledProviderDispatcher {
    fn new(
        pack_name: String,
        provider_name: String,
        provider: Arc<dyn CapabilityProvider>,
        pack: CapabilityPackDefinition,
        exec_ctx: &PrimitiveExecCtx,
    ) -> Self {
        let timeout_secs = pack
            .execution
            .as_ref()
            .and_then(|meta| meta.default_timeout_secs)
            .unwrap_or_else(|| provider.default_timeout_secs());
        Self {
            pack_name,
            provider_name,
            provider,
            action_schemas: pack.native_action_schemas,
            timeout_secs,
            session_id: exec_ctx
                .execution_id
                .clone()
                .or_else(|| exec_ctx.legacy_execution_id.clone()),
            provenance: provenance_params(exec_ctx),
            authority: exec_ctx.compiled_dispatch_authority.clone(),
            learning_store: exec_ctx
                .artifact_workspace
                .clone()
                .map(crate::magician_v2::learning::LearningStore::new),
        }
    }

    fn resolved_params(&self, arguments: &Value) -> HashMap<String, Value> {
        merge_model_arguments_with_provenance(arguments, &self.provenance)
    }
}

fn merge_model_arguments_with_provenance(
    arguments: &Value,
    provenance: &HashMap<String, Value>,
) -> HashMap<String, Value> {
    let mut params = HashMap::new();
    if let Some(obj) = arguments.as_object() {
        if let Some(nested) = obj.get("parameters").and_then(Value::as_object) {
            for (key, value) in nested {
                if !key.starts_with("__") {
                    params.insert(key.clone(), value.clone());
                }
            }
        }
        for (key, value) in obj {
            if key != "parameters" && !key.starts_with("__") {
                params.insert(key.clone(), value.clone());
            }
        }
    }
    // Runtime provenance always wins. Starting from model parameters lets
    // ordinary top-level values retain their existing precedence over the
    // nested `parameters` object, while every hidden control field remains
    // trusted even when the model attempts to supply it.
    params.extend(provenance.clone());
    params
}

impl CompiledProviderDispatcher {
    async fn execute_provider(
        &self,
        tool_name: &str,
        arguments: &Value,
    ) -> Result<PrimitiveToolResult> {
        let started = Instant::now();
        let mut resolved_params = self.resolved_params(arguments);
        resolved_params.insert(
            "__action_name".to_string(),
            Value::String(tool_name.to_string()),
        );
        let timeout_secs = self
            .action_schemas
            .get(tool_name)
            .and_then(|schema| schema.timeout_secs)
            .unwrap_or(self.timeout_secs);

        // Route through the neutral compiled-pack dispatcher. The pack
        // name and provider registry key differ for inner-loop
        // compiled-provider packs (one provider serves many action
        // names via `__action_name`), so we pass the provider directly
        // via `dispatch_compiled_provider` and use `provider_name` as
        // the `tool_routing_key` to drive `provider.lower()`'s
        // dispatch — same behaviour as the previous local
        // `PlanStep { tool: Some(self.provider_name.clone()), … }`
        // construction.
        //
        // `self.authority` is captured from
        // `exec_ctx.compiled_dispatch_authority` at dispatcher
        // construction. When wired (autonomous outer loop /
        // chat-spawned inner loop both forward their bundle), gated
        // compiled calls inside the inner loop charge the spend ledger
        // via reserve / commit / rollback. When absent (tests, orphan
        // dispatches), the dispatcher's degraded path runs the inner
        // action without ledger bookkeeping — same posture as chat
        // before the gating wiring landed.
        // `__workspace` is injected into `provenance` by the
        // inner-loop dispatcher (see the `insert_optional` for
        // `__workspace` below). Empty-string fallback is treated as
        // missing by `provenance_str_opt` so gated dispatches without
        // workspace context fail loudly inside `execute_maybe_gated`.
        let workspace = provenance_str_opt(&self.provenance, "__workspace");
        let task_id = provenance_str_opt(&self.provenance, "__task_id");
        let chat_session_id = provenance_str_opt(&self.provenance, "__chat_session_id");
        let ctx = CompiledDispatchContext {
            principal: provenance_str(&self.provenance, "__principal"),
            workspace,
            agent_id: provenance_str(&self.provenance, "__agent_id"),
            session_id: self.session_id.as_deref(),
            task_id,
            execution_id: self.session_id.as_deref(),
            chat_session_id,
            invocation_context: None,
            calling_profile_name: None,
            app_owner_execution_credential: None,
            // `inner-loop:` (hyphen) matches the namespace prefix used
            // by `cli_template::entry.rs` and `primitive_dispatch::dispatch.rs`
            // for their `active_owner` synthesis. Using the same prefix
            // here means any spend tokens issued under the inner-loop
            // active-owner namespace match regardless of which
            // inner-loop subsystem produced the call.
            active_owner: format!(
                "inner-loop:{}",
                self.session_id.as_deref().unwrap_or(&self.pack_name)
            ),
            effect_id: self
                .provenance
                .get("__effect_id")
                .and_then(|value| value.as_str()),
            invocation_source:
                crate::magician_v2::learning::LearningSkillInvocationSource::PrimitiveCompiledProvider,
            learning_store: self.learning_store.clone(),
        };
        match dispatch_compiled_provider(
            Arc::clone(&self.provider),
            &self.provider_name,
            resolved_params,
            Some(timeout_secs),
            &ctx,
            self.authority.as_ref(),
        )
        .await
        {
            Ok(result) => Ok(success_result(started, result)),
            Err(err) => Ok(failed_result(started, err.to_string())),
        }
    }
}

/// Pull a string value out of the inner-loop provenance map for the
/// dispatch context. Returns `""` when missing so the dispatch context
/// always carries a non-null string — the resolver downstream interprets
/// empty principal/agent_id as "no scope-prefixed token matches", which
/// is the correct behaviour for inner-loop calls that haven't had a
/// `CompiledDispatchAuthority` wired (gated arm short-circuits to
/// "no budget configured" rather than authorising every call).
fn provenance_str<'a>(provenance: &'a HashMap<String, Value>, key: &str) -> &'a str {
    provenance.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Variant that returns `None` for missing-or-empty values. Used by
/// `CompiledDispatchContext::workspace` where the gate distinguishes
/// "no workspace context" (caller error → loud failure) from a real
/// scope identifier — empty-string in the provenance map should not
/// silently pick an empty-named scope's ledger.
fn provenance_str_opt<'a>(provenance: &'a HashMap<String, Value>, key: &str) -> Option<&'a str> {
    provenance
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

#[async_trait]
impl PrimitiveDispatcher for CompiledProviderDispatcher {
    async fn dispatch(&self, tool_name: &str, arguments: &Value) -> Result<PrimitiveToolResult> {
        if !self.action_schemas.contains_key(tool_name) {
            return Err(anyhow!(
                "`{}` has no compiled-provider inner-loop action `{tool_name}`",
                self.pack_name
            ));
        }
        self.execute_provider(tool_name, arguments).await
    }
}

/// LLM-less single-primitive dispatch for a compiled-provider pack
/// (duckdb / imessage / internal_data / read_trace).
///
/// Constructs the `CompiledProviderDispatcher` and invokes ONE action directly,
/// with no nested LLM. Returns the raw [`PrimitiveToolResult`]; the caller folds
/// it into an `ActionResult` and decides failure handling.
pub async fn dispatch_compiled_provider_primitive(
    pack_name: &str,
    provider_name: &str,
    action: &str,
    arguments: &Value,
    registry: Arc<CapabilityRegistry>,
    exec_ctx: &PrimitiveExecCtx,
) -> Result<PrimitiveToolResult, ExecutionError> {
    let pack = registry.get_pack_definition(pack_name).ok_or_else(|| {
        ExecutionError::Step(format!("unknown compiled-provider pack `{pack_name}`"))
    })?;
    let provider = registry.get(provider_name).ok_or_else(|| {
        ExecutionError::Step(format!(
            "compiled provider `{provider_name}` not registered for pack `{pack_name}`"
        ))
    })?;
    let dispatcher = CompiledProviderDispatcher::new(
        pack_name.to_string(),
        provider_name.to_string(),
        provider,
        pack,
        exec_ctx,
    );
    dispatcher
        .dispatch(action, arguments)
        .await
        .map_err(|err| ExecutionError::Step(err.to_string()))
}

fn provenance_params(exec_ctx: &PrimitiveExecCtx) -> HashMap<String, Value> {
    let mut params = HashMap::new();
    insert_optional(&mut params, "__principal", exec_ctx.principal.as_deref());
    insert_optional(&mut params, "__workspace", exec_ctx.workspace.as_deref());
    insert_optional(&mut params, "__task_id", exec_ctx.task_id.as_deref());
    // Same `__`-prefixed provenance idiom as the identifiers above: this
    // dispatcher is built from an exec ctx it no longer holds, so the attempt
    // identity travels with the rest of the provenance.
    insert_optional(&mut params, "__effect_id", exec_ctx.effect_id.as_deref());
    insert_optional(
        &mut params,
        "__chat_session_id",
        exec_ctx.chat_session_id.as_deref(),
    );
    insert_optional(
        &mut params,
        "__execution_id",
        exec_ctx
            .execution_id
            .as_deref()
            .or(exec_ctx.legacy_execution_id.as_deref()),
    );
    insert_optional(&mut params, "__agent_id", exec_ctx.agent_id.as_deref());
    insert_optional(&mut params, "__goal_id", exec_ctx.goal_id.as_deref());
    if let Some(max_spawned_tasks) = exec_ctx.max_spawned_tasks {
        params.insert(
            "__max_spawned_tasks".to_string(),
            Value::from(max_spawned_tasks),
        );
    }
    params
}

fn insert_optional(params: &mut HashMap<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        params.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn success_result(started: Instant, result: ActionResult) -> PrimitiveToolResult {
    let stdout = action_result_stdout(&result);
    PrimitiveToolResult {
        success: result.is_success(),
        parsed_json: serde_json::from_str(stdout.trim()).ok(),
        stdout,
        stderr: String::new(),
        artifacts: Vec::new(),
        elapsed_ms: elapsed_ms(started),
    }
}

fn failed_result(started: Instant, error: impl Into<String>) -> PrimitiveToolResult {
    PrimitiveToolResult {
        success: false,
        stdout: String::new(),
        stderr: error.into(),
        parsed_json: None,
        artifacts: Vec::new(),
        elapsed_ms: elapsed_ms(started),
    }
}

fn action_result_stdout(result: &ActionResult) -> String {
    match result {
        ActionResult::Success => "success".to_string(),
        ActionResult::Text { content } => content.clone(),
        ActionResult::Http { body, .. } => body.clone(),
        ActionResult::Bool { value } => value.to_string(),
        ActionResult::List { items } => serde_json::to_string(items).unwrap_or_default(),
        ActionResult::Binary { .. } | ActionResult::Browser { .. } => {
            serde_json::to_string(result).unwrap_or_default()
        },
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn compiled_provider_provenance_overwrites_and_filters_hidden_model_fields() {
        let provenance = HashMap::from([
            (
                "__principal".to_string(),
                Value::String("runtime-user".to_string()),
            ),
            (
                "__workspace".to_string(),
                Value::String("runtime-workspace".to_string()),
            ),
            (
                "__agent_id".to_string(),
                Value::String("runtime-agent".to_string()),
            ),
            (
                "__execution_id".to_string(),
                Value::String("runtime-execution".to_string()),
            ),
            (
                "__task_id".to_string(),
                Value::String("runtime-task".to_string()),
            ),
            (
                "__chat_session_id".to_string(),
                Value::String("runtime-chat".to_string()),
            ),
            ("__max_spawned_tasks".to_string(), Value::from(2_u32)),
        ]);
        let arguments = serde_json::json!({
            "parameters": {
                "query": "nested",
                "__principal": "nested-spoof",
                "__unknown_control": "nested-spoof",
            },
            "query": "top-level",
            "__workspace": "top-level-spoof",
            "__chat_session_id": "top-level-spoof",
            "__max_spawned_tasks": 999,
        });
        let params = merge_model_arguments_with_provenance(&arguments, &provenance);

        assert_eq!(
            params.get("query"),
            Some(&Value::String("top-level".to_string()))
        );
        assert_eq!(
            params.get("__principal"),
            Some(&Value::String("runtime-user".to_string()))
        );
        assert_eq!(
            params.get("__workspace"),
            Some(&Value::String("runtime-workspace".to_string()))
        );
        assert_eq!(
            params.get("__agent_id"),
            Some(&Value::String("runtime-agent".to_string()))
        );
        assert_eq!(
            params.get("__execution_id"),
            Some(&Value::String("runtime-execution".to_string()))
        );
        assert_eq!(
            params.get("__task_id"),
            Some(&Value::String("runtime-task".to_string()))
        );
        assert_eq!(
            params.get("__chat_session_id"),
            Some(&Value::String("runtime-chat".to_string()))
        );
        assert_eq!(params.get("__max_spawned_tasks"), Some(&Value::from(2_u32)));
        assert!(!params.contains_key("__unknown_control"));
    }
}
