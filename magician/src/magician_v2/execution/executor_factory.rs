//! Factory for building `ActionExecutors` independently of the orchestrator.
//!
//! The orchestrator constructs `ActionExecutors` inline (v2_orchestrator.rs).
//! This module extracts that construction into a reusable factory so that
//! the future `AgentRuntime` (Phase 3) can build executors without coupling
//! to the orchestrator's internal state.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use runtime_core::{FileSandboxConfig, ShellSandboxConfig};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::artifact_v2::CapabilityWorkspaceManager;
use crate::magician_v2::artifact_v2::RuntimeCanonicalEventSink;
use crate::magician_v2::execution::agentic::ActionExecutors;
use crate::magician_v2::execution::{
    MagicutorClient, MultiLlmAgentAdapter, ScopedCapabilityResolver, ScreenshotStorage,
};
use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use crate::magician_v2::secrets::SecretStoreResolver;
use crate::magician_v2::user_requests::UserRequestService;

/// All service references needed to construct `ActionExecutors`.
///
/// Callers populate this struct from whatever context they have — the
/// orchestrator fills it from its own fields, a future `AgentRuntime`
/// will fill it from its dependency-injected services.
pub struct ExecutorFactoryConfig {
    /// Magicutor client for browser automation.
    pub magicutor_client: Arc<MagicutorClient>,
    /// Prompt manager for loading prompt templates.
    pub prompt_manager: Arc<PromptManager>,
    /// Screenshot storage for persisting observations.
    pub screenshot_storage: Arc<ScreenshotStorage>,
    /// Shell sandbox policy.
    pub shell_sandbox: ShellSandboxConfig,
    /// File sandbox policy.
    pub file_sandbox: FileSandboxConfig,
    /// Configured engine for headed/headless agent-browser sessions.
    pub browser_engine: Option<String>,
    /// Per-command output bound for typed retrieval browser handoffs.
    pub browser_capture_limit_bytes: Option<usize>,
    /// Configured local Magicutor proxy for typed authenticated handoffs.
    pub browser_cdp_url: String,
    /// Event broadcaster for realtime updates (optional).
    pub event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    /// Canonical V3 runtime event sink (optional).
    pub canonical_event_sink: Option<Arc<dyn RuntimeCanonicalEventSink>>,
    /// Required operation-aware LLM router for native agent decisions via
    /// `MultiLlmAgentAdapter`.
    pub operation_llm_router: Arc<OperationLlmRouter>,
    /// Trace manager for API mining (optional).
    pub trace_manager: Option<Arc<crate::magician_v2::api_mining::trace_manager::TraceManager>>,
    /// API router for API-first routing (optional).
    pub api_router:
        Option<std::sync::Arc<std::sync::Mutex<crate::magician_v2::api_mining::router::ApiRouter>>>,
    /// Base path for API mining data.
    pub api_mining_base_path: PathBuf,
    /// Enable passive XHR/Fetch validation.
    pub enable_xhr_validation: bool,
    /// Capability registry for pluggable tool dispatch (Phase 1).
    pub capability_registry: Arc<super::capability::CapabilityRegistry>,
    /// Shared secret store foundation for runtime secret management.
    pub secret_store: Option<Arc<crate::magician_v2::secrets::SecretStore>>,
    /// Persisted noisy-origin decisions for API mining.
    pub api_mining_origin_policy:
        Option<Arc<crate::magician_v2::api_mining::origin_policy::OriginPolicyStore>>,
    /// Tool authorization policy for unlisted tools ("ask" or "deny").
    /// Sourced from `MagicianConfig.tool_authorization.unlisted_policy`.
    pub tool_authorization_policy: String,
    /// Configured tool allowlist. When non-empty, only these tools are auto-allowed.
    pub config_tool_allowlist: HashSet<String>,
    /// Configured tool blocklist. Always rejected.
    pub config_tool_blocklist: HashSet<String>,
    /// Central user-request service for channel-agnostic executor escalations.
    pub user_request_service: Option<Arc<UserRequestService>>,
    /// Base storage path (e.g., `magician_data_v3` or a temp dir in tests).
    /// Used for taskplan_base_path, downloads, and other executor storage.
    pub storage_base_path: PathBuf,
    /// Authoritative repo root used for shared runtime assets and delegation env injection.
    pub repo_root: PathBuf,
    /// Shared scope-aware secret resolver for scoped treasurer wiring.
    pub secret_store_resolver: Option<Arc<SecretStoreResolver>>,
    /// Shared Chat/voice/task surface runtime and bounded caches.
    pub agent_surface_runtime_config: crate::config::AgentSurfaceRuntimeConfig,
    pub surface_plan_cache: crate::magician_v2::execution::flat_loop::SurfacePlanCache,
    pub surface_working_sets: crate::magician_v2::execution::flat_loop::SurfaceWorkingSetStore,

    // --- Resource Authority (spend-gated execution) ---
    /// Shared resource ledger for double-entry accounting.
    pub resource_ledger: Option<
        Arc<tokio::sync::RwLock<crate::magician_v2::resource_authority::ledger::ResourceLedger>>,
    >,
    /// Shared token store for spend token lifecycle.
    pub token_store: Option<
        Arc<tokio::sync::RwLock<crate::magician_v2::resource_authority::token_store::TokenStore>>,
    >,
    /// System-wide ceilings for per-commodity spend limits.
    pub system_ceilings: Option<
        Arc<tokio::sync::RwLock<Vec<crate::magician_v2::resource_authority::token::SystemCeiling>>>,
    >,
    /// System-wide freeze state for emergency spend halts.
    pub system_freeze: Option<
        Arc<tokio::sync::RwLock<crate::magician_v2::resource_authority::gate::SystemFreezeState>>,
    >,
}

/// Build an `ActionExecutors` from the provided config.
///
/// This reproduces the exact builder chain from v2_orchestrator.rs
/// but takes its dependencies via `ExecutorFactoryConfig` instead of reading
/// orchestrator fields.
pub fn build_action_executors(config: &ExecutorFactoryConfig) -> ActionExecutors {
    let service = &config.operation_llm_router;
    let llm_adapter = Arc::new(MultiLlmAgentAdapter::new(service.clone()));

    let mut executors = ActionExecutors::new_with_native_adapter(
        llm_adapter.clone(),
        config.prompt_manager.clone(),
        Some(service.clone()),
        llm_adapter,
    );

    executors = executors
        .with_browser(config.magicutor_client.clone())
        .with_browser_engine(config.browser_engine.clone())
        .with_browser_capture_limit_bytes(config.browser_capture_limit_bytes)
        .with_browser_cdp_url(config.browser_cdp_url.clone())
        .with_shell_sandbox_policy(config.shell_sandbox.clone())
        .with_file_sandbox_policy(config.file_sandbox.clone())
        .with_screenshot_storage(config.screenshot_storage.clone());

    if let Some(broadcaster) = &config.event_broadcaster {
        executors = executors.with_event_broadcaster(broadcaster.clone());
    }

    if let Some(ref sink) = config.canonical_event_sink {
        executors = executors.with_canonical_event_sink(sink.clone());
    }

    if let Some(ref trace_manager) = config.trace_manager {
        executors = executors.with_trace_manager(trace_manager.clone());
    }

    if let Some(ref api_router) = config.api_router {
        executors = executors.with_api_router(api_router.clone());
    }

    executors = executors.with_api_mining_base_path(config.api_mining_base_path.clone());
    executors = executors.with_xhr_validation(config.enable_xhr_validation);

    executors = executors.with_capability_registry(config.capability_registry.clone());
    executors = executors.with_agent_surface_runtime(
        config.agent_surface_runtime_config.clone(),
        config.surface_plan_cache.clone(),
        config.surface_working_sets.clone(),
    );
    let capability_workspace = Arc::new(CapabilityWorkspaceManager::new(
        ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(
            &config.storage_base_path,
        )),
        &config.repo_root,
    ));
    executors = executors.with_capability_scope_resolver(Arc::new(ScopedCapabilityResolver::new(
        capability_workspace,
        config.magicutor_client.clone(),
        config.file_sandbox.clone(),
        config.shell_sandbox.clone(),
        config.secret_store_resolver.clone(),
        config.resource_ledger.clone(),
        config.token_store.clone(),
    )));

    if let Some(ref secret_store) = config.secret_store {
        executors = executors.with_secret_store(secret_store.clone());
    }
    if let Some(ref resolver) = config.secret_store_resolver {
        executors = executors.with_secret_store_resolver(resolver.clone());
    }

    if let Some(ref origin_policy) = config.api_mining_origin_policy {
        executors = executors.with_api_mining_origin_policy(origin_policy.clone());
    }

    executors.tool_authorization_policy = config.tool_authorization_policy.clone();
    executors.config_tool_allowlist = config.config_tool_allowlist.clone();
    executors.config_tool_blocklist = config.config_tool_blocklist.clone();
    if let Some(ref service) = config.user_request_service {
        executors = executors.with_user_request_service(service.clone());
    }

    // Override storage base path (not the hardcoded magician_data_v3 default)
    executors.taskplan_base_path = config.storage_base_path.clone();

    // Resource Authority fields
    if let Some(ref ledger) = config.resource_ledger {
        executors = executors.with_resource_ledger(ledger.clone());
    }
    if let Some(ref store) = config.token_store {
        executors = executors.with_token_store(store.clone());
    }
    if let Some(ref ceilings) = config.system_ceilings {
        executors = executors.with_system_ceilings(ceilings.clone());
    }
    if let Some(ref freeze) = config.system_freeze {
        executors = executors.with_system_freeze(freeze.clone());
    }

    executors
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::ExecutionConfig;

    /// Verify that `build_action_executors` compiles and returns valid executors
    /// when given minimal operation-router configuration.
    #[test]
    fn build_with_llm_router_produces_valid_executors() {
        // MagicutorClient::new can panic from native TLS (e.g. "NULL object") in
        // environments without proper system security libraries. catch_unwind
        // handles both panics and Err returns gracefully.
        let magicutor_client = match std::panic::catch_unwind(|| {
            let exec_config = ExecutionConfig::new(url::Url::parse("http://localhost:0").unwrap());
            MagicutorClient::new(exec_config)
        }) {
            Ok(Ok(c)) => Arc::new(c),
            Ok(Err(_)) | Err(_) => {
                eprintln!("Skipping: MagicutorClient unavailable in this environment");
                return;
            },
        };
        let prompt_manager = {
            let storage =
                crate::magician_v2::prompts::JsonPromptStorage::with_default_config().unwrap();
            Arc::new(PromptManager::new(Arc::new(storage)))
        };
        let screenshot_storage = Arc::new(ScreenshotStorage::new());

        let (registry, _) = super::super::compiled_providers::build_compiled_registry(
            magicutor_client.clone(),
            FileSandboxConfig::default(),
            ShellSandboxConfig::default(),
            Vec::new(),
            None,
            std::path::PathBuf::from("."),
            None,
            None,
            None,
            None, // agent_resources — test/default factory
            None, // compiled_handlers — test/default factory
        );
        // Nothing binds late in this factory; withhold what has no provider so
        // its catalog never offers a tool this process cannot run.
        let _ = registry.withhold_unbound_compiled_packs();

        let config = ExecutorFactoryConfig {
            magicutor_client,
            prompt_manager,
            screenshot_storage,
            shell_sandbox: ShellSandboxConfig::default(),
            file_sandbox: FileSandboxConfig::default(),
            browser_engine: Some("custom-browser".into()),
            browser_capture_limit_bytes: Some(1024),
            browser_cdp_url: "ws://127.0.0.1:3999/devtools/browser/magicutor-proxy".into(),
            event_broadcaster: None,
            canonical_event_sink: None,
            operation_llm_router: Arc::new(OperationLlmRouter::new(None)),
            trace_manager: None,
            api_router: None,
            api_mining_base_path: PathBuf::from("/tmp/api_mining"),
            enable_xhr_validation: false,
            capability_registry: registry,
            secret_store: None,
            api_mining_origin_policy: None,
            tool_authorization_policy: "ask".to_string(),
            config_tool_allowlist: HashSet::new(),
            config_tool_blocklist: HashSet::new(),
            user_request_service: None,
            storage_base_path: PathBuf::from("/tmp/magician-test-executor"),
            repo_root: PathBuf::from("."),
            secret_store_resolver: None,
            agent_surface_runtime_config: crate::config::AgentSurfaceRuntimeConfig::default(),
            surface_plan_cache: crate::magician_v2::execution::flat_loop::SurfacePlanCache::new(16),
            surface_working_sets:
                crate::magician_v2::execution::flat_loop::SurfaceWorkingSetStore::new(16),
            resource_ledger: None,
            token_store: None,
            system_ceilings: None,
            system_freeze: None,
        };

        let executors = build_action_executors(&config);
        // Browser should be set
        assert!(executors.browser.is_some());
        assert_eq!(executors.browser_engine.as_deref(), Some("custom-browser"));
        assert_eq!(executors.browser_capture_limit_bytes, Some(1024));
        assert_eq!(
            executors.browser_cdp_url,
            "ws://127.0.0.1:3999/devtools/browser/magicutor-proxy"
        );
    }
}
