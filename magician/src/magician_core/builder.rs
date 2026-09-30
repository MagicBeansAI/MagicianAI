use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    error::{MagicianError, Result},
    magician_v2::{
        analytics::operation_llm_telemetry::{
            OperationLlmTelemetryContext, OperationLlmTelemetryScope,
        },
        artifact_v2::CapabilityScopePaths,
        ask_loop::clarifier::{ClarifierError, LlmService},
        execution::{
            CapabilityPackDefinition, ExecutionConfig as MagicutorExecutionConfig, MagicutorClient,
        },
        query_analysis::multi_llm_service::MultiLLMService,
        query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter},
        realtime_events::RuntimeTransportBroadcaster,
        secrets::SecretRuntimeBootstrap,
    },
};
use async_trait::async_trait;
use runtime_core::{
    FileSandboxConfig, RuntimeConfig, SemanticSearch, ShellSandboxConfig, ToolCatalog, ToolMatching,
};
use secrecy::SecretString;
use tracing::{debug, error, info, warn};
use url::Url;

use super::MagicianService;

struct MultiLlmClarifierAdapter {
    multi_llm: Arc<OperationLlmRouter>,
    operation: LLMOperation,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl MultiLlmClarifierAdapter {
    fn new(
        multi_llm: Arc<OperationLlmRouter>,
        operation: LLMOperation,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        Self {
            multi_llm,
            operation,
            broadcaster,
        }
    }
}

#[async_trait]
impl LlmService for MultiLlmClarifierAdapter {
    async fn generate(&self, prompt: &str) -> std::result::Result<String, ClarifierError> {
        self.multi_llm
            .generate_for_operation(&self.operation, prompt)
            .await
            .map(|response| response.content)
            .map_err(|e| ClarifierError::Llm(e.to_string()))
    }

    async fn generate_with_telemetry(
        &self,
        prompt: &str,
        telemetry_scope: Option<&OperationLlmTelemetryScope>,
    ) -> std::result::Result<String, ClarifierError> {
        let started = Instant::now();
        let response = self
            .multi_llm
            .generate_for_operation(&self.operation, prompt)
            .await
            .map_err(|e| ClarifierError::Llm(e.to_string()))?;
        if let Some(scope) = telemetry_scope {
            OperationLlmTelemetryContext::new(
                Arc::clone(&self.broadcaster),
                &scope.principal,
                &scope.workspace,
                "ask_loop_clarifier",
            )
            .emit_success(
                "ask_loop_clarifier",
                &response,
                started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                scope.attribution.clone(),
            );
        }
        Ok(response.content)
    }
}

// Adapter for AnswerInterpretationLLM trait
struct MultiLlmAnswerInterpreterAdapter {
    multi_llm: Arc<OperationLlmRouter>,
    operation: LLMOperation,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl MultiLlmAnswerInterpreterAdapter {
    fn new(
        multi_llm: Arc<OperationLlmRouter>,
        operation: LLMOperation,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        Self {
            multi_llm,
            operation,
            broadcaster,
        }
    }
}

#[async_trait]
impl crate::magician_v2::ask_loop::answer_interpreter::AnswerInterpretationLLM
    for MultiLlmAnswerInterpreterAdapter
{
    async fn interpret(&self, prompt: &str) -> std::result::Result<String, String> {
        self.multi_llm
            .generate_for_operation(&self.operation, prompt)
            .await
            .map(|response| response.content)
            .map_err(|e| e.to_string())
    }

    async fn interpret_with_system(
        &self,
        system_prompt: Option<&str>,
        prompt: &str,
    ) -> std::result::Result<String, String> {
        self.multi_llm
            .generate_for_operation_with_system(&self.operation, system_prompt, prompt)
            .await
            .map(|response| response.content)
            .map_err(|e| e.to_string())
    }

    async fn interpret_with_system_and_telemetry(
        &self,
        system_prompt: Option<&str>,
        prompt: &str,
        telemetry_scope: Option<&OperationLlmTelemetryScope>,
    ) -> std::result::Result<String, String> {
        let started = Instant::now();
        let response = self
            .multi_llm
            .generate_for_operation_with_system(&self.operation, system_prompt, prompt)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(scope) = telemetry_scope {
            OperationLlmTelemetryContext::new(
                Arc::clone(&self.broadcaster),
                &scope.principal,
                &scope.workspace,
                "answer_interpretation",
            )
            .emit_success(
                "answer_interpretation",
                &response,
                started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                scope.attribution.clone(),
            );
        }
        Ok(response.content)
    }
}

struct MagicianRuntimeConfig {
    storage_path: String,
    realtime_events: bool,
    max_conversations: usize,
    conversation_timeout: Duration,
    allow_consent_slots: bool,
    consumer_mode: bool,
    shell_sandbox: ShellSandboxConfig,
    file_sandbox: FileSandboxConfig,
    on_failure_mode: String,
}

impl MagicianRuntimeConfig {
    fn new(
        storage_path: String,
        realtime_events: bool,
        max_conversations: usize,
        conversation_timeout_secs: u64,
        allow_consent_slots: bool,
        consumer_mode: bool,
        shell_sandbox: ShellSandboxConfig,
        file_sandbox: FileSandboxConfig,
        on_failure_mode: String,
    ) -> Self {
        Self {
            storage_path,
            realtime_events,
            max_conversations,
            conversation_timeout: Duration::from_secs(conversation_timeout_secs.max(1)),
            allow_consent_slots,
            consumer_mode,
            shell_sandbox,
            file_sandbox,
            on_failure_mode,
        }
    }
}

impl RuntimeConfig for MagicianRuntimeConfig {
    fn realtime_events_enabled(&self) -> bool {
        self.realtime_events
    }

    fn storage_path(&self) -> &str {
        &self.storage_path
    }

    fn max_conversations(&self) -> usize {
        self.max_conversations
    }

    fn conversation_timeout(&self) -> Duration {
        self.conversation_timeout
    }

    fn allow_consent_slots(&self) -> bool {
        self.allow_consent_slots
    }

    fn consumer_mode_enabled(&self) -> bool {
        self.consumer_mode
    }

    fn shell_sandbox(&self) -> ShellSandboxConfig {
        self.shell_sandbox.clone()
    }

    fn file_sandbox(&self) -> FileSandboxConfig {
        self.file_sandbox.clone()
    }

    fn on_failure_mode(&self) -> &str {
        &self.on_failure_mode
    }
}

/// Builder for the Magician V2 service components.
pub struct MagicianServiceBuilder {
    tool_catalog: Arc<dyn ToolCatalog>,
    tool_matching: Arc<dyn ToolMatching>,
    semantic_search: Arc<dyn SemanticSearch>,
    capability_pack_defs: Vec<CapabilityPackDefinition>,
    secret_runtime_bootstrap: Option<SecretRuntimeBootstrap>,
    boot_config: Option<crate::config::MagicianConfig>,
    repo_root: std::path::PathBuf,
    capability_scope_paths: Option<CapabilityScopePaths>,
}

impl MagicianServiceBuilder {
    pub fn new(
        tool_catalog: Arc<dyn ToolCatalog>,
        tool_matching: Arc<dyn ToolMatching>,
        semantic_search: Arc<dyn SemanticSearch>,
        capability_pack_defs: Vec<CapabilityPackDefinition>,
        repo_root: std::path::PathBuf,
        capability_scope_paths: Option<CapabilityScopePaths>,
    ) -> Self {
        Self {
            tool_catalog,
            tool_matching,
            semantic_search,
            capability_pack_defs,
            secret_runtime_bootstrap: None,
            boot_config: None,
            repo_root,
            capability_scope_paths,
        }
    }

    pub fn with_secret_runtime_bootstrap(
        mut self,
        secret_runtime_bootstrap: SecretRuntimeBootstrap,
    ) -> Self {
        self.secret_runtime_bootstrap = Some(secret_runtime_bootstrap);
        self
    }

    /// Reuse the validated binary startup snapshot without mutating process
    /// environment or repeating config/pricing/authority initialization.
    pub fn with_boot_config(mut self, config: crate::config::MagicianConfig) -> Self {
        self.boot_config = Some(config);
        self
    }

    pub async fn build(self) -> Result<MagicianService> {
        let Self {
            tool_catalog,
            tool_matching,
            semantic_search,
            capability_pack_defs,
            secret_runtime_bootstrap,
            boot_config,
            repo_root,
            capability_scope_paths,
        } = self;

        use crate::{
            config::MagicianConfig,
            magician_v2::{
                ask_loop::{
                    api::AskLoopApi,
                    batch_tracker::QuestionBatchTracker,
                    budget::{BudgetConfig, BudgetPolicy},
                    clarifier::ClarifierLibrary,
                    history::ClarificationHistory,
                    ledger::BudgetLedger,
                    metrics::ClarificationMetrics,
                    pause::{
                        InMemoryQueueRepository, PauseResumeManager, QueueRepository, WaitingQueue,
                    },
                    session_manager::{ClarificationSessionManager, SessionManager},
                    triggers::ResumeTriggerService,
                },
                confidence::ConfidenceService,
                prompts::{json_storage::JsonStorageConfig, PromptStore},
                query_analysis::operation_llm_router::{OperationLLMWrapper, QueryAnalysisLLM},
                realtime_events::{
                    RuntimeTransportBroadcaster, DEFAULT_RUNTIME_TRANSPORT_CAPACITY,
                },
                services::MagicianV2Services,
                slot_graph::{
                    adapters::MultiLlmRewriteModel,
                    rewriter::{QuestionRewriter, RewriteModel, RewriterConfig},
                },
                state_tracker::StateTracker,
                storage::{FileV2Store, V2ConversationStore},
                tool_matcher::{ToolMatcherConfig, V2ToolMatcher},
                JsonPromptStorage, MagicianV2Orchestrator, PromptManager,
            },
        };

        // The binary already loaded environment/config before creating any
        // runtime threads. Embedded callers retain the standalone loader.
        if boot_config.is_none() {
            info!("🔑 Loading environment variables from .env files");

            // Load environment variables from .env files for API keys
            // This ensures OPENAI_API_KEY and other API keys are available
            match dotenvy::from_filename(
                crate::magician_v2::artifact_v2::workspace::runtime_config_path(
                    ".env.development",
                    ".env.development",
                ),
            ) {
                Ok(_) => info!("✅ Successfully loaded .env.development"),
                Err(e) => info!("⚠️  Could not load .env.development: {}", e),
            }
            match dotenvy::from_filename(
                crate::magician_v2::artifact_v2::workspace::runtime_config_path(".env", ".env"),
            ) {
                Ok(_) => info!("✅ Successfully loaded .env"),
                Err(e) => info!("⚠️  Could not load .env: {}", e),
            }

            // Verify critical API keys are loaded
            if std::env::var("OPENAI_API_KEY").is_ok() {
                info!("✅ OPENAI_API_KEY is set");
            } else {
                warn!("❌ OPENAI_API_KEY is NOT set - OpenAI LLM operations will fail!");
            }
            if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                info!("✅ ANTHROPIC_API_KEY is set");
            } else {
                info!("ℹ️  ANTHROPIC_API_KEY is not set (optional)");
            }
        }
        info!("Creating LLM service for query analysis");

        // Resolve the magician runtime config (runtime root → repo seed → legacy).
        let magician_config_path = crate::config::magician_config_path();

        if boot_config.is_none() && !magician_config_path.exists() {
            return Err(MagicianError::config(format!(
                "{} not found. This file is required for MagicianV2 operation. Please create it with LLM configurations.",
                magician_config_path.display()
            )));
        }

        info!(
            "Loading {} for multi-LLM support",
            magician_config_path.display()
        );

        // Load through the shared loader, never a raw parse: every derived
        // field — notably `llm.router.locality`, copied from
        // `privacy.processing.mode` — must reach this router exactly as it
        // reaches the main startup path. A raw parse here left the
        // orchestrator's router on `Local` while the file said `cloud`, so
        // channel operations kept dispatching to the local generation model.
        // The loader's process-global installs are first-install-wins, so the
        // main path having loaded first is not a conflict.
        let loader_path = magician_config_path.clone();
        let magician_config: MagicianConfig = if let Some(config) = boot_config {
            config
        } else {
            tokio::task::spawn_blocking(move || {
                crate::config::load_magician_config_from_path(&loader_path)
            })
            .await
            .map_err(|e| MagicianError::config(format!("config loader task failed to join: {e}")))?
            .map_err(|e| {
                MagicianError::config(format!(
                    "Failed to load {}: {:#}",
                    magician_config_path.display(),
                    e,
                ))
            })?
        };

        debug!("Creating V2 conversation store for V2 orchestrator");

        // Create V2-dedicated conversation store against the resolved workspace
        // provider so legacy conversation consumers follow the same canonical
        // runtime root as the main startup path.
        let workspace_storage_seed_root =
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::resolve_scoped_root(
                std::path::Path::new(&magician_config.storage_path),
            );
        // Publish the seed as the PROCESS-WIDE default so the many bare
        // `ArtifactV2Workspace::new(...)` subsystems resolve bootstrap templates
        // from the read-only repo seed instead of materializing them into
        // `<runtime_root>/system/`. Absolutize first (the dev path is a
        // CWD-relative `magician_data_v3`); guard on `<seed>/system` actually
        // existing so a misconfigured path never redirects template reads away
        // from a working store.
        {
            let seed_abs = if workspace_storage_seed_root.is_absolute() {
                workspace_storage_seed_root.clone()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(&workspace_storage_seed_root))
                    .unwrap_or_else(|_| workspace_storage_seed_root.clone())
            };
            if seed_abs.join("system").is_dir() {
                crate::magician_v2::artifact_v2::workspace::set_default_seed_root(seed_abs);
            }
        }
        // Runtime root: env override, else the `$HOME/MagicianNotes` default
        // (seed_root stays the repo seed — that's where the TEMPLATES live).
        let workspace_storage_bootstrap_root = crate::magician_v2::process_storage::runtime_root();
        let storage_workspace =
            crate::magician_v2::workspace_storage_settings::WorkspaceStorageSettingsStore::new(
                workspace_storage_bootstrap_root,
            )
            .with_seed_root(workspace_storage_seed_root)
            .resolve_workspace_sync()
            .map_err(|e| MagicianError::config(e.to_string()))?;
        let resolved_runtime_storage_path =
            storage_workspace.base_root().to_string_lossy().to_string();
        let file_conversation_store = Arc::new(FileV2Store::with_workspace_layout(
            storage_workspace.clone(),
        ));
        file_conversation_store
            .rebuild_execution_index()
            .await
            .map_err(|e| {
                MagicianError::internal(format!(
                    "Failed to initialize execution thread index: {}",
                    e
                ))
            })?;
        let conversation_store = file_conversation_store.clone() as Arc<dyn V2ConversationStore>;

        // Sized via `DEFAULT_RUNTIME_TRANSPORT_CAPACITY` (8192) rather than the
        // former 1000 slots: this single broadcast channel is shared by every
        // `/events` SSE forwarder AND the on-disk transport-log writer, so the
        // slowest co-subscriber lagging >capacity behind is what surfaces the
        // "buffer error" (`RecvError::Lagged`) banner. See the constant's doc.
        let event_broadcaster = Arc::new(
            RuntimeTransportBroadcaster::new(DEFAULT_RUNTIME_TRANSPORT_CAPACITY)
                .with_store(conversation_store.clone())
                .with_hitl_lifecycle_persistence(storage_workspace.clone()),
        );
        // Subscribe durable transport persistence immediately after creating
        // the broadcaster, before any producer-capable service is built. This
        // removes the startup window where live lifecycle events could be
        // broadcast but never reach the workspace log.
        let workspace_event_log_registry =
            crate::magician_v2::transport_log::WorkspaceEventLogRegistry::with_workspace_layout(
                storage_workspace.clone(),
            );
        crate::magician_v2::transport_log::spawn_workspace_event_log_writer(
            &event_broadcaster,
            workspace_event_log_registry.clone(),
        );
        info!("✅ V2 Event Broadcaster created for real-time progress tracking with storage persistence");
        let session_manager: Arc<dyn SessionManager> = Arc::new(ClarificationSessionManager::new(
            Arc::clone(&conversation_store),
            Arc::clone(&event_broadcaster),
        ));

        let execution_settings = &magician_config.execution;
        let base_url = Url::parse(&execution_settings.magicutor_base_url).map_err(|e| {
            MagicianError::config(format!(
                "Invalid magicutor_base_url '{}': {}",
                execution_settings.magicutor_base_url, e
            ))
        })?;

        let mut execution_config = MagicutorExecutionConfig::new(base_url);
        execution_config.request_timeout =
            Duration::from_secs(execution_settings.request_timeout_secs.max(1));

        if let Some(env_key) = &execution_settings.magicutor_api_key_env {
            match std::env::var(env_key) {
                Ok(value) if value.is_empty() => {
                    warn!(
                        "[MAGICIAN-V2] Magicutor API key env '{}' is defined but empty; continuing \
                         without authentication",
                        env_key
                    );
                },
                Ok(value) => {
                    execution_config.api_key = Some(SecretString::new(value));
                },
                Err(err) => {
                    warn!(
                        "[MAGICIAN-V2] Failed to read Magicutor API key env '{}': {}",
                        env_key, err
                    );
                },
            }
        }

        let magicutor_client = Arc::new(MagicutorClient::new(execution_config).map_err(|e| {
            MagicianError::config(format!("Failed to create Magicutor client: {}", e))
        })?);

        let router_config = magician_config.router_config().cloned();

        if router_config.is_none() {
            return Err(MagicianError::config(
                "active magician-config.yaml has no llm.router configuration. At least one LLM profile is required.".to_string(),
            ));
        }
        let router_config = router_config.expect("checked is_some");

        info!(
            "Loaded {} LLM profiles ({} adaptive composites) from {}",
            router_config.profiles.len(),
            router_config.adaptive_profiles.len(),
            magician_config_path.display()
        );
        info!("Operation mappings: {:?}", router_config.operation_mapping);
        if !router_config.adaptive_profiles.is_empty() {
            // Sort by name so successive boots log in the same order —
            // makes diff-comparing two startup logs trivial. HashMap
            // iteration is non-deterministic.
            let mut adaptive_names: Vec<&String> = router_config.adaptive_profiles.keys().collect();
            adaptive_names.sort();
            for name in adaptive_names {
                if let Some(adaptive) = router_config.adaptive_profiles.get(name) {
                    info!(
                        "Adaptive profile `{}`: fast=`{}` thinking=`{}`",
                        name, adaptive.fast_profile, adaptive.thinking_profile
                    );
                }
            }
        }

        // Build MultiLLMService from the same router config so ChatLlmService
        // gets a properly initialised service instead of falling back to a dummy.
        let multi_llm_service = Arc::new(MultiLLMService::from_router_config(&router_config));

        // Create operation-aware LLM router
        let operation_llm_router = Arc::new(OperationLlmRouter::new(Some(router_config)));

        // Structured-decision plane: the `decision-engine` process owns
        // models, routes, thresholds, and rollout policy
        // (`decision-engine.yaml`); this process only asks it, over its
        // socket. An unavailable configured engine fails the decision.
        crate::magician_v2::decision_host::set_global_decision_locality(
            magician_config.privacy.processing.mode == magicllm::ProcessingLocality::Local,
        );
        crate::magician_v2::decision_host::configure(&magician_config.decision);
        crate::magician_v2::decision_host::set_health_broadcaster(&event_broadcaster);

        // Wrap it for query analysis operation
        let llm_service = Arc::new(OperationLLMWrapper::new(
            operation_llm_router.clone(),
            LLMOperation::QueryAnalysis,
        )) as Arc<dyn QueryAnalysisLLM>;

        // Validate LLM is available before proceeding
        info!("Validating LLM service availability...");
        if !llm_service.is_available().await {
            error!("❌ LLM service is not available");
            return Err(MagicianError::config(
                "LLM service is not available. This is required for query analysis. Please check your API key configuration in the config file or environment variables.".to_string(),
            ));
        }
        info!("✅ LLM service validated successfully");

        debug!("Setting up prompt manager with actual data directory");

        // Set up prompt manager with actual data directory
        let data_dir = crate::magician_v2::prompts::json_storage::default_prompt_dir();
        let storage_config = JsonStorageConfig {
            storage_dir: data_dir,
            enable_cache: true,
            max_cache_entries: 50,
        };

        let prompt_storage: Arc<dyn PromptStore> =
            Arc::new(JsonPromptStorage::new(storage_config).map_err(|e| {
                MagicianError::config(format!("Failed to create prompt storage: {}", e))
            })?);

        // Initialize the prompt storage to create directories
        prompt_storage.initialize().await.map_err(|e| {
            MagicianError::config(format!("Failed to initialize prompt storage: {}", e))
        })?;

        let prompt_manager = Arc::new(PromptManager::new(Arc::clone(&prompt_storage)));
        // Process-global handle for code outside the app_data graph (bare
        // screen routes, media-rail sessions) — same pattern as
        // `set_global_operation_router`.
        crate::magician_v2::prompts::set_global_prompt_manager(Arc::clone(&prompt_manager));

        // The owner's taste profile loader, installed here rather than in the
        // HTTP binary so embedded and test harnesses that build a service
        // without serving HTTP get the same prompt content. One loader for
        // the process: its provider-fault latches warn once per stage per
        // loader, so a loader built per prompt assembly would warn on every
        // turn. A process that never installs one simply renders no profile,
        // which is the same answer as "no profile note exists".
        crate::magician_v2::taste_profile::install_global_taste_profile_loader(Arc::new(
            crate::magician_v2::taste_profile::TasteProfileLoader::new(
                crate::magician_v2::notes::NotesSettingsStore::with_workspace_layout(
                    storage_workspace.clone(),
                ),
                magician_config.memory.taste_profile.clone(),
            ),
        ));

        debug!("Creating MagicianV2 orchestrator with HTTP adapters");

        info!("Using provided HTTP adapters for tool catalog, matching, and semantic search");

        let on_failure_str = match magician_config.execution.on_failure {
            crate::config::OnFailureMode::AskUser => "ask_user".to_string(),
            crate::config::OnFailureMode::Fail => "fail".to_string(),
        };
        let runtime_config: Arc<dyn RuntimeConfig> = Arc::new(MagicianRuntimeConfig::new(
            resolved_runtime_storage_path,
            magician_config.realtime_events,
            magician_config.max_conversations,
            magician_config.conversation_timeout,
            false, // allow_consent_slots: default to false (runtime consent policies only)
            magician_config.consumer_mode,
            magician_config.execution.shell_sandbox.clone(),
            magician_config.execution.file_sandbox.clone(),
            on_failure_str,
        ));

        let services = MagicianV2Services::new(
            Arc::clone(&runtime_config),
            Arc::clone(&conversation_store),
            prompt_manager.clone(),
            Arc::clone(&tool_catalog),
            Arc::clone(&tool_matching),
            Some(Arc::clone(&semantic_search)),
            session_manager.clone(),
            Arc::clone(&magicutor_client),
            capability_pack_defs,
            repo_root,
            capability_scope_paths,
        )
        .with_workspace_layout(storage_workspace.clone());

        // Create V2 Tool Matcher with default configuration
        let tool_matcher_config = ToolMatcherConfig::default();

        // Initialize Ask Loop stack (clarifier + pause/resume pipeline)
        let confidence_service = Arc::new(ConfidenceService::default());
        let state_tracker = Arc::new(StateTracker::with_confidence_service(
            conversation_store.clone(),
            confidence_service.clone(),
        ));
        let budget_policy = Arc::new(BudgetPolicy::with_confidence_service(
            BudgetConfig::default(),
            confidence_service.clone(),
        ));
        let ledger = Arc::new(BudgetLedger::new(state_tracker.clone(), budget_policy));
        let queue_repo: Arc<dyn QueueRepository> = Arc::new(InMemoryQueueRepository::default());
        let waiting_queue = Arc::new(WaitingQueue::new(queue_repo));
        let pause_manager = Arc::new(PauseResumeManager::new(
            state_tracker.clone(),
            waiting_queue,
        ));

        let clarifier_llm: Arc<dyn LlmService> = Arc::new(MultiLlmClarifierAdapter::new(
            operation_llm_router.clone(),
            LLMOperation::Other("ask_loop_clarifier".to_string()),
            event_broadcaster.clone(),
        ));

        // Create AnswerInterpreter with explicit type coercion (Gap #11 fix)
        let answer_interpreter_llm: Arc<
            dyn crate::magician_v2::ask_loop::answer_interpreter::AnswerInterpretationLLM,
        > = Arc::new(MultiLlmAnswerInterpreterAdapter::new(
            operation_llm_router.clone(),
            LLMOperation::Other("answer_interpretation".to_string()),
            event_broadcaster.clone(),
        ));
        let answer_interpreter = Arc::new(
            crate::magician_v2::ask_loop::answer_interpreter::AnswerInterpreter::new(
                answer_interpreter_llm,
                prompt_manager.clone(), // Use prompt_manager like all other services
            ),
        );

        // Wire answer interpreter into clarifier - fail fast if prompts not available
        let mut clarifier_library = ClarifierLibrary::with_prompt_manager(
            prompt_manager.clone(),
            crate::magician_v2::prompts::constants::versions::ASK_LOOP_CLARIFIER,
            Arc::clone(&clarifier_llm),
        )
        .await
        .map_err(|err| {
            MagicianError::config(format!("Failed to load clarifier prompts: {}", err))
        })?;
        clarifier_library.set_answer_interpreter(answer_interpreter.clone());
        let clarifier = Arc::new(clarifier_library);
        let clarification_history = Arc::new(ClarificationHistory::new());
        let resume_service = Arc::new(ResumeTriggerService::new(
            pause_manager.clone(),
            clarifier.clone(),
            Some(ledger.clone()),
            Some(event_broadcaster.clone()),
            clarification_history.clone(),
        ));
        let batch_tracker = Arc::new(QuestionBatchTracker::new());
        let confidence_tracker =
            Arc::new(crate::magician_v2::ask_loop::PlanConfidenceTracker::with_defaults());

        // Create query rewriter for batch completion enrichment
        info!("Creating query rewriter for batch completion enrichment");
        let rewrite_model: Arc<dyn RewriteModel> = Arc::new(MultiLlmRewriteModel::new(
            operation_llm_router.clone(),
            LLMOperation::QuestionRewriting, // Use dedicated variant to hit configured model
        ));
        let curation_model: Arc<dyn RewriteModel> = Arc::new(MultiLlmRewriteModel::new(
            operation_llm_router.clone(),
            LLMOperation::QuestionCuration,
        ));
        let query_rewriter = Arc::new(QuestionRewriter::with_curation_model(
            rewrite_model,
            curation_model,
            prompt_manager.clone(),
            RewriterConfig::default(),
        ));
        info!("✅ Query rewriter created successfully");

        let clarification_metrics = Arc::new(ClarificationMetrics::new());

        let ask_loop_api = Arc::new(AskLoopApi::new(
            clarifier,
            pause_manager,
            ledger.clone(),
            resume_service,
            clarification_history,
            batch_tracker,
            confidence_tracker,
            query_rewriter,
            Arc::clone(&clarification_metrics),
            Arc::clone(&session_manager),
            Arc::clone(&conversation_store),
        ));

        // Phase 2 & 6: Wire up trackers to resume service for batch-aware resume and confidence tracking
        ask_loop_api.wire_batch_tracker();
        ask_loop_api.wire_confidence_tracker();
        ask_loop_api.wire_query_rewriter();

        // Create dedicated LLM service for tool evaluation/disambiguation.
        info!("Creating dedicated tool evaluation LLM service using ToolEvaluation operation");
        let tool_eval_llm_service = Arc::new(OperationLLMWrapper::new(
            operation_llm_router.clone(),
            LLMOperation::ToolEvaluation,
        )) as Arc<dyn QueryAnalysisLLM>;

        // V2 explicitly requires V2 Tool Matcher - no graceful degradation
        let v2_tool_matcher = Arc::new(
            V2ToolMatcher::new_with_broadcaster(
                Arc::clone(&tool_catalog),
                Some(Arc::clone(&semantic_search)),
                tool_eval_llm_service,
                prompt_manager.clone(),
                tool_matcher_config,
                Some(Arc::clone(&event_broadcaster)),
            )
            .map_err(|e| {
                MagicianError::config(format!("V2 Tool Matcher initialization failed: {}", e))
            })?,
        );

        info!("✅ V2 Tool Matcher initialized with compact router + LLM fallback");

        let secret_runtime_bootstrap = secret_runtime_bootstrap
            .unwrap_or_else(crate::magician_v2::secrets::bootstrap_secret_runtime);

        // Create the V2 orchestrator with full configuration including V2 Tool Matcher
        // V2 flow explicitly uses V2 Tool Matcher (required, not optional)
        // The orchestrator will use the same event broadcaster instance
        let mut v2_orchestrator = MagicianV2Orchestrator::new_with_secret_runtime(
            services,
            llm_service,
            secret_runtime_bootstrap,
        )
        .with_operation_llm_router(operation_llm_router.clone())
        .with_multi_llm_service(multi_llm_service)
        .with_confidence_service(confidence_service)
        .with_ask_loop_api(ask_loop_api.clone())
        .with_v2_tool_matcher(v2_tool_matcher.clone())
        .with_event_broadcaster(event_broadcaster.clone());

        // API Mining: Configure the API router if replay is enabled
        v2_orchestrator.configure_api_mining(&magician_config.api_mining);
        // Capability evolution track (default-off, explicit gating)
        v2_orchestrator.configure_capability_evolution(&magician_config.capability_evolution);
        // Developer-Mode `interactive_process` policy — per-program
        // concurrency caps (claude/codex/agy/opencode default to 1).
        v2_orchestrator.configure_interactive_process(&magician_config.interactive_process);
        // Tool authorization policy for unlisted tools
        v2_orchestrator.configure_tool_authorization(&magician_config.tool_authorization);
        crate::magician_v2::agents::configure_memory_prompt_budgets(&magician_config.memory);
        // Publish the coding budgets for readers that hold no config snapshot —
        // notably the agentic executor's wall-clock watchdog, which must move in
        // lockstep with the Pi turn clip or one ceiling is lifted and the other
        // silently is not.
        crate::magician_v2::execution::coding_engine::configure_coding_budgets(
            &magician_config.coding,
        );
        // RSS probe only. magician-bin already applies live_agent_limit from
        // the resolved plan; per-agent cap and RSS tripwire bytes remain PR8.
        crate::magician_v2::local_resource_governor::install_process_rss_probe();
        let v2_orchestrator = v2_orchestrator.initialize(); // Finalize with state transition service

        Ok(MagicianService::new(
            v2_orchestrator, // Already Arc<MagicianV2Orchestrator> from initialize()
            event_broadcaster,
            workspace_event_log_registry,
            ask_loop_api,
        ))
    }
}
