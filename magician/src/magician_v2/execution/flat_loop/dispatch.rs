//! Flat dispatch: route a flat per-action tool call to the right executor
//! without an inner LLM. Phase 2 of the flatten plan
//! (`docs/plans/2026-05-29-flat-loop-phase2-implementation.md`).
//!
//! `<pack>__<action>` tools whose pack is `ImplementationType::Primitive`
//! delegate to the LLM-less sibling `primitive_dispatch::dispatch::dispatch_primitive`
//! (which constructs the existing dispatcher and calls `dispatch(action, args)`
//! once). Bare-name compiled packs route to the existing
//! `compiled_dispatch::try_dispatch_compiled_pack`. Browser (Phase 6) routes
//! through the same `dispatch_primitive` path, which owns a
//! per-execution agent-browser session cache so `browser__*` primitives share
//! one tab/connection.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::magician_v2::execution::actions::ActionResult;
use crate::magician_v2::execution::capability::{CapabilityRegistry, ImplementationType};
use crate::magician_v2::execution::compiled_dispatch::CompiledDispatchContext;
use crate::magician_v2::execution::primitive_dispatch::dispatch::{
    dispatch_primitive, BROWSER_PACK_NAME,
};
use crate::magician_v2::execution::primitive_dispatch::exec_ctx::PrimitiveExecCtx;
use crate::magician_v2::execution::ExecutionError;

/// How a flat tool name resolves against the registry.
#[derive(Debug, Clone, PartialEq)]
pub enum FlatRoute {
    /// `<pack>__<action>` where the pack is an inner-loop compiled-provider pack
    /// (duckdb / imessage / internal_data / read_trace). Dispatched via the
    /// compiled provider, one primitive at a time.
    CompiledProviderPrimitive {
        provider_name: String,
        action: String,
    },
    /// `<pack>__<action>` where the pack is an inner-loop CLI-template pack
    /// (the skillshub subprocess skills). Dispatched via CliTemplateDispatcher.
    CliTemplatePrimitive { pack_name: String, action: String },
    /// Browser pack — out of scope until Phase 6 (session lifecycle redesign).
    Browser { action: String },
    /// A bare-name compiled/composite/command pack (shell, save_preference, …).
    /// Dispatched through the existing `dispatch_compiled_provider` path.
    PlainCompiled { tool_name: String },
    /// Name not found in the registry, or malformed for flat dispatch.
    Unknown { tool_name: String },
}

/// Split a flat tool name into `(pack, Option<action>)` on the FIRST `__`.
/// `duckdb__query` → `("duckdb", Some("query"))`; `shell` → `("shell", None)`.
pub fn parse_flat_tool_name(name: &str) -> (&str, Option<&str>) {
    match name.split_once("__") {
        Some((pack, action)) => (pack, Some(action)),
        None => (name, None),
    }
}

/// Classify a flat tool name against the registry's pack definitions.
pub fn classify_flat_route(registry: &Arc<CapabilityRegistry>, tool_name: &str) -> FlatRoute {
    let (pack_name, action) = parse_flat_tool_name(tool_name);
    let Some(def) = registry.get_pack_definition(pack_name) else {
        return FlatRoute::Unknown {
            tool_name: tool_name.to_string(),
        };
    };

    match (&def.implementation, action) {
        // Browser is special-cased by name in the inner-loop dispatcher; mirror that.
        (ImplementationType::Primitive { .. }, Some(action)) if pack_name == BROWSER_PACK_NAME => {
            FlatRoute::Browser {
                action: action.to_string(),
            }
        },
        (
            ImplementationType::Primitive {
                provider_name: Some(provider),
                ..
            },
            Some(action),
        ) => FlatRoute::CompiledProviderPrimitive {
            provider_name: provider.clone(),
            action: action.to_string(),
        },
        (
            ImplementationType::Primitive {
                provider_name: None,
                ..
            },
            Some(action),
        ) => FlatRoute::CliTemplatePrimitive {
            pack_name: pack_name.to_string(),
            action: action.to_string(),
        },
        // Bare-name compiled/composite/command pack (no `__`).
        (ImplementationType::Compiled { .. }, None)
        | (ImplementationType::Composite { .. }, None)
        | (ImplementationType::Command { .. }, None) => FlatRoute::PlainCompiled {
            tool_name: tool_name.to_string(),
        },
        // Inner-loop pack referenced without an action, or a compiled pack with a
        // spurious `__action` — malformed for flat dispatch.
        _ => FlatRoute::Unknown {
            tool_name: tool_name.to_string(),
        },
    }
}

/// The parameters a model's tool-call arguments may contribute to a dispatch:
/// everything except the runtime's own `__*` control fields. `pub(crate)` so
/// the sink walker can pin that the one flag it hands the HTTP adapter
/// survives this filter.
pub(crate) fn model_visible_params(arguments: &Value) -> HashMap<String, Value> {
    match arguments.as_object() {
        // Tool arguments originate with the model. Hidden `__*` fields are
        // exclusively runtime control data, so discard them before the runtime
        // provenance injection below. This also prevents an absent optional
        // runtime value from being silently supplied by the model.
        Some(obj) => obj
            .iter()
            .filter(|(key, value)| {
                // One `__*` field is the RUNTIME's, not the model's: the sink
                // walker stamps `__carries_credential` on a request it lowered a
                // secret into, and the HTTP adapter reads it to refuse redirects
                // and end a cross-origin attempt. Dropping it here switched that
                // protection off for every flat `http__*` call. Kept only when
                // it asserts the stricter value, so a model writing it can make
                // its own request safer and never laxer.
                if *key == crate::magician_v2::secrets::sinks::HTTP_PACK_CARRIES_CREDENTIAL {
                    return value.as_bool().unwrap_or(false);
                }
                !key.starts_with("__")
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        None => HashMap::new(),
    }
}

/// Build an actionable "this tool isn't available to me" error for a flat call
/// that resolved to no runnable capability in the agent's *scoped* registry.
///
/// The registry handed to flat dispatch is already scoped to the calling
/// agent's grants (`CapabilityRegistry::scoped`), so a miss here means the tool
/// is not one of this agent's granted capabilities — NOT a system fault. The
/// old messages ("`x` is not a compiled pack" / "unknown flat tool `x`") read
/// like an internal error, so the LLM retried the same ungranted tool, tripped
/// the stuck-warnings, and the run was cancelled. This frames it as a grant
/// issue, and tells the agent to delegate rather than retry, so it adapts on the
/// next turn. `delegation_targets` are not reachable at this dispatch site (they
/// live on the agent definition, not on `PrimitiveExecCtx`), so we give the
/// generic "delegate to an agent that has it" guidance rather than a named list.
fn ungranted_capability_error(tool_name: &str, agent_id: Option<&str>) -> ExecutionError {
    let owner = match agent_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => format!(" (agent `{id}`)"),
        None => String::new(),
    };
    ExecutionError::Step(format!(
        "`{tool_name}` is not available to this agent{owner} — it is not one of this agent's \
         granted capabilities (its `tools` grant does not include it). Do not retry this tool \
         directly; instead delegate this work to an agent that has this capability, or complete \
         the goal with the tools you do have."
    ))
}

/// Dispatch one flat tool call to the right executor, no inner LLM.
///
/// Phase 2: callable directly (tests / future flat loop). The live autonomous
/// loop does not call this yet.
pub async fn dispatch_flat_action(
    tool_name: &str,
    arguments: &Value,
    registry: &Arc<CapabilityRegistry>,
    exec_ctx: &PrimitiveExecCtx,
    cancellation_token: Option<CancellationToken>,
) -> Result<ActionResult, ExecutionError> {
    match classify_flat_route(registry, tool_name) {
        FlatRoute::CompiledProviderPrimitive { action, .. }
        | FlatRoute::CliTemplatePrimitive { action, .. } => {
            let (pack_name, _) = parse_flat_tool_name(tool_name);
            Box::pin(dispatch_primitive(
                pack_name,
                &action,
                arguments,
                exec_ctx,
                registry.clone(),
                cancellation_token,
            ))
            .await
        },
        FlatRoute::PlainCompiled { tool_name } => {
            // Bare-name compiled pack: reuse the existing fast-path dispatcher.
            // `try_dispatch_compiled_pack` returns `None` for non-Compiled packs.
            let active_owner = format!("flat:{}", exec_ctx.thread_id());
            let ctx = CompiledDispatchContext {
                principal: exec_ctx.principal.as_deref().unwrap_or(""),
                workspace: exec_ctx.workspace.as_deref(),
                agent_id: exec_ctx.agent_id.as_deref().unwrap_or(""),
                session_id: exec_ctx
                    .execution_id
                    .as_deref()
                    .or(exec_ctx.legacy_execution_id.as_deref()),
                task_id: exec_ctx.task_id.as_deref(),
                execution_id: exec_ctx
                    .execution_id
                    .as_deref()
                    .or(exec_ctx.legacy_execution_id.as_deref()),
                chat_session_id: exec_ctx.chat_session_id.as_deref(),
                invocation_context: exec_ctx.invocation_context.as_ref(),
                calling_profile_name: None,
                app_owner_execution_credential: None,
                active_owner,
                effect_id: exec_ctx.effect_id.as_deref(),
                invocation_source:
                    crate::magician_v2::learning::LearningSkillInvocationSource::CompiledPack,
                learning_store: exec_ctx
                    .artifact_workspace
                    .clone()
                    .map(crate::magician_v2::learning::LearningStore::new),
            };
            let mut params = model_visible_params(arguments);
            // Inject the `__`-prefixed scope identity that bare compiled handlers
            // (activate_skill / deactivate_skill / search_memory / save_preference /
            // switch_personality / create_task / …) require. Mirrors the chat bridge
            // (chat/service.rs) and the legacy autonomous dispatch (executor.rs).
            // `GenericCompiledProvider::execute` fills `__principal`/`__workspace` from
            // its own per-scope binding, but `__agent_id` (and `__execution_id` /
            // `__task_id`) have no other source on the flat autonomous path — the LLM
            // never supplies them — so a bare compiled handler errors on its
            // `require_scope_str("__agent_id", …)` check without this. Hidden
            // keys have already been removed from the model arguments, so these
            // inserts are the sole authority for the runtime control plane.
            if let Some(principal) = exec_ctx.principal.as_deref() {
                params.insert(
                    "__principal".to_string(),
                    Value::String(principal.to_string()),
                );
            }
            if let Some(workspace) = exec_ctx.workspace.as_deref() {
                params.insert(
                    "__workspace".to_string(),
                    Value::String(workspace.to_string()),
                );
            }
            if let Some(agent_id) = exec_ctx.agent_id.as_deref().filter(|id| !id.is_empty()) {
                params.insert(
                    "__agent_id".to_string(),
                    Value::String(agent_id.to_string()),
                );
            }
            if let Some(execution_id) = exec_ctx
                .execution_id
                .as_deref()
                .or(exec_ctx.legacy_execution_id.as_deref())
            {
                params.insert(
                    "__execution_id".to_string(),
                    Value::String(execution_id.to_string()),
                );
            }
            if let Some(task_id) = exec_ctx.task_id.as_deref() {
                params.insert("__task_id".to_string(), Value::String(task_id.to_string()));
            }
            if let Some(chat_session_id) = exec_ctx.chat_session_id.as_deref() {
                params.insert(
                    "__chat_session_id".to_string(),
                    Value::String(chat_session_id.to_string()),
                );
            }
            if let Some(invocation) = exec_ctx.invocation_context.as_ref() {
                params.insert(
                    "__source_kind".to_string(),
                    Value::String(invocation.source_kind.as_str().to_string()),
                );
                params.insert(
                    "__surface".to_string(),
                    Value::String(invocation.surface.as_str().to_string()),
                );
                params.insert(
                    "__feature_mode".to_string(),
                    Value::String(invocation.feature_mode.as_str().to_string()),
                );
                if let Some(source_agent_id) = invocation.source_agent_id.as_ref() {
                    params.insert(
                        "__source_agent_id".to_string(),
                        Value::String(source_agent_id.clone()),
                    );
                }
                if let Some(chat_turn_id) = invocation.chat_turn_id.as_ref() {
                    params.insert(
                        "__chat_turn_id".to_string(),
                        Value::String(chat_turn_id.clone()),
                    );
                }
            }
            if let Some(max_spawned_tasks) = exec_ctx.max_spawned_tasks {
                params.insert(
                    "__max_spawned_tasks".to_string(),
                    Value::from(max_spawned_tasks),
                );
            }
            // B2 — scope the per-execution cancel token across the compiled handler
            // so one that owns a cancellable downstream (run_coding_task -> Pi) can
            // opt into cooperative cancellation. The PlainCompiled dispatch otherwise
            // drops `cancellation_token`; this is the only place it would reach a bare
            // compiled handler. The chain to the handler is awaited inside both
            // scopes, so both task-locals are set for its whole run.
            //
            // P0.4 — nest the operator-approved sandbox roots task-local the same
            // way, so a file-touching compiled handler (`read_file`) can widen its
            // effective sandbox after a sandbox-override HITL approval (parity with
            // the native `files` pack path). Same awaited-inside-the-scope chain, so
            // it propagates.
            //
            // The dispatch future is heap-owned rather than nested inline in these
            // two `TaskLocalFuture`s. Propagation is unchanged — it is still polled
            // within both scopes — but each scope's own poll frame now holds a
            // pointer instead of the entire compiled-dispatch state machine beneath
            // it. This is the `heap_boxed` discipline the agentic loop already
            // applies, carried into the chain a content tool actually dispatches
            // through; without it a single `content_search` exhausted the
            // execution worker's default stack.
            crate::magician_v2::execution::compiled_dispatch::SESSION_FILE_SANDBOX_ROOTS
                .scope(
                    exec_ctx.session_file_sandbox_roots.clone(),
                    crate::magician_v2::execution::compiled_dispatch::EXECUTION_CANCEL_TOKEN
                        .scope(
                            cancellation_token,
                            Box::pin(
                                crate::magician_v2::execution::compiled_dispatch::try_dispatch_compiled_pack(
                                    registry,
                                    &tool_name,
                                    params,
                                    None,
                                    &ctx,
                                    exec_ctx.compiled_dispatch_authority.as_ref(),
                                ),
                            ),
                        ),
                )
                .await
                .ok_or_else(|| {
                    // `try_dispatch_compiled_pack` returned `None`: the bare name
                    // classified as a compiled pack but no runnable compiled
                    // provider is registered for this (scoped) agent — i.e. the
                    // capability is not granted to it. Emit an actionable
                    // grant-framed message instead of the bare, system-looking
                    // "is not a compiled pack" (which made agents retry + flail).
                    ungranted_capability_error(&tool_name, exec_ctx.agent_id.as_deref())
                })?
        },
        FlatRoute::Browser { action } => {
            // Phase 6: route the browser primitive through the LLM-less
            // single-primitive dispatcher, which owns the per-execution
            // agent-browser session cache + the BrowserDispatcher argv
            // translator. Mirrors the compiled-provider / CLI-template arms.
            Box::pin(dispatch_primitive(
                BROWSER_PACK_NAME,
                &action,
                arguments,
                exec_ctx,
                registry.clone(),
                cancellation_token,
            ))
            .await
        },
        FlatRoute::Unknown { tool_name } => {
            // The pack name is absent from this agent's scoped registry (not
            // granted), or malformed for flat dispatch. Same class of failure as
            // the compiled miss above — give the agent the same actionable,
            // grant-framed guidance rather than a bare "unknown flat tool".
            Err(ungranted_capability_error(
                &tool_name,
                exec_ctx.agent_id.as_deref(),
            ))
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::actions::{DuckDbAction, ExecutableAction};
    use crate::magician_v2::execution::capability::{CapabilityPackDefinition, CapabilityProvider};
    use crate::magician_v2::execution::compiled_providers::{
        embedded_compiled_pack_defs, project_runtime_package_to_pack, DuckDbCapabilityProvider,
    };
    use tool_runtime_core::manifest_parser::parse_skill_runtime_package;

    fn pack(yaml: &str) -> CapabilityPackDefinition {
        serde_yaml::from_str(yaml).expect("pack parses")
    }

    fn fixture_registry() -> Arc<CapabilityRegistry> {
        let reg = CapabilityRegistry::new();
        for p in [
            pack("name: duckdb\nparameters: []\nnative_action_schemas:\n  query:\n    parameters: [sql]\n    required: [sql]\nimplementation:\n  type: primitive\n  provider_name: duckdb\n"),
            pack("name: gmail\nparameters: []\nnative_action_schemas:\n  triage:\n    parameters: [query]\n    required: []\nimplementation:\n  type: primitive\n  command: [gmail]\n"),
            pack("name: shell\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: shell\n"),
            pack("name: browser\nparameters: []\nnative_action_schemas:\n  click:\n    parameters: []\nimplementation:\n  type: primitive\n"),
        ] {
            let name = p.name.clone();
            reg.set_pack_definition(&name, p);
        }
        Arc::new(reg)
    }

    #[test]
    fn parses_pack_and_action() {
        assert_eq!(
            parse_flat_tool_name("duckdb__query"),
            ("duckdb", Some("query"))
        );
        assert_eq!(parse_flat_tool_name("shell"), ("shell", None));
        // Only the FIRST `__` splits pack from action; action may contain `__`.
        assert_eq!(parse_flat_tool_name("a__b__c"), ("a", Some("b__c")));
    }

    #[test]
    fn flat_arguments_discard_model_supplied_hidden_control_fields() {
        let params = model_visible_params(&serde_json::json!({
            "command": "status",
            "__principal": "spoofed-principal",
            "__max_spawned_tasks": 999,
            "__unknown_control": "spoofed",
        }));

        assert_eq!(params.get("command"), Some(&serde_json::json!("status")));
        assert!(
            !params.keys().any(|key| key.starts_with("__")),
            "model arguments must never pass hidden control fields"
        );
    }

    #[test]
    fn classifies_compiled_provider_primitive() {
        let reg = fixture_registry();
        match classify_flat_route(&reg, "duckdb__query") {
            FlatRoute::CompiledProviderPrimitive {
                provider_name,
                action,
            } => {
                assert_eq!(provider_name, "duckdb");
                assert_eq!(action, "query");
            },
            other => panic!("expected CompiledProviderPrimitive, got {other:?}"),
        }
    }

    #[test]
    fn classifies_cli_template_primitive() {
        let reg = fixture_registry();
        match classify_flat_route(&reg, "gmail__triage") {
            FlatRoute::CliTemplatePrimitive { pack_name, action } => {
                assert_eq!(pack_name, "gmail");
                assert_eq!(action, "triage");
            },
            other => panic!("expected CliTemplatePrimitive, got {other:?}"),
        }
    }

    #[test]
    fn classifies_browser_primitive_route() {
        let reg = fixture_registry();
        match classify_flat_route(&reg, "browser__click") {
            FlatRoute::Browser { action } => assert_eq!(action, "click"),
            other => panic!("expected FlatRoute::Browser, got {other:?}"),
        }
    }

    #[test]
    fn classifies_plain_compiled_by_bare_name() {
        let reg = fixture_registry();
        assert!(matches!(
            classify_flat_route(&reg, "shell"),
            FlatRoute::PlainCompiled { .. }
        ));
    }

    #[test]
    fn unknown_pack_is_unknown() {
        let reg = fixture_registry();
        assert!(matches!(
            classify_flat_route(&reg, "nope__x"),
            FlatRoute::Unknown { .. }
        ));
        assert!(matches!(
            classify_flat_route(&reg, "nope"),
            FlatRoute::Unknown { .. }
        ));
        // Inner-loop pack referenced without an action is malformed for flat mode.
        assert!(matches!(
            classify_flat_route(&reg, "duckdb"),
            FlatRoute::Unknown { .. }
        ));
    }

    // Phase 6: browser__* now routes through dispatch_primitive (the
    // per-execution session cache + BrowserDispatcher), not a "not supported"
    // stub. End-to-end browser dispatch needs the agent-browser CLP + a live
    // Chrome/CDP target, so it is exercised by the browser session.rs tests
    // (fake-CLI) and the inner-loop browser path rather than here. The
    // session-cache guarantee that makes multi-primitive flat browser correct
    // (one shared session/tab per execution) is unit-tested in
    // `browser::session` (`flat_browser_session_cache_*`).

    #[tokio::test]
    async fn unknown_tool_is_rejected() {
        // A tool absent from this agent's scoped registry is reported as an
        // ungranted capability (grant-framed, actionable), not a bare system
        // "unknown flat tool" that made agents retry until they stalled.
        let reg = fixture_registry();
        let ctx = PrimitiveExecCtx::default_for_runtime();
        let err = dispatch_flat_action("nope__x", &serde_json::json!({}), &reg, &ctx, None)
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("`nope__x`"),
            "message should name the offending tool, got: {msg}"
        );
        assert!(
            msg.contains("not available to this agent"),
            "message should frame it as a grant issue, got: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("do not retry"),
            "message should tell the agent not to retry, got: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("delegate"),
            "message should suggest delegation, got: {msg}"
        );
    }

    #[test]
    fn ungranted_capability_error_is_actionable_and_names_the_tool() {
        // Live regression: the CRO agent (no `browser` grant) emitted
        // `pack:browser(...)`, which fell through to the compiled arm and failed
        // with the bare "is not a compiled pack" — so it retried and the run was
        // cancelled. The replacement must name the tool, frame it as a grant
        // issue, name the calling agent when known, and steer to delegate/stop.
        let err = ungranted_capability_error("browser", Some("cro"));
        let msg = format!("{err}");
        assert!(msg.contains("`browser`"), "names the tool, got: {msg}");
        assert!(
            msg.contains("not available to this agent"),
            "grant-framed, got: {msg}"
        );
        assert!(
            msg.contains("`tools` grant does not include it"),
            "explains it is a grant gap, got: {msg}"
        );
        assert!(msg.contains("agent `cro`"), "names the agent, got: {msg}");
        assert!(
            msg.to_lowercase().contains("do not retry"),
            "tells the agent not to retry, got: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("delegate"),
            "steers to delegation, got: {msg}"
        );

        // Fail-safe: no agent id → still a clear, actionable message (no dangling
        // "(agent ``)" fragment).
        let anon = format!("{}", ungranted_capability_error("browser", None));
        assert!(anon.contains("`browser`") && anon.contains("not available to this agent"));
        assert!(
            !anon.contains("agent ``"),
            "no empty agent fragment: {anon}"
        );
    }

    // === DuckDB old-flow vs new-flow equivalence ===
    //
    // DuckDB is an `primitive` pack with `provider_name: duckdb`
    // (embedded_pack_defs/duckdb.yaml). In the OLD flow the outer LLM picks the
    // coarse `duckdb` tool, a nested LLM translates intent into a `query`
    // primitive, and that primitive lowers to `ExecutableAction::DuckDb` and runs
    // through `DuckDbCapabilityProvider::execute`. In the NEW (flat) flow the
    // outer LLM picks the `duckdb__query` leaf directly and
    // `dispatch_flat_action` routes it — LLM-less — to the SAME provider via
    // `CompiledProviderDispatcher`. These tests pin that the flat wrapper reaches
    // the identical native executor and preserves DuckDB's session semantics.

    /// Build a registry holding the real embedded duckdb pack def AND a live
    /// `DuckDbCapabilityProvider` registered under `duckdb` — the minimum the
    /// flat compiled-provider-primitive path needs to actually execute.
    fn duckdb_registry() -> Arc<CapabilityRegistry> {
        let pack = embedded_compiled_pack_defs()
            .into_iter()
            .find(|p| p.name == "duckdb")
            .expect("embedded duckdb pack def present");
        let reg = CapabilityRegistry::new();
        reg.set_pack_definition("duckdb", pack.clone());
        let provider = DuckDbCapabilityProvider::new().expect("duckdb provider init (bundled dep)");
        reg.register(Arc::new(provider.with_pack_def(pack)));
        Arc::new(reg)
    }

    #[tokio::test]
    async fn duckdb_flat_dispatch_matches_direct_provider_execution() {
        let sql = "SELECT 42 AS answer, 'hello' AS label";

        // NEW (flat) path: `duckdb__query` leaf dispatched without an inner LLM.
        let reg = duckdb_registry();
        let ctx = PrimitiveExecCtx::default_for_runtime();
        let flat = dispatch_flat_action(
            "duckdb__query",
            &serde_json::json!({ "sql": sql }),
            &reg,
            &ctx,
            None,
        )
        .await
        .expect("flat duckdb dispatch");

        // OLD-flow terminal: what the nested inner-loop LLM ultimately produces —
        // an `ExecutableAction::DuckDb` run straight through the same provider.
        // `DuckDbAction::query` defaults to the same `output_format: "json"` that
        // `lower_duckdb_action` (the flat path's lowering) defaults to, so the two
        // actions are byte-for-byte equivalent before execution.
        let provider = DuckDbCapabilityProvider::new().expect("duckdb provider init");
        let direct = provider
            .execute(
                &ExecutableAction::DuckDb(DuckDbAction::query(sql)),
                None,
                30,
            )
            .await
            .expect("direct duckdb execute");

        let flat_text = flat.as_text().expect("flat result carries text");
        let direct_text = direct.as_text().expect("direct result carries text");
        assert_eq!(
            flat_text, direct_text,
            "flat-mode `duckdb__query` and inner-loop-terminal provider execution \
             must produce identical output"
        );
        assert!(
            flat_text.contains("42") && flat_text.contains("hello"),
            "expected the query result in output, got: {flat_text}"
        );
    }

    #[tokio::test]
    async fn duckdb_flat_dispatch_preserves_session_state_across_calls() {
        // The provider holds a persistent in-memory `DuckDbSession`. Two flat
        // dispatches against the SAME registry hit the SAME provider instance, so
        // a table created in the first call must be visible in the second —
        // proving flat mode does not reset DuckDB session state per primitive.
        let reg = duckdb_registry();
        let ctx = PrimitiveExecCtx::default_for_runtime();

        dispatch_flat_action(
            "duckdb__query",
            &serde_json::json!({ "sql": "CREATE TABLE t AS SELECT 7 AS v" }),
            &reg,
            &ctx,
            None,
        )
        .await
        .expect("flat create-table dispatch");

        let select = dispatch_flat_action(
            "duckdb__query",
            &serde_json::json!({ "sql": "SELECT v FROM t" }),
            &reg,
            &ctx,
            None,
        )
        .await
        .expect("flat select-from-table dispatch");

        let text = select.as_text().expect("select result carries text");
        assert!(
            text.contains('7'),
            "expected persisted row value 7 from the prior flat call, got: {text}"
        );
    }

    // === Phase 6: end-to-end flat browser dispatch (fake agent-browser CLI) ===

    /// A fake `agent-browser` CLI: handles the CDP `connect`, returns an
    /// accessibility tree for `snapshot`, and writes a PNG into
    /// `AGENT_BROWSER_SCREENSHOT_DIR` for `screenshot`. No live Chrome needed.
    const FAKE_AGENT_BROWSER_CLI: &str = r#"#!/bin/sh
for a in "$@"; do
  case "$a" in
    connect|open) exit 0 ;;
    snapshot) printf '%s\n' '- button "Submit" [ref=e1]'; exit 0 ;;
    screenshot)
      dir="${AGENT_BROWSER_SCREENSHOT_DIR:-/tmp}"
      mkdir -p "$dir"
      printf '\211PNG\r\n\032\n' > "$dir/flat-shot.png"
      printf '%s\n' "screenshot: $dir/flat-shot.png"
      exit 0 ;;
  esac
done
exit 0
"#;

    #[tokio::test]
    async fn flat_browser_snapshot_is_text_and_screenshot_carries_image() {
        // End-to-end flat browser dispatch through a fake agent-browser CLI:
        // classify_flat_route → dispatch_flat_action → dispatch_primitive
        // → dispatch_browser_primitive (session cache + BrowserDispatcher + the
        // argv translator) → fold. On-demand vision: `browser__snapshot` is text;
        // an explicit `browser__screenshot` carries the captured PNG path.
        use crate::magician_v2::execution::primitive_dispatch::browser::session::{
            flat_browser_session_ids_for_thread, forget_flat_browser_session,
        };

        // Dispatch caches under the bare override for an agent that declared no
        // `browser_transports`, and under `<override>--<ceiling>` for one that
        // did — so a `forget` of the bare id alone can leave a restricted
        // agent's cached handle and its staging dir behind for the rest of the
        // process. Sweep both, the same way teardown does.
        fn forget_every_variant(base: &str) {
            for id in flat_browser_session_ids_for_thread(base) {
                forget_flat_browser_session(&id);
            }
            forget_flat_browser_session(base);
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        // The scoped resolver looks for the CLI under
        // `<storage>/scopes/<principal>/<workspace>/skills/browser/bin/agent-browser`
        // (it ignores MAGICIAN_AGENT_BROWSER_CLI when a storage root is present).
        let (principal, workspace) = ("anon", "ws");
        let cli = tmp
            .path()
            .join("scopes")
            .join(principal)
            .join(workspace)
            .join("skills/browser/bin/agent-browser");
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, FAKE_AGENT_BROWSER_CLI).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let reg = {
            let r = CapabilityRegistry::new();
            r.set_pack_definition(
                "browser",
                pack("name: browser\nparameters: []\nnative_action_schemas:\n  snapshot:\n    parameters: []\n  screenshot:\n    parameters: []\nimplementation:\n  type: primitive\n"),
            );
            Arc::new(r)
        };

        let mut ctx = PrimitiveExecCtx::default_for_runtime();
        ctx.storage_base_path = tmp.path().to_path_buf();
        ctx.principal = Some(principal.to_string());
        ctx.workspace = Some(workspace.to_string());
        // Deterministic, collision-free session id (verbatim via the override),
        // so this test never shares the process-global session cache with others.
        let session_id = "flat-browser-e2e-test".to_string();
        ctx.browser_session_id_override = Some(session_id.clone());
        forget_every_variant(&session_id); // clean slate

        // snapshot → text (accessibility tree); no image.
        let snap = dispatch_flat_action(
            "browser__snapshot",
            &serde_json::json!({ "args": [] }),
            &reg,
            &ctx,
            None,
        )
        .await
        .expect("flat browser snapshot dispatch");
        assert!(
            snap.as_text().is_some_and(|t| t.contains("Submit")),
            "snapshot should surface the accessibility text, got: {snap:?}"
        );

        // screenshot → Browser result carrying the captured PNG path (on-demand vision).
        let shot = dispatch_flat_action(
            "browser__screenshot",
            &serde_json::json!({ "args": [] }),
            &reg,
            &ctx,
            None,
        )
        .await
        .expect("flat browser screenshot dispatch");
        match shot {
            ActionResult::Browser { data } => {
                let path = data
                    .get("screenshot_path")
                    .and_then(|v| v.as_str())
                    .expect("screenshot_path present in Browser result");
                assert!(
                    std::path::Path::new(path).exists(),
                    "captured screenshot file should exist at dispatch time: {path}"
                );
            },
            other => panic!("expected ActionResult::Browser with a screenshot, got {other:?}"),
        }

        forget_every_variant(&session_id); // evict cache + remove staging dir
    }

    // === Phase 7: end-to-end flat dispatch of a governed CLI package ===

    /// Real governed `awk` package. The action catalog and argv lowering are
    /// projected from this single source; no legacy schema participates.
    const AWK_SKILL: &str = include_str!("../../../../../skillshub/awk/SKILL.md");

    #[cfg(unix)]
    #[tokio::test]
    async fn flat_governed_cli_pack_dispatches_subprocess() {
        use std::os::unix::fs::PermissionsExt;

        // End-to-end governed dispatch: SKILL.md → typed catalog projection →
        // flat action → universal runtime → real `awk` subprocess.
        let skill_dir = std::path::Path::new("skillshub/awk");
        let manifest = crate::magician_v2::skills::loader::parse_manifest(AWK_SKILL, skill_dir)
            .expect("awk skill manifest");
        let package = parse_skill_runtime_package(AWK_SKILL)
            .expect("awk runtime package parses")
            .expect("awk runtime package exists");
        let pack_def = project_runtime_package_to_pack(
            &manifest.name,
            &manifest.description,
            None,
            skill_dir,
            skill_dir,
            package,
        )
        .expect("awk governed package projects");

        let tmp = tempfile::tempdir().expect("tempdir");
        // macOS exposes tempfile roots through `/var` while their canonical
        // spelling is `/private/var`. Production scope authority correctly
        // rejects aliases, so keep this end-to-end fixture canonical too.
        let temp_root = std::fs::canonicalize(tmp.path()).expect("canonical temp root");
        let (principal, workspace) = ("anon", "ws");
        let scope_root = temp_root.join("scopes").join(principal).join(workspace);
        let scoped_skill_dir = scope_root.join("skills/awk");
        std::fs::create_dir_all(&scoped_skill_dir).unwrap();
        let input = scope_root.join("data.txt");
        std::fs::write(&input, "hello world\nfoo bar\n").unwrap();
        // Keep the fixture independent of macOS arm64e code-signing rules for
        // privately snapshotted system binaries. This test-only executable is
        // still launched through the governed single-file boundary and then
        // delegates to the host awk so argv/output behavior remains real.
        let fixture_bin = temp_root.join("bin");
        std::fs::create_dir(&fixture_bin).unwrap();
        let fixture_awk = fixture_bin.join("awk");
        std::fs::write(&fixture_awk, "#!/bin/sh\nexec /usr/bin/awk \"$@\"\n").unwrap();
        std::fs::set_permissions(&fixture_awk, std::fs::Permissions::from_mode(0o700)).unwrap();

        let reg = {
            let r = CapabilityRegistry::new();
            r.set_pack_definition("awk", pack_def);
            Arc::new(r)
        };
        let mut ctx = PrimitiveExecCtx::default_for_runtime();
        ctx.storage_base_path = temp_root.clone();
        ctx.principal = Some(principal.to_string());
        ctx.workspace = Some(workspace.to_string());
        ctx.governed_executable_directory = Some(fixture_bin);
        ctx.secret_store = Some(Arc::new(
            crate::magician_v2::secrets::SecretStore::new_empty(
                Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
                temp_root.join("secrets"),
            ),
        ));

        let result = dispatch_flat_action(
            "awk__run",
            &serde_json::json!({
                "program": "{print $1}",
                "input_file": input.display().to_string(),
                "flags": "",
            }),
            &reg,
            &ctx,
            None,
        )
        .await
        .expect("flat awk dispatch");

        let out = result.as_text().expect("awk result carries text");
        assert!(
            out.contains("hello") && out.contains("foo"),
            "awk should print the first column of each line, got: {out}"
        );
    }
}
