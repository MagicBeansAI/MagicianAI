//! Compiled (built-in) capability providers wrapping existing tool dispatch.
//!
//! Each provider is a thin adapter implementing `CapabilityProvider` that delegates
//! to the existing lowering and execution functions. This preserves the exact
//! behavior of the original hardcoded dispatch while making it pluggable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::{
    DateTime, Duration as ChronoDuration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc,
};
use chrono_tz::Tz;
use runtime_core::{FileSandboxConfig, ShellSandboxConfig};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::timeout;
use tool_runtime_core::{
    action_overrides::{compile_typed_action_overrides, TypedActionStdin, TypedArgumentMapping},
    manifest::{
        AuthRequirement, AuthStorage, InjectionSource, InjectionTarget, ProfileSelection,
        RuntimeProtocol,
    },
    manifest_parser::{
        parse_skill_runtime_package, SkillRuntimePackage, SkillRuntimeSpendMetadata,
    },
    manifest_validation::validate_skill_runtime_contract,
    mcp_catalog_projection::project_mcp_catalog,
    registry::types::ToolDefinition,
};
use tracing::{debug, error, info, warn};

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{
    CapabilityAuthConfig, CapabilityPackDefinition, CapabilityProvider, CapabilityRegistry,
    ChatInlineAdapter, CommandArgMapping, ExecutionMetadata, ImplementationType,
    NativeActionSchemaDef, ParameterDef, ParameterType, SpendDeclaration,
};
use super::error::ExecutionError;
use super::lowering;
use super::magicutor_client::MagicutorClient;
use super::native_executors::{
    execute_bash_action, execute_file_action, execute_http_action, validate_file_read_path,
};
use super::treasurer_provider::TreasurerCapabilityProvider;
use crate::magician_v2::artifact_v2::capabilities::CapabilityScopePaths;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::secrets::{SecretBroker, SecretRuntimeCapabilities};
use crate::magician_v2::strategy::plan::PlanStep;
use crate::magician_v2::subprocess_owners;

/// One row per compiled-provider name. `deferred = true` means the
/// provider's backing instance is wired late — after
/// `ScopedCapabilityResolver::set_agent_resources(...)` +
/// `set_compiled_handlers(...)` have installed the handler registry
/// — so the early-boot registry-rebuild cleanup loop must keep the
/// pack definition around rather than warning about an "unbound"
/// provider. `deferred = false` means the provider has an always-on
/// Rust-native backing (`FileCapabilityProvider`, `DuckDbCapabilityProvider`,
/// etc.) that binds at the standard build time.
///
/// Phase 0.8c-12 consolidation: previously two arrays
/// (`KNOWN_COMPILED_PROVIDERS` + `DEFERRED_COMPILED_PROVIDERS`)
/// where the second was a subset of the first. The subset
/// relationship was fragile — a new handler-registered tool added
/// to KNOWN but forgotten in DEFERRED would be silently pruned
/// during early-boot rebuilds (and indeed `delete_task` +
/// `get_task_details` had this bug pre-consolidation). Single
/// source of truth eliminates the drift.
const COMPILED_PROVIDERS: &[(&str, bool)] = &[
    // Always-on Rust-native providers (struct-based; bind at registry
    // build time).
    ("analyze_image_via_openai", false),
    ("catchup_merge", false),
    ("delegation_files", false),
    ("delegation_shell", false),
    ("duckdb", false),
    ("files", false),
    ("http", false),
    ("imessage", false),
    ("meeting", false),
    ("search", false),
    ("shell", false),
    ("time_math", false),
    ("treasurer", false),
    ("vector", false),
    // Handler-registered providers (use `GenericCompiledProvider`;
    // bind late, after `set_compiled_handlers` installs the registry).
    ("activate_skill", true),
    ("append_note", true),
    ("apply_patch", true),
    ("ask_owner", true),
    ("create_agent", true),
    ("create_dashboard", true),
    ("create_monitor", true),
    ("create_note", true),
    ("create_proposal", true),
    ("create_task", true),
    ("open_note", true),
    ("open_pr", true),
    ("search_notes", true),
    ("android_snapshot", true),
    ("android_act", true),
    ("android_screenshot", true),
    ("android_app", true),
    ("android_notifications", true),
    ("preview_monitor", true),
    ("save_selection_to_note", true),
    ("publish_task_to_note", true),
    ("update_monitor", true),
    ("propose_program_missions", true),
    ("review_program_missions", true),
    ("deactivate_skill", true),
    ("delete_task", true),
    ("deploy_app", true),
    ("distill_evidence", true),
    ("edit_file", true),
    ("agent_roster_data", true),
    ("tasks_data", true),
    ("notes_data", true),
    ("memory_data", true),
    ("evidence_data", true),
    ("meetings_data", true),
    ("evaluate_harness", true),
    ("find_agents_for_capability", true),
    ("forget_memory", true),
    ("get_active_executions", true),
    ("get_agent_details", true),
    ("get_execution_history", true),
    ("get_task_details", true),
    ("glob", true),
    ("grep", true),
    ("imessage_send", true),
    ("inspect_backlog_delivery", true),
    ("inspect_agent", true),
    ("internal_data", true),
    ("list_agents", true),
    ("list_artifacts", true),
    ("list_episodes", true),
    ("list_memory_tiers", true),
    ("list_proposals", true),
    ("list_scheduled_tasks", true),
    ("list_tasks", true),
    ("macos_automation", true),
    ("magician_work_ledger", true),
    ("notify_owner", true),
    ("promote_backlog_item", true),
    ("replay_recipe", true),
    ("propose_backlog_item", true),
    ("review_backlog_delivery", true),
    ("propose_meeting", true),
    ("read_file", true),
    ("read_program_state", true),
    ("read_trace", true),
    ("reassign_task", true),
    ("refine_task", true),
    ("request_owner_action", true),
    ("retire_agent", true),
    ("run_coding_task", true),
    ("contribute_to_project", true),
    ("content_read", true),
    ("authorize_content_read", true),
    ("content_search", true),
    ("working_set_search", true),
    ("working_set_read", true),
    ("apply_code_proposal", true),
    ("run_project_checks", true),
    ("screenshot_preview", true),
    ("capture_reference", true),
    ("run_task", true),
    ("save_preference", true),
    ("search_memory", true),
    ("app_action_compose", true),
    ("app_action_invoke", true),
    ("app_data_query", true),
    ("app_data_search", true),
    ("app_data_compose", true),
    ("app_discover", true),
    ("app_memory_propose", true),
    ("stop_task", true),
    ("switch_personality", true),
    ("system_status", true),
    ("task_state", true),
    ("thinking_maps_data", true),
    ("tool_search", true),
    ("unpublish_dashboard", true),
    ("update_agent", true),
    ("update_delegation", true),
    ("update_memory_tier", true),
    ("update_program_state", true),
    ("update_task", true),
    ("web_fetch", true),
    ("web_search", true),
    // Provider-grounded answer lane (server-side search via LLM transport);
    // distinct from the free DuckDuckGo `web_search` list tool.
    ("web_answer", true),
    ("write_file", true),
    ("media_edit", true),
    ("media_edit_status", true),
];

/// Does the binary recognize this `provider_name`? Used by
/// `prune_unexecutable_pack_defs` at boot to drop pack defs that
/// reference a provider this binary can't dispatch.
fn is_known_compiled_provider(name: &str) -> bool {
    COMPILED_PROVIDERS.iter().any(|(n, _)| *n == name)
}

/// Is this provider's binding late (after `set_compiled_handlers`)?
/// Used by the registry-build cleanup to skip the "unbound provider"
/// warning for handler-based providers during early-boot rebuilds.
fn is_deferred_compiled_provider(name: &str) -> bool {
    COMPILED_PROVIDERS
        .iter()
        .any(|(n, deferred)| *n == name && *deferred)
}

const IMESSAGE_TOOL_NAME: &str = "imessage";
const TIME_MATH_TOOL_NAME: &str = "time_math";
const MEETING_TOOL_NAME: &str = "meeting";
const ANALYZE_IMAGE_VIA_OPENAI_TOOL_NAME: &str = "analyze_image_via_openai";
const APPLE_EPOCH_UNIX_SECONDS: i64 = 978_307_200;

// Compiled-pack names for the chat-native tools migrated from
// explicit `dispatch_chat_tool_call` arms.
const SEARCH_MEMORY_TOOL_NAME: &str = "search_memory";
const FORGET_MEMORY_TOOL_NAME: &str = "forget_memory";
const UPDATE_MEMORY_TIER_TOOL_NAME: &str = "update_memory_tier";
const DISTILL_EVIDENCE_TOOL_NAME: &str = "distill_evidence";
const SAVE_PREFERENCE_TOOL_NAME: &str = "save_preference";
const CREATE_DASHBOARD_TOOL_NAME: &str = "create_dashboard";
const UNPUBLISH_DASHBOARD_TOOL_NAME: &str = "unpublish_dashboard";
const SWITCH_PERSONALITY_TOOL_NAME: &str = "switch_personality";
const ACTIVATE_SKILL_TOOL_NAME: &str = "activate_skill";
const DEACTIVATE_SKILL_TOOL_NAME: &str = "deactivate_skill";
const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";
const APPLY_PATCH_TOOL_NAME: &str = "apply_patch";
const EDIT_FILE_TOOL_NAME: &str = "edit_file";
const GLOB_TOOL_NAME: &str = "glob";
const GREP_TOOL_NAME: &str = "grep";
const WEB_FETCH_TOOL_NAME: &str = "web_fetch";
const WEB_SEARCH_TOOL_NAME: &str = "web_search";
const READ_FILE_TOOL_NAME: &str = "read_file";
const WRITE_FILE_TOOL_NAME: &str = "write_file";
const LIST_AGENTS_TOOL_NAME: &str = "list_agents";
// Handler-backed introspection / capability-routing meta tools. Both are
// `type: compiled` self-provider packs force-offered to the model via the flat
// catalog when the agent has delegation targets (`flat_loop/catalog.rs`), so
// each MUST get a provider wired in `build_compiled_registry` — without one,
// the call dispatches to "is not a compiled pack".
const GET_AGENT_DETAILS_TOOL_NAME: &str = "get_agent_details";
const FIND_AGENTS_FOR_CAPABILITY_TOOL_NAME: &str = "find_agents_for_capability";
const LIST_MEMORY_TIERS_TOOL_NAME: &str = "list_memory_tiers";
const LIST_SCHEDULED_TASKS_TOOL_NAME: &str = "list_scheduled_tasks";
const LIST_ARTIFACTS_TOOL_NAME: &str = "list_artifacts";
const GET_ACTIVE_EXECUTIONS_TOOL_NAME: &str = "get_active_executions";
const GET_EXECUTION_HISTORY_TOOL_NAME: &str = "get_execution_history";
// Task-mutator batch (chat-side; `create_task` / `run_task` stay as
// explicit arms because they need the real chat session id +
// ui_thread_id for status cards + progress subscriptions).
const LIST_TASKS_TOOL_NAME: &str = "list_tasks";
const GET_TASK_DETAILS_TOOL_NAME: &str = "get_task_details";
const STOP_TASK_TOOL_NAME: &str = "stop_task";
const REFINE_TASK_TOOL_NAME: &str = "refine_task";
const UPDATE_TASK_TOOL_NAME: &str = "update_task";
const DELETE_TASK_TOOL_NAME: &str = "delete_task";
// Session-coupled task ops — chat fast path injects `__chat_session_id`
// into args; the bridge hydrates the live session from `chat_store`
// before forwarding to the legacy dispatchers.
const CREATE_TASK_TOOL_NAME: &str = "create_task";
const RUN_TASK_TOOL_NAME: &str = "run_task";
const APP_DATA_TOOL_NAMES: [&str; 7] = [
    "app_action_compose",
    "app_action_invoke",
    "app_data_query",
    "app_data_search",
    "app_data_compose",
    "app_discover",
    "app_memory_propose",
];

// ============================================================================
// Phase 0.8c — Dynamic compiled-handler registry
//
// Replaces ~50 lines of per-tool struct + impl boilerplate with a
// single generic provider + a name→handler map. Adding a new compiled
// tool is now:
//
//   1. YAML in `embedded_pack_defs/<name>.yaml` (declares schema)
//   2. An async function: `fn handle_xxx(resources, args) -> Value`
//   3. One registration line at the bottom of
//      `default_compiled_handler_registry()`
//   4. An entry in `COMPILED_PROVIDERS` with `true` (binds late),
//      and the `include_str!` line in the embedded pack list. Miss step 4
//      and the pack loads, finds no provider of its name, and is pruned
//      from the catalog with a single warn — the tool simply is not there,
//      which reads like a model that will not call it.
//
// The single `GenericCompiledProvider` type handles:
// - `tool_name()` — returns the name passed to it
// - `lower()` — reuses `pack_provider::maybe_wrap_with_spend_gate`
// - `default_timeout_secs()` — reads from pack_def
// - `execute()` — extracts args, calls the registered handler,
//   wraps the returned `Value` in `ActionResult::Text`
//
// Same ergonomics as adding a CliTemplate skill — drop a YAML, add an
// async function, register it. No struct, no trait impl per tool.
// ============================================================================

/// Async handler signature for compiled tools. Takes the shared agent
/// resources + the tool's args, returns a JSON `Value` to ship back
/// to the LLM. Errors propagate via `ExecutionError`.
pub type CompiledHandler = Arc<
    dyn Fn(
            Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
            Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Value, ExecutionError>> + Send>,
        > + Send
        + Sync,
>;

/// Helper to wrap an async fn into the `CompiledHandler` signature.
/// Use inside the registry's `register()` calls so each entry stays
/// concise.
#[macro_export]
macro_rules! compiled_handler {
    ($fn_name:path) => {
        std::sync::Arc::new(
            |resources: std::sync::Arc<
                $crate::magician_v2::execution::agent_resources::AgentResources,
            >,
             args: serde_json::Value| {
                Box::pin($fn_name(resources, args))
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = Result<
                                        serde_json::Value,
                                        $crate::magician_v2::execution::error::ExecutionError,
                                    >,
                                > + Send,
                        >,
                    >
            },
        )
    };
}

/// Maps compiled tool name → handler. Populated once at boot in
/// `default_compiled_handler_registry()` and consulted by
/// `GenericCompiledProvider::execute()` at dispatch time.
#[derive(Clone, Default)]
pub struct CompiledHandlerRegistry {
    handlers: HashMap<String, CompiledHandler>,
}

impl std::fmt::Debug for CompiledHandlerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledHandlerRegistry")
            .field("handler_count", &self.handlers.len())
            .field(
                "handler_names",
                &self.handlers.keys().cloned().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl CompiledHandlerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler under `name`. Last-wins if a name collides.
    pub fn register(&mut self, name: impl Into<String>, handler: CompiledHandler) {
        self.handlers.insert(name.into(), handler);
    }

    /// Look up a handler by tool name. Returns `None` when there's no
    /// registered handler — caller falls back to the legacy
    /// per-struct provider path.
    pub fn get(&self, name: &str) -> Option<CompiledHandler> {
        self.handlers.get(name).cloned()
    }

    /// Enumerate the registered handler names — useful for boot
    /// diagnostics ("here's what's available in this scope") and for
    /// the `build_compiled_registry` migration loop that turns each
    /// handler into a `GenericCompiledProvider`.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.handlers.keys().map(|s| s.as_str())
    }
}

/// One provider type for every compiled tool that's been migrated to
/// the handler-registry pattern. Holds the tool name, the resolved
/// handler, the shared `AgentResources` it'll pass through, and the
/// optional pack definition (for timeout + spend gating).
#[derive(Clone)]
pub struct GenericCompiledProvider {
    name: String,
    handler: CompiledHandler,
    resources: Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
    scope: Option<CapabilityScopePaths>,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for GenericCompiledProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenericCompiledProvider")
            .field("name", &self.name)
            .field(
                "has_pack_def",
                &self.pack_def.as_ref().map(|_| ()).is_some(),
            )
            .finish()
    }
}

impl GenericCompiledProvider {
    pub fn new(
        name: impl Into<String>,
        handler: CompiledHandler,
        resources: Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
        scope: Option<CapabilityScopePaths>,
    ) -> Self {
        Self {
            name: name.into(),
            handler,
            resources,
            scope,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for GenericCompiledProvider {
    fn tool_name(&self) -> &str {
        &self.name
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        // Re-uses the same lowering path the legacy per-tool
        // providers used — pack-def-driven param resolution + spend
        // gate construction. Decoupled here so the generic provider
        // doesn't need an `AgentBackendProviderShared`.
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: self.name.clone(),
            implementation: super::capability::ImplementationType::Compiled {
                provider_name: self.name.clone(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        session_id: Option<String>,
        _timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let mut args_map = match action {
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } if capability_name == &self.name => {
                let map: serde_json::Map<String, Value> =
                    resolved_params.clone().into_iter().collect();
                map
            },
            other => {
                return Err(ExecutionError::Step(format!(
                    "GenericCompiledProvider({}) received unexpected action shape: {:?}",
                    self.name, other
                )));
            },
        };

        // The invoked pack/tool identity is runtime-owned, just like scope.
        // Handlers that serve multiple generated aliases use this to bind an
        // alias to the server-selected resource instead of trusting a model-
        // supplied identifier that merely happened to have a schema default.
        args_map.insert(
            "__compiled_pack_name".to_string(),
            Value::String(self.name.clone()),
        );

        // Scope is server-owned runtime identity. Always overwrite values from
        // the resolved provider scope; never let forwarded/model arguments win.
        // `__agent_id` is deliberately not inferred from the public `agent_id`
        // argument because many tools use that public field as a lookup target.
        // Every execution surface must inject the active source agent explicitly.
        if let Some(scope) = self.scope.as_ref() {
            args_map.insert(
                "__principal".to_string(),
                Value::String(scope.principal.clone()),
            );
            args_map.insert(
                "__workspace".to_string(),
                Value::String(scope.workspace.clone()),
            );
        }
        if let Some(session_id) = session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            args_map
                .entry("__chat_session_id".to_string())
                .or_insert_with(|| Value::String(session_id.to_owned()));
        }

        // Honor the pack's documented `default_timeout_secs` (e.g.
        // macos_automation's 35) when the caller did not pass an explicit
        // `timeout_secs`. Handlers only read `args.timeout_secs`, so the
        // resolved pack/override default would otherwise be silently dropped.
        // `or_insert` keeps an explicit LLM-supplied arg winning.
        //
        // Mark the back-fill. Without this a handler cannot tell "the caller
        // asked for 1200 seconds" from "nobody asked and the pack default was
        // stamped in", so a handler with its own configured default — like
        // `run_coding_task`, whose turn budget lives in `coding.*` — would find
        // an argument always present and its configuration silently unreachable.
        if !args_map.contains_key("timeout_secs") {
            args_map.insert(
                "timeout_secs".to_string(),
                Value::from(self.default_timeout_secs()),
            );
            args_map.insert(PACK_DEFAULT_TIMEOUT_MARKER.to_string(), Value::Bool(true));
        }

        let value = (self.handler)(self.resources.clone(), Value::Object(args_map)).await?;
        // Marshal the JSON return into an `ActionResult::Text`.
        let serialized = serde_json::to_string(&value).map_err(|err| {
            ExecutionError::Step(format!(
                "compiled handler serialized response failed: {err}"
            ))
        })?;
        Ok(ActionResult::text(serialized))
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 15)
    }
}

// Silence the unused-variable warning on `scope` — it's plumbed
// through for parity with the legacy provider constructors. Once a
// handler genuinely needs scope-driven path resolution, this gets
// promoted to a real field used inside `execute()`.
#[allow(dead_code)]
fn _generic_provider_scope_accessor(p: &GenericCompiledProvider) -> Option<&CapabilityScopePaths> {
    p.scope.as_ref()
}

/// Build the canonical compiled-handler registry — populated with
/// every tool that has been migrated to the `GenericCompiledProvider`
/// pattern. Called once at boot in `bin/magician.rs` and installed
/// onto `ScopedCapabilityResolver` via `set_compiled_handlers`.
///
/// Each migration adds one line here, points at a freestanding async
/// function, and deletes the old struct+impl block from this file.
/// Empty initially — migrations populate it one by one.
pub fn default_compiled_handler_registry() -> CompiledHandlerRegistry {
    let mut registry = CompiledHandlerRegistry::new();
    registry.register(
        "media_edit",
        crate::compiled_handler!(crate::magician_v2::execution::media_edit::handle_media_edit),
    );
    registry.register(
        "media_edit_status",
        crate::compiled_handler!(
            crate::magician_v2::execution::media_edit::handle_media_edit_status
        ),
    );
    registry.register(
        "switch_personality",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::switch_personality::handle
        ),
    );
    registry.register(
        "propose_program_missions",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::propose_program_missions::handle
        ),
    );
    registry.register(
        "review_program_missions",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::review_program_missions::handle
        ),
    );
    registry.register(
        "activate_skill",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::activate_skill::handle
        ),
    );
    registry.register(
        "deactivate_skill",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::deactivate_skill::handle
        ),
    );
    registry.register(
        "list_memory_tiers",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::list_memory_tiers::handle
        ),
    );
    registry.register(
        "list_agents",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::list_agents::handle
        ),
    );
    registry.register(
        "list_scheduled_tasks",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::list_scheduled_tasks::handle
        ),
    );
    registry.register(
        "list_artifacts",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::list_artifacts::handle
        ),
    );
    registry.register(
        "replay_recipe",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::replay_recipe::handle
        ),
    );
    registry.register(
        "get_active_executions",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::get_active_executions::handle
        ),
    );
    registry.register(
        "get_execution_history",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::get_execution_history::handle
        ),
    );
    registry.register(
        "get_task_details",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::get_task_details::handle
        ),
    );
    registry.register(
        "list_tasks",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::list_tasks::handle
        ),
    );
    registry.register(
        "search_memory",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::search_memory::handle
        ),
    );
    registry.register(
        "app_action_compose",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::app_action_compose::handle
        ),
    );
    registry.register(
        "app_action_invoke",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::app_action_invoke::handle
        ),
    );
    registry.register(
        "app_data_query",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::app_data_query::handle
        ),
    );
    registry.register(
        "app_data_search",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::app_data_search::handle
        ),
    );
    registry.register(
        "app_data_compose",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::app_data_compose::handle
        ),
    );
    registry.register(
        "app_discover",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::app_discover::handle
        ),
    );
    registry.register(
        "app_memory_propose",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::app_memory_propose::handle
        ),
    );
    registry.register(
        "save_preference",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::save_preference::handle
        ),
    );
    registry.register(
        "update_memory_tier",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::update_memory_tier::handle
        ),
    );
    registry.register(
        "distill_evidence",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::distill_evidence::handle
        ),
    );
    registry.register(
        "forget_memory",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::forget_memory::handle
        ),
    );
    registry.register(
        "create_dashboard",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::create_dashboard::handle
        ),
    );
    registry.register(
        "unpublish_dashboard",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::unpublish_dashboard::handle
        ),
    );
    registry.register(
        "create_task",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::create_task::handle
        ),
    );
    // Recurring Monitors Phase 4 — the chat tool trio (plan §8). Preview is
    // deterministic and store-free; create/update dispatch to the SAME
    // ArtifactV2Service paths the /v3/monitors HTTP routes use.
    registry.register(
        "preview_monitor",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::preview_monitor::handle
        ),
    );
    registry.register(
        "create_monitor",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::create_monitor::handle
        ),
    );
    // Notes tools. Phase 4 shipped the publishing layer behind HTTP with no
    // tool in front of it, so the capability existed and no agent could reach
    // it.
    registry.register(
        "create_note",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::create_note::handle
        ),
    );
    registry.register(
        "append_note",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::append_note::handle
        ),
    );
    registry.register(
        "publish_task_to_note",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::publish_task_to_note::handle
        ),
    );
    registry.register(
        "open_note",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::open_note::handle
        ),
    );
    registry.register(
        "open_pr",
        crate::compiled_handler!(crate::magician_v2::execution::compiled_handlers::open_pr::handle),
    );
    registry.register(
        "android_snapshot",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::android_device::snapshot
        ),
    );
    registry.register(
        "android_act",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::android_device::act
        ),
    );
    registry.register(
        "android_screenshot",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::android_device::screenshot
        ),
    );
    registry.register(
        "android_notifications",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::android_device::notifications
        ),
    );
    registry.register(
        "android_app",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::android_device::app
        ),
    );
    registry.register(
        "search_notes",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::search_notes::handle
        ),
    );
    registry.register(
        "web_answer",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::web_answer::handle
        ),
    );
    registry.register(
        "save_selection_to_note",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::save_selection_to_note::handle
        ),
    );
    registry.register(
        "update_monitor",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::update_monitor::handle
        ),
    );
    // Owner-relay tools (chat-facing UserRequest emitters). These were
    // previously harness-only and so silently unreachable from chat; migrated
    // here so they dispatch in the reactive path like create_task.
    registry.register(
        "notify_owner",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::notify_owner::handle
        ),
    );
    registry.register(
        "ask_owner",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::ask_owner::handle
        ),
    );
    registry.register(
        "propose_meeting",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::propose_meeting::handle
        ),
    );
    registry.register(
        "request_owner_action",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::request_owner_action::handle
        ),
    );
    registry.register(
        "run_task",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::run_task::handle
        ),
    );
    registry.register(
        crate::magician_v2::execution::compiled_handlers::run_coding_task::TOOL_NAME,
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::run_coding_task::handle
        ),
    );
    registry.register(
        "contribute_to_project",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::contribute_to_project::handle
        ),
    );
    registry.register(
        "apply_code_proposal",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::apply_code_proposal::handle
        ),
    );
    registry.register(
        "run_project_checks",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::run_project_checks::handle
        ),
    );
    registry.register(
        "capture_reference",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::capture_reference::handle
        ),
    );
    registry.register(
        "screenshot_preview",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::screenshot_preview::handle
        ),
    );
    registry.register(
        "stop_task",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::stop_task::handle
        ),
    );
    registry.register(
        "refine_task",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::refine_task::handle
        ),
    );
    registry.register(
        "update_task",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::update_task::handle
        ),
    );
    registry.register(
        "delete_task",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::delete_task::handle
        ),
    );
    registry.register(
        "deploy_app",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::deploy_app::handle
        ),
    );
    registry.register(
        "get_agent_details",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::get_agent_details::handle
        ),
    );
    registry.register(
        "find_agents_for_capability",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::find_agents_for_capability::handle
        ),
    );
    registry.register(
        "tool_search",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::tool_search::handle
        ),
    );
    registry.register(
        "apply_patch",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::apply_patch::handle
        ),
    );
    registry.register(
        "edit_file",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::edit_file::handle
        ),
    );
    registry.register(
        "glob",
        crate::compiled_handler!(crate::magician_v2::execution::compiled_handlers::glob::handle),
    );
    registry.register(
        "grep",
        crate::compiled_handler!(crate::magician_v2::execution::compiled_handlers::grep::handle),
    );
    registry.register(
        "content_read",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::content_read::handle
        ),
    );
    registry.register(
        "authorize_content_read",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::authorize_content_read::handle
        ),
    );
    registry.register(
        "content_search",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::content_search::handle
        ),
    );
    registry.register(
        "working_set_search",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::working_sets::search
        ),
    );
    registry.register(
        "working_set_read",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::working_sets::read
        ),
    );
    registry.register(
        "web_fetch",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::web_fetch::handle
        ),
    );
    registry.register(
        "web_search",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::web_search::handle
        ),
    );
    registry.register(
        "read_file",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::read_file::handle
        ),
    );
    registry.register(
        "write_file",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::write_file::handle
        ),
    );
    registry.register(
        "macos_automation",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::macos_automation::handle
        ),
    );
    registry.register(
        "imessage_send",
        crate::compiled_handler!(
            crate::magician_v2::execution::compiled_handlers::imessage_send::handle
        ),
    );
    registry
}

// ============================================================================
// File
// ============================================================================

#[derive(Debug)]
pub struct FileCapabilityProvider {
    sandbox: FileSandboxConfig,
    pack_def: Option<CapabilityPackDefinition>,
}

impl FileCapabilityProvider {
    pub fn new(sandbox: FileSandboxConfig) -> Self {
        Self {
            sandbox,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for FileCapabilityProvider {
    fn tool_name(&self) -> &str {
        "files"
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, serde_json::Value>) -> bool {
        prove_bound_file_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let step = resolve_step_params(&self.pack_def, step)?;
        let action = lowering::lower_file_action(&step)?;
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &step.parameters,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        match action {
            ExecutableAction::File(file_action) => {
                let duration = std::time::Duration::from_secs(timeout_secs);
                timeout(duration, execute_file_action(file_action, &self.sandbox))
                    .await
                    .map_err(|_| {
                        ExecutionError::Step(format!(
                            "File action timed out after {}s",
                            timeout_secs
                        ))
                    })?
            },
            _ => Err(ExecutionError::Step(
                "FileCapabilityProvider received non-file action".to_string(),
            )),
        }
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 30)
    }
}

fn prove_bound_file_args(parameters: &HashMap<String, serde_json::Value>) -> bool {
    let Some(selected) = parameters
        .get("action")
        .or_else(|| parameters.get("operation"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
    else {
        return false;
    };
    let selected = selected.as_str();
    if !matches!(selected, "read" | "write") {
        return false;
    }
    let Some(path) = parameters.get("path").and_then(Value::as_str) else {
        return false;
    };
    if crate::magician_v2::apps::bound_path::normalize_app_relative_path(std::path::Path::new(path))
        .is_err()
    {
        return false;
    }
    for (key, value) in parameters {
        match key.as_str() {
            "path" => {},
            "action" | "operation" | "__action_name" => {
                if value
                    .as_str()
                    .and_then(|value| {
                        crate::magician_v2::apps::app_tool_bind::normalize_app_action_selector(
                            "files", value,
                        )
                    })
                    .as_deref()
                    != Some(selected)
                {
                    return false;
                }
            },
            "encoding" if selected == "read" => {
                if value.as_str().is_none_or(|value| {
                    !matches!(value.trim().to_ascii_lowercase().as_str(), "utf8" | "utf-8")
                }) {
                    return false;
                }
            },
            "content" if selected == "write" => {
                if value.as_str().is_none_or(|content| {
                    u64::try_from(content.len()).ok().is_none_or(|size| {
                        size > crate::magician_v2::apps::bound_path::APP_BOUND_FILE_CONTENT_CEILING
                    })
                }) {
                    return false;
                }
            },
            "create_dirs" if selected == "write" => {
                if !value.is_boolean() {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }
    selected == "read" || parameters.get("content").and_then(Value::as_str).is_some()
}

// ============================================================================
// HTTP
// ============================================================================

#[derive(Debug)]
pub struct HttpCapabilityProvider {
    pack_def: Option<CapabilityPackDefinition>,
}

impl Default for HttpCapabilityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpCapabilityProvider {
    pub fn new() -> Self {
        Self { pack_def: None }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for HttpCapabilityProvider {
    /// Outbound HTTP is exactly the case the default drops on the floor: a lost
    /// response is indistinguishable from a request that never arrived, and the
    /// remote is the only party that can tell them apart. Send it a derived key
    /// so it can.
    async fn execute_with_effect(
        &self,
        action: &ExecutableAction,
        session_id: Option<String>,
        timeout_secs: u64,
        effect_id: Option<&str>,
    ) -> Result<ActionResult, ExecutionError> {
        match action {
            ExecutableAction::Http(http_action) => {
                let duration = std::time::Duration::from_secs(timeout_secs);
                timeout(duration, execute_http_action(http_action, effect_id))
                    .await
                    .map_err(|_| {
                        ExecutionError::Step(format!(
                            "HTTP action timed out after {}s",
                            timeout_secs
                        ))
                    })?
            },
            _ => self.execute(action, session_id, timeout_secs).await,
        }
    }

    fn tool_name(&self) -> &str {
        "http"
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, serde_json::Value>) -> bool {
        prove_bound_http_get_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let step = resolve_step_params(&self.pack_def, step)?;
        let tool = step.tool.as_deref().unwrap_or("http_get");
        let action = lowering::lower_http_action(&step, tool)?;
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &step.parameters,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        match action {
            ExecutableAction::Http(http_action) => {
                let duration = std::time::Duration::from_secs(timeout_secs);
                // Reached only when a caller uses the identity-free `execute`.
                // The `execute_with_effect` override below is the attributable
                // path and is what the compiled dispatcher calls.
                timeout(duration, execute_http_action(http_action, None))
                    .await
                    .map_err(|_| {
                        ExecutionError::Step(format!(
                            "HTTP action timed out after {}s",
                            timeout_secs
                        ))
                    })?
            },
            _ => Err(ExecutionError::Step(
                "HttpCapabilityProvider received non-HTTP action".to_string(),
            )),
        }
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 30)
    }
}

fn prove_bound_http_get_args(parameters: &HashMap<String, serde_json::Value>) -> bool {
    let Some(url) = parameters.get("url").and_then(Value::as_str) else {
        return false;
    };
    if url.is_empty() || url.len() > 8 * 1024 {
        return false;
    }
    for (key, value) in parameters {
        match key.as_str() {
            "url" => {},
            "method" => {
                if value
                    .as_str()
                    .is_none_or(|method| !method.eq_ignore_ascii_case("GET"))
                {
                    return false;
                }
            },
            "operation" | "action" | "__action_name" => {
                if value.as_str().is_none_or(|operation| {
                    !matches!(
                        operation.trim().to_ascii_lowercase().as_str(),
                        "get" | "http_get"
                    )
                }) {
                    return false;
                }
            },
            "headers" => {
                let Some(raw) = value.as_str() else {
                    return false;
                };
                if raw.len() > 32 * 1024
                    || serde_json::from_str::<HashMap<String, String>>(raw).is_err()
                {
                    return false;
                }
            },
            "timeout" | "timeout_secs" => {
                let parsed = value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()));
                if parsed.is_none_or(|seconds| !(1..=120).contains(&seconds)) {
                    return false;
                }
            },
            "follow_redirects" => {
                if !value.is_boolean()
                    && value
                        .as_str()
                        .is_none_or(|raw| !matches!(raw, "true" | "false"))
                {
                    return false;
                }
            },
            // Runtime-owned correlation fields are not interpreted by the HTTP
            // lowerer or transport. Model-origin `__*` keys were stripped
            // before these values were injected.
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }
    true
}

// ============================================================================
// DuckDB
// ============================================================================

pub struct DuckDbCapabilityProvider {
    /// Shared session. Wrapped in RwLock so we can replace it after a timeout poisons the mutex.
    session: Arc<tokio::sync::RwLock<Arc<super::native_executors::DuckDbSession>>>,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for DuckDbCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuckDbCapabilityProvider").finish()
    }
}

impl DuckDbCapabilityProvider {
    pub fn new() -> Result<Self, super::error::ExecutionError> {
        let session = super::native_executors::DuckDbSession::new()?;
        Ok(Self {
            session: Arc::new(tokio::sync::RwLock::new(Arc::new(session))),
            pack_def: None,
        })
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }

    /// Replace the session with a fresh one after its blocking task panics.
    async fn reset_session(&self) {
        let replacement =
            tokio::task::spawn_blocking(super::native_executors::DuckDbSession::new).await;
        match replacement {
            Ok(Ok(new_session)) => {
                let mut guard = self.session.write().await;
                *guard = Arc::new(new_session);
                warn!(
                    "DuckDB session reset after task panic — previous session's temp tables are lost"
                );
            },
            Ok(Err(e)) => {
                error!("Failed to reset DuckDB session: {}", e);
            },
            Err(join_error) => {
                error!("DuckDB session reset task panicked: {}", join_error);
            },
        }
    }
}

#[async_trait]
impl CapabilityProvider for DuckDbCapabilityProvider {
    fn tool_name(&self) -> &str {
        "duckdb"
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, serde_json::Value>) -> bool {
        prove_bound_table_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let step = resolve_step_params(&self.pack_def, step)?;
        let action = lowering::lower_duckdb_action(&step)?;
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &step.parameters,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        match action {
            ExecutableAction::DuckDb(duckdb_action) => {
                let session = self.session.read().await.clone();
                let action = duckdb_action.clone();
                // Honor per-action timeout_secs if set, otherwise use provider default.
                let effective_timeout = duckdb_action.timeout_secs.unwrap_or(timeout_secs);
                let duration = std::time::Duration::from_secs(effective_timeout);

                // Run the complete guarded operation on the blocking pool. The
                // native executor includes guard wait time in the deadline and
                // retains the guard until DuckDB observes any interrupt and exits.
                let result = tokio::task::spawn_blocking(move || {
                    super::native_executors::execute_duckdb_action_with_timeout(
                        &action, &session, duration,
                    )
                })
                .await;

                match result {
                    Ok(inner) => inner,
                    Err(join_err) => {
                        // spawn_blocking panicked — mutex is likely poisoned.
                        self.reset_session().await;
                        Err(ExecutionError::Step(format!(
                            "DuckDB task panicked: {}. Session was reset.",
                            join_err
                        )))
                    },
                }
            },
            _ => Err(ExecutionError::Step(
                "DuckDbCapabilityProvider received non-DuckDB action".to_string(),
            )),
        }
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 120)
    }
}

fn prove_bound_table_args(parameters: &HashMap<String, serde_json::Value>) -> bool {
    let Some(operation) = parameters.get("__action_name").and_then(Value::as_str) else {
        return false;
    };
    if !matches!(operation, "preview" | "describe") {
        return false;
    }
    let Some(source) = parameters.get("source").and_then(Value::as_str) else {
        return false;
    };
    if crate::magician_v2::apps::bound_path::normalize_app_relative_path(std::path::Path::new(
        source,
    ))
    .is_err()
    {
        return false;
    }
    for (key, value) in parameters {
        match key.as_str() {
            "source" | "__action_name" => {},
            "operation" | "action" => {
                if value
                    .as_str()
                    .and_then(|value| {
                        crate::magician_v2::apps::app_tool_bind::normalize_app_action_selector(
                            "duckdb", value,
                        )
                    })
                    .as_deref()
                    != Some(operation)
                {
                    return false;
                }
            },
            "limit" if operation == "preview" => {
                if value
                    .as_u64()
                    .is_none_or(|limit| !(1..=1000).contains(&limit))
                {
                    return false;
                }
            },
            "output_format" => {
                if value
                    .as_str()
                    .is_none_or(|format| format.trim().to_ascii_lowercase().as_str() != "json")
                {
                    return false;
                }
            },
            "timeout_secs" => {
                let timeout = value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()));
                if timeout.is_none_or(|timeout| !(1..=120).contains(&timeout)) {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }
    true
}

// ============================================================================
// Time Math
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeMathInvocation {
    operation: String,
    #[serde(default)]
    start_date: Option<String>,
    #[serde(default)]
    end_date: Option<String>,
    #[serde(default)]
    end_inclusive: Option<bool>,
    #[serde(default)]
    timezone: Option<String>,
}

pub struct TimeMathCapabilityProvider {
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for TimeMathCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimeMathCapabilityProvider")
            .field("pack_def", &self.pack_def)
            .finish()
    }
}

impl Default for TimeMathCapabilityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeMathCapabilityProvider {
    pub fn new() -> Self {
        Self { pack_def: None }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for TimeMathCapabilityProvider {
    fn tool_name(&self) -> &str {
        TIME_MATH_TOOL_NAME
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, serde_json::Value>) -> bool {
        prove_time_math_args(parameters)
    }

    fn attest_app_tool_target(
        &self,
        tool_ref: crate::magician_v2::apps::models::AppReference,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Option<crate::magician_v2::apps::tool_disclosure::AttestedAppToolTarget> {
        let operation = parameters.get("operation").and_then(|value| value.as_str());
        let plan = crate::magician_v2::apps::app_tool_bind::plan_app_tool_call(
            TIME_MATH_TOOL_NAME,
            operation,
            crate::magician_v2::apps::app_tool_bind::AppToolContainProfile::InProcessCompiled,
        );
        if !plan.runnable || !self.prove_app_tool_args(parameters) {
            return None;
        }
        plan.mint(tool_ref)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: TIME_MATH_TOOL_NAME.to_string(),
            implementation: super::capability::ImplementationType::Compiled {
                provider_name: TIME_MATH_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        _timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let resolved_params = match action {
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } if capability_name == TIME_MATH_TOOL_NAME => resolved_params,
            _ => {
                return Err(ExecutionError::Step(
                    "TimeMathCapabilityProvider received a non-time_math action".to_string(),
                ))
            },
        };

        let invocation: TimeMathInvocation = serde_json::from_value(serde_json::Value::Object(
            resolved_params
                .clone()
                .into_iter()
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        ))
        .map_err(|err| ExecutionError::Step(format!("invalid time_math params: {}", err)))?;

        let output = execute_time_math(invocation)?;
        Ok(ActionResult::text(output))
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 5)
    }
}

/// Resolved params for the `meeting` compiled tool.
#[derive(Debug, Deserialize)]
struct MeetingInvocation {
    action: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    wake_phrases: Option<Vec<String>>,
    /// Calendar event title, when the agent found the link on an event. Keys the
    /// bot's chat thread (`meeting-<slug(title)>-<date>`); omit → the Meet code.
    #[serde(default)]
    title: Option<String>,
    /// Scheduled meeting date `YYYY-MM-DD`, when known. Omit → the join date.
    #[serde(default)]
    date: Option<String>,
}

/// Compiled provider for the `meeting` tool: dispatches join/status/leave/list
/// to the process-global `MeetingSessionManager`. See
/// `docs/components/magician/realtime-media-rails.md` (Meeting participant bot).
pub struct MeetingCapabilityProvider {
    pack_def: Option<CapabilityPackDefinition>,
    /// `(resources, principal, workspace)` for the calling agent's scope, used to
    /// build a `ScopedMeetingMemoryWriter` so the finished meeting's takeaways are
    /// persisted to that scope's memory. `None` (the default) → a no-op writer.
    memory_ctx: Option<(
        Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
        String,
        String,
    )>,
}

impl std::fmt::Debug for MeetingCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeetingCapabilityProvider")
            .field("pack_def", &self.pack_def)
            .field("has_memory_ctx", &self.memory_ctx.is_some())
            .finish()
    }
}

impl Default for MeetingCapabilityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MeetingCapabilityProvider {
    pub fn new() -> Self {
        Self {
            pack_def: None,
            memory_ctx: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }

    /// Inject the calling agent's scope so a finished meeting's takeaways are
    /// written to that scope's user memory tier on teardown.
    pub fn with_memory_context(
        mut self,
        resources: Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
        principal: String,
        workspace: String,
    ) -> Self {
        self.memory_ctx = Some((resources, principal, workspace));
        self
    }

    /// Build the Phase-2 browser-join seam for a `join`. On macOS and Linux,
    /// resolve the pinned agent-browser CLI (scope-aware when the provider has
    /// a memory context; scope-blind env/extras fallback otherwise) and return
    /// an `AgentBrowserMeetJoiner` with a UNIQUE per-join `--user-data-dir`
    /// (the disambiguator `pgrep` keys PID discovery off). On any resolution
    /// failure — or on another OS — return `NoopBrowserJoin` so the manual-join
    /// path (riding an already-open browser) still works.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn build_browser_joiner(&self) -> Arc<dyn crate::magician_v2::media_seam::BrowserJoin> {
        use crate::magician_v2::execution::primitive_dispatch::browser::session::{
            resolve_browser_engine_plan, AgentBrowserSession, BrowserEnginePlan, ConnectionMode,
        };
        use crate::magician_v2::media_seam::{AgentBrowserMeetJoiner, NoopBrowserJoin};

        // Scope-aware resolution when we have the agent's context (the same
        // resolver `dispatch`/`cleanup` use: storage root + principal + workspace),
        // else the scope-blind resolver (env override / extras) for detached calls.
        let cli_path = match &self.memory_ctx {
            Some((res, principal, workspace)) => AgentBrowserSession::resolve_cli_path_for_scope(
                Some(res.artifact_workspace.base_root()),
                Some(principal.as_str()),
                Some(workspace.as_str()),
            ),
            None => {
                tracing::warn!(
                    target: "meet_bot",
                    "meeting join has no agent scope; resolving agent-browser CLI scope-blind \
                     (env override / extras) for auto-join"
                );
                AgentBrowserSession::resolve_cli_path(None)
            },
        };

        match cli_path {
            Ok(cli_path) => {
                // Persist a signed-in Google profile so Workspace meetings the bot
                // is invited to auto-admit (Phase 4). Resolution order:
                //   1. `MEET_BOT_PROFILE_DIR` env override (highest priority, e.g.
                //      to pin a custom path or force ephemeral via a temp path);
                //   2. a STABLE per-scope dir `<scope workdir root>/meet-bot-profile`
                //      whenever we have agent scope — so NO startup env var is needed
                //      and the signed-in profile persists across restarts. It lives in
                //      the scope tree next to the bot's other session data (e.g. the
                //      WhatsApp `workdirs/home/.wu` session) and is gitignored;
                //   3. a per-join unique temp dir only for detached / scope-blind joins
                //      (anonymous + concurrency-safe).
                // A stable dir means one meeting at a time (two browsers can't share
                // one `--user-data-dir`) — the right trade-off for a single signed-in
                // identity; concurrent anonymous joins ride the temp-dir fallback.
                let user_data_dir = std::env::var("MEET_BOT_PROFILE_DIR")
                    .ok()
                    .filter(|dir| !dir.trim().is_empty())
                    .unwrap_or_else(|| match &self.memory_ctx {
                        Some((res, principal, workspace)) => subprocess_owners::workdirs_root(
                            &res.artifact_workspace,
                            principal.as_str(),
                            workspace.as_str(),
                        )
                        .join("meet-bot-profile")
                        .to_string_lossy()
                        .into_owned(),
                        None => std::env::temp_dir()
                            .join(format!("magician-meet-{}", uuid::Uuid::new_v4()))
                            .to_string_lossy()
                            .into_owned(),
                    });
                // Launch the same configured engine the regular `browser` tool resolves.
                // With no agent scope there is no configured engine, so agent-browser's
                // bundled browser applies. An explicit but invalid engine fails closed.
                let engine_plan = match &self.memory_ctx {
                    Some((res, principal, workspace)) => {
                        let browser_engine = res
                            .magician_config_snapshot()
                            .content_acquisition
                            .browser
                            .engine;
                        match resolve_browser_engine_plan(
                            res.artifact_workspace.base_root(),
                            principal.as_str(),
                            workspace.as_str(),
                            &ConnectionMode::Headed,
                            browser_engine.as_deref(),
                        ) {
                            Ok(engine_plan) => engine_plan,
                            Err(error) => {
                                tracing::warn!(
                                    target: "meet_bot",
                                    %error,
                                    "configured browser engine is unavailable; disabling automatic meeting join"
                                );
                                return Arc::new(NoopBrowserJoin);
                            },
                        }
                    },
                    None => BrowserEnginePlan::default(),
                };
                Arc::new(AgentBrowserMeetJoiner::new(
                    cli_path,
                    user_data_dir,
                    engine_plan,
                    self.memory_ctx.as_ref().map(|(resources, principal, workspace)| {
                        crate::magician_v2::browser_engine_analytics::BrowserEngineAnalyticsContext::for_scope(
                            resources.artifact_workspace.base_root(),
                            principal,
                            workspace,
                            None,
                            None,
                        ).with_work("meeting_join", "meeting-join")
                    }),
                ))
            },
            Err(error) => {
                tracing::warn!(
                    target: "meet_bot",
                    %error,
                    "could not resolve agent-browser CLI for meeting auto-join; \
                     falling back to manual-join (ride an already-open browser)"
                );
                Arc::new(NoopBrowserJoin)
            },
        }
    }

    /// Other hosts ride an already-open browser. macOS and Linux build
    /// `AgentBrowserMeetJoiner` above.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn build_browser_joiner(&self) -> Arc<dyn crate::magician_v2::media_seam::BrowserJoin> {
        Arc::new(crate::magician_v2::media_seam::NoopBrowserJoin)
    }
}

/// Start an agent-attendee meeting join with the scope-aware pieces the
/// compiled `meeting` tool builds: the scoped memory writer (teardown takeaways
/// land in the agent's memory), the browser joiner (scope-pinned agent-browser
/// CLI + cloak engine + persistent profile), and the chat-lane scope (the
/// per-meeting thread resolves in the agent's chat store). SINGLE owner shared
/// by the tool's `execute` and the `/meetings` API so the entry points can't
/// drift.
pub async fn join_meeting_with_scope(
    memory_ctx: Option<(
        Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
        String,
        String,
    )>,
    config: crate::magician_v2::media_seam::MeetingConfig,
    origin: crate::magician_v2::media_seam::meeting::CaptureControlOrigin,
) -> Result<String, String> {
    use crate::magician_v2::media_seam::meeting::{
        record_capture_control_detached, CaptureControlOutcome, CaptureControlRecord,
        CaptureControlVerb,
    };
    use crate::magician_v2::media_seam::{
        meeting_manager, BrowserJoin, MarkerContext, MeetingMemoryWriter, NoopMeetingMemoryWriter,
        ScopedMeetingMemoryWriter,
    };
    let writer: Arc<dyn MeetingMemoryWriter> = match &memory_ctx {
        Some((res, principal, workspace)) => Arc::new(ScopedMeetingMemoryWriter::new(
            res.clone(),
            principal.clone(),
            workspace.clone(),
        )),
        None => Arc::new(NoopMeetingMemoryWriter),
    };
    let provider =
        match &memory_ctx {
            Some((res, principal, workspace)) => MeetingCapabilityProvider::new()
                .with_memory_context(res.clone(), principal.clone(), workspace.clone()),
            None => MeetingCapabilityProvider::new(),
        };
    let browser: Arc<dyn BrowserJoin> = provider.build_browser_joiner();
    let scope = memory_ctx
        .as_ref()
        .map(|(_, principal, workspace)| (principal.clone(), workspace.clone()));
    // Crash marker: lets the boot sweep explain an interrupted capture in-thread
    // after a server death. Scoped runs only (no scope → no thread to post to).
    let marker = memory_ctx.as_ref().map(|(res, principal, workspace)| {
        MarkerContext::for_scope(
            subprocess_owners::workdirs_root(&res.artifact_workspace, principal, workspace),
            principal,
            workspace,
        )
    });
    // Runtime broadcaster (from the invoking agent's resources) so the meeting
    // bot's per-turn realtime cost/latency lands in the `llm_calls` analytics
    // ledger, like chat + browser/desktop voice. `None` for scope-blind joins.
    let broadcaster = memory_ctx
        .as_ref()
        .and_then(|(res, _, _)| res.event_broadcaster.clone());
    // The audit sits HERE, on the shared path, not in each caller: `origin` is a
    // required argument, so a future caller cannot reach the attendee rail
    // without declaring which door it came through. Unscoped joins record
    // nothing — there is no scope whose log the row would belong to.
    let audited_scope = scope.clone();
    let audited_url = config.meet_url.clone();
    let audited_title = config.title.clone();
    let outcome = meeting_manager()
        .join(config, writer, browser, scope, marker, broadcaster)
        .await;
    // Both `Some` together or neither: the scope IS derived from `memory_ctx`
    // above, so this destructure never silently drops a row that should exist.
    if let (Some((principal, workspace)), Some((resources, _, _))) =
        (audited_scope, memory_ctx.as_ref())
    {
        let workdirs_root =
            subprocess_owners::workdirs_root(&resources.artifact_workspace, &principal, &workspace);
        record_capture_control_detached(
            &workdirs_root,
            CaptureControlRecord::new(
                origin,
                CaptureControlVerb::Join,
                match &outcome {
                    Ok(_) => CaptureControlOutcome::Accepted,
                    Err(_) => CaptureControlOutcome::Refused,
                },
                principal,
                workspace,
            )
            .with_session(outcome.as_ref().ok().cloned())
            .with_meeting(None, audited_title, Some(audited_url))
            .with_detail(outcome.as_ref().err().cloned()),
        );
    }
    outcome
}

#[async_trait]
impl CapabilityProvider for MeetingCapabilityProvider {
    fn tool_name(&self) -> &str {
        MEETING_TOOL_NAME
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: MEETING_TOOL_NAME.to_string(),
            implementation: super::capability::ImplementationType::Compiled {
                provider_name: MEETING_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        _timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        use crate::magician_v2::media_seam::{meeting_manager, MeetingConfig};

        let resolved_params = match action {
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } if capability_name == MEETING_TOOL_NAME => resolved_params,
            _ => {
                return Err(ExecutionError::Step(
                    "MeetingCapabilityProvider received a non-meeting action".to_string(),
                ))
            },
        };

        let invocation: MeetingInvocation = serde_json::from_value(serde_json::Value::Object(
            resolved_params
                .clone()
                .into_iter()
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        ))
        .map_err(|err| ExecutionError::Step(format!("invalid meeting params: {}", err)))?;

        let manager = meeting_manager();
        let out = match invocation.action.trim().to_ascii_lowercase().as_str() {
            "join" => {
                let url = invocation
                    .url
                    .clone()
                    .filter(|u| !u.trim().is_empty())
                    .ok_or_else(|| {
                        ExecutionError::Step("meeting join requires a `url` parameter".to_string())
                    })?;
                let mut config = MeetingConfig {
                    meet_url: url,
                    ..Default::default()
                };
                if let Some(name) = invocation.display_name.filter(|n| !n.trim().is_empty()) {
                    config.display_name = name;
                }
                if let Some(phrases) = invocation.wake_phrases {
                    let phrases: Vec<String> = phrases
                        .into_iter()
                        .filter(|p| !p.trim().is_empty())
                        .collect();
                    if !phrases.is_empty() {
                        config.wake_phrases = phrases;
                    }
                }
                // Calendar context that keys the bot's per-meeting chat thread.
                config.title = invocation.title.filter(|t| !t.trim().is_empty());
                config.meeting_date = invocation.date.filter(|d| !d.trim().is_empty());
                // Shared join path (scoped memory writer + browser joiner + chat-lane
                // scope) — same function the /meetings API calls, so the two entry
                // points can't drift.
                let session_id = join_meeting_with_scope(
                    self.memory_ctx.clone(),
                    config,
                    crate::magician_v2::media_seam::meeting::CaptureControlOrigin::CompiledMeetingTool,
                )
                .await
                .map_err(ExecutionError::Step)?;
                json!({ "session_id": session_id, "status": "joining" })
            },
            "status" => {
                let id = invocation
                    .session_id
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        ExecutionError::Step("meeting status requires a `session_id`".to_string())
                    })?;
                match manager.status(&id).await {
                    Some(view) => json!({
                        "session_id": view.session_id,
                        "status": format!("{:?}", view.status),
                        "url": view.url,
                        "latest_summary": view.latest_summary,
                    }),
                    None => json!({ "session_id": id, "status": "unknown" }),
                }
            },
            "leave" => {
                let id = invocation
                    .session_id
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        ExecutionError::Step("meeting leave requires a `session_id`".to_string())
                    })?;
                // `leave` awaits teardown (final resummarize + memory write), so the
                // returned summary is the post-teardown final, not the rolling one.
                let final_summary = manager.leave(&id).await.map_err(ExecutionError::Step)?;
                json!({ "session_id": id, "status": "left", "final_summary": final_summary })
            },
            "list" => {
                let rows = manager.list().await;
                let items: Vec<serde_json::Value> = rows
                    .into_iter()
                    .map(|row| {
                        json!({
                            "session_id": row.session_id,
                            "status": format!("{:?}", row.status),
                            "url": row.url,
                        })
                    })
                    .collect();
                serde_json::Value::Array(items)
            },
            other => {
                return Err(ExecutionError::Step(format!(
                    "invalid meeting action '{}'; expected join, status, leave, or list",
                    other
                )))
            },
        };

        Ok(ActionResult::text(
            serde_json::to_string(&out).unwrap_or_default(),
        ))
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 30)
    }
}

fn execute_time_math(invocation: TimeMathInvocation) -> Result<String, ExecutionError> {
    match invocation.operation.trim().to_ascii_lowercase().as_str() {
        "date_range" => execute_time_math_date_range(invocation),
        "now" => execute_time_math_now(invocation),
        other => Err(ExecutionError::Step(format!(
            "invalid time_math operation '{}'; expected date_range or now",
            other
        ))),
    }
}

fn execute_time_math_now(invocation: TimeMathInvocation) -> Result<String, ExecutionError> {
    let (timezone, timezone_label) = parse_time_math_timezone(invocation.timezone.as_deref())?;
    let now = Utc::now().with_timezone(&timezone);
    let now_date = now.date_naive();
    let now_json = time_math_instant_json(now_date, now)?;
    let output = json!({
        "operation": "now",
        "timezone": timezone_label,
        "now": now_json,
        "warnings": [],
    });
    serialize_time_math_output(&output)
}

fn prove_time_math_args(parameters: &HashMap<String, serde_json::Value>) -> bool {
    let Ok(value) = serde_json::to_value(parameters) else {
        return false;
    };
    let Ok(invocation) = serde_json::from_value::<TimeMathInvocation>(value) else {
        return false;
    };
    let operation = invocation.operation.trim().to_ascii_lowercase();
    if invocation
        .timezone
        .as_ref()
        .is_some_and(|timezone| timezone.is_empty() || timezone.len() > 64)
    {
        return false;
    }
    match operation.as_str() {
        "date_range"
            if parameters.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "operation" | "start_date" | "end_date" | "timezone" | "end_inclusive"
                )
            }) =>
        {
            execute_time_math_date_range(invocation).is_ok()
        },
        "now"
            if parameters
                .keys()
                .all(|key| matches!(key.as_str(), "operation" | "timezone")) =>
        {
            parse_time_math_timezone(invocation.timezone.as_deref()).is_ok()
        },
        _ => false,
    }
}

fn execute_time_math_date_range(invocation: TimeMathInvocation) -> Result<String, ExecutionError> {
    let (timezone, timezone_label) = parse_time_math_timezone(invocation.timezone.as_deref())?;
    let start_date = parse_time_math_date("start_date", invocation.start_date.as_deref())?;
    let end_date = parse_time_math_date("end_date", invocation.end_date.as_deref())?;
    if end_date < start_date {
        return Err(ExecutionError::Step(format!(
            "date_range end_date {} is before start_date {}",
            end_date, start_date
        )));
    }
    let end_inclusive = invocation.end_inclusive.unwrap_or(true);
    let end_exclusive_date = if end_inclusive {
        end_date
            .checked_add_signed(ChronoDuration::days(1))
            .ok_or_else(|| {
                ExecutionError::Step("end_date overflow while calculating date range".to_string())
            })?
    } else {
        end_date
    };

    if end_exclusive_date < start_date {
        return Err(ExecutionError::Step(format!(
            "date_range end boundary {} is before start_date {}",
            end_exclusive_date, start_date
        )));
    }

    let mut warnings = Vec::new();
    let (start_boundary, start_warning) =
        time_math_local_midnight(timezone, start_date, "start_date")?;
    if let Some(warning) = start_warning {
        warnings.push(warning);
    }
    let (end_boundary, end_warning) =
        time_math_local_midnight(timezone, end_exclusive_date, "end_exclusive")?;
    if let Some(warning) = end_warning {
        warnings.push(warning);
    }

    let start_unix_seconds = start_boundary.timestamp();
    let end_unix_seconds = end_boundary.timestamp();
    let start_apple_nanos = apple_absolute_nanoseconds(&start_boundary)?;
    let end_apple_nanos = apple_absolute_nanoseconds(&end_boundary)?;
    let start_json = time_math_instant_json(start_date, start_boundary)?;
    let end_exclusive_json = time_math_instant_json(end_exclusive_date, end_boundary)?;

    let output = json!({
        "operation": "date_range",
        "timezone": timezone_label,
        "range": {
            "kind": "half_open",
            "start_date": start_date.to_string(),
            "end_date": end_date.to_string(),
            "end_inclusive": end_inclusive,
            "end_exclusive_date": end_exclusive_date.to_string(),
        },
        "start": start_json,
        "end_exclusive": end_exclusive_json,
        "sql": {
            "range_kind": "half_open",
            "unix_seconds_predicate_template": "COLUMN >= start.unix_seconds AND COLUMN < end_exclusive.unix_seconds",
            "unix_seconds_predicate_example": format!(
                "COLUMN >= {} AND COLUMN < {}",
                start_unix_seconds, end_unix_seconds
            ),
            "apple_absolute_nanoseconds_predicate_template": "COLUMN >= start.apple_absolute.nanoseconds AND COLUMN < end_exclusive.apple_absolute.nanoseconds",
            "apple_absolute_nanoseconds_predicate_example": format!(
                "COLUMN >= {} AND COLUMN < {}",
                start_apple_nanos, end_apple_nanos
            ),
        },
        "warnings": warnings,
    });
    serialize_time_math_output(&output)
}

fn parse_time_math_timezone(timezone: Option<&str>) -> Result<(Tz, String), ExecutionError> {
    let value = timezone
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("UTC");
    let normalized = if value.eq_ignore_ascii_case("z") {
        "UTC"
    } else {
        value
    };
    let timezone = normalized.parse::<Tz>().map_err(|_| {
        ExecutionError::Step(format!(
            "invalid time_math timezone '{}'; expected an IANA timezone such as UTC, Asia/Kolkata, or America/New_York",
            value
        ))
    })?;
    Ok((timezone, normalized.to_string()))
}

fn parse_time_math_date(label: &str, value: Option<&str>) -> Result<NaiveDate, ExecutionError> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ExecutionError::Step(format!("time_math {} is required", label)))?;
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|err| {
        ExecutionError::Step(format!(
            "invalid time_math {} '{}'; expected YYYY-MM-DD: {}",
            label, value, err
        ))
    })
}

fn time_math_local_midnight(
    timezone: Tz,
    date: NaiveDate,
    label: &str,
) -> Result<(DateTime<Tz>, Option<String>), ExecutionError> {
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).expect("00:00:00 is valid");
    let local_datetime = date.and_time(midnight);
    match timezone.from_local_datetime(&local_datetime) {
        LocalResult::Single(datetime) => Ok((datetime, None)),
        LocalResult::Ambiguous(first, second) => {
            let chosen = if first.timestamp_millis() <= second.timestamp_millis() {
                first
            } else {
                second
            };
            Ok((
                chosen,
                Some(format!(
                    "{} local midnight was ambiguous in {}; selected the earliest boundary",
                    label, timezone
                )),
            ))
        },
        LocalResult::None => {
            for minutes in 1..=180 {
                let candidate = local_datetime + ChronoDuration::minutes(minutes);
                match timezone.from_local_datetime(&candidate) {
                    LocalResult::Single(datetime) => {
                        return Ok((
                            datetime,
                            Some(format!(
                                "{} local midnight did not exist in {}; selected the first valid local time {}",
                                label,
                                timezone,
                                candidate.format("%Y-%m-%d %H:%M:%S")
                            )),
                        ));
                    },
                    LocalResult::Ambiguous(first, second) => {
                        let chosen = if first.timestamp_millis() <= second.timestamp_millis() {
                            first
                        } else {
                            second
                        };
                        return Ok((
                            chosen,
                            Some(format!(
                                "{} local midnight did not exist in {}; selected the earliest valid ambiguous boundary {}",
                                label,
                                timezone,
                                candidate.format("%Y-%m-%d %H:%M:%S")
                            )),
                        ));
                    },
                    LocalResult::None => {},
                }
            }
            Err(ExecutionError::Step(format!(
                "could not resolve {} local midnight for {} in timezone {}",
                label, date, timezone
            )))
        },
    }
}

fn time_math_instant_json(
    date: NaiveDate,
    datetime: DateTime<Tz>,
) -> Result<Value, ExecutionError> {
    let utc = datetime.with_timezone(&Utc);
    let apple_nanoseconds = apple_absolute_nanoseconds(&datetime)?;
    Ok(json!({
        "date": date.to_string(),
        "local": datetime.to_rfc3339(),
        "utc": utc.to_rfc3339(),
        "unix_seconds": utc.timestamp(),
        "unix_millis": utc.timestamp_millis(),
        "apple_absolute": {
            "seconds": utc.timestamp() - APPLE_EPOCH_UNIX_SECONDS,
            "nanoseconds": apple_nanoseconds,
        },
    }))
}

fn apple_absolute_nanoseconds(datetime: &DateTime<Tz>) -> Result<i64, ExecutionError> {
    let utc = datetime.with_timezone(&Utc);
    let seconds = utc
        .timestamp()
        .checked_sub(APPLE_EPOCH_UNIX_SECONDS)
        .ok_or_else(|| {
            ExecutionError::Step("time_math Apple epoch seconds underflow".to_string())
        })?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(i64::from(utc.timestamp_subsec_nanos())))
        .ok_or_else(|| {
            ExecutionError::Step("time_math Apple epoch nanoseconds overflow".to_string())
        })
}

fn serialize_time_math_output(output: &Value) -> Result<String, ExecutionError> {
    serde_json::to_string_pretty(output).map_err(|err| {
        ExecutionError::Step(format!("failed to serialize time_math output: {}", err))
    })
}

// ============================================================================
// Analyze Image via OpenAI (compiled vision skill)
// ============================================================================

/// Parsed parameters from the LLM tool call.
#[derive(Debug, Clone, Deserialize)]
struct AnalyzeImageViaOpenAiInvocation {
    image_ref: String,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// Default lead-with-summary prompt so the first paragraph is voice-
/// friendly and the rest reads as structured detail on screen. Mirrors
/// the question default in `ChatService::dispatch_analyze_image`.
const ANALYZE_IMAGE_VIA_OPENAI_DEFAULT_QUESTION: &str =
    "Describe this image in detail. Note any visible text, objects, layout, \
     people, charts, code, UI elements, and other salient context. Lead with \
     a one-sentence summary in the first line, then a structured description.";

// Kept in step with the `op-analyze-image-openai-mini` profile in
// magician-config.yaml. `gpt-4o-mini` is delisted legacy; the profile moved
// to the current cheap vision tier on 2026-07-27 and this fallback follows,
// so a caller that omits `model` gets the same model the profile would use.
const ANALYZE_IMAGE_VIA_OPENAI_DEFAULT_MODEL: &str = "gpt-6-luna";
const ANALYZE_IMAGE_VIA_OPENAI_OPERATION: &str = "analyze_image_via_openai";
const ANALYZE_IMAGE_VIA_OPENAI_MAX_BYTES: u64 = 20 * 1024 * 1024;

pub struct AnalyzeImageViaOpenAiCapabilityProvider {
    pack_def: Option<CapabilityPackDefinition>,
    operation_llm_router:
        Option<Arc<crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter>>,
    telemetry: Option<
        crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext,
    >,
    file_sandbox: FileSandboxConfig,
}

impl std::fmt::Debug for AnalyzeImageViaOpenAiCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalyzeImageViaOpenAiCapabilityProvider")
            .field("pack_def", &self.pack_def)
            .field(
                "operation_llm_router",
                &self.operation_llm_router.as_ref().map(|_| "<configured>"),
            )
            .field("telemetry", &self.telemetry)
            .field("file_sandbox", &"<FileSandboxConfig>")
            .finish()
    }
}

impl Default for AnalyzeImageViaOpenAiCapabilityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalyzeImageViaOpenAiCapabilityProvider {
    pub fn new() -> Self {
        Self {
            pack_def: None,
            operation_llm_router: None,
            telemetry: None,
            file_sandbox: FileSandboxConfig::default(),
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }

    pub fn with_runtime_context(
        mut self,
        resources: &crate::magician_v2::execution::agent_resources::AgentResources,
        scope: &CapabilityScopePaths,
    ) -> Self {
        self.operation_llm_router = resources.operation_llm_router.clone();
        self.file_sandbox = resources.file_sandbox.clone();
        // NOT JOURNALLED BY THE AGENTIC LOOP'S OUTBOX, AND THAT IS A REFUSAL.
        //
        // Recorded here on 2026-08-28 because
        // `execution::agentic::run_loop::phases::outbox` keeps a census of
        // phase-reachable emission sites, and the third precondition for
        // cutting inline emission is that every one of them either journals or
        // carries a written-down refusal. This context was in neither half.
        //
        // A phase reaches it, and the sweep could not see how. `Apply`
        // dispatches this pack; `analyze_image_via_openai` then calls
        // `emit_native_validated_success` / `emit_native_validation_failure`,
        // which end at `operation_llm_telemetry`'s `emit_usage_outcome` and its
        // `emit_transport_only`. The census's sweep is a call-graph closure
        // that resolves identifiers to definitions, and this is a
        // `dyn CapabilityProvider` — a trait object, which is a dead end for
        // that method (its own *METHOD, AND WHAT IT CANNOT SEE*, blind spot 4).
        //
        // Refused rather than owed a diff, for the reason that file's header
        // gives: a capability provider holds no `AgenticContext`, no
        // `ActionExecutors` and no iteration. It is constructed from
        // `AgentResources` and a scope, and the same provider is dispatched
        // from paths that have no agentic run behind them at all. There is no
        // address here to record.
        self.telemetry = resources.event_broadcaster.as_ref().map(|broadcaster| {
            crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                scope.principal.clone(),
                scope.workspace.clone(),
                "compiled.analyze_image_via_openai",
            )
        });
        self
    }
}

#[async_trait]
impl CapabilityProvider for AnalyzeImageViaOpenAiCapabilityProvider {
    fn tool_name(&self) -> &str {
        ANALYZE_IMAGE_VIA_OPENAI_TOOL_NAME
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: ANALYZE_IMAGE_VIA_OPENAI_TOOL_NAME.to_string(),
            implementation: super::capability::ImplementationType::Compiled {
                provider_name: ANALYZE_IMAGE_VIA_OPENAI_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        _timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let resolved_params = match action {
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } if capability_name == ANALYZE_IMAGE_VIA_OPENAI_TOOL_NAME => resolved_params,
            _ => {
                return Err(ExecutionError::Step(
                    "AnalyzeImageViaOpenAiCapabilityProvider received a non-analyze-image action"
                        .to_string(),
                ));
            },
        };

        let invocation: AnalyzeImageViaOpenAiInvocation = serde_json::from_value(Value::Object(
            resolved_params
                .clone()
                .into_iter()
                .collect::<serde_json::Map<String, Value>>(),
        ))
        .map_err(|err| {
            ExecutionError::Step(format!("invalid analyze_image_via_openai params: {}", err))
        })?;

        let output = self.execute_analyze_image_via_openai(invocation).await?;
        Ok(ActionResult::text(output))
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 60)
    }
}

impl AnalyzeImageViaOpenAiCapabilityProvider {
    async fn execute_analyze_image_via_openai(
        &self,
        invocation: AnalyzeImageViaOpenAiInvocation,
    ) -> Result<String, ExecutionError> {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;

        let image_ref = invocation.image_ref.trim();
        if image_ref.is_empty() {
            return Err(ExecutionError::Step(
                "analyze_image_via_openai: image_ref is required".to_string(),
            ));
        }

        let question = invocation
            .question
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(ANALYZE_IMAGE_VIA_OPENAI_DEFAULT_QUESTION);
        let model = invocation
            .model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(ANALYZE_IMAGE_VIA_OPENAI_DEFAULT_MODEL);

        // Compiled provider runs outside the chat-session file index, so
        // image_ref MUST be an absolute filesystem path. Chat surfaces
        // that want session-file lookups continue to use the native
        // `analyze_image` tool (which has access to the session index).
        let path = PathBuf::from(image_ref);
        if !path.is_absolute() {
            return Err(ExecutionError::Step(format!(
            "analyze_image_via_openai: image_ref `{image_ref}` must be an absolute filesystem path \
             (compiled providers don't see the chat-session file index)"
        )));
        }
        if !path.exists() {
            return Err(ExecutionError::Step(format!(
                "analyze_image_via_openai: image path `{}` does not exist",
                path.display()
            )));
        }

        let mut effective_sandbox = self.file_sandbox.clone();
        if let Some(roots) =
            crate::magician_v2::execution::compiled_dispatch::current_session_file_sandbox_roots()
        {
            if let Ok(guard) = roots.lock() {
                effective_sandbox
                    .allowed_roots
                    .extend(guard.iter().cloned());
            }
        }
        validate_file_read_path(&path, &effective_sandbox)?;

        let mime = analyze_image_mime_from_extension(&path).ok_or_else(|| {
            ExecutionError::Step(format!(
                "analyze_image_via_openai: path `{}` does not have a recognized image extension \
             (jpg/png/gif/webp/heic/heif)",
                path.display()
            ))
        })?;

        let metadata = tokio::fs::metadata(&path).await.map_err(|err| {
            ExecutionError::Step(format!(
                "analyze_image_via_openai: failed to inspect `{}`: {err}",
                path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(ExecutionError::Step(format!(
                "analyze_image_via_openai: `{}` is not a regular file",
                path.display()
            )));
        }
        if metadata.len() > ANALYZE_IMAGE_VIA_OPENAI_MAX_BYTES {
            return Err(ExecutionError::Step(format!(
            "analyze_image_via_openai: `{}` is {} bytes; the maximum accepted image is {} bytes",
            path.display(),
            metadata.len(),
            ANALYZE_IMAGE_VIA_OPENAI_MAX_BYTES
        )));
        }

        let bytes = tokio::fs::read(&path).await.map_err(|err| {
            ExecutionError::Step(format!(
                "analyze_image_via_openai: failed to read `{}`: {err}",
                path.display()
            ))
        })?;

        let router = self.operation_llm_router.as_ref().ok_or_else(|| {
        ExecutionError::Configuration(
            "analyze_image_via_openai: operation LLM router is not configured; refusing an unobserved direct provider call"
                .to_string(),
        )
    })?;
        let telemetry = self.telemetry.as_ref().ok_or_else(|| {
        ExecutionError::Configuration(
            "analyze_image_via_openai: canonical LLM telemetry is not configured; refusing an unobserved provider call"
                .to_string(),
        )
    })?;
        let call_context =
        crate::magician_v2::execution::compiled_dispatch::current_compiled_llm_call_context()
            .ok_or_else(|| {
                ExecutionError::Configuration(
                    "analyze_image_via_openai: compiled dispatch did not supply valid tenant and call identity"
                        .to_string(),
                )
            })?;
        let image = crate::magician_v2::slot_graph::extraction::ImageData::new(
            STANDARD.encode(&bytes),
            mime.clone(),
        );
        let operation =
            crate::magician_v2::query_analysis::operation_llm_router::LLMOperation::Other(
                ANALYZE_IMAGE_VIA_OPENAI_OPERATION.to_string(),
            );
        let started = std::time::Instant::now();
        let response = router
            .generate_for_execution_native_tools_with_trace(
                &operation,
                None,
                question,
                Vec::new(),
                Some(model),
                Some(std::slice::from_ref(&image)),
                None,
                None,
                Some(call_context.trace_context.clone()),
            )
            .await;
        let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let attribution =
            crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution {
                execution_id: call_context.trace_context.execution_id.clone(),
                task_id: call_context.trace_context.task_id.clone(),
                agent_id: Some(call_context.agent_id.clone()),
                chat_session_id: call_context.trace_context.chat_session_id.clone(),
                ..Default::default()
            };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                return Err(ExecutionError::Step(format!(
                    "analyze_image_via_openai model request failed: {error:#}"
                )));
            },
        };
        let description = response.text.as_deref().unwrap_or_default().to_string();
        if description.trim().is_empty() {
            telemetry.emit_native_validation_failure(
                ANALYZE_IMAGE_VIA_OPENAI_OPERATION,
                &response,
                latency_ms,
                attribution,
                "vision_description_nonempty",
                "vision description was empty",
            );
            return Err(ExecutionError::Step(
                "analyze_image_via_openai: OpenAI returned an empty description".to_string(),
            ));
        }
        telemetry.emit_native_validated_success(
            ANALYZE_IMAGE_VIA_OPENAI_OPERATION,
            &response,
            latency_ms,
            attribution,
            "vision_description_nonempty",
        );

        let voice_summary = analyze_image_first_paragraph_summary(&description);
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string);
        let result = json!({
            "status": "ok",
            "image_ref": image_ref,
            "label": label,
            "media_type": mime,
            "question": question,
            "description": description,
            "voice_summary": voice_summary,
            "model": response.model.unwrap_or_else(|| model.to_string()),
            "provider": response.provider.unwrap_or_else(|| "openai".to_string()),
        });
        serde_json::to_string_pretty(&result).map_err(|err| {
            ExecutionError::Step(format!(
                "analyze_image_via_openai output serialization failed: {err}"
            ))
        })
    }
}

/// Map a filesystem path's extension to an OpenAI-acceptable image
/// mime. Mirrors the chat-side `mime_from_extension` helper but lives
/// here so the compiled provider has no chat-service dependency.
fn analyze_image_mime_from_extension(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg".to_string(),
        "png" => "image/png".to_string(),
        "gif" => "image/gif".to_string(),
        "webp" => "image/webp".to_string(),
        "heic" => "image/heic".to_string(),
        "heif" => "image/heif".to_string(),
        _ => return None,
    })
}

/// First paragraph of the model's description, truncated at a
/// sentence boundary at ~280 chars. The yaml prompt asks the model to
/// lead with a summary in the first line, so this gives voice a
/// short utterance without needing a second LLM call. Free function
/// inside this module — no leak into the chat-service helpers.
fn analyze_image_first_paragraph_summary(description: &str) -> String {
    let paragraph = description
        .split("\n\n")
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    // Reuse a tiny inline sentence-boundary truncator (~280 chars).
    const BUDGET: usize = 280;
    if paragraph.chars().count() <= BUDGET {
        return paragraph.to_string();
    }
    let mut last_boundary: Option<usize> = None;
    let mut count = 0usize;
    for (idx, ch) in paragraph.char_indices() {
        if count >= BUDGET {
            break;
        }
        if matches!(ch, '.' | '!' | '?') {
            let next_byte = idx + ch.len_utf8();
            if next_byte >= paragraph.len()
                || paragraph[next_byte..]
                    .chars()
                    .next()
                    .map(|c| c.is_whitespace())
                    .unwrap_or(true)
            {
                last_boundary = Some(next_byte);
            }
        }
        count += 1;
    }
    match last_boundary {
        Some(end) => paragraph[..end].trim().to_string(),
        None => {
            let mut end = 0;
            let mut count = 0;
            for (idx, _) in paragraph.char_indices() {
                if count >= BUDGET {
                    end = idx;
                    break;
                }
                count += 1;
            }
            if end == 0 {
                paragraph.to_string()
            } else {
                format!("{}…", &paragraph[..end].trim_end())
            }
        },
    }
}

// ============================================================================
// iMessage
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct ImessageInvocation {
    sql: String,
    #[serde(default)]
    output_format: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImessageOutputFormat {
    Json,
    Csv,
    Table,
}

pub struct ImessageCapabilityProvider {
    pack_def: Option<CapabilityPackDefinition>,
    db_path: PathBuf,
}

impl std::fmt::Debug for ImessageCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImessageCapabilityProvider")
            .field("db_path", &self.db_path)
            .field("pack_def", &self.pack_def)
            .finish()
    }
}

impl Default for ImessageCapabilityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ImessageCapabilityProvider {
    pub fn new() -> Self {
        Self {
            pack_def: None,
            db_path: resolve_messages_db_path(),
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for ImessageCapabilityProvider {
    fn tool_name(&self) -> &str {
        IMESSAGE_TOOL_NAME
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: IMESSAGE_TOOL_NAME.to_string(),
            implementation: super::capability::ImplementationType::Compiled {
                provider_name: IMESSAGE_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let resolved_params = match action {
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } if capability_name == IMESSAGE_TOOL_NAME => resolved_params,
            _ => {
                return Err(ExecutionError::Step(
                    "ImessageCapabilityProvider received a non-imessage action".to_string(),
                ))
            },
        };

        let invocation: ImessageInvocation = serde_json::from_value(serde_json::Value::Object(
            resolved_params
                .clone()
                .into_iter()
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        ))
        .map_err(|err| ExecutionError::Step(format!("invalid imessage params: {}", err)))?;

        let sql = normalize_imessage_sql_for_sqlite(&invocation.sql);
        validate_imessage_sql(&sql)?;

        if !cfg!(target_os = "macos") && !self.db_path.exists() {
            let format = parse_imessage_output_format(invocation.output_format.as_deref())?;
            let result = super::compiled_handlers::imessage_read::query(
                &sql,
                invocation.timeout_secs.unwrap_or(timeout_secs),
            )
            .await
            .map_err(ExecutionError::Step)?;
            let output = match format {
                ImessageOutputFormat::Json => {
                    format_imessage_rows_as_json(&result.columns, &result.rows)
                        .map_err(ExecutionError::Step)?
                },
                ImessageOutputFormat::Csv => {
                    format_imessage_rows_as_csv(&result.columns, &result.rows)
                },
                ImessageOutputFormat::Table => {
                    format_imessage_rows_as_table(&result.columns, &result.rows)
                },
            };
            return Ok(ActionResult::text(output));
        }

        if !self.db_path.exists() {
            return Err(ExecutionError::Step(format!(
                "Messages database not found at '{}'. On macOS this usually means Messages has not created chat.db or the process cannot see the user's Library path.",
                self.db_path.display()
            )));
        }

        let output_format = parse_imessage_output_format(invocation.output_format.as_deref())?;
        let effective_timeout = invocation.timeout_secs.unwrap_or(timeout_secs).max(1);
        let db_path = self.db_path.clone();
        let query_timeout = std::time::Duration::from_secs(effective_timeout);

        let query_task = tokio::task::spawn_blocking(move || {
            execute_imessage_sqlite_query(&db_path, &sql, output_format, query_timeout)
        });

        let output = timeout(query_timeout, query_task)
            .await
            .map_err(|_| {
                ExecutionError::Step(format!(
                    "iMessage SQLite query timed out after {}s",
                    effective_timeout
                ))
            })?
            .map_err(|err| ExecutionError::Step(format!("iMessage SQLite task failed: {}", err)))?
            .map_err(ExecutionError::Step)?;

        Ok(ActionResult::text(output))
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 30)
    }
}

fn parse_imessage_output_format(
    output_format: Option<&str>,
) -> Result<ImessageOutputFormat, ExecutionError> {
    match output_format
        .unwrap_or("json")
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => Ok(ImessageOutputFormat::Json),
        "csv" => Ok(ImessageOutputFormat::Csv),
        "table" => Ok(ImessageOutputFormat::Table),
        other => Err(ExecutionError::Step(format!(
            "invalid imessage output_format '{}'; expected json, csv, or table",
            other
        ))),
    }
}

fn execute_imessage_sqlite_query(
    db_path: &Path,
    sql: &str,
    output_format: ImessageOutputFormat,
    query_timeout: std::time::Duration,
) -> Result<String, String> {
    let started_at = std::time::Instant::now();
    let deadline = started_at + query_timeout;
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|err| format!("failed to open Messages database read-only: {}", err))?;
    conn.busy_timeout(query_timeout)
        .map_err(|err| format!("failed to set SQLite busy timeout: {}", err))?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .map_err(|err| format!("failed to enable SQLite read-only query mode: {}", err))?;
    conn.progress_handler(10_000, Some(move || std::time::Instant::now() >= deadline));

    let mut stmt = conn.prepare(sql).map_err(|err| {
        format_imessage_sqlite_error(
            "failed to prepare iMessage SQLite query",
            err,
            started_at,
            query_timeout,
        )
    })?;
    let columns: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(ToString::to_string)
        .collect();
    let mut rows = stmt.query([]).map_err(|err| {
        format_imessage_sqlite_error(
            "failed to execute iMessage SQLite query",
            err,
            started_at,
            query_timeout,
        )
    })?;
    let mut values = Vec::new();
    while let Some(row) = rows.next().map_err(|err| {
        format_imessage_sqlite_error(
            "failed to read iMessage SQLite row",
            err,
            started_at,
            query_timeout,
        )
    })? {
        let mut cells = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            cells
                .push(sqlite_value_to_json(row.get_ref(index).map_err(|err| {
                    format!("failed to read SQLite value: {}", err)
                })?));
        }
        values.push(cells);
    }

    Ok(match output_format {
        ImessageOutputFormat::Json => format_imessage_rows_as_json(&columns, &values)?,
        ImessageOutputFormat::Csv => format_imessage_rows_as_csv(&columns, &values),
        ImessageOutputFormat::Table => format_imessage_rows_as_table(&columns, &values),
    })
}

fn format_imessage_sqlite_error(
    context: &str,
    err: rusqlite::Error,
    started_at: std::time::Instant,
    query_timeout: std::time::Duration,
) -> String {
    if started_at.elapsed() >= query_timeout {
        format!("{}: timed out after {}s", context, query_timeout.as_secs())
    } else {
        format!("{}: {}", context, err)
    }
}

fn sqlite_value_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::from(value),
        ValueRef::Real(value) => Value::from(value),
        ValueRef::Text(value) => Value::String(String::from_utf8_lossy(value).into_owned()),
        ValueRef::Blob(value) => Value::String(format!("0x{}", hex::encode(value))),
    }
}

fn format_imessage_rows_as_json(columns: &[String], rows: &[Vec<Value>]) -> Result<String, String> {
    let values: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut object = serde_json::Map::with_capacity(columns.len());
            for (column, value) in columns.iter().zip(row.iter()) {
                object.insert(column.clone(), value.clone());
            }
            Value::Object(object)
        })
        .collect();
    serde_json::to_string(&values).map_err(|err| format!("failed to serialize JSON: {}", err))
}

fn format_imessage_rows_as_csv(columns: &[String], rows: &[Vec<Value>]) -> String {
    let mut output = String::new();
    output.push_str(
        &columns
            .iter()
            .map(|column| csv_escape(column))
            .collect::<Vec<_>>()
            .join(","),
    );
    output.push('\n');
    for row in rows {
        output.push_str(
            &row.iter()
                .map(|value| csv_escape(&json_value_to_cell(value)))
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push('\n');
    }
    output
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn format_imessage_rows_as_table(columns: &[String], rows: &[Vec<Value>]) -> String {
    if columns.is_empty() {
        return "(no columns)\n".to_string();
    }

    let mut widths: Vec<usize> = columns.iter().map(|column| column.len()).collect();
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(json_value_to_cell).collect())
        .collect();
    for row in &table_rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.len());
        }
    }

    let mut output = String::new();
    output.push_str(&format_table_line(columns, &widths));
    output.push_str(&format_table_separator(&widths));
    for row in &table_rows {
        output.push_str(&format_table_line(row, &widths));
    }
    output
}

fn format_table_line(cells: &[String], widths: &[usize]) -> String {
    let mut line = String::from("|");
    for (cell, width) in cells.iter().zip(widths.iter()) {
        line.push(' ');
        line.push_str(&format!("{:<width$}", cell, width = *width));
        line.push(' ');
        line.push('|');
    }
    line.push('\n');
    line
}

fn format_table_separator(widths: &[usize]) -> String {
    let mut line = String::from("|");
    for width in widths {
        line.push(' ');
        line.push_str(&"-".repeat(*width));
        line.push(' ');
        line.push('|');
    }
    line.push('\n');
    line
}

fn json_value_to_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn resolve_messages_db_path() -> PathBuf {
    if let Ok(path) = std::env::var("MAGICIAN_IMESSAGE_DB_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
        .join("Library/Messages/chat.db")
}

fn normalize_imessage_sql_for_sqlite(sql: &str) -> String {
    let sqlite_scan = regex::Regex::new(
        r#"(?is)sqlite_scan\s*\(\s*['"][^'"]*['"]\s*,\s*['"]([A-Za-z_][A-Za-z0-9_]*)['"]\s*\)"#,
    )
    .expect("valid sqlite_scan regex");
    sqlite_scan.replace_all(sql, "$1").into_owned()
}

fn validate_imessage_sql(sql: &str) -> Result<(), ExecutionError> {
    let Some(keyword) = first_sql_keyword(sql) else {
        return Err(ExecutionError::Step(
            "iMessage SQL is empty; provide a read-only SELECT, WITH, PRAGMA, or EXPLAIN query"
                .to_string(),
        ));
    };

    match keyword.as_str() {
        "SELECT" | "WITH" | "PRAGMA" | "EXPLAIN" => Ok(()),
        other => Err(ExecutionError::Step(format!(
            "iMessage SQL must be read-only; first statement keyword was '{}'",
            other
        ))),
    }
}

fn first_sql_keyword(sql: &str) -> Option<String> {
    let mut remaining = sql.trim_start();
    loop {
        if let Some(stripped) = remaining.strip_prefix("--") {
            if let Some((_, rest)) = stripped.split_once('\n') {
                remaining = rest.trim_start();
                continue;
            }
            return None;
        }
        if let Some(stripped) = remaining.strip_prefix("/*") {
            if let Some((_, rest)) = stripped.split_once("*/") {
                remaining = rest.trim_start();
                continue;
            }
            return None;
        }
        break;
    }

    let keyword: String = remaining
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .collect();
    if keyword.is_empty() {
        None
    } else {
        Some(keyword.to_ascii_uppercase())
    }
}

// ============================================================================
// Shell
// ============================================================================

pub struct ShellCapabilityProvider {
    sandbox: ShellSandboxConfig,
    pack_def: Option<CapabilityPackDefinition>,
    scope_paths: Option<CapabilityScopePaths>,
    /// Defense-in-depth: RawShellHandler with CommandGuard runs before
    /// ShellSandboxConfig. Blocks commands matching dangerous patterns
    /// or referencing protected paths.
    raw_shell: super::agentic::shell_tool::RawShellHandler,
    /// Tracks whether the initial auth check has been performed.
    /// Set to `true` after the first `check_command` probe (or when no auth is configured).
    auth_checked: AtomicBool,
}

impl std::fmt::Debug for ShellCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellCapabilityProvider")
            .field("sandbox", &self.sandbox)
            .field("pack_def", &self.pack_def)
            .field("auth_checked", &self.auth_checked.load(Ordering::Relaxed))
            .finish()
    }
}

impl ShellCapabilityProvider {
    pub fn new(sandbox: ShellSandboxConfig) -> Self {
        // Build CommandGuard from the sandbox's blocked_command_fragments.
        // This gives RawShellHandler the same blocklist, plus protected_paths support.
        let guard = super::agentic::shell_tool::CommandGuard::new(
            sandbox.blocked_command_fragments.clone(),
            vec![], // protected_paths can be configured here in the future
        );
        Self {
            sandbox,
            pack_def: None,
            scope_paths: None,
            raw_shell: super::agentic::shell_tool::RawShellHandler::new(guard),
            auth_checked: AtomicBool::new(false),
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }

    pub fn with_scope_paths(mut self, scope_paths: CapabilityScopePaths) -> Self {
        self.scope_paths = Some(scope_paths);
        self
    }

    /// Core execution without auth wrapping.
    ///
    /// Defense-in-depth: checks CommandGuard (via RawShellHandler) before
    /// ShellSandboxConfig enforcement in execute_bash_action.
    async fn execute_inner(
        &self,
        action: &ExecutableAction,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        match action {
            ExecutableAction::Bash(bash_action) => {
                // CommandGuard check: block dangerous command patterns and protected paths.
                self.raw_shell
                    .guard()
                    .check(&bash_action.command)
                    .map_err(|e| ExecutionError::Step(e.to_string()))?;

                // Isolation (coding-scoped): a generic `shell` that supplies NO
                // explicit `working_dir` otherwise inherits the process CWD — the
                // live magician repo — so a bare `… > file` / heredoc write lands
                // in the user's real source tree (the RCA write vector; write_file
                // already scope-rejects, the shell did not). Inside a coding run,
                // default the CWD to the scope's `workdirs` root (under the storage
                // sandbox base, which the repo deny-fence treats as writable),
                // mirroring write_file's scope confinement and complementing the OS
                // sandbox (defense-in-depth for when the launcher probe fails open).
                // General (non-coding) flows are byte-equivalent — they keep
                // inheriting the process CWD (in-repo make/cargo/npm depend on it),
                // and `search`/`delegation_shell` are separate providers, untouched.
                let confined_cwd_action = if bash_action.working_dir.is_none()
                    && crate::magician_v2::execution::coding_engine::coding_context_active()
                {
                    self.scope_paths.as_ref().map(|paths| {
                        let workdir = paths.workdirs_root.clone();
                        if let Err(e) = std::fs::create_dir_all(&workdir) {
                            warn!(
                                "[SHELL] failed to create scope workdir '{}' for coding-run CWD \
                                 confinement: {}",
                                workdir.display(),
                                e
                            );
                        }
                        let mut confined = bash_action.clone();
                        confined.working_dir = Some(workdir);
                        confined
                    })
                } else {
                    None
                };
                let bash_action = confined_cwd_action.as_ref().unwrap_or(bash_action);

                let duration = std::time::Duration::from_secs(timeout_secs);
                timeout(
                    duration,
                    execute_bash_action(bash_action, &self.sandbox, None, None),
                )
                .await
                .map_err(|_| {
                    ExecutionError::Step(format!("Shell action timed out after {}s", timeout_secs))
                })?
            },
            _ => Err(ExecutionError::Step(
                "ShellCapabilityProvider received non-bash action".to_string(),
            )),
        }
    }

    /// Run a shell command for auth lifecycle (check / setup / reauth).
    /// Returns `true` if the command succeeded (exit 0).
    async fn run_auth_command(cmd: &str, scope_paths: Option<&CapabilityScopePaths>) -> bool {
        let resolved = scope_paths
            .map(|paths| paths.apply_vars(cmd))
            .unwrap_or_else(|| cmd.to_string());
        // Augment PATH with the scope's tool-bin dirs so a bare
        // `gws auth status`/`gws auth login` (npm-vendored in
        // node_modules/.bin, or an absolute gws whose `#!/usr/bin/env node`
        // shebang needs node on PATH) resolves instead of exiting 127 and
        // falsely reporting auth broken. Fail-safe: no existing bin dirs →
        // leave the inherited PATH. Computed first so the shell itself is
        // resolved against the PATH the child will see and the spawn stays
        // on `posix_spawn` (see `runtime_core::process`).
        let child_path = scope_paths.and_then(|paths| {
            let parent = std::env::var("PATH").unwrap_or_default();
            paths.subprocess_bin_path(None, &parent)
        });
        let mut command = tokio::process::Command::new(runtime_core::process::resolve_program_str(
            "sh",
            child_path.as_deref(),
        ));
        command
            .arg("-c")
            .arg(&resolved)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(paths) = scope_paths {
            command.env("HOME", &paths.home_root);
            if let Some(p) = child_path.as_deref() {
                command.env("PATH", p);
            }
        }
        match command.status().await {
            Ok(status) => status.success(),
            Err(e) => {
                warn!(
                    "[SHELL_AUTH] Failed to run auth command '{}': {}",
                    resolved, e
                );
                false
            },
        }
    }

    /// Ensure authentication is valid before execution.
    ///
    /// On first invocation: runs `check_command` (if configured). When the check
    /// fails, runs `setup_command` (or `reauth_command` as fallback) to establish
    /// credentials. The result is cached so subsequent calls skip the probe.
    async fn ensure_auth(&self, auth: &super::capability::CapabilityAuthConfig) {
        if self.auth_checked.load(Ordering::Relaxed) {
            return;
        }

        if let Some(check_cmd) = &auth.check_command {
            debug!("[SHELL_AUTH] Running auth check: {}", check_cmd);
            let ok = Self::run_auth_command(check_cmd, self.scope_paths.as_ref()).await;
            if !ok {
                info!("[SHELL_AUTH] Auth check failed — running setup/reauth command");
                let setup = auth
                    .setup_command
                    .as_deref()
                    .or(auth.reauth_command.as_deref());
                if let Some(cmd) = setup {
                    let _ = Self::run_auth_command(cmd, self.scope_paths.as_ref()).await;
                }
            }
        }

        self.auth_checked.store(true, Ordering::Relaxed);
    }

    /// Check whether a result (success or error) contains an auth-failure pattern.
    /// If so, run `reauth_command` (or `setup_command`) and retry the action once.
    async fn maybe_reauth_and_retry(
        &self,
        auth: &super::capability::CapabilityAuthConfig,
        result: Result<ActionResult, ExecutionError>,
        action: &ExecutableAction,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        if auth.error_patterns.is_empty() {
            return result;
        }

        // Collect the text to scan for auth error patterns.
        let output_text: Option<String> = match &result {
            Err(e) => Some(e.to_string()),
            Ok(action_result) => action_result.as_text().map(|s| s.to_string()),
        };

        let is_auth_error = output_text
            .as_deref()
            .map(|text| {
                auth.error_patterns
                    .iter()
                    .any(|pattern| text.contains(pattern))
            })
            .unwrap_or(false);

        if !is_auth_error {
            return result;
        }

        info!(
            "[SHELL_AUTH] Auth error pattern detected in output — re-authenticating and retrying"
        );

        // Run reauth (prefer reauth_command, fall back to setup_command).
        let reauth = auth
            .reauth_command
            .as_deref()
            .or(auth.setup_command.as_deref());
        if let Some(cmd) = reauth {
            let _ = Self::run_auth_command(cmd, self.scope_paths.as_ref()).await;
        }

        // Reset the cached check so subsequent calls will re-probe.
        self.auth_checked.store(false, Ordering::Relaxed);

        // Retry the original action once.
        self.execute_inner(action, timeout_secs).await
    }
}

#[async_trait]
impl CapabilityProvider for ShellCapabilityProvider {
    fn tool_name(&self) -> &str {
        "shell"
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let step = resolve_step_params(&self.pack_def, step)?;
        let action = lowering::lower_bash_action(&step)?;
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &step.parameters,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let auth_config = self
            .pack_def
            .as_ref()
            .and_then(|p| p.auth.as_ref())
            .filter(|a| a.required);

        // Pre-execution auth gate.
        if let Some(auth) = auth_config {
            self.ensure_auth(auth).await;
        }

        // Execute the actual command.
        let result = self.execute_inner(action, timeout_secs).await;

        // Post-execution: check for auth failure patterns and retry if needed.
        if let Some(auth) = auth_config {
            return self
                .maybe_reauth_and_retry(auth, result, action, timeout_secs)
                .await;
        }

        result
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 30)
    }
}

// ============================================================================
// Search
// ============================================================================

#[derive(Debug)]
pub struct SearchCapabilityProvider {
    sandbox: ShellSandboxConfig,
    pack_def: Option<CapabilityPackDefinition>,
}

impl SearchCapabilityProvider {
    pub fn new(sandbox: ShellSandboxConfig) -> Self {
        Self {
            sandbox,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for SearchCapabilityProvider {
    fn tool_name(&self) -> &str {
        "search"
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let step = resolve_step_params(&self.pack_def, step)?;
        let action = lowering::lower_search_action(&step)?;
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &step.parameters,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        // Search lowers to a BashAction (rg command)
        match action {
            ExecutableAction::Bash(bash_action) => {
                let duration = std::time::Duration::from_secs(timeout_secs);
                timeout(
                    duration,
                    execute_bash_action(bash_action, &self.sandbox, None, None),
                )
                .await
                .map_err(|_| {
                    ExecutionError::Step(format!("Search action timed out after {}s", timeout_secs))
                })?
            },
            _ => Err(ExecutionError::Step(
                "SearchCapabilityProvider received non-bash action".to_string(),
            )),
        }
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 30)
    }
}

// ============================================================================
// Delegation — shared path validation
// ============================================================================

/// Validate that a path is safe for delegation: must exist, be a directory,
/// be under the user's home directory, and not a sensitive system path.
fn validate_delegation_path(path: &Path) -> Result<(), ExecutionError> {
    let canon = path.canonicalize().map_err(|e| {
        ExecutionError::Step(format!(
            "Cannot resolve delegation path '{}': {}",
            path.display(),
            e
        ))
    })?;
    if !canon.is_dir() {
        return Err(ExecutionError::Step(format!(
            "Delegation path is not a directory: {}",
            canon.display()
        )));
    }
    let s = canon.to_string_lossy();
    let blocked = [
        "/", "/etc", "/usr", "/var", "/System", "/bin", "/sbin", "/Library", "/private", "/tmp",
        // Linux-specific
        "/proc", "/sys", "/dev", "/root", "/boot", "/opt", "/srv",
    ];
    if blocked.iter().any(|b| s.as_ref() == *b) {
        return Err(ExecutionError::Step(format!(
            "Delegation path blocked (sensitive system path): {}",
            s
        )));
    }
    let home = std::env::var("HOME").map_err(|_| {
        ExecutionError::Step(
            "Cannot validate delegation path: HOME environment variable not set".to_string(),
        )
    })?;
    if !canon.starts_with(&home) {
        // Out-of-approved-area path. This is not a dead-end: nudge the model to
        // request owner approval on its next turn rather than retrying into
        // consecutive failures. (The sensitive-system blocklist above stays a
        // genuine hard deny; this branch is the recoverable "just not approved
        // yet" case.)
        return Err(ExecutionError::Step(format!(
            "Delegation path '{}' is outside the approved area; ask the owner to \
             approve this folder via need_user_input before retrying",
            s
        )));
    }
    Ok(())
}

// ============================================================================
// Delegation Shell
// ============================================================================

/// Shell provider for delegation capability packs.
/// Auto-expands sandbox to allow the working_dir specified in the step.
/// Injects `MAGICIAN_ROOT` env var for binary path resolution.
pub struct DelegationShellProvider {
    base_sandbox: ShellSandboxConfig,
    repo_root: PathBuf,
}

impl std::fmt::Debug for DelegationShellProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegationShellProvider")
            .field("base_sandbox", &self.base_sandbox)
            .field("repo_root", &self.repo_root)
            .finish()
    }
}

impl DelegationShellProvider {
    pub fn new(base_sandbox: ShellSandboxConfig, repo_root: PathBuf) -> Self {
        Self {
            base_sandbox,
            repo_root,
        }
    }
}

#[async_trait]
impl CapabilityProvider for DelegationShellProvider {
    fn tool_name(&self) -> &str {
        "delegation_shell"
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        lowering::lower_bash_action(step).map(MaybeGatedAction::Bare)
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        match action {
            ExecutableAction::Bash(bash_action) => {
                // 1. Expand sandbox with working_dir
                let mut sandbox = self.base_sandbox.clone();
                if let Some(wd) = &bash_action.working_dir {
                    validate_delegation_path(wd)?;
                    sandbox
                        .allowed_working_dirs
                        .push(wd.to_string_lossy().to_string());
                }

                // 2. Inject MAGICIAN_ROOT for binary path resolution
                let mut action_with_env = bash_action.clone();
                action_with_env.env.insert(
                    "MAGICIAN_ROOT".to_string(),
                    self.repo_root.to_string_lossy().to_string(),
                );

                let duration = std::time::Duration::from_secs(timeout_secs);
                timeout(
                    duration,
                    execute_bash_action(&action_with_env, &sandbox, None, None),
                )
                .await
                .map_err(|_| {
                    ExecutionError::Step(format!(
                        "Delegation shell action timed out after {}s",
                        timeout_secs
                    ))
                })?
            },
            _ => Err(ExecutionError::Step(
                "DelegationShellProvider only supports Bash actions".to_string(),
            )),
        }
    }

    fn default_timeout_secs(&self) -> u64 {
        3600
    }
}

// ============================================================================
// Delegation Files
// ============================================================================

/// File provider for external file access.
/// Auto-expands allowed_roots with the file's nearest existing directory ancestor.
pub struct DelegationFilesProvider {
    base_sandbox: FileSandboxConfig,
}

impl std::fmt::Debug for DelegationFilesProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegationFilesProvider")
            .field("base_sandbox", &self.base_sandbox)
            .finish()
    }
}

impl DelegationFilesProvider {
    pub fn new(base_sandbox: FileSandboxConfig) -> Self {
        Self { base_sandbox }
    }

    /// Extract all paths from a FileAction for sandbox expansion.
    fn extract_paths(action: &super::actions::FileAction) -> Vec<&Path> {
        use super::actions::FileAction;
        match action {
            FileAction::Read { path, .. } => vec![path.as_path()],
            FileAction::Write { path, .. } => vec![path.as_path()],
            FileAction::Append { path, .. } => vec![path.as_path()],
            FileAction::Delete { path, .. } => vec![path.as_path()],
            FileAction::Copy {
                source,
                destination,
            } => vec![source.as_path(), destination.as_path()],
            FileAction::Move {
                source,
                destination,
            } => vec![source.as_path(), destination.as_path()],
            FileAction::Exists { path } => vec![path.as_path()],
            FileAction::List { path, .. } => vec![path.as_path()],
            FileAction::CreateDir { path } => vec![path.as_path()],
        }
    }
}

#[async_trait]
impl CapabilityProvider for DelegationFilesProvider {
    fn tool_name(&self) -> &str {
        "delegation_files"
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        lowering::lower_file_action(step).map(MaybeGatedAction::Bare)
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        match action {
            ExecutableAction::File(file_action) => {
                let mut sandbox = self.base_sandbox.clone();

                // Expand allowed_roots with each file's nearest existing directory ancestor.
                for path in Self::extract_paths(file_action) {
                    let mut ancestor = path.to_path_buf();
                    // Walk up until we find an existing DIRECTORY
                    while !ancestor.is_dir() {
                        if !ancestor.pop() {
                            break;
                        }
                    }
                    if ancestor.is_dir() {
                        validate_delegation_path(&ancestor)?;
                        sandbox
                            .allowed_roots
                            .push(ancestor.to_string_lossy().to_string());
                    }
                }

                let duration = std::time::Duration::from_secs(timeout_secs);
                timeout(duration, execute_file_action(file_action, &sandbox))
                    .await
                    .map_err(|_| {
                        ExecutionError::Step(format!(
                            "Delegation file action timed out after {}s",
                            timeout_secs
                        ))
                    })?
            },
            _ => Err(ExecutionError::Step(
                "DelegationFilesProvider only supports File actions".to_string(),
            )),
        }
    }

    fn default_timeout_secs(&self) -> u64 {
        120
    }
}

// ============================================================================
// Vector (lancedb-backed semantic toolkit)
// ============================================================================

const VECTOR_TOOL_NAME: &str = "vector";

/// `vector` capability: index / search / rank operations against a
/// lancedb-backed per-namespace table, embedded via Ollama. Availability
/// is gated at catalog-build time by `runtime::ollama_lifecycle::is_available`;
/// this provider does its own defense-in-depth check at execute time and
/// returns a clear `{status: "ollama_unavailable"}` payload if Ollama died
/// in the window between catalog build and dispatch.
#[derive(Debug)]
pub struct VectorCapabilityProvider {
    pack_def: Option<CapabilityPackDefinition>,
    scope_paths: Option<CapabilityScopePaths>,
}

impl Default for VectorCapabilityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorCapabilityProvider {
    pub fn new() -> Self {
        Self {
            pack_def: None,
            scope_paths: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }

    pub fn with_scope_paths(mut self, scope_paths: CapabilityScopePaths) -> Self {
        self.scope_paths = Some(scope_paths);
        self
    }

    fn vector_root(&self) -> PathBuf {
        // Per-scope root for vector tables. Each agent-chosen namespace
        // becomes a directory underneath. Scope_paths.capabilities_root is
        // the scope's runtime tree root.
        match &self.scope_paths {
            Some(paths) => paths.capabilities_root.join("vector_tables"),
            // Fallback when scope_paths weren't injected (tests, etc.).
            None => std::env::temp_dir().join("magician_vector_tables"),
        }
    }
}

#[async_trait]
impl CapabilityProvider for VectorCapabilityProvider {
    fn tool_name(&self) -> &str {
        VECTOR_TOOL_NAME
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: VECTOR_TOOL_NAME.to_string(),
            implementation: super::capability::ImplementationType::Compiled {
                provider_name: VECTOR_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let resolved_params = match action {
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } if capability_name == VECTOR_TOOL_NAME => resolved_params,
            _ => {
                return Err(ExecutionError::Step(
                    "VectorCapabilityProvider received a non-vector action".to_string(),
                ))
            },
        };
        let duration = std::time::Duration::from_secs(timeout_secs);
        let root = self.vector_root();
        timeout(duration, execute_vector_action(resolved_params, &root))
            .await
            .map_err(|_| {
                ExecutionError::Step(format!("vector action timed out after {timeout_secs}s"))
            })?
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 60)
    }
}

#[derive(Debug, Deserialize)]
struct VectorActionParams {
    /// Which sub-action to dispatch: `index`, `search`, `rank`.
    action: String,
    /// For `index`: items to embed + persist.
    /// For `rank`: items to reorder/cluster/dedupe in-memory.
    #[serde(default)]
    items: Option<Vec<VectorItemParam>>,
    /// Query text. Required for `search`, required for `rank` when
    /// `output=ranked`, optional for `rank` with `output=clusters|deduped`.
    #[serde(default)]
    query: Option<String>,
    /// Lancedb namespace (directory under `<scope>/vector_tables/`).
    /// Defaults to `"default"` if omitted. Per-task scoping is the agent's
    /// responsibility — pass e.g. `task_<task_id>` to isolate per task.
    #[serde(default)]
    namespace: Option<String>,
    /// `index`/`search` only. `hybrid` (default), `fts`, or `vector`.
    #[serde(default)]
    mode: Option<String>,
    /// `rank` only. `ranked` (default), `clusters`, or `deduped`.
    #[serde(default)]
    output: Option<String>,
    /// Result cap. Defaults: 10 for search, all for rank.
    #[serde(default)]
    limit: Option<usize>,
    /// `rank` only. Cosine similarity threshold for cluster/dedupe.
    #[serde(default)]
    threshold: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct VectorItemParam {
    id: String,
    text: String,
    #[serde(default)]
    metadata: serde_json::Value,
}

async fn execute_vector_action(
    raw: &std::collections::HashMap<String, serde_json::Value>,
    vector_root: &Path,
) -> Result<ActionResult, ExecutionError> {
    let params: VectorActionParams = serde_json::from_value(serde_json::Value::Object(
        raw.clone()
            .into_iter()
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    ))
    .map_err(|e| ExecutionError::Step(format!("invalid vector params: {e}")))?;

    // Defense-in-depth: re-check Ollama availability at execute time. The
    // catalog filter normally hides this tool when Ollama is down, but the
    // health flag can flip in the window between catalog build and dispatch.
    if !crate::magician_v2::runtime::ollama_lifecycle::is_available() {
        return Ok(ActionResult::text(
            serde_json::json!({
                "status": "ollama_unavailable",
                "reason": "Ollama daemon is not reachable or embedding model not pulled; vector ops are disabled. Re-check `runtime::ollama_lifecycle::is_available()` after Ollama is restored.",
                "action": params.action,
            })
            .to_string(),
        ));
    }
    let Some(embedder) = crate::magician_v2::runtime::ollama_lifecycle::embedder() else {
        return Ok(ActionResult::text(
            serde_json::json!({
                "status": "ollama_unavailable",
                "reason": "Ollama lifecycle not initialized.",
                "action": params.action,
            })
            .to_string(),
        ));
    };

    let namespace = params
        .namespace
        .clone()
        .unwrap_or_else(|| "default".to_string());
    if !is_safe_namespace(&namespace) {
        return Err(ExecutionError::Step(format!(
            "invalid namespace `{namespace}`: must match [A-Za-z0-9_.:-]+"
        )));
    }
    let table_dir = vector_root.join(&namespace);

    match params.action.as_str() {
        "index" => {
            let items = params
                .items
                .as_ref()
                .ok_or_else(|| ExecutionError::Step("vector.index requires `items`".to_string()))?;
            let toolkit_items: Vec<magician_vector_index::VectorItem> = items
                .iter()
                .map(|it| magician_vector_index::VectorItem {
                    id: it.id.clone(),
                    text: it.text.clone(),
                    metadata: it.metadata.clone(),
                })
                .collect();
            let table = magician_vector_index::VectorTable::at(&table_dir, embedder.config().dims);
            let count = table
                .index(&embedder, &toolkit_items)
                .await
                .map_err(|e| ExecutionError::Step(format!("vector.index failed: {e}")))?;
            Ok(ActionResult::text(
                serde_json::json!({
                    "status": "ok",
                    "action": "index",
                    "namespace": namespace,
                    "count": count,
                    "table_uri": table_dir.to_string_lossy().to_string(),
                })
                .to_string(),
            ))
        },
        "search" => {
            let query = params.query.as_ref().ok_or_else(|| {
                ExecutionError::Step("vector.search requires `query`".to_string())
            })?;
            let mode = parse_search_mode(params.mode.as_deref())?;
            let limit = params.limit.unwrap_or(10);
            let table = magician_vector_index::VectorTable::at(&table_dir, embedder.config().dims);
            let hits = table
                .search(&embedder, query, mode, limit)
                .await
                .map_err(|e| ExecutionError::Step(format!("vector.search failed: {e}")))?;
            Ok(ActionResult::text(
                serde_json::json!({
                    "status": "ok",
                    "action": "search",
                    "namespace": namespace,
                    "query": query,
                    "mode": params.mode.clone().unwrap_or_else(|| "hybrid".to_string()),
                    "count": hits.len(),
                    "hits": hits,
                })
                .to_string(),
            ))
        },
        "rank" => {
            let items = params
                .items
                .as_ref()
                .ok_or_else(|| ExecutionError::Step("vector.rank requires `items`".to_string()))?;
            let toolkit_items: Vec<magician_vector_index::VectorItem> = items
                .iter()
                .map(|it| magician_vector_index::VectorItem {
                    id: it.id.clone(),
                    text: it.text.clone(),
                    metadata: it.metadata.clone(),
                })
                .collect();
            let output = parse_rank_output(params.output.as_deref())?;
            let threshold = params.threshold.unwrap_or(match output {
                magician_vector_index::RankOutput::Clusters => 0.85,
                magician_vector_index::RankOutput::Deduped => 0.90,
                magician_vector_index::RankOutput::Ranked => 0.0,
            });
            // Cosine similarity is bounded [-1, 1] for L2-normalized vectors.
            // Reject out-of-range thresholds explicitly instead of silently
            // producing "no clusters form" (threshold > 1) or "cluster
            // everything together" (threshold < -1).
            if !(0.0..=1.0).contains(&threshold) {
                return Err(ExecutionError::Step(format!(
                    "vector.rank: threshold {threshold} out of range — must be in [0.0, 1.0]"
                )));
            }
            let result = magician_vector_index::VectorTable::rank(
                &embedder,
                &toolkit_items,
                params.query.as_deref(),
                output,
                threshold,
                params.limit,
            )
            .await
            .map_err(|e| ExecutionError::Step(format!("vector.rank failed: {e}")))?;
            let payload = match result {
                magician_vector_index::RankResult::Ranked(hits) => serde_json::json!({
                    "status": "ok",
                    "action": "rank",
                    "output": "ranked",
                    "count": hits.len(),
                    "items": hits,
                }),
                magician_vector_index::RankResult::Clusters(clusters) => serde_json::json!({
                    "status": "ok",
                    "action": "rank",
                    "output": "clusters",
                    "count": clusters.len(),
                    "clusters": clusters,
                }),
                magician_vector_index::RankResult::Deduped(hits) => serde_json::json!({
                    "status": "ok",
                    "action": "rank",
                    "output": "deduped",
                    "count": hits.len(),
                    "items": hits,
                }),
            };
            Ok(ActionResult::text(payload.to_string()))
        },
        other => Err(ExecutionError::Step(format!(
            "vector: unknown action `{other}`; expected index|search|rank"
        ))),
    }
}

fn parse_search_mode(
    raw: Option<&str>,
) -> Result<magician_vector_index::SearchMode, ExecutionError> {
    // Treat None and empty-after-trim as "use default". Some LLMs emit
    // `mode: ""` when they want the default; erroring there is unhelpful.
    let normalized = raw.unwrap_or("").trim().to_ascii_lowercase();
    let mode = if normalized.is_empty() {
        "hybrid"
    } else {
        normalized.as_str()
    };
    match mode {
        "hybrid" => Ok(magician_vector_index::SearchMode::Hybrid),
        "fts" | "keyword" | "lexical" => Ok(magician_vector_index::SearchMode::Fts),
        "vector" | "semantic" => Ok(magician_vector_index::SearchMode::Vector),
        other => Err(ExecutionError::Step(format!(
            "vector.search: unknown mode `{other}`; expected hybrid|fts|vector"
        ))),
    }
}

fn parse_rank_output(
    raw: Option<&str>,
) -> Result<magician_vector_index::RankOutput, ExecutionError> {
    let normalized = raw.unwrap_or("").trim().to_ascii_lowercase();
    let output = if normalized.is_empty() {
        "ranked"
    } else {
        normalized.as_str()
    };
    match output {
        "ranked" | "rerank" => Ok(magician_vector_index::RankOutput::Ranked),
        "clusters" | "cluster" => Ok(magician_vector_index::RankOutput::Clusters),
        "deduped" | "dedupe" => Ok(magician_vector_index::RankOutput::Deduped),
        other => Err(ExecutionError::Step(format!(
            "vector.rank: unknown output `{other}`; expected ranked|clusters|deduped"
        ))),
    }
}

fn is_safe_namespace(ns: &str) -> bool {
    // Hard rejects: empty, anything containing `..` (path traversal),
    // anything starting with `.` (hidden / current-dir / parent-dir
    // shorthand). Then per-char allowlist.
    if ns.is_empty() || ns.contains("..") || ns.starts_with('.') {
        return false;
    }
    ns.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

// ============================================================================
// Catchup Merge (in-process port of skillshub/whatsgoingon2/scripts/whatsgoingon2.py)
// ============================================================================

const CATCHUP_MERGE_TOOL_NAME: &str = "catchup_merge";
const RRF_K: f64 = 60.0;
const CLUSTER_JACCARD_THRESHOLD: f64 = 0.5;

/// `catchup_merge` capability: deterministic cross-source merge for catch-up
/// research envelopes. Pure Rust port of the Python whatsgoingon2 merge engine
/// — eliminates the subprocess + file-I/O round-trip that the script required.
///
/// Algorithm (same as the script):
/// 1. Normalize URLs via `magician_vector_index::normalize_url` (single source
///    of truth; same function the `vector` capability uses).
/// 2. Dedup items by normalized URL — merge sources / take longest snippet /
///    take max per engagement key.
/// 3. RRF-fuse per-source 1-indexed ranks with optional source-weight
///    multipliers.
/// 4. Score each survivor: `0.5*rrf_norm + 0.3*engagement_norm + 0.2*freshness_norm`.
/// 5. Sort descending by final_score.
/// 6. Optional clustering (Jaccard ≥ 0.5 on title tokens).
/// 7. Truncate to `limit`. Emit JSON envelope.
#[derive(Debug)]
pub struct CatchupMergeProvider {
    pack_def: Option<CapabilityPackDefinition>,
}

impl Default for CatchupMergeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CatchupMergeProvider {
    pub fn new() -> Self {
        Self { pack_def: None }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for CatchupMergeProvider {
    fn tool_name(&self) -> &str {
        CATCHUP_MERGE_TOOL_NAME
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: CATCHUP_MERGE_TOOL_NAME.to_string(),
            implementation: super::capability::ImplementationType::Compiled {
                provider_name: CATCHUP_MERGE_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let resolved_params = match action {
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } if capability_name == CATCHUP_MERGE_TOOL_NAME => resolved_params,
            _ => {
                return Err(ExecutionError::Step(
                    "CatchupMergeProvider received a non-catchup_merge action".to_string(),
                ))
            },
        };
        let duration = std::time::Duration::from_secs(timeout_secs);
        timeout(duration, execute_catchup_merge(resolved_params))
            .await
            .map_err(|_| {
                ExecutionError::Step(format!("catchup_merge timed out after {timeout_secs}s"))
            })?
    }

    fn default_timeout_secs(&self) -> u64 {
        timeout_from_pack(&self.pack_def, 30)
    }
}

#[derive(Debug, Deserialize)]
struct CatchupMergeParams {
    query: String,
    envelopes: Vec<CatchupSourceEnvelope>,
    #[serde(default)]
    cluster: Option<serde_json::Value>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    source_weights: Option<HashMap<String, f64>>,
}

#[derive(Debug, Deserialize, Clone)]
struct CatchupSourceEnvelope {
    source: String,
    #[serde(default)]
    items: Vec<serde_json::Value>,
}

#[derive(Debug, Clone)]
struct PoolEntry {
    item: serde_json::Map<String, serde_json::Value>,
    rrf_total: f64,
    sources: Vec<String>,
    source_native_ranks: HashMap<String, usize>,
    url_key: String,
}

async fn execute_catchup_merge(
    raw: &HashMap<String, serde_json::Value>,
) -> Result<ActionResult, ExecutionError> {
    let params: CatchupMergeParams = serde_json::from_value(serde_json::Value::Object(
        raw.clone()
            .into_iter()
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    ))
    .map_err(|e| ExecutionError::Step(format!("invalid catchup_merge params: {e}")))?;

    let started = std::time::Instant::now();
    let do_cluster = match params.cluster.as_ref() {
        None => true,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "y" | "on"
        ),
        Some(_) => true,
    };
    // Honor explicit `limit=0` as "no items, just metadata"
    // (caller wants dedup_summary / source_summary without the items).
    // usize can't be negative so no clamp needed.
    let limit = params.limit.unwrap_or(60);

    let weights = params.source_weights.unwrap_or_default();
    let mut total_input: usize = 0;
    let mut dropped_no_url: usize = 0;
    // BTreeMap (not HashMap) so tied-score items get a deterministic
    // tiebreaker (lexicographic url_key) instead of HashMap's randomized
    // iteration order. Without this, "deterministic merge" produces
    // different ordering across runs whenever final_score ties exist.
    let mut pool: std::collections::BTreeMap<String, PoolEntry> = std::collections::BTreeMap::new();

    for env in &params.envelopes {
        total_input += env.items.len();
        let weight = weights.get(&env.source).copied().unwrap_or(1.0);
        for (rank0, raw_item) in env.items.iter().enumerate() {
            let normalized = normalize_envelope_item(raw_item, &env.source);
            let url_key = magician_vector_index::normalize_url(
                normalized
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            );
            if url_key.is_empty() {
                dropped_no_url += 1;
                continue;
            }
            let rank = rank0 + 1;
            let contrib = weight * rrf_score(rank);
            if let Some(existing) = pool.get_mut(&url_key) {
                existing.rrf_total += contrib;
                if !existing.sources.contains(&env.source) {
                    existing.sources.push(env.source.clone());
                }
                existing
                    .source_native_ranks
                    .insert(env.source.clone(), rank);
                merge_item_into_existing(existing, normalized);
            } else {
                let mut native_ranks = HashMap::new();
                native_ranks.insert(env.source.clone(), rank);
                pool.insert(
                    url_key.clone(),
                    PoolEntry {
                        item: normalized,
                        rrf_total: contrib,
                        sources: vec![env.source.clone()],
                        source_native_ranks: native_ranks,
                        url_key,
                    },
                );
            }
        }
    }

    // True dedup count = items that arrived with URLs but collapsed into an
    // existing pool entry. Excludes items dropped for missing URLs (those
    // are counted separately in `dropped_no_url` so the agent can audit
    // them without conflating signals).
    let with_url = total_input.saturating_sub(dropped_no_url);
    let deduped_count = with_url.saturating_sub(pool.len());
    let ranked = compute_final_scores(pool);
    let truncated: Vec<serde_json::Value> = ranked.into_iter().take(limit).collect();
    let clusters = if do_cluster {
        Some(cluster_items(&truncated))
    } else {
        None
    };
    let final_count = truncated.len();

    let source_summary: Vec<serde_json::Value> = params
        .envelopes
        .iter()
        .map(|env| {
            let contributed = truncated
                .iter()
                .filter(|item| {
                    item.get("_sources")
                        .and_then(serde_json::Value::as_array)
                        .map(|arr| arr.iter().any(|s| s.as_str() == Some(env.source.as_str())))
                        .unwrap_or(false)
                })
                .count();
            serde_json::json!({
                "source": env.source,
                "input_count": env.items.len(),
                "contributed_to_final": contributed,
            })
        })
        .collect();

    let duration_ms = started.elapsed().as_millis() as u64;
    let mut envelope = serde_json::Map::new();
    envelope.insert("query".to_string(), serde_json::Value::String(params.query));
    envelope.insert(
        "total_items".to_string(),
        serde_json::Value::Number(final_count.into()),
    );
    envelope.insert("items".to_string(), serde_json::Value::Array(truncated));
    envelope.insert(
        "source_summary".to_string(),
        serde_json::Value::Array(source_summary),
    );
    envelope.insert(
        "dedup_summary".to_string(),
        serde_json::json!({
            "total_input_items": total_input,
            "dropped_no_url": dropped_no_url,
            "deduped_count": deduped_count,
            "final_count": final_count,
        }),
    );
    envelope.insert(
        "duration_ms".to_string(),
        serde_json::Value::Number(duration_ms.into()),
    );
    if let Some(clusters) = clusters {
        envelope.insert("clusters".to_string(), serde_json::Value::Array(clusters));
    }

    Ok(ActionResult::text(
        serde_json::Value::Object(envelope).to_string(),
    ))
}

fn normalize_envelope_item(
    raw: &serde_json::Value,
    source: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let obj = raw.as_object().cloned().unwrap_or_default();
    let pick_string = |k: &str| -> String {
        obj.get(k)
            .and_then(serde_json::Value::as_str)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let title = pick_string("title");
    let url = pick_string("url");
    let snippet = {
        let s = pick_string("snippet");
        if s.is_empty() {
            pick_string("body")
        } else {
            s
        }
    };
    let published_at = obj
        .get("published_at")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let source_value = obj
        .get("source")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::String(source.to_string()));
    let source_native_id = obj
        .get("source_native_id")
        .or_else(|| obj.get("id"))
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    let engagement = obj
        .get("engagement")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    let author = obj
        .get("author")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let container = obj
        .get("container")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let mut out = serde_json::Map::new();
    out.insert("title".to_string(), serde_json::Value::String(title));
    out.insert("url".to_string(), serde_json::Value::String(url));
    out.insert("snippet".to_string(), serde_json::Value::String(snippet));
    out.insert("published_at".to_string(), published_at);
    out.insert("source".to_string(), source_value);
    out.insert(
        "source_native_id".to_string(),
        serde_json::Value::String(source_native_id),
    );
    out.insert("engagement".to_string(), engagement);
    out.insert("author".to_string(), author);
    out.insert("container".to_string(), container);
    out
}

fn rrf_score(rank_1_indexed: usize) -> f64 {
    1.0 / (RRF_K + rank_1_indexed as f64)
}

fn merge_item_into_existing(
    existing: &mut PoolEntry,
    incoming: serde_json::Map<String, serde_json::Value>,
) {
    // Prefer the longer snippet between the duplicate items.
    let existing_snippet_len = existing
        .item
        .get("snippet")
        .and_then(serde_json::Value::as_str)
        .map(str::len)
        .unwrap_or(0);
    let incoming_snippet = incoming
        .get("snippet")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    if incoming_snippet.len() > existing_snippet_len {
        existing.item.insert(
            "snippet".to_string(),
            serde_json::Value::String(incoming_snippet),
        );
    }
    // Merge engagement keys: take the max numeric value per key when both
    // sides have a number. Otherwise prefer the existing value over a null
    // (so a second envelope's `null` doesn't silently wipe a real number
    // from the first envelope). Non-null non-numeric incoming values still
    // overwrite (rare; documents the data shape).
    if let Some(serde_json::Value::Object(incoming_eng)) = incoming.get("engagement") {
        let existing_eng = existing
            .item
            .entry("engagement".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let serde_json::Value::Object(ref mut existing_eng_obj) = existing_eng {
            for (k, v) in incoming_eng {
                match (existing_eng_obj.get(k), v) {
                    (Some(serde_json::Value::Number(prev)), serde_json::Value::Number(new))
                        if prev.as_f64().is_some() && new.as_f64().is_some() =>
                    {
                        let p = prev.as_f64().unwrap_or(0.0);
                        let n = new.as_f64().unwrap_or(0.0);
                        existing_eng_obj.insert(k.clone(), serde_json::json!(p.max(n)));
                    },
                    // Incoming is null AND existing has a value → keep existing.
                    (Some(_), serde_json::Value::Null) => {},
                    (_, new) => {
                        existing_eng_obj.insert(k.clone(), new.clone());
                    },
                };
            }
        }
    }
}

fn engagement_normalized(item: &serde_json::Map<String, serde_json::Value>) -> f64 {
    let total: f64 = item
        .get("engagement")
        .and_then(serde_json::Value::as_object)
        .map(|obj| {
            obj.values()
                .filter_map(serde_json::Value::as_f64)
                .filter(|v| v.is_finite())
                .sum()
        })
        .unwrap_or(0.0);
    if total <= 0.0 {
        return 0.0;
    }
    let denom = (1.0_f64 + 10_000.0).ln();
    if denom <= 0.0 {
        return 0.0;
    }
    ((1.0_f64 + total).ln() / denom).min(1.0)
}

fn freshness_normalized(
    item: &serde_json::Map<String, serde_json::Value>,
    now: chrono::DateTime<chrono::Utc>,
) -> f64 {
    let Some(raw) = item.get("published_at").and_then(serde_json::Value::as_str) else {
        return 0.5;
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(&raw.replace("Z", "+00:00"));
    let dt = match parsed {
        Ok(d) => d.with_timezone(&chrono::Utc),
        Err(_) => return 0.5,
    };
    let age_secs = (now - dt).num_seconds().max(0) as f64;
    let age_days = age_secs / 86_400.0;
    let score = 1.0 - age_days / 30.0;
    score.clamp(0.0, 1.0)
}

fn compute_final_scores(
    pool: std::collections::BTreeMap<String, PoolEntry>,
) -> Vec<serde_json::Value> {
    if pool.is_empty() {
        return Vec::new();
    }
    let max_rrf = pool
        .values()
        .map(|e| e.rrf_total)
        .fold(f64::MIN, f64::max)
        .max(1e-12);
    let now = chrono::Utc::now();
    let mut scored: Vec<(f64, serde_json::Value)> = pool
        .into_values()
        .map(|mut entry| {
            let rrf_norm = (entry.rrf_total / max_rrf).clamp(0.0, 1.0);
            let eng_norm = engagement_normalized(&entry.item);
            let fresh_norm = freshness_normalized(&entry.item, now);
            let final_score = 0.5 * rrf_norm + 0.3 * eng_norm + 0.2 * fresh_norm;
            entry.item.insert(
                "_score_breakdown".to_string(),
                serde_json::json!({
                    "rrf_norm": round4(rrf_norm),
                    "engagement_norm": round4(eng_norm),
                    "freshness_norm": round4(fresh_norm),
                    "final_score": round4(final_score),
                }),
            );
            entry.item.insert(
                "_sources".to_string(),
                serde_json::Value::Array(
                    entry
                        .sources
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
            entry.item.insert(
                "_source_ranks".to_string(),
                serde_json::Value::Object(
                    entry
                        .source_native_ranks
                        .into_iter()
                        .map(|(k, v)| (k, serde_json::Value::Number(v.into())))
                        .collect(),
                ),
            );
            entry.item.insert(
                "_url_key".to_string(),
                serde_json::Value::String(entry.url_key),
            );
            (final_score, serde_json::Value::Object(entry.item))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .map(|(_, mut item)| {
            // Drop `_url_key` internal field; keep `_sources`, `_source_ranks`,
            // `_score_breakdown` so callers can inspect contributions.
            if let serde_json::Value::Object(ref mut obj) = item {
                obj.remove("_url_key");
            }
            item
        })
        .collect()
}

fn round4(x: f64) -> f64 {
    (x * 10000.0).round() / 10000.0
}

fn catchup_tokens(text: &str) -> std::collections::BTreeSet<String> {
    const STOPWORDS: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "he", "in", "is",
        "it", "its", "of", "on", "or", "that", "the", "to", "was", "were", "will", "with", "this",
        "we", "you", "they", "have", "but",
    ];
    let mut tokens = std::collections::BTreeSet::new();
    for raw in text.to_ascii_lowercase().split_whitespace() {
        let cleaned: String = raw.chars().filter(|c| c.is_alphanumeric()).collect();
        if cleaned.len() > 2 && !STOPWORDS.contains(&cleaned.as_str()) {
            tokens.insert(cleaned);
        }
    }
    tokens
}

fn jaccard(a: &std::collections::BTreeSet<String>, b: &std::collections::BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn cluster_items(items: &[serde_json::Value]) -> Vec<serde_json::Value> {
    struct Cluster {
        representative: serde_json::Value,
        members: Vec<serde_json::Value>,
        tokens: std::collections::BTreeSet<String>,
    }
    let mut clusters: Vec<Cluster> = Vec::new();
    for item in items {
        let title = item
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let tokens = catchup_tokens(title);
        if tokens.is_empty() {
            clusters.push(Cluster {
                representative: item.clone(),
                members: vec![item.clone()],
                tokens,
            });
            continue;
        }
        let mut placed = false;
        for cluster in clusters.iter_mut() {
            if jaccard(&tokens, &cluster.tokens) >= CLUSTER_JACCARD_THRESHOLD {
                cluster.members.push(item.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            clusters.push(Cluster {
                representative: item.clone(),
                members: vec![item.clone()],
                tokens,
            });
        }
    }
    clusters
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "representative_url": c.representative.get("url").cloned().unwrap_or(serde_json::Value::Null),
                "representative_title": c.representative.get("title").cloned().unwrap_or(serde_json::Value::Null),
                "member_count": c.members.len(),
                "member_urls": c
                    .members
                    .iter()
                    .map(|m| m.get("url").cloned().unwrap_or(serde_json::Value::Null))
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

// ============================================================================
// Pack-aware helpers
// ============================================================================

/// If a pack definition is present, resolve parameters (defaults, aliases)
/// and return an owned `PlanStep` with the resolved map. Falls back to cloning
/// the original step when no pack is configured.
fn resolve_step_params(
    pack_def: &Option<CapabilityPackDefinition>,
    step: &PlanStep,
) -> Result<PlanStep, ExecutionError> {
    match pack_def {
        Some(def) => {
            let resolved = def.resolve_params(&step.parameters)?;
            let mut s = step.clone();
            s.parameters = resolved;
            Ok(s)
        },
        None => Ok(step.clone()),
    }
}

/// Read `execution.default_timeout_secs` from a pack, falling back to `fallback` (default 30s).
/// Set alongside a back-filled `timeout_secs` so a handler can tell a
/// caller-chosen budget from the pack's stamped-in default. Handlers that do
/// not care simply ignore it, like every other `__`-prefixed dispatch arg.
pub const PACK_DEFAULT_TIMEOUT_MARKER: &str = "__timeout_secs_from_pack_default";

fn timeout_from_pack(pack_def: &Option<CapabilityPackDefinition>, fallback: u64) -> u64 {
    pack_def
        .as_ref()
        .and_then(|d| d.execution.as_ref())
        .and_then(|e| e.default_timeout_secs)
        .unwrap_or(fallback)
}

// ============================================================================
// Registry Builder
// ============================================================================

/// Info about a pack-derived tool: (tool_name, ToolDefinition, optional guide text).
pub type PackToolInfo = (String, ToolDefinition, Option<String>);

/// Convert pre-loaded `CapabilityPackDefinition`s into `PackToolInfo` entries
/// suitable for injection into the `RegistryService` / tool guides.
///
/// All pack types (compiled and pack-defined) generate tool infos so the
/// planner/tool-catalog can discover them. The HTTP pack additionally generates
/// per-method tool definition variants.
/// The per-method aliases the catalog generates for the compiled `http` pack.
/// Every name here reaches the same HTTP adapter, so the credential-sink lane
/// must know it too: `secrets::sinks::HTTP_PACK_CAPABILITIES` is the closed set
/// it checks, and `http_pack_aliases_are_all_known_credential_sinks` holds the
/// two lists together.
const HTTP_METHOD_ALIASES: [(&str, &str); 8] = [
    ("http_get", "Send an HTTP GET request"),
    ("http_post", "Send an HTTP POST request"),
    ("http_put", "Send an HTTP PUT request"),
    ("http_patch", "Send an HTTP PATCH request"),
    ("http_delete", "Send an HTTP DELETE request"),
    (
        "http_head",
        "Send an HTTP HEAD request (headers only, no body)",
    ),
    (
        "http_options",
        "Send an HTTP OPTIONS request (CORS preflight)",
    ),
    (
        "http_request",
        "Send a generic HTTP request with configurable method",
    ),
];

pub fn pack_defs_to_tool_infos(packs: &[CapabilityPackDefinition]) -> Vec<PackToolInfo> {
    let mut infos = Vec::new();

    for pack in packs {
        let base_def = pack.to_tool_definition();
        let guide = pack.guide.clone();

        if pack.name == "http" {
            infos.push(("http".to_string(), base_def.clone(), guide.clone()));

            for (method_name, desc) in HTTP_METHOD_ALIASES {
                let mut method_def = base_def.clone();
                method_def.name = method_name.to_string();
                method_def.description = desc.to_string();
                infos.push((method_name.to_string(), method_def, guide.clone()));
            }
        } else {
            infos.push((pack.name.clone(), base_def, guide));
        }
    }

    infos
}

/// Build a `CapabilityRegistry` populated with compiled (built-in) providers
/// and any pack-driven (YAML-defined) providers from the same pack definitions.
///
/// `packs` contains pre-loaded `CapabilityPackDefinition`s (already read from disk).
/// Compiled packs are matched to built-in providers by `provider_name`.
/// Pack-defined entries (Composite/JavaScript) are registered as
/// `PackCapabilityProvider` instances so the registry is the single
/// source of truth for all capabilities.
///
/// Returns `Arc<CapabilityRegistry>` and the pack-derived tool definitions.
pub fn build_compiled_registry(
    magicutor_client: Arc<MagicutorClient>,
    file_sandbox: FileSandboxConfig,
    shell_sandbox: ShellSandboxConfig,
    mut packs: Vec<CapabilityPackDefinition>,
    secret_broker: Option<Arc<dyn SecretBroker>>,
    repo_root: PathBuf,
    scope_paths: Option<CapabilityScopePaths>,
    resource_ledger: Option<
        Arc<tokio::sync::RwLock<crate::magician_v2::resource_authority::ledger::ResourceLedger>>,
    >,
    token_store: Option<
        Arc<tokio::sync::RwLock<crate::magician_v2::resource_authority::token_store::TokenStore>>,
    >,
    // Chat-native compiled providers — the heavyweight "control" tools
    // we migrated from explicit `dispatch_chat_tool_call` arms into
    // compiled packs (`search_memory`, `update_memory_tier`,
    // `save_preference`, `create_dashboard`, `unpublish_dashboard`).
    // Phase 0.8c — neutral agent-resource bundle for compiled
    // providers. Constructed at boot in `bin/magician.rs` and
    // threaded through `ScopedCapabilityResolver::agent_resources()`.
    agent_resources: Option<Arc<crate::magician_v2::execution::agent_resources::AgentResources>>,
    // Phase 0.8c — handler-registry for the migrated tools. When a
    // pack name has a registered handler AND `agent_resources` is
    // present, we wire a `GenericCompiledProvider` instead of the
    // legacy per-tool struct. Otherwise the legacy provider runs
    // (bridge-coupled). One-by-one migration: write a handler, add
    // a register line, the legacy block becomes unreachable, delete
    // it.
    compiled_handlers: Option<&CompiledHandlerRegistry>,
) -> (Arc<CapabilityRegistry>, Vec<PackToolInfo>) {
    let registry = Arc::new(CapabilityRegistry::new());

    // `time_math` is an app-qualified reserved built-in. Scope and extra-pack
    // discovery may contribute ordinary providers, but it must never replace
    // the exact embedded definition used to mint the built-in witness. Strip
    // both friendly-name shadows and alternate pack names that claim the
    // reserved provider bucket before any registry/tool-index projection.
    let embedded_time_math = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == TIME_MATH_TOOL_NAME)
        .expect("embedded time_math definition")
        .clone();
    let mut retained_embedded_time_math = false;
    packs.retain(|pack| {
        let claims_reserved_name = pack.name == TIME_MATH_TOOL_NAME;
        let claims_reserved_provider = matches!(
            &pack.implementation,
            super::capability::ImplementationType::Compiled { provider_name }
                if provider_name == TIME_MATH_TOOL_NAME
        );
        if !claims_reserved_name && !claims_reserved_provider {
            return true;
        }
        if !retained_embedded_time_math && pack == &embedded_time_math {
            retained_embedded_time_math = true;
            return true;
        }
        warn!(
            "[CAPABILITY] Ignoring non-embedded definition claiming reserved app provider '{}'",
            TIME_MATH_TOOL_NAME
        );
        false
    });
    // The descriptor-backed filesystem/table owners also depend on exact
    // centrally embedded provider schemas. Same-named scoped packs remain
    // ordinary agent capabilities and cannot inherit an Apps witness.
    let embedded_files = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == "files")
        .expect("embedded files definition")
        .clone();
    let mut retained_embedded_files = false;
    packs.retain(|pack| {
        let claims_reserved_name = pack.name == "files";
        let claims_reserved_provider = matches!(
            &pack.implementation,
            super::capability::ImplementationType::Compiled { provider_name }
                if provider_name == "files"
        );
        if !claims_reserved_name && !claims_reserved_provider {
            return true;
        }
        if !retained_embedded_files && pack == &embedded_files {
            retained_embedded_files = true;
            return true;
        }
        warn!(
            "[CAPABILITY] Ignoring non-embedded definition claiming reserved app provider 'files'"
        );
        false
    });
    let embedded_duckdb = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == "duckdb")
        .expect("embedded duckdb definition")
        .clone();
    let mut retained_embedded_duckdb = false;
    packs.retain(|pack| {
        let claims_reserved_name = pack.name == "duckdb";
        let claims_reserved_provider = matches!(
            &pack.implementation,
            super::capability::ImplementationType::Compiled { provider_name }
                if provider_name == "duckdb"
        );
        if !claims_reserved_name && !claims_reserved_provider {
            return true;
        }
        if !retained_embedded_duckdb && pack == &embedded_duckdb {
            retained_embedded_duckdb = true;
            return true;
        }
        warn!(
            "[CAPABILITY] Ignoring non-embedded definition claiming reserved app provider 'duckdb'"
        );
        false
    });
    // Bound HTTP is also an app-qualified built-in, but only through the
    // move-only GET physical owner. Reserve the exact embedded pack/provider
    // identity so a scope-local pack named `http` cannot inherit that owner.
    let embedded_http = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == "http")
        .expect("embedded http definition")
        .clone();
    let mut retained_embedded_http = false;
    packs.retain(|pack| {
        let claims_reserved_name = pack.name == "http";
        let claims_reserved_provider = matches!(
            &pack.implementation,
            super::capability::ImplementationType::Compiled { provider_name }
                if provider_name == "http"
        );
        if !claims_reserved_name && !claims_reserved_provider {
            return true;
        }
        if !retained_embedded_http && pack == &embedded_http {
            retained_embedded_http = true;
            return true;
        }
        warn!(
            "[CAPABILITY] Ignoring non-embedded definition claiming reserved app provider 'http'"
        );
        false
    });

    // Phase 1 (flat loop): build the process tool index from the full loaded
    // pack list (embedded + skillshub) and install it on `AgentResources` so
    // `tool_search` can fetch deferred-tool schemas on demand. Done here, while
    // `packs` is still owned and before the move at the type-split below.
    // `set_tool_index` is idempotent (OnceLock), so re-entrant registry builds
    // are safe. Inert until a flat execution mode consumes the catalog
    // (Phase 3) — `tool_search` returns its inactive shape when unset.
    if let Some(resources) = agent_resources.as_ref() {
        let index = crate::magician_v2::execution::flat_loop::build_tool_index(&packs);
        resources.set_tool_index(Arc::new(index));
    }

    for pack in &packs {
        registry.set_pack_definition(&pack.name, pack.clone());
    }

    // Generate tool infos from the pre-loaded defs (all pack types)
    let mut pack_tool_infos = pack_defs_to_tool_infos(&packs);

    // Separate compiled-backed packs from pack-defined ones
    let mut pack_by_provider: HashMap<String, CapabilityPackDefinition> = HashMap::new();
    let mut pack_defined_packs: Vec<CapabilityPackDefinition> = Vec::new();
    for pack in packs {
        match &pack.implementation {
            super::capability::ImplementationType::Compiled { provider_name }
                if provider_name == "replay_recipe" =>
            {
                // Task recipes intentionally share one compiled handler while
                // retaining distinct planner-facing tool names and bound
                // recipe_id defaults. A provider-keyed HashMap would collapse
                // these aliases to the last recipe, so register each alias
                // directly under its own pack name.
                match (
                    agent_resources.as_ref(),
                    compiled_handlers.and_then(|handlers| handlers.get("replay_recipe")),
                ) {
                    (Some(resources), Some(handler)) => {
                        let name = pack.name.clone();
                        registry.register(Arc::new(
                            GenericCompiledProvider::new(
                                name,
                                handler,
                                Arc::clone(resources),
                                scope_paths.clone(),
                            )
                            .with_pack_def(pack),
                        ));
                    },
                    _ => {
                        registry.remove_pack_definition(&pack.name);
                        pack_tool_infos.retain(|(name, _, _)| name != &pack.name);
                    },
                }
            },
            super::capability::ImplementationType::Compiled { provider_name } => {
                pack_by_provider.insert(provider_name.clone(), pack);
            },
            _ => {
                pack_defined_packs.push(pack);
            },
        }
    }

    // Browser is no longer a compiled provider. `browser.yaml` must declare
    // `implementation.type: primitive`, and the outer executor dispatches it
    // through `primitive_dispatch::browser` instead of the compiled provider registry.
    // Keep provider construction in bounded call frames. In debug builds the
    // concrete provider locals from this historically 1,000-line function were
    // all reserved in one native frame, large enough to overflow an ordinary
    // Rust/Tokio worker stack before the first provider was registered.
    #[rustfmt::skip]
    (|| {

    // Files
    let files_pack = pack_by_provider.remove("files");
    if retained_embedded_files {
        let files_provider = FileCapabilityProvider::new(file_sandbox.clone())
            .with_pack_def(embedded_files);
        registry.register_builtin_files_provider(
            Arc::new(files_provider),
            embedded_compiled_pack_yaml("files")
                .expect("embedded files pack")
                .as_bytes(),
        );
    } else {
        let mut files_provider = FileCapabilityProvider::new(file_sandbox.clone());
        if let Some(pack) = files_pack {
            files_provider = files_provider.with_pack_def(pack);
        }
        registry.register(Arc::new(files_provider));
    }

    // HTTP
    let http_pack = pack_by_provider.remove("http");
    if retained_embedded_http {
        let http_provider = HttpCapabilityProvider::new().with_pack_def(embedded_http);
        registry.register_builtin_http_provider(
            Arc::new(http_provider),
            embedded_compiled_pack_yaml("http")
                .expect("embedded http pack")
                .as_bytes(),
        );
    } else {
        // Defensive boot fallback: no app witness is minted when the exact
        // embedded definition was not among the loaded packs.
        let mut http_provider = HttpCapabilityProvider::new();
        if let Some(pack) = http_pack {
            http_provider = http_provider.with_pack_def(pack);
        }
        registry.register(Arc::new(http_provider));
    }

    // DuckDB
    let duckdb_pack = pack_by_provider.remove("duckdb");
    match DuckDbCapabilityProvider::new() {
        Ok(mut duckdb_provider) => {
            if retained_embedded_duckdb {
                duckdb_provider = duckdb_provider.with_pack_def(embedded_duckdb);
                registry.register_builtin_duckdb_provider(
                    Arc::new(duckdb_provider),
                    embedded_compiled_pack_yaml("duckdb")
                        .expect("embedded duckdb pack")
                        .as_bytes(),
                );
            } else {
                if let Some(pack) = duckdb_pack {
                    duckdb_provider = duckdb_provider.with_pack_def(pack);
                }
                registry.register(Arc::new(duckdb_provider));
            }
        },
        Err(e) => {
            warn!("[CAPABILITY] Failed to initialize DuckDB provider: {}", e);
        },
    }

    let _ = pack_by_provider.remove(TIME_MATH_TOOL_NAME);
    if retained_embedded_time_math {
        // Do not use the provider-name bucket's selected value here. The
        // provider and witness are both constructed from the centrally parsed
        // embedded definition, so discovery order cannot influence app code.
        let time_math_provider =
            TimeMathCapabilityProvider::new().with_pack_def(embedded_time_math);
        registry.register_builtin_time_math_provider(
            Arc::new(time_math_provider),
            embedded_compiled_pack_yaml(TIME_MATH_TOOL_NAME)
                .expect("embedded time_math pack")
                .as_bytes(),
        );
    }

    let meeting_pack = pack_by_provider.remove(MEETING_TOOL_NAME);
    if let Some(pack) = meeting_pack {
        let mut meeting_provider = MeetingCapabilityProvider::new();
        meeting_provider = meeting_provider.with_pack_def(pack);
        // When both the agent's resources and scope are available, give the
        // provider the scope so a finished meeting's takeaways are written to
        // that scope's user memory tier on teardown.
        if let (Some(resources), Some(scope)) = (agent_resources.as_ref(), scope_paths.as_ref()) {
            meeting_provider = meeting_provider.with_memory_context(
                Arc::clone(resources),
                scope.principal.clone(),
                scope.workspace.clone(),
            );
        }
        registry.register(Arc::new(meeting_provider));
    }

    let analyze_image_via_openai_pack = pack_by_provider.remove(ANALYZE_IMAGE_VIA_OPENAI_TOOL_NAME);
    if let Some(pack) = analyze_image_via_openai_pack {
        let mut provider = AnalyzeImageViaOpenAiCapabilityProvider::new();
        provider = provider.with_pack_def(pack);
        if let (Some(resources), Some(scope)) = (agent_resources.as_ref(), scope_paths.as_ref()) {
            provider = provider.with_runtime_context(resources, scope);
        }
        registry.register(Arc::new(provider));
    }
    })();

    // Chat-native bridge-backed compiled providers. Pack defs are
    // always registered (they're embedded), but `execute()` returns a
    // clear "bridge not configured" error if the orchestrator hasn't
    // called `ScopedCapabilityResolver::set_agent_backend` yet
    // (e.g. boot phase before `ChatService` exists, or in tests).
    #[rustfmt::skip]
    (|| {
    if let Some(pack) = pack_by_provider.remove(SEARCH_MEMORY_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(SEARCH_MEMORY_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                SEARCH_MEMORY_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(FORGET_MEMORY_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(FORGET_MEMORY_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                FORGET_MEMORY_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(UPDATE_MEMORY_TIER_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(UPDATE_MEMORY_TIER_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                UPDATE_MEMORY_TIER_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(DISTILL_EVIDENCE_TOOL_NAME) {
        // WEG: distil a producer's tier rows into evidence (observe/meeting writers).
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(DISTILL_EVIDENCE_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                DISTILL_EVIDENCE_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(SAVE_PREFERENCE_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(SAVE_PREFERENCE_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                SAVE_PREFERENCE_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(SWITCH_PERSONALITY_TOOL_NAME) {
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(SWITCH_PERSONALITY_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                SWITCH_PERSONALITY_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(ACTIVATE_SKILL_TOOL_NAME) {
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(ACTIVATE_SKILL_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                ACTIVATE_SKILL_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(DEACTIVATE_SKILL_TOOL_NAME) {
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(DEACTIVATE_SKILL_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                DEACTIVATE_SKILL_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(TOOL_SEARCH_TOOL_NAME) {
        // Phase 0.8c-10 — migrated from Decision::ChatControl rail.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(TOOL_SEARCH_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                TOOL_SEARCH_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(APPLY_PATCH_TOOL_NAME) {
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(APPLY_PATCH_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                APPLY_PATCH_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(EDIT_FILE_TOOL_NAME) {
        // Phase 0.8c-10 — migrated from Decision::ChatControl rail.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(EDIT_FILE_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                EDIT_FILE_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(GLOB_TOOL_NAME) {
        // Phase 0.8c-10 — migrated from Decision::ChatControl rail.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(GLOB_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                GLOB_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(GREP_TOOL_NAME) {
        // Phase 0.8c-10 — migrated from Decision::ChatControl rail.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(GREP_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                GREP_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(WEB_FETCH_TOOL_NAME) {
        // Phase 0.8c-10 batch 2 — migrated from Decision::ChatControl rail.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(WEB_FETCH_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                WEB_FETCH_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(WEB_SEARCH_TOOL_NAME) {
        // Phase 0.8c-10 batch 2 — migrated from Decision::ChatControl rail.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(WEB_SEARCH_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                WEB_SEARCH_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    // Provider-neutral content acquisition. The canonical search/read packs
    // accept scalar or vector requests. Merely embedding these deferred pack
    // definitions is insufficient: `try_dispatch_compiled_pack` also requires
    // a provider under the pack name.
    for content_tool in [
        "content_search",
        "content_read",
        "authorize_content_read",
        "working_set_search",
        "working_set_read",
    ] {
        if let Some(pack) = pack_by_provider.remove(content_tool) {
            if let (Some(resources), Some(handler)) = (
                agent_resources.as_ref(),
                compiled_handlers
                    .as_ref()
                    .and_then(|registry| registry.get(content_tool)),
            ) {
                let provider = GenericCompiledProvider::new(
                    content_tool,
                    handler,
                    Arc::clone(resources),
                    scope_paths.clone(),
                )
                .with_pack_def(pack);
                registry.register(Arc::new(provider));
            }
        }
    }
    if let Some(pack) = pack_by_provider.remove(READ_FILE_TOOL_NAME) {
        // Phase 0.8c-11 — migrated from Rail A (FileAction::Read native lane).
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(READ_FILE_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                READ_FILE_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    })();

    // A second bounded handler-registration frame keeps the stack cost
    // independent of how many compiled tools are added to the catalog.
    #[rustfmt::skip]
    (|| {
    if let Some(pack) = pack_by_provider.remove(WRITE_FILE_TOOL_NAME) {
        // Phase 0.8c-11 — migrated from Rail A (FileAction::Write native lane).
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(WRITE_FILE_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                WRITE_FILE_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(CREATE_DASHBOARD_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(CREATE_DASHBOARD_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                CREATE_DASHBOARD_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(UNPUBLISH_DASHBOARD_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(UNPUBLISH_DASHBOARD_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                UNPUBLISH_DASHBOARD_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(LIST_AGENTS_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(LIST_AGENTS_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                LIST_AGENTS_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(GET_AGENT_DETAILS_TOOL_NAME) {
        // Handler-backed introspection meta tool. Force-offered via the flat
        // catalog when the agent has delegation targets; has a handler in
        // `default_compiled_handler_registry` and a deferred=true COMPILED_PROVIDERS
        // entry, but previously NO remove block here — so no provider was ever
        // registered and dispatch failed with "is not a compiled pack". Wire it
        // like list_agents.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(GET_AGENT_DETAILS_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                GET_AGENT_DETAILS_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(FIND_AGENTS_FOR_CAPABILITY_TOOL_NAME) {
        // Handler-backed capability-routing meta tool — same force-offer path and
        // same missing-provider gap as get_agent_details. Without this block the
        // PA-root call fails ("is not a compiled pack") and only recovers via the
        // injected delegation roster.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(FIND_AGENTS_FOR_CAPABILITY_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                FIND_AGENTS_FOR_CAPABILITY_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    // Coding triad — `run_coding_task` / `apply_code_proposal` /
    // `run_project_checks` are handler-backed (registered in
    // `default_compiled_handler_registry`) with deferred=true COMPILED_PROVIDERS
    // entries, but — exactly like the earlier get_agent_details /
    // find_agents_for_capability gap — had NO remove+register block here, so no
    // scoped provider was ever registered and every dispatch failed with
    // "`run_coding_task` is not a compiled pack". The VibeDev coding cockpit was
    // committed but never live-verified (no Pi/browser in the dev env), so the
    // gap went unnoticed until the first real end-to-end coding run. Wire all
    // three like the other handler-backed tools.
    for coding_tool in [
        "run_coding_task",
        "apply_code_proposal",
        "run_project_checks",
        "screenshot_preview",
        "capture_reference",
    ] {
        if let Some(pack) = pack_by_provider.remove(coding_tool) {
            if let (Some(resources), Some(handler)) = (
                agent_resources.as_ref(),
                compiled_handlers
                    .as_ref()
                    .and_then(|reg| reg.get(coding_tool)),
            ) {
                let provider = GenericCompiledProvider::new(
                    coding_tool,
                    handler,
                    Arc::clone(resources),
                    scope_paths.clone(),
                )
                .with_pack_def(pack);
                registry.register(Arc::new(provider));
            }
        }
    }
    // Host-relayed macOS automation tools — handler-backed, deferred=true. Same
    // trap as the coding triad above: registering the handler + pack-def is NOT
    // enough; without this remove+register block no scoped provider is bound and
    // every dispatch fails with "is not a compiled pack" even though the tool is
    // advertised. (`imessage_send` ≠ the read-history `imessage` provider.)
    for host_tool in ["macos_automation", "imessage_send"] {
        if let Some(pack) = pack_by_provider.remove(host_tool) {
            if let (Some(resources), Some(handler)) = (
                agent_resources.as_ref(),
                compiled_handlers
                    .as_ref()
                    .and_then(|reg| reg.get(host_tool)),
            ) {
                let provider = GenericCompiledProvider::new(
                    host_tool,
                    handler,
                    Arc::clone(resources),
                    scope_paths.clone(),
                )
                .with_pack_def(pack);
                registry.register(Arc::new(provider));
            }
        }
    }
    })();

    // Task/read and owner-relay handlers form their own bounded frame for the
    // same reason; all mutate the shared registry/map synchronously.
    #[rustfmt::skip]
    (|| {
    if let Some(pack) = pack_by_provider.remove(LIST_MEMORY_TIERS_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(LIST_MEMORY_TIERS_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                LIST_MEMORY_TIERS_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(LIST_SCHEDULED_TASKS_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(LIST_SCHEDULED_TASKS_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                LIST_SCHEDULED_TASKS_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(LIST_ARTIFACTS_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(LIST_ARTIFACTS_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                LIST_ARTIFACTS_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(GET_ACTIVE_EXECUTIONS_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(GET_ACTIVE_EXECUTIONS_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                GET_ACTIVE_EXECUTIONS_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(GET_EXECUTION_HISTORY_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(GET_EXECUTION_HISTORY_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                GET_EXECUTION_HISTORY_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(LIST_TASKS_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(LIST_TASKS_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                LIST_TASKS_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(GET_TASK_DETAILS_TOOL_NAME) {
        // Compiled-only tool: no legacy bridge provider. The chat-runtime
        // `get_task_details_for_chat` surface owns the rich previews; this
        // compiled version is the autonomous minimal read and exists only
        // through the handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(GET_TASK_DETAILS_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                GET_TASK_DETAILS_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(STOP_TASK_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(STOP_TASK_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                STOP_TASK_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(REFINE_TASK_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(REFINE_TASK_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                REFINE_TASK_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(UPDATE_TASK_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(UPDATE_TASK_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                UPDATE_TASK_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(DELETE_TASK_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(DELETE_TASK_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                DELETE_TASK_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(CREATE_TASK_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(CREATE_TASK_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                CREATE_TASK_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }
    if let Some(pack) = pack_by_provider.remove(RUN_TASK_TOOL_NAME) {
        // Phase 0.8c-5 — migrated to handler registry.
        if let (Some(resources), Some(handler)) = (
            agent_resources.as_ref(),
            compiled_handlers
                .as_ref()
                .and_then(|reg| reg.get(RUN_TASK_TOOL_NAME)),
        ) {
            let provider = GenericCompiledProvider::new(
                RUN_TASK_TOOL_NAME,
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }

    // Owner-relay tools (notify_owner + envoy ask_owner / propose_meeting /
    // request_owner_action) — chat-facing UserRequest emitters, handler-backed
    // so they dispatch in the reactive path. Identical wiring to the per-tool
    // blocks above, so loop over the names.
    for name in [
        "notify_owner",
        "ask_owner",
        "propose_meeting",
        "request_owner_action",
    ] {
        if let Some(pack) = pack_by_provider.remove(name) {
            if let (Some(resources), Some(handler)) = (
                agent_resources.as_ref(),
                compiled_handlers.as_ref().and_then(|reg| reg.get(name)),
            ) {
                let provider = GenericCompiledProvider::new(
                    name,
                    handler,
                    Arc::clone(resources),
                    scope_paths.clone(),
                )
                .with_pack_def(pack);
                registry.register(Arc::new(provider));
            }
        }
    }
    })();

    #[rustfmt::skip]
    (|| {
    let imessage_pack = pack_by_provider.remove(IMESSAGE_TOOL_NAME);
    if let Some(pack) = imessage_pack {
        let mut imessage_provider = ImessageCapabilityProvider::new();
        imessage_provider = imessage_provider.with_pack_def(pack);
        registry.register(Arc::new(imessage_provider));
    }

    // Shell
    let shell_pack = pack_by_provider.remove("shell");
    let mut shell_provider = ShellCapabilityProvider::new(shell_sandbox.clone());
    if let Some(pack) = shell_pack {
        shell_provider = shell_provider.with_pack_def(pack);
    }
    if let Some(paths) = scope_paths.clone() {
        shell_provider = shell_provider.with_scope_paths(paths);
    }
    registry.register(Arc::new(shell_provider));

    // Delegation Shell (sandbox auto-expansion for external project directories)
    let _ = pack_by_provider.remove("delegation_shell");
    let delegation_shell = DelegationShellProvider::new(shell_sandbox.clone(), repo_root);
    registry.register(Arc::new(delegation_shell));

    // Delegation Files (file sandbox auto-expansion for external file access)
    let _ = pack_by_provider.remove("delegation_files");
    let delegation_files = DelegationFilesProvider::new(file_sandbox.clone());
    registry.register(Arc::new(delegation_files));

    // Search
    let search_pack = pack_by_provider.remove("search");
    let mut search_provider = SearchCapabilityProvider::new(shell_sandbox);
    if let Some(pack) = search_pack {
        search_provider = search_provider.with_pack_def(pack);
    }
    registry.register(Arc::new(search_provider));

    // Treasurer
    if let Some(secret_broker) = secret_broker {
        let treasurer_pack = pack_by_provider.remove("treasurer");
        let mut treasurer_provider = TreasurerCapabilityProvider::new(secret_broker);
        if let Some(pack) = treasurer_pack {
            treasurer_provider = treasurer_provider.with_pack_def(pack);
        }
        registry.register(Arc::new(treasurer_provider));
    }

    // Vector toolkit (lancedb-backed semantic operations). Per-call
    // availability is also re-checked inside `execute_vector_action` via
    // `runtime::ollama_lifecycle::is_available`; the catalog filter normally
    // hides this tool when Ollama is unreachable.
    let vector_pack = pack_by_provider.remove(VECTOR_TOOL_NAME);
    let mut vector_provider = VectorCapabilityProvider::new();
    if let Some(pack) = vector_pack {
        vector_provider = vector_provider.with_pack_def(pack);
    }
    if let Some(paths) = scope_paths.clone() {
        vector_provider = vector_provider.with_scope_paths(paths);
    }
    registry.register(Arc::new(vector_provider));

    // Catchup merge — deterministic cross-source merge for catch-up research
    // envelopes. Pure Rust port of `skillshub/whatsgoingon2/scripts/whatsgoingon2.py`.
    // Eliminates the subprocess + file-I/O round-trip the script required.
    let catchup_pack = pack_by_provider.remove(CATCHUP_MERGE_TOOL_NAME);
    let mut catchup_provider = CatchupMergeProvider::new();
    if let Some(pack) = catchup_pack {
        catchup_provider = catchup_provider.with_pack_def(pack);
    }
    registry.register(Arc::new(catchup_provider));
    })();

    // Resource Authority providers — shared ledger and token store
    {
        use super::resource_authority_provider::ResourceAuthorityProvider;
        use crate::magician_v2::resource_authority::ledger::ResourceLedger;
        use crate::magician_v2::resource_authority::token_store::TokenStore;

        let ra_ledger = resource_ledger
            .unwrap_or_else(|| Arc::new(tokio::sync::RwLock::new(ResourceLedger::new())));
        let ra_store =
            token_store.unwrap_or_else(|| Arc::new(tokio::sync::RwLock::new(TokenStore::new())));

        let _ = pack_by_provider.remove("resource_ledger_read");
        let _ = pack_by_provider.remove("resource_token_issue");
        let _ = pack_by_provider.remove("resource_token_revoke");

        registry.register(Arc::new(ResourceAuthorityProvider::new(
            "resource_ledger_read",
            ra_ledger.clone(),
            ra_store.clone(),
        )));
        registry.register(Arc::new(ResourceAuthorityProvider::new(
            "resource_token_issue",
            ra_ledger.clone(),
            ra_store.clone(),
        )));
        registry.register(Arc::new(ResourceAuthorityProvider::new(
            "resource_token_revoke",
            ra_ledger,
            ra_store,
        )));
    }

    // Owner-facing app-data tools share the same generic provider shape. They
    // are registered as one family so query/search/compose cannot drift into
    // the handler-present-but-unexecutable state that Phase 5C previously had.
    for tool_name in APP_DATA_TOOL_NAMES {
        let Some(pack) = pack_by_provider.remove(tool_name) else {
            continue;
        };
        match (
            agent_resources.as_ref(),
            compiled_handlers.and_then(|handlers| handlers.get(tool_name)),
        ) {
            (Some(resources), Some(handler)) => registry.register(Arc::new(
                GenericCompiledProvider::new(
                    tool_name,
                    handler,
                    Arc::clone(resources),
                    scope_paths.clone(),
                )
                .with_pack_def(pack),
            )),
            _ => {
                if agent_resources.is_none() && compiled_handlers.is_none() {
                    // The control-plane registry is built without any
                    // agent-resource bundle or handler registry on purpose;
                    // per-scope resolvers supply both. Expected, not drift.
                    debug!(
                        "[CAPABILITY] Governed app-data pack '{}' skipped in the control-plane registry (no agent resources or handlers here); per-scope resolvers wire it",
                        pack.name
                    );
                } else {
                    warn!(
                        "[CAPABILITY] Governed app-data pack '{}' has no bound handler/resources; removing it from both planner and dispatch catalogs",
                        pack.name
                    );
                }
                registry.remove_pack_definition(&pack.name);
                pack_tool_infos.retain(|(name, _, _)| name != &pack.name);
            },
        }
    }

    // Consume (and discard) pack definitions for memory tools that are not
    // registered as agentic providers — reads happen via prompt injection,
    // writes via post-step consolidation hooks.
    let _ = pack_by_provider.remove("episode_recall");
    let _ = pack_by_provider.remove("tier_read");
    let _ = pack_by_provider.remove("tier_write");

    // Every deferred pack still here whose handler is registered binds through
    // the same generic provider the blocks above construct by hand. A handler
    // registered at boot IS the provider; a pack that had one and no block of
    // its own was advertised in every catalog and served by nothing — since
    // 2026-09-16 withheld from every scope snapshot as "no provider", before
    // that dispatched into `No provider registered`. Pilot's four Android
    // verbs, the notes and monitor tools, `web_answer` and `media_edit` were
    // all in that state.
    if let (Some(resources), Some(handlers)) = (agent_resources.as_ref(), compiled_handlers) {
        let mut bindable: Vec<String> = pack_by_provider
            .keys()
            .filter(|provider| {
                is_deferred_compiled_provider(provider) && handlers.get(provider).is_some()
            })
            .cloned()
            .collect();
        bindable.sort();
        for provider_name in bindable {
            let Some(pack) = pack_by_provider.remove(&provider_name) else {
                continue;
            };
            let Some(handler) = handlers.get(&provider_name) else {
                continue;
            };
            let provider = GenericCompiledProvider::new(
                pack.name.clone(),
                handler,
                Arc::clone(resources),
                scope_paths.clone(),
            )
            .with_pack_def(pack);
            registry.register(Arc::new(provider));
        }
    }

    prune_unknown_compiled_packs(&pack_by_provider, &mut pack_tool_infos);
    for (provider, pack) in &pack_by_provider {
        if is_deferred_compiled_provider(provider) {
            continue;
        }
        registry.remove_pack_definition(&pack.name);
    }

    // Pack-defined capabilities (Composite / JavaScript)
    for pack in pack_defined_packs {
        let name = pack.name.clone();
        let description = pack.description.clone();
        // Provider-backed primitive packs are part of a choreographed
        // double-registration (pack-defined here, provider-overridden below
        // for inner-loop dispatch) — their overwrites are expected and must
        // not fire the silent-replacement tripwire.
        let provider_backed = matches!(
            &pack.implementation,
            super::capability::ImplementationType::Primitive {
                provider_name: Some(_),
                ..
            }
        );
        // Store full pack definition for step-scoped injection (progressive disclosure).
        registry.set_pack_definition(&name, pack.clone());
        let mut provider = super::pack_provider::PackCapabilityProvider::new(
            pack,
            registry.clone(),
            Some(magicutor_client.clone()),
        );
        if let Some(paths) = scope_paths.clone() {
            provider = provider.with_scope_paths(paths);
        }
        if provider_backed {
            registry.register_override(Arc::new(provider));
        } else {
            registry.register(Arc::new(provider));
        }
        // Propagate description for prompt injection.
        if let Some(desc) = description {
            registry.set_description(&name, desc);
        }
        debug!(
            "[CAPABILITY] Registered pack capability '{}' from pack defs",
            name
        );
    }

    // Provider-backed inner-loop packs still need their safe Rust providers
    // available for primitive dispatch inside the nested loop. They are
    // pack-defined for outer routing, then provider-overridden here for
    // inner-loop execution.
    if let Some(pack) = provider_backed_primitive_pack(&registry, "duckdb", "duckdb") {
        match DuckDbCapabilityProvider::new() {
            Ok(provider) => {
                registry.register_override(Arc::new(provider.with_pack_def(pack)));
            },
            Err(e) => {
                warn!("[CAPABILITY] Failed to initialize DuckDB provider: {}", e);
            },
        }
    }
    if let Some(pack) =
        provider_backed_primitive_pack(&registry, IMESSAGE_TOOL_NAME, IMESSAGE_TOOL_NAME)
    {
        registry.register_override(Arc::new(
            ImessageCapabilityProvider::new().with_pack_def(pack),
        ));
    }

    (registry, pack_tool_infos)
}

fn provider_backed_primitive_pack(
    registry: &Arc<CapabilityRegistry>,
    pack_name: &str,
    provider_name: &str,
) -> Option<CapabilityPackDefinition> {
    let pack = registry.get_pack_definition(pack_name)?;
    match &pack.implementation {
        super::capability::ImplementationType::Primitive {
            provider_name: Some(inner_provider),
            ..
        } if inner_provider == provider_name => Some(pack),
        _ => None,
    }
}

/// Remove tool infos for compiled packs whose `provider_name` was not claimed
/// by any of the known providers. This prevents the planner from picking
/// tools that have no executor.
///
/// `leftover` is the subset of `pack_by_provider` remaining after the known
/// providers have `.remove()`d their entries.
fn prune_unknown_compiled_packs(
    leftover: &HashMap<String, CapabilityPackDefinition>,
    pack_tool_infos: &mut Vec<PackToolInfo>,
) {
    if leftover.is_empty() {
        return;
    }

    let mut prune_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (provider, pack) in leftover {
        if is_deferred_compiled_provider(provider) {
            debug!(
                "[CAPABILITY] Keeping deferred compiled pack '{}' for provider '{}'",
                pack.name, provider
            );
            continue;
        }
        warn!(
            "[CAPABILITY] Compiled pack '{}' references unknown provider '{}'; \
             removing from catalog to prevent planner from picking an unexecutable tool",
            pack.name, provider
        );
        prune_names.insert(pack.name.clone());
        // The "http" pack auto-generates http_get, http_post, etc. aliases
        // in pack_defs_to_tool_infos — prune those too.
        if pack.name == "http" {
            for suffix in [
                "get", "post", "put", "patch", "delete", "head", "options", "request",
            ] {
                prune_names.insert(format!("http_{}", suffix));
            }
        }
    }
    pack_tool_infos.retain(|(name, _, _)| !prune_names.contains(name));
}

/// Remove compiled packs whose `provider_name` does not match any known built-in provider.
///
/// Call this on the raw pack defs **before** generating tool infos for the planner
/// so the planner and executor catalogs stay in sync.
pub fn prune_unexecutable_pack_defs(packs: &mut Vec<CapabilityPackDefinition>) {
    packs.retain(|pack| {
        match &pack.implementation {
            super::capability::ImplementationType::Compiled { provider_name } => {
                if is_known_compiled_provider(provider_name) {
                    true
                } else {
                    warn!(
                        "[CAPABILITY] Compiled pack '{}' references unknown provider '{}'; \
                         pruning from pack defs before tool catalog generation",
                        pack.name, provider_name
                    );
                    false
                }
            },
            // Pack-defined entries (Composite / JavaScript) are always kept
            _ => true,
        }
    });
}

/// Remove compiled pack defs that are structurally valid but unavailable in the
/// current runtime capabilities.
///
/// This is separate from `prune_unexecutable_pack_defs()` because the pack can
/// still refer to a known provider while being intentionally disabled for the
/// current host, such as `treasurer` when durable vault storage is unavailable.
pub fn prune_runtime_disabled_pack_defs(
    packs: &mut Vec<CapabilityPackDefinition>,
    secret_capabilities: &SecretRuntimeCapabilities,
) {
    if secret_capabilities.treasurer_enabled() {
        return;
    }

    packs.retain(|pack| {
        if pack.name == "treasurer" {
            warn!(
                "[CAPABILITY] Compiled pack '{}' is disabled for this runtime; pruning from pack defs before tool catalog generation",
                pack.name
            );
            return false;
        }
        true
    });
}

/// Compiled-provider pack definitions embedded into the binary via
/// `include_str!`. These wrap in-process Rust providers
/// (`create_agent`, `treasurer`, `list_agents`, …) that cannot be
/// externalized — their schemas are part of the magician binary, not
/// runtime data.
///
/// Add a new entry here when adding a new compiled provider. The
/// matching YAML lives at
/// `magician/src/magician_v2/execution/embedded_pack_defs/<name>.yaml`.
///
/// Parse failures are panics: a malformed embedded YAML is a build error,
/// not a runtime condition.
fn embedded_compiled_pack_source_table() -> &'static [(&'static str, &'static str)] {
    const EMBEDDED: &[(&str, &str)] = &[
        // Harness state managers (compiled).
        (
            "analyze_image_via_openai",
            include_str!("embedded_pack_defs/analyze_image_via_openai.yaml"),
        ),
        (
            "create_agent",
            include_str!("embedded_pack_defs/create_agent.yaml"),
        ),
        (
            "create_dashboard",
            include_str!("embedded_pack_defs/create_dashboard.yaml"),
        ),
        (
            "create_proposal",
            include_str!("embedded_pack_defs/create_proposal.yaml"),
        ),
        (
            "create_task",
            include_str!("embedded_pack_defs/create_task.yaml"),
        ),
        // Recurring Monitors Phase 4 chat tools (plan §8).
        (
            "preview_monitor",
            include_str!("embedded_pack_defs/preview_monitor.yaml"),
        ),
        (
            "create_monitor",
            include_str!("embedded_pack_defs/create_monitor.yaml"),
        ),
        (
            "create_note",
            include_str!("embedded_pack_defs/create_note.yaml"),
        ),
        (
            "append_note",
            include_str!("embedded_pack_defs/append_note.yaml"),
        ),
        (
            "publish_task_to_note",
            include_str!("embedded_pack_defs/publish_task_to_note.yaml"),
        ),
        (
            "open_note",
            include_str!("embedded_pack_defs/open_note.yaml"),
        ),
        ("open_pr", include_str!("embedded_pack_defs/open_pr.yaml")),
        (
            "search_notes",
            include_str!("embedded_pack_defs/search_notes.yaml"),
        ),
        (
            "android_snapshot",
            include_str!("embedded_pack_defs/android_snapshot.yaml"),
        ),
        (
            "android_act",
            include_str!("embedded_pack_defs/android_act.yaml"),
        ),
        (
            "android_screenshot",
            include_str!("embedded_pack_defs/android_screenshot.yaml"),
        ),
        (
            "android_notifications",
            include_str!("embedded_pack_defs/android_notifications.yaml"),
        ),
        (
            "android_app",
            include_str!("embedded_pack_defs/android_app.yaml"),
        ),
        (
            "save_selection_to_note",
            include_str!("embedded_pack_defs/save_selection_to_note.yaml"),
        ),
        (
            "update_monitor",
            include_str!("embedded_pack_defs/update_monitor.yaml"),
        ),
        (
            "propose_program_missions",
            include_str!("embedded_pack_defs/propose_program_missions.yaml"),
        ),
        (
            "review_program_missions",
            include_str!("embedded_pack_defs/review_program_missions.yaml"),
        ),
        (
            "delegation_files",
            include_str!("embedded_pack_defs/delegation_files.yaml"),
        ),
        (
            "delete_task",
            include_str!("embedded_pack_defs/delete_task.yaml"),
        ),
        (
            "deploy_app",
            include_str!("embedded_pack_defs/deploy_app.yaml"),
        ),
        (
            "distill_evidence",
            include_str!("embedded_pack_defs/distill_evidence.yaml"),
        ),
        (
            "evaluate_harness",
            include_str!("embedded_pack_defs/evaluate_harness.yaml"),
        ),
        (
            "forget_memory",
            include_str!("embedded_pack_defs/forget_memory.yaml"),
        ),
        (
            "get_active_executions",
            include_str!("embedded_pack_defs/get_active_executions.yaml"),
        ),
        (
            "get_execution_history",
            include_str!("embedded_pack_defs/get_execution_history.yaml"),
        ),
        (
            "get_task_details",
            include_str!("embedded_pack_defs/get_task_details.yaml"),
        ),
        (
            "inspect_backlog_delivery",
            include_str!("embedded_pack_defs/inspect_backlog_delivery.yaml"),
        ),
        (
            "inspect_agent",
            include_str!("embedded_pack_defs/inspect_agent.yaml"),
        ),
        (
            "list_agents",
            include_str!("embedded_pack_defs/list_agents.yaml"),
        ),
        (
            "list_artifacts",
            include_str!("embedded_pack_defs/list_artifacts.yaml"),
        ),
        (
            "list_episodes",
            include_str!("embedded_pack_defs/list_episodes.yaml"),
        ),
        (
            "list_memory_tiers",
            include_str!("embedded_pack_defs/list_memory_tiers.yaml"),
        ),
        (
            "list_proposals",
            include_str!("embedded_pack_defs/list_proposals.yaml"),
        ),
        (
            "list_scheduled_tasks",
            include_str!("embedded_pack_defs/list_scheduled_tasks.yaml"),
        ),
        (
            "replay_recipe",
            include_str!("embedded_pack_defs/replay_recipe.yaml"),
        ),
        (
            "list_tasks",
            include_str!("embedded_pack_defs/list_tasks.yaml"),
        ),
        (
            "magician_work_ledger",
            include_str!("embedded_pack_defs/magician_work_ledger.yaml"),
        ),
        (
            "notify_owner",
            include_str!("embedded_pack_defs/notify_owner.yaml"),
        ),
        (
            "ask_owner",
            include_str!("embedded_pack_defs/ask_owner.yaml"),
        ),
        (
            "promote_backlog_item",
            include_str!("embedded_pack_defs/promote_backlog_item.yaml"),
        ),
        (
            "propose_backlog_item",
            include_str!("embedded_pack_defs/propose_backlog_item.yaml"),
        ),
        (
            "review_backlog_delivery",
            include_str!("embedded_pack_defs/review_backlog_delivery.yaml"),
        ),
        (
            "propose_meeting",
            include_str!("embedded_pack_defs/propose_meeting.yaml"),
        ),
        (
            "request_owner_action",
            include_str!("embedded_pack_defs/request_owner_action.yaml"),
        ),
        (
            "read_trace",
            include_str!("embedded_pack_defs/read_trace.yaml"),
        ),
        (
            "read_program_state",
            include_str!("embedded_pack_defs/read_program_state.yaml"),
        ),
        (
            "reassign_task",
            include_str!("embedded_pack_defs/reassign_task.yaml"),
        ),
        (
            "refine_task",
            include_str!("embedded_pack_defs/refine_task.yaml"),
        ),
        (
            "retire_agent",
            include_str!("embedded_pack_defs/retire_agent.yaml"),
        ),
        (
            "run_coding_task",
            include_str!("embedded_pack_defs/run_coding_task.yaml"),
        ),
        (
            "contribute_to_project",
            include_str!("embedded_pack_defs/contribute_to_project.yaml"),
        ),
        (
            "apply_code_proposal",
            include_str!("embedded_pack_defs/apply_code_proposal.yaml"),
        ),
        (
            "run_project_checks",
            include_str!("embedded_pack_defs/run_project_checks.yaml"),
        ),
        (
            "screenshot_preview",
            include_str!("embedded_pack_defs/screenshot_preview.yaml"),
        ),
        (
            "capture_reference",
            include_str!("embedded_pack_defs/capture_reference.yaml"),
        ),
        ("run_task", include_str!("embedded_pack_defs/run_task.yaml")),
        (
            "save_preference",
            include_str!("embedded_pack_defs/save_preference.yaml"),
        ),
        (
            "search_memory",
            include_str!("embedded_pack_defs/search_memory.yaml"),
        ),
        (
            "app_action_compose",
            include_str!("embedded_pack_defs/app_action_compose.yaml"),
        ),
        (
            "app_action_invoke",
            include_str!("embedded_pack_defs/app_action_invoke.yaml"),
        ),
        (
            "app_data_query",
            include_str!("embedded_pack_defs/app_data_query.yaml"),
        ),
        (
            "app_data_search",
            include_str!("embedded_pack_defs/app_data_search.yaml"),
        ),
        (
            "app_data_compose",
            include_str!("embedded_pack_defs/app_data_compose.yaml"),
        ),
        (
            "app_discover",
            include_str!("embedded_pack_defs/app_discover.yaml"),
        ),
        (
            "app_memory_propose",
            include_str!("embedded_pack_defs/app_memory_propose.yaml"),
        ),
        (
            "switch_personality",
            include_str!("embedded_pack_defs/switch_personality.yaml"),
        ),
        (
            "activate_skill",
            include_str!("embedded_pack_defs/activate_skill.yaml"),
        ),
        (
            "deactivate_skill",
            include_str!("embedded_pack_defs/deactivate_skill.yaml"),
        ),
        (
            "stop_task",
            include_str!("embedded_pack_defs/stop_task.yaml"),
        ),
        (
            "system_status",
            include_str!("embedded_pack_defs/system_status.yaml"),
        ),
        (
            "task_state",
            include_str!("embedded_pack_defs/task_state.yaml"),
        ),
        (
            "treasurer",
            include_str!("embedded_pack_defs/treasurer.yaml"),
        ),
        (
            "unpublish_dashboard",
            include_str!("embedded_pack_defs/unpublish_dashboard.yaml"),
        ),
        (
            "update_agent",
            include_str!("embedded_pack_defs/update_agent.yaml"),
        ),
        (
            "update_memory_tier",
            include_str!("embedded_pack_defs/update_memory_tier.yaml"),
        ),
        (
            "update_task",
            include_str!("embedded_pack_defs/update_task.yaml"),
        ),
        (
            "update_delegation",
            include_str!("embedded_pack_defs/update_delegation.yaml"),
        ),
        (
            "update_program_state",
            include_str!("embedded_pack_defs/update_program_state.yaml"),
        ),
        // Filesystem / search / meta universals migrated from the
        // Decision::ChatControl rail (Phase 0.8c-10).
        (
            "tool_search",
            include_str!("embedded_pack_defs/tool_search.yaml"),
        ),
        (
            "get_agent_details",
            include_str!("embedded_pack_defs/get_agent_details.yaml"),
        ),
        (
            "find_agents_for_capability",
            include_str!("embedded_pack_defs/find_agents_for_capability.yaml"),
        ),
        (
            "apply_patch",
            include_str!("embedded_pack_defs/apply_patch.yaml"),
        ),
        (
            "edit_file",
            include_str!("embedded_pack_defs/edit_file.yaml"),
        ),
        ("glob", include_str!("embedded_pack_defs/glob.yaml")),
        ("grep", include_str!("embedded_pack_defs/grep.yaml")),
        (
            "content_read",
            include_str!("embedded_pack_defs/content_read.yaml"),
        ),
        (
            "authorize_content_read",
            include_str!("embedded_pack_defs/authorize_content_read.yaml"),
        ),
        (
            "content_search",
            include_str!("embedded_pack_defs/content_search.yaml"),
        ),
        (
            "working_set_search",
            include_str!("embedded_pack_defs/working_set_search.yaml"),
        ),
        (
            "working_set_read",
            include_str!("embedded_pack_defs/working_set_read.yaml"),
        ),
        (
            "web_fetch",
            include_str!("embedded_pack_defs/web_fetch.yaml"),
        ),
        (
            "web_search",
            include_str!("embedded_pack_defs/web_search.yaml"),
        ),
        (
            "web_answer",
            include_str!("embedded_pack_defs/web_answer.yaml"),
        ),
        (
            "read_file",
            include_str!("embedded_pack_defs/read_file.yaml"),
        ),
        (
            "write_file",
            include_str!("embedded_pack_defs/write_file.yaml"),
        ),
        (
            "macos_automation",
            include_str!("embedded_pack_defs/macos_automation.yaml"),
        ),
        (
            "imessage_send",
            include_str!("embedded_pack_defs/imessage_send.yaml"),
        ),
        // Compiled providers (Rust-backed): files, http, shell, search,
        // time_math, vector.
        ("files", include_str!("embedded_pack_defs/files.yaml")),
        ("http", include_str!("embedded_pack_defs/http.yaml")),
        ("shell", include_str!("embedded_pack_defs/shell.yaml")),
        ("search", include_str!("embedded_pack_defs/search.yaml")),
        ("meeting", include_str!("embedded_pack_defs/meeting.yaml")),
        (
            "time_math",
            include_str!("embedded_pack_defs/time_math.yaml"),
        ),
        ("vector", include_str!("embedded_pack_defs/vector.yaml")),
        (
            "catchup_merge",
            include_str!("embedded_pack_defs/catchup_merge.yaml"),
        ),
        // Specialized inner-loop dispatchers (Rust-backed): duckdb,
        // imessage, internal_data.
        ("duckdb", include_str!("embedded_pack_defs/duckdb.yaml")),
        ("imessage", include_str!("embedded_pack_defs/imessage.yaml")),
        (
            "internal_data",
            include_str!("embedded_pack_defs/internal_data.yaml"),
        ),
        // The scope's agent roster projected to its social-participation face
        // (queue item 6): the fifth host-read binder, and the narrowest.
        (
            "agent_roster_data",
            include_str!("embedded_pack_defs/agent_roster_data.yaml"),
        ),
        // The owner's task list projected to a narrow read face: the sixth
        // host-read binder.
        (
            "tasks_data",
            include_str!("embedded_pack_defs/tasks_data.yaml"),
        ),
        // Notes search and read with no host paths: the seventh host-read
        // binder.
        (
            "notes_data",
            include_str!("embedded_pack_defs/notes_data.yaml"),
        ),
        // Owner-granted memory reads for apps (`app_memory_read_v1`): the
        // eighth host-read binder.
        (
            "memory_data",
            include_str!("embedded_pack_defs/memory_data.yaml"),
        ),
        (
            "evidence_data",
            include_str!("embedded_pack_defs/evidence_data.yaml"),
        ),
        // Scoped meeting-rail host reads (queue item 5): live capture state,
        // the thread index, transcripts, takeaways, calendar context, keyword
        // retrieval. Reads only — capture control is its own action class.
        (
            "meetings_data",
            include_str!("embedded_pack_defs/meetings_data.yaml"),
        ),
        // Scoped thinking-map host reads (the 2.5 learning-read pattern
        // generalized; the Phase 4 Brainstorm verdict's re-open condition).
        (
            "thinking_maps_data",
            include_str!("embedded_pack_defs/thinking_maps_data.yaml"),
        ),
        (
            "media_edit",
            include_str!("embedded_pack_defs/media_edit.yaml"),
        ),
        (
            "media_edit_status",
            include_str!("embedded_pack_defs/media_edit_status.yaml"),
        ),
    ];
    EMBEDDED
}

pub fn embedded_compiled_pack_defs_ref() -> &'static [CapabilityPackDefinition] {
    static PARSED: OnceLock<Vec<CapabilityPackDefinition>> = OnceLock::new();
    PARSED
        .get_or_init(|| {
            embedded_compiled_pack_source_table()
                .iter()
                .map(|(name, content)| {
                    serde_yaml::from_str::<CapabilityPackDefinition>(content).unwrap_or_else(|e| {
                        panic!(
                            "embedded compiled pack def '{name}' failed to parse: {e} \
                             — this is a build error; check {name}.yaml in \
                             magician/src/magician_v2/execution/embedded_pack_defs/"
                        )
                    })
                })
                .collect()
        })
        .as_slice()
}

/// Exact embedded YAML for one compiled pack. This is the snapshotable
/// contract an app lock can pin. Missing names are not compiled packs.
pub fn embedded_compiled_pack_yaml(name: &str) -> Option<&'static str> {
    embedded_compiled_pack_source_table()
        .iter()
        .find(|(pack_name, _)| *pack_name == name)
        .map(|(_, yaml)| *yaml)
}

/// Owned compatibility view of the process-cached embedded catalog.
///
/// Callers that only inspect definitions should use
/// [`embedded_compiled_pack_defs_ref`] and avoid cloning the complete catalog.
pub fn embedded_compiled_pack_defs() -> Vec<CapabilityPackDefinition> {
    embedded_compiled_pack_defs_ref().to_vec()
}

/// Strip a leading `# Heading\n\n` (and optional bom/whitespace) from a
/// SKILL.md body before injecting as the pack `guide`. The legacy pack
/// YAML's `guide:` field has no top-level heading; the SKILL.md body
/// typically does. Removing it keeps the inner-loop guide rendering
/// byte-identical to the legacy YAML behaviour.
fn strip_leading_heading(body: &str) -> &str {
    let stripped = body.trim_start_matches('\u{FEFF}').trim_start();
    if let Some(rest) = stripped.strip_prefix("# ") {
        if let Some(nl) = rest.find('\n') {
            return rest[nl + 1..].trim_start_matches('\n');
        }
    }
    stripped
}

pub fn project_runtime_package_to_pack(
    skill_id: &str,
    description: &str,
    guide: Option<String>,
    skill_dir: &Path,
    resolved_skill_dir: &Path,
    package: SkillRuntimePackage,
) -> Result<CapabilityPackDefinition, String> {
    let spend = package
        .catalog
        .spend
        .as_ref()
        .map(project_runtime_spend)
        .transpose()?;
    let validated = validate_skill_runtime_contract(&package.contract)
        .map_err(|error| format!("runtime contract validation failed: {error}"))?;
    if matches!(&package.contract.runtime, RuntimeProtocol::Mcp { .. }) {
        return project_mcp_runtime_package_to_pack(skill_id, description, guide, package, spend);
    }
    let projected_profile = package.projected_profile_parameter().map(|parameter| {
        (
            parameter.name.to_owned(),
            parameter
                .enum_values
                .map(|values| values.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default(),
        )
    });
    let actions = package
        .actions
        .as_ref()
        .ok_or_else(|| "CLI runtime package has no typed actions".to_owned())?;
    let compiled = compile_typed_action_overrides(skill_id, validated, actions)
        .map_err(|error| format!("typed action compilation failed: {error}"))?;
    let executable_directory = {
        let candidate = resolved_skill_dir.join("bin");
        candidate.is_dir().then_some(candidate)
    };
    let legacy_secret_environment = {
        let candidate = skill_dir.join("config/.env");
        candidate.is_file().then_some(candidate)
    };

    let mut native_action_schemas = HashMap::with_capacity(compiled.actions.len());
    for (action_id, action) in &compiled.actions {
        let authored = actions
            .actions
            .get(action_id)
            .ok_or_else(|| "compiled action lost its authored source".to_owned())?;
        let properties = action
            .definition
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| "compiled action has no property map".to_owned())?;
        let compiled_required = action
            .definition
            .input_schema
            .get("required")
            .and_then(Value::as_array);
        let mut required = compiled_required
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|name| authored.parameters.contains_key(*name))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut parameters = authored.parameters.keys().cloned().collect::<Vec<_>>();
        let mut parameter_overrides = parameters
            .iter()
            .filter_map(|name| {
                properties
                    .get(name)
                    .map(|schema| (name.clone(), schema.clone()))
            })
            .collect::<HashMap<_, _>>();
        if let Some((profile_name, enum_values)) = projected_profile.as_ref() {
            if authored.parameters.contains_key(profile_name) {
                return Err(
                    "profile parameter alias collides with an authored action parameter".to_owned(),
                );
            }
            let mut profile_schema = properties
                .get("profile")
                .cloned()
                .ok_or_else(|| "selectable profile action lost its profile schema".to_owned())?;
            if !enum_values.is_empty() {
                profile_schema
                    .as_object_mut()
                    .ok_or_else(|| "profile schema is not an object".to_owned())?
                    .insert("enum".to_owned(), json!(enum_values));
            }
            if compiled_required
                .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("profile")))
            {
                required.push(profile_name.clone());
            }
            parameters.push(profile_name.clone());
            parameter_overrides.insert(profile_name.clone(), profile_schema);
        }
        if package.catalog.expose_timeout_control {
            let mut timeout_schema = properties
                .get("timeout_secs")
                .cloned()
                .ok_or_else(|| "runtime action lost its timeout control".to_owned())?;
            let Value::Object(timeout_schema_object) = &mut timeout_schema else {
                return Err("runtime action timeout control is not an object".to_owned());
            };
            let timeout_default_secs = package
                .catalog
                .timeout_default_secs
                .unwrap_or(action.invocation.timeout_ceiling_secs)
                .min(action.invocation.timeout_ceiling_secs);
            timeout_schema_object.insert("default".to_owned(), Value::from(timeout_default_secs));
            parameters.push("timeout_secs".to_owned());
            parameter_overrides.insert("timeout_secs".to_owned(), timeout_schema);
        }
        if package.catalog.expose_working_directory_control {
            let working_directory_name = package
                .projected_working_directory_parameter()
                .ok_or_else(|| "runtime action lost its working-directory metadata".to_owned())?
                .name
                .to_owned();
            if authored.parameters.contains_key(&working_directory_name) {
                return Err(
                    "working-directory parameter alias collides with an authored action parameter"
                        .to_owned(),
                );
            }
            let mut working_directory_schema = properties
                .get("working_dir")
                .cloned()
                .ok_or_else(|| "runtime action lost its working-directory control".to_owned())?;
            if let Some(default) = package.catalog.working_directory_default.as_ref() {
                working_directory_schema
                    .as_object_mut()
                    .ok_or_else(|| "working-directory schema is not an object".to_owned())?
                    .insert("default".to_owned(), Value::String(default.clone()));
            }
            parameters.push(working_directory_name.clone());
            parameter_overrides.insert(working_directory_name, working_directory_schema);
        }
        if !matches!(
            authored.stdin,
            TypedActionStdin::Inherit | TypedActionStdin::Denied
        ) {
            let stdin_schema = properties
                .get("stdin")
                .cloned()
                .ok_or_else(|| "runtime action lost its refined stdin control".to_owned())?;
            if compiled_required
                .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("stdin")))
            {
                required.push("stdin".to_owned());
            }
            parameters.push("stdin".to_owned());
            parameter_overrides.insert("stdin".to_owned(), stdin_schema);
        }
        let arg_mappings = action
            .invocation
            .mappings
            .iter()
            .filter_map(|mapping| project_typed_mapping(mapping).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        native_action_schemas.insert(
            action_id.clone(),
            NativeActionSchemaDef {
                description: Some(action.definition.description.clone()),
                reliability: None,
                parameters,
                required,
                parameter_overrides,
                argv: action.invocation.fixed_args.clone(),
                skip_tool_name: true,
                arg_mappings,
                suffix_args: action.invocation.suffix_args.clone(),
                timeout_secs: Some(u64::from(action.invocation.timeout_ceiling_secs)),
            },
        );
    }

    let RuntimeProtocol::Cli { limits, .. } = &package.contract.runtime else {
        return Err("capability-pack projection supports only CLI packages".to_owned());
    };
    let timeout_ceiling_secs = limits.timeout_secs.map(u64::from).unwrap_or(30);
    let timeout_default_secs = package
        .catalog
        .timeout_default_secs
        .map(u64::from)
        .or_else(|| {
            native_action_schemas
                .values()
                .filter_map(|action| action.timeout_secs)
                .min()
        })
        .unwrap_or(timeout_ceiling_secs);
    let env = project_profile_environment(&package)?;
    let auth = project_compatibility_auth(&package, &compiled.execution.executable, &env)?;
    let command_parameter = ParameterDef {
        name: "command".to_owned(),
        required: true,
        default: None,
        description: Some("Describe the bounded operation for the inner tool loop.".to_owned()),
        param_type: Some(ParameterType::String),
        aliases: Vec::new(),
        enum_values: None,
        schema: serde_json::json!({
            "type": "string",
            "description": "Describe the bounded operation for the inner tool loop."
        }),
    };
    let timeout_parameter = ParameterDef {
        name: "timeout_secs".to_owned(),
        required: false,
        default: Some(timeout_default_secs.to_string()),
        description: Some("Command timeout in seconds.".to_owned()),
        param_type: Some(ParameterType::Integer),
        aliases: Vec::new(),
        enum_values: None,
        schema: serde_json::json!({
            "type": "integer",
            "description": "Command timeout in seconds.",
            "minimum": 1,
            "maximum": timeout_ceiling_secs,
            "default": timeout_default_secs
        }),
    };
    // A one-action package already has one unambiguous public input contract.
    // Re-project that exact action at the pack boundary instead of replacing it
    // with the legacy inner-loop `command` abstraction. This is what makes a
    // single SKILL.md genuinely drop-in: callers that still inspect pack-level
    // parameters see the same names, defaults, and types as action callers.
    // Multi-action packages retain `command` because the pack-level surface
    // cannot represent a union of action-specific required fields safely.
    let mut pack_parameters = if native_action_schemas.len() == 1 {
        let action = native_action_schemas
            .values()
            .next()
            .ok_or_else(|| "single-action runtime package lost its action".to_owned())?;
        action
            .parameters
            .iter()
            .map(|name| {
                let schema = action.parameter_overrides.get(name).ok_or_else(|| {
                    "single-action runtime package lost a parameter schema".to_owned()
                })?;
                project_pack_parameter(
                    name,
                    schema,
                    action.required.iter().any(|required| required == name),
                )
            })
            .collect::<Result<Vec<_>, String>>()?
    } else {
        vec![command_parameter]
    };
    let single_action_parameters = native_action_schemas.len() == 1;
    if let Some((profile_name, enum_values)) = projected_profile.as_ref() {
        if single_action_parameters {
            // The action projection above already includes the profile alias.
            // Do not duplicate it at pack level.
            if !pack_parameters
                .iter()
                .any(|parameter| parameter.name == *profile_name)
            {
                return Err("single-action runtime package lost its profile parameter".to_owned());
            }
        } else {
            let default = match &package.contract.auth.profile_selection {
                ProfileSelection::Selectable { default } => default.clone(),
                _ => None,
            };
            let mut schema = serde_json::json!({
                "type": "string",
                "description": "Configured local profile alias; never an email or credential.",
                "minLength": 1,
                "maxLength": 128,
                "pattern": "^[A-Za-z0-9._-]+$"
            });
            if let Some(default) = default.as_ref() {
                schema["default"] = Value::String(default.clone());
            }
            if !enum_values.is_empty() {
                schema["enum"] = json!(enum_values);
            }
            pack_parameters.push(ParameterDef {
                name: profile_name.clone(),
                required: default.is_none(),
                default,
                description: Some(
                    "Configured local profile alias; never an email or credential.".to_owned(),
                ),
                param_type: Some(ParameterType::String),
                aliases: Vec::new(),
                enum_values: (!enum_values.is_empty()).then(|| enum_values.clone()),
                schema,
            });
        }
    }
    if package.catalog.expose_working_directory_control && !single_action_parameters {
        let working_directory_name = package
            .projected_working_directory_parameter()
            .ok_or_else(|| "runtime pack lost its working-directory metadata".to_owned())?
            .name
            .to_owned();
        let default = package.catalog.working_directory_default.clone();
        let mut schema = serde_json::json!({
            "type": "string",
            "description": "Working directory authorized by the governed runtime.",
            "minLength": 1,
            "maxLength": tool_runtime_core::manifest_synthesis::MAX_SYNTHESIZED_WORKING_DIRECTORY_BYTES,
            "format": "workspace-relative-path"
        });
        if let Some(default) = default.as_ref() {
            schema["default"] = Value::String(default.clone());
        }
        pack_parameters.push(ParameterDef {
            name: working_directory_name,
            required: false,
            default,
            description: Some("Working directory authorized by the governed runtime.".to_owned()),
            param_type: Some(ParameterType::String),
            aliases: Vec::new(),
            enum_values: None,
            schema,
        });
    }
    if !single_action_parameters {
        pack_parameters.push(timeout_parameter);
    }

    Ok(CapabilityPackDefinition {
        name: skill_id.to_owned(),
        description: Some(description.to_owned()),
        version: None,
        guide,
        native_action_schemas,
        parameters: pack_parameters,
        implementation: ImplementationType::Primitive {
            runtime_package: Some(Arc::new(super::capability::GovernedRuntimeImplementation {
                package: package.clone(),
                actions: Some(compiled.clone()),
                executable_directory,
                legacy_secret_environment,
            })),
            provider_name: None,
            intent_description: None,
            command: Some(vec![compiled.execution.executable.clone()]),
            cwd: None,
            env,
            suffix_args: Vec::new(),
            timeout_secs: Some(timeout_default_secs),
            operation: None,
            prompt: None,
        },
        execution: Some(ExecutionMetadata {
            requires_browser_session: Some(false),
            default_timeout_secs: Some(timeout_default_secs),
            chat_inline_adapter: project_chat_inline_adapter(
                package.catalog.chat_inline_adapter.as_deref(),
            )?,
            categories: package.catalog.categories,
            sandbox: Some("shell".to_owned()),
            composition_category: package.catalog.composition_category,
            spend,
            ..ExecutionMetadata::default()
        }),
        auth,
        reliability: None,
        result_projection: None,
    })
}

fn project_chat_inline_adapter(value: Option<&str>) -> Result<Option<ChatInlineAdapter>, String> {
    match value {
        None => Ok(None),
        Some("tutor_screen_draw") => Ok(Some(ChatInlineAdapter::TutorScreenDraw)),
        Some(_) => Err("runtime catalog declares an unknown chat inline adapter".to_owned()),
    }
}

fn project_mcp_runtime_package_to_pack(
    skill_id: &str,
    description: &str,
    guide: Option<String>,
    package: SkillRuntimePackage,
    spend: Option<SpendDeclaration>,
) -> Result<CapabilityPackDefinition, String> {
    let projected = project_mcp_catalog(&package)?;
    let timeout_secs = projected.timeout_secs;
    let native_action_schemas = projected
        .actions
        .into_iter()
        .map(|(name, action)| {
            let parameters = action
                .parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect::<Vec<_>>();
            let required = action
                .parameters
                .iter()
                .filter(|parameter| parameter.required)
                .map(|parameter| parameter.name.clone())
                .collect::<Vec<_>>();
            let parameter_overrides = action
                .parameters
                .into_iter()
                .map(|parameter| (parameter.name, parameter.schema))
                .collect::<HashMap<_, _>>();
            (
                name,
                NativeActionSchemaDef {
                    description: Some(action.description),
                    parameters,
                    required,
                    parameter_overrides,
                    skip_tool_name: true,
                    timeout_secs: Some(timeout_secs),
                    ..NativeActionSchemaDef::default()
                },
            )
        })
        .collect::<HashMap<_, _>>();

    let pack_parameters = vec![ParameterDef {
        name: "command".to_owned(),
        required: true,
        default: None,
        description: Some("Describe the bounded MCP operation for the inner tool loop.".to_owned()),
        param_type: Some(ParameterType::String),
        aliases: Vec::new(),
        enum_values: None,
        schema: json!({
            "type": "string",
            "description": "Describe the bounded MCP operation for the inner tool loop."
        }),
    }];
    let auth = (!matches!(package.contract.auth.requirement, AuthRequirement::None)).then(|| {
        CapabilityAuthConfig {
            required: matches!(
                package.contract.auth.requirement,
                AuthRequirement::Required | AuthRequirement::AtLeastOne
            ),
            setup_command: None,
            check_command: None,
            reauth_command: None,
            error_patterns: Vec::new(),
        }
    });
    let chat_inline_adapter =
        project_chat_inline_adapter(package.catalog.chat_inline_adapter.as_deref())?;
    let categories = package.catalog.categories.clone();
    let composition_category = package.catalog.composition_category.clone();

    Ok(CapabilityPackDefinition {
        name: skill_id.to_owned(),
        description: Some(description.to_owned()),
        version: None,
        guide,
        native_action_schemas,
        parameters: pack_parameters,
        implementation: ImplementationType::Primitive {
            runtime_package: Some(Arc::new(super::capability::GovernedRuntimeImplementation {
                package,
                actions: None,
                executable_directory: None,
                legacy_secret_environment: None,
            })),
            provider_name: None,
            intent_description: None,
            command: None,
            cwd: None,
            env: HashMap::new(),
            suffix_args: Vec::new(),
            timeout_secs: Some(timeout_secs),
            operation: None,
            prompt: None,
        },
        execution: Some(ExecutionMetadata {
            requires_browser_session: Some(false),
            default_timeout_secs: Some(timeout_secs),
            chat_inline_adapter,
            categories,
            sandbox: Some("none".to_owned()),
            composition_category,
            spend,
        }),
        auth,
        reliability: None,
        result_projection: None,
    })
}

pub(crate) fn project_runtime_spend(
    spend: &SkillRuntimeSpendMetadata,
) -> Result<SpendDeclaration, String> {
    match spend {
        SkillRuntimeSpendMetadata::Committed {
            commodity,
            cost_parameter,
        } => Ok(SpendDeclaration::Committed {
            commodity: commodity.clone(),
            cost_parameter: cost_parameter.clone(),
        }),
        SkillRuntimeSpendMetadata::Metered {
            commodity,
            estimated_cost,
            max_cost,
        } => Ok(SpendDeclaration::Metered {
            commodity: commodity.clone(),
            estimated_cost: estimated_cost
                .parse()
                .map_err(|_| "runtime spend estimated_cost is invalid".to_owned())?,
            max_cost: max_cost
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(|_| "runtime spend max_cost is invalid".to_owned())?,
        }),
        SkillRuntimeSpendMetadata::Counted {
            commodity,
            cost_per_action,
        } => Ok(SpendDeclaration::Counted {
            commodity: commodity.clone(),
            cost_per_action: cost_per_action
                .parse()
                .map_err(|_| "runtime spend cost_per_action is invalid".to_owned())?,
        }),
    }
}

fn project_pack_parameter(
    name: &str,
    schema: &Value,
    required: bool,
) -> Result<ParameterDef, String> {
    let object = schema
        .as_object()
        .ok_or_else(|| "single-action runtime parameter schema is not an object".to_owned())?;
    let param_type = match object.get("type").and_then(Value::as_str) {
        Some("string") => ParameterType::String,
        Some("integer") => ParameterType::Integer,
        Some("number") => ParameterType::Number,
        Some("boolean") => ParameterType::Boolean,
        Some("array") => ParameterType::Array,
        Some("object") => ParameterType::Object,
        _ => return Err("single-action runtime parameter type is unsupported".to_owned()),
    };
    let default = object.get("default").map(|value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    });
    let enum_values = object
        .get("enum")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| match value {
                    Value::String(text) => Ok(text.clone()),
                    Value::Number(number) => Ok(number.to_string()),
                    Value::Bool(boolean) => Ok(boolean.to_string()),
                    _ => {
                        Err("single-action runtime parameter enum is not scalar-valued".to_owned())
                    },
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?;
    Ok(ParameterDef {
        name: name.to_owned(),
        required,
        default,
        description: object
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        param_type: Some(param_type),
        aliases: Vec::new(),
        enum_values,
        schema: schema.clone(),
    })
}

fn project_typed_mapping(
    mapping: &TypedArgumentMapping,
) -> Result<Option<CommandArgMapping>, String> {
    match mapping {
        TypedArgumentMapping::Positional { parameter } => Ok(Some(CommandArgMapping::Positional {
            param: parameter.clone(),
        })),
        TypedArgumentMapping::Flag {
            flag, parameter, ..
        }
        | TypedArgumentMapping::JsonFlag { flag, parameter } => Ok(Some(CommandArgMapping::Flag {
            flag: flag.clone(),
            param: parameter.clone(),
        })),
        TypedArgumentMapping::BoolFlag { flag, parameter } => {
            Ok(Some(CommandArgMapping::BoolFlag {
                flag: flag.clone(),
                param: parameter.clone(),
            }))
        },
        TypedArgumentMapping::Passthrough { parameter } => {
            Ok(Some(CommandArgMapping::Passthrough {
                param: parameter.clone(),
            }))
        },
        TypedArgumentMapping::RepeatedFlag { .. } => {
            Err("legacy dispatcher projection cannot represent a repeated typed flag".to_owned())
        },
        TypedArgumentMapping::Literal { arguments } => Ok(Some(CommandArgMapping::FixedArgs {
            args: arguments.clone(),
        })),
        TypedArgumentMapping::SplitPositional { parameter, .. } => {
            Ok(Some(CommandArgMapping::SplitPositional {
                param: parameter.clone(),
            }))
        },
        TypedArgumentMapping::RuntimeControl { .. } => Ok(None),
    }
}

fn project_profile_environment(
    package: &SkillRuntimePackage,
) -> Result<HashMap<String, String>, String> {
    let AuthStorage::ScopedDirectory { namespace, .. } = &package.contract.auth.storage else {
        return Ok(HashMap::new());
    };
    let profile = match &package.contract.auth.profile_selection {
        ProfileSelection::Fixed { alias } => alias.clone(),
        ProfileSelection::Selectable { .. } => {
            let parameter = package
                .projected_profile_parameter()
                .ok_or_else(|| "selectable profile lost its model parameter".to_owned())?;
            format!("{{{}}}", parameter.name)
        },
        ProfileSelection::None | ProfileSelection::Implicit => {
            return Err("scoped profile storage requires an explicit profile selector".to_owned())
        },
    };
    let root = format!("{{scope_capability_auth_root}}/{namespace}-{profile}");
    let mut env = HashMap::new();
    for injection in &package.contract.auth.injections {
        let InjectionSource::ProfileAuthRoot { path } = &injection.source else {
            continue;
        };
        let InjectionTarget::Environment { name } = &injection.target else {
            continue;
        };
        let value = path.iter().fold(root.clone(), |mut value, segment| {
            value.push('/');
            value.push_str(segment);
            value
        });
        env.insert(name.clone(), value);
    }
    Ok(env)
}

fn project_compatibility_auth(
    package: &SkillRuntimePackage,
    executable: &str,
    env: &HashMap<String, String>,
) -> Result<Option<CapabilityAuthConfig>, String> {
    if matches!(package.contract.auth.requirement, AuthRequirement::None) {
        return Ok(None);
    }
    let lifecycle = &package.contract.auth.lifecycle;
    let command = |args: &[String]| {
        let mut prefixes = env.iter().collect::<Vec<_>>();
        prefixes.sort_by(|left, right| left.0.cmp(right.0));
        let mut parts = prefixes
            .into_iter()
            .map(|(name, value)| format!("{name}={}", shell_quote(value)))
            .collect::<Vec<_>>();
        parts.push(shell_quote(executable));
        parts.extend(args.iter().map(|value| shell_quote(value)));
        parts.join(" ")
    };
    let check_command = lifecycle.status.as_ref().map(|hook| command(&hook.args));
    let login_command = lifecycle.login.as_ref().map(|hook| command(&hook.args));
    Ok(Some(CapabilityAuthConfig {
        required: matches!(
            package.contract.auth.requirement,
            AuthRequirement::Required | AuthRequirement::AtLeastOne
        ),
        setup_command: login_command.clone(),
        check_command,
        reauth_command: login_command,
        error_patterns: if package.contract.auth.provider.as_deref() == Some("google-workspace") {
            vec![
                "401".to_owned(),
                "authError".to_owned(),
                "No OAuth client configured".to_owned(),
                "Token has been expired or revoked".to_owned(),
                "ACCESS_TOKEN_SCOPE_INSUFFICIENT".to_owned(),
                "insufficientPermissions".to_owned(),
                "insufficient authentication scopes".to_owned(),
                "Request had insufficient authentication scopes".to_owned(),
                "Request had insufficient authentication scopes.".to_owned(),
            ]
        } else {
            Vec::new()
        },
    }))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./{}-".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Load `CapabilityPackDefinition`s from a skills directory.
///
/// Walks `skills_dir/<skill-name>/` looking for exactly one executable
/// catalog source: a governed runtime package in `SKILL.md`, or a legacy
/// `tool_schema.yaml` while that skill is still pending migration.
///
/// 1. Parse `tool_schema.yaml` as a `CapabilityPackDefinition`. The
///    migration script strips `description`, `guide`, and `version`
///    (which now live in the sibling SKILL.md), so those fields are
///    typically absent.
/// 2. Read the sibling `SKILL.md` via `SkillLoader`. Inject
///    `description` from the frontmatter and `guide` from the body
///    (with the leading `# Heading\n\n` stripped).
///
/// A directory containing both sources fails closed instead of creating two
/// active routes. Plain steering skills with neither source are skipped.
///
/// Errors are logged and the pack is skipped; the rest of the directory
/// continues to load.
pub fn pack_def_from_app_computed_capability(
    entry: &crate::magician_v2::apps::capability_catalog::AppComputedCapabilityOverlayEntry,
) -> Option<CapabilityPackDefinition> {
    let source = std::str::from_utf8(&entry.skill_document).ok()?;
    if let Ok(Some(package)) = parse_skill_runtime_package(source) {
        let empty = PathBuf::from("/dev/null");
        if let Ok(pack) = project_runtime_package_to_pack(
            &entry.tool_name,
            &entry.description,
            None,
            &empty,
            &empty,
            package,
        ) {
            return Some(pack);
        }
    }
    None
}

/// One skill the loader could not turn into a capability, and why. The
/// scoped capability cache reads these to refuse a catalog rebuild that
/// would silently drop a skill the previous catalog served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillLoadFailure {
    /// The skill directory's name.
    pub skill: String,
    pub path: String,
    pub reason: String,
}

/// What a skills directory loaded: the packs, the skills (by directory
/// name) that produced at least one, and every skill that failed.
#[derive(Debug, Default)]
pub struct SkillsDirLoad {
    pub packs: Vec<CapabilityPackDefinition>,
    pub loaded_skills: Vec<String>,
    pub failures: Vec<SkillLoadFailure>,
}

pub fn load_pack_defs_from_skills_dir(skills_dir: &Path) -> Vec<CapabilityPackDefinition> {
    load_skills_dir(skills_dir).packs
}

pub fn load_skills_dir(skills_dir: &Path) -> SkillsDirLoad {
    let mut load = SkillsDirLoad::default();
    let mut packs = Vec::new();
    let mut failures: Vec<SkillLoadFailure> = Vec::new();
    let mut fail = |skill_dir: &Path, path: &Path, reason: String| {
        warn!("{reason} ('{}')", path.display());
        failures.push(SkillLoadFailure {
            skill: skill_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.display().to_string(),
            reason,
        });
    };

    let mut paths: Vec<PathBuf> = match std::fs::read_dir(skills_dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(e) => {
            debug!(
                "Could not read skills dir '{}': {}",
                skills_dir.display(),
                e
            );
            load.packs = packs;
            return load;
        },
    };
    paths.sort();
    let mut loaded_skills: Vec<String> = Vec::new();

    // Use SkillLoader to source descriptions + bodies — it already
    // handles frontmatter parsing, BOM stripping, and lazy body
    // loading. Discovery is cheap (frontmatter only).
    let manifests =
        match crate::magician_v2::skills::SkillLoader::new(vec![skills_dir.to_path_buf()])
            .discover()
        {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    "skill discovery failed at {}: {} — pack defs will load \
                 without description/guide enrichment",
                    skills_dir.display(),
                    e
                );
                Vec::new()
            },
        };
    let manifest_by_dir: HashMap<PathBuf, &crate::magician_v2::skills::SkillManifest> = manifests
        .iter()
        .map(|m| (m.source_dir.clone(), m))
        .collect();

    for skill_dir in paths.iter().cloned() {
        // `tool_schema.yaml` is a host-absolute symlink into skillshub on a
        // materialized scope; rewrite the target for this environment (identity
        // on a native host) so the is_file + read resolve in a container —
        // otherwise the raw symlink dangles and every cli-template skill is
        // silently dropped from the registry (mirror scope_loader.rs:92).
        let schema_path = crate::magician_v2::skills::path_rewrite::resolve_skill_path(
            &skill_dir.join("tool_schema.yaml"),
        );
        let skill_path = crate::magician_v2::skills::path_rewrite::resolve_skill_path(
            &skill_dir.join("SKILL.md"),
        );
        let runtime_package = if skill_path.is_file() {
            match crate::magician_v2::skills::embedded_extensions::read_bounded_skill_markdown(
                &skill_dir.join("SKILL.md"),
            ) {
                Ok(source)
                    if source.contains("runtime_contract:")
                        || source.contains("runtime_actions:") =>
                {
                    match parse_skill_runtime_package(&source) {
                        Ok(package) => package,
                        Err(error) => {
                            fail(
                                &skill_dir,
                                &skill_path,
                                format!("failed to parse governed runtime package: {error}"),
                            );
                            continue;
                        },
                    }
                },
                Ok(_) => None,
                Err(error) => {
                    fail(
                        &skill_dir,
                        &skill_path,
                        format!("failed to read SKILL.md: {error}"),
                    );
                    continue;
                },
            }
        } else {
            None
        };
        if let Some(package) = runtime_package {
            if schema_path.is_file() {
                fail(
                    &skill_dir,
                    &schema_path,
                    "skill declares both a governed runtime package and legacy tool_schema.yaml; \
                     skipping duplicate active routes"
                        .to_string(),
                );
                continue;
            }
            let Some(manifest) = manifest_by_dir.get(&skill_dir) else {
                fail(
                    &skill_dir,
                    &skill_path,
                    "governed runtime package has no loadable SKILL.md manifest".to_string(),
                );
                continue;
            };
            let guide = match manifest.body() {
                Ok(body) => {
                    let body = strip_leading_heading(body);
                    (!body.trim().is_empty()).then(|| body.to_owned())
                },
                Err(error) => {
                    warn!(
                        "Failed to read governed SKILL.md body at '{}': {}",
                        skill_path.display(),
                        error
                    );
                    None
                },
            };
            match project_runtime_package_to_pack(
                &manifest.name,
                &manifest.description,
                guide,
                &skill_dir,
                skill_path.parent().unwrap_or(&skill_dir),
                package,
            ) {
                Ok(pack) => {
                    debug!(
                        "Loaded governed capability pack '{}' from {}",
                        pack.name,
                        skill_path.display()
                    );
                    packs.push(pack);
                    loaded_skills.push(skill_name_of(&skill_dir));
                },
                Err(error) => fail(
                    &skill_dir,
                    &skill_path,
                    format!("failed to compile governed runtime package: {error}"),
                ),
            }
            continue;
        }
        if !schema_path.is_file() {
            continue;
        }
        let content = match std::fs::read_to_string(&schema_path) {
            Ok(c) => c,
            Err(e) => {
                fail(
                    &skill_dir,
                    &schema_path,
                    format!("failed to read tool_schema.yaml: {e}"),
                );
                continue;
            },
        };
        let mut pack: CapabilityPackDefinition = match serde_yaml::from_str(&content) {
            Ok(p) => p,
            Err(e) => {
                fail(
                    &skill_dir,
                    &schema_path,
                    format!("failed to parse tool_schema.yaml: {e}"),
                );
                continue;
            },
        };

        match manifest_by_dir.get(&skill_dir) {
            Some(manifest) => {
                if pack.description.is_none() && !manifest.description.is_empty() {
                    pack.description = Some(manifest.description.clone());
                }
                if pack.guide.is_none() {
                    match manifest.body() {
                        Ok(body) => {
                            let stripped = strip_leading_heading(body);
                            if !stripped.trim().is_empty() {
                                pack.guide = Some(stripped.to_string());
                            }
                        },
                        Err(e) => {
                            warn!(
                                "Failed to read SKILL.md body for pack '{}' at '{}': {} \
                                 — guide will be empty",
                                pack.name,
                                skill_dir.display(),
                                e
                            );
                        },
                    }
                }
            },
            None => {
                warn!(
                    "tool_schema.yaml at '{}' has no sibling SKILL.md; \
                     pack '{}' will load with empty description and guide",
                    schema_path.display(),
                    pack.name
                );
            },
        }

        debug!(
            "Loaded capability pack '{}' from {}",
            pack.name,
            schema_path.display()
        );
        packs.push(pack);
        loaded_skills.push(skill_name_of(&skill_dir));
    }

    // A skill's embedded extensions are contracts other subsystems parse
    // later (the content reader declares its manifest in SKILL.md and the
    // content-source registry rejects unknown fields). A pack still loads
    // when its extension is invalid, but the skill has lost a capability,
    // and a catalog rebuild must know that before it replaces the last one
    // that served it.
    for skill_dir in &paths {
        let skill_path = crate::magician_v2::skills::path_rewrite::resolve_skill_path(
            &skill_dir.join("SKILL.md"),
        );
        if !skill_path.is_file() {
            continue;
        }
        if let Err(error) =
            crate::magician_v2::content_sources::load_optional_capability_reader_manifest(
                &skill_path,
            )
        {
            fail(
                skill_dir,
                &skill_path,
                format!("invalid content_reader extension: {error:#}"),
            );
        }
    }

    drop(fail);
    load.packs = packs;
    load.loaded_skills = loaded_skills;
    load.failures = failures;
    load
}

fn skill_name_of(skill_dir: &Path) -> String {
    skill_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::ExecutionConfig;

    fn vision_provider_for_root(root: &Path) -> AnalyzeImageViaOpenAiCapabilityProvider {
        let mut provider = AnalyzeImageViaOpenAiCapabilityProvider::new();
        provider.file_sandbox = FileSandboxConfig {
            allowed_roots: vec![root.display().to_string()],
            ..FileSandboxConfig::default()
        };
        provider
    }

    #[tokio::test]
    async fn openai_vision_rejects_files_outside_the_effective_sandbox() {
        let allowed = tempfile::tempdir().expect("allowed tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let image = outside.path().join("secret.png");
        std::fs::write(&image, b"not-a-real-image").expect("write fixture");
        let error = vision_provider_for_root(allowed.path())
            .execute_analyze_image_via_openai(AnalyzeImageViaOpenAiInvocation {
                image_ref: image.display().to_string(),
                question: None,
                model: None,
            })
            .await
            .expect_err("outside-root image must fail");
        assert!(matches!(error, ExecutionError::PathAccessDenied { .. }));
    }

    #[tokio::test]
    async fn openai_vision_rejects_oversized_files_before_provider_dispatch() {
        let allowed = tempfile::tempdir().expect("allowed tempdir");
        let image = allowed.path().join("oversized.png");
        let file = std::fs::File::create(&image).expect("create sparse fixture");
        file.set_len(ANALYZE_IMAGE_VIA_OPENAI_MAX_BYTES + 1)
            .expect("size sparse fixture");
        let error = vision_provider_for_root(allowed.path())
            .execute_analyze_image_via_openai(AnalyzeImageViaOpenAiInvocation {
                image_ref: image.display().to_string(),
                question: None,
                model: None,
            })
            .await
            .expect_err("oversized image must fail");
        assert!(error.to_string().contains("maximum accepted image"));
    }

    #[tokio::test]
    async fn openai_vision_fails_closed_when_router_or_telemetry_is_absent() {
        let allowed = tempfile::tempdir().expect("allowed tempdir");
        let image = allowed.path().join("small.png");
        std::fs::write(&image, b"fixture").expect("write fixture");
        let error = vision_provider_for_root(allowed.path())
            .execute_analyze_image_via_openai(AnalyzeImageViaOpenAiInvocation {
                image_ref: image.display().to_string(),
                question: None,
                model: None,
            })
            .await
            .expect_err("unobserved provider call must fail");
        assert!(matches!(error, ExecutionError::Configuration(_)));
        assert!(error
            .to_string()
            .contains("operation LLM router is not configured"));
    }

    fn try_build_test_registry() -> Option<Arc<CapabilityRegistry>> {
        let magicutor_client = match std::panic::catch_unwind(|| {
            let exec_config = ExecutionConfig::new(url::Url::parse("http://localhost:0").unwrap());
            MagicutorClient::new(exec_config)
        }) {
            Ok(Ok(c)) => Arc::new(c),
            Ok(Err(_)) | Err(_) => return None,
        };

        let (registry, _) = build_compiled_registry(
            magicutor_client,
            FileSandboxConfig::default(),
            ShellSandboxConfig::default(),
            Vec::new(),
            None,
            PathBuf::from("."),
            None,
            None,
            None,
            None, // agent_resources
            None, // compiled_handlers
        );
        Some(registry)
    }

    /// Build the registry the way a scope does at boot — every embedded
    /// compiled pack offered — but with no late-binding handlers installed.
    fn try_build_test_registry_with_embedded_packs() -> Option<Arc<CapabilityRegistry>> {
        let magicutor_client = match std::panic::catch_unwind(|| {
            let exec_config = ExecutionConfig::new(url::Url::parse("http://localhost:0").unwrap());
            MagicutorClient::new(exec_config)
        }) {
            Ok(Ok(c)) => Arc::new(c),
            Ok(Err(_)) | Err(_) => return None,
        };
        let (registry, _) = build_compiled_registry(
            magicutor_client,
            FileSandboxConfig::default(),
            ShellSandboxConfig::default(),
            embedded_compiled_pack_defs_ref().to_vec(),
            None,
            PathBuf::from("."),
            None,
            None,
            None,
            None, // agent_resources
            None, // compiled_handlers
        );
        Some(registry)
    }

    /// Build the registry the way a scope does — every embedded compiled
    /// pack, the boot's handler registry, and a resource bundle for the
    /// handler-backed providers to bind against.
    fn try_build_test_registry_with_handlers(
        root: &std::path::Path,
    ) -> Option<Arc<CapabilityRegistry>> {
        use crate::config::MagicianConfig;
        use crate::magician_v2::agents::definition_store::AgentDefinitionStore;
        use crate::magician_v2::agents::memory::AgentMemoryResolver;
        use crate::magician_v2::agents::storage::AgentStorage;
        use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
        use crate::magician_v2::execution::agent_resources::AgentResources;

        let magicutor_client = match std::panic::catch_unwind(|| {
            let exec_config = ExecutionConfig::new(url::Url::parse("http://localhost:0").unwrap());
            MagicutorClient::new(exec_config)
        }) {
            Ok(Ok(c)) => Arc::new(c),
            Ok(Err(_)) | Err(_) => return None,
        };
        let resources = Arc::new(AgentResources {
            magician_config: Arc::new(std::sync::RwLock::new(MagicianConfig::default())),
            memory_resolver: Arc::new(AgentMemoryResolver::new(root)),
            agent_definition_store: Arc::new(AgentDefinitionStore::new(AgentStorage::new(root))),
            artifact_workspace: ArtifactV2Workspace::new(root),
            artifact_v2_service: None,
            event_broadcaster: None,
            operation_llm_router: None,
            secret_store_resolver: None,
            content_acquisition_resolver: Arc::new(std::sync::RwLock::new(None)),
            file_sandbox: Default::default(),
            tool_index: Arc::new(std::sync::OnceLock::new()),
            user_request_service: None,
            agent_runtime: None,
        });
        let handlers = default_compiled_handler_registry();
        let (registry, _) = build_compiled_registry(
            magicutor_client,
            FileSandboxConfig::default(),
            ShellSandboxConfig::default(),
            embedded_compiled_pack_defs_ref().to_vec(),
            None,
            PathBuf::from("."),
            None,
            None,
            None,
            Some(resources),
            Some(&handlers),
        );
        Some(registry)
    }

    /// A handler registered at boot is a provider the scope can serve, so its
    /// pack binds — through the same generic provider the hand-written blocks
    /// use — without a block of its own. Until 2026-09-21 only the tools with
    /// a block bound; `android_snapshot`, `android_act`, `android_screenshot`,
    /// `android_app`, the notes and monitor tools, `web_answer` and
    /// `media_edit` were handler-registered, never bound, and withheld from
    /// every scope snapshot as "no provider" — Pilot searched its own four
    /// verbs and found nothing.
    #[test]
    fn every_handler_registered_pack_binds_without_a_hand_written_block() {
        let root = std::env::temp_dir().join(format!(
            "compiled_registry_handlers_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("temp root");
        let Some(registry) = try_build_test_registry_with_handlers(&root) else {
            eprintln!("Skipping: MagicutorClient unavailable");
            return;
        };
        for name in [
            "android_snapshot",
            "android_act",
            "android_screenshot",
            "android_app",
            "android_notifications",
            "web_answer",
            "media_edit",
            "create_note",
        ] {
            assert!(
                registry.has(name),
                "`{name}` has a registered handler and must be bound in a scope registry"
            );
        }
        let withheld = registry.withhold_unbound_compiled_packs();
        let still_handler_backed: Vec<&String> = withheld
            .iter()
            .filter(|name| default_compiled_handler_registry().get(name).is_some())
            .collect();
        assert!(
            still_handler_backed.is_empty(),
            "handler-backed packs were withheld as unbound: {still_handler_backed:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The build keeps every compiled definition — the boot and the scope
    /// resolver bind deferred providers in place from those definitions — and
    /// the caller withholds what stayed unbound before publishing a catalog.
    /// A build with no handlers therefore publishes `task_state` for the boot
    /// to bind, and withholding afterwards removes `read_file` and the rest of
    /// the handler-bound family that nothing could serve.
    #[test]
    fn a_handlerless_build_keeps_deferred_definitions_and_withholding_clears_the_unbound() {
        let registry = match try_build_test_registry_with_embedded_packs() {
            Some(r) => r,
            None => {
                eprintln!("Skipping: MagicutorClient unavailable");
                return;
            },
        };
        assert!(
            registry.get_pack_definition("task_state").is_some(),
            "the build must leave deferred definitions for in-place late binding"
        );
        let withheld = registry.withhold_unbound_compiled_packs();
        assert!(
            withheld.iter().any(|name| name == "read_file"),
            "read_file has no provider without handlers and must be withheld, got {withheld:?}"
        );
        let unserviceable: Vec<String> = registry
            .all_pack_definitions()
            .into_iter()
            .filter(|pack| {
                matches!(
                    &pack.implementation,
                    super::super::capability::ImplementationType::Compiled { .. }
                )
            })
            .filter(|pack| !registry.has(&pack.name))
            .map(|pack| pack.name)
            .collect();
        assert!(
            unserviceable.is_empty(),
            "compiled packs still published without a provider after withholding: {unserviceable:?}"
        );
    }

    #[test]
    fn registry_has_compiled_providers() {
        let registry = match try_build_test_registry() {
            Some(r) => r,
            None => {
                eprintln!("Skipping: MagicutorClient unavailable");
                return;
            },
        };

        assert!(!registry.has("browser"));
        assert!(registry.has("files"));
        assert!(registry.has("http"));
        // Local Messages access is absent unless an iMessage pack is loaded.
        assert!(!registry.has("imessage"));
        assert!(registry.has("shell"));
        assert!(registry.has("search"));
        // episode_recall is no longer registered — reads happen via prompt injection
        assert!(!registry.has("episode_recall"));

        // http_* prefix fallback
        assert!(registry.has("http_get"));
        assert!(registry.has("http_post"));
    }

    /// A name the registry routes to the HTTP adapter but the sink walker does
    /// not know would refuse a credential-bearing request instead of lowering
    /// it; a name in the sink set the catalog never generates would be the
    /// reverse mistake. The two lists are written apart, so they are compared.
    #[test]
    fn http_pack_aliases_are_all_known_credential_sinks() {
        use crate::magician_v2::secrets::sinks::{is_http_pack, HTTP_PACK_CAPABILITIES};
        for (alias, _) in HTTP_METHOD_ALIASES {
            assert!(
                is_http_pack(alias),
                "{alias} is generated but not a known HTTP sink"
            );
        }
        assert!(is_http_pack("http"));
        for known in HTTP_PACK_CAPABILITIES {
            assert!(
                known == "http" || HTTP_METHOD_ALIASES.iter().any(|(alias, _)| *alias == known),
                "{known} is a known HTTP sink the catalog never generates"
            );
        }
        // A capability that merely starts with the prefix is not the adapter.
        assert!(!is_http_pack("http_scrape"));
    }

    #[test]
    fn provider_tool_names_match() {
        let registry = match try_build_test_registry() {
            Some(r) => r,
            None => {
                eprintln!("Skipping: MagicutorClient unavailable");
                return;
            },
        };

        assert!(registry.get("browser").is_none());
        assert_eq!(registry.get("files").unwrap().tool_name(), "files");
        assert_eq!(registry.get("http").unwrap().tool_name(), "http");
        assert!(registry.get("imessage").is_none());
        assert_eq!(registry.get("shell").unwrap().tool_name(), "shell");
        assert_eq!(registry.get("search").unwrap().tool_name(), "search");
        assert!(registry.get("episode_recall").is_none());
    }

    #[test]
    fn app_tool_attestation_is_operation_specific_and_effect_free() {
        let files = FileCapabilityProvider::new(FileSandboxConfig::default());
        let tool_ref =
            crate::magician_v2::apps::models::AppReference::parse("capability:files").unwrap();
        for action in [
            "read", "exists", "list", "write", "append", "delete", "copy", "move", "mkdir",
        ] {
            let parameters = HashMap::from([(
                "action".to_owned(),
                serde_json::Value::String(action.to_owned()),
            )]);
            assert!(files
                .attest_app_tool_target(tool_ref.clone(), &parameters)
                .is_none());
        }

        let duckdb = DuckDbCapabilityProvider::new().unwrap();
        let duckdb_ref =
            crate::magician_v2::apps::models::AppReference::parse("capability:duckdb").unwrap();
        for parameters in [
            serde_json::json!({"sql":"SELECT 1"}),
            serde_json::json!({"sql":"CREATE TABLE secret AS SELECT 1"}),
            serde_json::json!({"sql":"COPY (SELECT 1) TO 'out.csv'"}),
            serde_json::json!({"attach_path":"state.duckdb","alias":"state"}),
        ] {
            let parameters = serde_json::from_value::<HashMap<String, Value>>(parameters).unwrap();
            assert!(duckdb
                .attest_app_tool_target(duckdb_ref.clone(), &parameters)
                .is_none());
        }

        let time_math = TimeMathCapabilityProvider::new();
        let time_math_ref =
            crate::magician_v2::apps::models::AppReference::parse("capability:time_math").unwrap();
        let date_range = serde_json::from_value::<HashMap<String, Value>>(serde_json::json!({
            "operation":"date_range",
            "start_date":"2026-01-01",
            "end_date":"2026-01-02",
            "timezone":"UTC"
        }))
        .unwrap();
        let target = time_math
            .attest_app_tool_target(time_math_ref.clone(), &date_range)
            .expect("time_math is deterministic and has no external effects");
        assert_eq!(target.tool_ref(), &time_math_ref);
        assert_eq!(
            target.runtime_ref().map(ToString::to_string).as_deref(),
            Some("runtime:compiled:time_math:v1")
        );
        assert!(target.endpoint().is_none());
        assert_eq!(
            target.result_policy(),
            crate::magician_v2::apps::tool_disclosure::AppToolResultPolicy::PureTransformInheritsInput
        );
        let now_parameters = serde_json::from_value::<HashMap<String, Value>>(
            serde_json::json!({"operation":"now","timezone":"UTC"}),
        )
        .unwrap();
        let now_target = time_math
            .attest_app_tool_target(time_math_ref.clone(), &now_parameters)
            .expect("time_math now is a trusted local clock read");
        assert_eq!(
            now_target.runtime_ref().map(ToString::to_string).as_deref(),
            Some("runtime:compiled:time_math:now:v1")
        );
        assert!(now_target.endpoint().is_none());
        assert_eq!(
            now_target.result_policy(),
            crate::magician_v2::apps::tool_disclosure::AppToolResultPolicy::TrustedLocalClock
        );
        assert!(time_math
            .attest_app_tool_target(
                time_math_ref.clone(),
                &serde_json::from_value::<HashMap<String, Value>>(serde_json::json!({
                    "operation":"now",
                    "timezone":"Not/AZone"
                }))
                .unwrap(),
            )
            .is_none());
        assert!(time_math
            .attest_app_tool_target(
                time_math_ref,
                &serde_json::from_value::<HashMap<String, Value>>(
                    serde_json::json!({"operation":"explode"})
                )
                .unwrap(),
            )
            .is_none());

        let http = HttpCapabilityProvider::new();
        assert!(http
            .attest_app_tool_target(
                crate::magician_v2::apps::models::AppReference::parse("capability:http").unwrap(),
                &HashMap::new(),
            )
            .is_none());
        use crate::magician_v2::apps::app_tool_bind::{
            app_effect_owner_supported, app_tool_is_dispatchable, plan_app_tool_call,
            AppToolContainProfile,
        };
        assert!(app_effect_owner_supported(&plan_app_tool_call(
            "time_math",
            Some("date_range"),
            AppToolContainProfile::InProcessCompiled,
        )));
        assert!(app_tool_is_dispatchable(
            "files",
            AppToolContainProfile::InProcessCompiled,
        ));
        assert!(!app_tool_is_dispatchable(
            "duckdb",
            AppToolContainProfile::InProcessCompiled,
        ));
        assert!(app_tool_is_dispatchable(
            "http",
            AppToolContainProfile::InProcessCompiled,
        ));
        assert!(app_effect_owner_supported(&plan_app_tool_call(
            "time_math",
            Some("date_range"),
            AppToolContainProfile::OsJail,
        )));
    }

    #[test]
    fn app_file_and_table_argument_proofs_are_exact_and_reject_ambient_authority() {
        let file_read = serde_json::from_value::<HashMap<String, Value>>(serde_json::json!({
            "action": "read",
            "path": "input/note.txt",
            "encoding": "utf-8"
        }))
        .unwrap();
        assert!(prove_bound_file_args(&file_read));
        let file_escape = serde_json::from_value::<HashMap<String, Value>>(serde_json::json!({
            "action": "read",
            "path": "../secret.txt"
        }))
        .unwrap();
        assert!(!prove_bound_file_args(&file_escape));
        let file_delete = serde_json::from_value::<HashMap<String, Value>>(serde_json::json!({
            "action": "delete",
            "path": "note.txt"
        }))
        .unwrap();
        assert!(!prove_bound_file_args(&file_delete));

        let table = serde_json::from_value::<HashMap<String, Value>>(serde_json::json!({
            "__action_name": "preview",
            "source": "tables/input.parquet",
            "limit": 10,
            "output_format": "json"
        }))
        .unwrap();
        assert!(prove_bound_table_args(&table));
        for hostile in [
            serde_json::json!({
                "__action_name": "query",
                "source": "tables/input.parquet",
                "sql": "select * from read_csv('/etc/passwd')"
            }),
            serde_json::json!({
                "__action_name": "preview",
                "source": "tables/input.parquet",
                "database": "state.duckdb"
            }),
            serde_json::json!({
                "__action_name": "preview",
                "source": "tables/*.parquet"
            }),
        ] {
            assert!(!prove_bound_table_args(
                &serde_json::from_value::<HashMap<String, Value>>(hostile).unwrap()
            ));
        }
    }

    #[test]
    fn compiled_providers_do_not_require_browser_session_by_default() {
        let registry = match try_build_test_registry() {
            Some(r) => r,
            None => {
                eprintln!("Skipping: MagicutorClient unavailable");
                return;
            },
        };

        assert!(registry.get("browser").is_none());
        assert!(!registry.get("files").unwrap().requires_browser_session());
        assert!(!registry.get("http").unwrap().requires_browser_session());
        assert!(!registry.get("shell").unwrap().requires_browser_session());
        assert!(!registry.get("search").unwrap().requires_browser_session());
    }

    #[test]
    fn vector_provider_registered() {
        let registry = match try_build_test_registry() {
            Some(r) => r,
            None => {
                eprintln!("Skipping: MagicutorClient unavailable");
                return;
            },
        };
        assert!(registry.has("vector"));
        assert_eq!(registry.get("vector").unwrap().tool_name(), "vector");
        assert!(!registry.get("vector").unwrap().requires_browser_session());
    }

    #[test]
    fn vector_parse_search_mode_accepts_aliases() {
        assert!(matches!(
            parse_search_mode(Some("hybrid")).unwrap(),
            magician_vector_index::SearchMode::Hybrid
        ));
        assert!(matches!(
            parse_search_mode(Some("FTS")).unwrap(),
            magician_vector_index::SearchMode::Fts
        ));
        assert!(matches!(
            parse_search_mode(Some("keyword")).unwrap(),
            magician_vector_index::SearchMode::Fts
        ));
        assert!(matches!(
            parse_search_mode(Some("semantic")).unwrap(),
            magician_vector_index::SearchMode::Vector
        ));
        // Default is hybrid.
        assert!(matches!(
            parse_search_mode(None).unwrap(),
            magician_vector_index::SearchMode::Hybrid
        ));
    }

    #[test]
    fn vector_parse_search_mode_rejects_unknown() {
        assert!(parse_search_mode(Some("bogus")).is_err());
    }

    #[test]
    fn vector_parse_search_mode_empty_string_defaults_to_hybrid() {
        // Some LLMs emit `mode: ""` when they want the default. We should
        // accept that rather than emitting an unhelpful "unknown mode" error.
        assert!(matches!(
            parse_search_mode(Some("")).unwrap(),
            magician_vector_index::SearchMode::Hybrid
        ));
        assert!(matches!(
            parse_search_mode(Some("   ")).unwrap(),
            magician_vector_index::SearchMode::Hybrid
        ));
    }

    #[test]
    fn vector_parse_rank_output_empty_string_defaults_to_ranked() {
        assert!(matches!(
            parse_rank_output(Some("")).unwrap(),
            magician_vector_index::RankOutput::Ranked
        ));
        assert!(matches!(
            parse_rank_output(Some(" ")).unwrap(),
            magician_vector_index::RankOutput::Ranked
        ));
    }

    #[test]
    fn vector_parse_rank_output_accepts_aliases() {
        assert!(matches!(
            parse_rank_output(Some("ranked")).unwrap(),
            magician_vector_index::RankOutput::Ranked
        ));
        assert!(matches!(
            parse_rank_output(Some("rerank")).unwrap(),
            magician_vector_index::RankOutput::Ranked
        ));
        assert!(matches!(
            parse_rank_output(Some("cluster")).unwrap(),
            magician_vector_index::RankOutput::Clusters
        ));
        assert!(matches!(
            parse_rank_output(Some("dedupe")).unwrap(),
            magician_vector_index::RankOutput::Deduped
        ));
        // Default is ranked.
        assert!(matches!(
            parse_rank_output(None).unwrap(),
            magician_vector_index::RankOutput::Ranked
        ));
    }

    #[test]
    fn vector_parse_rank_output_rejects_unknown() {
        assert!(parse_rank_output(Some("bogus")).is_err());
    }

    #[test]
    fn vector_is_safe_namespace_accepts_alphanumeric_and_dashes() {
        assert!(is_safe_namespace("default"));
        assert!(is_safe_namespace("task_abc123"));
        assert!(is_safe_namespace("catchup-snapshots"));
        assert!(is_safe_namespace("ns.with.dots"));
        assert!(is_safe_namespace("urn:vector:foo"));
    }

    #[test]
    fn vector_is_safe_namespace_rejects_unsafe_chars() {
        assert!(!is_safe_namespace(""));
        assert!(!is_safe_namespace("../escape"));
        assert!(!is_safe_namespace("name with space"));
        assert!(!is_safe_namespace("name/with/slash"));
    }

    #[test]
    fn vector_is_safe_namespace_rejects_path_traversal() {
        // SECURITY: `..` resolves to parent dir via PathBuf::join. Letting any
        // namespace contain `..` would let an agent (or prompt-injection
        // attacker) escape vector_root and nuke arbitrary scope dirs via the
        // index path's remove_dir_all step.
        assert!(!is_safe_namespace(".."));
        assert!(!is_safe_namespace("."));
        assert!(!is_safe_namespace("..hidden"));
        assert!(!is_safe_namespace(".hidden"));
        assert!(!is_safe_namespace("foo..bar"));
        assert!(!is_safe_namespace("valid_..escape"));
    }

    #[test]
    fn catchup_merge_registered() {
        let registry = match try_build_test_registry() {
            Some(r) => r,
            None => {
                eprintln!("Skipping: MagicutorClient unavailable");
                return;
            },
        };
        assert!(registry.has("catchup_merge"));
        assert_eq!(
            registry.get("catchup_merge").unwrap().tool_name(),
            "catchup_merge"
        );
    }

    #[tokio::test]
    async fn catchup_merge_dedups_cross_source_by_normalized_url() {
        let mut params = HashMap::new();
        params.insert("query".to_string(), serde_json::json!("test merge"));
        params.insert(
            "envelopes".to_string(),
            serde_json::json!([
                {"source": "reddit", "items": [
                    {"title": "post A", "url": "https://Reddit.com/r/X/?utm_source=foo",
                     "engagement": {"upvotes": 50}}
                ]},
                {"source": "hackernews", "items": [
                    {"title": "post A about same article", "url": "https://reddit.com/r/X",
                     "engagement": {"points": 12}}
                ]}
            ]),
        );
        let result = execute_catchup_merge(&params).await.unwrap();
        let text = match result {
            ActionResult::Text { content } => content,
            other => panic!("expected text, got {other:?}"),
        };
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let items = parsed.get("items").unwrap().as_array().unwrap();
        assert_eq!(
            items.len(),
            1,
            "two sources of same URL should dedup to one item"
        );
        let sources = items[0].get("_sources").unwrap().as_array().unwrap();
        assert_eq!(
            sources.len(),
            2,
            "both sources should be merged into _sources"
        );
        assert_eq!(
            parsed
                .get("dedup_summary")
                .unwrap()
                .get("deduped_count")
                .unwrap()
                .as_u64()
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn catchup_merge_rrf_prefers_consensus_items() {
        let mut params = HashMap::new();
        params.insert("query".to_string(), serde_json::json!("rrf test"));
        params.insert(
            "envelopes".to_string(),
            serde_json::json!([
                {"source": "a", "items": [
                    {"title": "consensus", "url": "https://example.com/consensus"},
                    {"title": "only-a", "url": "https://example.com/only-a"}
                ]},
                {"source": "b", "items": [
                    {"title": "consensus", "url": "https://example.com/consensus"},
                    {"title": "only-b", "url": "https://example.com/only-b"}
                ]}
            ]),
        );
        params.insert("cluster".to_string(), serde_json::json!(false));
        let result = execute_catchup_merge(&params).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&match result {
            ActionResult::Text { content } => content,
            other => panic!("got {other:?}"),
        })
        .unwrap();
        let items = parsed.get("items").unwrap().as_array().unwrap();
        // Consensus item appears in both → highest RRF sum.
        let first_url = items[0].get("url").unwrap().as_str().unwrap();
        assert_eq!(first_url, "https://example.com/consensus");
    }

    #[tokio::test]
    async fn catchup_merge_limit_zero_returns_no_items() {
        // Honor explicit limit=0 — caller may want only metadata
        // (dedup_summary, source_summary, duration_ms) for diagnostics.
        let mut params = HashMap::new();
        params.insert("query".to_string(), serde_json::json!("limit zero"));
        params.insert(
            "envelopes".to_string(),
            serde_json::json!([
                {"source": "reddit", "items": [
                    {"title": "x", "url": "https://r.com/x"},
                    {"title": "y", "url": "https://r.com/y"}
                ]}
            ]),
        );
        params.insert("limit".to_string(), serde_json::json!(0));
        let result = execute_catchup_merge(&params).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&match result {
            ActionResult::Text { content } => content,
            _ => panic!(),
        })
        .unwrap();
        assert_eq!(
            parsed.get("items").unwrap().as_array().unwrap().len(),
            0,
            "limit=0 must return no items"
        );
        // But dedup_summary still reflects the actual pool, not 0.
        let dedup = parsed.get("dedup_summary").unwrap();
        assert_eq!(dedup.get("total_input_items").unwrap().as_u64(), Some(2));
    }

    #[tokio::test]
    async fn catchup_merge_returns_per_source_summary() {
        let mut params = HashMap::new();
        params.insert("query".to_string(), serde_json::json!("summary test"));
        params.insert(
            "envelopes".to_string(),
            serde_json::json!([
                {"source": "reddit", "items": [
                    {"title": "p1", "url": "https://r.com/1"},
                    {"title": "p2", "url": "https://r.com/2"}
                ]},
                {"source": "hn", "items": [
                    {"title": "p3", "url": "https://news.ycombinator.com/p/3"}
                ]}
            ]),
        );
        let result = execute_catchup_merge(&params).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&match result {
            ActionResult::Text { content } => content,
            other => panic!("got {other:?}"),
        })
        .unwrap();
        let summary = parsed.get("source_summary").unwrap().as_array().unwrap();
        assert_eq!(summary.len(), 2);
        let reddit = &summary[0];
        assert_eq!(reddit.get("source").unwrap().as_str(), Some("reddit"));
        assert_eq!(reddit.get("input_count").unwrap().as_u64(), Some(2));
    }

    #[test]
    fn catchup_merge_engagement_zero_when_no_signals() {
        let mut item = serde_json::Map::new();
        item.insert("engagement".to_string(), serde_json::json!({}));
        assert_eq!(engagement_normalized(&item), 0.0);
    }

    #[test]
    fn catchup_merge_engagement_in_unit_range() {
        let mut item = serde_json::Map::new();
        item.insert(
            "engagement".to_string(),
            serde_json::json!({"upvotes": 100, "comments": 20}),
        );
        let n = engagement_normalized(&item);
        assert!(n > 0.0 && n <= 1.0);
    }

    #[test]
    fn catchup_merge_freshness_decays_over_30_days() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-16T00:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut fresh = serde_json::Map::new();
        fresh.insert(
            "published_at".to_string(),
            serde_json::json!("2026-05-15T00:00:00+00:00"),
        );
        let mut stale = serde_json::Map::new();
        stale.insert(
            "published_at".to_string(),
            serde_json::json!("2026-04-01T00:00:00+00:00"),
        );
        assert!(freshness_normalized(&fresh, now) > freshness_normalized(&stale, now));
        assert_eq!(freshness_normalized(&stale, now), 0.0);
    }

    #[test]
    fn catchup_merge_freshness_missing_published_is_neutral() {
        let now = chrono::Utc::now();
        let item = serde_json::Map::new();
        assert_eq!(freshness_normalized(&item, now), 0.5);
    }

    #[test]
    fn catchup_merge_jaccard_clusters_high_overlap_titles() {
        // Jaccard at 0.5 needs substantial token overlap — by design it
        // clusters near-identical headlines (e.g. wire-service rewrites) but
        // misses paraphrases that share only the entity. Semantic dedupe
        // via `vector.rank(output=deduped)` is the right tool for the
        // paraphrase case.
        let a = catchup_tokens("OpenAI Sora model launches public preview");
        let b = catchup_tokens("OpenAI Sora model public preview now live");
        let j = jaccard(&a, &b);
        assert!(j >= CLUSTER_JACCARD_THRESHOLD, "got jaccard={j}");
    }

    #[tokio::test]
    async fn catchup_merge_deterministic_ordering_for_tied_scores() {
        // Same items in different envelope orders should produce the same
        // output ordering for tied scores. HashMap iteration order would
        // randomize this; BTreeMap keyed by url_key keeps it deterministic.
        let make_params = |envelopes: serde_json::Value| {
            let mut p = HashMap::new();
            p.insert("query".to_string(), serde_json::json!("determinism"));
            p.insert("envelopes".to_string(), envelopes);
            p.insert("cluster".to_string(), serde_json::json!(false));
            p
        };
        // Three items, three sources — none overlap so all final_scores tie
        // on RRF (each has rank 1 in its sole source) and 0.5 freshness
        // (no published_at) and 0 engagement → all share final_score.
        let envelopes_a = serde_json::json!([
            {"source": "a", "items": [{"title": "alpha", "url": "https://x.com/alpha"}]},
            {"source": "b", "items": [{"title": "bravo", "url": "https://x.com/bravo"}]},
            {"source": "c", "items": [{"title": "charlie", "url": "https://x.com/charlie"}]}
        ]);
        let envelopes_b = serde_json::json!([
            {"source": "c", "items": [{"title": "charlie", "url": "https://x.com/charlie"}]},
            {"source": "a", "items": [{"title": "alpha", "url": "https://x.com/alpha"}]},
            {"source": "b", "items": [{"title": "bravo", "url": "https://x.com/bravo"}]}
        ]);
        let extract_urls = |result: ActionResult| -> Vec<String> {
            let text = match result {
                ActionResult::Text { content } => content,
                _ => panic!(),
            };
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
            parsed
                .get("items")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|it| it.get("url").unwrap().as_str().unwrap().to_string())
                .collect()
        };
        // Run several times to flush out non-determinism if any HashMap
        // randomization slipped through.
        let baseline = extract_urls(
            execute_catchup_merge(&make_params(envelopes_a.clone()))
                .await
                .unwrap(),
        );
        for _ in 0..5 {
            let run_a = extract_urls(
                execute_catchup_merge(&make_params(envelopes_a.clone()))
                    .await
                    .unwrap(),
            );
            let run_b = extract_urls(
                execute_catchup_merge(&make_params(envelopes_b.clone()))
                    .await
                    .unwrap(),
            );
            assert_eq!(run_a, baseline);
            assert_eq!(run_b, baseline);
        }
    }

    #[test]
    fn catchup_merge_engagement_merge_preserves_existing_when_incoming_null() {
        // Regression for the silent-data-loss bug: an incoming null was
        // overwriting a prior valid number from the first envelope.
        let mut existing = PoolEntry {
            item: serde_json::Map::new(),
            rrf_total: 0.0,
            sources: vec!["a".into()],
            source_native_ranks: HashMap::new(),
            url_key: "x".into(),
        };
        existing
            .item
            .insert("engagement".to_string(), serde_json::json!({"upvotes": 42}));
        let mut incoming = serde_json::Map::new();
        incoming.insert(
            "engagement".to_string(),
            serde_json::json!({"upvotes": null}),
        );
        merge_item_into_existing(&mut existing, incoming);
        let upvotes = existing
            .item
            .get("engagement")
            .and_then(|e| e.get("upvotes"))
            .and_then(|n| n.as_u64())
            .unwrap();
        assert_eq!(
            upvotes, 42,
            "incoming null must NOT wipe a valid prior number"
        );
    }

    #[tokio::test]
    async fn vector_rank_rejects_out_of_range_threshold() {
        if crate::magician_v2::runtime::ollama_lifecycle::is_available() {
            eprintln!("Skipping: Ollama is available; threshold validation runs before embedder call but test is clearer when unavail");
            // Test still runs correctly either way — the validation is purely
            // arithmetic, not embedder-dependent.
        }
        let mut params = HashMap::new();
        params.insert("action".to_string(), serde_json::json!("rank"));
        params.insert(
            "items".to_string(),
            serde_json::json!([{"id": "x", "text": "hello"}]),
        );
        params.insert("output".to_string(), serde_json::json!("clusters"));
        params.insert("threshold".to_string(), serde_json::json!(1.5));
        let tmp = std::env::temp_dir().join("vector_threshold_test");
        let result = execute_vector_action(&params, &tmp).await;
        // Either ollama_unavailable (lifecycle not started) returns an Ok
        // payload, OR the threshold validation fires first. In both cases
        // the path is well-defined; if ollama is available, threshold
        // validation MUST reject this.
        if crate::magician_v2::runtime::ollama_lifecycle::is_available() {
            assert!(
                result.is_err(),
                "expected threshold rejection, got {result:?}"
            );
            assert!(format!("{result:?}").contains("out of range"));
        }
    }

    #[test]
    fn catchup_merge_jaccard_low_for_paraphrases_documents_limitation() {
        let a = catchup_tokens("OpenAI Sora 2 launches today");
        let b = catchup_tokens("Sora 2 hits public beta with new features");
        let j = jaccard(&a, &b);
        assert!(
            j < CLUSTER_JACCARD_THRESHOLD,
            "Jaccard 0.5 on title tokens is intentionally permissive but should miss \
             this paraphrase pair; use vector.rank(output=deduped) for semantic catch. \
             Got jaccard={j}"
        );
    }

    #[tokio::test]
    async fn vector_execute_returns_ollama_unavailable_when_lifecycle_not_started() {
        // The lifecycle module's STATE OnceLock is process-global. Other
        // tests in the same binary may have initialized it; this test only
        // validates the helper returns a well-shaped error envelope when
        // is_available() is false.
        if crate::magician_v2::runtime::ollama_lifecycle::is_available() {
            eprintln!("Skipping: Ollama lifecycle already available in this test process");
            return;
        }
        let mut params = std::collections::HashMap::new();
        params.insert("action".to_string(), serde_json::json!("index"));
        params.insert(
            "items".to_string(),
            serde_json::json!([{"id": "x", "text": "hello"}]),
        );
        let tmp = std::env::temp_dir().join("vector_unavail_test");
        let result = execute_vector_action(&params, &tmp).await.unwrap();
        let text = match result {
            ActionResult::Text { content } => content,
            other => panic!("expected text result, got {other:?}"),
        };
        assert!(text.contains("ollama_unavailable"), "got: {text}");
    }

    #[test]
    fn imessage_sql_normalization_accepts_legacy_sqlite_scan() {
        let sql = "SELECT * FROM sqlite_scan('~/Library/Messages/chat.db', 'message') m JOIN sqlite_scan(\"/tmp/chat.db\", \"handle\") h ON h.ROWID = m.handle_id";

        assert_eq!(
            normalize_imessage_sql_for_sqlite(sql),
            "SELECT * FROM message m JOIN handle h ON h.ROWID = m.handle_id"
        );
    }

    #[test]
    fn imessage_sql_validation_allows_read_only_queries() {
        assert!(validate_imessage_sql("SELECT * FROM message LIMIT 1").is_ok());
        assert!(validate_imessage_sql("-- schema\nPRAGMA table_info(message)").is_ok());
        assert!(validate_imessage_sql(
            "/* inspect */ WITH recent AS (SELECT 1) SELECT * FROM recent"
        )
        .is_ok());
        assert!(validate_imessage_sql("UPDATE message SET text = 'x'").is_err());
    }

    // -- Pruning tests — exercise prune_unknown_compiled_packs directly,
    //    no MagicutorClient required so these always run. --

    fn make_compiled_pack(name: &str, provider_name: &str) -> CapabilityPackDefinition {
        CapabilityPackDefinition {
            name: name.to_string(),
            description: Some(format!("test pack {}", name)),
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: Vec::new(),
            implementation: crate::magician_v2::execution::ImplementationType::Compiled {
                provider_name: provider_name.to_string(),
            },
            execution: None,
            auth: None,
            reliability: None,
            result_projection: None,
        }
    }

    /// Build a leftover map simulating what `build_compiled_registry` would
    /// leave behind after known compiled providers have `.remove()`d their entries.
    fn leftover_map(
        packs: Vec<CapabilityPackDefinition>,
    ) -> HashMap<String, CapabilityPackDefinition> {
        let mut map = HashMap::new();
        for pack in packs {
            if let crate::magician_v2::execution::ImplementationType::Compiled {
                ref provider_name,
            } = pack.implementation
            {
                if !is_known_compiled_provider(provider_name) {
                    map.insert(provider_name.clone(), pack);
                }
            }
        }
        map
    }

    #[test]
    fn unknown_provider_pruned_from_tool_infos() {
        let packs = vec![make_compiled_pack("custom_exec", "nonexistent")];
        let mut infos = pack_defs_to_tool_infos(&packs);

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].0, "custom_exec");

        let leftover = leftover_map(packs);
        prune_unknown_compiled_packs(&leftover, &mut infos);

        let names: Vec<&str> = infos.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(
            !names.contains(&"custom_exec"),
            "unknown pack should be pruned, got: {:?}",
            names
        );
        assert!(infos.is_empty());
    }

    #[test]
    fn unknown_provider_with_name_neq_provider_pruned() {
        // name "my_shell" but provider_name "custom_shell" — differs from key
        let packs = vec![make_compiled_pack("my_shell", "custom_shell")];
        let mut infos = pack_defs_to_tool_infos(&packs);

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].0, "my_shell");

        let leftover = leftover_map(packs);
        prune_unknown_compiled_packs(&leftover, &mut infos);

        let names: Vec<&str> = infos.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(
            !names.contains(&"my_shell"),
            "name!=provider pack should be pruned, got: {:?}",
            names
        );
    }

    #[test]
    fn deferred_task_state_provider_stays_in_tool_infos() {
        let packs = vec![make_compiled_pack("task_state", "task_state")];
        let mut infos = pack_defs_to_tool_infos(&packs);

        let leftover = leftover_map(packs);
        prune_unknown_compiled_packs(&leftover, &mut infos);

        let names: Vec<&str> = infos.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(
            names.contains(&"task_state"),
            "deferred task_state tool should remain in catalog, got: {:?}",
            names
        );
    }

    #[test]
    fn deferred_harness_provider_stays_in_tool_infos() {
        let packs = vec![make_compiled_pack("list_agents", "list_agents")];
        let mut infos = pack_defs_to_tool_infos(&packs);

        let leftover = leftover_map(packs);
        prune_unknown_compiled_packs(&leftover, &mut infos);

        let names: Vec<&str> = infos.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(
            names.contains(&"list_agents"),
            "deferred harness tool should remain in catalog, got: {:?}",
            names
        );
    }

    #[test]
    fn prune_unexecutable_pack_defs_keeps_task_state() {
        let mut packs = vec![make_compiled_pack("task_state", "task_state")];
        prune_unexecutable_pack_defs(&mut packs);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].name, "task_state");
    }

    #[test]
    fn prune_unexecutable_pack_defs_keeps_harness_tools() {
        let mut packs = vec![make_compiled_pack("read_trace", "read_trace")];
        prune_unexecutable_pack_defs(&mut packs);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].name, "read_trace");
    }

    #[test]
    fn prune_unexecutable_pack_defs_keeps_treasurer() {
        let mut packs = vec![make_compiled_pack("treasurer", "treasurer")];
        prune_unexecutable_pack_defs(&mut packs);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].name, "treasurer");
    }

    /// `delete_task` + `get_task_details` were silently pruned at boot before
    /// `COMPILED_PROVIDERS` was consolidated into one source of truth (see the
    /// array's own doc comment) — the YAML and handler registration were both
    /// correct, only this array was missing the entry. Pinned here so
    /// `media_edit`/`media_edit_status` can't regress the same way.
    #[test]
    fn prune_unexecutable_pack_defs_keeps_media_edit() {
        let mut packs = vec![
            make_compiled_pack("media_edit", "media_edit"),
            make_compiled_pack("media_edit_status", "media_edit_status"),
        ];
        prune_unexecutable_pack_defs(&mut packs);
        assert_eq!(packs.len(), 2);
        assert!(packs.iter().any(|p| p.name == "media_edit"));
        assert!(packs.iter().any(|p| p.name == "media_edit_status"));
    }

    #[test]
    fn prune_runtime_disabled_pack_defs_removes_treasurer_when_vault_is_disabled() {
        let mut packs = vec![make_compiled_pack("treasurer", "treasurer")];
        let capabilities =
            SecretRuntimeCapabilities::without_durable_storage("keychain", "disabled for test");

        prune_runtime_disabled_pack_defs(&mut packs, &capabilities);

        assert!(packs.is_empty());
    }

    #[test]
    fn prune_runtime_disabled_pack_defs_keeps_treasurer_when_vault_is_available() {
        let mut packs = vec![make_compiled_pack("treasurer", "treasurer")];
        let capabilities = SecretRuntimeCapabilities::fully_available("keychain");

        prune_runtime_disabled_pack_defs(&mut packs, &capabilities);

        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].name, "treasurer");
    }

    #[test]
    fn unavailable_treasurer_provider_is_pruned_from_tool_infos() {
        let pack = make_compiled_pack("treasurer", "treasurer");
        let mut infos = pack_defs_to_tool_infos(std::slice::from_ref(&pack));
        let mut leftover = HashMap::new();
        leftover.insert("treasurer".to_string(), pack);

        prune_unknown_compiled_packs(&leftover, &mut infos);

        assert!(
            infos.is_empty(),
            "unavailable treasurer pack should be pruned"
        );
    }

    #[test]
    fn unavailable_treasurer_provider_is_removed_from_registry_pack_defs() {
        let magicutor_client = match std::panic::catch_unwind(|| {
            let exec_config = ExecutionConfig::new(url::Url::parse("http://localhost:0").unwrap());
            MagicutorClient::new(exec_config)
        }) {
            Ok(Ok(c)) => Arc::new(c),
            Ok(Err(_)) | Err(_) => {
                eprintln!("Skipping: MagicutorClient unavailable");
                return;
            },
        };

        let (registry, infos) = build_compiled_registry(
            magicutor_client,
            FileSandboxConfig::default(),
            ShellSandboxConfig::default(),
            vec![make_compiled_pack("treasurer", "treasurer")],
            None,
            PathBuf::from("."),
            None,
            None,
            None,
            None, // agent_resources
            None, // compiled_handlers
        );

        assert!(!registry.has("treasurer"));
        assert!(registry.get_pack_definition("treasurer").is_none());
        assert!(infos.is_empty());
    }

    #[test]
    fn unknown_http_provider_prunes_all_aliases() {
        // name "http" but provider_name "custom_http" — auto-generated
        // http_get, http_post, etc. must also be pruned.
        let packs = vec![make_compiled_pack("http", "custom_http")];
        let mut infos = pack_defs_to_tool_infos(&packs);

        // pack_defs_to_tool_infos generates base + 8 aliases = 9 entries
        assert_eq!(infos.len(), 9);

        let leftover = leftover_map(packs);
        prune_unknown_compiled_packs(&leftover, &mut infos);

        let names: Vec<&str> = infos.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(
            !names.contains(&"http"),
            "http base should be pruned, got: {:?}",
            names
        );
        assert!(
            !names.contains(&"http_get"),
            "http_get should be pruned, got: {:?}",
            names
        );
        assert!(
            !names.contains(&"http_post"),
            "http_post should be pruned, got: {:?}",
            names
        );
        assert!(
            !names.contains(&"http_request"),
            "http_request should be pruned, got: {:?}",
            names
        );
        assert!(
            infos.is_empty(),
            "all 9 entries should be pruned, got: {:?}",
            names
        );
    }

    #[test]
    fn strip_leading_heading_removes_title_block() {
        assert_eq!(
            strip_leading_heading("# Awk\n\nTool name: `awk`\nUse for: …\n"),
            "Tool name: `awk`\nUse for: …\n"
        );
        assert_eq!(
            strip_leading_heading("# Awk\nTool name: `awk`\n"),
            "Tool name: `awk`\n"
        );
        assert_eq!(strip_leading_heading("\u{FEFF}# Awk\n\nbody"), "body");
    }

    #[test]
    fn strip_leading_heading_preserves_body_without_heading() {
        assert_eq!(
            strip_leading_heading("Tool name: `awk`\nUse for: …"),
            "Tool name: `awk`\nUse for: …"
        );
        assert_eq!(
            strip_leading_heading("## not the top heading\nbody"),
            "## not the top heading\nbody"
        );
    }

    #[test]
    fn load_pack_defs_from_skills_dir_merges_description_and_guide() {
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_dir = dir.path().join("widget");
        fs::create_dir_all(&skill_dir).unwrap();

        // tool_schema.yaml carries the typed schema only; no
        // description / guide.
        fs::write(
            skill_dir.join("tool_schema.yaml"),
            "name: widget\n\
             parameters:\n\
             - name: input\n  required: true\n  param_type: string\n  description: Input value\n\
             implementation:\n  type: primitive\n  command: [\"widget\"]\n",
        )
        .unwrap();

        // SKILL.md carries description (frontmatter) + guide (body).
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: widget\ndescription: One-liner about widgets.\n---\n\
             # Widget\n\nThis is the widget guide body.\nLine two.\n",
        )
        .unwrap();

        let packs = load_pack_defs_from_skills_dir(dir.path());
        assert_eq!(packs.len(), 1);
        let pack = &packs[0];
        assert_eq!(pack.name, "widget");
        assert_eq!(
            pack.description.as_deref(),
            Some("One-liner about widgets.")
        );
        let guide = pack
            .guide
            .as_deref()
            .expect("guide injected from SKILL.md body");
        assert!(
            guide.contains("This is the widget guide body."),
            "guide should contain body content, got: {guide:?}"
        );
        assert!(
            !guide.contains("# Widget"),
            "leading heading must be stripped, got: {guide:?}"
        );
    }

    #[test]
    fn embedded_compiled_pack_yaml_matches_the_named_definition() {
        let yaml = embedded_compiled_pack_yaml("content_read").expect("content_read yaml");
        let def: CapabilityPackDefinition = serde_yaml::from_str(yaml).expect("parse");
        assert_eq!(def.name, "content_read");
        assert!(embedded_compiled_pack_yaml("not-a-compiled-pack").is_none());
    }

    #[test]
    fn embedded_compiled_pack_defs_parses_all_embedded() {
        let defs = embedded_compiled_pack_defs();
        let names: std::collections::BTreeSet<String> =
            defs.iter().map(|p| p.name.clone()).collect();
        let expected: std::collections::BTreeSet<String> = [
            // Harness state + agent / proposal / program lifecycle.
            "create_agent",
            "create_dashboard",
            "create_proposal",
            "create_task",
            // Recurring Monitors Phase 4 chat tools.
            "preview_monitor",
            "create_monitor",
            "update_monitor",
            "delegation_files",
            "evaluate_harness",
            "inspect_backlog_delivery",
            "inspect_agent",
            "list_agents",
            "list_episodes",
            "list_proposals",
            "magician_work_ledger",
            "notify_owner",
            "propose_program_missions",
            "review_program_missions",
            // Company backlog directive (harness action).
            "promote_backlog_item",
            "propose_backlog_item",
            "review_backlog_delivery",
            // Envoy upward-request tools (Phase B diode).
            "ask_owner",
            "propose_meeting",
            "request_owner_action",
            "read_program_state",
            "read_trace",
            "reassign_task",
            "retire_agent",
            "system_status",
            "task_state",
            "treasurer",
            "unpublish_dashboard",
            "update_agent",
            "update_delegation",
            "update_program_state",
            // Chat-native bridge tools migrated to compiled packs.
            "delete_task",
            "deploy_app",
            "get_active_executions",
            "get_execution_history",
            "get_task_details",
            "list_artifacts",
            "list_memory_tiers",
            "list_scheduled_tasks",
            "replay_recipe",
            "list_tasks",
            "forget_memory",
            "refine_task",
            "run_coding_task",
            "apply_code_proposal",
            "run_project_checks",
            "contribute_to_project",
            "screenshot_preview",
            "capture_reference",
            "run_task",
            "save_preference",
            "search_memory",
            "stop_task",
            "update_memory_tier",
            // Work-evidence graph: distil tier rows into evidence (observe/meeting writers).
            "distill_evidence",
            "update_task",
            // Vision / image analysis.
            "analyze_image_via_openai",
            // Filesystem / search / meta universals (Phase 0.8c-10).
            "tool_search",
            "get_agent_details",
            "find_agents_for_capability",
            "apply_patch",
            "edit_file",
            "glob",
            "grep",
            "content_read",
            "authorize_content_read",
            "content_search",
            "working_set_search",
            "working_set_read",
            "web_fetch",
            "web_search",
            "web_answer",
            "read_file",
            "write_file",
            // Rust-backed compiled providers.
            "files",
            "http",
            "shell",
            "search",
            "time_math",
            "vector",
            "catchup_merge",
            // Meeting participant bot tool.
            "meeting",
            // Skill management + personality (Phase 0.8c).
            "activate_skill",
            "deactivate_skill",
            "switch_personality",
            // Specialized Rust inner-loop dispatchers.
            "duckdb",
            "imessage",
            // Host-relayed macOS automation tools (Tauri host gateway).
            "imessage_send",
            "macos_automation",
            "internal_data",
            // Scoped agent-roster host reads (queue item 6).
            "agent_roster_data",
            // Scoped task-list host reads.
            "tasks_data",
            // Scoped notes search and read.
            "notes_data",
            // Owner-granted app memory reads.
            "memory_data",
            // Scoped claims/evidence/entity/commitment host reads.
            "evidence_data",
            // Scoped meeting-rail host reads (queue item 5).
            "meetings_data",
            // Scoped thinking-map host reads (Phase 4 Brainstorm re-open:
            // the 2.5 learning-read pattern generalized).
            "thinking_maps_data",
            // Notes. Phase 4 shipped publishing, Phase 5 the tools, Phase 6 search.
            "create_note",
            "append_note",
            "publish_task_to_note",
            "open_note",
            "open_pr",
            "save_selection_to_note",
            "search_notes",
            // Android companion: four governed public verbs. The device roster
            // beneath them is authoritative MCP tools/list state.
            "android_snapshot",
            "android_act",
            "android_screenshot",
            "android_app",
            "android_notifications",
            // App-platform Phase 5C: governed app data, result composition and memory proposal.
            "app_action_compose",
            "app_action_invoke",
            "app_data_query",
            "app_data_search",
            "app_data_compose",
            "app_discover",
            "app_memory_propose",
            // ffmpeg-backed media editing: one outer tool + a status poll,
            // internal op registry never exposed as separate top-level tools.
            "media_edit",
            "media_edit_status",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(names, expected);
        assert_eq!(defs.len(), expected.len());
        for pack in &defs {
            assert!(
                pack.description.is_some(),
                "embedded pack '{}' should declare a description",
                pack.name
            );
        }
    }

    #[test]
    fn embedded_compiled_pack_defs_are_process_cached_with_owned_compatibility_views() {
        let first = embedded_compiled_pack_defs_ref();
        let second = embedded_compiled_pack_defs_ref();
        assert_eq!(first.as_ptr(), second.as_ptr());
        assert_eq!(first.len(), second.len());

        let mut owned = embedded_compiled_pack_defs();
        let cached_len = first.len();
        owned.pop();
        assert_eq!(
            first.len(),
            cached_len,
            "owned callers cannot mutate the cache"
        );
        assert_eq!(owned.len() + 1, cached_len);
    }

    #[test]
    fn every_embedded_compiled_pack_has_a_known_provider() {
        // Regression guard: an embedded `type: compiled` pack whose `provider_name`
        // is not in COMPILED_PROVIDERS is pruned from the catalog on EVERY build
        // (a per-scope warning flood) and has no executor ("is not a compiled
        // pack"). Registering the handler + adding the pack-def is NOT enough — the
        // provider must also be allow-listed here (and, for handler-backed tools,
        // wired in build_compiled_registry). This fails the moment they drift.
        use crate::magician_v2::execution::capability::ImplementationType;
        for pack in embedded_compiled_pack_defs() {
            if let ImplementationType::Compiled { provider_name } = &pack.implementation {
                assert!(
                    is_known_compiled_provider(provider_name),
                    "embedded compiled pack '{}' references provider '{}' missing from \
                     COMPILED_PROVIDERS — add it (and wire a provider in \
                     build_compiled_registry), else the catalog prunes it every build \
                     and the tool cannot dispatch",
                    pack.name,
                    provider_name,
                );
            }
        }
    }

    #[test]
    fn social_publishing_is_no_longer_a_compiled_tool() {
        // Retired with the first-party engine (queue item 6, slice 4). Agents
        // reach the square through the package's `ambient_turn` behavior now,
        // and a tool that still wrote the retired store would write a corpus
        // nothing reads. Pinned rather than merely deleted, because a stale
        // grant naming it must not quietly resolve again.
        assert!(!is_known_compiled_provider("publish_social_post"));
        assert!(!is_deferred_compiled_provider("publish_social_post"));
        assert!(default_compiled_handler_registry()
            .get("publish_social_post")
            .is_none());
        assert!(!embedded_compiled_pack_defs()
            .iter()
            .any(|pack| pack.name == "publish_social_post"));
    }

    #[test]
    fn app_data_provider_family_is_complete_and_handler_backed() {
        let handlers = default_compiled_handler_registry();
        let embedded = embedded_compiled_pack_defs()
            .into_iter()
            .map(|pack| pack.name)
            .collect::<std::collections::BTreeSet<_>>();
        for name in APP_DATA_TOOL_NAMES {
            assert!(is_known_compiled_provider(name));
            assert!(
                is_deferred_compiled_provider(name),
                "{name} must survive early registry construction until resources bind"
            );
            assert!(
                handlers.get(name).is_some(),
                "{name} is missing its handler"
            );
            assert!(
                embedded.contains(name),
                "{name} is missing its embedded pack"
            );
        }
    }

    #[test]
    fn media_edit_handlers_are_deferred_and_registered() {
        for name in ["media_edit", "media_edit_status"] {
            assert!(
                is_known_compiled_provider(name),
                "`{name}` must be a known compiled provider"
            );
            assert!(
                is_deferred_compiled_provider(name),
                "`{name}` is handler-backed and must survive early catalog pruning"
            );
            assert!(
                default_compiled_handler_registry().get(name).is_some(),
                "`{name}` must have an executable handler registered"
            );
        }
    }

    #[test]
    fn content_acquisition_and_working_set_handlers_stay_dispatchable() {
        let handlers = default_compiled_handler_registry();
        for name in [
            "content_search",
            "content_read",
            "authorize_content_read",
            "working_set_search",
            "working_set_read",
        ] {
            assert!(
                handlers.get(name).is_some(),
                "content acquisition handler `{name}` must remain dispatchable"
            );
        }
    }

    #[test]
    fn load_pack_defs_from_skills_dir_skips_dirs_without_tool_schema() {
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");

        // Plain steering skill — SKILL.md only, no tool_schema.yaml.
        let plain = dir.path().join("plain-skill");
        fs::create_dir_all(&plain).unwrap();
        fs::write(
            plain.join("SKILL.md"),
            "---\nname: plain-skill\ndescription: a plain steering skill\n---\n# Body\n",
        )
        .unwrap();

        let packs = load_pack_defs_from_skills_dir(dir.path());
        assert!(packs.is_empty(), "plain skills are not capability packs");
    }

    /// What the scoped capability cache validates a rebuild on: every skill
    /// the loader had to drop, with the reason, and a broken embedded
    /// extension counted even though its pack loads — the content-source
    /// registry parses that extension later and rejects unknown fields.
    #[test]
    fn load_skills_dir_reports_every_skill_it_could_not_load() {
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");

        let widget = dir.path().join("widget");
        fs::create_dir_all(&widget).unwrap();
        fs::write(
            widget.join("tool_schema.yaml"),
            "name: widget\nparameters:\n- name: input\n  required: true\n  param_type: string\n  description: Input value\nimplementation:\n  type: primitive\n  command: [\"widget\"]\n",
        )
        .unwrap();
        fs::write(
            widget.join("SKILL.md"),
            "---\nname: widget\ndescription: fine\n---\n# Widget\n",
        )
        .unwrap();

        let broken = dir.path().join("broken");
        fs::create_dir_all(&broken).unwrap();
        fs::write(
            broken.join("tool_schema.yaml"),
            "name: broken\nparameters: [unclosed\n",
        )
        .unwrap();
        fs::write(
            broken.join("SKILL.md"),
            "---\nname: broken\ndescription: bad save\n---\n",
        )
        .unwrap();

        let reader = dir.path().join("reader");
        fs::create_dir_all(&reader).unwrap();
        fs::write(
            reader.join("tool_schema.yaml"),
            "name: reader\nparameters:\n- name: input\n  required: true\n  param_type: string\n  description: Input value\nimplementation:\n  type: primitive\n  command: [\"reader\"]\n",
        )
        .unwrap();
        fs::write(
            reader.join("SKILL.md"),
            "---\nname: reader\ndescription: a reader whose extension this binary does not understand\nmetadata:\n  magician:\n    content_reader:\n      schema_version: 1\n      reader:\n        id: static-http\n        display_name: Static\n        class: web_page\n        a_field_from_the_future: true\n---\n",
        )
        .unwrap();

        let load = load_skills_dir(dir.path());

        assert_eq!(
            load.loaded_skills,
            vec!["reader".to_string(), "widget".to_string()]
        );
        assert_eq!(
            load.packs.len(),
            2,
            "the reader's pack still loads; only its extension is broken"
        );
        let mut failed: Vec<(&str, &str)> = load
            .failures
            .iter()
            .map(|failure| (failure.skill.as_str(), failure.reason.as_str()))
            .collect();
        failed.sort();
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert_eq!(failed[0].0, "broken");
        assert!(failed[0].1.contains("tool_schema.yaml"), "{}", failed[0].1);
        assert_eq!(failed[1].0, "reader");
        assert!(failed[1].1.contains("content_reader"), "{}", failed[1].1);
    }

    #[test]
    fn load_pack_defs_compiles_governed_skill_package_without_legacy_schema() {
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_dir = dir.path().join("fixed-records");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: fixed-records
description: Fixed-profile records.
metadata:
  magician:
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [gws]
      runtime:
        protocol: cli
        command_prefix: []
        limits:
          timeout_secs: 30
      auth:
        kind: cli_profile
        requirement: required
        provider: google-workspace
        profile_selection:
          mode: fixed
          alias: presto
        storage:
          kind: scoped_directory
          namespace: gws
          partition_by_profile: true
        injections:
          - source: {kind: profile_auth_root, path: []}
            target: {kind: environment, name: GOOGLE_WORKSPACE_CLI_CONFIG_DIR}
          - source: {kind: profile_auth_root, path: [cloudsdk]}
            target: {kind: environment, name: CLOUDSDK_CONFIG}
        lifecycle:
          login:
            args: [auth, login]
            interaction: pty
            timeout_secs: 300
      policy_floor:
        approval: conditional_external_side_effect
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        list:
          description: List records.
          fixed_args: [records, list]
          parameters:
            args:
              type: string_array
              description: Exact argv.
              required: true
              max_items: 8
              max_item_bytes: 64
          mappings:
            - type: passthrough
              parameter: args
        auth_login:
          description: Log in to the fixed profile.
          route: auth_login
          fixed_args: [auth, login]
    runtime_catalog:
      categories: [productivity, records]
      composition_category: record_operations
---
# Fixed records

Use the bounded record actions.
"#,
        )
        .unwrap();

        let packs = load_pack_defs_from_skills_dir(dir.path());
        assert_eq!(packs.len(), 1);
        let pack = &packs[0];
        assert_eq!(pack.name, "fixed-records");
        assert_eq!(pack.native_action_schemas.len(), 2);
        assert_eq!(
            pack.native_action_schemas["list"].argv,
            vec!["records", "list"]
        );
        assert_eq!(pack.native_action_schemas["list"].required, vec!["args"]);
        let ImplementationType::Primitive { command, env, .. } = &pack.implementation else {
            panic!("governed CLI package must project to the compatibility dispatcher");
        };
        assert_eq!(command.as_ref().expect("command"), &vec!["gws".to_owned()]);
        assert_eq!(
            env.get("GOOGLE_WORKSPACE_CLI_CONFIG_DIR")
                .map(String::as_str),
            Some("{scope_capability_auth_root}/gws-presto")
        );
        assert_eq!(
            pack.execution
                .as_ref()
                .and_then(|execution| execution.composition_category.as_deref()),
            Some("record_operations")
        );
        assert!(pack.auth.as_ref().is_some_and(|auth| auth.required));
    }

    #[test]
    fn governed_skill_and_legacy_schema_cannot_create_dual_active_routes() {
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_dir = dir.path().join("dual");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: dual\ndescription: dual\nmetadata:\n  magician:\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires:\n        bins: [dual]\n      runtime:\n        protocol: cli\n        command_prefix: []\n    runtime_actions:\n      schema_version: tool-runtime.typed-action-overrides.v1\n      actions:\n        run:\n          description: Run.\n---\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("tool_schema.yaml"),
            "name: dual\nimplementation:\n  type: primitive\n  command: [dual]\n",
        )
        .unwrap();
        assert!(load_pack_defs_from_skills_dir(dir.path()).is_empty());
    }

    #[test]
    fn higgsfield_governed_catalog_is_direct_and_preserves_the_public_contract() {
        use tool_runtime_core::action_overrides::{
            TypedArgumentMapping, TypedConstrainedArgumentPrefix,
        };

        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let governed = load_pack_defs_from_skills_dir(&workspace.join("skillshub"))
            .into_iter()
            .find(|pack| pack.name == "higgsfield")
            .expect("governed Higgsfield pack");
        let deprecated_source = std::fs::read_to_string(
            workspace.join("magician/tests/fixtures/tool_runtime_legacy_contracts/higgsfield.yaml"),
        )
        .expect("Higgsfield compatibility contract");
        let deprecated: CapabilityPackDefinition =
            serde_yaml::from_str(&deprecated_source).expect("deprecated Higgsfield pack");

        let public_parameters = |pack: &CapabilityPackDefinition| {
            pack.parameters
                .iter()
                .map(|parameter| {
                    (
                        parameter.name.clone(),
                        (parameter.required, parameter.default.clone()),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert_eq!(public_parameters(&governed), public_parameters(&deprecated));
        assert_eq!(
            governed.native_action_schemas["run"].timeout_secs,
            deprecated.native_action_schemas["run"].timeout_secs
        );
        assert_eq!(
            governed
                .execution
                .as_ref()
                .and_then(|execution| { execution.composition_category.as_deref() }),
            Some("media_operations")
        );

        let ImplementationType::Primitive {
            runtime_package,
            command,
            cwd,
            ..
        } = &governed.implementation
        else {
            panic!("Higgsfield must remain a direct primitive CLI");
        };
        assert_eq!(
            command.as_deref(),
            Some(["higgsfield".to_owned()].as_slice())
        );
        assert!(cwd.is_none());
        let runtime = runtime_package.as_ref().expect("governed runtime owner");
        let actions = runtime.cli_actions().expect("CLI action catalog");
        assert_eq!(actions.execution.executable, "higgsfield");
        assert!(runtime
            .executable_directory
            .as_ref()
            .is_none_or(|path| { path.ends_with(Path::new("higgsfield").join("bin")) }));
        let run = &actions.actions["run"];
        assert_eq!(
            run.invocation.mappings,
            vec![TypedArgumentMapping::Passthrough {
                parameter: "args".to_owned(),
            }]
        );
        assert_eq!(
            run.invocation.argument_rules.denied_prefixes,
            vec![vec!["auth".to_owned()]]
        );
        assert_eq!(
            run.invocation.argument_rules.constrained_prefixes,
            vec![TypedConstrainedArgumentPrefix {
                prefix: vec!["workspace".to_owned()],
                allowed_next_tokens: std::collections::BTreeSet::from([
                    "list".to_owned(),
                    "status".to_owned(),
                ]),
            }]
        );
    }

    fn assert_governed_catalog_matches_deprecated_invocation_contract(
        skill_name: &str,
        narrowed_help_prefix: &str,
    ) {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let skill_dir = workspace.join("skillshub").join(skill_name);
        let skill_path = skill_dir.join("SKILL.md");
        let skill_source = std::fs::read_to_string(&skill_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", skill_path.display()));
        let manifest =
            crate::magician_v2::skills::loader::parse_manifest(&skill_source, &skill_dir)
                .unwrap_or_else(|error| panic!("parse {}: {error}", skill_path.display()));
        let package = parse_skill_runtime_package(&skill_source)
            .unwrap_or_else(|error| panic!("parse runtime {}: {error}", skill_path.display()))
            .unwrap_or_else(|| panic!("{} has no governed runtime", skill_path.display()));
        project_runtime_package_to_pack(
            &manifest.name,
            &manifest.description,
            None,
            &skill_dir,
            &skill_dir,
            package,
        )
        .unwrap_or_else(|error| panic!("project runtime {}: {error}", skill_path.display()));

        let governed = load_pack_defs_from_skills_dir(&workspace.join("skillshub"))
            .into_iter()
            .find(|pack| pack.name == skill_name)
            .unwrap_or_else(|| panic!("governed {skill_name} pack was omitted by the loader"));
        let compatibility_fixture = workspace
            .join("magician/tests/fixtures/tool_runtime_legacy_contracts")
            .join(format!("{skill_name}.yaml"));
        let deprecated_source =
            std::fs::read_to_string(compatibility_fixture).expect("public compatibility fixture");
        let deprecated: CapabilityPackDefinition =
            serde_yaml::from_str(&deprecated_source).expect("deprecated pack fixture");

        assert_eq!(governed.name, deprecated.name);
        let parameter_contract = |pack: &CapabilityPackDefinition| {
            pack.parameters
                .iter()
                .map(|parameter| {
                    (
                        parameter.name.clone(),
                        (
                            parameter.required,
                            parameter.default.clone(),
                            parameter.param_type.clone(),
                        ),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert_eq!(
            parameter_contract(&governed),
            parameter_contract(&deprecated),
            "top-level parameter contract drifted for {skill_name}"
        );
        assert_eq!(
            governed
                .native_action_schemas
                .keys()
                .collect::<std::collections::BTreeSet<_>>(),
            deprecated
                .native_action_schemas
                .keys()
                .collect::<std::collections::BTreeSet<_>>()
        );
        let governed_default_timeout = governed
            .execution
            .as_ref()
            .and_then(|execution| execution.default_timeout_secs)
            .expect("governed default timeout");
        let deprecated_default_timeout = deprecated
            .execution
            .as_ref()
            .and_then(|execution| execution.default_timeout_secs)
            .expect("deprecated default timeout");
        assert_eq!(
            governed_default_timeout, deprecated_default_timeout,
            "execution default timeout drifted for {skill_name}"
        );
        for (name, old) in &deprecated.native_action_schemas {
            let new = &governed.native_action_schemas[name];
            assert_eq!(
                new.parameters
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                old.parameters
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                "parameter names drifted for {name}"
            );
            assert_eq!(
                new.required
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                old.required
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                "required parameters drifted for {name}"
            );
            if name == "help" {
                assert_eq!(new.argv, vec![narrowed_help_prefix]);
                assert!(old.argv.is_empty());
            } else {
                assert_eq!(new.argv, old.argv, "fixed argv drifted for {name}");
            }
            assert_eq!(
                new.timeout_secs.unwrap_or(governed_default_timeout),
                old.timeout_secs.unwrap_or(deprecated_default_timeout),
                "effective timeout drifted for {name}"
            );
            let normalize = |schema: &NativeActionSchemaDef| {
                if schema.arg_mappings.is_empty()
                    && schema
                        .parameters
                        .iter()
                        .any(|parameter| parameter == "args")
                {
                    vec![CommandArgMapping::Passthrough {
                        param: "args".to_owned(),
                    }]
                } else {
                    schema.arg_mappings.clone()
                }
            };
            assert_eq!(
                normalize(new),
                normalize(old),
                "argv mapping drifted for {name}"
            );
            for parameter in &old.parameters {
                let old_type = old.parameter_overrides[parameter].get("type");
                let new_type = new.parameter_overrides[parameter].get("type");
                assert_eq!(
                    new_type, old_type,
                    "parameter type drifted for {name}.{parameter}"
                );
                let enum_values = |schema: &Value| {
                    schema.get("enum").and_then(Value::as_array).map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<std::collections::BTreeSet<_>>()
                    })
                };
                assert_eq!(
                    enum_values(&new.parameter_overrides[parameter]),
                    enum_values(&old.parameter_overrides[parameter]),
                    "parameter enum drifted for {name}.{parameter}"
                );
                assert_eq!(
                    new.parameter_overrides[parameter]
                        .get("items")
                        .and_then(|items| items.get("type")),
                    old.parameter_overrides[parameter]
                        .get("items")
                        .and_then(|items| items.get("type")),
                    "array item type drifted for {name}.{parameter}"
                );
            }
        }

        let ImplementationType::Primitive {
            runtime_package: governed_runtime,
            command: governed_command,
            env: governed_env,
            timeout_secs: governed_timeout,
            ..
        } = &governed.implementation
        else {
            panic!("governed pack must use the compatibility primitive route");
        };
        let ImplementationType::Primitive {
            runtime_package: deprecated_runtime,
            command: deprecated_command,
            env: deprecated_env,
            timeout_secs: deprecated_timeout,
            ..
        } = &deprecated.implementation
        else {
            panic!("deprecated fixture must be primitive");
        };
        let governed_runtime = governed_runtime
            .as_ref()
            .expect("governed package must own execution");
        let governed_clone = governed.clone();
        let ImplementationType::Primitive {
            runtime_package: cloned_runtime,
            ..
        } = &governed_clone.implementation
        else {
            unreachable!("clone preserves primitive implementation type");
        };
        assert!(Arc::ptr_eq(
            governed_runtime,
            cloned_runtime
                .as_ref()
                .expect("clone preserves governed execution marker")
        ));
        let actions = governed_runtime.cli_actions().expect("CLI action catalog");
        assert_eq!(actions.skill_id, skill_name);
        assert_eq!(actions.actions.len(), governed.native_action_schemas.len());
        assert!(
            deprecated_runtime.is_none(),
            "frozen legacy schema must not gain a governed execution marker"
        );
        assert_eq!(governed_command, deprecated_command);
        assert_eq!(governed_env, deprecated_env);
        assert_eq!(
            governed_timeout, deprecated_timeout,
            "implementation default timeout drifted for {skill_name}"
        );
        let governed_auth = governed.auth.expect("governed compatibility auth");
        let deprecated_auth = deprecated.auth.expect("deprecated auth");
        assert_eq!(governed_auth.check_command, deprecated_auth.check_command);
        assert_eq!(governed_auth.setup_command, deprecated_auth.setup_command);
        assert_eq!(governed_auth.reauth_command, deprecated_auth.reauth_command);
    }

    #[test]
    fn presto_calendar_governed_catalog_matches_deprecated_invocation_contract() {
        assert_governed_catalog_matches_deprecated_invocation_contract(
            "presto-calendar",
            "calendar",
        );
    }

    #[test]
    fn presto_gmail_governed_catalog_matches_deprecated_invocation_contract() {
        assert_governed_catalog_matches_deprecated_invocation_contract("presto-gmail", "gmail");
    }

    #[test]
    fn presto_sheets_governed_catalog_matches_deprecated_invocation_contract() {
        assert_governed_catalog_matches_deprecated_invocation_contract("presto-sheets", "sheets");
    }

    #[test]
    fn calendar_governed_catalog_matches_deprecated_invocation_contract() {
        assert_governed_catalog_matches_deprecated_invocation_contract("calendar", "calendar");
    }

    #[test]
    fn gmail_governed_catalog_matches_deprecated_invocation_contract() {
        assert_governed_catalog_matches_deprecated_invocation_contract("gmail", "gmail");
    }

    #[test]
    fn sheets_governed_catalog_matches_deprecated_invocation_contract() {
        assert_governed_catalog_matches_deprecated_invocation_contract("sheets", "sheets");
    }

    fn assert_governed_static_secret_catalog_matches_deprecated(
        skill_name: &str,
        executable: &str,
        auth_required: bool,
    ) {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let skill_dir = workspace.join("skillshub").join(skill_name);
        let skill_path = skill_dir.join("SKILL.md");
        let skill_source = std::fs::read_to_string(&skill_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", skill_path.display()));
        let manifest =
            crate::magician_v2::skills::loader::parse_manifest(&skill_source, &skill_dir)
                .unwrap_or_else(|error| panic!("parse {}: {error}", skill_path.display()));
        let package = parse_skill_runtime_package(&skill_source)
            .unwrap_or_else(|error| panic!("parse runtime {}: {error}", skill_path.display()))
            .unwrap_or_else(|| panic!("{} has no governed runtime", skill_path.display()));
        project_runtime_package_to_pack(
            &manifest.name,
            &manifest.description,
            None,
            &skill_dir,
            &skill_dir,
            package,
        )
        .unwrap_or_else(|error| panic!("project runtime {}: {error}", skill_path.display()));

        let governed = load_pack_defs_from_skills_dir(&workspace.join("skillshub"))
            .into_iter()
            .find(|pack| pack.name == skill_name)
            .unwrap_or_else(|| panic!("governed {skill_name} pack was omitted by the loader"));
        let compatibility_fixture = workspace
            .join("magician/tests/fixtures/tool_runtime_legacy_contracts")
            .join(format!("{skill_name}.yaml"));
        let deprecated_source = std::fs::read_to_string(compatibility_fixture)
            .unwrap_or_else(|_| panic!("{skill_name} compatibility contract"));
        let deprecated: CapabilityPackDefinition = serde_yaml::from_str(&deprecated_source)
            .unwrap_or_else(|_| panic!("deprecated {skill_name} pack"));

        let new = &governed.native_action_schemas["run"];
        let old = &deprecated.native_action_schemas["run"];
        let provider_parameters = |parameters: &[String]| {
            parameters
                .iter()
                .filter(|parameter| parameter.as_str() != "timeout_secs")
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            provider_parameters(&new.parameters),
            provider_parameters(&old.parameters),
            "provider parameter drifted for {skill_name}"
        );
        assert_eq!(
            new.required
                .iter()
                .collect::<std::collections::BTreeSet<_>>(),
            old.required
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        let normalized_default = |value: Option<&Value>| {
            value.and_then(|value| match value {
                Value::String(value) if value.is_empty() => None,
                Value::String(value) => Some(value.clone()),
                other => Some(other.to_string()),
            })
        };
        for parameter in &old.parameters {
            assert_eq!(
                new.parameter_overrides[parameter].get("type"),
                old.parameter_overrides[parameter].get("type"),
                "parameter type drifted for {parameter}"
            );
            assert_eq!(
                normalized_default(new.parameter_overrides[parameter].get("default")),
                normalized_default(old.parameter_overrides[parameter].get("default")),
                "parameter default drifted for {parameter}"
            );
            let enum_values = |schema: &Value| {
                schema.get("enum").and_then(Value::as_array).map(|values| {
                    values
                        .iter()
                        .map(|value| match value {
                            Value::String(value) => value.clone(),
                            other => other.to_string(),
                        })
                        .collect::<std::collections::BTreeSet<_>>()
                })
            };
            assert_eq!(
                enum_values(&new.parameter_overrides[parameter]),
                enum_values(&old.parameter_overrides[parameter]),
                "parameter enum drifted for {parameter}"
            );
        }
        assert_eq!(new.timeout_secs, old.timeout_secs);

        let ImplementationType::Primitive {
            runtime_package,
            command,
            env,
            ..
        } = &governed.implementation
        else {
            panic!("{skill_name} must remain a primitive pack");
        };
        let runtime = runtime_package.as_ref().expect("governed runtime owner");
        assert_eq!(
            runtime
                .cli_actions()
                .expect("CLI action catalog")
                .execution
                .executable,
            executable
        );
        assert!(runtime
            .executable_directory
            .as_ref()
            .is_some_and(|path| path.ends_with(Path::new(skill_name).join("bin"))));
        assert_eq!(
            command.as_ref().expect("compatibility command"),
            &[executable]
        );
        assert!(
            env.is_empty(),
            "secret values must not enter compatibility env"
        );
        assert_eq!(
            governed.auth.as_ref().map(|auth| auth.required),
            Some(auth_required)
        );
    }

    #[test]
    fn tavily_governed_catalog_preserves_public_action_schema_and_owns_execution() {
        assert_governed_static_secret_catalog_matches_deprecated(
            "news-search-via-tavily",
            "tavily-search",
            true,
        );
    }

    #[test]
    fn exa_governed_catalog_preserves_public_action_schema_and_owns_execution() {
        assert_governed_static_secret_catalog_matches_deprecated(
            "semantic-websearch-via-exa",
            "exa-search",
            true,
        );
    }

    #[test]
    fn openai_websearch_governed_catalog_preserves_public_action_schema_and_owns_execution() {
        assert_governed_static_secret_catalog_matches_deprecated(
            "websearch-via-openai",
            "openai-websearch",
            true,
        );
    }

    #[test]
    fn claude_websearch_governed_catalog_preserves_public_action_schema_and_owns_execution() {
        assert_governed_static_secret_catalog_matches_deprecated(
            "websearch-via-claude",
            "claude-websearch",
            true,
        );
    }

    #[test]
    fn klipy_gif_search_governed_catalog_preserves_public_action_schema_and_owns_execution() {
        assert_governed_static_secret_catalog_matches_deprecated(
            "gif-search-via-klipy",
            "klipy-gif-search",
            true,
        );
    }

    #[test]
    fn imgflip_meme_governed_catalog_preserves_public_action_schema_and_owns_execution() {
        assert_governed_static_secret_catalog_matches_deprecated(
            "meme-generation-via-imgflip",
            "imgflip-meme",
            true,
        );
    }

    #[test]
    fn remaining_static_secret_catalogs_preserve_public_action_schema() {
        for (skill, executable, required) in [
            ("deep-research-with-openai", "openai-deep-research", true),
            ("deep-research-with-claude", "claude-deep-research", true),
            ("github-search", "github-search", false),
            ("image-generation", "nanobanana2", true),
            ("video-generation-via-veo", "veo31", true),
        ] {
            assert_governed_static_secret_catalog_matches_deprecated(skill, executable, required);
        }
    }

    #[test]
    fn metabase_governed_catalog_preserves_all_native_cli_actions() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let governed = load_pack_defs_from_skills_dir(&workspace.join("skillshub"))
            .into_iter()
            .find(|pack| pack.name == "metabase")
            .expect("governed Metabase pack");
        let frozen = std::fs::read_to_string(
            workspace.join("magician/tests/fixtures/tool_runtime_legacy_contracts/metabase.yaml"),
        )
        .expect("Metabase compatibility contract");
        let deprecated: CapabilityPackDefinition =
            serde_yaml::from_str(&frozen).expect("deprecated Metabase pack");

        assert_eq!(governed.native_action_schemas.len(), 101);
        assert_eq!(
            governed.native_action_schemas.len(),
            deprecated.native_action_schemas.len()
        );
        let provider_parameters = |parameters: &[String]| {
            parameters
                .iter()
                .filter(|parameter| parameter.as_str() != "timeout_secs")
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        };
        for (name, old) in &deprecated.native_action_schemas {
            let new = governed
                .native_action_schemas
                .get(name)
                .unwrap_or_else(|| panic!("missing governed Metabase action {name}"));
            assert_eq!(
                provider_parameters(&new.parameters),
                provider_parameters(&old.parameters),
                "parameter drift: {name}"
            );
            assert_eq!(
                new.required
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                old.required
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                "required drift: {name}"
            );
            assert_eq!(new.argv, old.argv, "argv prefix drift: {name}");
            assert_eq!(
                new.suffix_args, old.suffix_args,
                "argv suffix drift: {name}"
            );
            assert_eq!(new.timeout_secs, old.timeout_secs, "timeout drift: {name}");
            for parameter in &old.parameters {
                let old_schema = &old.parameter_overrides[parameter];
                let new_schema = &new.parameter_overrides[parameter];
                assert_eq!(
                    new_schema.get("type"),
                    old_schema.get("type"),
                    "type drift: {name}.{parameter}"
                );
                let normalized = |value: Option<&Value>| {
                    value.map(|value| match value {
                        Value::String(value) => value.clone(),
                        other => other.to_string(),
                    })
                };
                assert_eq!(
                    normalized(new_schema.get("default")),
                    normalized(old_schema.get("default")),
                    "default drift: {name}.{parameter}"
                );
                let enum_set = |schema: &Value| {
                    schema.get("enum").and_then(Value::as_array).map(|values| {
                        values
                            .iter()
                            .map(|value| value.to_string())
                            .collect::<std::collections::BTreeSet<_>>()
                    })
                };
                assert_eq!(
                    enum_set(new_schema),
                    enum_set(old_schema),
                    "enum drift: {name}.{parameter}"
                );
            }
        }

        let ImplementationType::Primitive {
            runtime_package,
            env,
            ..
        } = &governed.implementation
        else {
            panic!("Metabase must remain a direct primitive CLI");
        };
        let runtime = runtime_package.as_ref().expect("governed runtime owner");
        let actions = runtime.cli_actions().expect("CLI action catalog");
        assert_eq!(actions.execution.executable, "metabase-pp-cli");
        assert_eq!(
            actions.execution.command_prefix,
            ["--json", "--no-input", "--no-color", "--yes"]
        );
        assert!(
            env.is_empty(),
            "Metabase secrets must not enter compatibility env"
        );
    }

    #[tokio::test]
    async fn meeting_list_action_executes() {
        let provider = MeetingCapabilityProvider::new();
        let mut params = std::collections::HashMap::new();
        params.insert("action".to_string(), serde_json::json!("list"));
        let result = provider
            .execute_direct(params, None, 30)
            .await
            .expect("list ok");
        // ActionResult text is a JSON array (possibly empty).
        let text = match result {
            ActionResult::Text { content } => content,
            other => panic!("expected text, got {other:?}"),
        };
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert!(parsed.is_array(), "list returns a JSON array, got {parsed}");
    }
}
