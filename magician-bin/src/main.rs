//! Standalone Magician V2 service binary.
//!
//! This binary hosts the Magician V2 orchestrator behind an Actix-web server so
//! it can run out-of-process with local in-process tool discovery and matching.

mod adapters;
mod startup_http;

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use actix_files::{Files, NamedFile};
use actix_web::{
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    error::JsonPayloadError,
    middleware::{from_fn, Next},
    mime, web, App, HttpRequest, HttpResponse,
};
use adapters::local_tool_services::LocalToolServices;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use magician::{
    config::{
        MagicianConfig, MagicianFrontendMode, RecordingSttConfig, RecordingSttProviderConfig,
        TtsConfig, TtsProviderConfig,
    },
    magician_core::{MagicianService, MagicianServiceBuilder},
    magician_v2::agents::{
        inspect_scope_memory_index, memory_index_stale_reason_is_soft, optimize_scope_memory_index,
        rebuild_scope_memory_index, AgentDefinitionStore, AgentMemoryResolver,
    },
    magician_v2::execution::{
        embedded_compiled_pack_defs, embedded_compiled_pack_defs_ref, embedded_compiled_pack_yaml,
        load_pack_defs_from_skills_dir, pack_defs_to_tool_infos, prune_runtime_disabled_pack_defs,
        prune_unexecutable_pack_defs, register_harness_action_providers,
        register_harness_read_providers, AgentRosterDataProvider, CapabilityPackDefinition,
        EvidenceDataProvider, InternalDataProvider, MeetingsDataProvider, MemoryDataProvider,
        NotesDataProvider, TaskStateProvider, TasksDataProvider, ThinkingMapsDataProvider,
    },
    magician_v2::{
        analytics::{
            memory_eval_runner::{MemoryEvalRunner, MemoryEvalRunnerConfig},
            memory_index_maintainer::{MemoryIndexMaintainer, MemoryIndexMaintainerConfig},
            memory_utility_batch_runner::{
                MemoryUtilityBatchRunner, MemoryUtilityBatchRunnerConfig,
            },
        },
        artifact_v2::{scheduler::V3DelegationDispatcher, ArtifactV2Service},
        ask_loop::api::submit_clarification_handler,
        chat::{
            escalation_listener::EscalationListener,
            llm_service::ChatLlmService,
            planning_listener::PlanningListener,
            public_contact_profile::FilePublicContactProfileStore,
            service::{ChatAgenticRuntime, ChatProgressSync, ChatService},
            storage::FileChatStore,
        },
        execution::agentic::FullPauseStore,
        feed::{FeedMaterializer, FeedStore},
        gaui::{emitter::MuijDeltaEmitter, MuijStorage},
        orchestrator::MagicianV2Orchestrator,
        progress_channel_seam::{
            event_log::EventLog, AgentMemoryChannel as ProgressAgentMemoryChannel,
            ChatChannel as ProgressChatChannel, ExecutionProgressRouter,
            WebhookChannel as ProgressWebhookChannel,
        },
        user_requests::UserRequestService,
    },
    runtime_plan::{resolve_boot_runtime_plan, EffectiveRuntimePlan, LeftoverDispatchScalars},
};
use magician_api::{
    ambient_api::{
        get_ambient_stats_handler, get_ambient_status_handler, post_ambient_distill_handler,
        post_ambient_enroll_handler, post_ambient_signals_batch_handler,
        put_ambient_config_handler, AmbientApi, AmbientDistillConfig, AmbientDistillWorker,
    },
    api_mining_api::{
        allow_origin, approve_projection, bless_capability, block_and_purge_origin, block_origin,
        disable_and_purge_api_mining, get_all_auth_statuses, get_api_mining_settings,
        get_auth_status, get_capability, get_capability_evolution_summary, get_noisy_origins,
        get_openapi, get_origin_auth_refresh_status, get_overview, get_passive_validation_metrics,
        get_projection_metrics, get_recipe, get_recipe_metrics, get_registry, get_registry_health,
        get_replay_metrics, get_router_metrics, get_sequence, get_sequence_metrics, get_workflow,
        get_workflow_metrics, list_projections, list_recipe_runs, list_recipes, list_replay_grants,
        list_sequences, list_workflows, purge_origin, purge_projection_rows,
        put_api_mining_settings, query_known_resource_handler, refresh_origin_auth,
        replay_capability, replay_recipe, replay_workflow, revoke_replay_grant,
        set_origin_allow_replay, set_origin_replay_mode, ApiMiningApi,
    },
    apps_api::{configure_app_routes, AppPlatformApi},
    artifact_api::{list_durable_artifacts, read_durable_artifact, ArtifactApi},
    bot_api::{
        delete_bot_config_handler, get_bot_auth_handler, get_bot_auth_state_handler,
        get_bot_config_handler, get_bot_env_handler, get_bot_logs_handler, get_bot_qr_handler,
        list_bots_auth_handler, list_bots_handler, put_bot_config_handler, put_bot_env_handler,
        restart_bot_handler, start_bot_auth_handler, start_bot_handler, stop_bot_handler,
        submit_bot_auth_input_handler, BotApi,
    },
    chat_api::{
        submit_concurrent_voice_handler, list_concurrent_voice_handler,
        cancel_concurrent_voice_handler, concurrent_voice_delivery_handler, concurrent_voice_result_handler,
        cancel_chat_run_handler, clear_messages_handler, clear_queued_messages_handler,
        delete_message_handler, delete_queued_message_handler, delete_session_handler,
        delete_tailed_task_handler, get_active_session_handler, get_messages_handler,
        get_public_chat_status_handler, get_reference_catalog_handler, get_session_handler,
        get_session_output_handler, get_tailed_task_handler, list_chat_profiles_handler,
        list_public_contact_profiles_handler, list_queued_messages_handler, list_sessions_handler,
        new_session_handler, open_output_file_handler, open_output_folder_handler,
        post_invoke_server_action_handler, post_transcript_handler, post_tutor_cancel_handler,
        post_tutor_user_action_handler, read_chat_result_handler, send_message_handler,
        send_message_stream_handler, subscribe_to_tailed_task_handler, update_session_handler,
        upload_attachment_handler, ChatApi,
    },
    close_interactive_session_handler,
    components_api::{configure_component_routes, ComponentSetupApi},
    contextual_writing_api::{
        contextual_writing_action_handler, contextual_writing_catalog_handler,
        create_contextual_writing_session_handler,
    },
    enrollment_api::{
        approve_handler, cancel_handler, enroll_handler, enrollment_status_handler, revoke_handler,
        EnrollmentApi,
    },
    events_api::{debug_emit_event_handler, list_events_v3_handler, EventsApi},
    evidence_api::{
        get_evidence_dashboard_handler, get_evidence_utility_handler, list_entities_handler,
        list_evidence_handler, list_evidence_reviews_handler, post_entity_correct_handler,
        post_evidence_correct_handler, post_evidence_dashboard_publish_handler,
        post_evidence_review_feedback_handler, post_evidence_review_handler, EvidenceApi,
    },
    execution_panel_api::{
        get_execution_panel_handler, get_task_execution_panel_handler, ExecutionPanelApi,
    },
    feed_api::{
        feed_archive_learning_candidate_handler, feed_archive_learning_insight_handler,
        feed_attention_dismiss_handler, feed_attention_handler, feed_attention_item_handler,
        feed_attention_undismiss_handler, feed_clear_handler,
        feed_confirm_learning_candidate_handler, feed_counts_handler,
        feed_create_learning_insight_task_handler, feed_delete_item_handler,
        feed_edit_confirm_learning_candidate_handler, feed_purge_orphans_handler,
        feed_save_learning_insight_handler, list_feed_handler, today_handler,
        today_item_action_handler, today_visibility_handler, today_visibility_list_handler,
        FeedApi,
    },
    gaui_api::{get_layout_handler, put_layout_handler, GauiApi},
    interactive_session_buffer_handler, interactive_session_diff_handler,
    list_interactive_cli_runtimes_handler, list_interactive_directories_handler,
    list_interactive_sessions_handler,
    llm_routing_api::{
        llm_routing_clear_handler, llm_routing_engine_clear_handler,
        llm_routing_engine_set_handler, llm_routing_overview_handler, llm_routing_set_handler,
    },
    local_generation_api::{
        get_local_generation_settings_handler, put_local_generation_settings_handler,
    },
    media_api::{
        clear_tts_cache_handler, delete_media_session_handler, get_audio_settings_handler,
        get_media_preferences_handler, get_media_session_handler,
        get_resolved_audio_surface_handler, heartbeat_media_session_handler,
        list_media_providers_handler, list_media_sessions_handler, patch_media_session_handler,
        post_audio_engine_model_control_handler, post_media_event_handler,
        post_voice_note_event_handler, put_audio_settings_handler, put_media_preferences_handler,
        register_media_session_handler, submit_voice_note_handler, synthesize_message_handler,
        synthesize_tts_handler, transcribe_stt_handler, transcribe_stt_stream_handler,
        tts_cache_stats_handler, MediaApi,
    },
    memory_api::{
        confirm_user_memory_entry_handler, get_memory_effect_review_handler,
        keep_user_memory_entry_conflict_handler, list_user_memory_entries_handler,
        memory_save_preference_handler, memory_search_handler,
        patch_user_memory_entry_scope_handler, post_memory_effect_review_handler, MemoryApi,
    },
    monitors_api::{
        convert_task_to_monitor_v3_handler, create_monitor_v3_handler, delete_monitor_v3_handler,
        get_monitor_feedback_v3_handler, get_monitor_runs_v3_handler,
        get_monitor_updates_v3_handler, get_monitor_v3_handler, get_monitors_metrics_v3_handler,
        list_monitor_updates_v3_handler, list_monitors_v3_handler, pause_monitor_v3_handler,
        post_monitor_feedback_v3_handler, resume_monitor_v3_handler, run_monitor_v3_handler,
        update_monitor_v3_handler,
    },
    notes_api::{configure_notes_routes, NotesApi},
    plane_api::{
        plane_chat_engine_put_handler, plane_decision_mode_put_handler, plane_decision_routing_get_handler, plane_decision_routing_put_handler, plane_engine_put_handler,
        plane_engines_handler, plane_grants_list_handler, plane_grants_mint_handler,
        plane_grants_revoke_handler, plane_mcp_delete_handler, plane_mcp_handler,
        plane_mcp_sse_handler,
    },
    privacy_api::{get_privacy_settings_handler, put_privacy_settings_handler},
    progress_channel_api::{
        create_webhook_subscription_handler, delete_progress_subscription_handler,
        list_webhook_subscriptions_handler, ProgressChannelApi,
    },
    resource_authority_api::configure_resource_authority_routes,
    runtime_env_api::{configure_runtime_env_routes, RuntimeEnvApi},
    secret_vault_api::{configure_secret_vault_routes, SecretVaultApi},
    skills_api::{configure_skills_routes, SkillsApi},
    start_interactive_session_handler,
    task_api_v3::{
        analyze_task_v3_handler, approve_task_plan_v3_handler, approve_task_v3_handler,
        cancel_execution_v3_handler, create_task_v3_handler, delete_internal_task_v3_handler,
        delete_task_plan_version_v3_handler, delete_task_v3_handler, download_artifact_v3_handler,
        download_task_output_v3_handler, execute_task_v3_handler, export_task_outputs_v3_handler,
        get_execution_delegations_v3_handler, get_execution_outputs_v3_handler,
        get_execution_refs_v3_handler, get_execution_schedule_v3_handler,
        get_execution_tree_v3_handler, get_execution_v3_handler,
        get_published_surface_render_v3_handler, get_published_surface_top_feed_v3_handler,
        get_published_surface_v3_handler, get_task_details_v3_handler, get_task_outputs_v3_handler,
        get_task_plan_analysis_v3_handler, get_task_plan_attempts_v3_handler,
        get_task_plan_clarifications_v3_handler, get_task_plan_pending_questions_v3_handler,
        get_task_plan_slots_v3_handler, get_task_plan_v3_handler, get_task_plan_version_v3_handler,
        get_task_progress_v3_handler, get_task_refs_v3_handler, get_task_v3_handler,
        list_executions_v3_handler, list_internal_tasks_v3_handler,
        list_pending_task_plan_clarifications_v3_handler,
        list_published_surface_projections_v3_handler, list_published_surfaces_v3_handler,
        list_task_plan_versions_v3_handler, list_tasks_v3_handler,
        open_task_output_file_v3_handler, open_task_output_folder_v3_handler,
        publish_surface_v3_handler, read_task_result_v3_handler, reject_task_plan_v3_handler,
        replan_task_v3_handler, republish_surface_v3_handler, restore_task_plan_version_v3_handler,
        resume_task_plan_clarifications_v3_handler, resynthesize_task_user_output_v3_handler,
        retry_synthesis_v3_handler, start_task_planning_v3_handler,
        submit_task_plan_clarification_v3_handler, unpublish_surface_v3_handler,
        update_task_plan_v3_handler, update_task_status_v3_handler, update_task_v3_handler,
        TaskApiV3,
    },
    tray_bridge_handler::tray_bridge_ws_handler,
    tutor_api::{configure_tutor_routes, TutorApi},
    ui_preferences_api::{
        get_ui_preferences_handler, put_ui_preferences_handler, UiPreferencesApi,
    },
    ui_thread_api::{
        create_ui_thread_handler, delete_ui_thread_handler, get_ui_thread_handler,
        list_ui_threads_handler, reorder_ui_threads_handler, search_history_handler,
        update_ui_thread_handler, UiThreadApi,
    },
    user_request_api::{list_user_requests_handler, respond_user_request_handler},
    vibedev_api::{
        activate_vibedev_project_handler, check_vibedev_deploy_settings_handler,
        citizen_code_knowledge_handler, citizen_preview_url_handler, citizen_secret_handler,
        control_vibedev_run_handler, create_vibedev_project_handler,
        delete_vibedev_project_handler, deploy_vibedev_project_handler,
        get_vibedev_deploy_settings_handler, get_vibedev_preview_handler,
        get_vibedev_project_file_handler, get_vibedev_project_files_handler,
        get_vibedev_project_info_handler, get_vibedev_project_screenshots_handler,
        get_vibedev_run_checkpoints_handler, get_vibedev_run_coding_events_handler,
        get_vibedev_run_logs_handler, get_vibedev_run_proposals_handler,
        list_vibedev_projects_handler, open_vibedev_repo_handler,
        put_vibedev_deploy_settings_handler, revert_vibedev_checkpoint_handler,
        run_vibedev_check_handler, start_vibedev_preview_handler, start_vibedev_run_handler,
        stop_vibedev_preview_handler, update_vibedev_project_handler, VibeDevApi,
    },
    vibedev_preview_proxy::vibedev_preview_proxy_handler,
    web_api::{
        cancel_active_execution_handler, cancel_paused_execution_handler,
        consolidate_user_memory_handler, continue_agentic_execution_handler,
        create_agent_definition_handler, create_execution_v2_handler, create_proposal_handler,
        delete_agent_definition_handler, delete_execution_v2_handler,
        delete_user_knowledge_key_handler, get_agent_artifacts_handler,
        get_agent_definition_handler, get_agent_effective_tools_handler,
        get_agent_episodes_handler, get_agent_harness_overview_handler, get_agent_health_handler,
        get_agent_memory_consolidation_health_handler, get_agent_memory_handler,
        get_agent_memory_tier_handler, get_agent_runtime_context_cache_handler,
        get_approval_handler, get_crew_health_handler, get_execution_control_state_handler,
        get_execution_responsibility_handler, get_execution_status_handler,
        get_execution_summary_handler, get_execution_v2_handler,
        get_harness_runtime_status_handler, get_mode_handler, get_observation_json_handler,
        get_observation_screenshot_handler, get_pause_state_handler, get_proposal_handler,
        get_storage_stats_handler, get_trust_policy_handler, get_user_knowledge_handler,
        search_owner_memory_handler,
        list_agent_definitions_handler, list_approvals_handler, list_coding_profiles_handler,
        list_executions_v2_handler, list_observations_handler, list_proposals_handler,
        list_triggers_handler, list_turns_v2_handler, manual_trigger_agent_handler,
        memory_pending_tasks_handler, memory_synthesize_handler, patch_agent_definition_handler,
        pause_active_execution_handler, pause_agent_handler, pause_outward_agents_handler,
        post_message_v2_handler, refresh_agent_definitions_handler,
        refresh_agent_runtime_context_cache_handler, refresh_agy_cli_handler,
        refresh_claude_code_handler, refresh_codex_app_server_handler, refresh_grok_acp_handler,
        reload_magician_config_handler, resolve_approval_handler, resolve_proposal_handler,
        respond_hitl_handler, restore_trust_policy_template_handler,
        resume_active_execution_handler, resume_agent_handler, resume_agentic_execution_handler,
        resume_outward_agents_handler, set_primary_agent_handler,
        spawn_agent_services_startup_hydration, spawn_agent_supervisor_tasks,
        start_execution_handler, steer_active_execution_handler, update_agent_definition_handler,
        update_execution_status_handler, update_harness_runtime_handler,
        update_trust_policy_handler, MagicianV2Api,
    },
    websocket_handler,
    workspace_storage_api::{configure_workspace_storage_routes, WorkspaceStorageApi},
    write_interactive_stdin_handler,
};
use magician_learning::execution_panel::{
    ExecutionPanelProjector, ExecutionPanelRuntimeStore, V3ExecutionPanelAdapter,
};

use mime::TEXT_HTML_UTF_8;
use runtime_core::{SemanticSearch, ToolCatalog, ToolMatching};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
// `Layer as _` brings `Layer::with_filter` into scope for `init_tracing`
// without claiming the (very common) `Layer` name inside this large file.
use tracing_subscriber::{
    fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer as _,
};

// The API CORS layer: one wildcard grant per response and every preflight
// answered before routing — see `magician_v2::cors` for why a catch-all
// route cannot do the latter.
use magician::magician_v2::cors::api_cors_middleware;

const API_JSON_PAYLOAD_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const CONTEXTUAL_WRITING_JSON_PAYLOAD_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const AGENT_SUPERVISOR_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

type DynSttProvider = Arc<dyn magician_media::media_rails::SttProvider>;
type DynTtsProvider = Arc<dyn magician_media::media_rails::TtsProvider>;
type DynStreamingSttProvider =
    Arc<dyn magician_media::media_rails::providers::StreamingSttProvider>;

#[derive(Parser, Debug)]
#[command(name = "magician")]
#[command(about = "Standalone Magician V2 service", version)]
struct Cli {
    /// Configuration file path used for local capability/registry loading
    #[arg(short, long, default_value = "tool-runtime-config.yaml")]
    config: PathBuf,

    /// Log level to use for tracing output
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Host interface to bind the HTTP server to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to expose the Magician HTTP API on
    #[arg(long, default_value_t = 3002)]
    port: u16,

    /// Path to the compiled Magician frontend assets (used when serving a local bundle)
    #[arg(long)]
    frontend_dir: Option<PathBuf>,

    /// How the Magician frontend UI should be delivered (auto-detects between api-only/static)
    #[arg(long, value_enum, default_value_t = FrontendModeFlag::Auto)]
    frontend_mode: FrontendModeFlag,

    /// Discard the list index and rebuild it from the records on disk, then
    /// exit without starting the server. The operator's recovery route for an
    /// index a running server cannot repair itself: a boot rebuild RESUMES,
    /// trusting every unit a previous run finished, so it can fill in a unit
    /// that was never walked but never correct one whose rows are wrong.
    #[arg(long)]
    reindex: bool,

    /// Storage bootstrap file. When omitted, and when
    /// `MAGICIAN_STORAGE_BOOTSTRAP_CONFIG` is unset, the process keeps the
    /// current local workspace_storage profile. Remote profiles fail closed
    /// before stores are opened.
    #[arg(long)]
    storage_bootstrap: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    /// Create, validate, test and pack strict declarative apps.
    ///
    /// Apps declare `dependencies.tools` by real name. `tools list`
    /// / `tools show` / `agents list` / `personalities list` /
    /// `procedure list` print the same catalog VibeDev App options
    /// use. Pack snapshots existing typed skills and compiled packs
    /// (`content_read`, `search_memory`, `create_task`, …).
    /// Workflows may name an `agent` and `personality`. `capability
    /// check` is only for a *new* tool SKILL.md. `approve` enables a
    /// `ready_for_review` installation through the same owner grant
    /// kernel the Apps directory uses.
    App {
        /// Emit one closed, versioned JSON envelope. Command failures exit 1;
        /// Clap usage errors remain exit 2.
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        command: magician::magician_v2::apps::authoring::AppAuthoringCommand,
    },
    /// Print normalized Ollama daemon/prewarm settings for lifecycle scripts.
    #[command(name = "ollama-launch-config", hide = true)]
    OllamaLaunchConfig,
    /// Plan or explicitly execute the dormant Ollama logical-chunk shadow
    /// fixtures. The command has no persistence handle and emits JSON only.
    #[command(name = "logical-chunk-eval")]
    LogicalChunkEval(LogicalChunkEvalArgs),
    /// Inspect or apply the bounded one-time drain for pre-fix agent-created tasks.
    #[command(name = "reconcile-orphaned-tasks")]
    ReconcileOrphanedTasks(ReconcileOrphanedTasksArgs),
    /// Operate on the scoped derived memory retrieval index.
    MemoryIndex {
        #[command(subcommand)]
        command: MemoryIndexCommand,
    },
    /// Irreversibly declare one scope free of pre-reverse-index stateless
    /// writers. Run only after the deployment has drained every older binary.
    #[command(name = "seal-stateless-loop-cutover")]
    SealStatelessLoopCutover(StatelessLoopCutoverArgs),
    /// Distill recent agent episodes into evidence-record proposals (read-only;
    /// prints candidates, persists nothing). Work-evidence graph, Phase 0.
    DistillEvidence(DistillEvidenceArgs),
    /// Generate an impact summary from an agent's accrued evidence over a
    /// chosen window (`--days`). Work-evidence graph, Phase 0 read path.
    Review(ReviewArgs),
    /// Print a deterministic evidence-quality health report for a scope (totals,
    /// correction / sensitive / anchored fractions, distinct entities, facet
    /// coverage, compaction headroom). Read-only, no LLM — WEG generic-substrate
    /// eval depth. Emits JSON.
    EvidenceEval(EvidenceEvalArgs),
    /// Print Layer-3 derived views over a scope's evidence graph: an entity
    /// neighborhood (`--entity <key>`) or the top sparse co-occurrence relations.
    /// Facet-scoped (`--facet`, "all" = no filter), sensitivity-suppressed,
    /// read-only, no LLM. Emits JSON.
    EvidenceGraph(EvidenceGraphArgs),
    /// Derive facet-scoped, evidence-grounded CLAIMS over a window: the LLM
    /// proposes interpretive assertions, the runtime validates citation grounding
    /// deterministically and drops ungrounded ones. Read-only + ephemeral (never
    /// persisted). WEG generic substrate. Emits JSON.
    EvidenceClaims(EvidenceClaimsArgs),
    /// LLM-grade evidence-vs-source faithfulness (WEG eval depth): sample evidence
    /// records, load each one's source episode, ask the judge whether the summary
    /// is faithful (invents nothing), and report a precision score. Needs the LLM
    /// router; ambient records with no source episode are gradeable-skipped. JSON.
    EvidencePrecision(EvidencePrecisionArgs),
    /// Export a human-readable work ledger to `docs/worklog/<agent>.md` (one file
    /// per agent): render each agent's durable `work_outcome` evidence records to
    /// deterministic, newest-first Markdown. Pass `--agent` for one agent, or omit
    /// to export every agent in the scope. ONE-WAY — reads evidence, writes
    /// Markdown; the rendered docs are not agent-reachable and are never read back.
    WorklogExport(WorklogExportArgs),
    /// Analytics DuckDB migration helpers: EXPORT/IMPORT a (possibly legacy /
    /// version-incompatible) analytics DB across a DuckDB storage-format change.
    /// EXPORT must be run with a binary whose DuckDB can read the source file
    /// (i.e. the OLD pin); IMPORT with the new pin. See the analytics migration
    /// runbook. Run with `MAGICIAN_SKIP_KEYCHAIN=1` (no secrets needed).
    Analytics {
        #[command(subcommand)]
        command: AnalyticsCommand,
    },
    /// Install immutable Attention Learning artifacts. Installation never
    /// activates a model or edits runtime configuration.
    #[command(name = "attention-learning")]
    AttentionLearning {
        #[command(subcommand)]
        command: AttentionLearningCommand,
    },
    /// Channel Assist helpers (fixture export and classifier eval).
    /// Run with `MAGICIAN_SKIP_KEYCHAIN=1` (no secrets needed).
    #[command(name = "channel-assist")]
    ChannelAssist {
        #[command(subcommand)]
        command: ChannelAssistCommand,
    },
    /// One-shot migration of the retired first-party Town Square corpus into
    /// the `town_square` app package's entity store. Idempotent: a corpus that
    /// already matches is left alone and a partial run is finished rather than
    /// duplicated. Read-only against the social store — it never writes there.
    #[command(name = "town-square-migrate")]
    TownSquareMigrate(TownSquareMigrateArgs),
    /// Track B storage activation. Default server startup does not run these
    /// commands. Cutover stays fail-closed until Decision Gate 3 is accepted.
    Storage {
        #[command(subcommand)]
        command: magician::magician_v2::storage_activation::StorageCommand,
    },
}

#[derive(clap::Args, Debug, Clone)]
struct LogicalChunkEvalArgs {
    /// Synthetic/approved fixture suite. Production memory is never loaded.
    #[arg(long)]
    fixtures: std::path::PathBuf,
    /// Execute local Ollama shadow calls. Without this flag the command only
    /// validates dormant config and runs the deterministic planner.
    #[arg(long)]
    execute: bool,
    /// Also run the same chunk/adapters against each current authoritative
    /// cloud profile. Requires --execute and may incur provider cost.
    #[arg(long, requires = "execute")]
    compare_cloud: bool,
    /// Repeat count for each live shadow case (ignored in plan-only mode).
    #[arg(long, default_value_t = 5)]
    repeats: u32,
    /// Optional JSON report path. The same report is always printed to stdout.
    #[arg(long)]
    output: Option<std::path::PathBuf>,
}

#[derive(clap::Args, Debug, Clone)]
struct ReconcileOrphanedTasksArgs {
    /// Running Magician service base URL.
    #[arg(long, default_value = "http://127.0.0.1:3002")]
    api_base: String,
    /// Maximum keeper tasks dispatched in this pass (1-100).
    #[arg(
        long,
        default_value_t = magician::magician_v2::execution::task_reconcile::DEFAULT_RECONCILE_BATCH_CAP
    )]
    batch_cap: usize,
    /// Apply the plan. Without this flag the command is read-only.
    #[arg(long)]
    apply: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct TownSquareMigrateArgs {
    /// Scope principal whose square is migrated.
    #[arg(long, default_value = magician::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_PRINCIPAL)]
    principal: String,
    /// Scope workspace whose square is migrated.
    #[arg(long, default_value = magician::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_WORKSPACE)]
    workspace: String,
    /// Emit the full migration report as JSON.
    #[arg(long)]
    json: bool,
    /// Restore history while preserving the live roster, moods and policy.
    #[arg(long)]
    history_only: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct StatelessLoopCutoverArgs {
    /// Principal whose legacy writers have been drained.
    #[arg(long)]
    principal: String,
    /// Workspace whose legacy writers have been drained.
    #[arg(long)]
    workspace: String,
    /// Stable rollout/deployment identifier recorded in the immutable seal.
    #[arg(long)]
    deployment_id: String,
    /// Required acknowledgement that every binary lacking the reverse-binding
    /// protocol is stopped. This operation cannot be undone.
    #[arg(long)]
    confirm_legacy_writers_drained: bool,
}

#[derive(Subcommand, Debug)]
enum ChannelAssistCommand {
    /// Export the newest synced Gmail thread rows as labeled-ready JSONL
    /// for hand-labeling (the Phase-2 classifier precision gate). Privacy
    /// shape per row: HASHED thread ref (short sha256), REAL subject
    /// (labeling needs it — the file is LOCAL-only, never committed),
    /// sender name + domain only, label ids, message count, ages. NO
    /// recipient data of any kind.
    ExportFixtures(ChannelAssistExportFixturesArgs),
    /// Run the Phase-2 classifier over a hand-labeled JSONL (one `EvalFixture`
    /// per line: `gold_label` + subject/summary/metadata) and report per-label
    /// precision/recall. Requires `channel_classify` bound in `operation_mapping`.
    ClassifyEval(ChannelAssistClassifyEvalArgs),
    /// Score synthetic/hand-reviewed draft fixtures for usefulness,
    /// hallucination/privacy guardrails, and exact writing preferences.
    DraftEval(ChannelAssistDraftEvalArgs),
}

#[derive(Subcommand, Debug)]
enum AttentionLearningCommand {
    /// Validate and immutably install an actionability model snapshot without
    /// activating it.
    #[command(name = "install-actionability-snapshot")]
    InstallActionabilitySnapshot(InstallActionabilitySnapshotArgs),
    /// Validate and immutably install a calibrated pair-model snapshot without
    /// activating grouping.
    #[command(name = "install-pair-model-snapshot")]
    InstallPairModelSnapshot(InstallPairModelSnapshotArgs),
    /// Validate and immutably install a calibrated lane-routing policy without
    /// activating shadow or canary serving.
    #[command(name = "install-routing-policy-snapshot")]
    InstallRoutingPolicySnapshot(InstallRoutingPolicySnapshotArgs),
    /// Validate and immutably install a personal bandit policy without
    /// activating shadow/canary serving.
    #[command(name = "install-bandit-policy-snapshot")]
    InstallBanditPolicySnapshot(InstallBanditPolicySnapshotArgs),
    /// Preview or apply scoped analytical retention and bandit compaction.
    #[command(name = "retain-scope")]
    RetainScope(AttentionRetentionArgs),
    /// Preview or explicitly delete one principal/workspace learning scope.
    #[command(name = "delete-scope")]
    DeleteScope(AttentionDeleteScopeArgs),
    /// Fit an actionability snapshot from explicit scoped outcomes.
    /// Refuses to write when the trainer gates fail.
    #[command(name = "train-actionability")]
    TrainActionability(TrainActionabilityArgs),
    /// Fit a routing policy. Owner feedback does not choose lanes, so this
    /// command currently refuses until an explicit routing label source exists.
    #[command(name = "train-routing")]
    TrainRouting(TrainActionabilityArgs),
}

#[derive(clap::Args, Debug, Clone)]
struct InstallActionabilitySnapshotArgs {
    /// Local JSON artifact containing one complete model snapshot.
    #[arg(long)]
    snapshot: PathBuf,
}

#[derive(clap::Args, Debug, Clone)]
struct InstallPairModelSnapshotArgs {
    /// Local JSON artifact containing one complete pair-model snapshot.
    #[arg(long)]
    snapshot: PathBuf,
}

#[derive(clap::Args, Debug, Clone)]
struct InstallRoutingPolicySnapshotArgs {
    /// Local JSON artifact containing one complete routing-policy snapshot.
    #[arg(long)]
    snapshot: PathBuf,
}

#[derive(clap::Args, Debug, Clone)]
struct InstallBanditPolicySnapshotArgs {
    /// Local JSON artifact containing one complete personal-policy snapshot.
    #[arg(long)]
    snapshot: PathBuf,
}

#[derive(clap::Args, Debug, Clone)]
struct AttentionRetentionArgs {
    #[arg(long)]
    principal: String,
    #[arg(long)]
    workspace: String,
    #[arg(long)]
    cutoff_at: i64,
    /// Without this flag the command is a read-only preview.
    #[arg(long)]
    apply: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct AttentionDeleteScopeArgs {
    #[arg(long)]
    principal: String,
    #[arg(long)]
    workspace: String,
    /// Without this flag the command is a read-only preview.
    #[arg(long)]
    apply: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct TrainActionabilityArgs {
    #[arg(long, default_value = "anonymous")]
    principal: String,
    #[arg(long, default_value = "default")]
    workspace: String,
    /// Inclusive data cutoff as unix millis or YYYY-MM-DD. Defaults to now.
    #[arg(long)]
    cutoff: Option<String>,
    #[arg(long, default_value = "temporal")]
    split: String,
    #[arg(long)]
    out: PathBuf,
    /// After a passing run, install the artifact. First install is forced to
    /// shadow and never rewrites runtime YAML.
    #[arg(long)]
    install: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct ChannelAssistClassifyEvalArgs {
    /// Path to the labeled fixtures JSONL (each line an `EvalFixture`).
    #[arg(long)]
    fixtures: std::path::PathBuf,
}

#[derive(clap::Args, Debug, Clone)]
struct ChannelAssistDraftEvalArgs {
    /// JSONL path (one `DraftEvalFixture` per line).
    #[arg(long)]
    fixtures: std::path::PathBuf,
    /// Minimum expectation accuracy required for a successful exit.
    #[arg(long, default_value_t = 0.8)]
    threshold: f64,
}

#[derive(clap::Args, Debug, Clone)]
struct ChannelAssistExportFixturesArgs {
    /// Maximum number of thread rows to export (newest activity first).
    #[arg(long, default_value_t = 200)]
    limit: usize,
    /// Only export threads from this account alias (default: all synced
    /// accounts in the scope).
    #[arg(long)]
    account: Option<String>,
    /// Output JSONL path. Defaults to
    /// `<storage root>/channel_assist_fixtures/fixtures-<date>.jsonl`
    /// (outside the repo — fixture files must never be committed).
    #[arg(long)]
    out: Option<std::path::PathBuf>,
    /// Scope principal.
    #[arg(long, default_value = "anonymous")]
    principal: String,
    /// Scope workspace.
    #[arg(long, default_value = "default")]
    workspace: String,
}

#[derive(Subcommand, Debug)]
enum AnalyticsCommand {
    /// Open a DuckDB analytics DB and `EXPORT DATABASE` it to a portable Parquet
    /// directory. On open failure it surfaces the full DuckDB error (so it also
    /// diagnoses incompatible-version vs. corruption).
    Export(AnalyticsExportArgs),
    /// Create/open a DuckDB analytics DB and `IMPORT DATABASE` a directory
    /// previously produced by `export`.
    Import(AnalyticsImportArgs),
    /// Corrective reprice of historical `llm_calls` Parquet rows: recompute
    /// each row's `cost_usd` at its own timestamp against the ACTIVE
    /// effective-dated pricing table (llm_pricing.json layered over the
    /// built-in base) and atomically rewrite only files whose costs changed.
    /// DRY RUN by default — pass --apply to rewrite. Run with
    /// `MAGICIAN_SKIP_KEYCHAIN=1` (no secrets needed).
    RepriceLlmCalls(AnalyticsRepriceArgs),
}

#[derive(clap::Args, Debug, Clone)]
struct AnalyticsExportArgs {
    /// Path to the source analytics DuckDB file (e.g. an
    /// `analytics.duckdb.incompatible-<ts>` quarantined file).
    #[arg(long)]
    db: std::path::PathBuf,
    /// Output directory for the EXPORT DATABASE dump (created if absent).
    #[arg(long)]
    out: std::path::PathBuf,
}

#[derive(clap::Args, Debug, Clone)]
struct AnalyticsImportArgs {
    /// Directory previously produced by `analytics export`.
    #[arg(long = "in")]
    input: std::path::PathBuf,
    /// Destination analytics DuckDB file (created if absent; should be empty/new).
    #[arg(long)]
    db: std::path::PathBuf,
}

#[derive(clap::Args, Debug, Clone)]
struct AnalyticsRepriceArgs {
    /// Rewrite changed Parquet files in place. Without this flag the command
    /// is a dry run: it computes and reports every change, modifies nothing.
    #[arg(long)]
    apply: bool,
    /// Only reprice this principal (default: every scope under the storage root).
    #[arg(long)]
    principal: Option<String>,
    /// Only reprice this workspace (default: every workspace of the selected
    /// principals).
    #[arg(long)]
    workspace: Option<String>,
    /// Inclusive lower partition-date bound (matches `dt=YYYY-MM-DD` dirs).
    #[arg(long)]
    from: Option<String>,
    /// Inclusive upper partition-date bound (matches `dt=YYYY-MM-DD` dirs).
    #[arg(long)]
    to: Option<String>,
    /// Also reprice today's partition. Skipped by default because the live
    /// sink appends new files there while the sweep runs.
    #[arg(long)]
    include_today: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct DistillEvidenceArgs {
    /// Agent id whose recent episodes to distill.
    #[arg(long)]
    agent: String,

    /// Number of most-recent episodes to distill.
    #[arg(long, default_value_t = 20)]
    limit: usize,

    /// Principal scope.
    #[arg(long, default_value = "anonymous")]
    principal: String,

    /// Workspace scope.
    #[arg(long, default_value = "default")]
    workspace: String,

    /// Persist promoted + salient records to the agent's evidence store
    /// (default is read-only — print candidates only).
    #[arg(long)]
    persist: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct ReviewArgs {
    /// Agent id whose evidence to summarize.
    #[arg(long)]
    agent: String,

    /// Look-back window in days — the adjustable review period (a UI slider
    /// binds to this; there is no fixed weekly cadence).
    #[arg(long, default_value_t = 7)]
    days: i64,

    /// Facet to scope the review to (e.g. "work"); use "all" for no filter.
    #[arg(long, default_value = "work")]
    facet: String,

    /// Principal scope.
    #[arg(long, default_value = "anonymous")]
    principal: String,

    /// Workspace scope.
    #[arg(long, default_value = "default")]
    workspace: String,
}

#[derive(clap::Args, Debug, Clone)]
struct EvidenceEvalArgs {
    /// Agent id whose task-linked evidence to include (user-owned ambient
    /// evidence is always included).
    #[arg(long, default_value = "personal-assistant")]
    agent: String,
    /// Principal scope.
    #[arg(long, default_value = "anonymous")]
    principal: String,
    /// Workspace scope.
    #[arg(long, default_value = "default")]
    workspace: String,
}

#[derive(clap::Args, Debug, Clone)]
struct EvidenceGraphArgs {
    /// Entity key to center a neighborhood view on; omit for top co-occurrence edges.
    #[arg(long)]
    entity: Option<String>,
    /// Facet filter ("all" = no filter).
    #[arg(long, default_value = "all")]
    facet: String,
    /// Minimum co-occurrence weight for the edges view.
    #[arg(long, default_value_t = 2)]
    min_weight: usize,
    /// Agent id whose task-linked evidence to include (user-owned always included).
    #[arg(long, default_value = "personal-assistant")]
    agent: String,
    /// Principal scope.
    #[arg(long, default_value = "anonymous")]
    principal: String,
    /// Workspace scope.
    #[arg(long, default_value = "default")]
    workspace: String,
}

#[derive(clap::Args, Debug, Clone)]
struct EvidenceClaimsArgs {
    /// Agent id whose task-linked evidence to include (user-owned always included).
    #[arg(long, default_value = "personal-assistant")]
    agent: String,
    /// Look-back window in days.
    #[arg(long, default_value_t = 7)]
    days: i64,
    /// Facet to scope claims to ("all" = no filter).
    #[arg(long, default_value = "all")]
    facet: String,
    /// Principal scope.
    #[arg(long, default_value = "anonymous")]
    principal: String,
    /// Workspace scope.
    #[arg(long, default_value = "default")]
    workspace: String,
}

#[derive(clap::Args, Debug, Clone)]
struct EvidencePrecisionArgs {
    /// Agent id whose task-linked evidence to grade.
    #[arg(long, default_value = "personal-assistant")]
    agent: String,
    /// Max records to grade (one LLM call each).
    #[arg(long, default_value_t = 10)]
    sample: usize,
    /// Principal scope.
    #[arg(long, default_value = "anonymous")]
    principal: String,
    /// Workspace scope.
    #[arg(long, default_value = "default")]
    workspace: String,
}

#[derive(clap::Args, Debug, Clone)]
struct WorklogExportArgs {
    /// Agent id whose work ledger to export. Omit to export every agent in the
    /// scope (enumerated from the scoped agent-definition store).
    #[arg(long)]
    agent: Option<String>,
    /// Output directory for the rendered per-agent worklogs (created if absent;
    /// existing files are overwritten). Relative paths resolve from the CWD.
    #[arg(long, default_value = "docs/worklog")]
    out_dir: std::path::PathBuf,
    /// Principal scope.
    #[arg(long, default_value = "anonymous")]
    principal: String,
    /// Workspace scope.
    #[arg(long, default_value = "default")]
    workspace: String,
}

#[derive(Subcommand, Debug)]
enum MemoryIndexCommand {
    /// Print the scoped memory index freshness status.
    Status(MemoryIndexScopeArgs),
    /// Rebuild the scoped memory index from canonical memory JSON.
    Rebuild(MemoryIndexRebuildArgs),
    /// Compact and optimize the scoped LanceDB memory index.
    Optimize(MemoryIndexScopeArgs),
}

#[derive(clap::Args, Debug, Clone)]
struct MemoryIndexScopeArgs {
    /// Principal scope to inspect or rebuild.
    #[arg(long, default_value = "anonymous")]
    principal: String,

    /// Workspace scope to inspect or rebuild.
    #[arg(long, default_value = "default")]
    workspace: String,

    /// Emit machine-readable JSON instead of a compact human summary.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct MemoryIndexRebuildArgs {
    #[command(flatten)]
    scope: MemoryIndexScopeArgs,

    /// Exit without rebuilding when the current manifest is already fresh.
    #[arg(long)]
    skip_if_fresh: bool,

    /// Rebuild even when --skip-if-fresh would skip a fresh or soft-stale index.
    #[arg(long, alias = "hard")]
    force: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum FrontendModeFlag {
    /// Detect frontend delivery from CLI flags or configuration (default)
    Auto,
    /// API only — no frontend serving
    ApiOnly,
    /// Serve frontend assets from a local directory
    Static,
}

#[derive(Clone, Debug)]
enum FrontendDelivery {
    ApiOnly,
    Static(Arc<PathBuf>),
}

async fn log_http_requests<B>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<B>, actix_web::Error>
where
    B: MessageBody + 'static,
{
    let path = req.path().to_owned();
    let method = req.method().clone();
    let connection = req.connection_info().clone();
    let peer = connection
        .realip_remote_addr()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "-".to_string());
    let start = std::time::Instant::now();

    let res = next.call(req).await?;

    let status = res.status();
    let elapsed_ms = start.elapsed().as_millis();
    debug!(
        method = %method,
        path = %path,
        status = %status.as_u16(),
        remote = %peer,
        elapsed_ms = %elapsed_ms,
        "HTTP request handled"
    );

    Ok(res)
}

fn channel_assist_scope_for_prefix(prefix: &'static str) -> actix_web::Scope {
    use magician_api::attention_learning_api;
    use magician_api::channel_assist_api;
    use magician_api::resurfacing_api;

    let scope = web::scope(prefix)
        // Sync status + manual one-pass trigger.
        .route(
            "/sync/status",
            web::get().to(channel_assist_api::get_channel_assist_sync_status_handler),
        )
        .route(
            "/sync/run",
            web::post().to(channel_assist_api::post_channel_assist_sync_run_handler),
        )
        // Pipeline observability and live distillation feed.
        .route(
            "/stats",
            web::get().to(channel_assist_api::get_channel_assist_stats_handler),
        )
        .route(
            "/distill/recent",
            web::get().to(channel_assist_api::get_channel_assist_distill_recent_handler),
        )
        .route(
            "/distill/backfill",
            web::post().to(channel_assist_api::post_channel_assist_distill_backfill_handler),
        )
        // Channel-toggle UI: discovered accounts + registry state.
        .route(
            "/channels",
            web::get().to(channel_assist_api::get_channel_assist_channels_handler),
        )
        .route(
            "/channels",
            web::put().to(channel_assist_api::put_channel_assist_channels_handler),
        )
        // Secure HITL P6: the verification-code purpose on one account.
        .route(
            "/channels/purpose",
            web::put().to(magician_api::verification_codes_api::put_channel_purpose_handler),
        )
        // Annotation reads and user actions.
        .route(
            "/annotations",
            web::get().to(channel_assist_api::get_channel_assist_annotations_handler),
        )
        .route(
            "/annotations/seed",
            web::post().to(channel_assist_api::post_channel_assist_annotation_seed_handler),
        )
        .route(
            "/annotations/{id}/dismiss",
            web::post().to(channel_assist_api::post_channel_assist_annotation_dismiss_handler),
        )
        .route(
            "/annotations/{id}/feedback",
            web::post().to(channel_assist_api::post_channel_assist_annotation_feedback_handler),
        )
        .route(
            "/annotations/{id}/writing-preferences",
            web::get().to(channel_assist_api::get_channel_assist_writing_preferences_handler),
        )
        .route(
            "/annotations/{id}/writing-preferences",
            web::post().to(channel_assist_api::post_channel_assist_writing_preferences_handler),
        )
        .route(
            "/writing-preferences/{id}/promote",
            web::post()
                .to(channel_assist_api::post_channel_assist_writing_preference_promote_handler),
        )
        .route(
            "/writing-preferences/{id}/dismiss",
            web::post()
                .to(channel_assist_api::post_channel_assist_writing_preference_dismiss_handler),
        )
        .route(
            "/needs-you",
            web::get().to(channel_assist_api::get_channel_assist_needs_you_handler),
        )
        .route(
            "/follow-ups",
            web::get().to(channel_assist_api::get_channel_assist_needs_you_handler),
        )
        .route(
            "/follow-ups/groups/{cluster_id}/members",
            web::get().to(channel_assist_api::get_follow_up_group_members_handler),
        )
        .route(
            "/attention-learning/pair-corrections",
            web::post().to(channel_assist_api::post_attention_pair_correction_handler),
        )
        .route(
            "/attention-learning/impressions",
            web::post().to(attention_learning_api::post_attention_impression_handler),
        )
        .route(
            "/attention-learning/decisions/{decision_id}",
            web::get().to(attention_learning_api::get_attention_decision_handler),
        )
        .route(
            "/attention-learning/canonical-projection",
            web::get().to(
                magician_api::canonical_attention_api::get_canonical_attention_projection_handler,
            ),
        )
        .route(
            "/attention-learning/canonical-deliveries/{lane}",
            web::get().to(
                magician_api::canonical_attention_api::get_canonical_attention_delivery_handler,
            ),
        )
        .route(
            "/attention-learning/delivery-health",
            web::get()
                .to(magician_api::canonical_attention_api::get_attention_delivery_health_handler),
        )
        .route(
            "/attention-learning/rank-recompute/jobs/{job_id}",
            web::get().to(attention_learning_api::get_attention_rank_recompute_job_handler),
        )
        .route(
            "/attention-learning/rank-recompute/status",
            web::get().to(attention_learning_api::get_attention_rank_recompute_status_handler),
        )
        .route(
            "/attention-learning/rank-recompute/schedule",
            web::post().to(attention_learning_api::post_attention_rank_recompute_schedule_handler),
        )
        .route(
            "/attention-learning/rank-recompute/requeue",
            web::post().to(attention_learning_api::post_attention_rank_recompute_requeue_handler),
        )
        .route(
            "/attention-learning/rank-recompute/process",
            web::post().to(attention_learning_api::post_attention_rank_recompute_process_handler),
        )
        .route(
            "/attention-learning/historical-bootstrap/status",
            web::get()
                .to(attention_learning_api::get_attention_historical_bootstrap_status_handler),
        )
        .route(
            "/attention-learning/semantic-extraction/status",
            web::get().to(attention_learning_api::get_semantic_extraction_health_handler),
        )
        .route(
            "/attention-learning/semantic-extraction/enqueue",
            web::post().to(attention_learning_api::post_semantic_extraction_enqueue_handler),
        )
        .route(
            "/attention-learning/actionability-training/status",
            web::get().to(attention_learning_api::get_actionability_training_status_handler),
        )
        .route(
            "/attention-learning/actionability-training/run",
            web::post().to(attention_learning_api::post_actionability_training_run_handler),
        )
        .route(
            "/attention-learning/routing-training/status",
            web::get().to(attention_learning_api::get_routing_training_status_handler),
        )
        .route(
            "/attention-learning/routing-training/run",
            web::post().to(attention_learning_api::post_routing_training_run_handler),
        )
        .route(
            "/annotations/{id}/approve",
            web::post().to(channel_assist_api::post_channel_assist_annotation_approve_handler),
        )
        .route(
            "/annotations/{id}/snooze",
            web::post().to(channel_assist_api::post_channel_assist_annotation_snooze_handler),
        )
        .route(
            "/annotations/{id}/action/{action_id}/compose",
            web::post().to(channel_assist_api::post_channel_assist_action_compose_handler),
        )
        .route(
            "/annotations/{id}/action/{action_id}/commit",
            web::post().to(channel_assist_api::post_channel_assist_action_commit_handler),
        )
        .route(
            "/annotations/{id}/review",
            web::post().to(channel_assist_api::post_channel_assist_annotation_review_handler),
        )
        .route(
            "/annotations/{id}/acknowledge",
            web::post().to(channel_assist_api::post_channel_assist_annotation_acknowledge_handler),
        )
        .route(
            "/annotations/{id}/useful",
            web::post().to(channel_assist_api::post_channel_assist_annotation_useful_handler),
        )
        .route(
            "/annotations/{id}/message",
            web::get().to(channel_assist_api::get_channel_assist_message_handler),
        )
        // Proactive resurfacing cards and action feedback.
        .route(
            "/resurfacing/today",
            web::get().to(resurfacing_api::get_resurfacing_today_handler),
        )
        .route(
            "/resurfacing/groups/{cluster_id}/members",
            web::get().to(resurfacing_api::get_resurfacing_group_members_handler),
        )
        .route(
            "/resurfacing/repair",
            web::post().to(resurfacing_api::post_resurfacing_active_repair_handler),
        )
        .route(
            "/resurfacing/{candidate_id}/detail",
            web::get().to(resurfacing_api::get_resurfacing_detail_handler),
        )
        .route(
            "/resurfacing/{candidate_id}/original",
            web::get().to(resurfacing_api::get_resurfacing_original_handler),
        )
        .route(
            "/resurfacing/{candidate_id}/recommendation-event",
            web::post().to(resurfacing_api::post_resurfacing_recommendation_event_handler),
        )
        .route(
            "/resurfacing/{candidate_id}/actions",
            web::post().to(resurfacing_api::post_resurfacing_contextual_action_handler),
        )
        // Same contextual-action contract as the Worth-a-look lane, so a client
        // that can already run one can run the other. Only the source-independent
        // kinds resolve here; the service rejects the rest.
        .route(
            "/follow-ups/{annotation_id}/actions",
            web::post().to(channel_assist_api::post_channel_follow_up_contextual_action_handler),
        )
        .route(
            "/resurfacing/{candidate_id}/action",
            web::post().to(resurfacing_api::post_resurfacing_action_handler),
        )
        .route(
            "/resurfacing/stats",
            web::get().to(resurfacing_api::get_resurfacing_stats_handler),
        )
        .route(
            "/resurfacing/observability",
            web::get().to(resurfacing_api::get_resurfacing_observability_handler),
        );

    scope
}

fn configure_channel_assist_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(channel_assist_scope_for_prefix("/channel-assist"));
}

fn json_payload_error_handler_with_limit(err: JsonPayloadError, limit: usize) -> actix_web::Error {
    use actix_web::http::StatusCode;

    let (status, error_message) = match &err {
        JsonPayloadError::OverflowKnownLength { .. } | JsonPayloadError::Overflow { .. } => (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("JSON payload exceeds {limit} bytes"),
        ),
        JsonPayloadError::ContentType => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json".to_string(),
        ),
        JsonPayloadError::Deserialize(_) => {
            (StatusCode::BAD_REQUEST, "Invalid JSON payload".to_string())
        },
        _ => (StatusCode::BAD_REQUEST, "Invalid JSON payload".to_string()),
    };

    let response = HttpResponse::build(status).json(serde_json::json!({
        "code": "invalid_request_payload",
        "error": "Invalid request payload",
        "details": { "reason": error_message },
    }));

    actix_web::error::InternalError::from_response(err, response).into()
}

/// `GET /health/execution-driver` — which loop driver this process is taking,
/// and how many runs each arm has served since it started.
///
/// # The canary the stateless cutover is staged behind
///
/// `docs/archive/plans/2026-08-25-stateless-loop-design.md` requires the flip to
/// `MAGICIAN_EXECUTION_DRIVER=stateless` to be staged behind live canaries, and
/// recorded that the canary was **unreadable**: `driver_run_counts()` existed
/// and nothing called it, so an operator who set the variable and watched had
/// no way to tell whether any run actually took the arm. The only evidence was
/// `[AGENTIC-DRIVER]` log lines. A canary you cannot read is a deploy.
///
/// # It lives in the binary, and that is the dependency direction
///
/// The counters are in `magician`; `/health` is served by a handler in
/// `magician_api`, and **`magician` depends on `magician_api`, not the other
/// way round**. So the aggregated health handler cannot reach the counters, and
/// this is a sibling route rather than three new fields on that response. The
/// binary is the one layer that composes both.
///
/// Deliberately its own path rather than an addition to `/health`: existing
/// consumers of that response (the iOS health pill, the desktop tray's
/// aggregator, the unified-ui readiness probe) parse fixed fields, and a
/// cutover dial is not liveness.
///
/// `resolved_driver` is what THIS process would give a run starting now. Note
/// that a process which has served runs on both arms is not confused: the
/// variable is resolved once per run, so runs in flight keep the arm they
/// started on and no run changes arm underneath itself.
///
/// **`ExecutionDriver::select` and not `from_env`, deliberately.** `from_env`
/// logs at `error` when the variable names a driver this build does not know,
/// which is right for a once-per-run call and wrong for an endpoint a monitor
/// polls: a persistent misconfiguration would emit a line per poll forever.
/// `select` is the same resolution without the logging, so the misconfiguration
/// is REPORTED as the `unrecognised` field instead — which is what a health
/// surface is for.
async fn execution_driver_health() -> actix_web::HttpResponse {
    use magician::magician_v2::execution::agentic::run_loop::{
        driver_run_counts, ExecutionDriver, EXECUTION_DRIVER_ENV,
    };

    let (inprocess_runs, stateless_runs) = driver_run_counts();
    let raw = std::env::var(EXECUTION_DRIVER_ENV).ok();
    let selection = ExecutionDriver::select(raw.as_deref());

    actix_web::HttpResponse::Ok().json(serde_json::json!({
        "variable": EXECUTION_DRIVER_ENV,
        "resolved_driver": match selection.driver {
            ExecutionDriver::Inprocess => ExecutionDriver::INPROCESS,
            ExecutionDriver::Stateless => ExecutionDriver::STATELESS,
        },
        // `null` unless the variable names something this build does not know.
        // Unknown values fail toward the shipped stateless default while this
        // field keeps the operator-visible configuration error explicit.
        "unrecognised": selection.unrecognised,
        "runs_since_start": {
            "inprocess": inprocess_runs,
            "stateless": stateless_runs,
        },
        // The number an operator actually watches during a canary: zero here
        // with the variable set to `stateless` means the arm is not being taken
        // and the rollout has not started, which is the failure the design doc
        // says was invisible.
        "total_runs_since_start": inprocess_runs.saturating_add(stateless_runs),
    }))
}

/// `GET /health/storage` — selected storage profile, adapter health, and
/// held scope-lease generations. Sibling of `/health` so existing liveness
/// consumers keep a fixed schema.
async fn storage_health(
    runtime: web::Data<Arc<magician_storage::StorageRuntime>>,
) -> actix_web::HttpResponse {
    let snapshot = runtime.operator_health();
    if snapshot.lease_lost {
        actix_web::HttpResponse::ServiceUnavailable().json(snapshot)
    } else {
        actix_web::HttpResponse::Ok().json(snapshot)
    }
}

fn json_payload_error_handler(err: JsonPayloadError, _req: &HttpRequest) -> actix_web::Error {
    json_payload_error_handler_with_limit(err, API_JSON_PAYLOAD_LIMIT_BYTES)
}

fn contextual_writing_json_payload_error_handler(
    err: JsonPayloadError,
    _req: &HttpRequest,
) -> actix_web::Error {
    json_payload_error_handler_with_limit(err, CONTEXTUAL_WRITING_JSON_PAYLOAD_LIMIT_BYTES)
}

fn register_builtin_chunk_adapters_for_boot() -> Result<()> {
    magician_chunking::register_builtin_chunk_adapters()
        .context("Failed to register built-in logical-context chunk adapters")
}

fn resolve_storage_profile(cli: &Cli) -> Result<magician_storage::ResolvedStorageProfile> {
    let env_path = std::env::var_os(magician_storage::BOOTSTRAP_ENV).map(PathBuf::from);
    magician_storage::resolve(magician_storage::ResolveOptions {
        cli_path: cli.storage_bootstrap.as_deref(),
        env_path: env_path.as_deref(),
        allow_ambient: false,
    })
    .map_err(Into::into)
}

/// Finish the teardown of app jails a previous Magician process left behind
/// (it died before proving every jailed process dead). macOS exec-roots jails
/// only; elsewhere this counts nothing. A kept sentinel means a jail's owner
/// could not be read, so its processes may still be running.
fn sweep_stale_app_jail_members() {
    // A process killed mid-call can leave a key file in its private
    // credential directory; nothing is running yet, so every one is stale.
    let credential_roots =
        magician::magician_v2::apps::os_jail_in_place::sweep_stale_credential_roots();
    if credential_roots > 0 {
        tracing::warn!(
            credential_roots,
            "[APP-JAIL] removed private credential directories left by a previous process"
        );
    }
    let sweep = tool_runtime_core::governed_process_jail::sweep_stale_jail_members();
    if sweep.sentinels_kept > 0 {
        tracing::warn!(
            sentinels_removed = sweep.sentinels_removed,
            sentinels_kept = sweep.sentinels_kept,
            members_killed = sweep.members_killed,
            "[APP-JAIL] stale jail sweep kept sentinels whose owner could not be read; \
             their processes may still be running"
        );
    } else {
        tracing::info!(
            sentinels_removed = sweep.sentinels_removed,
            members_killed = sweep.members_killed,
            "[APP-JAIL] stale jail sweep finished"
        );
    }
}

fn main() -> Result<()> {
    if magician_apps::apps::surface_worker::maybe_run_surface_worker() {
        return Ok(());
    }
    let cli = Cli::parse();
    // Before config load, router construction, or any credential read — and on
    // every path, server included. `dotenvy` does not overwrite what the
    // environment already carries, so an operator export still wins.
    load_runtime_env_files();
    let app_json_mode = matches!(&cli.command, Some(CliCommand::App { json: true, .. }));
    // A machine-readable App command owns stdout completely. Skipping tracing
    // here prevents the default formatting layer from interleaving boot logs
    // with its single JSON envelope.
    if !app_json_mode {
        init_tracing(&cli.log_level)?;
    }
    // App authoring is a standalone provider-free path. Dispatch it before
    // constructing either Tokio runtime, loading live config/pricing, or
    // starting service workers so it remains usable in a clean package tree.
    if let Some(CliCommand::App { json, command }) = &cli.command {
        if !command.requires_live_workspace() {
            let status =
                magician::magician_v2::apps::authoring::run_app_authoring_command_with_output(
                    command, *json,
                )?;
            return finish_app_authoring_status(status);
        }
        register_builtin_chunk_adapters_for_boot()?;
        let storage_profile = resolve_storage_profile(&cli)?;
        if matches!(
            storage_profile.kind,
            magician_storage::ProfileKind::RemoteDurable
        ) {
            anyhow::bail!(
                "remote durable storage adapters are not installed; use local_embedded until Task 6"
            );
        }
        return run_live_app_before_boot(command, *json);
    }

    // Live-runtime paths load and resolve the boot plan before constructing
    // main, execution, or Lance runtimes. Provider-free app authoring above
    // stays config-free.
    register_builtin_chunk_adapters_for_boot()?;
    let storage_profile = resolve_storage_profile(&cli)?;
    info!(
        profile = ?storage_profile.kind,
        source = ?storage_profile.source,
        "resolved storage profile before opening stores"
    );
    let mut magician_config = load_magician_config(&cli.config)?;
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let env_profile = std::env::var("MAGICIAN_SCALE_PROFILE").ok();
    let env_profile = env_profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let runtime_plan = resolve_boot_runtime_plan(
        magician_config.runtime.scale.profile,
        &magician_config.runtime.scale.overrides,
        cpu_count,
        env_profile,
        LeftoverDispatchScalars::from(&magician_config.llm.dispatch),
    )?;
    runtime_plan.apply_to_dispatch(&mut magician_config.llm.dispatch);
    magician::magician_v2::local_resource_governor::configure_live_agent_limit(
        runtime_plan.live_agent_limit,
    );
    magician::magician_v2::configure_lance_search_concurrency(runtime_plan.lance_search_cost_units);
    magician::magician_v2::configure_blocking_admission(runtime_plan.blocking_admission_permits);
    info!(
        yaml_profile = runtime_plan.yaml_profile.as_str(),
        selected_profile = runtime_plan.selected_profile.as_str(),
        env_profile = runtime_plan
            .env_profile
            .map(magician::runtime_plan::ScaleProfile::as_str)
            .unwrap_or(""),
        cpu_count = runtime_plan.cpu_count,
        main_worker_threads = runtime_plan.main_worker_threads,
        http_worker_threads = runtime_plan.http_worker_threads,
        execution_worker_threads = runtime_plan.execution_worker_threads,
        background_worker_threads = runtime_plan.background_worker_threads,
        lance_worker_threads = runtime_plan.lance_worker_threads,
        lance_search_cost_units = runtime_plan.lance_search_cost_units,
        blocking_admission_permits = runtime_plan.blocking_admission_permits,
        live_agent_limit = runtime_plan.live_agent_limit,
        dispatch_workers = runtime_plan.workers,
        reserved_interactive_workers = runtime_plan.reserved_interactive_workers,
        global_cloud_concurrency = runtime_plan.global_cloud_concurrency,
        queue_capacity_high = runtime_plan.queue_capacity_high,
        queue_capacity_normal = runtime_plan.queue_capacity_normal,
        queue_capacity_background = runtime_plan.queue_capacity_background,
        "resolved effective runtime plan before constructing runtimes"
    );

    // Full agentic loops use a separate, explicitly named runtime. Keep it owned
    // by `main` for the lifetime of the application; `run` registers only its
    // handle with execution producers. Both runtimes intentionally keep their
    // normal Tokio stack policy: the executor API owns its large state machine on
    // the heap instead of making process threads compensate for its type size.
    let execution_runtime =
        magician::magician_v2::execution::runtime_boundary::build_execution_runtime_with_threads(
            runtime_plan.execution_worker_threads,
        )?;
    let execution_runtime_handle = execution_runtime.handle().clone();
    // Owned here for process lifetime; `run` only registers the handle. Ambient
    // mode skips the extra worker pool (`MAGICIAN_LANCE_RUNTIME=ambient`).
    let lance_runtime = if magician::magician_v2::should_build_lance_runtime() {
        Some(magician::magician_v2::build_lance_runtime_with_threads(
            runtime_plan.lance_worker_threads,
        )?)
    } else {
        None
    };
    let lance_runtime_handle = lance_runtime
        .as_ref()
        .map(|runtime| runtime.handle().clone());

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(runtime_plan.main_worker_threads)
        .enable_all()
        .build()?
        .block_on(async move {
            let result = run(
                cli,
                execution_runtime_handle,
                lance_runtime_handle,
                magician_config,
                runtime_plan,
                storage_profile,
            )
            .await;
            // Also reap a child when initialization fails before the normal HTTP
            // shutdown sequence has been constructed.
            magician::magician_v2::runtime::ollama_lifecycle::stop().await;
            result
        })
}

fn run_live_app_before_boot(
    command: &magician::magician_v2::apps::authoring::AppAuthoringCommand,
    json: bool,
) -> Result<()> {
    let attempt = (|| -> Result<_> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(run_live_app_command(command, json))
    })();
    let status = match attempt {
        Ok(status) => status,
        Err(error) => magician::magician_v2::apps::authoring::emit_app_authoring_result(
            command,
            json,
            Err(error),
        )?,
    };
    finish_app_authoring_status(status)
}

async fn run(
    cli: Cli,
    execution_runtime_handle: tokio::runtime::Handle,
    lance_runtime_handle: Option<tokio::runtime::Handle>,
    magician_config: MagicianConfig,
    runtime_plan: EffectiveRuntimePlan,
    storage_profile: magician_storage::ResolvedStorageProfile,
) -> Result<()> {
    // Register after tracing is initialized so the startup record proves which
    // worker pool owns agentic execution in this process.
    // This still happens before any execution producer or HTTP worker starts.
    magician::magician_v2::execution::runtime_boundary::set_execution_runtime_handle(
        execution_runtime_handle,
    );
    if let Some(lance_runtime_handle) = lance_runtime_handle {
        magician::magician_v2::set_lance_runtime_handle(lance_runtime_handle);
    }
    magician::magician_v2::learning::start_procedure_index_maintainer();
    // Install the effective-dated LLM pricing table (llm_pricing.json over
    // the built-in base) IMMEDIATELY after main config load: the pricing
    // OnceLock in `magicllm::active_table()` silently locks in the
    // builtin-only table on first use, so this must precede every path that
    // can price a call — service/builder construction below AND the CLI
    // command branch (analytics repricing computes costs too). Nothing
    // earlier in this function (tracing init, config parse/validation)
    // touches the pricing table.
    magician::magician_v2::llm_pricing_config::load_and_install_llm_pricing();
    magician::magician_v2::agents::configure_memory_prompt_budgets(&magician_config.memory);
    // Coding budgets are read by the executor's watchdog, which holds no config
    // snapshot; publish them before anything can run.
    magician::magician_v2::execution::coding_engine::configure_coding_budgets(
        &magician_config.coding,
    );
    // Outward posture, published BEFORE the CLI branch below and before any
    // server start: the executor's capture gate holds no config snapshot, and
    // an unpublished posture defaults to capture. Installing it here means the
    // only way to send is a deliberate config edit, not an initialisation order
    // nobody noticed. Readiness review §9 steps 2 and 4.
    magician::magician_v2::agents::outward_actions::install_outward_capture_posture(
        magician_config.outward_actions.capture_only,
    );
    if magician_config.outward_actions.capture_only {
        tracing::info!(
            "[OUTWARD-CAPTURE] outward actions are CAPTURED, not sent \
             (outward_actions.capture_only = true)"
        );
    } else {
        tracing::warn!(
            "[OUTWARD-LIVE] outward actions will be SENT FOR REAL \
             (outward_actions.capture_only = false)"
        );
    }
    let storage_workspace = resolve_storage_workspace(&magician_config)?;
    let acquire_default_lease = cli.command.is_none() && !cli.reindex;
    let storage_runtime =
        install_process_storage_runtime(&storage_workspace, storage_profile, acquire_default_lease)
            .await?;
    if let Some(command) = &cli.command {
        return run_cli_command(&magician_config, command).await;
    }
    if cli.reindex {
        return run_list_index_reindex(&magician_config).await;
    }
    sweep_stale_app_jail_members();
    // Workspaces deleted with `?purge=true` lose their directory here: server
    // path only (a CLI command can run beside a live server), after the
    // runtime lease is held, and before any store below discovers scopes —
    // every boot sweep enumerates directories, so nothing can resurrect one or
    // hold a handle into it yet.
    for outcome in magician::magician_v2::auth::workspace_registry::drain_pending_purges(
        &storage_workspace.base_root().join("scopes"),
    ) {
        use magician::magician_v2::auth::workspace_registry::PurgeOutcome;
        match outcome {
            PurgeOutcome::Removed {
                principal,
                workspace,
            } => {
                info!(principal = %principal, workspace = %workspace, "purged a deleted workspace's data");
            },
            PurgeOutcome::Skipped {
                principal,
                workspace,
                reason,
            } => {
                tracing::warn!(principal = %principal, workspace = %workspace, reason, "skipped a queued workspace purge");
            },
            PurgeOutcome::Failed {
                principal,
                workspace,
                error,
            } => {
                tracing::error!(principal = %principal, workspace = %workspace, error = %error, "workspace purge failed; retrying at next start");
            },
        }
    }
    let frontend_delivery = resolve_frontend_delivery(&cli, &magician_config)?;
    let startup_http = startup_http::StartupHttp::start(
        &cli.host,
        cli.port,
        runtime_plan.http_worker_threads,
        match &frontend_delivery {
            FrontendDelivery::Static(dir) => Some(Arc::clone(dir)),
            FrontendDelivery::ApiOnly => None,
        },
    )
    .context("starting bootstrap HTTP listener")?;
    magician::magician_v2::runtime::ollama_lifecycle::start_background();
    magician::magician_v2::execution::coding_engine::qualify_worker::spawn_codex_qualify_worker();
    magician::magician_v2::execution::coding_engine::qualify_worker::spawn_grok_version_worker();
    magician::magician_v2::execution::coding_engine::qualify_worker::spawn_claude_version_worker();
    magician::magician_v2::execution::coding_engine::qualify_worker::spawn_agy_version_worker();

    info!("🎩 Starting Magician V2 service binary");

    // Resolve extra "system-root" directories declared by
    // `tool-runtime-config.yaml :: registry.paths`. Each entry overlays
    // skills (loaded later in `build_tool_services` + per-scope
    // `load_pack_defs_for_scope`) and agent personality templates
    // (consumed via `config_extras::extra_agent_template_dirs` from every
    // `AgentDefinitionStore::with_workspace_root` constructor).
    let extra_system_roots = load_extra_system_roots(&cli.config);
    magician::magician_v2::config_extras::set_extra_system_roots(extra_system_roots.clone());
    if !extra_system_roots.is_empty() {
        info!(
            count = extra_system_roots.len(),
            roots = ?extra_system_roots,
            "[STARTUP] extra system roots installed for skills + agent templates (from tool-runtime-config.yaml :: registry.paths)"
        );
    }

    let secret_runtime = magician::magician_v2::secrets::bootstrap_secret_runtime();
    if !secret_runtime
        .install_app_control_plane_signer()
        .map_err(anyhow::Error::msg)?
    {
        warn!(
            "OS-backed app control-plane sealing is unavailable; protected app workflows will remain disabled"
        );
    }
    let repo_root = cli
        .config
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    // Isolation: record the live repo source root so sandboxed shell actions
    // (native bash + delegated shells) refuse to mutate the user's real repo,
    // even when invoked far from any scope context. See the coding_engine
    // deny-fence (`reject_repo_source_tree` / `live_repo_source_fence`).
    magician::magician_v2::execution::coding_engine::set_live_repo_source_root(repo_root.clone());
    // Isolation: record the resolved scope-storage base so the deny-fence + OS
    // sandbox gate treat it as writable (it nests inside the repo) without
    // hardcoding `magician_data_v3` — correct for non-default `storage_path` too.
    {
        let storage = storage_workspace.base_root();
        let storage_base = if storage.is_absolute() {
            storage.to_path_buf()
        } else {
            repo_root.join(storage)
        };
        magician::magician_v2::execution::coding_engine::set_live_storage_base(storage_base);
    }

    // Seed the built-in Tutor drawing primitive recipes into the runtime root's
    // global `tutor_primitives/` folder. Create-if-missing, plus an overwrite
    // when the built-in carries a strictly higher `version` — never deletes,
    // and never touches an unversioned file, which is what a hand-written
    // recipe looks like. Source is the repo seed
    // `magician_data_v3/system/tutor_primitives/`.
    {
        let tutor_seed_dir = storage_workspace
            .system_seed_root()
            .join("tutor_primitives");
        let tutor_runtime_root = storage_workspace.base_root().to_path_buf();
        match magician::magician_v2::tutor::primitives::seed_builtin_recipes(
            &tutor_seed_dir,
            &tutor_runtime_root,
        ) {
            Ok(outcome) if outcome.touched_anything() => info!(
                created = outcome.created,
                upgraded = outcome.upgraded,
                dir = %tutor_runtime_root.join("tutor_primitives").display(),
                "[STARTUP] Seeded built-in tutor primitive recipes"
            ),
            Ok(_) => {},
            Err(error) => warn!(
                error = %error,
                seed_dir = %tutor_seed_dir.display(),
                "[STARTUP] Failed to seed tutor primitive recipes"
            ),
        }
    }

    // Skillshub project-local runtimes. The dispatcher prepends each of
    // these to every skill subprocess's PATH, so `python3` / `node` /
    // `npm` / `npx` resolve to the pinned project copies without
    // depending on host brew / nvm / fnm / pyenv. Warn at startup when
    // either is missing — affected skills fall back to whatever the
    // host PATH offers, which often lacks the required version or libs.
    let skillshub_venv_python = repo_root.join("skillshub/.venv/bin/python3");
    if skillshub_venv_python.is_file() {
        info!(path = %skillshub_venv_python.display(), "Skillshub Python venv found");
    } else {
        warn!(
            path = %skillshub_venv_python.display(),
            "Skillshub Python venv not found — Python-using skills (comic-strip, etc.) will use host `python3` and may lack Pillow / fpdf2 / google-genai. Run `make -C skillshub setup-python` to populate it."
        );
    }
    let skillshub_node = repo_root.join("skillshub/.node/bin/node");
    if skillshub_node.is_file() {
        info!(path = %skillshub_node.display(), "Skillshub Node runtime found");
    } else {
        warn!(
            path = %skillshub_node.display(),
            "Skillshub Node runtime not found — Node-using skills will fall back to host `node` and may run an unintended version. Run `make -C skillshub setup-node` to install the pinned Node 22 LTS."
        );
    }

    magician::magician_v2::runtime::device_transport::install(Arc::new(
        magician::magician_v2::runtime::device_transport::LocalLoopback::from_urls(
            magician_config.content_acquisition.browser.cdp_url.clone(),
            magician_config.runtime.ollama.embedding_base_url.clone(),
        ),
    ));
    {
        let renew = Arc::clone(&storage_runtime);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
            loop {
                interval.tick().await;
                if let Err(err) = renew.scope_leases.renew_all().await {
                    tracing::error!(error = %err, "storage scope lease renew failed");
                    break;
                }
            }
        });
    }

    let capability_workspace = Arc::new(
        magician::magician_v2::artifact_v2::CapabilityWorkspaceManager::new(
            storage_workspace,
            &repo_root,
        ),
    );
    capability_workspace.ensure_seeded_for_existing_scopes()?;

    let (
        tool_catalog,
        tool_matching,
        semantic_search,
        pack_defs,
        disk_pack_names,
        local_tool_services,
    ) = build_tool_services(
        &cli,
        Arc::clone(&capability_workspace),
        secret_runtime.capabilities(),
    )
    .await?;

    info!("🔧 Building Magician V2 service components");
    let magician_service = MagicianServiceBuilder::new(
        tool_catalog,
        tool_matching,
        semantic_search,
        pack_defs.clone(),
        repo_root.clone(),
        Some(capability_workspace.default_scope_paths()),
    )
    // NOTE: keep `repo_root` alive — it's threaded into `run_http_server`
    // below so the learning API can use it as the source-repo root.
    .with_secret_runtime_bootstrap(secret_runtime)
    .with_boot_config(magician_config.clone())
    .build()
    .await?;

    run_http_server(
        cli.host,
        cli.port,
        magician_service,
        frontend_delivery,
        local_tool_services,
        magician_config,
        capability_workspace,
        disk_pack_names,
        repo_root,
        runtime_plan,
        storage_runtime,
        startup_http,
    )
    .await
}

async fn build_tool_services(
    cli: &Cli,
    capability_workspace: Arc<magician::magician_v2::artifact_v2::CapabilityWorkspaceManager>,
    secret_capabilities: &magician::magician_v2::secrets::SecretRuntimeCapabilities,
) -> Result<(
    Arc<dyn ToolCatalog>,
    Arc<dyn ToolMatching>,
    Arc<dyn SemanticSearch>,
    Vec<CapabilityPackDefinition>,
    std::collections::HashSet<String>,
    Arc<LocalToolServices>,
)> {
    info!(
        "📦 Using local in-process tool backend (config: {})",
        cli.config.display()
    );

    // Pack catalog is layered: scope = canonical source, extras = config-
    // declared overlay that fills gaps, embedded = final fallback for the
    // 33 in-binary providers. The system-shared install tier was retired
    // (v0.6.572) — skills are workspace-scoped only. Sources are merged
    // in this order:
    //
    //   1. `<storage_root>/scopes/<default-scope>/skills/<skill>/tool_schema.yaml`
    //      — default-scope skills. Canonical source for everything the
    //      workspace explicitly installed via `make -C skillshub
    //      install-scope SCOPE=anonymous/default`.
    //   2. `<extra_path>/skills/<skill>/tool_schema.yaml` — each entry
    //      from `tool-runtime-config.yaml :: registry.paths`, in declared
    //      order. Fills any gap the scope didn't cover; later extras
    //      win over earlier extras, but scope wins over all extras.
    //   3. Embedded compiled pack defs (built into the binary via
    //      `include_str!`). The 33 internal capabilities that wrap
    //      in-process Rust providers (`create_agent`, `treasurer`,
    //      `list_agents`, `notify_owner`, …). Fill in any name a
    //      disk source didn't already provide.
    //
    // Per-turn `registry_for_scope` further overlays packs from any
    // non-default scope on top of this boot snapshot when chat / task
    // execution targets that scope.
    let default_scope_skills_dir = capability_workspace
        .default_scope_paths()
        .capabilities_root
        .join("skills");

    // 1. Start with the default-scope skills.
    let mut pack_defs = load_pack_defs_from_skills_dir(&default_scope_skills_dir);
    let scope_skills_loaded = pack_defs.len();
    // Retain origin independently from the parsed definition. Parsed equality
    // is not byte provenance: a disk override with reordered YAML keys can be
    // semantically equal to an embedded pack and must still not inherit the
    // embedded provider's Apps witness.
    let mut disk_pack_names: std::collections::HashSet<String> =
        pack_defs.iter().map(|pack| pack.name.clone()).collect();
    let mut name_to_index: std::collections::HashMap<String, usize> = pack_defs
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.clone(), i))
        .collect();

    // 2. Fall through to each extras root in declared order — extras
    //    fill any gap the scope didn't cover. First-listed extras wins
    //    over later-listed on collision (matches `SkillLoader::discover`'s
    //    `or_insert` + the PATH / XDG_DATA_DIRS convention). Scope wins
    //    over all extras.
    let mut extra_skills_loaded = 0usize;
    for extra_skills_dir in magician::magician_v2::config_extras::extra_skills_dirs() {
        for pack in load_pack_defs_from_skills_dir(&extra_skills_dir) {
            extra_skills_loaded += 1;
            disk_pack_names.insert(pack.name.clone());
            if name_to_index.contains_key(&pack.name) {
                continue;
            }
            name_to_index.insert(pack.name.clone(), pack_defs.len());
            pack_defs.push(pack);
        }
    }

    // 3. Fall back to embedded compiled defs for any name still missing.
    let mut embedded_loaded = 0usize;
    for pack in embedded_compiled_pack_defs() {
        if name_to_index.contains_key(&pack.name) {
            continue;
        }
        name_to_index.insert(pack.name.clone(), pack_defs.len());
        embedded_loaded += 1;
        pack_defs.push(pack);
    }

    tracing::info!(
        scope_skills_loaded,
        extra_skills_loaded,
        extra_roots = magician::magician_v2::config_extras::extra_system_roots().len(),
        default_scope_skills_dir = %default_scope_skills_dir.display(),
        embedded_loaded,
        total_packs = pack_defs.len(),
        "[STARTUP] capability pack defs loaded — layered: default-scope skills \
         → `registry.paths` extras (fill gaps; later extras win over earlier) \
         → embedded compiled defs"
    );
    // Prune compiled packs with no known Rust provider BEFORE generating
    // tool infos — ensures planner and executor catalogs stay in sync.
    prune_unexecutable_pack_defs(&mut pack_defs);
    // Prune packs that are compiled correctly but intentionally unavailable for
    // this host runtime (for example `treasurer` when durable vault storage is
    // disabled). The orchestrator consumes the same startup snapshot.
    prune_runtime_disabled_pack_defs(&mut pack_defs, secret_capabilities);
    let pack_tool_infos = pack_defs_to_tool_infos(&pack_defs);

    let local_tool_services = Arc::new(
        LocalToolServices::from_config_path(
            &cli.config,
            pack_tool_infos,
            Some(Arc::clone(&capability_workspace)),
            secret_capabilities.clone(),
        )
        .await?,
    );
    let tool_catalog: Arc<dyn ToolCatalog> = local_tool_services.clone();
    let tool_matching: Arc<dyn ToolMatching> = local_tool_services.clone();
    let semantic_search: Arc<dyn SemanticSearch> = local_tool_services.clone();
    Ok((
        tool_catalog,
        tool_matching,
        semantic_search,
        pack_defs,
        disk_pack_names,
        local_tool_services,
    ))
}

/// The filter this process runs at when nothing overrides it — and the filter
/// the runtime activity view runs at, always.
///
/// Quiet defaults for noisy third-party crates sit in front of the level, and
/// one credential cap sits behind it (directives resolve last-wins per target):
///
/// - LanceDB (the memory vector store) logs its per-index-part load traces
///   (`load_scalar_part`, inverted-index chatter) at INFO with no operational
///   signal, so `lance` / `lance_index` are pinned to WARN and genuine index
///   errors still surface.
/// - The official MCP SDK currently emits authorization codes and token
///   responses at DEBUG, so its OAuth target is hard-capped at INFO *after* the
///   caller's directives. That cap is a credential boundary and intentionally
///   cannot be lifted by `--log-level`.
const BASELINE_LOG_DIRECTIVES: &str = "lance=warn,lance_index=warn,info,rmcp::transport::auth=info";

/// One tracing filter, falling back to [`BASELINE_LOG_DIRECTIVES`] when the
/// operator's directives do not parse.
fn log_filter(directives: &str) -> Result<EnvFilter> {
    EnvFilter::try_new(directives)
        .or_else(|_| EnvFilter::try_new(BASELINE_LOG_DIRECTIVES))
        .context("invalid log level")
}

fn init_tracing(level: &str) -> Result<()> {
    // `--log-level` is the only operator input to logging in this process:
    // these are `EnvFilter::try_new` calls, which parse the string they are
    // handed and never consult `RUST_LOG`. The flag is reachable through the
    // supervisor too — `SupervisorConfig::magician_args` is a deserialized
    // config field that defaults to `--log-level info`.
    let base_level = if level.is_empty() { "info" } else { level };
    let operator_directives =
        format!("lance=warn,lance_index=warn,{base_level},rmcp::transport::auth=info");

    // Every layer carries its OWN filter (`Layer::with_filter`) rather than the
    // subscriber carrying one filter for all of them. That distinction is the
    // point, not a style preference:
    //
    //   * An `EnvFilter` added as a bare layer — `registry().with(env_filter)`
    //     — is a GLOBAL filter. `Layered::enabled` short-circuits the whole
    //     stack the moment one layer says no, so under `--log-level warn` the
    //     activity layer would simply never be shown the INFO work the runtime
    //     is doing. No error, no gap marker, just absence.
    //   * A per-layer filter only narrows the layer it wraps. A span or event
    //     is recorded when *any* layer's filter admits it, and skipped by the
    //     layers whose filter did not.
    //
    // So the console and the analytics events table follow the operator, and
    // the runtime activity view follows the code. How complete that view is, is
    // a property of this file — not of how somebody launched the process.
    //
    // The consequences, stated where the next person will hit them:
    //   * With `--log-level warn` the process still evaluates and dispatches
    //     INFO-level tracing, because the activity layer wants it. A quiet
    //     console no longer means a quiet runtime.
    //   * The activity layer therefore sees events the console does not. That
    //     is deliberate; someone debugging a filtered console should expect the
    //     activity view to stay full.
    //   * `--log-level debug` no longer floods the view either: the activity
    //     filter is pinned in both directions, so the view is comparable
    //     across boots and across operators.
    //   * The credential cap still holds for the view — its filter carries the
    //     same `rmcp::transport::auth=info` bound, and the layer forwards only
    //     INFO and above regardless.
    let console_filter = log_filter(&operator_directives)?;
    let analytics_filter = log_filter(&operator_directives)?;
    let activity_filter = log_filter(BASELINE_LOG_DIRECTIVES)?;

    let fmt_layer = fmt::layer().with_target(false);
    let analytics_layer = magician::magician_v2::analytics::tracing_layer::AnalyticsTracingLayer;
    // Writes into the process-wide activity channel from the first boot span.
    // The forwarder that drains that channel onto the event bus starts later,
    // in `run_http_server`, once the broadcaster exists — the channel is the
    // seam that lets this producer run before its consumer does.
    let activity_layer =
        magician::magician_v2::analytics::runtime_activity_layer::RuntimeActivityLayer::new();

    tracing_subscriber::registry()
        .with(fmt_layer.with_filter(console_filter))
        .with(analytics_layer.with_filter(analytics_filter))
        .with(activity_layer.with_filter(activity_filter))
        .init();

    Ok(())
}

fn load_magician_config(config_path: &Path) -> Result<MagicianConfig> {
    // Preserve the relocated live-config default while honoring an explicit
    // CLI path. JSON App commands rely on this distinction so a pre-boot input
    // failure is rendered by their closed envelope instead of consulting an
    // unrelated ambient configuration.
    let magician_config_path = if config_path == Path::new("tool-runtime-config.yaml") {
        magician::config::magician_config_path()
    } else {
        config_path.to_path_buf()
    };

    info!("📄 Loading {}", magician_config_path.display());
    magician::config::load_magician_config_from_path(&magician_config_path)
}

async fn install_process_storage_runtime(
    storage_workspace: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    storage_profile: magician_storage::ResolvedStorageProfile,
    acquire_default_lease: bool,
) -> Result<Arc<magician_storage::StorageRuntime>> {
    magician::magician_v2::artifact_v2::workspace::set_installed_runtime_root(
        storage_workspace.base_root().to_path_buf(),
    );
    let adapter_root =
        magician_storage::StorageRuntime::adapter_root_for_workspace(storage_workspace.base_root());
    let pid = std::process::id();
    let owner = magician_storage::OwnerId::parse(&format!("magician-{pid}"))
        .or_else(|_| magician_storage::OwnerId::parse("magician"))
        .map_err(|err| anyhow::anyhow!("storage owner: {err}"))?;
    let runtime = Arc::new(
        magician_storage::StorageRuntime::open_local(adapter_root, storage_profile, owner)
            .map_err(|err| anyhow::anyhow!("opening storage runtime: {err}"))?,
    );
    let default_scope = magician_storage::ScopeId::new(
        magician_storage::PrincipalId::parse(
            magician::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_PRINCIPAL,
        )
        .map_err(|err| anyhow::anyhow!("default principal: {err}"))?,
        magician_storage::WorkspaceId::parse(
            magician::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_WORKSPACE,
        )
        .map_err(|err| anyhow::anyhow!("default workspace: {err}"))?,
    );
    if acquire_default_lease {
        runtime
            .scope_leases
            .acquire(&default_scope)
            .await
            .map_err(|err| anyhow::anyhow!("acquiring default scope lease: {err}"))?;
        runtime.scope_leases.set_on_loss(Arc::new(|err| {
            tracing::error!(
                error = %err,
                "storage scope lease lost; canonical writers must fail closed"
            );
        }));
    }
    magician_storage::StorageRuntime::install(Arc::clone(&runtime));
    let health = runtime.operator_health();
    info!(
        profile = %health.profile,
        source = %health.source,
        generation = runtime.scope_leases.generation(&default_scope).unwrap_or(0),
        "installed process-wide StorageRuntime"
    );
    Ok(runtime)
}

fn resolve_storage_workspace(
    config: &MagicianConfig,
) -> Result<magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace> {
    let seed_root =
        magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::resolve_scoped_root(
            Path::new(&config.storage_path),
        );
    // Runtime root: env override, else the `$HOME/MagicianNotes` default.
    // (seed_root below stays the repo seed — that's where the TEMPLATES live.)
    let bootstrap_root = magician::magician_v2::artifact_v2::workspace::default_storage_base_path();
    magician::magician_v2::workspace_storage_settings::WorkspaceStorageSettingsStore::new(
        bootstrap_root,
    )
    .with_seed_root(seed_root)
    .resolve_workspace_sync()
}

/// The boot gate for the list index: does this open owe a walk of the disk?
///
/// A thin wrapper so the startup path asks the question by name and a test can
/// ask the same question the same way. It must NOT be `opened().needs_rebuild()`
/// — that is false for a reused index whose rebuild was interrupted, which
/// leaves such a file unfinished on every subsequent boot and every list
/// permanently served by the walk. `ListIndex::rebuild_is_owed` carries the
/// reasoning.
///
/// An index that cannot even be asked is treated as owing a rebuild. It is a
/// cache: the expensive answer is always available, and refusing to boot over
/// one would make the cache load-bearing.
fn list_index_rebuild_owed(list_index: &magician::magician_v2::storage::ListIndex) -> bool {
    list_index.rebuild_is_owed().unwrap_or_else(|error| {
        warn!(
            error = %error,
            "[LIST-INDEX] Could not read the list index rebuild marker; rebuilding from disk"
        );
        true
    })
}

/// `magician --reindex`: discard the list index and rebuild it from the
/// records on disk, in the foreground, then exit.
///
/// Nothing else in the process starts — no HTTP server, no workers, no
/// scheduler — so this is runnable against a store whose service is stopped,
/// which is when an operator wants it. The walk is the same one the boot path
/// runs; the difference is the discard in front of it, which is what makes
/// this able to repair rows a resume would have trusted.
async fn run_list_index_reindex(config: &MagicianConfig) -> Result<()> {
    let storage_workspace = resolve_storage_workspace(config)?;
    let base_root = storage_workspace.base_root().to_path_buf();
    let index = magician::magician_v2::storage::ListIndex::open_discarding(&base_root)
        .context("discarding the list index")?;
    println!(
        "discarded the list index at {} ({:?}); walking {}",
        index.path().display(),
        index.opened(),
        storage_workspace.scopes_root().display()
    );

    let report = index
        .rebuild_from_disk_async(storage_workspace.scopes_root())
        .await
        .context("rebuilding the list index from disk")?;
    println!(
        "reindexed: {} units walked, {} entries indexed, {} skipped, {} hidden, \
         {} unparsed timestamps",
        report.units_walked,
        report.entries_indexed,
        report.entries_skipped,
        report.entries_hidden,
        report.unparsed_timestamps
    );
    // The marker, not the report, is what a reader consults. Printing it
    // proves the rebuild committed rather than merely returned.
    println!(
        "list index ready: {}",
        index.is_ready().context("reading list index readiness")?
    );
    Ok(())
}

#[derive(Debug, Clone, Default, Deserialize)]
struct HostSpeechStatusResponse {
    #[serde(default)]
    available: bool,
    #[serde(default)]
    stt_available: bool,
    #[serde(default)]
    tts_available: bool,
    #[serde(default)]
    helper_exists: bool,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
struct HostSpeechAvailability {
    stt: bool,
    tts: bool,
}

async fn probe_host_speech_availability(host_gateway_url: &str) -> HostSpeechAvailability {
    const HOST_SPEECH_PROBE_ATTEMPTS: usize = 5;
    const HOST_SPEECH_PROBE_RETRY_DELAY: Duration = Duration::from_millis(250);

    let endpoint = format!(
        "{}/host/speech/status",
        host_gateway_url.trim_end_matches('/')
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .user_agent("magician-media-rails/0.1 host-speech-probe")
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            warn!("macOS speech availability probe client failed: {error}");
            return HostSpeechAvailability::default();
        },
    };
    let mut last_error = None;
    let mut response = None;
    for attempt in 1..=HOST_SPEECH_PROBE_ATTEMPTS {
        match client.get(&endpoint).send().await {
            Ok(next_response) => {
                response = Some(next_response);
                break;
            },
            Err(error) => {
                last_error = Some(error);
                if attempt < HOST_SPEECH_PROBE_ATTEMPTS {
                    tokio::time::sleep(HOST_SPEECH_PROBE_RETRY_DELAY).await;
                }
            },
        }
    }
    let Some(response) = response else {
        if let Some(error) = last_error {
            info!(
                url = endpoint,
                attempts = HOST_SPEECH_PROBE_ATTEMPTS,
                "macOS speech host gateway unavailable; local STT/TTS will not be advertised: {error}"
            );
        }
        return HostSpeechAvailability::default();
    };
    let status = response.status();
    if !status.is_success() {
        warn!(
            url = endpoint,
            status = status.as_u16(),
            "macOS speech host gateway status probe failed; local STT/TTS will not be advertised"
        );
        return HostSpeechAvailability::default();
    }
    let status = match response.json::<HostSpeechStatusResponse>().await {
        Ok(status) => status,
        Err(error) => {
            warn!(
                url = endpoint,
                "macOS speech host gateway status response was invalid; local STT/TTS will not be advertised: {error}"
            );
            return HostSpeechAvailability::default();
        },
    };
    if !status.available || !status.helper_exists {
        info!(
            reason = status.reason.as_deref().unwrap_or("unavailable"),
            "macOS speech host gateway reports unavailable; local STT/TTS will not be advertised"
        );
        return HostSpeechAvailability::default();
    }
    HostSpeechAvailability {
        stt: status.stt_available,
        tts: status.tts_available,
    }
}

fn build_tts_providers(
    config: &TtsConfig,
    legacy_openai_api_key: Option<String>,
    legacy_openai_base_url: Option<String>,
    legacy_minimax_api_key: Option<String>,
    legacy_minimax_group_id: Option<String>,
    cache_capacity: usize,
    fluid_audio_manager: Option<
        &Arc<magician_media::media_rails::fluid_audio::FluidAudioEngineManager>,
    >,
    fluid_audio_supportable: bool,
) -> Vec<DynTtsProvider> {
    let mut providers = Vec::new();
    let mut seen_ids = std::collections::BTreeSet::new();
    for provider in &config.providers {
        match build_configured_tts_provider(
            provider,
            legacy_openai_api_key.as_deref(),
            legacy_openai_base_url.as_deref(),
            legacy_minimax_api_key.as_deref(),
            legacy_minimax_group_id.as_deref(),
            cache_capacity,
            fluid_audio_manager,
            fluid_audio_supportable,
        ) {
            Some(provider) => {
                let id_key = provider.id().to_ascii_lowercase();
                if !seen_ids.insert(id_key) {
                    warn!(
                        provider = provider.id(),
                        "skipping duplicate TTS provider id from config"
                    );
                    continue;
                }
                providers.push(provider);
            },
            None => {},
        }
    }
    if !providers.is_empty() {
        return providers;
    }

    // Compatibility path for older magician-config.yaml files that do not yet
    // declare `media.tts.providers`. New remote TTS/model choices should live in
    // YAML, not env-only boot wiring.
    let mut legacy = Vec::new();
    if let Some(provider) = build_legacy_openai_tts_provider(
        legacy_openai_api_key,
        legacy_openai_base_url,
        cache_capacity,
    ) {
        legacy.push(provider);
    }
    if let Some(provider) = build_legacy_minimax_tts_provider(
        legacy_minimax_api_key,
        legacy_minimax_group_id,
        cache_capacity,
    ) {
        legacy.push(provider);
    }
    legacy
}

fn build_configured_tts_provider(
    config: &TtsProviderConfig,
    legacy_openai_api_key: Option<&str>,
    legacy_openai_base_url: Option<&str>,
    legacy_minimax_api_key: Option<&str>,
    legacy_minimax_group_id: Option<&str>,
    cache_capacity: usize,
    fluid_audio_manager: Option<
        &Arc<magician_media::media_rails::fluid_audio::FluidAudioEngineManager>,
    >,
    fluid_audio_supportable: bool,
) -> Option<DynTtsProvider> {
    if !config.enabled {
        return None;
    }
    let id = config.id.trim();
    if id.is_empty() {
        warn!("skipping TTS provider with empty id");
        return None;
    }
    let model = config.model.trim();
    if model.is_empty() {
        warn!(provider = id, "skipping TTS provider with empty model");
        return None;
    }

    match config.adapter.trim().to_ascii_lowercase().as_str() {
        "openai_speech" | "openai_tts" | "openai_audio_speech" | "openai_compatible" | "openai" => {
            let base_url = config
                .base_url
                .clone()
                .or_else(|| legacy_openai_base_url.map(str::to_string))
                .unwrap_or_else(|| {
                    magician_media::media_rails::OPENAI_TTS_DEFAULT_BASE_URL.to_string()
                });
            let api_key_env = config.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
            let default_openai_key_env = config
                .api_key_env
                .as_deref()
                .map(|value| value == "OPENAI_API_KEY")
                .unwrap_or(true);
            let api_key = if default_openai_key_env {
                legacy_openai_api_key
                    .map(str::to_string)
                    .or_else(|| env_nonempty(api_key_env))
            } else {
                env_nonempty(api_key_env)
            }
            .or_else(|| {
                if config.base_url.is_some() || legacy_openai_base_url.is_some() {
                    Some("local".to_string())
                } else {
                    None
                }
            });
            let Some(api_key) = api_key else {
                info!(
                    provider = id,
                    api_key_env, "skipping TTS provider because API key env is empty"
                );
                return None;
            };
            let voice = config
                .voice
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(
                    magician_media::media_rails::providers::openai_tts::OPENAI_TTS_DEFAULT_VOICE,
                );
            let format = config
                .format
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(
                    magician_media::media_rails::providers::openai_tts::OPENAI_TTS_DEFAULT_FORMAT,
                );
            let provider =
                magician_media::media_rails::OpenAiTtsProvider::with_base_url(api_key, base_url)
                    .with_defaults(model.to_string(), voice.to_string(), format.to_string());
            Some(configured_cached_tts_provider(
                Arc::new(provider),
                id,
                config_tts_label(config),
                cache_capacity,
            ))
        },
        "minimax_t2a_v2" | "minimax_tts" | "minimax" => {
            let api_key_env = config.api_key_env.as_deref().unwrap_or("MINIMAX_API_KEY");
            let api_key = if api_key_env == "MINIMAX_API_KEY" {
                legacy_minimax_api_key
                    .map(str::to_string)
                    .or_else(|| env_nonempty(api_key_env))
            } else {
                env_nonempty(api_key_env)
            };
            let Some(api_key) = api_key else {
                info!(
                    provider = id,
                    api_key_env, "skipping MiniMax TTS provider because API key env is empty"
                );
                return None;
            };
            let group_id_env = config
                .group_id_env
                .as_deref()
                .unwrap_or("MAGICIAN_MINIMAX_GROUP_ID");
            let group_id = config
                .group_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    if group_id_env == "MAGICIAN_MINIMAX_GROUP_ID" {
                        legacy_minimax_group_id.map(str::to_string)
                    } else {
                        env_nonempty(group_id_env)
                    }
                })
                .or_else(|| env_nonempty(group_id_env));
            let Some(group_id) = group_id else {
                info!(
                    provider = id,
                    group_id_env, "skipping MiniMax TTS provider because group id is empty"
                );
                return None;
            };
            let base_url = config.base_url.clone().unwrap_or_else(|| {
                magician_media::media_rails::MINIMAX_TTS_DEFAULT_BASE_URL.to_string()
            });
            let voice = config
                .voice
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(magician_media::media_rails::MINIMAX_TTS_DEFAULT_VOICE);
            let format = config
                .format
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(magician_media::media_rails::MINIMAX_TTS_DEFAULT_FORMAT);
            let provider = magician_media::media_rails::MiniMaxTtsProvider::with_base_url(
                api_key, group_id, base_url,
            )
            .with_defaults(model.to_string(), voice.to_string(), format.to_string());
            Some(configured_cached_tts_provider(
                Arc::new(provider),
                id,
                config_tts_label(config),
                cache_capacity,
            ))
        },
        "gemini_tts" | "gemini_generate_content_tts" | "gemini_audio" => {
            let api_key_env = config.api_key_env.as_deref().unwrap_or("GEMINI_API_KEY");
            let Some(api_key) = env_nonempty(api_key_env) else {
                info!(
                    provider = id,
                    api_key_env, "skipping Gemini TTS provider because API key env is empty"
                );
                return None;
            };
            let base_url = config.base_url.clone().unwrap_or_else(|| {
                magician_media::media_rails::GEMINI_TTS_DEFAULT_BASE_URL.to_string()
            });
            let voice = config
                .voice
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(magician_media::media_rails::GEMINI_TTS_DEFAULT_VOICE);
            let format = config
                .format
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(magician_media::media_rails::GEMINI_TTS_DEFAULT_FORMAT);
            let provider =
                magician_media::media_rails::GeminiTtsProvider::with_base_url(api_key, base_url)
                    .with_defaults(model.to_string(), voice.to_string(), format.to_string());
            Some(configured_cached_tts_provider(
                Arc::new(provider),
                id,
                config_tts_label(config),
                cache_capacity,
            ))
        },
        "grok_tts" | "xai_tts" | "grok" => {
            let api_key_env = config.api_key_env.as_deref().unwrap_or("XAI_API_KEY");
            let Some(api_key) = env_nonempty(api_key_env) else {
                info!(
                    provider = id,
                    api_key_env, "skipping Grok TTS provider because API key env is empty"
                );
                return None;
            };
            let base_url = config.base_url.clone().unwrap_or_else(|| {
                magician_media::media_rails::GROK_TTS_DEFAULT_BASE_URL.to_string()
            });
            let voice = config
                .voice
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(magician_media::media_rails::GROK_TTS_DEFAULT_VOICE);
            let format = config
                .format
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(magician_media::media_rails::GROK_TTS_DEFAULT_FORMAT);
            let mut provider =
                magician_media::media_rails::GrokTtsProvider::with_base_url(api_key, base_url)
                    .with_defaults(model.to_string(), voice.to_string(), format.to_string());
            if !config.voices.is_empty() {
                provider = provider.with_voices(config.voices.clone());
            }
            if !config.formats.is_empty() {
                provider = provider.with_formats(config.formats.clone());
            }
            Some(configured_cached_tts_provider(
                Arc::new(provider),
                id,
                config_tts_label(config),
                cache_capacity,
            ))
        },
        magician_media::media_rails::fluid_audio::FLUID_AUDIO_TTS_ADAPTER => {
            let Some(manager) = fluid_audio_manager
                .filter(|manager| fluid_audio_supportable && manager.model_is_supportable(id))
            else {
                info!(
                    provider = id,
                    "skipping FluidAudio TTS provider because the engine is unavailable"
                );
                return None;
            };
            let Some(voice) = config
                .voice
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                warn!(
                    provider = id,
                    "skipping FluidAudio TTS provider without a default voice"
                );
                return None;
            };
            match magician_media::media_rails::fluid_audio::FluidAudioTtsProvider::new(
                id,
                config_tts_label(config),
                model,
                voice,
                config.voices.clone(),
                config.formats.clone(),
                Arc::clone(manager),
            ) {
                Ok(provider) => {
                    let (enabled_gate, generation_gate) = manager.cache_gate();
                    let cache =
                        Arc::new(magician_media::media_rails::CachedTtsProvider::new_gated(
                            Arc::new(provider),
                            cache_capacity,
                            enabled_gate,
                            generation_gate,
                        ));
                    manager.register_tts_cache(Arc::downgrade(&cache));
                    Some(cache as DynTtsProvider)
                },
                Err(error) => {
                    warn!(provider = id, %error, "skipping invalid FluidAudio TTS provider");
                    None
                },
            }
        },
        other => {
            warn!(
                provider = id,
                adapter = other,
                "skipping TTS provider with unsupported adapter"
            );
            None
        },
    }
}

fn build_legacy_openai_tts_provider(
    legacy_api_key: Option<String>,
    legacy_base_url: Option<String>,
    cache_capacity: usize,
) -> Option<DynTtsProvider> {
    let key = legacy_api_key?;
    let base_url = legacy_base_url
        .unwrap_or_else(|| magician_media::media_rails::OPENAI_TTS_DEFAULT_BASE_URL.to_string());
    let mut tts = magician_media::media_rails::OpenAiTtsProvider::with_base_url(key, base_url);
    let tts_model = std::env::var("MAGICIAN_TTS_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let tts_voice = std::env::var("MAGICIAN_TTS_VOICE")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let tts_format = std::env::var("MAGICIAN_TTS_FORMAT")
        .ok()
        .filter(|v| !v.trim().is_empty());
    if tts_model.is_some() || tts_voice.is_some() || tts_format.is_some() {
        tts = tts.with_defaults(
            tts_model.as_deref().unwrap_or(
                magician_media::media_rails::providers::openai_tts::OPENAI_TTS_DEFAULT_MODEL,
            ),
            tts_voice.as_deref().unwrap_or(
                magician_media::media_rails::providers::openai_tts::OPENAI_TTS_DEFAULT_VOICE,
            ),
            tts_format.as_deref().unwrap_or(
                magician_media::media_rails::providers::openai_tts::OPENAI_TTS_DEFAULT_FORMAT,
            ),
        );
    }
    Some(magician_media::media_rails::CachedTtsProvider::wrap(
        Arc::new(tts),
        cache_capacity,
    ))
}

fn build_legacy_minimax_tts_provider(
    legacy_api_key: Option<String>,
    legacy_group_id: Option<String>,
    cache_capacity: usize,
) -> Option<DynTtsProvider> {
    let (Some(key), Some(group_id)) = (legacy_api_key, legacy_group_id) else {
        return None;
    };
    let base_url = std::env::var("MAGICIAN_MINIMAX_TTS_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| magician_media::media_rails::MINIMAX_TTS_DEFAULT_BASE_URL.to_string());
    let mut mm_tts =
        magician_media::media_rails::MiniMaxTtsProvider::with_base_url(key, group_id, base_url);
    let mm_model = std::env::var("MAGICIAN_MINIMAX_TTS_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let mm_voice = std::env::var("MAGICIAN_MINIMAX_TTS_VOICE")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let mm_format = std::env::var("MAGICIAN_MINIMAX_TTS_FORMAT")
        .ok()
        .filter(|v| !v.trim().is_empty());
    if mm_model.is_some() || mm_voice.is_some() || mm_format.is_some() {
        mm_tts = mm_tts.with_defaults(
            mm_model
                .as_deref()
                .unwrap_or(magician_media::media_rails::MINIMAX_TTS_DEFAULT_MODEL),
            mm_voice
                .as_deref()
                .unwrap_or(magician_media::media_rails::MINIMAX_TTS_DEFAULT_VOICE),
            mm_format
                .as_deref()
                .unwrap_or(magician_media::media_rails::MINIMAX_TTS_DEFAULT_FORMAT),
        );
    }
    Some(magician_media::media_rails::CachedTtsProvider::wrap(
        Arc::new(mm_tts),
        cache_capacity,
    ))
}

fn configured_cached_tts_provider(
    provider: DynTtsProvider,
    id: &str,
    label: Option<String>,
    cache_capacity: usize,
) -> DynTtsProvider {
    magician_media::media_rails::CachedTtsProvider::wrap(
        Arc::new(magician_media::media_rails::ConfiguredTtsProvider::new(
            provider,
            id.to_string(),
            label,
        )),
        cache_capacity,
    )
}

fn build_recording_stt_providers(
    config: &RecordingSttConfig,
    legacy_api_key: Option<String>,
    legacy_base_url: Option<String>,
    fluid_audio_manager: Option<
        &Arc<magician_media::media_rails::fluid_audio::FluidAudioEngineManager>,
    >,
) -> Vec<DynSttProvider> {
    let mut providers = Vec::new();
    for provider in &config.providers {
        match build_configured_recording_stt_provider(
            provider,
            legacy_api_key.as_deref(),
            legacy_base_url.as_deref(),
            fluid_audio_manager,
        ) {
            Some(provider) => providers.push(provider),
            None => {},
        }
    }
    if !providers.is_empty() {
        return providers;
    }

    // Compatibility path for older magician-config.yaml files that do not yet
    // declare `media.recording_stt.providers`. New model choices should live in
    // YAML, not here.
    let Some(key) = legacy_api_key else {
        return Vec::new();
    };
    let base_url = legacy_base_url.unwrap_or_else(|| {
        magician_media::media_rails::OPENAI_WHISPER_DEFAULT_BASE_URL.to_string()
    });
    let mut stt = magician_media::media_rails::OpenAiWhisperProvider::with_base_url(key, base_url);
    if let Ok(model) = std::env::var("MAGICIAN_STT_MODEL") {
        if !model.trim().is_empty() {
            stt = stt.with_default_model(model.trim().to_string());
        }
    }
    vec![Arc::new(stt)]
}

fn build_streaming_stt_catalog_providers(
    config: &magician_media::media_rails::AudioProviderCatalogConfig,
    openai_api_key: Option<String>,
    gemini_api_key: Option<String>,
    fluid_audio_manager: Option<
        &Arc<magician_media::media_rails::fluid_audio::FluidAudioEngineManager>,
    >,
) -> Vec<DynStreamingSttProvider> {
    use magician_media::media_rails::providers::{
        MacOsSpeechSttProvider, OpenAiDiarizeStreamingSttProvider, OpenAiStreamingSttProvider,
        OpenAiWhisperProvider,
    };

    let mut providers = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for binding in &config.providers {
        if !binding.enabled || !seen.insert(binding.id.to_ascii_lowercase()) {
            continue;
        }
        let provider: Option<DynStreamingSttProvider> = match binding.adapter.as_str() {
            "macos_speech_streaming" => {
                magician_media::media_rails::meeting::macos_speech_helper_present()
                    .then(|| Arc::new(MacOsSpeechSttProvider::new()) as DynStreamingSttProvider)
            },
            "openai_streaming_stt" => openai_api_key.as_ref().map(|key| {
                let whisper = OpenAiWhisperProvider::new(key.clone())
                    .with_default_model(binding.model.clone());
                Arc::new(OpenAiStreamingSttProvider::with_whisper(whisper))
                    as DynStreamingSttProvider
            }),
            "openai_diarize_streaming_stt" => openai_api_key.as_ref().map(|key| {
                Arc::new(
                    OpenAiDiarizeStreamingSttProvider::new(key.clone())
                        .with_model(binding.model.clone()),
                ) as DynStreamingSttProvider
            }),
            "gemini_live_transcribe" | "gemini_live_transcribe_streaming_stt" => {
                match gemini_api_key.as_ref() {
                    Some(key) => {
                        let mut provider =
                            magician_media::media_rails::GeminiLiveTranscribeSttProvider::new(
                                key.clone(),
                            )
                            .with_provider_id(binding.id.clone())
                            .with_model(binding.model.clone())
                            .with_language_codes(binding.language_codes.clone());
                        if let Some(label) = binding
                            .label
                            .as_deref()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                        {
                            provider = provider.with_label(label.to_string());
                        }
                        Some(Arc::new(provider) as DynStreamingSttProvider)
                    },
                    None => {
                        info!(
                            provider = %binding.id,
                            "skipping Gemini Live Transcribe because GEMINI_API_KEY is empty"
                        );
                        None
                    },
                }
            },
            magician_media::media_rails::fluid_audio::FLUID_AUDIO_STREAMING_STT_ADAPTER => {
                fluid_audio_manager.map(|manager| {
                    Arc::new(
                        magician_media::media_rails::fluid_audio::FluidAudioStreamingSttProvider::new(
                            binding.id.clone(),
                            binding.label.clone(),
                            binding.model.clone(),
                            binding.language_codes.first().cloned(),
                            Arc::clone(manager),
                        ),
                    ) as DynStreamingSttProvider
                })
            },
            adapter => {
                warn!(
                    provider = binding.id,
                    adapter, "streaming STT binding is catalog-only in this build"
                );
                None
            },
        };
        if let Some(provider) = provider {
            providers.push(provider);
        }
    }
    if !providers.is_empty() || !config.providers.is_empty() {
        return providers;
    }

    if magician_media::media_rails::meeting::macos_speech_helper_present() {
        providers.push(Arc::new(MacOsSpeechSttProvider::new()));
    }
    if let Some(key) = openai_api_key {
        providers.push(Arc::new(OpenAiDiarizeStreamingSttProvider::new(
            key.clone(),
        )));
        providers.push(Arc::new(OpenAiStreamingSttProvider::new(key)));
    }
    providers
}

fn build_configured_recording_stt_provider(
    config: &RecordingSttProviderConfig,
    legacy_api_key: Option<&str>,
    legacy_base_url: Option<&str>,
    fluid_audio_manager: Option<
        &Arc<magician_media::media_rails::fluid_audio::FluidAudioEngineManager>,
    >,
) -> Option<DynSttProvider> {
    if !config.enabled {
        return None;
    }
    let id = config.id.trim();
    if id.is_empty() {
        warn!("skipping recording STT provider with empty id");
        return None;
    }
    let model = config.model.trim();
    if model.is_empty() {
        warn!(
            provider = id,
            "skipping recording STT provider with empty model"
        );
        return None;
    }

    match config.adapter.trim().to_ascii_lowercase().as_str() {
        magician_media::media_rails::fluid_audio::FLUID_AUDIO_RECORDING_STT_ADAPTER => {
            let Some(manager) = fluid_audio_manager else {
                info!(
                    provider = id,
                    "skipping FluidAudio recording STT because the sidecar is unavailable"
                );
                return None;
            };
            if !manager.model_is_supportable(id) {
                info!(
                    provider = id,
                    "skipping FluidAudio recording STT because this host does not support the configured model"
                );
                return None;
            }
            Some(Arc::new(
                magician_media::media_rails::fluid_audio::FluidAudioRecordingSttProvider::new(
                    id.to_string(),
                    config_label(config),
                    model.to_string(),
                    Arc::clone(manager),
                ),
            ))
        },
        "openai_transcriptions" | "openai_audio_transcriptions" | "openai" => {
            let base_url = config
                .base_url
                .clone()
                .or_else(|| legacy_base_url.map(str::to_string))
                .unwrap_or_else(|| {
                    magician_media::media_rails::OPENAI_WHISPER_DEFAULT_BASE_URL.to_string()
                });
            let api_key_env = config.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
            let default_openai_key_env = config
                .api_key_env
                .as_deref()
                .map(|value| value == "OPENAI_API_KEY")
                .unwrap_or(true);
            let api_key = if default_openai_key_env {
                legacy_api_key
                    .map(str::to_string)
                    .or_else(|| env_nonempty(api_key_env))
            } else {
                env_nonempty(api_key_env)
            }
            .or_else(|| {
                if config.base_url.is_some() || legacy_base_url.is_some() {
                    Some("local".to_string())
                } else {
                    None
                }
            });
            let Some(api_key) = api_key else {
                warn!(
                    provider = id,
                    api_key_env, "skipping recording STT provider because API key env is empty"
                );
                return None;
            };
            let mut provider = magician_media::media_rails::OpenAiWhisperProvider::with_base_url(
                api_key, base_url,
            )
            .with_provider_id(id.to_string())
            .with_default_model(model.to_string());
            if let Some(label) = config_label(config) {
                provider = provider.with_label(label);
            }
            Some(Arc::new(provider))
        },
        "gemini_generate_content" | "gemini_audio" | "gemini" => {
            let base_url = config.base_url.clone().unwrap_or_else(|| {
                magician_media::media_rails::GEMINI_STT_DEFAULT_BASE_URL.to_string()
            });
            let api_key_env = config.api_key_env.as_deref().unwrap_or("GEMINI_API_KEY");
            let Some(api_key) = env_nonempty(api_key_env) else {
                info!(
                    provider = id,
                    api_key_env,
                    "skipping Gemini recording STT provider because API key env is empty"
                );
                return None;
            };
            let mut provider =
                magician_media::media_rails::GeminiSttProvider::with_base_url(api_key, base_url)
                    .with_provider_id(id.to_string())
                    .with_default_model(model.to_string());
            if let Some(label) = config_label(config) {
                provider = provider.with_label(label);
            }
            if let Some(prompt) = config
                .prompt
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                provider = provider.with_default_prompt(prompt.to_string());
            }
            if let Some(inline_max_bytes) = config.inline_max_bytes {
                provider = provider.with_inline_max_bytes(inline_max_bytes);
            }
            if let Some(generation_config) = config.generation_config.clone() {
                provider = provider.with_generation_config(generation_config);
            }
            Some(Arc::new(provider))
        },
        "gemini_transcribe" | "gemini_interactions_transcribe" => {
            let base_url = config.base_url.clone().unwrap_or_else(|| {
                magician_media::media_rails::GEMINI_TRANSCRIBE_DEFAULT_BASE_URL.to_string()
            });
            let api_key_env = config.api_key_env.as_deref().unwrap_or("GEMINI_API_KEY");
            let Some(api_key) = env_nonempty(api_key_env) else {
                info!(
                    provider = id,
                    api_key_env, "skipping Gemini 3.5 Transcribe because API key env is empty"
                );
                return None;
            };
            let mut provider =
                magician_media::media_rails::GeminiTranscribeSttProvider::with_base_url(
                    api_key, base_url,
                )
                .with_provider_id(id.to_string())
                .with_default_model(model.to_string());
            if let Some(label) = config_label(config) {
                provider = provider.with_label(label);
            }
            Some(Arc::new(provider))
        },
        "google_cloud_speech_v2"
        | "google_cloud_speech"
        | "google_speech_v2"
        | "cloud_speech_v2" => {
            let auth_token_env = config
                .auth_token_env
                .as_deref()
                .unwrap_or("GOOGLE_CLOUD_ACCESS_TOKEN");
            let Some(access_token) = env_nonempty(auth_token_env) else {
                warn!(
                    provider = id,
                    auth_token_env,
                    "skipping Google Cloud Speech STT provider because OAuth token env is empty"
                );
                return None;
            };
            let project_id = config
                .project_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    env_nonempty(
                        config
                            .project_id_env
                            .as_deref()
                            .unwrap_or("GOOGLE_CLOUD_PROJECT"),
                    )
                })
                .unwrap_or_default();
            let location = config
                .location
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| env_nonempty("GOOGLE_CLOUD_LOCATION"))
                .unwrap_or_else(|| {
                    magician_media::media_rails::GOOGLE_CLOUD_SPEECH_STT_DEFAULT_LOCATION
                        .to_string()
                });
            let base_url = config.base_url.clone().unwrap_or_else(|| {
                magician_media::media_rails::google_cloud_speech_default_base_url_for_location(
                    &location,
                )
            });
            let Some(recognizer) = magician_media::media_rails::google_cloud_speech_recognizer_name(
                &project_id,
                &location,
                config.recognizer.as_deref(),
            ) else {
                warn!(
                    provider = id,
                    project_id_env = config
                        .project_id_env
                        .as_deref()
                        .unwrap_or("GOOGLE_CLOUD_PROJECT"),
                    location,
                    "skipping Google Cloud Speech STT provider because project/location/recognizer config is incomplete"
                );
                return None;
            };
            let mut provider =
                magician_media::media_rails::GoogleCloudSpeechSttProvider::with_base_url(
                    access_token,
                    base_url,
                    recognizer,
                )
                .with_provider_id(id.to_string())
                .with_default_model(model.to_string())
                .with_language_codes(nonempty_strings(&config.language_codes));
            if let Some(label) = config_label(config) {
                provider = provider.with_label(label);
            }
            if let Some(features) = config.cloud_features.clone() {
                provider = provider.with_cloud_features(features);
            }
            if let Some(cloud_config) = config.cloud_config.clone() {
                provider = provider.with_cloud_config(cloud_config);
            }
            Some(Arc::new(provider))
        },
        "grok_stt" | "xai_stt" | "grok_transcribe" => {
            let api_key_env = config.api_key_env.as_deref().unwrap_or("XAI_API_KEY");
            let Some(api_key) = env_nonempty(api_key_env) else {
                info!(
                    provider = id,
                    api_key_env, "skipping Grok recording STT because API key env is empty"
                );
                return None;
            };
            let base_url = config.base_url.clone().unwrap_or_else(|| {
                magician_media::media_rails::GROK_STT_DEFAULT_BASE_URL.to_string()
            });
            let mut provider =
                magician_media::media_rails::GrokSttProvider::with_base_url(api_key, base_url)
                    .with_provider_id(id.to_string())
                    .with_default_model(model.to_string());
            if let Some(label) = config_label(config) {
                provider = provider.with_label(label);
            }
            Some(Arc::new(provider))
        },
        other => {
            warn!(
                provider = id,
                adapter = other,
                "skipping recording STT provider with unsupported adapter"
            );
            None
        },
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn config_label(config: &RecordingSttProviderConfig) -> Option<String> {
    config
        .label
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn config_tts_label(config: &TtsProviderConfig) -> Option<String> {
    config
        .label
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn nonempty_strings(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

/// Read `tool-runtime-config.yaml` and resolve `registry.paths` into a
/// list of existing absolute directories. Returns `[]` on any read /
/// parse error (logged at warn level) so a missing or malformed extras
/// list never blocks startup — the system baseline always loads.
fn load_extra_system_roots(config_path: &Path) -> Vec<PathBuf> {
    let runtime_config = match tool_runtime_core::config::Config::load(config_path, None, None) {
        Ok(cfg) => cfg,
        Err(err) => {
            warn!(
                config = %config_path.display(),
                error = %err,
                "failed to load tool-runtime-config.yaml for extra paths resolution; \
                 continuing with no extras"
            );
            return Vec::new();
        },
    };
    let config_dir = config_path.parent();
    let mut resolved = Vec::new();
    for path in runtime_config.registry.resolved_paths(config_dir) {
        if path.exists() {
            resolved.push(path);
        } else {
            warn!(
                path = %path.display(),
                "skipping non-existent extra system root declared in tool-runtime-config.yaml :: registry.paths"
            );
        }
    }
    resolved
}

async fn run_live_app_command(
    command: &magician::magician_v2::apps::authoring::AppAuthoringCommand,
    json: bool,
) -> Result<magician::magician_v2::apps::authoring::AppAuthoringCliExitStatus> {
    use magician::magician_v2::apps::{
        authoring::AppAuthoringCommand,
        package_transfer::{
            admit_package_archive, APP_PACKAGE_ARCHIVE_MAX_BYTES, APP_PACKAGE_ARCHIVE_MEDIA_TYPE,
        },
        portable_archive_transfer::{
            decode_app_portable_archive, AppArchivePassphrase,
            APP_PORTABLE_ARCHIVE_ENCRYPTED_MEDIA_TYPE, APP_PORTABLE_ARCHIVE_MAX_BYTES,
            APP_PORTABLE_ARCHIVE_PLAINTEXT_MEDIA_TYPE,
        },
    };

    const MAX_LIVE_JSON_BYTES: usize = 4 * 1_048_576;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .build()
        .context("constructing the bounded live Apps client")?;
    let result: Result<serde_json::Value> = async {
        match command {
            AppAuthoringCommand::List(args) => {
                let mut url = live_app_url(&args.live, &["directory"])?;
                {
                    let mut query = url.query_pairs_mut();
                    query.append_pair("section", args.section.as_str());
                    query.append_pair("limit", &args.limit.to_string());
                    if let Some(search) = args.search.as_deref() {
                        query.append_pair("search", search);
                    }
                    if let Some(cursor) = args.cursor.as_deref() {
                        query.append_pair("cursor", cursor);
                    }
                }
                live_json_request(&client, reqwest::Method::GET, url, &args.live, None).await
            },
            AppAuthoringCommand::Detail(args) => {
                let url = live_app_url(
                    &args.live,
                    &["installations", args.installation_id.as_str()],
                )?;
                let value =
                    live_json_request(&client, reqwest::Method::GET, url, &args.live, None).await?;
                require_correlated_string(&value, "installation_id", &args.installation_id)?;
                Ok(value)
            },
            AppAuthoringCommand::Review(args) => {
                let url = live_app_url(
                    &args.live,
                    &["installations", args.installation_id.as_str(), "review"],
                )?;
                let value =
                    live_json_request(&client, reqwest::Method::GET, url, &args.live, None).await?;
                require_correlated_string(&value, "installation_id", &args.installation_id)?;
                Ok(value)
            },
            AppAuthoringCommand::Approve(args) => {
                let url = live_app_url(
                    &args.live,
                    &["installations", args.installation_id.as_str(), "approve"],
                )?;
                let body = serde_json::json!({
                    "review_material_digest": args.review_digest,
                    "granted_tools": optional_json_array(&args.grant_tools),
                    "granted_agents": optional_json_array(&args.grant_agents),
                    "granted_personalities": optional_json_array(&args.grant_personalities),
                    "migration_run_id": args.migration_run_id,
                    "update_plan_digest": args.update_plan_digest,
                    "destructive_migration_confirmed": args.confirm_destructive_migration,
                });
                let value = live_json_request(
                    &client,
                    reqwest::Method::POST,
                    url,
                    &args.live,
                    Some(body),
                )
                .await?;
                require_correlated_string(&value, "installation_id", &args.installation_id)?;
                Ok(value)
            },
            AppAuthoringCommand::Disable(args) => {
                live_lifecycle_control(&client, args, "disable", Some("disabled")).await
            },
            AppAuthoringCommand::Quarantine(args) => {
                live_lifecycle_control(&client, args, "quarantine", Some("quarantined")).await
            },
            AppAuthoringCommand::UninstallRetain(args) => {
                live_lifecycle_control(
                    &client,
                    args,
                    "uninstall",
                    Some("uninstalled_retained"),
                )
                .await
            },
            AppAuthoringCommand::GrantRevoke(args) => {
                live_lifecycle_control(
                    &client,
                    args,
                    "grant-revocations",
                    Some("quarantined"),
                )
                .await
            },
            AppAuthoringCommand::UpdateBegin(args) => {
                live_lifecycle_control(&client, args, "update-begin", Some("update_pending")).await
            },
            AppAuthoringCommand::UpdateAbort(args) => {
                live_lifecycle_control(&client, args, "update-abort", None).await
            },
            AppAuthoringCommand::UpdatePlan(args) => {
                let operations = args
                    .operations_file
                    .as_ref()
                    .map(|path| {
                        let bytes = read_bounded_regular_file(
                            path,
                            MAX_LIVE_JSON_BYTES,
                            "migration operations",
                        )?;
                        serde_json::from_slice::<serde_json::Value>(&bytes)
                            .context("migration operations file is not JSON")
                    })
                    .transpose()?
                    .unwrap_or_else(|| serde_json::json!([]));
                if !operations.is_array() {
                    anyhow::bail!("migration operations file must contain a JSON array");
                }
                let url = live_app_url(
                    &args.live,
                    &[
                        "installations",
                        args.installation_id.as_str(),
                        "update-plans",
                    ],
                )?;
                let value = live_json_request(
                    &client,
                    reqwest::Method::POST,
                    url,
                    &args.live,
                    Some(serde_json::json!({
                        "attempt_id": args.attempt_id,
                        "expected_parked_generation": args.expected_generation,
                        "migration_operations": operations,
                    })),
                )
                .await?;
                require_correlated_string(&value, "installation_id", &args.installation_id)?;
                Ok(value)
            },
            AppAuthoringCommand::UpdateBackup(args) => {
                let passphrase = read_archive_passphrase(&args.passphrase_file)?;
                let url = live_app_url(
                    &args.live,
                    &["updates", args.migration_run_id.as_str(), "backup"],
                )?;
                let response = live_request(&client, reqwest::Method::POST, url, &args.live)
                    .header(
                        "x-magician-app-archive-passphrase",
                        reqwest::header::HeaderValue::from_bytes(&passphrase)
                            .context("archive passphrase is not a valid HTTP header value")?,
                    )
                    .send()
                    .await
                    .context("calling the live update-backup owner")?;
                let value = parse_live_json_response(response, MAX_LIVE_JSON_BYTES).await?;
                require_correlated_string(&value, "migration_run_id", &args.migration_run_id)?;
                Ok(value)
            },
            AppAuthoringCommand::RollbackCode(args) => {
                let url = live_app_url(
                    &args.live,
                    &[
                        "installations",
                        args.installation_id.as_str(),
                        "rollbacks",
                        "code",
                    ],
                )?;
                let value = live_json_request(
                    &client,
                    reqwest::Method::POST,
                    url,
                    &args.live,
                    Some(serde_json::json!({
                        "migration_run_id": args.migration_run_id,
                        "expected_installation_generation": args.expected_generation,
                        "request_id": args.request_id,
                    })),
                )
                .await?;
                require_correlated_string(&value, "installation_id", &args.installation_id)?;
                Ok(value)
            },
            AppAuthoringCommand::RewindPreview(args) => {
                let passphrase = read_archive_passphrase(&args.passphrase_file)?;
                let url = live_app_url(
                    &args.live,
                    &[
                        "updates",
                        args.migration_run_id.as_str(),
                        "rewind-preview",
                    ],
                )?;
                let response = live_request(&client, reqwest::Method::POST, url, &args.live)
                    .header(
                        "x-magician-app-archive-passphrase",
                        reqwest::header::HeaderValue::from_bytes(&passphrase)
                            .context("archive passphrase is not a valid HTTP header value")?,
                    )
                    .send()
                    .await
                    .context("calling the live data-rewind preview owner")?;
                let value = parse_live_json_response(response, MAX_LIVE_JSON_BYTES).await?;
                require_correlated_string(&value, "migration_run_id", &args.migration_run_id)?;
                Ok(value)
            },
            AppAuthoringCommand::RewindCommit(args) => {
                if !args.confirm_data_rewind {
                    anyhow::bail!("data rewind requires --confirm-data-rewind");
                }
                let passphrase = read_archive_passphrase(&args.passphrase_file)?;
                let url = live_app_url(
                    &args.live,
                    &[
                        "updates",
                        args.migration_run_id.as_str(),
                        "rewind-commit",
                    ],
                )?;
                let response = live_request(&client, reqwest::Method::POST, url, &args.live)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .header(
                        "x-magician-app-archive-passphrase",
                        reqwest::header::HeaderValue::from_bytes(&passphrase)
                            .context("archive passphrase is not a valid HTTP header value")?,
                    )
                    .json(&serde_json::json!({
                        "migration_run_id": args.migration_run_id,
                        "preview_digest": args.preview_digest,
                        "request_id": args.request_id,
                        "explicit_rewind_confirmed": true,
                    }))
                    .send()
                    .await
                    .context("calling the live data-rewind commit owner")?;
                parse_live_json_response(response, MAX_LIVE_JSON_BYTES).await
            },
            AppAuthoringCommand::ReenableReview(args) => {
                let url = live_app_url(
                    &args.live,
                    &[
                        "installations",
                        args.installation_id.as_str(),
                        "reenable-review",
                    ],
                )?;
                let value =
                    live_json_request(&client, reqwest::Method::GET, url, &args.live, None).await?;
                require_correlated_string(&value, "installation_id", &args.installation_id)?;
                Ok(value)
            },
            AppAuthoringCommand::Reenable(args) => {
                let url = live_app_url(
                    &args.live,
                    &["installations", args.installation_id.as_str(), "reenable"],
                )?;
                let value = live_json_request(
                    &client,
                    reqwest::Method::POST,
                    url,
                    &args.live,
                    Some(serde_json::json!({
                        "expected_generation": args.expected_generation,
                        "request_id": args.request_id,
                        "review_digest": args.review_digest,
                    })),
                )
                .await?;
                validate_lifecycle_receipt(
                    &value,
                    &args.installation_id,
                    args.expected_generation,
                    Some("enabled"),
                )?;
                Ok(value)
            },
            AppAuthoringCommand::PackageImport(args) => {
                let bytes = read_bounded_regular_file(
                    &args.archive,
                    APP_PACKAGE_ARCHIVE_MAX_BYTES,
                    "package archive",
                )?;
                admit_package_archive(&bytes)
                    .context("the local package-only archive was rejected")?;
                let url = live_app_url(&args.live, &["packages", "import"])?;
                let response = live_request(&client, reqwest::Method::POST, url, &args.live)
                    .header(reqwest::header::CONTENT_TYPE, APP_PACKAGE_ARCHIVE_MEDIA_TYPE)
                    .body(bytes)
                    .send()
                    .await
                    .context("calling the live package-import owner")?;
                let value = parse_live_json_response(response, MAX_LIVE_JSON_BYTES).await?;
                if value.get("state").and_then(serde_json::Value::as_str)
                    != Some("staged_for_local_conformance")
                    || value
                        .get("foreign_authority_transferred")
                        .and_then(serde_json::Value::as_bool)
                        != Some(false)
                    || value
                        .pointer("/requirements/foreign_grants_transfer")
                        .and_then(serde_json::Value::as_bool)
                        != Some(false)
                    || [
                        "reverify_complete_bundle_digest",
                        "run_local_conformance",
                        "run_local_permission_review",
                        "rebuild_verify_and_sandbox_executable_content_if_present",
                    ]
                    .into_iter()
                    .any(|field| {
                        value
                            .pointer(&format!("/requirements/{field}"))
                            .and_then(serde_json::Value::as_bool)
                            != Some(true)
                    })
                    || value
                        .get("local_identity_resolution_required")
                        .and_then(serde_json::Value::as_bool)
                        != Some(true)
                {
                    anyhow::bail!("the package-import response widened portable authority");
                }
                Ok(value)
            },
            AppAuthoringCommand::CandidatePublish(args) => {
                let request_id = magician::magician_v2::apps::models::AppReference::parse(
                    args.request_id.clone(),
                )
                .context("candidate request id is invalid")?;
                let bytes = read_bounded_regular_file(
                    &args.archive,
                    APP_PACKAGE_ARCHIVE_MAX_BYTES,
                    "package archive",
                )?;
                let admitted = admit_package_archive(&bytes)
                    .context("the local package-only archive was rejected")?;
                let package = admitted.package();
                let expected_package_id = package.package_id.to_string();
                let expected_publisher = package.publisher_identity.to_string();
                let expected_digest = package.package_content_digest.to_string();
                let url = live_app_url(&args.live, &["packages", "candidates"])?;
                let mut request = live_request(&client, reqwest::Method::POST, url, &args.live)
                    .header(reqwest::header::CONTENT_TYPE, APP_PACKAGE_ARCHIVE_MEDIA_TYPE)
                    .header("x-magician-app-request-id", request_id.as_str())
                    .header("x-magician-app-package-id", &expected_package_id)
                    .header("x-magician-app-source-publisher", &expected_publisher)
                    .header("x-magician-app-content-digest", &expected_digest)
                    .body(bytes);
                if let (Some(installation_id), Some(kind)) =
                    (args.installation_id.as_deref(), args.attempt_kind)
                {
                    request = request
                        .header("x-magician-app-target-installation", installation_id)
                        .header("x-magician-app-attempt-kind", kind.as_str());
                }
                let response = request
                    .send()
                    .await
                    .context("calling the live candidate-publication owner")?;
                let value = parse_live_json_response(response, MAX_LIVE_JSON_BYTES).await?;
                require_correlated_string(&value, "request_id", request_id.as_str())?;
                require_correlated_string(
                    &value,
                    "source_publisher_identity",
                    &expected_publisher,
                )?;
                require_correlated_string(
                    &value,
                    "package_content_digest",
                    &expected_digest,
                )?;
                let expected_state = match args.attempt_kind {
                    None => "ready_for_review",
                    Some(
                        magician::magician_v2::apps::authoring::AppRevisionCandidateKindArg::Update,
                    ) => "update_pending",
                    Some(
                        magician::magician_v2::apps::authoring::AppRevisionCandidateKindArg::Reinstall,
                    ) => "uninstalled_retained",
                };
                if value.get("state").and_then(serde_json::Value::as_str)
                    != Some(expected_state)
                    || value
                        .get("activation_authority_granted")
                        .and_then(serde_json::Value::as_bool)
                        != Some(false)
                {
                    anyhow::bail!(
                        "the candidate-publication response did not preserve inert review state"
                    );
                }
                Ok(value)
            },
            AppAuthoringCommand::DataExport(args) => {
                let request_id = magician::magician_v2::apps::models::AppReference::parse(
                    args.request_id.clone(),
                )
                .context("data-export request id is invalid")?;
                let passphrase_bytes = args
                    .passphrase_file
                    .as_ref()
                    .map(|path| read_archive_passphrase(path))
                    .transpose()?;
                if !args.explicit_plaintext && passphrase_bytes.is_none() {
                    anyhow::bail!("encrypted data export requires --passphrase-file");
                }
                let url = live_app_url(
                    &args.live,
                    &[
                        "installations",
                        args.installation_id.as_str(),
                        "portable-exports",
                    ],
                )?;
                let mut request = live_request(&client, reqwest::Method::POST, url, &args.live)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .json(&serde_json::json!({
                        "request_id": request_id,
                        "kind": args.kind.as_str(),
                        "protection": if args.explicit_plaintext { "explicit_plaintext" } else { "default" },
                        "warned_plaintext_confirmed": args.explicit_plaintext,
                    }));
                if let Some(bytes) = passphrase_bytes.as_ref() {
                    request = request.header(
                        "x-magician-app-archive-passphrase",
                        reqwest::header::HeaderValue::from_bytes(bytes)
                            .context("archive passphrase is not a valid HTTP header value")?,
                    );
                }
                let response = request.send().await.context("calling the live data-export owner")?;
                let status = response.status();
                if !status.is_success() {
                    let _ = read_bounded_http_bytes(response, MAX_LIVE_JSON_BYTES).await?;
                    anyhow::bail!("the live data-export owner rejected the request ({status})");
                }
                let response_request_id = response
                    .headers()
                    .get("x-magician-app-request-id")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let response_logical_digest = response
                    .headers()
                    .get("x-magician-app-logical-digest")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let response_envelope_digest = response
                    .headers()
                    .get("x-magician-app-envelope-digest")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let response_ciphertext_digest = response
                    .headers()
                    .get("x-magician-app-ciphertext-digest")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let expected_media_type = if args.explicit_plaintext {
                    APP_PORTABLE_ARCHIVE_PLAINTEXT_MEDIA_TYPE
                } else {
                    APP_PORTABLE_ARCHIVE_ENCRYPTED_MEDIA_TYPE
                };
                let actual_media_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.split(';').next())
                    .map(str::trim);
                if actual_media_type != Some(expected_media_type) {
                    anyhow::bail!("the data-export owner returned an unexpected media type");
                }
                let bytes = read_bounded_http_bytes(response, APP_PORTABLE_ARCHIVE_MAX_BYTES).await?;
                let passphrase = passphrase_bytes
                    .as_deref()
                    .map(AppArchivePassphrase::parse)
                    .transpose()
                    .context("archive passphrase is invalid")?;
                let decoded = decode_app_portable_archive(&bytes, passphrase.as_ref())
                    .context("the exported archive failed local authentication")?;
                if response_request_id.as_deref() != Some(request_id.as_str())
                    || response_logical_digest.as_deref()
                        != Some(decoded.receipt.logical_payload_digest.as_str())
                    || response_envelope_digest.as_deref()
                        != Some(decoded.receipt.envelope_header_digest.as_str())
                    || response_ciphertext_digest.as_deref()
                        != Some(decoded.receipt.ciphertext_digest.as_str())
                {
                    anyhow::bail!("the data-export receipt did not match the exact response bytes");
                }
                if matches!(args.kind, magician::magician_v2::apps::authoring::AppPortableExportKindArg::Combined)
                    != decoded.package_archive.is_some()
                {
                    anyhow::bail!("the exported archive kind did not match the request");
                }
                write_create_new_atomic(&args.output, &bytes)?;
                Ok(serde_json::json!({
                    "state": "exported",
                    "kind": args.kind.as_str(),
                    "encrypted": decoded.receipt.encrypted,
                    "byte_count": bytes.len(),
                    "logical_payload_digest": decoded.receipt.logical_payload_digest,
                    "envelope_header_digest": decoded.receipt.envelope_header_digest,
                }))
            },
            AppAuthoringCommand::DataImportPreview(args) => {
                let request_id = magician::magician_v2::apps::models::AppReference::parse(
                    args.request_id.clone(),
                )
                .context("data-import preview request id is invalid")?;
                let bytes = read_bounded_regular_file(
                    &args.archive,
                    APP_PORTABLE_ARCHIVE_MAX_BYTES,
                    "app data archive",
                )?;
                let passphrase_bytes = args
                    .passphrase_file
                    .as_ref()
                    .map(|path| read_archive_passphrase(path))
                    .transpose()?;
                let passphrase = passphrase_bytes
                    .as_deref()
                    .map(AppArchivePassphrase::parse)
                    .transpose()
                    .context("archive passphrase is invalid")?;
                let decoded = decode_app_portable_archive(&bytes, passphrase.as_ref())
                    .context("the local app data archive was rejected")?;
                if !decoded.receipt.encrypted && passphrase_bytes.is_some() {
                    anyhow::bail!("a plaintext archive does not accept --passphrase-file");
                }
                if matches!(decoded.logical, magician::magician_v2::apps::portability::AppLogicalArchive::Package { .. }) {
                    anyhow::bail!("package-only archives use `magician app package-import`");
                }
                let media_type = if decoded.receipt.encrypted {
                    APP_PORTABLE_ARCHIVE_ENCRYPTED_MEDIA_TYPE
                } else {
                    APP_PORTABLE_ARCHIVE_PLAINTEXT_MEDIA_TYPE
                };
                let url = live_app_url(
                    &args.live,
                    &["installations", args.installation_id.as_str(), "data-imports", "preview"],
                )?;
                let mut request = live_request(&client, reqwest::Method::POST, url, &args.live)
                    .header(reqwest::header::CONTENT_TYPE, media_type)
                    .header("x-magician-app-request-id", request_id.as_str())
                    .body(bytes);
                if let Some(value) = passphrase_bytes.as_ref() {
                    request = request.header(
                        "x-magician-app-archive-passphrase",
                        reqwest::header::HeaderValue::from_bytes(value)
                            .context("archive passphrase is not a valid HTTP header value")?,
                    );
                }
                let value = parse_live_json_response(
                    request.send().await.context("calling the live data-import preview owner")?,
                    MAX_LIVE_JSON_BYTES,
                ).await?;
                require_correlated_string(&value, "request_id", request_id.as_str())?;
                for (field, expected) in [
                    (
                        "logical_payload_digest",
                        decoded.receipt.logical_payload_digest.as_str(),
                    ),
                    (
                        "envelope_header_digest",
                        decoded.receipt.envelope_header_digest.as_str(),
                    ),
                    (
                        "ciphertext_digest",
                        decoded.receipt.ciphertext_digest.as_str(),
                    ),
                ] {
                    if value
                        .pointer(&format!("/archive_receipt/{field}"))
                        .and_then(serde_json::Value::as_str)
                        != Some(expected)
                    {
                        anyhow::bail!("the data-import preview receipt did not match the exact archive");
                    }
                }
                if value.get("foreign_authority_transferred").and_then(serde_json::Value::as_bool) != Some(false) {
                    anyhow::bail!("the data-import preview widened portable authority");
                }
                Ok(value)
            },
            AppAuthoringCommand::DataImportApprove(args) => {
                let url = live_app_url(
                    &args.live,
                    &["installations", args.installation_id.as_str(), "data-imports", "approve"],
                )?;
                let value = live_json_request(
                    &client,
                    reqwest::Method::POST,
                    url,
                    &args.live,
                    Some(serde_json::json!({
                        "request_id": args.request_id,
                        "preview_digest": args.preview_digest,
                    })),
                ).await?;
                require_correlated_string(&value, "request_id", &args.request_id)?;
                require_correlated_string(&value, "preview_digest", &args.preview_digest)?;
                Ok(value)
            },
            AppAuthoringCommand::DataImportCommit(args) => {
                let url = live_app_url(
                    &args.live,
                    &["installations", args.installation_id.as_str(), "data-imports", "commit"],
                )?;
                let value = live_json_request(
                    &client,
                    reqwest::Method::POST,
                    url,
                    &args.live,
                    Some(serde_json::json!({
                        "request_id": args.request_id,
                        "preview_digest": args.preview_digest,
                        "approval_ref": args.approval_ref,
                    })),
                ).await?;
                require_correlated_string(&value, "request_id", &args.request_id)?;
                if value.get("foreign_authority_transferred").and_then(serde_json::Value::as_bool) != Some(false) {
                    anyhow::bail!("the data-import commit widened portable authority");
                }
                Ok(value)
            },
            AppAuthoringCommand::PackageExport(args) => {
                let url = live_app_url(
                    &args.live,
                    &[
                        "installations",
                        args.installation_id.as_str(),
                        "package-export",
                    ],
                )?;
                let response = live_request(&client, reqwest::Method::GET, url, &args.live)
                    .send()
                    .await
                    .context("calling the live package-export owner")?;
                let status = response.status();
                if !status.is_success() {
                    let _ = read_bounded_http_bytes(response, MAX_LIVE_JSON_BYTES).await?;
                    anyhow::bail!("the live package-export owner rejected the request ({status})");
                }
                let media_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.split(';').next())
                    .map(str::trim);
                if media_type != Some(APP_PACKAGE_ARCHIVE_MEDIA_TYPE) {
                    anyhow::bail!("the live package-export owner returned an unexpected media type");
                }
                let bytes = read_bounded_http_bytes(response, APP_PACKAGE_ARCHIVE_MAX_BYTES).await?;
                admit_package_archive(&bytes)
                    .context("the exported package-only archive was rejected")?;
                write_create_new_atomic(&args.output, &bytes)?;
                Ok(serde_json::json!({
                    "state": "exported",
                    "byte_count": bytes.len(),
                    "archive_digest": magician::magician_v2::apps::models::AppDigest::blake3(&bytes),
                }))
            },
            AppAuthoringCommand::PurgePreview(args) => {
                let url = live_app_url(
                    &args.live,
                    &[
                        "installations",
                        args.installation_id.as_str(),
                        "purge-preview",
                    ],
                )?;
                let value = live_json_request(
                    &client,
                    reqwest::Method::POST,
                    url,
                    &args.live,
                    Some(serde_json::json!({})),
                )
                .await?;
                require_correlated_string(&value, "installation_id", &args.installation_id)?;
                Ok(value)
            },
            AppAuthoringCommand::PurgeCommit(args) => {
                let bytes = read_bounded_regular_file(
                    &args.preview,
                    MAX_LIVE_JSON_BYTES,
                    "purge preview JSON",
                )?;
                let document: serde_json::Value = serde_json::from_slice(&bytes)
                    .context("the purge preview file is not valid JSON")?;
                let preview = if document.get("ok") == Some(&serde_json::Value::Bool(true)) {
                    document
                        .get("result")
                        .cloned()
                        .context("the CLI envelope has no purge preview result")?
                } else {
                    document
                };
                if preview.get("installation_id").and_then(serde_json::Value::as_str)
                    != Some(args.installation_id.as_str())
                {
                    anyhow::bail!("the purge preview belongs to a different installation");
                }
                let body = serde_json::json!({
                    "preview_ref": required_json_field(&preview, "preview_ref")?,
                    "preview_digest": required_json_field(&preview, "preview_digest")?,
                    "installation_generation": required_json_field(&preview, "installation_generation")?,
                    "observed_at": required_json_field(&preview, "observed_at")?,
                    "expires_at": required_json_field(&preview, "expires_at")?,
                    "idempotency_key": args.idempotency_key,
                });
                let url = live_app_url(
                    &args.live,
                    &["installations", args.installation_id.as_str(), "purge"],
                )?;
                let value = live_json_request(
                    &client,
                    reqwest::Method::POST,
                    url,
                    &args.live,
                    Some(body),
                )
                .await?;
                require_correlated_string(&value, "installation_id", &args.installation_id)?;
                require_correlated_string(
                    &value,
                    "preview_digest",
                    required_json_field(&preview, "preview_digest")?
                        .as_str()
                        .context("the purge preview digest is invalid")?,
                )?;
                Ok(value)
            },
            AppAuthoringCommand::PurgeStatus(args) => {
                let url = live_app_url(
                    &args.live,
                    &["purges", args.idempotency_key.as_str()],
                )?;
                let value =
                    live_json_request(&client, reqwest::Method::GET, url, &args.live, None).await?;
                let digest = magician::magician_v2::apps::models::AppDigest::parse(
                    args.idempotency_key.clone(),
                )?;
                let suffix = digest
                    .as_str()
                    .strip_prefix("blake3:")
                    .unwrap_or(digest.as_str());
                require_correlated_string(
                    &value,
                    "approval_ref",
                    &format!("app-purge-approval:{suffix}"),
                )?;
                Ok(value)
            },
            _ => anyhow::bail!("provider-free command reached the live Apps client"),
        }
    }
    .await;
    magician::magician_v2::apps::authoring::emit_app_authoring_result(command, json, result)
}

fn optional_json_array(values: &[String]) -> Option<&[String]> {
    (!values.is_empty()).then_some(values)
}

fn validate_live_scope_value(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 192 || value.chars().any(char::is_control) {
        anyhow::bail!("the live Apps {label} is missing or invalid");
    }
    Ok(())
}

fn live_app_url(
    live: &magician::magician_v2::apps::authoring::AppLiveScopeArgs,
    tail: &[&str],
) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(live.api_base.trim())
        .context("the live Apps API base URL is invalid")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        anyhow::bail!("the live Apps API base URL must be an origin-only HTTP(S) URL");
    }
    let loopback = match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    };
    if !loopback {
        anyhow::bail!("the live Apps API base URL must resolve literally to loopback");
    }
    url.set_path("");
    let mut segments = url
        .path_segments_mut()
        .map_err(|_| anyhow::anyhow!("the live Apps API base URL cannot carry path segments"))?;
    segments.extend(["api", "magician", "v2", "apps"]);
    for segment in tail {
        validate_live_scope_value("path identity", segment)?;
        segments.push(segment);
    }
    drop(segments);
    Ok(url)
}

fn live_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: reqwest::Url,
    _live: &magician::magician_v2::apps::authoring::AppLiveScopeArgs,
) -> reqwest::RequestBuilder {
    let request = client
        .request(method, url)
        .header(reqwest::header::ACCEPT, "application/json");
    let token = std::env::var("MAGICIAN_BEARER_TOKEN")
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty());
    if let Some(token) = token {
        request.bearer_auth(token)
    } else {
        request
    }
}

async fn live_json_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: reqwest::Url,
    live: &magician::magician_v2::apps::authoring::AppLiveScopeArgs,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let mut request = live_request(client, method, url, live);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .context("calling the authenticated live Apps owner")?;
    parse_live_json_response(response, 4 * 1_048_576).await
}

async fn live_lifecycle_control(
    client: &reqwest::Client,
    args: &magician::magician_v2::apps::authoring::AppLifecycleControlArgs,
    route: &str,
    expected_status: Option<&str>,
) -> Result<serde_json::Value> {
    let url = live_app_url(
        &args.live,
        &["installations", args.installation_id.as_str(), route],
    )?;
    let value = live_json_request(
        client,
        reqwest::Method::POST,
        url,
        &args.live,
        Some(serde_json::json!({
            "expected_generation": args.expected_generation,
            "request_id": args.request_id,
        })),
    )
    .await?;
    validate_lifecycle_receipt(
        &value,
        &args.installation_id,
        args.expected_generation,
        expected_status,
    )?;
    Ok(value)
}

fn validate_lifecycle_receipt(
    value: &serde_json::Value,
    installation_id: &str,
    expected_generation: u64,
    expected_status: Option<&str>,
) -> Result<()> {
    require_correlated_string(value, "installation_id", installation_id)?;
    if value.get("generation").and_then(serde_json::Value::as_u64)
        != expected_generation.checked_add(1)
    {
        anyhow::bail!("the live lifecycle receipt generation was not correlated");
    }
    if let Some(expected_status) = expected_status {
        require_correlated_string(value, "status", expected_status)?;
    } else if value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .is_none()
    {
        anyhow::bail!("the live lifecycle receipt omitted its status");
    }
    Ok(())
}

fn require_correlated_string(value: &serde_json::Value, field: &str, expected: &str) -> Result<()> {
    if value.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
        anyhow::bail!("the live Apps response was not correlated to the request");
    }
    Ok(())
}

async fn parse_live_json_response(
    response: reqwest::Response,
    maximum_bytes: usize,
) -> Result<serde_json::Value> {
    let status = response.status();
    let bytes = read_bounded_http_bytes(response, maximum_bytes).await?;
    if !status.is_success() {
        let code = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|code| {
                code.len() <= 96
                    && code.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
            .unwrap_or_else(|| "app_live_api_rejected".to_owned());
        anyhow::bail!("the live Apps owner rejected the request ({status}, {code})");
    }
    serde_json::from_slice(&bytes).context("the live Apps owner returned invalid JSON")
}

async fn read_bounded_http_bytes(
    mut response: reqwest::Response,
    maximum_bytes: usize,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        anyhow::bail!("the live Apps response exceeds its fixed byte limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("reading the bounded live Apps response")?
    {
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .context("the live Apps response length overflowed")?;
        if next > maximum_bytes {
            anyhow::bail!("the live Apps response exceeds its fixed byte limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn read_bounded_regular_file(path: &Path, maximum_bytes: usize, label: &str) -> Result<Vec<u8>> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("opening the bounded {label}"))?;
    if !metadata.file_type().is_file() || metadata.len() > maximum_bytes as u64 {
        anyhow::bail!("the {label} must be a bounded regular file");
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .with_context(|| format!("opening the bounded {label}"))?;
    let opened = file
        .metadata()
        .with_context(|| format!("inspecting the bounded {label}"))?;
    if !opened.is_file() || opened.len() > maximum_bytes as u64 {
        anyhow::bail!("the {label} must be a bounded regular file");
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    Read::take(file, maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading the bounded {label}"))?;
    if bytes.len() > maximum_bytes {
        anyhow::bail!("the {label} exceeds its fixed byte limit");
    }
    Ok(bytes)
}

fn read_archive_passphrase(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = read_bounded_regular_file(path, 1_026, "archive passphrase file")?;
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    if !(12..=1_024).contains(&bytes.len())
        || bytes.iter().any(|byte| !(b' '..=b'~').contains(byte))
    {
        anyhow::bail!("the archive passphrase file must contain 12-1024 printable ASCII bytes");
    }
    Ok(bytes)
}

fn write_create_new_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !fs::metadata(parent)
        .with_context(|| "opening the package-export parent directory")?
        .is_dir()
    {
        anyhow::bail!("the package-export parent is not a directory");
    }
    if fs::symlink_metadata(path).is_ok() {
        anyhow::bail!("the package-export destination already exists");
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .context("the package-export destination has no file name")?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.partial",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let write_result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temporary)
            .context("creating the private package-export staging file")?;
        file.write_all(bytes)
            .context("writing the package-export staging file")?;
        file.sync_all()
            .context("syncing the package-export staging file")?;
        fs::hard_link(&temporary, path)
            .context("publishing the package export without overwrite")?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    write_result
}

fn required_json_field<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Result<&'a serde_json::Value> {
    value
        .get(field)
        .filter(|value| !value.is_null())
        .with_context(|| format!("the purge preview is missing {field}"))
}

fn finish_app_authoring_status(
    status: magician::magician_v2::apps::authoring::AppAuthoringCliExitStatus,
) -> Result<()> {
    match status {
        magician::magician_v2::apps::authoring::AppAuthoringCliExitStatus::Success => Ok(()),
        magician::magician_v2::apps::authoring::AppAuthoringCliExitStatus::CommandFailed => {
            std::process::exit(1)
        },
    }
}

async fn run_cli_command(config: &MagicianConfig, command: &CliCommand) -> Result<()> {
    match command {
        CliCommand::App { json, command } => {
            let status = run_live_app_command(command, *json).await?;
            finish_app_authoring_status(status)
        },
        CliCommand::OllamaLaunchConfig => print_ollama_launch_config(config),
        CliCommand::LogicalChunkEval(args) => run_logical_chunk_eval(config, args).await,
        CliCommand::ReconcileOrphanedTasks(args) => run_reconcile_orphaned_tasks(args).await,
        CliCommand::MemoryIndex { command } => run_memory_index_command(config, command).await,
        CliCommand::SealStatelessLoopCutover(args) => {
            run_seal_stateless_loop_cutover(config, args).await
        },
        CliCommand::TownSquareMigrate(args) => run_town_square_migrate(config, args).await,
        CliCommand::DistillEvidence(args) => run_distill_evidence(config, args).await,
        CliCommand::Review(args) => run_review(config, args).await,
        CliCommand::EvidenceEval(args) => run_evidence_quality(config, args).await,
        CliCommand::EvidenceGraph(args) => run_evidence_graph(config, args).await,
        CliCommand::EvidenceClaims(args) => run_evidence_claims(config, args).await,
        CliCommand::EvidencePrecision(args) => run_evidence_precision(config, args).await,
        CliCommand::WorklogExport(args) => run_worklog_export(config, args).await,
        CliCommand::Analytics { command } => run_analytics_command(config, command).await,
        CliCommand::AttentionLearning { command } => {
            run_attention_learning_command(config, command).await
        },
        CliCommand::ChannelAssist { command } => run_channel_assist_command(config, command).await,
        CliCommand::Storage { command } => run_storage_activation_command(command).await,
    }
}

async fn run_storage_activation_command(
    command: &magician::magician_v2::storage_activation::StorageCommand,
) -> Result<()> {
    let report =
        magician::magician_v2::storage_activation::run_storage_command(command.clone()).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.ok {
        anyhow::bail!("storage activation operation blocked");
    }
    Ok(())
}

/// One-shot Town Square corpus migration.
///
/// A CLI command rather than an HTTP route on purpose: this is operator-run,
/// runs once, needs no auth surface, and should not stay network-reachable for
/// the rest of the product's life.
///
/// The command refuses rather than guesses in the two places where guessing
/// would be destructive — no enabled Town Square installation, or a source
/// store that cannot be read. `migrate_town_square_corpus` derives its source
/// store from the same scope as its destination, so this cannot move one
/// principal's square into another's installation.
async fn run_town_square_migrate(
    config: &MagicianConfig,
    args: &TownSquareMigrateArgs,
) -> Result<()> {
    use magician::magician_v2::apps::{
        authority::AuthenticatedAppScope,
        entity_store::AppEntityStoreService,
        models::{AppDigest, AppReference, AppScopeBindingRef},
        records::AppScope,
        registry::AppRegistryService,
        town_square_migration::migrate_town_square_corpus_with_options,
    };
    use magician::magician_v2::social::store::SocialStoreRegistry;

    let workspace = resolve_storage_workspace(config)?;
    let now = chrono::Utc::now();

    let scope = AppScope {
        principal: AppReference::parse(args.principal.clone())?,
        workspace: AppReference::parse(args.workspace.clone())?,
    };
    let digest = AppDigest::blake3(format!("{}\0{}", args.principal, args.workspace).as_bytes());
    let scope_binding_ref = AppScopeBindingRef::parse(format!(
        "scope_{}",
        digest.as_str().trim_start_matches("blake3:")
    ))?;
    let authenticated = AuthenticatedAppScope::from_system_worker(
        scope,
        scope_binding_ref,
        AppReference::parse("worker:town-square-migration")?,
        AppReference::parse(format!(
            "run:town-square-migration:{}",
            uuid::Uuid::new_v4()
        ))?,
        now,
        now + chrono::Duration::minutes(10),
    )
    .map_err(|error| anyhow::anyhow!("minting the migration scope: {error}"))?;

    let registry = AppRegistryService::new(workspace.clone());
    let installations = registry
        .enabled_installations_bounded(&authenticated, 256, now)
        .await
        .map_err(|error| anyhow::anyhow!("listing installations: {error}"))?;
    let mut town_square = None;
    for installation in installations {
        let revision = registry
            .package_revision(&authenticated, &installation.package_revision_ref, now)
            .await
            .map_err(|error| anyhow::anyhow!("reading a package revision: {error}"))?;
        let Some(revision) = revision else { continue };
        if revision.package_id.as_str() == "app:town-square" {
            town_square = Some(installation.installation_id);
            break;
        }
    }
    // Refusing beats guessing: with no destination there is nothing to migrate
    // INTO, and inventing one would put the corpus somewhere nothing reads.
    let installation_id = town_square.ok_or_else(|| {
        anyhow::anyhow!(
            "no enabled `app:town-square` installation in {}/{} — boot the server once so \
             system-package boot admission installs it, then re-run",
            args.principal,
            args.workspace
        )
    })?;

    let social = SocialStoreRegistry::new(workspace.clone());
    let entities = AppEntityStoreService::new(registry);
    let report = migrate_town_square_corpus_with_options(
        &social,
        &entities,
        &authenticated,
        &installation_id,
        chrono::Utc::now(),
        args.history_only,
    )
    .await
    .map_err(|error| anyhow::anyhow!("town square migration: {error}"))?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "town square migration: {} ({} written, {} updated, {} already present, {} batches)",
            if report.already_complete {
                "already complete"
            } else {
                "applied"
            },
            report.rows_written,
            report.rows_updated,
            report.rows_already_present,
            report.batches_issued
        );
        for table in &report.tables {
            println!(
                "  {:<18} source={:<6} store={:<6} matched={:<6} divergent={:<6} {}",
                table.source_table,
                table.source_rows,
                table.store_rows,
                table.matched_rows,
                table.divergent_rows,
                if table.is_faithful() {
                    "ok"
                } else {
                    "DIVERGENT"
                }
            );
        }
    }

    // The exit status is the go/no-go the cutover depends on. A migration that
    // "succeeded" while a table diverged must not read as success, because the
    // next step retires the source.
    if !report.is_faithful() {
        anyhow::bail!(
            "migration is not faithful — do NOT retire the social store; \
             re-run after investigating the divergent tables above"
        );
    }
    Ok(())
}

async fn run_seal_stateless_loop_cutover(
    config: &MagicianConfig,
    args: &StatelessLoopCutoverArgs,
) -> Result<()> {
    if !args.confirm_legacy_writers_drained {
        anyhow::bail!(
            "refusing irreversible stateless-loop cutover without \
            --confirm-legacy-writers-drained"
        );
    }
    validate_stateless_loop_cutover_scope_component("principal", &args.principal)?;
    validate_stateless_loop_cutover_scope_component("workspace", &args.workspace)?;
    validate_stateless_loop_cutover_deployment_id(&args.deployment_id)?;
    let workspace = resolve_storage_workspace(config)?;
    let store =
        magician::magician_v2::execution::agentic::run_loop::store::fs::FsLoopStateStore::new(
            workspace.base_root(),
        );
    let already_sealed = store
        .legacy_writers_retired(&args.principal, &args.workspace)
        .await
        .map_err(|error| anyhow::anyhow!("reading stateless-loop cutover: {error}"))?;
    store
        .seal_legacy_writer_cutover(
            &args.principal,
            &args.workspace,
            &args.deployment_id,
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("sealing stateless-loop cutover: {error}"))?;
    println!(
        "{}",
        serde_json::json!({
            "schema_version": 1,
            "principal": args.principal,
            "workspace": args.workspace,
            "deployment_id": args.deployment_id,
            "legacy_writers_retired": true,
            "already_sealed": already_sealed,
        })
    );
    Ok(())
}

fn validate_stateless_loop_cutover_scope_component(label: &str, value: &str) -> Result<()> {
    let (canonical, _) =
        magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::scope_dir_segments(
            value, value,
        );
    anyhow::ensure!(
        canonical == value,
        "stateless-loop cutover {label} must already be a canonical scoped-storage segment"
    );
    Ok(())
}

fn validate_stateless_loop_cutover_deployment_id(value: &str) -> Result<()> {
    anyhow::ensure!(
        value == value.trim()
            && !value.is_empty()
            && value.len() <= 128
            && !value.chars().any(char::is_control),
        "stateless-loop cutover deployment id must be canonical, non-empty, and at most 128 bytes"
    );
    Ok(())
}

async fn run_logical_chunk_eval(
    config: &MagicianConfig,
    args: &LogicalChunkEvalArgs,
) -> Result<()> {
    if args.execute {
        load_runtime_env_files();
    }
    let fixture_text = std::fs::read_to_string(&args.fixtures)
        .with_context(|| format!("reading {}", args.fixtures.display()))?;
    let fixtures: magician_chunking::shadow_eval::ChunkEvalFixtureSuite =
        serde_json::from_str(&fixture_text)
            .with_context(|| format!("parsing {}", args.fixtures.display()))?;
    let router_config = config
        .router_config()
        .cloned()
        .context("no llm.router configured in magician-config.yaml")?;
    let registry = magician::magician_v2::llm_chunking::global_chunk_adapter_registry()
        .read()
        .map_err(|_| anyhow::anyhow!("logical chunk adapter registry lock is poisoned"))?
        .clone();
    let report = magician_chunking::shadow_eval::run_chunk_shadow_eval(
        &router_config,
        registry,
        fixtures,
        args.execute,
        args.compare_cloud,
        args.repeats,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    let rendered = serde_json::to_string_pretty(&report)?;
    if let Some(output) = &args.output {
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(output, format!("{rendered}\n"))
            .with_context(|| format!("writing {}", output.display()))?;
    }
    println!("{rendered}");
    if !report.all_plans_valid || report.all_live_runs_valid == Some(false) {
        anyhow::bail!("logical chunk eval gates did not pass");
    }
    Ok(())
}

fn print_ollama_launch_config(config: &MagicianConfig) -> Result<()> {
    let ollama = &config.runtime.ollama;
    let generation_models = magician::config::resolved_ollama_generation_models(config)?;
    // Cloud locality leaves the generation set empty on purpose (the router's
    // `when_cloud` arms serve those operations), and the daemon still hosts the
    // embedder, so size its context from that instead of refusing to answer.
    // Refusing made `run-ollama.sh` fall back to a resolver that knew nothing
    // about locality, which prewarmed the local generation model anyway.
    let daemon_context_tokens = generation_models
        .iter()
        .map(|entry| entry.context_tokens)
        .max()
        .unwrap_or(ollama.embedding_context_tokens);
    println!("max_loaded_models={}", ollama.max_loaded_models);
    println!("daemon_context_tokens={daemon_context_tokens}");
    println!("generation_model_count={}", generation_models.len());
    for (index, entry) in generation_models.iter().enumerate() {
        println!("generation_model_{index}={}", entry.model);
        println!("generation_context_tokens_{index}={}", entry.context_tokens);
    }
    println!(
        "embedding_context_tokens={}",
        ollama.embedding_context_tokens
    );
    println!("embedding_base_url={}", ollama.embedding_base_url);
    println!("embedding_keep_alive={}", ollama.embedding_keep_alive);
    println!("embedding_num_parallel={}", ollama.embedding_num_parallel);
    println!(
        "embedding_max_loaded_models={}",
        ollama.embedding_max_loaded_models
    );
    println!(
        "embedding_query_timeout_ms={}",
        ollama.embedding_query_timeout_ms
    );
    println!(
        "embedding_write_timeout_ms={}",
        ollama.embedding_write_timeout_ms
    );
    println!("embedding_batch_tokens={}", ollama.embedding_batch_tokens);
    println!("embedding_batch_size={}", ollama.embedding_batch_size);
    println!("embedding_model={}", ollama.embedding_model);
    println!("embedding_dimensions={}", ollama.embedding_dimensions);
    println!("kv_cache_type={}", ollama.kv_cache_type);
    println!("flash_attention={}", ollama.flash_attention);
    println!("prewarm={}", ollama.prewarm);
    println!(
        "replace_existing_local_daemon={}",
        ollama.replace_existing_local_daemon
    );
    println!(
        "keep_alive={}",
        ollama.keep_alive.as_deref().unwrap_or_default()
    );
    Ok(())
}

async fn run_reconcile_orphaned_tasks(args: &ReconcileOrphanedTasksArgs) -> Result<()> {
    let endpoint = format!(
        "{}/api/magician/v2/admin/tasks/reconcile-orphaned-ready",
        args.api_base.trim_end_matches('/')
    );
    let request = reqwest::Client::new()
        .post(&endpoint)
        .json(&serde_json::json!({
            "batch_cap": args.batch_cap,
            "dry_run": !args.apply,
        }));
    let token = std::env::var("MAGICIAN_BEARER_TOKEN")
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty());
    let request = if let Some(token) = token {
        request.bearer_auth(token)
    } else {
        request
    };
    let response = request
        .send()
        .await
        .with_context(|| format!("calling orphaned-task reconcile endpoint {endpoint}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("reading orphaned-task reconcile response")?;
    let payload: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("parsing orphaned-task reconcile response: {body}"))?;
    println!("{}", serde_json::to_string_pretty(&payload)?);
    if !status.is_success() {
        anyhow::bail!("orphaned-task reconcile request failed with HTTP {status}");
    }
    if args.apply && payload.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        anyhow::bail!("orphaned-task reconcile completed with mutation errors");
    }
    Ok(())
}

/// Load `$MAGICIAN_ROOT_DIR/.env.development` then `.env` into the process
/// environment, once, before anything reads a credential.
///
/// `dotenvy` never overwrites a variable that is already set, and
/// `.env.development` is read first, so precedence is: real environment beats
/// `.env.development` beats `.env`. An operator export therefore still wins.
///
/// This used to run only on CLI subcommands, which meant the *server* saw a
/// key exactly when the shell that launched the supervisor happened to export
/// it. Launched from a LaunchAgent, a GUI, or a fresh terminal it saw none, and
/// boot died with "Failed to read DeepSeek API key from environment variable"
/// while a perfectly good key sat in the runtime `.env`. The server is the path
/// that most needs these files, so it loads them too.
fn load_runtime_env_files() {
    use std::sync::Once;
    static LOADED: Once = Once::new();
    LOADED.call_once(|| {
        let _ = dotenvy::from_filename(
            magician::magician_v2::artifact_v2::workspace::runtime_config_path(
                ".env.development",
                ".env.development",
            ),
        );
        let _ = dotenvy::from_filename(
            magician::magician_v2::artifact_v2::workspace::runtime_config_path(".env", ".env"),
        );
    });
}

/// Work-evidence graph, Phase 0 (read-only validation): distill the most-recent
/// agent episodes into evidence-record proposals and print the candidates.
/// Persists nothing — this is for eyeballing distillation quality on real
/// episodes before wiring it into `memory_consolidator`.
async fn run_distill_evidence(config: &MagicianConfig, args: &DistillEvidenceArgs) -> Result<()> {
    use std::sync::Arc;

    use magician::magician_v2::evidence::{
        distill_episode, entity_candidates_from_proposal, is_salient, EvidenceRecord,
    };
    use magician::magician_v2::prompts::{
        json_storage::{default_prompt_dir, JsonStorageConfig},
        JsonPromptStorage, PromptManager, PromptStore,
    };
    use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

    // Already loaded in `main`; idempotent, and kept so this path stays correct
    // if it is ever invoked directly.
    load_runtime_env_files();

    // 1. LLM router (no dispatch queue in CLI context — routes directly).
    let router_config = config
        .router_config()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no llm.router configured in magician-config.yaml"))?;
    let router = OperationLlmRouter::new(Some(router_config));

    // 2. Prompt manager over the on-disk prompt store.
    let storage_config = JsonStorageConfig {
        storage_dir: default_prompt_dir(),
        enable_cache: true,
        max_cache_entries: 50,
    };
    let prompt_storage: Arc<dyn PromptStore> = Arc::new(JsonPromptStorage::new(storage_config)?);
    prompt_storage.initialize().await?;
    let prompt_manager = PromptManager::new(Arc::clone(&prompt_storage));

    // 3. Scoped memory service.
    let storage_workspace = resolve_storage_workspace(config)?;
    let memory = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.principal, args.workspace
            )
        })?;

    // 4. Most-recent N episodes for the agent.
    let mut episodes = memory.load_native_episodes(&args.agent).await?;
    episodes.sort_by(|a, b| a.completed_at.cmp(&b.completed_at));
    let recent: Vec<_> = episodes.into_iter().rev().take(args.limit).collect();
    if recent.is_empty() {
        eprintln!(
            "No episodes found for agent '{}' in {}/{}.",
            args.agent, args.principal, args.workspace
        );
        return Ok(());
    }
    eprintln!(
        "Distilling {} most-recent episode(s) for agent '{}'{}…",
        recent.len(),
        args.agent,
        if args.persist {
            " (persisting salient records)"
        } else {
            " (read-only)"
        }
    );

    // 5. Distill per episode via the shared distiller.
    let mut promoted = 0usize;
    let mut skipped = 0usize;
    let mut persisted = 0usize;
    for episode in &recent {
        let proposal = match distill_episode(episode, &router, &prompt_manager).await {
            Ok(proposal) => proposal,
            Err(err) => {
                eprintln!("  [error] episode {}: {err}", episode.episode_id);
                continue;
            },
        };

        match EvidenceRecord::from_proposal(&proposal, episode) {
            Some(record) => {
                promoted += 1;
                println!("{}", serde_json::to_string_pretty(&record)?);
                if args.persist && is_salient(&record) {
                    // Mirror the consolidation hook: resolve entity anchors from
                    // the proposal so `--persist` seeds a complete graph
                    // (evidence.json + entities.json).
                    let candidates = entity_candidates_from_proposal(
                        &proposal,
                        &record.facets,
                        &record.source_refs,
                        &record.last_seen_at,
                    );
                    memory
                        .append_native_evidence(&args.agent, record)
                        .await
                        .with_context(|| {
                            format!("persisting evidence for {}", episode.episode_id)
                        })?;
                    persisted += 1;
                    if !candidates.is_empty() {
                        memory
                            .resolve_native_entities(&args.agent, candidates)
                            .await
                            .with_context(|| {
                                format!("resolving entities for {}", episode.episode_id)
                            })?;
                    }
                }
            },
            None => {
                skipped += 1;
                eprintln!(
                    "  [skip] episode {}: {}",
                    episode.episode_id,
                    proposal
                        .skip_reason
                        .as_deref()
                        .unwrap_or("(no reason given)")
                );
            },
        }
    }

    eprintln!(
        "Done: {promoted} promoted, {skipped} skipped, {persisted} persisted{}.",
        if args.persist {
            ""
        } else {
            " (read-only — pass --persist to save salient records)"
        }
    );
    Ok(())
}

/// Work-evidence graph, Phase 0 read path: assemble an agent's accrued evidence
/// over a chosen window + facet, then synthesize an impact summary (Markdown).
/// The window (`--days`) is fully dynamic — no fixed weekly cadence.
/// Deterministic evidence-quality health report for a scope (WEG generic
/// substrate eval depth). Read-only, no LLM; emits JSON.
async fn run_evidence_quality(config: &MagicianConfig, args: &EvidenceEvalArgs) -> Result<()> {
    use magician::magician_v2::evidence::eval::evidence_quality_report;

    let storage_workspace = resolve_storage_workspace(config)?;
    let memory = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.principal, args.workspace
            )
        })?;

    // RAW lanes (task-linked native ∪ user-owned ambient), NOT the
    // suppression-filtered unified read, so `sensitive` / `corrected` counts
    // reflect reality.
    let mut records = memory.load_native_evidence(&args.agent).await?;
    records.extend(memory.load_user_work_evidence().await?);

    let report = evidence_quality_report(&records);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// One-way human-facing work-ledger export: render each agent's durable
/// `work_outcome` evidence to `docs/worklog/<agent>.md`. Reads evidence, writes
/// Markdown — nothing else. The rendered docs are NOT agent-reachable (the memory
/// index does not ingest `docs/`) and are never read back; this is a pure
/// projection FROM the ledger.
async fn run_worklog_export(config: &MagicianConfig, args: &WorklogExportArgs) -> Result<()> {
    use magician::magician_v2::evidence::render_agent_worklog_markdown;

    let storage_workspace = resolve_storage_workspace(config)?;
    let memory = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.principal, args.workspace
            )
        })?;

    // Which agents to export: an explicit `--agent`, else every agent in the
    // scope's definition store (same enumeration the memory-index inspection uses).
    let agent_ids: Vec<String> = match &args.agent {
        Some(agent) => vec![agent.clone()],
        None => {
            let definition_store =
                AgentDefinitionStore::with_workspace_layout(storage_workspace.clone())
                    .for_scope(&args.principal, &args.workspace);
            let mut ids: Vec<String> = definition_store
                .list_definitions()
                .await
                .with_context(|| {
                    format!(
                        "listing agent definitions for {}/{}",
                        args.principal, args.workspace
                    )
                })?
                .into_iter()
                .map(|record| record.definition.agent_id)
                .collect();
            ids.sort();
            ids.dedup();
            ids
        },
    };

    if agent_ids.is_empty() {
        eprintln!(
            "No agents to export in {}/{} (empty scope — pass --agent to target one).",
            args.principal, args.workspace
        );
        return Ok(());
    }

    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("creating worklog out-dir {}", args.out_dir.display()))?;

    let mut files_written = 0usize;
    for agent_id in &agent_ids {
        // Native (task-linked) evidence only — the `work_outcome` ledger lane is
        // agent-scoped; the render filters to `producer == "work_outcome"`.
        let records = memory
            .load_native_evidence(agent_id)
            .await
            .with_context(|| format!("loading evidence for agent '{agent_id}'"))?;
        let work_count = records
            .iter()
            .filter(|record| record.producer == "work_outcome")
            .count();
        let markdown = render_agent_worklog_markdown(agent_id, &records);

        let out_path = args.out_dir.join(format!("{agent_id}.md"));
        std::fs::write(&out_path, markdown)
            .with_context(|| format!("writing worklog {}", out_path.display()))?;
        files_written += 1;
        eprintln!(
            "  wrote {} ({work_count} work-outcome record(s))",
            out_path.display()
        );
    }

    eprintln!(
        "Worklog export done: {files_written} file(s) written to {} for {}/{}.",
        args.out_dir.display(),
        args.principal,
        args.workspace
    );
    Ok(())
}

/// Layer-3 derived views over a scope's evidence graph (WEG generic substrate):
/// an entity neighborhood (`--entity`) or top sparse co-occurrence relations.
/// Read-only, no LLM; emits JSON.
async fn run_evidence_graph(config: &MagicianConfig, args: &EvidenceGraphArgs) -> Result<()> {
    use magician::magician_v2::evidence::{cooccurrence_edges, entity_neighborhood};

    let storage_workspace = resolve_storage_workspace(config)?;
    let memory = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.principal, args.workspace
            )
        })?;

    // Raw lanes; the views themselves drop sensitive/non-live records.
    let mut records = memory.load_native_evidence(&args.agent).await?;
    records.extend(memory.load_user_work_evidence().await?);

    let facet = if args.facet.eq_ignore_ascii_case("all") {
        None
    } else {
        Some(args.facet.as_str())
    };

    let json = if let Some(entity) = args.entity.as_deref() {
        serde_json::to_string_pretty(&entity_neighborhood(&records, entity, facet))?
    } else {
        serde_json::to_string_pretty(&cooccurrence_edges(&records, facet, args.min_weight))?
    };
    println!("{json}");
    Ok(())
}

/// Derive facet-scoped, evidence-grounded claims over a window (WEG generic
/// substrate). LLM proposes; the runtime validates citation grounding and drops
/// ungrounded claims. Ephemeral — nothing is persisted. Emits JSON.
async fn run_evidence_claims(config: &MagicianConfig, args: &EvidenceClaimsArgs) -> Result<()> {
    use std::sync::Arc;

    use chrono::{Duration, Utc};
    use magician::magician_v2::evidence::{
        build_review_packet, propose_claims, select_evidence_window, validate_claim_grounding,
    };
    use magician::magician_v2::prompts::{
        json_storage::{default_prompt_dir, JsonStorageConfig},
        JsonPromptStorage, PromptManager, PromptStore,
    };
    use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

    // Already loaded in `main`; idempotent, and kept so this path stays correct
    // if it is ever invoked directly.
    load_runtime_env_files();

    let router_config = config
        .router_config()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no llm.router configured in magician-config.yaml"))?;
    let router = OperationLlmRouter::new(Some(router_config));

    let storage_config = JsonStorageConfig {
        storage_dir: default_prompt_dir(),
        enable_cache: true,
        max_cache_entries: 50,
    };
    let prompt_storage: Arc<dyn PromptStore> = Arc::new(JsonPromptStorage::new(storage_config)?);
    prompt_storage.initialize().await?;
    let prompt_manager = PromptManager::new(Arc::clone(&prompt_storage));

    let storage_workspace = resolve_storage_workspace(config)?;
    let memory = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.principal, args.workspace
            )
        })?;

    let mut records = memory.load_native_evidence(&args.agent).await?;
    records.extend(memory.load_user_work_evidence().await?);

    let since = Utc::now() - Duration::days(args.days);
    let facet = if args.facet.eq_ignore_ascii_case("all") {
        None
    } else {
        Some(args.facet.as_str())
    };
    let selected = select_evidence_window(&records, since, facet);
    if selected.is_empty() {
        eprintln!(
            "No evidence in the last {} day(s){}.",
            args.days,
            facet
                .map(|f| format!(" for facet '{f}'"))
                .unwrap_or_default()
        );
        return Ok(());
    }

    let (packet, _ids) = build_review_packet(&selected);
    eprintln!(
        "Proposing claims from {} evidence record(s) (last {} days, facet: {})…",
        selected.len(),
        args.days,
        args.facet
    );
    let proposed = propose_claims(&packet, args.days, facet, &router, &prompt_manager).await?;

    // Deterministic grounding gate: keep only claims whose every citation
    // resolves to an in-scope record; drop hallucinated / unsupported ones.
    let kept: Vec<serde_json::Value> = proposed
        .into_iter()
        .map(|claim| {
            let grounding = validate_claim_grounding(&claim, &selected);
            (claim, grounding)
        })
        .filter(|(_, grounding)| grounding.grounded)
        .map(|(claim, grounding)| serde_json::json!({ "claim": claim, "grounding": grounding }))
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({ "claims": kept }))?
    );
    Ok(())
}

/// LLM-grade evidence-vs-source faithfulness (WEG eval depth). Samples evidence,
/// loads each record's source episode, judges faithfulness, reports precision.
/// Ambient records (no source episode) are gradeable-skipped. Emits JSON.
async fn run_evidence_precision(
    config: &MagicianConfig,
    args: &EvidencePrecisionArgs,
) -> Result<()> {
    use std::sync::Arc;

    use magician::magician_v2::evidence::grade_evidence_precision;
    use magician::magician_v2::prompts::{
        json_storage::{default_prompt_dir, JsonStorageConfig},
        JsonPromptStorage, PromptManager, PromptStore,
    };
    use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

    // Already loaded in `main`; idempotent, and kept so this path stays correct
    // if it is ever invoked directly.
    load_runtime_env_files();

    let router_config = config
        .router_config()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no llm.router configured in magician-config.yaml"))?;
    let router = OperationLlmRouter::new(Some(router_config));

    let storage_config = JsonStorageConfig {
        storage_dir: default_prompt_dir(),
        enable_cache: true,
        max_cache_entries: 50,
    };
    let prompt_storage: Arc<dyn PromptStore> = Arc::new(JsonPromptStorage::new(storage_config)?);
    prompt_storage.initialize().await?;
    let prompt_manager = PromptManager::new(Arc::clone(&prompt_storage));

    let storage_workspace = resolve_storage_workspace(config)?;
    let memory = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.principal, args.workspace
            )
        })?;

    let records = memory.load_native_evidence(&args.agent).await?;
    let episodes = memory.load_native_episodes(&args.agent).await?;

    // Pair each record with its source-episode excerpt; records with no loadable
    // source episode (e.g. the ambient lane) are gradeable-skipped.
    let mut samples: Vec<(magician::magician_v2::evidence::EvidenceRecord, String)> = Vec::new();
    let mut ungradeable = 0usize;
    for record in &records {
        if samples.len() >= args.sample {
            break;
        }
        let ep_id = record
            .source_refs
            .iter()
            .find_map(|s| s.strip_prefix("episode:"))
            .or_else(|| record.evidence_id.strip_prefix("evd:"));
        let source = ep_id
            .and_then(|id| episodes.iter().find(|e| e.episode_id == id))
            .map(|ep| {
                format!(
                    "Goal: {}\nOutcome ({}): {}",
                    ep.goal_key, ep.outcome_kind, ep.outcome_summary
                )
            });
        match source {
            Some(source_text) => samples.push((record.clone(), source_text)),
            None => ungradeable += 1,
        }
    }

    if samples.is_empty() {
        eprintln!(
            "No gradeable evidence (no records with a loadable source episode); {ungradeable} ungradeable."
        );
        return Ok(());
    }
    eprintln!(
        "Grading {} evidence record(s) for faithfulness-to-source ({ungradeable} ungradeable)…",
        samples.len()
    );
    let report = grade_evidence_precision(&samples, &router, &prompt_manager).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({ "report": report, "ungradeable": ungradeable })
        )?
    );
    Ok(())
}

async fn run_review(config: &MagicianConfig, args: &ReviewArgs) -> Result<()> {
    use std::sync::Arc;

    use chrono::{Duration, Utc};
    use magician::magician_v2::evidence::{
        build_review_packet, select_evidence_window, synthesize_review,
    };
    use magician::magician_v2::prompts::{
        json_storage::{default_prompt_dir, JsonStorageConfig},
        JsonPromptStorage, PromptManager, PromptStore,
    };
    use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

    // Already loaded in `main`; idempotent, and kept so this path stays correct
    // if it is ever invoked directly.
    load_runtime_env_files();

    let router_config = config
        .router_config()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no llm.router configured in magician-config.yaml"))?;
    let router = OperationLlmRouter::new(Some(router_config));

    let storage_config = JsonStorageConfig {
        storage_dir: default_prompt_dir(),
        enable_cache: true,
        max_cache_entries: 50,
    };
    let prompt_storage: Arc<dyn PromptStore> = Arc::new(JsonPromptStorage::new(storage_config)?);
    prompt_storage.initialize().await?;
    let prompt_manager = PromptManager::new(Arc::clone(&prompt_storage));

    let storage_workspace = resolve_storage_workspace(config)?;
    let memory = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.principal, args.workspace
            )
        })?;

    let records = memory.load_native_evidence(&args.agent).await?;
    if records.is_empty() {
        eprintln!(
            "No evidence for agent '{}' yet. Run `distill-evidence --persist`, or let consolidation accrue some.",
            args.agent
        );
        return Ok(());
    }

    let since = Utc::now() - Duration::days(args.days);
    let facet = if args.facet.eq_ignore_ascii_case("all") {
        None
    } else {
        Some(args.facet.as_str())
    };
    let selected = select_evidence_window(&records, since, facet);
    if selected.is_empty() {
        eprintln!(
            "No evidence in the last {} day(s){}.",
            args.days,
            facet
                .map(|f| format!(" for facet '{f}'"))
                .unwrap_or_default()
        );
        return Ok(());
    }
    eprintln!(
        "Assembling review from {} evidence record(s) (last {} days, facet: {})…",
        selected.len(),
        args.days,
        args.facet
    );

    let (packet, evidence_ids) = build_review_packet(&selected);
    let review = synthesize_review(&packet, args.days, facet, &router, &prompt_manager).await?;

    // Persist as a consumable durable artifact (Markdown + YAML frontmatter),
    // served by the existing artifact API (`list_durable_artifacts` /
    // `read_durable_artifact`). The `input_evidence_ids` lineage travels in the
    // content footer (the frontmatter has no arbitrary-metadata map).
    use magician::magician_v2::artifacts::durable_store::{
        open_local_durable_artifacts, DurableFrontmatter,
    };

    let now = Utc::now();
    let artifact_body = format!(
        "# Impact review — {facet} · last {days} day(s)\n\n> Generated {generated} from {count} evidence record(s).\n\n{review}\n\n<!-- input_evidence_ids: {ids} -->\n",
        facet = args.facet,
        days = args.days,
        generated = now.to_rfc3339(),
        count = selected.len(),
        review = review,
        ids = evidence_ids.join(", "),
    );
    let artifact_name = format!(
        "{}-{}d-{}.md",
        args.facet,
        args.days,
        now.format("%Y%m%dT%H%M%SZ")
    );
    let artifact_store =
        open_local_durable_artifacts(&storage_workspace, &args.principal, &args.workspace)?;
    let frontmatter = DurableFrontmatter {
        namespace: "evidence-reviews".to_string(),
        name: artifact_name.clone(),
        created_by: "evidence-review".to_string(),
        last_updated_by: "evidence-review".to_string(),
        last_updated: now,
        content_type: Some("text/markdown".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: Some(args.agent.clone()),
        producer_stage: Some("evidence_review".to_string()),
    };
    let path = artifact_store
        .write(
            "evidence-reviews",
            &artifact_name,
            &artifact_body,
            frontmatter,
        )
        .await?;

    // Print to stdout too, for CLI convenience.
    println!("{artifact_body}");
    eprintln!(
        "Saved review artifact `evidence-reviews/{}` → {}",
        artifact_name,
        path.display()
    );
    Ok(())
}

async fn run_memory_index_command(
    config: &MagicianConfig,
    command: &MemoryIndexCommand,
) -> Result<()> {
    load_runtime_env_files();
    match command {
        MemoryIndexCommand::Status(args) => {
            let status = inspect_memory_index_for_scope(config, args).await?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "principal": args.principal.as_str(),
                        "workspace": args.workspace.as_str(),
                        "stale": status.stale,
                        "reason": status.reason.as_str(),
                        "current_document_count": status.current_document_count,
                        "current_source_count": status.current_source_count,
                        "manifest": &status.manifest,
                    }))?
                );
            } else {
                print_memory_index_status_summary(args, &status);
            }
            Ok(())
        },
        MemoryIndexCommand::Rebuild(args) => rebuild_memory_index_for_scope(config, args).await,
        MemoryIndexCommand::Optimize(args) => optimize_memory_index_for_scope(config, args).await,
    }
}

/// Analytics maintenance subcommands. EXPORT/IMPORT operate directly on file
/// paths for cross-version DuckDB migration — no scope resolution, no secrets
/// (run with `MAGICIAN_SKIP_KEYCHAIN=1`). The DuckDB linked into THIS binary
/// decides which storage versions it can read: `export` must run with a binary
/// whose DuckDB can open the source file (the OLD pin for a legacy DB);
/// `import` with the new pin. REPRICE-LLM-CALLS walks the scoped `llm_calls`
/// Parquet lakehouse and recomputes stored costs against the active
/// effective-dated pricing table (installed from llm_pricing.json at startup,
/// before this command runs).
async fn run_analytics_command(config: &MagicianConfig, command: &AnalyticsCommand) -> Result<()> {
    match command {
        AnalyticsCommand::Export(args) => {
            let conn = duckdb::Connection::open(&args.db).with_context(|| {
                format!(
                    "opening analytics DB {} for export — if this fails on storage version, \
                     re-run with a binary pinned to the OLD DuckDB that wrote it",
                    args.db.display()
                )
            })?;
            std::fs::create_dir_all(&args.out)
                .with_context(|| format!("creating export dir {}", args.out.display()))?;
            let out_sql = args.out.display().to_string().replace('\'', "''");
            conn.execute_batch(&format!("EXPORT DATABASE '{out_sql}' (FORMAT PARQUET);"))
                .with_context(|| format!("EXPORT DATABASE to {}", args.out.display()))?;
            println!(
                "exported analytics DB {} -> {}",
                args.db.display(),
                args.out.display()
            );
            Ok(())
        },
        AnalyticsCommand::Import(args) => {
            let conn = duckdb::Connection::open(&args.db)
                .with_context(|| format!("opening/creating analytics DB {}", args.db.display()))?;
            let in_sql = args.input.display().to_string().replace('\'', "''");
            conn.execute_batch(&format!("IMPORT DATABASE '{in_sql}';"))
                .with_context(|| format!("IMPORT DATABASE from {}", args.input.display()))?;
            println!(
                "imported {} -> analytics DB {}",
                args.input.display(),
                args.db.display()
            );
            Ok(())
        },
        AnalyticsCommand::RepriceLlmCalls(args) => {
            for (flag, value) in [("--from", &args.from), ("--to", &args.to)] {
                if let Some(value) = value {
                    if chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err() {
                        bail!("{flag} must be a YYYY-MM-DD partition date, got `{value}`");
                    }
                }
            }
            let storage_workspace = resolve_storage_workspace(config)?;
            let opts = magician::magician_v2::analytics::llm_reprice::RepriceOptions {
                apply: args.apply,
                principal: args.principal.clone(),
                workspace: args.workspace.clone(),
                from: args.from.clone(),
                to: args.to.clone(),
                include_today: args.include_today,
            };
            // One-shot CLI: the synchronous DuckDB sweep runs inline, exactly
            // like the export/import arms above.
            let report = magician::magician_v2::analytics::llm_reprice::run_reprice(
                &storage_workspace,
                &opts,
            )?;
            print!("{}", report.render());
            Ok(())
        },
    }
}

const MAX_ACTIONABILITY_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024;

fn ensure_actionability_snapshot_install_is_nonactivating(
    config: &MagicianConfig,
    snapshot_id: &str,
) -> Result<()> {
    anyhow::ensure!(
        config.attention_learning.actionability.mode
            == magician::config::AttentionActionabilityMode::Disabled
            || config.attention_learning.actionability.snapshot_id.as_deref() != Some(snapshot_id),
        "refusing to install snapshot '{}' while runtime configuration already selects it in {} mode; disable that selection before installation",
        snapshot_id,
        config.attention_learning.actionability.mode.as_str()
    );
    Ok(())
}

fn ensure_pair_snapshot_install_is_nonactivating(
    config: &MagicianConfig,
    snapshot_id: &str,
) -> Result<()> {
    anyhow::ensure!(
        config.attention_learning.grouping.mode
            == magician::config::AttentionGroupingMode::Disabled
            || config.attention_learning.grouping.snapshot_id.as_deref() != Some(snapshot_id),
        "refusing to install pair snapshot '{}' while runtime configuration already selects it in {} mode; disable that selection before installation",
        snapshot_id,
        config.attention_learning.grouping.mode.as_str()
    );
    Ok(())
}

fn ensure_routing_snapshot_install_is_nonactivating(
    config: &MagicianConfig,
    snapshot_id: &str,
) -> Result<()> {
    anyhow::ensure!(
        config.attention_learning.routing.mode
            == magician::config::AttentionRoutingMode::Baseline
            || config.attention_learning.routing.snapshot_id.as_deref() != Some(snapshot_id),
        "refusing to install routing snapshot '{}' while runtime configuration already selects it in {} mode; restore baseline before installation",
        snapshot_id,
        config.attention_learning.routing.mode.as_str()
    );
    Ok(())
}

fn ensure_bandit_snapshot_install_is_nonactivating(
    config: &MagicianConfig,
    snapshot_id: &str,
) -> Result<()> {
    anyhow::ensure!(
        config.attention_learning.bandit.mode
            == magician::config::AttentionBanditMode::Disabled
            || config.attention_learning.bandit.snapshot_id.as_deref() != Some(snapshot_id),
        "refusing to install bandit snapshot '{}' while runtime configuration already selects it in {} mode; disable bandit serving before installation",
        snapshot_id,
        config.attention_learning.bandit.mode.as_str()
    );
    Ok(())
}

fn read_actionability_snapshot_artifact(
    path: &Path,
) -> Result<magician::magician_v2::attention::learning::ActionabilityModelSnapshot> {
    // Inspect and read through one handle so a path replacement cannot swap in
    // a different file between the regular-file/size checks and parsing.
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening actionability snapshot: {}", path.display()))?;
    let metadata = file.metadata().with_context(|| {
        format!(
            "reading actionability snapshot metadata: {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        metadata.is_file(),
        "actionability snapshot must be a regular file: {}",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= MAX_ACTIONABILITY_SNAPSHOT_BYTES,
        "actionability snapshot exceeds the {} byte limit",
        MAX_ACTIONABILITY_SNAPSHOT_BYTES
    );
    let mut encoded = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_ACTIONABILITY_SNAPSHOT_BYTES.saturating_add(1))
        .read_to_end(&mut encoded)
        .with_context(|| format!("reading actionability snapshot: {}", path.display()))?;
    anyhow::ensure!(
        encoded.len() as u64 <= MAX_ACTIONABILITY_SNAPSHOT_BYTES,
        "actionability snapshot exceeds the {} byte limit",
        MAX_ACTIONABILITY_SNAPSHOT_BYTES
    );
    let snapshot: magician::magician_v2::attention::learning::ActionabilityModelSnapshot =
        serde_json::from_slice(&encoded)
            .with_context(|| format!("parsing actionability snapshot JSON: {}", path.display()))?;
    snapshot.validate().with_context(|| {
        format!(
            "validating actionability snapshot contract: {}",
            path.display()
        )
    })?;
    Ok(snapshot)
}

fn parse_attention_training_cutoff(raw: Option<&str>) -> Result<i64> {
    let Some(raw) = raw else {
        return Ok(chrono::Utc::now().timestamp_millis());
    };
    if let Ok(millis) = raw.parse::<i64>() {
        anyhow::ensure!(millis > 0, "--cutoff millis must be positive");
        return Ok(millis);
    }
    let date = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .with_context(|| format!("parsing --cutoff {raw} as YYYY-MM-DD or unix millis"))?;
    let end = date
        .and_hms_milli_opt(23, 59, 59, 999)
        .context("constructing cutoff timestamp")?;
    Ok(end.and_utc().timestamp_millis())
}

fn read_pair_model_snapshot_artifact(
    path: &Path,
) -> Result<magician::magician_v2::attention::learning::AttentionPairModelSnapshot> {
    // Keep validation and the bounded read on one handle so a path swap cannot
    // replace the inspected artifact before parsing.
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening pair-model snapshot: {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("reading pair-model snapshot metadata: {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "pair-model snapshot must be a regular file: {}",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= MAX_ACTIONABILITY_SNAPSHOT_BYTES,
        "pair-model snapshot exceeds the {} byte limit",
        MAX_ACTIONABILITY_SNAPSHOT_BYTES
    );
    let mut encoded = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_ACTIONABILITY_SNAPSHOT_BYTES.saturating_add(1))
        .read_to_end(&mut encoded)
        .with_context(|| format!("reading pair-model snapshot: {}", path.display()))?;
    anyhow::ensure!(
        encoded.len() as u64 <= MAX_ACTIONABILITY_SNAPSHOT_BYTES,
        "pair-model snapshot exceeds the {} byte limit",
        MAX_ACTIONABILITY_SNAPSHOT_BYTES
    );
    let snapshot: magician::magician_v2::attention::learning::AttentionPairModelSnapshot =
        serde_json::from_slice(&encoded)
            .with_context(|| format!("parsing pair-model snapshot JSON: {}", path.display()))?;
    snapshot.validate().with_context(|| {
        format!(
            "validating pair-model snapshot contract: {}",
            path.display()
        )
    })?;
    Ok(snapshot)
}

fn read_routing_policy_snapshot_artifact(
    path: &Path,
) -> Result<magician::magician_v2::attention::learning::AttentionRoutingPolicySnapshot> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening routing-policy snapshot: {}", path.display()))?;
    let metadata = file.metadata().with_context(|| {
        format!(
            "reading routing-policy snapshot metadata: {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        metadata.is_file(),
        "routing-policy snapshot must be a regular file: {}",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= MAX_ACTIONABILITY_SNAPSHOT_BYTES,
        "routing-policy snapshot exceeds the {} byte limit",
        MAX_ACTIONABILITY_SNAPSHOT_BYTES
    );
    let mut encoded = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_ACTIONABILITY_SNAPSHOT_BYTES.saturating_add(1))
        .read_to_end(&mut encoded)
        .with_context(|| format!("reading routing-policy snapshot: {}", path.display()))?;
    anyhow::ensure!(
        encoded.len() as u64 <= MAX_ACTIONABILITY_SNAPSHOT_BYTES,
        "routing-policy snapshot exceeds the {} byte limit",
        MAX_ACTIONABILITY_SNAPSHOT_BYTES
    );
    let snapshot: magician::magician_v2::attention::learning::AttentionRoutingPolicySnapshot =
        serde_json::from_slice(&encoded)
            .with_context(|| format!("parsing routing-policy snapshot JSON: {}", path.display()))?;
    snapshot.validate().with_context(|| {
        format!(
            "validating routing-policy snapshot contract: {}",
            path.display()
        )
    })?;
    Ok(snapshot)
}

fn read_bandit_policy_snapshot_artifact(
    path: &Path,
) -> Result<magician::magician_v2::attention::learning::AttentionBanditPolicySnapshot> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening bandit-policy snapshot: {}", path.display()))?;
    let metadata = file.metadata().with_context(|| {
        format!(
            "reading bandit-policy snapshot metadata: {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        metadata.is_file(),
        "bandit-policy snapshot must be a regular file: {}",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= MAX_ACTIONABILITY_SNAPSHOT_BYTES,
        "bandit-policy snapshot exceeds the {} byte limit",
        MAX_ACTIONABILITY_SNAPSHOT_BYTES
    );
    let mut encoded = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_ACTIONABILITY_SNAPSHOT_BYTES.saturating_add(1))
        .read_to_end(&mut encoded)
        .with_context(|| format!("reading bandit-policy snapshot: {}", path.display()))?;
    anyhow::ensure!(
        encoded.len() as u64 <= MAX_ACTIONABILITY_SNAPSHOT_BYTES,
        "bandit-policy snapshot exceeds the {} byte limit",
        MAX_ACTIONABILITY_SNAPSHOT_BYTES
    );
    let snapshot: magician::magician_v2::attention::learning::AttentionBanditPolicySnapshot =
        serde_json::from_slice(&encoded)
            .with_context(|| format!("parsing bandit-policy snapshot JSON: {}", path.display()))?;
    snapshot.validate().with_context(|| {
        format!(
            "validating bandit-policy snapshot contract: {}",
            path.display()
        )
    })?;
    Ok(snapshot)
}

/// Validate and immutably install a snapshot. This command intentionally has
/// no activation flag and never writes configuration: serving remains disabled
/// until a separate, explicit config rollout selects the snapshot.
async fn run_attention_learning_command(
    config: &MagicianConfig,
    command: &AttentionLearningCommand,
) -> Result<()> {
    match command {
        AttentionLearningCommand::InstallActionabilitySnapshot(args) => {
            let snapshot = read_actionability_snapshot_artifact(&args.snapshot)?;
            ensure_actionability_snapshot_install_is_nonactivating(config, &snapshot.snapshot_id)?;
            let storage_workspace = resolve_storage_workspace(config)?;
            let store = magician::magician_v2::attention::learning::AttentionLearningStore::open(
                storage_workspace.base_root(),
            )
            .context("opening attention learning store")?;
            store
                .install_actionability_snapshot(&snapshot)
                .await
                .context("installing immutable actionability snapshot")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "installed_or_already_present",
                    "snapshot_id": snapshot.snapshot_id,
                    "model_version": snapshot.model_version,
                    "activation_changed": false,
                    "activation_status": "not_evaluated",
                }))?
            );
            Ok(())
        },
        AttentionLearningCommand::InstallPairModelSnapshot(args) => {
            let snapshot = read_pair_model_snapshot_artifact(&args.snapshot)?;
            ensure_pair_snapshot_install_is_nonactivating(config, &snapshot.snapshot_id)?;
            let storage_workspace = resolve_storage_workspace(config)?;
            let store = magician::magician_v2::attention::learning::AttentionLearningStore::open(
                storage_workspace.base_root(),
            )
            .context("opening attention learning store")?;
            store
                .install_pair_model_snapshot(&snapshot)
                .await
                .context("installing immutable pair-model snapshot")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "installed_or_already_present",
                    "snapshot_id": snapshot.snapshot_id,
                    "model_version": snapshot.model_version,
                    "activation_changed": false,
                    "activation_status": "not_evaluated",
                }))?
            );
            Ok(())
        },
        AttentionLearningCommand::InstallRoutingPolicySnapshot(args) => {
            let snapshot = read_routing_policy_snapshot_artifact(&args.snapshot)?;
            ensure_routing_snapshot_install_is_nonactivating(config, &snapshot.snapshot_id)?;
            let storage_workspace = resolve_storage_workspace(config)?;
            let store = magician::magician_v2::attention::learning::AttentionLearningStore::open(
                storage_workspace.base_root(),
            )
            .context("opening attention learning store")?;
            store
                .install_routing_policy_snapshot(&snapshot)
                .await
                .context("installing immutable routing-policy snapshot")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "installed_or_already_present",
                    "snapshot_id": snapshot.snapshot_id,
                    "model_version": snapshot.model_version,
                    "activation_changed": false,
                    "activation_status": "not_evaluated",
                }))?
            );
            Ok(())
        },
        AttentionLearningCommand::InstallBanditPolicySnapshot(args) => {
            let snapshot = read_bandit_policy_snapshot_artifact(&args.snapshot)?;
            ensure_bandit_snapshot_install_is_nonactivating(config, &snapshot.snapshot_id)?;
            let storage_workspace = resolve_storage_workspace(config)?;
            let store = magician::magician_v2::attention::learning::AttentionLearningStore::open(
                storage_workspace.base_root(),
            )
            .context("opening attention learning store")?;
            store
                .install_bandit_policy_snapshot(&snapshot)
                .await
                .context("installing immutable bandit-policy snapshot")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "installed_or_already_present",
                    "snapshot_id": snapshot.snapshot_id,
                    "model_version": snapshot.model_version,
                    "activation_changed": false,
                    "activation_status": "not_evaluated",
                }))?
            );
            Ok(())
        },
        AttentionLearningCommand::RetainScope(args) => {
            let storage_workspace = resolve_storage_workspace(config)?;
            let learning =
                magician::magician_v2::attention::learning::AttentionLearningService::open(
                    storage_workspace.base_root(),
                    config.attention_learning.clone(),
                )?;
            let mut report = learning
                .store()
                .apply_scoped_retention(
                    &args.principal,
                    &args.workspace,
                    args.cutoff_at,
                    args.apply,
                )
                .await?;
            let delivery_report = learning
                .compact_attention_deliveries(
                    &args.principal,
                    &args.workspace,
                    chrono::Utc::now().timestamp_millis(),
                    args.apply,
                )
                .await?;
            report.affected_rows.extend(delivery_report.affected_rows);
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        },
        AttentionLearningCommand::DeleteScope(args) => {
            let storage_workspace = resolve_storage_workspace(config)?;
            let store = magician::magician_v2::attention::learning::AttentionLearningStore::open(
                storage_workspace.base_root(),
            )?;
            let report = store
                .delete_scope(&args.principal, &args.workspace, args.apply)
                .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        },
        AttentionLearningCommand::TrainActionability(args) => {
            anyhow::ensure!(
                args.split == "temporal",
                "only --split temporal is supported"
            );
            let cutoff_at = parse_attention_training_cutoff(args.cutoff.as_deref())?;
            let storage_workspace = resolve_storage_workspace(config)?;
            let store = magician::magician_v2::attention::learning::AttentionLearningStore::open(
                storage_workspace.base_root(),
            )
            .context("opening attention learning store")?;
            let mut training =
                magician::magician_v2::attention::learning::ActionabilityTrainingConfig::default();
            training.cutoff_at = cutoff_at;
            let outcome = magician::magician_v2::attention::learning::train_actionability(
                &store,
                &args.principal,
                &args.workspace,
                &training,
                &args.out,
            )
            .await
            .context("training actionability snapshot")?;
            match outcome {
                magician::magician_v2::attention::learning::TrainingOutcome::Refused {
                    reason,
                    metrics,
                    counts,
                } => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "status": "refused",
                            "reason": reason,
                            "usable": counts.usable,
                            "unlinked": counts.unlinked,
                            "positive": counts.positive,
                            "negative": counts.negative,
                            "excluded": counts.excluded,
                            "label_count": metrics.label_count,
                            "auc": metrics.auc,
                            "ece": metrics.ece,
                        }))?
                    );
                    Ok(())
                },
                magician::magician_v2::attention::learning::TrainingOutcome::Written {
                    snapshot_id,
                    path,
                    metrics,
                    counts,
                } => {
                    let mut installed_mode = None;
                    if args.install {
                        let snapshot = read_actionability_snapshot_artifact(&path)?;
                        ensure_actionability_snapshot_install_is_nonactivating(
                            config,
                            &snapshot.snapshot_id,
                        )?;
                        let mode = store
                            .install_actionability_snapshot_for_scope(
                                &args.principal,
                                &args.workspace,
                                &snapshot,
                                magician::config::AttentionActionabilityMode::Enforced,
                            )
                            .await
                            .context("installing trained actionability snapshot")?;
                        installed_mode = Some(mode.as_str().to_string());
                    }
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "status": "written",
                            "snapshot_id": snapshot_id,
                            "path": path,
                            "usable": counts.usable,
                            "unlinked": counts.unlinked,
                            "label_count": metrics.label_count,
                            "auc": metrics.auc,
                            "ece": metrics.ece,
                            "installed_mode": installed_mode,
                            "activation_changed": false,
                        }))?
                    );
                    Ok(())
                },
            }
        },
        AttentionLearningCommand::TrainRouting(args) => {
            anyhow::ensure!(
                args.split == "temporal",
                "only --split temporal is supported"
            );
            let cutoff_at = parse_attention_training_cutoff(args.cutoff.as_deref())?;
            let storage_workspace = resolve_storage_workspace(config)?;
            let store = magician::magician_v2::attention::learning::AttentionLearningStore::open(
                storage_workspace.base_root(),
            )
            .context("opening attention learning store")?;
            let mut training =
                magician::magician_v2::attention::learning::RoutingTrainingConfig::default();
            training.cutoff_at = cutoff_at;
            let outcome = magician::magician_v2::attention::learning::train_routing(
                &store,
                &args.principal,
                &args.workspace,
                &training,
                &args.out,
            )
            .await
            .context("training routing snapshot")?;
            match outcome {
                magician::magician_v2::attention::learning::TrainingOutcome::Refused {
                    reason,
                    metrics,
                    counts,
                } => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "status": "refused",
                            "reason": reason,
                            "usable": counts.usable,
                            "unlinked": counts.unlinked,
                            "label_count": metrics.label_count,
                            "auc": metrics.auc,
                            "ece": metrics.ece,
                        }))?
                    );
                    Ok(())
                },
                magician::magician_v2::attention::learning::TrainingOutcome::Written {
                    snapshot_id,
                    path,
                    metrics,
                    counts,
                } => {
                    let mut installed_mode = None;
                    if args.install {
                        let encoded = std::fs::read_to_string(&path)?;
                        let snapshot: magician::magician_v2::attention::learning::AttentionRoutingPolicySnapshot =
                            serde_json::from_str(&encoded)?;
                        snapshot.validate()?;
                        let mode = store
                            .install_routing_policy_snapshot_for_scope(
                                &args.principal,
                                &args.workspace,
                                &snapshot,
                                magician::config::AttentionRoutingMode::Shadow,
                            )
                            .await
                            .context("installing trained routing snapshot")?;
                        installed_mode = Some(mode.as_str().to_string());
                    }
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "status": "written",
                            "snapshot_id": snapshot_id,
                            "path": path,
                            "usable": counts.usable,
                            "unlinked": counts.unlinked,
                            "label_count": metrics.label_count,
                            "auc": metrics.auc,
                            "ece": metrics.ece,
                            "installed_mode": installed_mode,
                            "activation_changed": false,
                        }))?
                    );
                    Ok(())
                },
            }
        },
    }
}

/// Channel Assist maintenance subcommands. EXPORT-FIXTURES
/// reads the scoped message-thread table and writes labeled-ready JSONL
/// rows to a LOCAL file (default under the storage root, outside the
/// repo). The row shape is the privacy contract implemented in
/// `channel_assist::assist::fixtures` — hashed thread refs, real subject,
/// sender name+domain, labels/counts/ages, no recipient data.
async fn run_channel_assist_command(
    config: &MagicianConfig,
    command: &ChannelAssistCommand,
) -> Result<()> {
    match command {
        ChannelAssistCommand::ExportFixtures(args) => {
            let storage_workspace = resolve_storage_workspace(config)?;
            let fixtures_root = storage_workspace.base_root().to_path_buf();
            let store =
                magician_comms::channel_assist::channel::ChannelAssistStore::open_workspace(
                    storage_workspace,
                )
                .context("opening channel assist store")?;
            let threads = store
                .list_recent_threads(
                    &args.principal,
                    &args.workspace,
                    magician_comms::channel_assist::adapter_registry::GMAIL_PROVIDER,
                    args.account.as_deref(),
                    args.limit,
                )
                .await
                .context("listing synced channel threads")?;
            let now_ms = chrono::Utc::now().timestamp_millis();
            let rows: Vec<_> = threads
                .iter()
                .map(|t| magician_comms::channel_assist::assist::fixtures::fixture_row(t, now_ms))
                .collect();
            let jsonl = magician_comms::channel_assist::assist::fixtures::render_jsonl(&rows)?;
            let out = match &args.out {
                Some(out) => out.clone(),
                None => fixtures_root.join("channel_assist_fixtures").join(format!(
                    "fixtures-{}.jsonl",
                    chrono::Local::now().format("%Y-%m-%d")
                )),
            };
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("creating fixtures output dir {}", parent.display())
                })?;
            }
            std::fs::write(&out, jsonl)
                .with_context(|| format!("writing fixtures JSONL {}", out.display()))?;
            println!("exported {} fixture rows -> {}", rows.len(), out.display());
            println!(
                "privacy: rows carry REAL subjects (hashed thread refs, no recipients) — \
                 keep this file local; never commit it"
            );
            Ok(())
        },
        ChannelAssistCommand::ClassifyEval(args) => {
            use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
            use magician_comms::channel_assist::assist::classify;

            let router_config = config.router_config().cloned().ok_or_else(|| {
                anyhow::anyhow!("no llm.router configured in magician-config.yaml")
            })?;
            let router = std::sync::Arc::new(OperationLlmRouter::new(Some(router_config)));
            // Eval CLI: no runtime broadcaster (telemetry emission is worker-only).
            let llm = classify::RouterClassifyLlm::new(router, None, "anonymous", "default");
            if !classify::ClassifyLlm::bound(&llm) {
                anyhow::bail!(
                    "'{}' is not bound in operation_mapping — bind it (local or remote) to run the eval",
                    classify::CHANNEL_CLASSIFY_OPERATION
                );
            }

            let raw = std::fs::read_to_string(&args.fixtures)
                .with_context(|| format!("reading fixtures {}", args.fixtures.display()))?;
            let fixtures: Vec<classify::EvalFixture> = raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .enumerate()
                .map(|(i, line)| {
                    serde_json::from_str::<classify::EvalFixture>(line)
                        .with_context(|| format!("parsing fixture line {}", i + 1))
                })
                .collect::<Result<_>>()?;
            if fixtures.is_empty() {
                anyhow::bail!("no fixtures found in {}", args.fixtures.display());
            }

            let mut pairs: Vec<(String, String)> = Vec::with_capacity(fixtures.len());
            for fixture in &fixtures {
                let predicted = classify::classify_row(&llm, &fixture.to_row())
                    .await
                    .map(|c| c.label)
                    .unwrap_or_else(|error| {
                        eprintln!("classify failed for a fixture ({error}); counting as 'error'");
                        "error".to_string()
                    });
                pairs.push((fixture.gold_label.trim().to_ascii_lowercase(), predicted));
            }
            print!("{}", classify::score(&pairs).render());
            Ok(())
        },
        ChannelAssistCommand::DraftEval(args) => {
            use magician_comms::channel_assist::assist::draft_eval;

            if !(0.0..=1.0).contains(&args.threshold) {
                anyhow::bail!("--threshold must be between 0 and 1");
            }
            let raw = std::fs::read_to_string(&args.fixtures)
                .with_context(|| format!("reading fixtures {}", args.fixtures.display()))?;
            let fixtures: Vec<draft_eval::DraftEvalFixture> = raw
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .enumerate()
                .map(|(index, line)| {
                    serde_json::from_str(line)
                        .with_context(|| format!("parsing draft fixture line {}", index + 1))
                })
                .collect::<Result<_>>()?;
            if fixtures.is_empty() {
                anyhow::bail!("no fixtures found in {}", args.fixtures.display());
            }
            let report = draft_eval::score(&fixtures);
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report.expectation_accuracy < args.threshold {
                anyhow::bail!(
                    "draft usefulness expectation accuracy {:.3} is below threshold {:.3}",
                    report.expectation_accuracy,
                    args.threshold
                );
            }
            Ok(())
        },
    }
}

async fn inspect_memory_index_for_scope(
    config: &MagicianConfig,
    args: &MemoryIndexScopeArgs,
) -> Result<magician::magician_v2::agents::MemoryIndexStatus> {
    let storage_workspace = resolve_storage_workspace(config)?;
    let definition_store = AgentDefinitionStore::with_workspace_layout(storage_workspace.clone())
        .for_scope(&args.principal, &args.workspace);
    let memory_service = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.principal, args.workspace
            )
        })?;

    inspect_scope_memory_index(memory_service.storage(), &definition_store)
        .await
        .with_context(|| {
            format!(
                "inspecting memory index for {}/{}",
                args.principal, args.workspace
            )
        })
}

async fn rebuild_memory_index_for_scope(
    config: &MagicianConfig,
    args: &MemoryIndexRebuildArgs,
) -> Result<()> {
    if args.skip_if_fresh && !args.force {
        match inspect_memory_index_for_scope(config, &args.scope).await {
            Ok(status) if !status.stale || memory_index_stale_reason_is_soft(&status.reason) => {
                let skipped_reason = if status.stale {
                    format!("usable_soft_stale:{}", status.reason)
                } else {
                    status.reason.clone()
                };
                if args.scope.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "principal": args.scope.principal,
                            "workspace": args.scope.workspace,
                            "skipped": true,
                            "reason": skipped_reason.as_str(),
                            "stale": status.stale,
                            "current_document_count": status.current_document_count,
                            "current_source_count": status.current_source_count,
                            "manifest": &status.manifest,
                        }))?
                    );
                } else {
                    if status.stale {
                        println!(
                            "memory index usable but soft-stale for {}/{} ({}); skipped blocking rebuild",
                            args.scope.principal, args.scope.workspace, status.reason
                        );
                    } else {
                        println!(
                            "memory index fresh for {}/{}; skipped rebuild",
                            args.scope.principal, args.scope.workspace
                        );
                    }
                    print_memory_index_status_summary(&args.scope, &status);
                }
                return Ok(());
            },
            Ok(status) => {
                info!(
                    principal = args.scope.principal,
                    workspace = args.scope.workspace,
                    reason = status.reason.as_str(),
                    "memory index stale; rebuilding"
                );
            },
            Err(error) => {
                warn!(
                    principal = args.scope.principal,
                    workspace = args.scope.workspace,
                    error = %error,
                    "memory index freshness inspection failed; attempting rebuild"
                );
            },
        }
    }

    magician::magician_v2::runtime::ollama_lifecycle::start().await;

    let storage_workspace = resolve_storage_workspace(config)?;
    let definition_store = AgentDefinitionStore::with_workspace_layout(storage_workspace.clone())
        .for_scope(&args.scope.principal, &args.scope.workspace);
    let memory_service = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.scope.principal, &args.scope.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.scope.principal, args.scope.workspace
            )
        })?;

    let outcome = rebuild_scope_memory_index(memory_service.storage(), &definition_store)
        .await
        .with_context(|| {
            format!(
                "rebuilding memory index for {}/{}",
                args.scope.principal, args.scope.workspace
            )
        })?;
    let manifest = &outcome.manifest;

    if args.scope.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "principal": args.scope.principal,
                "workspace": args.scope.workspace,
                "skipped": false,
                "document_count": manifest.document_count,
                "chunk_count": manifest.chunk_count,
                "source_count": manifest.source_count,
                "backend": manifest.backend.as_str(),
                "backend_status": manifest.backend_status.as_str(),
                "embedding_provider": manifest.embedding_provider.as_str(),
                "embedding_model": &manifest.embedding_model,
                "embedding_dimensions": manifest.embedding_dimensions,
                "rebuilt_at_ms": manifest.rebuilt_at.timestamp_millis(),
                "manifest_path": outcome.manifest_path.display().to_string(),
                "documents_path": outcome.documents_path.display().to_string(),
                "lancedb_write": &outcome.lancedb_write,
                "manifest": &manifest,
            }))?
        );
    } else {
        println!(
            "rebuilt memory index for {}/{}: {} documents, {} chunks, {} sources ({}, {})",
            args.scope.principal,
            args.scope.workspace,
            manifest.document_count,
            manifest.chunk_count,
            manifest.source_count,
            manifest.backend,
            manifest.backend_status
        );
        println!("manifest: {}", outcome.manifest_path.display());
        println!("documents: {}", outcome.documents_path.display());
        println!(
            "lancedb: mode={}, chunks={}, inserted={:?}, updated={:?}, deleted={:?}, optimize={}",
            outcome.lancedb_write.mode,
            outcome.lancedb_write.chunk_count,
            outcome.lancedb_write.inserted_rows,
            outcome.lancedb_write.updated_rows,
            outcome.lancedb_write.deleted_rows,
            if outcome.lancedb_write.optimize.completed {
                "completed"
            } else if outcome.lancedb_write.optimize.attempted {
                "failed"
            } else {
                outcome
                    .lancedb_write
                    .optimize
                    .reason
                    .as_deref()
                    .unwrap_or("not_attempted")
            }
        );
    }

    Ok(())
}

async fn optimize_memory_index_for_scope(
    config: &MagicianConfig,
    args: &MemoryIndexScopeArgs,
) -> Result<()> {
    let storage_workspace = resolve_storage_workspace(config)?;
    let memory_service = AgentMemoryResolver::with_workspace_layout(storage_workspace.clone())
        .resolve_for_scope(&args.principal, &args.workspace)
        .with_context(|| {
            format!(
                "resolving memory service for {}/{}",
                args.principal, args.workspace
            )
        })?;

    let outcome = optimize_scope_memory_index(memory_service.storage())
        .await
        .with_context(|| {
            format!(
                "optimizing memory index for {}/{}",
                args.principal, args.workspace
            )
        })?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "principal": args.principal.as_str(),
                "workspace": args.workspace.as_str(),
                "optimized": outcome.optimized,
                "skipped_reason": &outcome.skipped_reason,
                "index_dir": outcome.index_dir.display().to_string(),
                "table_name": outcome.table_name.as_str(),
                "compaction_ran": outcome.compaction_ran,
                "prune_ran": outcome.prune_ran,
            }))?
        );
    } else if outcome.optimized {
        println!(
            "optimized memory index for {}/{}: table={}, compaction={}, prune={}",
            args.principal,
            args.workspace,
            outcome.table_name,
            outcome.compaction_ran,
            outcome.prune_ran
        );
        println!("index: {}", outcome.index_dir.display());
    } else {
        println!(
            "skipped memory index optimize for {}/{}: {}",
            args.principal,
            args.workspace,
            outcome.skipped_reason.as_deref().unwrap_or("not_required")
        );
    }

    Ok(())
}

fn print_memory_index_status_summary(
    args: &MemoryIndexScopeArgs,
    status: &magician::magician_v2::agents::MemoryIndexStatus,
) {
    let freshness = if !status.stale {
        "fresh"
    } else if memory_index_stale_reason_is_soft(&status.reason) {
        "soft-stale"
    } else {
        "stale"
    };
    println!(
        "memory index {freshness} for {}/{}: {} documents, {} sources ({})",
        args.principal,
        args.workspace,
        status.current_document_count,
        status.current_source_count,
        status.reason
    );
    if let Some(manifest) = &status.manifest {
        println!(
            "manifest: {} documents, {} chunks, {} sources, {}, {}, dims={}",
            manifest.document_count,
            manifest.chunk_count,
            manifest.source_count,
            manifest.backend,
            manifest.backend_status,
            manifest.embedding_dimensions
        );
    }
}

fn resolve_frontend_delivery(cli: &Cli, config: &MagicianConfig) -> Result<FrontendDelivery> {
    let config_frontend = Some(&config.frontend);

    match cli.frontend_mode {
        FrontendModeFlag::ApiOnly => Ok(FrontendDelivery::ApiOnly),
        FrontendModeFlag::Static => {
            let path = cli
                .frontend_dir
                .clone()
                .or_else(|| config_frontend.and_then(|cfg| cfg.directory.as_ref().map(PathBuf::from)))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "frontend mode 'static' requires --frontend-dir or config.magician.frontend.directory"
                    )
                })?;
            let canonical = canonicalize_directory(&path)?;
            Ok(FrontendDelivery::Static(Arc::new(canonical)))
        },
        FrontendModeFlag::Auto => {
            if let Some(path) = cli.frontend_dir.clone() {
                let canonical = canonicalize_directory(&path)?;
                return Ok(FrontendDelivery::Static(Arc::new(canonical)));
            }

            if let Some(frontend_cfg) = config_frontend {
                match frontend_cfg.mode {
                    MagicianFrontendMode::ApiOnly => Ok(FrontendDelivery::ApiOnly),
                    MagicianFrontendMode::Filesystem => {
                        let path = frontend_cfg.directory.as_ref().ok_or_else(|| {
                            anyhow::anyhow!(
                                "magician.frontend.directory must be set when mode is 'filesystem'"
                            )
                        })?;
                        let canonical = canonicalize_directory(path)?;
                        Ok(FrontendDelivery::Static(Arc::new(canonical)))
                    },
                }
            } else {
                Ok(FrontendDelivery::ApiOnly)
            }
        },
    }
}

fn canonicalize_directory<P: AsRef<Path>>(path: P) -> Result<PathBuf> {
    let path_ref = path.as_ref();
    if !path_ref.exists() {
        bail!("frontend directory '{}' does not exist", path_ref.display());
    }

    path_ref.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize frontend directory '{}'",
            path_ref.display()
        )
    })
}

/// True when the pause record's execution can never resume: its execution
/// directory is gone, its state record never landed (a pre-V3 directory with
/// events and artifacts but no `state.json`), or its durable state is already
/// terminal. None of those can be resumed by the current runtime, so the
/// record is a leftover the store must not carry. Three legacy monoliths on
/// July runs blocked every scope-wide pause admission check for two months:
/// one on a failed run was reaped once terminal counted, two on pre-V3
/// directories survived because "unreadable" also covered "absent".
/// A genuine I/O failure is not proof and keeps the record.
fn pause_execution_record_is_gone_or_terminal(
    workspace: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    pending: &magician::magician_v2::execution::agentic::PendingPauseInfo,
) -> bool {
    let principal = pending.principal.as_deref().unwrap_or("");
    let scoped_workspace = pending.workspace.as_deref().unwrap_or("");
    let Some(task_id) = pending.task_id.as_deref().filter(|value| !value.is_empty()) else {
        return false;
    };
    let execution_id = pending.execution_id.trim();
    if principal.is_empty() || scoped_workspace.is_empty() || execution_id.is_empty() {
        return false;
    }
    match workspace
        .execution_dir(principal, scoped_workspace, task_id, execution_id)
        .try_exists()
    {
        Ok(false) => return true,
        Ok(true) => {},
        // An unreadable directory is not proof of anything; keep the record.
        Err(_) => return false,
    }
    let state_path =
        workspace.execution_state_path(principal, scoped_workspace, task_id, execution_id);
    let bytes = match std::fs::read(&state_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    document
        .get("status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(execution_status_is_terminal_on_disk)
}

/// Mirrors the reducer's terminal execution statuses
/// (`artifact_v2::reducer::execution_status_is_terminal`).
fn execution_status_is_terminal_on_disk(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "canceled")
}

/// Resolve the phone-reachable address for the optional same-Wi-Fi pairing
/// route. The listener still decides whether LAN access exists: a loopback-only
/// process never advertises a private address. Native wildcard listeners use
/// the default-route interface, while containers stay remote-only unless an
/// operator supplies an explicit host-reachable override.
fn mobile_local_origin(host: &str, port: u16) -> Option<String> {
    let configured = std::env::var("MAGICIAN_MOBILE_LOCAL_ORIGIN")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if configured.as_deref().is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "off" | "disabled" | "none"
        )
    }) {
        return None;
    }

    let listener = host.trim().trim_matches(['[', ']']);
    if listener.eq_ignore_ascii_case("localhost")
        || listener == "::1"
        || listener.starts_with("127.")
    {
        return None;
    }
    if let Some(origin) = configured {
        return Some(origin);
    }
    if std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
        || std::env::var_os("MAGICIAN_CONTAINER_HOST").is_some()
    {
        return None;
    }

    let address = listener
        .parse::<std::net::IpAddr>()
        .ok()
        .filter(|address| !address.is_unspecified())
        .or_else(|| {
            let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).ok()?;
            socket
                .connect((std::net::Ipv4Addr::new(1, 1, 1, 1), 80))
                .ok()?;
            Some(socket.local_addr().ok()?.ip())
        })?;
    let private = match address {
        std::net::IpAddr::V4(address) => {
            address.is_private() || address.is_loopback() || address.is_link_local()
        },
        std::net::IpAddr::V6(address) => {
            address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local()
        },
    };
    if !private || address.is_loopback() {
        return None;
    }
    Some(match address {
        std::net::IpAddr::V4(address) => format!("http://{address}:{port}"),
        std::net::IpAddr::V6(address) => format!("http://[{address}]:{port}"),
    })
}

async fn run_http_server(
    host: String,
    port: u16,
    service: MagicianService,
    frontend_delivery: FrontendDelivery,
    local_tool_services: Arc<LocalToolServices>,
    magician_config: MagicianConfig,
    capability_workspace: Arc<magician::magician_v2::artifact_v2::CapabilityWorkspaceManager>,
    disk_pack_names: std::collections::HashSet<String>,
    repo_root: PathBuf,
    runtime_plan: EffectiveRuntimePlan,
    storage_runtime: Arc<magician_storage::StorageRuntime>,
    mut startup_http: startup_http::StartupHttp,
) -> Result<()> {
    let MagicianService {
        orchestrator,
        event_broadcaster,
        workspace_event_log_registry,
        ask_loop_api,
    } = service;

    MagicianV2Orchestrator::register_resume_listener(&orchestrator, &ask_loop_api);

    let static_dir: Option<Arc<PathBuf>> = match frontend_delivery {
        FrontendDelivery::ApiOnly => {
            info!("🎨 Magician running in API-only mode (no frontend serving)");
            None
        },
        FrontendDelivery::Static(dir) => {
            info!("🎨 Serving Magician frontend assets from {}", dir.display());
            Some(dir)
        },
    };

    let static_dir_for_server = static_dir.clone();
    // Keep every outward-facing runtime surface on the same resolved V3 root.
    let storage_workspace = capability_workspace.workspace_layout().clone();
    let storage_root = storage_workspace.base_root().to_path_buf();
    let shared_storage_runtime = web::Data::new(storage_runtime);

    // Phase 5C3b — one process-wide, bounded MCP OAuth callback registry. The
    // callback route is live, but the broker remains dormant until the governed MCP
    // strategy registers an exact coordinator and begins a flow.
    let mobile_public_origin = magician_config.mobile_access.resolved_public_origin();
    let mobile_local_origin = mobile_local_origin(&host, port);
    let shared_mcp_oauth_api = Arc::new(
        magician_api::mcp_oauth_api::McpOAuthApi::new_with_callback_origin(
            Arc::clone(&event_broadcaster),
            port,
            mobile_public_origin.as_deref(),
        )
        .map_err(|_| anyhow::anyhow!("invalid MCP OAuth callback public origin"))?,
    );
    magician_api::mcp_oauth_api::McpOAuthApi::install_shared(Arc::clone(&shared_mcp_oauth_api))
        .map_err(|_| anyhow::anyhow!("MCP OAuth callback broker was installed more than once"))?;
    let shared_mcp_oauth_api = web::Data::from(shared_mcp_oauth_api);

    // Work-evidence graph (Phase 0) read-path API. Built here (outside the chat
    // let-tuple) so the actix factory can capture it for app_data.
    let shared_evidence_api = web::Data::new(
        EvidenceApi::new(
            AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
            storage_workspace.clone(),
        )
        .with_event_broadcaster(Arc::clone(&event_broadcaster)),
    );
    // WEG Phase 2: ambient browser-capture ingestion + consent + distillation.
    let shared_ambient_api = web::Data::new(AmbientApi::new(
        storage_workspace.clone(),
        AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
        Some(Arc::clone(&event_broadcaster)),
    ));
    let shared_storage_root = web::Data::new(storage_root.clone());
    let shared_app_platform_api = web::Data::new(
        AppPlatformApi::new(storage_workspace.clone())
            .with_event_broadcaster(Arc::clone(&event_broadcaster)),
    );
    // Subscribe the legacy LLM-call Parquet compatibility mirror before the
    // canonical owner's synchronous recovery barrier. This keeps the temporary
    // mirror and canonical receiver at the same pre-producer boundary: events
    // buffered while recovery runs cannot appear canonically but disappear
    // from Phase-2 reconciliation.
    let _llm_parquet_sink =
        magician::magician_v2::analytics::llm_parquet_sink::LlmParquetSink::spawn(
            Arc::clone(&event_broadcaster),
            storage_workspace.clone(),
        );

    // Canonical trace recovery owns its journal and does not depend on App
    // package admission. Start its blocking recovery now so those independent
    // startup paths overlap. The handle is joined below before any model queue
    // or HTTP admission; recovery failure still prevents service readiness.
    let llm_trace_startup = {
        let broadcaster = Arc::clone(&event_broadcaster);
        let workspace = storage_workspace.clone();
        tokio::task::spawn_blocking(move || {
            magician::magician_v2::analytics::llm_trace_activation::LlmTraceActivation::start(
                broadcaster,
                workspace,
                magician::magician_v2::analytics::llm_trace_activation::LlmTraceActivationConfig::default(),
            )
        })
    };

    // Admit the deployment's own `distribution: system` packages before any
    // worker or route can look for them. Both staging and publication refuse a
    // system manifest without this owner, so until it ran the seeded packages
    // (meetings, town_square, claims_review, learning, thinking_map) parsed in
    // tests and were unreachable in a running binary.
    //
    // Awaited rather than spawned: a projection worker or a `/social/*` request
    // that arrives before admission finishes would legitimately answer "not
    // installed", and that answer would be cached reasoning about a square that
    // is about to exist. Publication is idempotent by content, so this is cheap
    // on every boot after the first.
    startup_http.phase("apps_and_history");
    let process_chat_store = Arc::new(FileChatStore::with_workspace_layout(
        storage_workspace.clone(),
    ));
    // Independent stores own disjoint files. Keep their recovery barriers while
    // overlapping I/O; no consumer or producer sees partially loaded state.
    let (_system_package_reports, user_request_service, ()) = tokio::join!(
        shared_app_platform_api
            .admit_system_packages_at_boot(magician_config.app_platform.system_packages),
        async {
            Arc::new(
                UserRequestService::new(Arc::clone(&event_broadcaster))
                    .with_workspace_layout(storage_workspace.clone())
                    .with_history_persist_path(storage_root.join("user_request_history.json"))
                    .with_pending_persist_path(storage_root.join("user_request_pending.json"))
                    .await,
            )
        },
        async {
            if let Err(error) = process_chat_store.initialize().await {
                warn!(%error, "Failed to initialize chat store index");
            }
        },
    );
    let _app_projection_worker = shared_app_platform_api.spawn_projection_worker();
    startup_http.phase("execution_and_chat");

    // Resource-authority scope resolver — constructed once at outer
    // scope so BOTH the dispatch gate (via `CompiledDispatchAuthority`,
    // built inside the chat let-tuple block below) and the REST API
    // surface (`ResourceAuthorityApi`, constructed after the tuple
    // closes) share the same `Arc<dyn ScopedAuthorityResolver>`. The
    // shared resolver guarantees per-`(principal, workspace)` ledger
    // + token store + ceilings + freeze Arcs are reused across both
    // paths — writes through one surface are visible to the other.
    let scoped_authority_resolver: Arc<
        dyn magician::magician_v2::resource_authority::scoped_authority::ScopedAuthorityResolver,
    > = Arc::new(
        magician::magician_v2::resource_authority::scoped_authority::DiskBackedScopedResolver::new(
            magician_config.resource_authority.clone(),
            storage_workspace.clone(),
        ),
    );
    let analytics_shutdown = CancellationToken::new();
    let analytics_dispatcher =
        Arc::new(magician::magician_v2::analytics::AnalyticsDispatcher::new(
            storage_workspace.clone(),
            analytics_shutdown.clone(),
        ));
    let _ = magician::magician_v2::analytics::init(analytics_dispatcher);
    info!("Analytics layer initialized (scoped DuckDB)");

    // Drain the runtime activity channel onto the event bus. The layer that
    // fills it was registered back in `init_tracing`, long before any
    // broadcaster existed, so this is the moment the producer gains a
    // consumer: every span and log line queued during boot is delivered here,
    // minus whatever the bounded queue had to evict (counted, and carried on
    // the `dropped` field of every activity event).
    //
    // Shares `analytics_shutdown` because it is the same plane and that token
    // is cancelled last, so late shutdown activity still reaches a viewer.
    // CLI-only invocations never reach this point and never build a
    // broadcaster; the channel simply stays bounded at 4096 and drops oldest,
    // which is why it must never be an unbounded buffer.
    // The retrospective half of the same capture point. The forwarder taps
    // every closed span into it on the way to the bus, so the live stream and
    // the durable store cannot disagree about what happened — they are fed by
    // one drain, not by two instrumentation passes.
    //
    // Constructed before the forwarder so no close is drained without a sink
    // to receive it.
    let (activity_rows_sink, activity_rows_handle) =
        magician::magician_v2::analytics::activity_rows_sink::ActivityRowsSink::spawn(
            storage_workspace.clone(),
        );

    let activity_forwarder =
        magician::magician_v2::analytics::runtime_activity_layer::spawn_activity_forwarder(
            magician::magician_v2::analytics::runtime_activity_layer::global_activity_channel(),
            &event_broadcaster,
            Some(activity_rows_handle),
            analytics_shutdown.clone(),
        );

    // Canonical Phase-2 LLM capture is process-owned and active by default.
    // Subscribe before the rest of the HTTP/runtime workers start emitting so
    // every scoped response fact reaches the append-before-materialize journal.
    // The old llm_calls writer above remains only as a compatibility mirror.
    // Scoped llm_dispatch remains an auxiliary queue/local-prep timing source,
    // joined to canonical attempts by stable dispatch identity.
    // Preserve the recovery barrier before content capture, queue attachment,
    // or HTTP readiness. Recovery ran beside independent App admission above.
    let mut llm_trace_activation = llm_trace_startup
        .await
        .context("joining canonical LLM trace startup")?
        .context("activating canonical LLM trace journal and materializer")?;
    let restricted_recovery_scopes =
        magician::magician_v2::analytics::llm_restricted_content::discover_restricted_recovery_scopes(
            &storage_workspace,
        )?;
    let restricted_capacity = magician_config.analytics.llm_trace.payload_records;
    let restricted_llm_pipeline =
        magician::magician_v2::analytics::llm_trace_materializer::build_restricted_llm_trace_pipeline(
            storage_workspace.clone(),
            magician::magician_v2::analytics::llm_trace_journal::LlmTraceJournalConfig {
                critical_capacity: restricted_capacity.max(1),
                lineage_capacity: restricted_capacity.saturating_mul(8).clamp(1, 8_192),
                restricted_payload_capacity: restricted_capacity,
                ..Default::default()
            },
            restricted_recovery_scopes,
        )
        .context("activating restricted LLM content journal and materializer")?;
    let restricted_llm_recorder: Arc<
        dyn magician::magician_v2::analytics::llm_trace_recorder::LlmTraceRecorder,
    > = restricted_llm_pipeline.recorder();
    let llm_content_capture =
        magician::magician_v2::analytics::llm_trace_content::LlmContentCaptureRuntime::start(
            storage_workspace.clone(),
            Arc::clone(&restricted_llm_recorder),
            llm_trace_activation.recorder(),
            Arc::clone(orchestrator.secret_store_resolver()),
            magician_config.analytics.llm_trace.clone(),
        );
    if !llm_content_capture.is_active() {
        warn!("Restricted LLM content capture is unavailable; metadata capture remains active");
    }
    let llm_content_settings = llm_content_capture.settings_handle();
    let restricted_llm_content = Arc::new(
        magician::magician_v2::analytics::llm_restricted_content::LlmRestrictedContentService::new(
            storage_workspace.clone(),
            Arc::clone(&restricted_llm_recorder),
            Arc::clone(orchestrator.secret_store_resolver()),
            llm_content_settings.clone(),
        ),
    );
    let _llm_restricted_content_retention =
        magician::magician_v2::analytics::llm_restricted_content::LlmRestrictedContentRetention::spawn(
            storage_workspace.clone(),
            llm_content_settings.clone(),
        );
    if let Some(operation_router) = orchestrator.operation_llm_router() {
        operation_router.set_event_broadcaster(Arc::clone(&event_broadcaster));
        operation_router.set_content_capture_sink(llm_content_capture.sink());
    }

    // Separate flat dataset for local embedding-batch telemetry. Registered as
    // the process-global sink so embed call sites (which hold scope) can record
    // best-effort/non-blocking without threading a handle through every
    // subsystem. Kept apart from llm_calls so the hot per-call path is never
    // bloated. The singular storage-maintenance runtime below owns its
    // completed-day compaction, active-day rolling compaction, and retention.
    let _llm_embeddings_sink = {
        let sink = magician::magician_v2::analytics::llm_embeddings_sink::LlmEmbeddingsSink::spawn(
            storage_workspace.clone(),
        );
        if magician::magician_v2::analytics::llm_embeddings_sink::set_global_sink(sink.clone())
            .is_err()
        {
            warn!("llm_embeddings sink was already registered; keeping the existing global sink");
        }
        sink
    };

    // Chat transcript compaction is owned by the same background maintenance
    // lifecycle. Build its store before spawning that owner; request and answer
    // paths only share this instance and never schedule compaction themselves.
    // Publish the initialized store so workspace-coupled READ owners built
    // without the server's wiring — the `meetings_data` app binder's thread and
    // transcript reads — share this index instead of rebuilding one per scope.
    // Read-only by contract; every mutation still goes through the owners that
    // already hold the store.
    if !magician::magician_v2::chat::storage::publish_global_chat_store(Arc::clone(
        &process_chat_store,
    )
        as Arc<dyn magician::magician_v2::chat::storage::ChatStore>)
    {
        warn!("A chat store was already published for this process; keeping the first one");
    }

    // One guarded owner coordinates rolling canonical LLM compaction,
    // completed-day batch compaction, and the shared 90-day
    // events/memory/LLM lineage retention policy. Keeping this singular avoids
    // a legacy retention worker racing verified Parquet publication.
    let storage_maintenance =
        magician::magician_v2::analytics::parquet_maintenance::StorageMaintenanceRuntime::spawn_with_chat_store(
            storage_workspace.clone(),
            Arc::clone(&process_chat_store),
        );

    // Dashboard theme registry — embedded YAMLs loaded at startup; scope
    // overlays are intentionally not loaded here since the process is
    // multi-scope. Per-scope overlay loading is a follow-up if/when a
    // scope-specific theme registry is needed.
    let dashboard_theme_registry =
        match magician::magician_v2::dashboard_themes::DashboardThemeRegistry::load(None) {
            Ok(registry) => {
                info!(themes = registry.len(), "Dashboard theme registry loaded");
                web::Data::new(registry)
            },
            Err(error) => {
                warn!(error = %error, "Dashboard theme registry failed to load — falling back to empty registry");
                web::Data::new(
                    magician::magician_v2::dashboard_themes::DashboardThemeRegistry::default(),
                )
            },
        };

    // Create shared FullPauseStore ONCE — not per-worker.
    // actix-web calls the HttpServer::new() factory once per worker thread.
    // If the store is created inside the factory, each worker gets its own
    // independent DashMap, so pause data stored by one worker is invisible
    // to API handlers running on a different worker.
    let shared_pause_store = {
        let pause_states_path = orchestrator.pause_states_storage_path();
        let store = FullPauseStore::new_shared_with_scoped_v3_persistence_root(&pause_states_path);
        // Load any paused executions from disk (crash recovery), reaping any
        // ORPHANED pause states before surfacing the rest.
        //
        // A pause can only resume if its execution record still exists on disk
        // (resume reloads that record). When the execution dir is gone — the task
        // was deleted/archived, or it's a legacy "direct" execution whose record
        // was never v3-persisted — the pause is a dead ghost: every boot reloads
        // it and it shows as a perpetual paused/"running" entry that no cancel
        // endpoint can clear ("Execution not found"). Reap those so they stop
        // accumulating. Conservative: only reap when we can positively locate the
        // execution path AND it is absent; anything we cannot verify is kept.
        match store.load_pending_index_from_disk() {
            Ok(loaded) => {
                let mut reaped = 0usize;
                let mut survivors = 0usize;
                for pause_data in &loaded {
                    let task_id = pause_data.task_id.as_deref();
                    let execution_id =
                        Some(pause_data.execution_id.as_str()).filter(|value| !value.is_empty());
                    if pause_execution_record_is_gone_or_terminal(&storage_workspace, pause_data) {
                        if let Some(execution_id) = execution_id {
                            if store.remove_for_execution(execution_id) > 0 {
                                reaped += 1;
                                warn!(
                                    execution_id,
                                    task_id = task_id.unwrap_or(""),
                                    "[STARTUP] Reaped orphaned pause state — execution gone or already terminal, cannot resume"
                                );
                                continue;
                            }
                        }
                    }
                    survivors += 1;
                    info!(
                        "[STARTUP] Found paused execution: execution={:?}, plan={:?}, step={:?}",
                        execution_id, pause_data.plan_id, pause_data.step_id,
                    );
                }
                if reaped > 0 {
                    info!(
                        reaped,
                        "[STARTUP] Reaped orphaned pause states (no execution record on disk)"
                    );
                }
                if survivors > 0 {
                    info!(
                        survivors,
                        "[STARTUP] Loaded paused executions from disk for crash recovery"
                    );
                }
            },
            Err(e) => {
                warn!(
                    "[STARTUP] Failed to load paused executions from disk: {}",
                    e
                );
            },
        }
        // Set on orchestrator once (not per-worker)
        orchestrator.set_full_pause_store(store.clone());
        store.begin_legacy_repair_reconciliation();
        let repair = store.schedule_legacy_pending_index_background_repair();
        let repaired_store = Arc::clone(&store);
        let repaired_workspace = storage_workspace.clone();
        tokio::spawn(async move {
            let mut repair = repair;
            loop {
                if let Some(active_repair) = repair.take() {
                    if let Err(error) = active_repair.await {
                        warn!(
                            %error,
                            "[STARTUP] Legacy pause metadata repair task failed to join; keeping pause API fail-closed"
                        );
                        return;
                    }
                } else {
                    // Another startup/API owner may already hold the
                    // single-flight drainer. Wait without doing filesystem
                    // work on this Tokio task.
                    while repaired_store.legacy_pending_index_background_repair_active() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }

                if !repaired_store.legacy_pending_index_repair_work_in_progress() {
                    break;
                }
                repair = repaired_store.schedule_legacy_pending_index_background_repair();
            }
            let mut reaped = 0usize;
            for pending in repaired_store.persisted_pending_snapshot() {
                if !pause_execution_record_is_gone_or_terminal(&repaired_workspace, &pending) {
                    continue;
                }
                if repaired_store.remove_for_execution(&pending.execution_id) > 0 {
                    reaped = reaped.saturating_add(1);
                    warn!(
                        execution_id = %pending.execution_id,
                        task_id = pending.task_id.as_deref().unwrap_or(""),
                        "[STARTUP] Reaped repaired legacy orphan pause state — execution gone or already terminal"
                    );
                }
            }
            if reaped > 0 {
                info!(
                    reaped,
                    "[STARTUP] Reaped repaired legacy orphan pause states"
                );
            }
            repaired_store.complete_legacy_repair_reconciliation();
        });
        store
    };
    // Create shared agent API services ONCE — not per-worker.
    // This keeps runtime/scheduler/paused state coherent across workers.
    let shared_agent_api = MagicianV2Api::build_shared_agent_api_services_with_workspace(
        &orchestrator,
        storage_workspace.clone(),
    );

    orchestrator.set_definition_store(Arc::clone(&shared_agent_api.definition_store));
    info!("[STARTUP] Agent definition store wired to main orchestrator");
    // Startup hydration is deliberately NOT spawned here. It re-arms scoped
    // autonomous goals, and a re-armed goal that is already due fires straight
    // into the agent runtime — which at this point has no `artifact_v2_service`,
    // because that is wired several thousand lines below. Every such cycle was
    // recorded as a *failed* goal cycle rather than deferred, so each boot
    // silently burned the due harness cycles for every scope. It is spawned
    // after `stateless_loop_lifecycle_ready` opens instead; see the call site.
    let _memory_eval_runner = MemoryEvalRunner::spawn(
        storage_workspace.clone(),
        shared_agent_api.definition_store.as_ref().clone(),
        AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
        MemoryEvalRunnerConfig::from_env(),
    );
    let _memory_utility_batch_runner = MemoryUtilityBatchRunner::spawn(
        storage_workspace.clone(),
        AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
        orchestrator.operation_llm_router(),
        Some(Arc::clone(&event_broadcaster)),
        MemoryUtilityBatchRunnerConfig::from_env(),
    );
    // WEG Phase 2: Observe-tabs lifecycle distillation. Collection is controlled
    // by the `/observe` card; this worker only processes scopes whose ambient
    // consent flag is currently enabled, and the distill operation is
    // fail-closed to a local Ollama profile.
    let _ambient_distill_worker = AmbientDistillWorker::spawn(
        storage_workspace.clone(),
        AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
        Some(Arc::clone(&event_broadcaster)),
        orchestrator.operation_llm_router(),
        orchestrator.prompt_manager(),
        AmbientDistillConfig::from_env(),
    );
    // Channel Assist: background metadata sync worker.
    // Kill-switch CHANNEL_SYNC_ENABLED=0 skips the spawn entirely; the loop
    // additionally no-ops per tick until the user enables the email
    // observe producer for the default scope (the `/observe` card). The
    // store instance is shared with the sync status/run APIs below.
    let channel_assist_store =
        magician_comms::channel_assist::channel::ChannelAssistStore::open_workspace(
            storage_workspace.clone(),
        )
        .context("opening mail assist store")?
        .with_database_maintenance(magician_config.database_maintenance.clone());
    let attention_funnel_store =
        magician::magician_v2::attention_funnel_store::AttentionFunnelStore::open_at(
            &magician::magician_v2::database_owners::host_database_path(
                storage_workspace.base_root(),
                magician::magician_v2::database_owners::DatabaseOwner::AttentionFunnel,
            ),
        )
        .context("opening attention funnel store")?;
    // The rebuildable list index beside the file store. Opening it can
    // DISCARD a stale or damaged file — that is the designed outcome, not a
    // failure, so the only error worth propagating is a root we cannot write
    // to at all.
    let list_index = magician::magician_v2::storage::ListIndex::open(storage_workspace.base_root())
        .context("opening list index")?;
    if list_index_rebuild_owed(&list_index) {
        // Off the reactor and off the startup path: the walk is disk-bound
        // and unbounded in size. While it runs `is_ready()` is false and
        // every list handler serves from the file walk, so the server is
        // correct from its first request rather than from the moment this
        // finishes.
        let rebuilding = list_index.clone();
        let scopes_root = storage_workspace.scopes_root();
        tokio::spawn(async move {
            match rebuilding.rebuild_from_disk_async(scopes_root).await {
                Ok(report) => info!(
                    units_walked = report.units_walked,
                    units_resumed = report.units_resumed,
                    entries_indexed = report.entries_indexed,
                    entries_skipped = report.entries_skipped,
                    entries_hidden = report.entries_hidden,
                    unparsed_timestamps = report.unparsed_timestamps,
                    "[LIST-INDEX] Rebuilt the list index from disk"
                ),
                // `{:#}` rather than `%error`: `Display` on an `anyhow::Error`
                // prints only the outermost context, and the whole diagnostic
                // value here is the `io::ErrorKind` underneath it — an
                // operator needs to know whether the root was EACCES, ENOTDIR
                // or EMFILE, not merely which path failed.
                Err(error) => warn!(
                    error = %format!("{error:#}"),
                    "[LIST-INDEX] List index rebuild failed; lists keep serving from the file walk"
                ),
            }
        });
    }
    // The backstop behind the write hooks. Every writer that lands a task
    // record is supposed to tell the index, and three separate correctness
    // bugs have been one that did not. This pass does not care which door the
    // drift came through: a writer nobody hooked costs one interval of
    // wrongness instead of an unbounded wrong answer.
    //
    // Spawned AFTER the boot rebuild above is decided, and its first tick is
    // one interval out rather than immediate. `reconcile_from_disk` declines a
    // half-built index on its own, so an overlapping pass would be harmless —
    // but a boot that owes no rebuild would otherwise walk every scope while
    // the rest of startup is still disk-bound, for drift that can just as well
    // be found five minutes later.
    {
        const RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);
        let reconciling = list_index.clone();
        let reconcile_scopes_root = storage_workspace.scopes_root();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval_at(
                tokio::time::Instant::now() + RECONCILE_INTERVAL,
                RECONCILE_INTERVAL,
            );
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let index = reconciling.clone();
                let scopes_root = reconcile_scopes_root.clone();
                // Disk-bound and unbounded in size, exactly like the rebuild
                // above, so it runs off the reactor.
                let pass =
                    tokio::task::spawn_blocking(move || index.reconcile_from_disk(&scopes_root))
                        .await;
                match pass {
                    // Silent unless something moved. There is no alert and no
                    // health check on this loop, so a repair count that stops
                    // being zero is the ONLY way a missing writer hook becomes
                    // noticeable — and a line every five minutes reporting that
                    // nothing happened is exactly where that signal would be
                    // buried.
                    Ok(Ok(report)) => {
                        if report.repaired > 0 || report.removed > 0 {
                            info!(
                                units = report.units,
                                records_read = report.records_read,
                                repaired = report.repaired,
                                removed = report.removed,
                                "[LIST-INDEX] Reconciliation repaired list index drift"
                            );
                        }
                    },
                    // Keep ticking. A reconciler that stops after one bad pass
                    // is worse than none at all, because it still looks alive.
                    Ok(Err(error)) => warn!(
                        error = %error,
                        "[LIST-INDEX] A reconciliation pass failed; the next tick retries it"
                    ),
                    Err(error) => warn!(
                        error = %error,
                        "[LIST-INDEX] The reconciliation pass panicked; the next tick retries it"
                    ),
                }
            }
        });
    }
    let attention_learning_service =
        magician::magician_v2::attention::learning::AttentionLearningService::open(
            storage_workspace.base_root(),
            magician_config.attention_learning.clone(),
        )
        .context("opening attention learning service")?;
    // One device-bridge hub for the process. Companion devices dial in and hold
    // a socket; the hub owns the registry of who is connected and the table of
    // in-flight requests, so it must outlive any single worker thread.
    let device_bridge_hub = Arc::new(magician::magician_v2::device_bridge::DeviceBridgeHub::new());

    // Desktop Edge sessions are a separate typed transport from mobile MCP.
    // They reuse paired-device credentials, while this registry owns the
    // outbound socket generation, lease, capability inventory and calls.
    let edge_session_registry =
        Arc::new(magician::magician_v2::runtime::edge_sessions::EdgeSessionRegistry::new());

    // Published so compiled tools can reach it without threading a handle
    // through every agent-resource construction site.
    magician::magician_v2::device_bridge::install_global_hub(device_bridge_hub.clone());

    // The roster of devices allowed to open a bridge. A missing file means
    // nothing is paired yet, which is the right state for a fresh install and
    // not a reason to refuse startup.
    let device_pairing_store = Arc::new(
        match magician::magician_v2::device_pairing::DevicePairingStore::open(
            storage_workspace.base_root(),
        )
        .await
        {
            Ok(store) => store,
            Err(error) => {
                warn!(
                    error = %error,
                    "[DEVICE-PAIRING] durable owner unavailable; device authority routes are disabled"
                );
                magician::magician_v2::device_pairing::DevicePairingStore::unavailable(
                    storage_workspace.base_root(),
                    error.to_string(),
                )
            },
        },
    );
    #[cfg(target_os = "macos")]
    let android_apps_owner_store = Arc::new(
        match magician::magician_v2::apps::android_owner::AppAndroidOwnerStore::open(
            storage_workspace.base_root(),
        )
        .await
        {
            Ok(store) => store,
            Err(error) => {
                warn!(
                    error = %error,
                    "[ANDROID-APPS-OWNER] desktop owner authority unavailable; Android Apps owner routes are disabled"
                );
                magician::magician_v2::apps::android_owner::AppAndroidOwnerStore::unavailable(
                    storage_workspace.base_root(),
                    error.to_string(),
                )
            },
        },
    );
    #[cfg(not(target_os = "macos"))]
    let android_apps_owner_store = Arc::new(
        magician::magician_v2::apps::android_owner::AppAndroidOwnerStore::unavailable(
            storage_workspace.base_root(),
            "Android Apps desktop-owner bootstrap requires a signed macOS runtime",
        ),
    );
    magician::magician_v2::apps::android_owner::install_global_android_owner_store(
        android_apps_owner_store.clone(),
    );
    #[cfg(target_os = "macos")]
    if android_apps_owner_store.is_available() {
        let bootstrap_store = android_apps_owner_store.clone();
        tokio::spawn(async move {
            loop {
                let result = magician::magician_v2::apps::android_owner_bootstrap::run_once(
                    bootstrap_store.clone(),
                )
                .await;
                if bootstrap_store.is_peer_verified() && result.is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }
    let mobile_push_store = Arc::new(
        magician::magician_v2::mobile_push::MobilePushStore::open(storage_workspace.base_root())
            .await
            .context("opening mobile push registration store")?,
    );
    let mobile_push_config = magician::magician_v2::mobile_push::MobilePushConfig::from_env();
    let mobile_push_dispatcher = mobile_push_config.any_enabled().then(|| {
        let dispatcher = Arc::new(
            magician::magician_v2::mobile_push::MobilePushDispatcher::new(
                mobile_push_store.clone(),
                mobile_push_config.clone(),
            ),
        );
        Arc::clone(&dispatcher).start(event_broadcaster.clone());
        dispatcher
    });

    // Critical-request delivery (secure HITL plan §6.1): one coordinator over
    // the HITL lifecycle drives the attention push and the channel bots the
    // owner enabled, with one policy and one delivery record per destination.
    // Always running — bots may be enabled without push — and published
    // process-wide for the API's claims, reports, status and test action.
    {
        use magician::magician_v2::hitl_delivery;
        let delivery_store =
            Arc::new(hitl_delivery::DeliveryStore::open(storage_workspace.base_root()).await);
        let push_sink: Option<Arc<dyn hitl_delivery::PushSink>> = mobile_push_dispatcher
            .clone()
            .map(|dispatcher| dispatcher as Arc<dyn hitl_delivery::PushSink>);
        let oracle = Arc::new(hitl_delivery::RuntimeRequestOracle::new(
            Arc::clone(&user_request_service),
            Some(Arc::clone(&shared_pause_store)),
            Some(Arc::clone(&process_chat_store)
                as Arc<dyn magician::magician_v2::chat::storage::ChatStore>),
        ));
        let coordinator = Arc::new(hitl_delivery::DeliveryCoordinator::new(
            hitl_delivery::DeliveryPolicy::from_config(&magician_config),
            delivery_store,
            push_sink,
            Arc::clone(&event_broadcaster) as Arc<dyn hitl_delivery::ChannelTransport>,
            oracle,
        ));
        // Deliveries the previous run left live are re-driven, not retired: the
        // owner still owes an answer to a credential ask that outlived the
        // restart, and nothing else re-announces it. Each one is checked against
        // its own request first, so a request that closed while the runtime was
        // down is retired instead of re-alerted.
        //
        // BEFORE the subscription, and awaited: an announcement that arrives
        // while recovery is reading would otherwise have its fresh rows swept
        // into the recovery's own group and re-offered, and `fan_out`'s
        // `active` insert would overwrite the live task's cancellation token —
        // two cards for one ask, one of them no longer cancellable.
        coordinator.recover_after_restart().await;
        Arc::clone(&coordinator).start(event_broadcaster.clone());
        hitl_delivery::install_global(coordinator);
    }

    // Automatic verification-code retrieval (secure HITL plan §6.2): one
    // resolver over the HITL lifecycle watches the sources the owner permitted
    // for a live `otp` ask and answers it through the ask's own path. The
    // agentic answer sink is installed by the API layer once it exists.
    {
        use magician::magician_v2::verification_codes;
        let layout = Arc::new(storage_workspace.clone());
        let registry = Arc::new(verification_codes::RuntimeSourceRegistry::new(Arc::clone(
            &layout,
        )));
        let sink = Arc::new(verification_codes::RuntimeAnswerSink::new(Arc::clone(
            &user_request_service,
        )));
        let resolver = Arc::new(verification_codes::VerificationCodeResolver::new(
            magician_config.hitl.verification_codes.clone(),
            registry,
            sink,
            Arc::clone(&event_broadcaster) as Arc<dyn verification_codes::StatusTransport>,
        ));
        resolver
            .register_watch(Arc::new(
                magician_comms::channel_assist::verification_sources::GmailVerificationWatch::new(
                    Arc::clone(&layout),
                    repo_root.clone(),
                ),
            ))
            .await;
        resolver
            .register_watch(Arc::new(
                magician_comms::channel_assist::verification_sources::AgentMailVerificationWatch::new(
                    Arc::clone(&layout),
                    repo_root.clone(),
                ),
            ))
            .await;
        resolver
            .register_watch(magician_comms::channel_assist::verification_sources::MessagesVerificationWatch::shared())
            .await;
        resolver
            .register_watch(Arc::new(verification_codes::AndroidVerificationWatch))
            .await;
        Arc::clone(&resolver).start(event_broadcaster.clone());
        if let Some(coordinator) = magician::magician_v2::hitl_delivery::global() {
            coordinator
                .set_retrieval_oracle(Arc::clone(&resolver)
                    as Arc<dyn magician::magician_v2::hitl_delivery::RetrievalOracle>)
                .await;
        }
        verification_codes::install_global(resolver);
    }

    // Device governance: the scope-wide screenshot policy and the durable
    // audit of what was done on the phone. Published process-wide the same
    // way the hub is, so the compiled android_* handlers can reach them.
    let device_policy_store = Arc::new(
        magician::magician_v2::device_governance::DevicePolicyStore::open(
            storage_workspace.base_root(),
        )
        .await
        .context("opening device policy store")?,
    );
    let device_action_audit = Arc::new(
        magician::magician_v2::device_governance::DeviceActionAudit::open(
            storage_workspace.base_root(),
        ),
    );
    magician::magician_v2::device_governance::install_global_device_governance(
        device_policy_store.clone(),
        device_action_audit.clone(),
    );

    // Engagement authority (OPC Workstream B): the store the dispatch boundary
    // consults for engagement-scoped executions. Published process-wide like
    // the device stores. A corrupt roster fails boot deliberately — a defaulted
    // roster under live engagement ids would deny every scoped action
    // undebuggably; better to refuse and keep the file's history.
    let engagement_store = Arc::new(
        magician::magician_v2::engagements::EngagementStore::open(storage_workspace.base_root())
            .await
            .context("opening engagement authority store")?,
    );
    magician::magician_v2::engagements::install_global_engagement_store(engagement_store.clone());

    // The counterparty register (OPC Workstream B): who we are in a relationship
    // WITH, as distinct from the engagement that authorises acting inside it.
    // Published process-wide for the same reason and by the same pattern — the
    // owner write routes, the inbound identification path and the outbound
    // minting leg are all compiled handlers that cannot be handed a constructor
    // argument.
    //
    // Not installing it is not a degraded mode. `global_counterparty_store()`
    // answering `None` makes every one of those callers report "this
    // organisation has no addresses" for a register it never opened, which is a
    // broken process describing a healthy one. Installing at boot is what makes
    // an empty answer mean empty.
    magician::magician_v2::counterparties::install_global_counterparty_store(Arc::new(
        magician::magician_v2::counterparties::CounterpartyStore::new(storage_workspace.clone()),
    ));
    magician_media::scheduling::install_recipient_compliance_scheduling_reader();

    // Cloudflare Access identity. `None` unless both the team domain and the
    // application audience are configured. Native device credentials are
    // independently verified even when this optional browser assertion layer
    // is absent.
    let access_verifier = magician::magician_v2::cloudflare_access::AccessVerifier::from_env();
    let (
        android_apps_signing_sha256,
        android_attestation_root_sha256,
        android_apps_apk_sha256,
        android_apps_version_codes,
        android_play_integrity_cloud_project_number,
        android_play_integrity_version_codes,
        android_play_integrity_required_device_verdicts,
    ) = magician_config
        .mobile_access
        .android_apps_attestation_pins();
    let mobile_enrollment_config = web::Data::new(
        magician_api::device_pairing_api::MobileEnrollmentConfig::resolve_with_apps_pins(
            mobile_public_origin.as_deref(),
            mobile_local_origin.as_deref(),
            android_apps_signing_sha256,
            android_attestation_root_sha256,
            android_apps_apk_sha256,
            android_apps_version_codes,
            android_play_integrity_cloud_project_number,
            android_play_integrity_version_codes,
            android_play_integrity_required_device_verdicts,
        )
        .map_err(|code| anyhow::anyhow!("invalid mobile enrollment origin: {code}"))?,
    );
    match mobile_enrollment_config.public_origin.as_deref() {
        Some(origin) => info!("[MOBILE-ACCESS] remote enrollment origin: {origin}"),
        None => info!("[MOBILE-ACCESS] remote enrollment disabled: no public origin configured"),
    }
    match mobile_enrollment_config.local_origin.as_deref() {
        Some(origin) => info!("[MOBILE-ACCESS] same-Wi-Fi enrollment origin: {origin}"),
        None => info!("[MOBILE-ACCESS] same-Wi-Fi enrollment unavailable"),
    }
    match access_verifier.as_ref() {
        Some(verifier) => info!(
            "[CF-ACCESS] verifying assertions from {} (mode: {:?})",
            verifier.issuer(),
            verifier.mode()
        ),
        None => info!("[CF-ACCESS] not configured; bearer auth remains the API identity authority"),
    }

    // Shared resurfacing store. It is created before channel distillation so
    // bounded brief repair can prioritize currently surfaced comms; the main
    // resurfacing worker itself is still spawned later, after artifact-v2.
    let resurfacing_store =
        magician::magician_v2::attention::resurfacing::store::ResurfacingStore::open(
            storage_workspace.base_root(),
        )
        .context("opening resurfacing store")?;
    magician::magician_v2::attention::resurfacing::memory_effects::init_memory_effect_runtime(
        storage_workspace
            .base_root()
            .join("memory_effect_mode.json"),
    );
    let historical_bootstrap_worker = Arc::new(
        magician_comms::channel_assist::attention_learning::AttentionHistoricalBootstrapWorker::new(
            attention_learning_service.clone(),
            channel_assist_store.clone(),
            resurfacing_store.clone(),
            magician_config
                .attention_learning
                .historical_bootstrap
                .clone(),
            storage_workspace.base_root().join("scopes"),
        ),
    );
    let _historical_bootstrap_worker_handle = if magician_config
        .attention_learning
        .historical_bootstrap
        .enabled
    {
        // Persist the immutable pre-live boundary before any channel or
        // resurfacing producer starts. Later restarts reuse this exact cutoff.
        let historical_scopes = historical_bootstrap_worker
            .initialize_existing_scopes(chrono::Utc::now().timestamp_millis())
            .await
            .context("initializing historical attention bootstrap")?;
        historical_bootstrap_worker
            .drain_historical_worth_before_live(&historical_scopes)
            .await
            .context("draining capped historical Worth-a-look evidence")?;
        Some(historical_bootstrap_worker.clone().spawn())
    } else {
        None
    };
    let semantic_extractor: Arc<
        dyn magician_comms::channel_assist::attention_learning::SemanticFeatureExtractor,
    > = match orchestrator.operation_llm_router() {
        Some(router) => Arc::new(
            magician_comms::channel_assist::assist::classify::RouterClassifyLlm::new(
                router,
                Some(Arc::clone(&event_broadcaster)),
                "anonymous",
                "default",
            )
            .for_semantic_backfill(),
        ),
        None => Arc::new(
            magician_comms::channel_assist::attention_learning::UnavailableSemanticFeatureExtractor,
        ),
    };
    let semantic_extraction_worker =
        magician_comms::channel_assist::attention_learning::SemanticExtractionWorker::new(
            attention_learning_service.store(),
            channel_assist_store.clone(),
            resurfacing_store.clone(),
            semantic_extractor,
            magician_config.attention_learning.semantic_backfill.clone(),
            format!("magician:{}", std::process::id()),
        );
    let _semantic_extraction_worker_handle = magician_config
        .attention_learning
        .semantic_backfill
        .enabled
        .then(|| {
            semantic_extraction_worker
                .clone()
                .spawn("anonymous", "default")
        });
    let rank_recompute_worker =
        magician_comms::channel_assist::attention_learning::AttentionRankRecomputeWorker::new(
            attention_learning_service.clone(),
            channel_assist_store.clone(),
            resurfacing_store.clone(),
            magician_config.attention_learning.rank_recompute.clone(),
            format!("magician-rank-recompute:{}", std::process::id()),
        );
    let _rank_recompute_worker_handle = magician_config
        .attention_learning
        .rank_recompute
        .enabled
        .then(|| rank_recompute_worker.clone().spawn());
    let actionability_training_worker =
        magician::magician_v2::attention::learning::AttentionActionabilityTrainingWorker::new(
            attention_learning_service.clone(),
            magician_config
                .attention_learning
                .actionability
                .training
                .clone(),
            storage_workspace
                .base_root()
                .join("attention_learning")
                .join("snapshots"),
            storage_workspace.base_root().join("scopes"),
        );
    let _actionability_training_worker_handle = (magician_config
        .attention_learning
        .actionability
        .training
        .enabled
        || magician_config.attention_learning.routing.training.enabled
        || magician_config.attention_learning.bandit.training.enabled)
        .then(|| Arc::new(actionability_training_worker.clone()).spawn());
    let channel_sync_config = magician_comms::channel_assist::sync::ChannelSyncConfig::from_env();
    let observe_catch_up_controller = Arc::new(
        magician::magician_v2::observe_catchup::ObserveCatchUpController::new(
            storage_workspace.clone(),
        ),
    );
    let _channel_sync_worker = channel_sync_config.enabled.then(|| {
        magician_comms::channel_assist::sync::ChannelSyncWorker::spawn(
            storage_workspace.clone(),
            channel_assist_store.clone(),
            channel_sync_config.clone(),
            Some(Arc::clone(&observe_catch_up_controller)),
        )
    });
    // Channel Assist Phase 1b (N3): local-only distillation queue worker.
    // Drains `distill_state = pending` rows at the local model's pace.
    // FAIL-CLOSED: it distills only while `channel_ingest_distill` is
    // explicitly bound to a local (ollama-family) profile in config —
    // otherwise rows land metadata-only (`skipped`) with one loud warn
    // per streak, and no remote path exists in code. Kill-switch
    // CHANNEL_DISTILL_ENABLED=0 skips the spawn entirely.
    let channel_distill_config =
        magician_comms::channel_assist::assist::distill::ChannelDistillConfig::from_settings(
            &magician_config.channel_assist.distillation,
        );
    let _channel_distill_worker = channel_distill_config.enabled.then(|| {
        magician_comms::channel_assist::assist::distill::ChannelDistillWorker::spawn(
            storage_workspace.clone(),
            channel_assist_store.clone(),
            orchestrator.operation_llm_router(),
            Some(Arc::clone(&event_broadcaster)),
            channel_distill_config.clone(),
            Some(resurfacing_store.clone()),
            Some(Arc::clone(&observe_catch_up_controller)),
        )
    });
    // Unified observe+assist (U1): evidence bridge — distilled channel-assist
    // rows feed `user.{email,chat}_evidence`, so the existing tier_distill →
    // work-evidence-graph path picks them up (retiring the email digest in
    // U3). Kill-switch CHANNEL_EVIDENCE_BRIDGE_ENABLED=0.
    let _channel_evidence_bridge_worker =
        magician_comms::channel_assist::evidence_bridge::ChannelEvidenceBridgeWorker::enabled_from_env()
            .then(|| {
                magician_comms::channel_assist::evidence_bridge::ChannelEvidenceBridgeWorker::spawn(
                    channel_assist_store.clone(),
                )
            });
    // Feedback → memory bridge: consumes channel Follow-up triage verdicts
    // (Do it / Acknowledged / Dismiss + reason) into the
    // `user.channel_feedback` memory tier. Kill-switch
    // CHANNEL_FEEDBACK_BRIDGE_ENABLED=0.
    let _channel_feedback_bridge_worker =
        magician_comms::channel_assist::feedback_bridge::ChannelFeedbackBridgeWorker::enabled_from_env()
            .then(|| {
                magician_comms::channel_assist::feedback_bridge::ChannelFeedbackBridgeWorker::spawn(
                    channel_assist_store.clone(),
                )
            });
    // Passive pattern synthesis: PASSIVELY consolidates the channel corpus into
    // recurring topics + timing (the `user.channel_patterns` tier). Idle unless
    // `channel_pattern_synthesis` is bound in operation_mapping. Kill-switch
    // CHANNEL_PATTERN_SYNTHESIS_ENABLED=0.
    let _channel_pattern_synthesis_worker =
        magician_comms::channel_assist::pattern_synthesis::ChannelPatternSynthesisWorker::enabled_from_env()
            .then(|| {
                magician_comms::channel_assist::pattern_synthesis::ChannelPatternSynthesisWorker::spawn(
                    channel_assist_store.clone(),
                    orchestrator.operation_llm_router(),
                    Some(Arc::clone(&event_broadcaster)),
                )
            });
    // Phase 2: body-blind thread classifier. Turns distilled threads into
    // labeled annotations (needs_reply/follow_up/fyi/no_action). Body-blind
    // (summaries only, never raw bodies), so it's an ordinary op dispatch —
    // idle until `channel_classify` is bound in operation_mapping (local OR
    // remote). Kill-switch CHANNEL_CLASSIFY_ENABLED=0.
    let channel_classify_config =
        magician_comms::channel_assist::assist::classify::ChannelClassifyConfig::from_env();
    let _channel_classify_worker = channel_classify_config.enabled.then(|| {
        magician_comms::channel_assist::assist::classify::ChannelClassifyWorker::spawn_with_attention(
            channel_assist_store.clone(),
            orchestrator.operation_llm_router(),
            Some(Arc::clone(&event_broadcaster)),
            Some(attention_funnel_store.clone()),
            channel_classify_config.clone(),
        )
    });
    // Comms Assist reconciliation: provider-neutral cleanup of active
    // follow-up annotations as newer locally-distilled mail/chat evidence
    // arrives. It never mutates providers and uses guarded state transitions
    // so user actions/action claims win races.
    let channel_reconcile_config =
        magician_comms::channel_assist::assist::reconcile::ChannelReconcileConfig::from_env();
    // Reconciliation is also the only place that observes the owner discharging
    // a follow-up outside the product — replying from their own mail client,
    // where there is no button to record. That is the sole positive label the
    // actionability model can learn from, so the retirement pass doubles as its
    // label source.
    let reconcile_attention_labeller: std::sync::Arc<
        dyn magician_comms::channel_assist::assist::completion_port::ReconcileCompletionSink,
    > = std::sync::Arc::new(
        magician_api::channel_assist_api::ReconcileAttentionLabeller::new(
            channel_assist_store.clone(),
            attention_learning_service.clone(),
            channel_reconcile_config.batch,
        ),
    );
    let _channel_reconcile_worker = channel_reconcile_config.enabled.then(|| {
        magician_comms::channel_assist::assist::reconcile::ChannelReconcileWorker::spawn_with_completion_sink(
            channel_assist_store.clone(),
            channel_reconcile_config.clone(),
            Some(reconcile_attention_labeller),
        )
    });
    // Proactive Resurfacing Engine (Phase 1): background scorer + curator. The
    // single-file store is scoped by principal/workspace columns (pass the
    // storage base root, not a per-scope path) and is shared with the
    // resurfacing HTTP handlers registered below. On by default; kill-switch
    // RESURFACING_ENABLED=0 makes `spawn` return a no-op handle.
    // The worker is spawned later — after `shared_artifact_v2_service` is
    // constructed — so it can sweep the task/episode + comms sources and curate
    // via the LLM path.
    // Analytics query API (optional — only available when analytics layer is active).
    // It also owns the manual memory-eval trigger, so wire the same scoped
    // definition store / memory resolver used by the periodic runner.
    let shared_llm_analytics_read_service = Arc::new(
        magician::magician_v2::analytics::llm_analytics_read_service::LlmAnalyticsReadService::new(
            storage_workspace.clone(),
        ),
    );
    let shared_analytics_api = Some(web::Data::new(
        magician_api::analytics_api::AnalyticsApi::new(storage_workspace.clone())
            .with_llm_analytics_read_service(Arc::clone(&shared_llm_analytics_read_service))
            .with_memory_eval_runtime(
                shared_agent_api.definition_store.as_ref().clone(),
                AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
            )
            .with_operation_router(orchestrator.operation_llm_router())
            .with_event_broadcaster(Arc::clone(&event_broadcaster))
            .with_restricted_llm_content(Arc::clone(&restricted_llm_content)),
    ));
    let shared_learning_api = web::Data::new(magician_api::learning_api::LearningApi::new(
        storage_workspace.clone(),
        repo_root.clone(),
    ));
    let supervisor_shutdown = CancellationToken::new();
    let mut deferred_startup_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Ollama's configured handle was published before core initialization;
    // availability remains false until its independently owned warm-up finishes.

    // Pack hot-reload was deleted: every pack ships either as an
    // installed skill (under `<system>/skills/<skill>/tool_schema.yaml`)
    // or as an embedded compiled-pack def in the binary. There's no
    // runtime disk-watched pack registry to refresh — magician restart
    // is the canonical "reload skills" path.
    //
    // Forge `/tools` and `/crew/new` still read the live pack list
    // through `GET /api/magician/v2/capabilities/packs`, so we expose
    // a read-only snapshot of `CapabilityRegistry` here. The matching
    // `POST /capabilities/reload` returns an empty `ReloadReport` —
    // the button is functionally a no-op now (kept for UI continuity).
    //
    // `web::Data::from(arc)` re-uses the existing `Arc` instead of
    // wrapping it again — the handler receives `web::Data<Registry>`
    // (single Arc inside) rather than `web::Data<Arc<Registry>>`
    // (Arc<Arc<Registry>>).
    let shared_capability_registry = web::Data::from(orchestrator.capability_registry().clone());

    // Create shared GAUI Layout API ONCE — not per-worker.
    // MuijStorage contains a write gate (Mutex) that must be shared across workers
    // to serialize concurrent writes to the same agent layout.
    let shared_muij_storage = MuijStorage::new(storage_workspace.scoped_agent_runtime_root(
        magician::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_PRINCIPAL,
        magician::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_WORKSPACE,
    ));

    // Task state
    let task_state_tool_name = "task_state";
    let task_state_pack_def = orchestrator
        .capability_registry()
        .get_pack_definition(task_state_tool_name);
    match task_state_pack_def {
        Some(pack_def) => {
            let provider =
                TaskStateProvider::new(storage_workspace.clone()).with_pack_def(pack_def.clone());
            orchestrator
                .capability_registry()
                .register(Arc::new(provider));
            if let Some(description) = &pack_def.description {
                orchestrator
                    .capability_registry()
                    .set_description(task_state_tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                orchestrator
                    .capability_registry()
                    .set_guide(task_state_tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                orchestrator
                    .capability_registry()
                    .set_param_defs(task_state_tool_name, pack_def.parameters.clone());
            }
            info!(
                "Registered deferred compiled capability '{}'",
                task_state_tool_name
            );
        },
        None => {
            local_tool_services.remove_pack_tools(&[task_state_tool_name.to_string()]);
            warn!(
                "Capability pack for '{}' was not loaded; disabling tool catalog entry",
                task_state_tool_name
            );
        },
    }

    // Internal diagnostics
    let internal_data_tool_name = "internal_data";
    let internal_data_pack_def = orchestrator
        .capability_registry()
        .get_pack_definition(internal_data_tool_name);
    // The Apps witness (plan 2.5 learning review reads) is minted only when
    // the loaded pack actually came from the embedded fallback. Parsed struct
    // equality is not byte provenance: a disk pack with reordered YAML keys
    // must remain an ordinary agent capability and cannot inherit app dispatch.
    let embedded_internal_data = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == internal_data_tool_name)
        .cloned();
    let retained_embedded_internal_data = match (
        internal_data_pack_def.as_ref(),
        embedded_internal_data.as_ref(),
    ) {
        (Some(loaded), Some(embedded)) => {
            !disk_pack_names.contains(internal_data_tool_name) && loaded == embedded
        },
        _ => false,
    };
    match internal_data_pack_def {
        Some(pack_def) => {
            let provider = InternalDataProvider::new(storage_workspace.clone())
                .with_llm_analytics_read_service(Arc::clone(&shared_llm_analytics_read_service))
                .with_pack_def(pack_def.clone());
            if retained_embedded_internal_data {
                orchestrator
                    .capability_registry()
                    .register_builtin_internal_data_provider(
                        Arc::new(provider),
                        embedded_compiled_pack_yaml(internal_data_tool_name)
                            .expect("embedded internal_data pack")
                            .as_bytes(),
                    );
            } else {
                orchestrator
                    .capability_registry()
                    .register_override(Arc::new(provider));
            }
            if let Some(description) = &pack_def.description {
                orchestrator
                    .capability_registry()
                    .set_description(internal_data_tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                orchestrator
                    .capability_registry()
                    .set_guide(internal_data_tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                orchestrator
                    .capability_registry()
                    .set_param_defs(internal_data_tool_name, pack_def.parameters.clone());
            }
            info!(
                "Registered provider-backed inner-loop capability '{}'{}",
                internal_data_tool_name,
                if retained_embedded_internal_data {
                    " with the app learning-read witness"
                } else {
                    ""
                }
            );
        },
        None => {
            local_tool_services.remove_pack_tools(&[internal_data_tool_name.to_string()]);
            warn!(
                "Capability pack for '{}' was not loaded; disabling tool catalog entry",
                internal_data_tool_name
            );
        },
    }

    // Scoped thinking-map host reads (the 2.5 learning-read pattern
    // generalized; the Phase 4 Brainstorm verdict's re-open condition).
    // Same wiring shape as internal_data: the Apps witness is minted only
    // for the actual embedded fallback, never from parsed equality with a disk
    // definition.
    let thinking_maps_data_tool_name = "thinking_maps_data";
    let thinking_maps_data_pack_def = orchestrator
        .capability_registry()
        .get_pack_definition(thinking_maps_data_tool_name);
    let embedded_thinking_maps_data = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == thinking_maps_data_tool_name)
        .cloned();
    let retained_embedded_thinking_maps_data = match (
        thinking_maps_data_pack_def.as_ref(),
        embedded_thinking_maps_data.as_ref(),
    ) {
        (Some(loaded), Some(embedded)) => {
            !disk_pack_names.contains(thinking_maps_data_tool_name) && loaded == embedded
        },
        _ => false,
    };
    match thinking_maps_data_pack_def {
        Some(pack_def) => {
            let provider = ThinkingMapsDataProvider::new(storage_workspace.clone())
                .with_pack_def(pack_def.clone());
            if retained_embedded_thinking_maps_data {
                orchestrator
                    .capability_registry()
                    .register_builtin_thinking_maps_data_provider(
                        Arc::new(provider),
                        embedded_compiled_pack_yaml(thinking_maps_data_tool_name)
                            .expect("embedded thinking_maps_data pack")
                            .as_bytes(),
                    );
            } else {
                orchestrator
                    .capability_registry()
                    .register_override(Arc::new(provider));
            }
            if let Some(description) = &pack_def.description {
                orchestrator
                    .capability_registry()
                    .set_description(thinking_maps_data_tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                orchestrator
                    .capability_registry()
                    .set_guide(thinking_maps_data_tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                orchestrator
                    .capability_registry()
                    .set_param_defs(thinking_maps_data_tool_name, pack_def.parameters.clone());
            }
            info!(
                "Registered provider-backed inner-loop capability '{}'{}",
                thinking_maps_data_tool_name,
                if retained_embedded_thinking_maps_data {
                    " with the app thinking-map read witness"
                } else {
                    ""
                }
            );
        },
        None => {
            local_tool_services.remove_pack_tools(&[thinking_maps_data_tool_name.to_string()]);
            warn!(
                "Capability pack for '{}' was not loaded; disabling tool catalog entry",
                thinking_maps_data_tool_name
            );
        },
    }

    // Bounded read-only claims-review binder. As with internal_data and
    // thinking_maps_data, only the actual embedded fallback mints the built-in
    // Apps witness; parsed equality with a disk override is not provenance.
    let evidence_data_tool_name = "evidence_data";
    let evidence_data_pack_def = orchestrator
        .capability_registry()
        .get_pack_definition(evidence_data_tool_name);
    let embedded_evidence_data = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == evidence_data_tool_name)
        .cloned();
    let retained_embedded_evidence_data = match (
        evidence_data_pack_def.as_ref(),
        embedded_evidence_data.as_ref(),
    ) {
        (Some(loaded), Some(embedded)) => {
            !disk_pack_names.contains(evidence_data_tool_name) && loaded == embedded
        },
        _ => false,
    };
    match evidence_data_pack_def {
        Some(pack_def) => {
            let provider = EvidenceDataProvider::new(storage_workspace.clone())
                .with_pack_def(pack_def.clone());
            if retained_embedded_evidence_data {
                orchestrator
                    .capability_registry()
                    .register_builtin_evidence_data_provider(
                        Arc::new(provider),
                        embedded_compiled_pack_yaml(evidence_data_tool_name)
                            .expect("embedded evidence_data pack")
                            .as_bytes(),
                    );
            } else {
                orchestrator
                    .capability_registry()
                    .register_override(Arc::new(provider));
            }
            if let Some(description) = &pack_def.description {
                orchestrator
                    .capability_registry()
                    .set_description(evidence_data_tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                orchestrator
                    .capability_registry()
                    .set_guide(evidence_data_tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                orchestrator
                    .capability_registry()
                    .set_param_defs(evidence_data_tool_name, pack_def.parameters.clone());
            }
            info!(
                "Registered provider-backed inner-loop capability '{}'{}",
                evidence_data_tool_name,
                if retained_embedded_evidence_data {
                    " with the app evidence-data read witness"
                } else {
                    ""
                }
            );
        },
        None => {
            local_tool_services.remove_pack_tools(&[evidence_data_tool_name.to_owned()]);
            warn!(
                "Capability pack for '{}' was not loaded; disabling tool catalog entry",
                evidence_data_tool_name
            );
        },
    }

    // Bounded read-only meetings binder. Same provenance rule as the three
    // binders above: only the actual embedded fallback mints the built-in Apps
    // witness; parsed equality with a disk override is not provenance. The
    // provider reads the process chat store published at boot, the scope's
    // memory tier, the shared calendar cache and the process-global capture
    // registries — and holds no mutation path at all.
    let meetings_data_tool_name = "meetings_data";
    let meetings_data_pack_def = orchestrator
        .capability_registry()
        .get_pack_definition(meetings_data_tool_name);
    let embedded_meetings_data = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == meetings_data_tool_name)
        .cloned();
    let retained_embedded_meetings_data = match (
        meetings_data_pack_def.as_ref(),
        embedded_meetings_data.as_ref(),
    ) {
        (Some(loaded), Some(embedded)) => {
            !disk_pack_names.contains(meetings_data_tool_name) && loaded == embedded
        },
        _ => false,
    };
    match meetings_data_pack_def {
        Some(pack_def) => {
            let provider = MeetingsDataProvider::new(storage_workspace.clone())
                .with_pack_def(pack_def.clone());
            if retained_embedded_meetings_data {
                orchestrator
                    .capability_registry()
                    .register_builtin_meetings_data_provider(
                        Arc::new(provider),
                        embedded_compiled_pack_yaml(meetings_data_tool_name)
                            .expect("embedded meetings_data pack")
                            .as_bytes(),
                    );
            } else {
                orchestrator
                    .capability_registry()
                    .register_override(Arc::new(provider));
            }
            if let Some(description) = &pack_def.description {
                orchestrator
                    .capability_registry()
                    .set_description(meetings_data_tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                orchestrator
                    .capability_registry()
                    .set_guide(meetings_data_tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                orchestrator
                    .capability_registry()
                    .set_param_defs(meetings_data_tool_name, pack_def.parameters.clone());
            }
            info!(
                "Registered provider-backed inner-loop capability '{}'{}",
                meetings_data_tool_name,
                if retained_embedded_meetings_data {
                    " with the app meetings-data read witness"
                } else {
                    ""
                }
            );
        },
        None => {
            local_tool_services.remove_pack_tools(&[meetings_data_tool_name.to_owned()]);
            warn!(
                "Capability pack for '{}' was not loaded; disabling tool catalog entry",
                meetings_data_tool_name
            );
        },
    }

    // Bounded read-only agent-roster binder. Same provenance rule as the four
    // binders above: only the actual embedded fallback mints the built-in Apps
    // witness. It projects the roster's participation face and holds no path
    // that edits an agent definition. The face's `busy` bit needs the artifact
    // service, which does not exist yet at this point in boot; this
    // process-wide copy reports it as `null`, and the per-scope registries the
    // app path reads (`ScopedCapabilityResolver::registry_for_scope`) attach
    // the service and answer it for real.
    let agent_roster_data_tool_name = "agent_roster_data";
    let agent_roster_data_pack_def = orchestrator
        .capability_registry()
        .get_pack_definition(agent_roster_data_tool_name);
    let embedded_agent_roster_data = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == agent_roster_data_tool_name)
        .cloned();
    let retained_embedded_agent_roster_data = match (
        agent_roster_data_pack_def.as_ref(),
        embedded_agent_roster_data.as_ref(),
    ) {
        (Some(loaded), Some(embedded)) => {
            !disk_pack_names.contains(agent_roster_data_tool_name) && loaded == embedded
        },
        _ => false,
    };
    match agent_roster_data_pack_def {
        Some(pack_def) => {
            let provider = AgentRosterDataProvider::new(storage_workspace.clone())
                .with_definition_store(Arc::clone(&shared_agent_api.definition_store))
                .with_pack_def(pack_def.clone());
            if retained_embedded_agent_roster_data {
                orchestrator
                    .capability_registry()
                    .register_builtin_agent_roster_data_provider(
                        Arc::new(provider),
                        embedded_compiled_pack_yaml(agent_roster_data_tool_name)
                            .expect("embedded agent_roster_data pack")
                            .as_bytes(),
                    );
            } else {
                orchestrator
                    .capability_registry()
                    .register_override(Arc::new(provider));
            }
            if let Some(description) = &pack_def.description {
                orchestrator
                    .capability_registry()
                    .set_description(agent_roster_data_tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                orchestrator
                    .capability_registry()
                    .set_guide(agent_roster_data_tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                orchestrator
                    .capability_registry()
                    .set_param_defs(agent_roster_data_tool_name, pack_def.parameters.clone());
            }
            info!(
                "Registered provider-backed inner-loop capability '{}'{}",
                agent_roster_data_tool_name,
                if retained_embedded_agent_roster_data {
                    " with the app agent-roster read witness"
                } else {
                    ""
                }
            );
        },
        None => {
            local_tool_services.remove_pack_tools(&[agent_roster_data_tool_name.to_owned()]);
            warn!(
                "Capability pack for '{}' was not loaded; disabling tool catalog entry",
                agent_roster_data_tool_name
            );
        },
    }

    // Bounded read-only task-list binder. Same provenance rule as the host-read
    // binders above. The artifact service does not exist yet at this point in
    // boot, so this process-wide copy fails every call with an explicit error;
    // the per-scope registries the app path reads attach the service.
    let tasks_data_tool_name = "tasks_data";
    let tasks_data_pack_def = orchestrator
        .capability_registry()
        .get_pack_definition(tasks_data_tool_name);
    let embedded_tasks_data = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == tasks_data_tool_name)
        .cloned();
    let retained_embedded_tasks_data =
        match (tasks_data_pack_def.as_ref(), embedded_tasks_data.as_ref()) {
            (Some(loaded), Some(embedded)) => {
                !disk_pack_names.contains(tasks_data_tool_name) && loaded == embedded
            },
            _ => false,
        };
    match tasks_data_pack_def {
        Some(pack_def) => {
            let provider = TasksDataProvider::new().with_pack_def(pack_def.clone());
            if retained_embedded_tasks_data {
                orchestrator
                    .capability_registry()
                    .register_builtin_tasks_data_provider(
                        Arc::new(provider),
                        embedded_compiled_pack_yaml(tasks_data_tool_name)
                            .expect("embedded tasks_data pack")
                            .as_bytes(),
                    );
            } else {
                orchestrator
                    .capability_registry()
                    .register_override(Arc::new(provider));
            }
            if let Some(description) = &pack_def.description {
                orchestrator
                    .capability_registry()
                    .set_description(tasks_data_tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                orchestrator
                    .capability_registry()
                    .set_guide(tasks_data_tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                orchestrator
                    .capability_registry()
                    .set_param_defs(tasks_data_tool_name, pack_def.parameters.clone());
            }
            info!(
                "Registered provider-backed inner-loop capability '{}'{}",
                tasks_data_tool_name,
                if retained_embedded_tasks_data {
                    " with the app task-list read witness"
                } else {
                    ""
                }
            );
        },
        None => {
            local_tool_services.remove_pack_tools(&[tasks_data_tool_name.to_owned()]);
            warn!(
                "Capability pack for '{}' was not loaded; disabling tool catalog entry",
                tasks_data_tool_name
            );
        },
    }

    // Bounded read-only notes binder: search, and the exact read of a searched
    // note. Same provenance rule as the host-read binders above.
    let notes_data_tool_name = "notes_data";
    let notes_data_pack_def = orchestrator
        .capability_registry()
        .get_pack_definition(notes_data_tool_name);
    let embedded_notes_data = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == notes_data_tool_name)
        .cloned();
    let retained_embedded_notes_data =
        match (notes_data_pack_def.as_ref(), embedded_notes_data.as_ref()) {
            (Some(loaded), Some(embedded)) => {
                !disk_pack_names.contains(notes_data_tool_name) && loaded == embedded
            },
            _ => false,
        };
    match notes_data_pack_def {
        Some(pack_def) => {
            let provider =
                NotesDataProvider::new(storage_workspace.clone()).with_pack_def(pack_def.clone());
            if retained_embedded_notes_data {
                orchestrator
                    .capability_registry()
                    .register_builtin_notes_data_provider(
                        Arc::new(provider),
                        embedded_compiled_pack_yaml(notes_data_tool_name)
                            .expect("embedded notes_data pack")
                            .as_bytes(),
                    );
            } else {
                orchestrator
                    .capability_registry()
                    .register_override(Arc::new(provider));
            }
            if let Some(description) = &pack_def.description {
                orchestrator
                    .capability_registry()
                    .set_description(notes_data_tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                orchestrator
                    .capability_registry()
                    .set_guide(notes_data_tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                orchestrator
                    .capability_registry()
                    .set_param_defs(notes_data_tool_name, pack_def.parameters.clone());
            }
            info!(
                "Registered provider-backed inner-loop capability '{}'{}",
                notes_data_tool_name,
                if retained_embedded_notes_data {
                    " with the app notes read witness"
                } else {
                    ""
                }
            );
        },
        None => {
            local_tool_services.remove_pack_tools(&[notes_data_tool_name.to_owned()]);
            warn!(
                "Capability pack for '{}' was not loaded; disabling tool catalog entry",
                notes_data_tool_name
            );
        },
    }

    // Owner-granted app memory reads (`app_memory_read_v1`). Same provenance
    // rule as the host-read binders above.
    let memory_data_tool_name = "memory_data";
    let memory_data_pack_def = orchestrator
        .capability_registry()
        .get_pack_definition(memory_data_tool_name);
    let embedded_memory_data = embedded_compiled_pack_defs_ref()
        .iter()
        .find(|candidate| candidate.name == memory_data_tool_name)
        .cloned();
    let retained_embedded_memory_data =
        match (memory_data_pack_def.as_ref(), embedded_memory_data.as_ref()) {
            (Some(loaded), Some(embedded)) => {
                !disk_pack_names.contains(memory_data_tool_name) && loaded == embedded
            },
            _ => false,
        };
    match memory_data_pack_def {
        Some(pack_def) => {
            let provider = MemoryDataProvider::new(storage_workspace.clone())
                .with_definition_store(Arc::clone(&shared_agent_api.definition_store))
                .with_pack_def(pack_def.clone());
            if retained_embedded_memory_data {
                orchestrator
                    .capability_registry()
                    .register_builtin_memory_data_provider(
                        Arc::new(provider),
                        embedded_compiled_pack_yaml(memory_data_tool_name)
                            .expect("embedded memory_data pack")
                            .as_bytes(),
                    );
            } else {
                orchestrator
                    .capability_registry()
                    .register_override(Arc::new(provider));
            }
            if let Some(description) = &pack_def.description {
                orchestrator
                    .capability_registry()
                    .set_description(memory_data_tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                orchestrator
                    .capability_registry()
                    .set_guide(memory_data_tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                orchestrator
                    .capability_registry()
                    .set_param_defs(memory_data_tool_name, pack_def.parameters.clone());
            }
            info!(
                "Registered provider-backed inner-loop capability '{}'{}",
                memory_data_tool_name,
                if retained_embedded_memory_data {
                    " with the app memory read witness"
                } else {
                    ""
                }
            );
        },
        None => {
            local_tool_services.remove_pack_tools(&[memory_data_tool_name.to_owned()]);
            warn!(
                "Capability pack for '{}' was not loaded; disabling tool catalog entry",
                memory_data_tool_name
            );
        },
    }

    // Every deferred provider this boot binds in place has bound by now. What
    // is still unbound cannot be served by this process, and the catalog the
    // control-plane scope projects from these definitions — the one a harness
    // engine reads — must not offer it. Scope snapshots withhold on their own
    // after their late binding; this covers the shared base registry.
    {
        let withheld = orchestrator
            .capability_registry()
            .withhold_unbound_compiled_packs();
        if !withheld.is_empty() {
            info!(
                "[CAPABILITY] Withholding {} compiled pack(s) with no provider from the base catalog: {:?}",
                withheld.len(),
                withheld
            );
            local_tool_services.remove_pack_tools(&withheld);
        }
    }

    // Spawn GAUI delta emitter (background task) — translates agentic execution
    // events into agent.ui.delta envelopes for WebSocket consumers.
    let (_gaui_emitter_handle, shared_muij_doc_cache) =
        MuijDeltaEmitter::spawn_scoped(Arc::clone(&event_broadcaster), storage_workspace.clone());

    // Spawn the agent update journal — subscribes to the shared broadcaster
    // and persists every `agent.update` envelope to per-workspace JSONL logs
    // under `<storage_root>/scopes/<principal>/<workspace>/updates.jsonl`.
    // Handle is intentionally leaked: the task lives for the process lifetime.
    let agent_update_journal_config = magician::magician_v2::agents::agent_update_journal::AgentUpdateJournalConfig::with_workspace_layout(storage_workspace.clone());
    let _agent_update_journal_handle =
        magician::magician_v2::agents::agent_update_journal::spawn_journal(
            agent_update_journal_config.clone(),
            Arc::clone(&event_broadcaster),
        );
    let shared_agent_update_journal_config = web::Data::new(agent_update_journal_config);

    // Spawn the feed-stall watchdog — emits `FeedStalled` updates for cycles
    // that started but haven't produced a terminal event within the quiet
    // window. Handle intentionally leaked: task lives for the process.
    let _feed_stall_watchdog_handle =
        magician::magician_v2::agents::feed_stall_watchdog::spawn_watchdog(
            magician::magician_v2::agents::feed_stall_watchdog::FeedStallWatchdogConfig::default(),
            Arc::clone(&event_broadcaster),
        );

    // LLM Calls Overview demo dashboard — boot-time seed runs below, after
    // `shared_artifact_v2_service` is constructed and reconciled.
    let shared_gaui_api = {
        let api = GauiApi::new(
            shared_muij_storage.clone(),
            Arc::clone(&shared_agent_api.definition_store),
        )
        .with_doc_cache(shared_muij_doc_cache.clone());
        web::Data::new(api)
    };

    let shared_muij_storage = web::Data::new(shared_muij_storage);
    let shared_muij_doc_cache = web::Data::new(shared_muij_doc_cache);

    let shared_artifact_api = web::Data::new(ArtifactApi::new(storage_workspace.clone()));

    // Create shared API Mining API — exposes capability registry and captured
    // auth status to the frontend API Explorer.
    let shared_api_mining_api = web::Data::new(
        ApiMiningApi::with_workspace_layout(
            storage_workspace.clone(),
            Arc::clone(orchestrator.secret_store_resolver()),
            magician_config.api_mining.clone(),
        )
        .with_switch(orchestrator.api_mining_switch().clone())
        .with_magicutor_runtime(
            magician_config.execution.magicutor_base_url.clone(),
            Arc::clone(orchestrator.magicutor_client()),
        ),
    );

    let shared_secret_vault_api = web::Data::new(SecretVaultApi::with_workspace_layout(
        storage_workspace.clone(),
        Arc::clone(orchestrator.secret_store_resolver()),
    ));
    let shared_skills_api = web::Data::new(SkillsApi::with_workspace_layout(
        storage_workspace.clone(),
        repo_root.clone(),
    ));
    let shared_runtime_env_api = web::Data::new(RuntimeEnvApi::new(repo_root.clone()));
    let shared_component_setup_api = web::Data::new(ComponentSetupApi::new(repo_root.clone()));
    let shared_notes_api = web::Data::new(Arc::new(NotesApi::with_workspace_layout(
        storage_workspace.clone(),
    )));
    let shared_workspace_storage_api =
        web::Data::new(Arc::new(WorkspaceStorageApi::new(storage_root.clone())));
    // Tutor drawing primitives: served from the runtime root's global
    // `tutor_primitives/` overlaid with each scope's custom files.
    let shared_tutor_api = web::Data::new(Arc::new(TutorApi::new(storage_root.clone())));
    // Live Thinking Map REST surface. Routes are unconditionally mounted (GA).
    let shared_thinking_maps_api = web::Data::new(std::sync::Arc::new(
        magician_api::thinking_maps_api::ThinkingMapsApi::new(storage_workspace.clone()),
    ));

    // Ambient Thinking Map session coordinator — subscribes read-only to the
    // transport bus and auto-maps finalized USER utterances from *registered*
    // chat/voice sessions onto their bound Live Thinking Map (so a map "builds
    // itself as you speak"). Created here at the outer scope so the same `Arc`
    // reaches BOTH the attach/detach API handlers (as app_data below) and the
    // bus-subscription task spawned inside the async setup block (see
    // `thinking_map_coordinator.spawn()` near the ChatStoreSink spawn). The
    // spawn subscribes before other emitters run, so no early utterance is lost.
    let thinking_map_coordinator =
        magician_surfaces::thinking_map::ThinkingMapSessionCoordinator::new(
            Arc::clone(&event_broadcaster),
            storage_workspace.clone(),
        );
    let shared_thinking_map_coordinator = web::Data::new(Arc::clone(&thinking_map_coordinator));

    // Realtime media + control rails (Phase 0): process-local registry
    // of connected client surfaces (web mobile/desktop, tray, extension)
    // with capability + permission advertisement and per-channel
    // lifecycle events. Shares the transport broadcaster so every
    // media.* event flows through the same bus as the rest of the
    // runtime telemetry. See `magician_v2::media_rails`.
    let realtime_session_registry = Arc::new(
        magician_media::media_rails::RealtimeSessionRegistry::new(Arc::clone(&event_broadcaster)),
    );

    // Phase 3+ provider registry — TTS, STT, realtime voice.
    //
    // Adapters speak the OpenAI HTTP shape but the base URL is
    // overridable per-channel, so any OpenAI-compatible server works:
    //   * LocalAI       (`http://localhost:8080/v1/...`)
    //   * Kokoro-FastAPI (`http://localhost:8880/v1/audio/speech`) — TTS
    //   * faster-whisper-server (`http://localhost:8000/v1/audio/transcriptions`)
    //   * Ollama        (chat/STT only; no realtime API today)
    //
    // Per-channel API key envs (`MAGICIAN_TTS_API_KEY`, etc.) fall
    // back to `OPENAI_API_KEY`. Local servers that don't validate
    // tokens accept any non-empty string — we substitute `local`
    // when no key is set anywhere so adapters still construct.
    let openai_key = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty());

    let tts_base_url = std::env::var("MAGICIAN_TTS_BASE_URL").ok();
    let stt_base_url = std::env::var("MAGICIAN_STT_BASE_URL").ok();

    let tts_key = std::env::var("MAGICIAN_TTS_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| openai_key.clone())
        .or_else(|| tts_base_url.as_ref().map(|_| "local".to_string()));
    let stt_key = std::env::var("MAGICIAN_STT_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| openai_key.clone())
        .or_else(|| stt_base_url.as_ref().map(|_| "local".to_string()));
    // Realtime voice api key + base URL are no longer read from env —
    // the OpenAI realtime factory reads `OPENAI_API_KEY` directly,
    // and the base URL ships from the `magician-config.yaml >
    // realtime_voice.profiles.*.base_url` field.

    let mut media_providers = magician_media::media_rails::MediaProviderRegistry::new();

    // Shared cache capacity for every registered TTS provider. Read
    // once so primary + fallback decoration use the same value (and
    // the env vars don't have to drift between them). `0` disables
    // caching entirely for both providers.
    let tts_cache_capacity = std::env::var("MAGICIAN_TTS_CACHE_CAPACITY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(200);

    let host_gateway_url = std::env::var("MAGICIAN_HOST_GATEWAY_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());

    // Independent optional host probes overlap. Registration still consumes
    // their confirmed results before feature routes become available.
    let (host_speech_availability, host_gateway_up, cua_up) = tokio::join!(
        async {
            if let Some(url) = host_gateway_url.as_deref() {
                probe_host_speech_availability(url).await
            } else {
                HostSpeechAvailability::default()
            }
        },
        async {
            magician_media::media_rails::providers::HostAutomationProvider::new(
                magician_media::media_rails::providers::host_gateway_url_from_env(),
            )
            .gateway_available()
            .await
        },
        async {
            let local_cua = if let Some(binary) = runtime_core::cua::driver_binary() {
                if runtime_core::cua::has_desktop_session() {
                    true
                } else {
                    let mut command = tokio::process::Command::new(binary);
                    command.arg("status").kill_on_drop(true);
                    matches!(tokio::time::timeout(Duration::from_secs(2), command.output()).await,
                        Ok(Ok(output)) if output.status.success())
                }
            } else {
                false
            };
            local_cua
                || magician_media::media_rails::providers::HostAutomationProvider::new(
                    magician_media::media_rails::providers::host_gateway_url_from_env(),
                )
                .cua_available()
                .await
        },
    );
    magician::magician_v2::config_extras::set_host_gateway_available(host_gateway_up);
    magician::magician_v2::config_extras::set_cua_available(cua_up);
    info!(
        available = cua_up,
        "CUA provider availability (independent of Mac automation)"
    );
    if host_gateway_up {
        info!("macOS host-automation gateway reachable; host-gateway skills enabled");
    } else {
        info!(
            "macOS host-automation gateway unreachable; host-gateway skills (screen/mac automation) \
             suppressed from agent catalogs until the desktop app is running"
        );
    }

    if let Some(host_gateway_url) = host_gateway_url
        .as_deref()
        .filter(|_| host_speech_availability.tts)
    {
        let macos_tts = magician_media::media_rails::CachedTtsProvider::wrap(
            Arc::new(magician_media::media_rails::MacOsTtsProvider::new(
                host_gateway_url,
            )),
            tts_cache_capacity,
        );
        media_providers = media_providers.with_tts(macos_tts);
    }

    let minimax_tts_key = std::env::var("MINIMAX_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty());
    let minimax_group_id = std::env::var("MAGICIAN_MINIMAX_GROUP_ID")
        .ok()
        .filter(|g| !g.trim().is_empty());

    let fluid_audio_manager =
        magician_media::media_rails::fluid_audio::FluidAudioEngineManager::from_media_settings(
            &magician_config.media,
            magician::config::magician_config_path(),
        )
        .map_err(anyhow::Error::msg)?
        .map(Arc::new);
    if let Some(manager) = fluid_audio_manager.as_ref() {
        manager
            .install_broadcaster(Arc::clone(&event_broadcaster))
            .map_err(anyhow::Error::msg)?;
    }
    // Register configured FluidAudio adapters even while the engine is disabled.
    // Runtime resolution and the manager gate keep them unavailable until the
    // user enables the engine again through audio settings.
    let fluid_audio_host_available = match fluid_audio_manager.as_ref() {
        Some(manager) => manager.is_host_supportable().await,
        None => false,
    };
    let fluid_audio_registerable = fluid_audio_manager
        .as_ref()
        .is_some_and(|manager| manager.can_register_providers());
    if fluid_audio_registerable {
        if let Some(manager) = fluid_audio_manager
            .as_ref()
            .filter(|manager| manager.startup_prewarm_enabled())
        {
            let manager = Arc::clone(manager);
            tokio::spawn(async move {
                if !magician::magician_v2::runtime::startup::wait_for_http().await {
                    return;
                }
                match manager.prewarm_configured_models().await {
                    Ok(()) => info!("FluidAudio startup prewarm completed"),
                    Err(error) => warn!(%error, "FluidAudio startup prewarm degraded"),
                }
            });
        }
    }

    let configured_tts_providers = build_tts_providers(
        &magician_config.media.tts,
        tts_key,
        tts_base_url,
        minimax_tts_key,
        minimax_group_id,
        tts_cache_capacity,
        fluid_audio_manager.as_ref(),
        fluid_audio_registerable,
    );
    for provider in configured_tts_providers {
        if media_providers.has_tts() {
            media_providers = media_providers.with_tts_fallback(provider);
        } else {
            media_providers = media_providers.with_tts(provider);
        }
    }

    let recording_stt_providers = build_recording_stt_providers(
        &magician_config.media.recording_stt,
        stt_key,
        stt_base_url,
        fluid_audio_registerable
            .then_some(())
            .and(fluid_audio_manager.as_ref()),
    );

    let macos_speech_stt_provider: Option<Arc<dyn magician_media::media_rails::SttProvider>> =
        host_gateway_url
            .clone()
            .filter(|_| host_speech_availability.stt)
            .map(|host_gateway_url| {
                Arc::new(magician_media::media_rails::MacOsSpeechProvider::new(
                    host_gateway_url,
                )) as Arc<dyn magician_media::media_rails::SttProvider>
            });

    if let Some(macos_speech_stt) = macos_speech_stt_provider {
        // Local macOS Speech is the default for recorded STT when the
        // host speech verifier reports STT available; cloud STT stays
        // as the fallback and explicit provider requests can still
        // select either one.
        media_providers = media_providers.with_stt(macos_speech_stt);
        for provider in &recording_stt_providers {
            media_providers = media_providers.with_stt_fallback(Arc::clone(provider));
        }
    } else if let Some((primary, rest)) = recording_stt_providers.split_first() {
        media_providers = media_providers.with_stt(Arc::clone(primary));
        for provider in rest {
            media_providers = media_providers.with_stt_fallback(Arc::clone(provider));
        }
    }

    // Register the current streaming adapters for capability resolution and
    // compatibility-profile introspection. Meeting/Listening execution keeps
    // using its existing builder until later migration phases.
    for provider in build_streaming_stt_catalog_providers(
        &magician_config.media.streaming_stt,
        openai_key.clone(),
        env_nonempty("GEMINI_API_KEY"),
        fluid_audio_registerable
            .then_some(())
            .and(fluid_audio_manager.as_ref()),
    ) {
        if media_providers.has_streaming_stt() {
            media_providers = media_providers.with_streaming_stt_fallback(provider);
        } else {
            media_providers = media_providers.with_streaming_stt(provider);
        }
    }

    // Register manager-backed providers on supported hosts independently of
    // current process availability. The runtime catalog keeps them unavailable
    // until the binary or configured external service passes host validation.
    if let Some(manager) = fluid_audio_manager.as_ref() {
        if fluid_audio_registerable {
            for binding in magician_config
                .media
                .vad
                .providers
                .iter()
                .filter(|binding| {
                    binding.enabled
                        && binding.engine_id == "fluid_audio"
                        && binding.adapter == "fluid_audio_vad"
                })
            {
                let provider = Arc::new(
                    magician_media::media_rails::fluid_audio::FluidAudioVadProvider::new(
                        binding.id.clone(),
                        binding.label.clone(),
                        binding.model.clone(),
                        Arc::clone(manager),
                    ),
                );
                if media_providers.has_vad() {
                    media_providers = media_providers.with_vad_fallback(provider);
                } else {
                    media_providers = media_providers.with_vad(provider);
                }
            }
            for binding in magician_config
                .media
                .diarization
                .providers
                .iter()
                .filter(|binding| {
                    binding.enabled
                        && binding.engine_id == "fluid_audio"
                        && binding.adapter
                            == magician_media::media_rails::fluid_audio::FLUID_AUDIO_DIARIZATION_ADAPTER
                })
            {
                let provider = Arc::new(
                    magician_media::media_rails::fluid_audio::FluidAudioDiarizationProvider::new(
                        binding.id.clone(),
                        binding.label.clone(),
                        binding.model.clone(),
                        Arc::clone(manager),
                    ),
                );
                if media_providers.diarization_chain().is_empty() {
                    media_providers = media_providers.with_diarization(provider);
                } else {
                    media_providers = media_providers.with_diarization_fallback(provider);
                }
            }
            info!(
                "[STARTUP] FluidAudio recording STT, VAD, streaming STT, and diarization catalogs registered"
            );
        } else {
            info!("[STARTUP] FluidAudio sidecar unavailable; existing media providers unchanged");
        }
    }

    // Realtime voice providers are resolved per-call through
    // `OperationLlmRouter::resolve_realtime_provider` against the
    // `magician-config.yaml > realtime_voice` profile section — that
    // stays the source of truth. We ALSO cache the resolved provider
    // on the registry at boot so the `/media/providers` introspection
    // endpoint can advertise availability to the frontend without
    // re-resolving on every request. The frontend's `VoiceCallButton`
    // gate reads `providers.realtime_voice !== null` from this snapshot.
    if let Some(router) = orchestrator.operation_llm_router() {
        use magician::magician_v2::query_analysis::operation_llm_router::LLMOperation;
        let default_profile = router.realtime_voice_default_profile_name();
        let profile_catalog = router
            .realtime_voice_profiles()
            .into_iter()
            .filter(|(_, profile)| profile.selectable && profile.display_name.is_some())
            .map(|(profile_id, profile)| {
                let availability = router.resolve_realtime_provider_profile(&profile_id);
                let (available, unavailable_reason, topology) = match availability {
                    Some(Ok(provider)) => (true, None, provider.audio_topology()),
                    Some(Err(error)) => (
                        false,
                        Some(error.to_string()),
                        magicllm::realtime::RealtimeAudioTopology::BackendProxied,
                    ),
                    None => (
                        false,
                        Some("profile is not configured".to_string()),
                        magicllm::realtime::RealtimeAudioTopology::BackendProxied,
                    ),
                };
                magician_media::media_rails::RealtimeVoiceProfileInfo {
                    profile_id,
                    label: profile
                        .display_name
                        .unwrap_or_else(|| profile.model.clone()),
                    provider: profile.provider.clone(),
                    model: profile.model,
                    topology,
                    mode: profile.mode,
                    turn_detection_mode: profile.turn_detection_mode,
                    transcription_model: profile.transcription_model,
                    transcription_fallback_model: profile.transcription_fallback_model,
                    available,
                    unavailable_reason,
                    translation_target_language: profile.translation_target_language,
                    voice: profile.voice.clone(),
                    voices: magicllm::realtime::voices_for_realtime_provider(&profile.provider)
                        .iter()
                        .map(
                            |choice| magician_media::media_rails::RealtimeVoiceChoiceInfo {
                                id: choice.id.to_string(),
                                label: choice.label.to_string(),
                            },
                        )
                        .collect(),
                }
            })
            .collect();
        media_providers =
            media_providers.with_realtime_voice_profiles(default_profile, profile_catalog);
        match router.resolve_realtime_provider(&LLMOperation::VoiceController) {
            Some(Ok(realtime_provider)) => {
                info!(
                    "[STARTUP] Realtime voice provider registered: {} ({})",
                    realtime_provider.id(),
                    realtime_provider.default_model()
                );
                media_providers = media_providers.with_realtime_voice(realtime_provider);
            },
            Some(Err(err)) => {
                warn!(
                    "[STARTUP] Realtime voice profile configured but provider failed to build: {err}"
                );
            },
            None => {
                info!(
                    "[STARTUP] No realtime voice profile configured — voice-call affordance will be hidden in UI"
                );
            },
        }
    }
    let shared_media_providers = Arc::new(media_providers);
    let shared_audio_runtime = Arc::new(
        magician_media::media_rails::AudioRuntimeConfigManager::new(
            magician_config.media.clone(),
            Arc::clone(&shared_media_providers),
            magician::config::magician_config_path(),
        )
        .map_err(|error| anyhow::anyhow!(error))
        .context("building audio runtime configuration")?,
    );
    if fluid_audio_manager.is_some() {
        shared_audio_runtime
            .set_engine_runtime_availability(
                "fluid_audio",
                fluid_audio_host_available,
                (!fluid_audio_host_available).then(|| {
                    "FluidAudio sidecar binary or configured external service is unavailable"
                        .to_string()
                }),
            )
            .map_err(|error| anyhow::anyhow!(error))
            .context("applying FluidAudio host availability")?;
    }
    // Process-local map of voice-session id → control-WS downstream
    // sender. Shared between `MediaApi` (the orchestrator registers a
    // sender here when a call starts) and `ArtifactV2Service` (which
    // pushes `task.completed` to the corresponding live voice call
    // on terminal task transitions).
    let voice_downstream_fanout = magician_media::media_rails::VoiceDownstreamFanout::new();
    let shared_media_preferences = Arc::new(
        magician_media::media_rails::MediaPreferencesStore::with_workspace_layout(
            storage_workspace.clone(),
        ),
    );
    magician_media::media_rails::install_audio_pipeline_services(
        magician_media::media_rails::AudioPipelineServices::new(
            Arc::clone(&shared_audio_runtime),
            Arc::clone(&shared_media_providers),
            Arc::clone(&shared_media_preferences),
            Arc::clone(&event_broadcaster),
        ),
    )
    .map_err(anyhow::Error::msg)
    .context("installing audio pipeline services")?;
    let shared_ui_preferences = Arc::new(
        magician::magician_v2::ui_preferences::UiPreferencesStore::with_workspace_layout(
            storage_workspace.clone(),
        ),
    );
    let shared_media_api = web::Data::new(Arc::new(
        MediaApi::new(Arc::clone(&realtime_session_registry))
            .with_providers(Arc::clone(&shared_media_providers))
            .with_preferences(Arc::clone(&shared_media_preferences))
            .with_audio_runtime(Arc::clone(&shared_audio_runtime))
            .with_fluid_audio(fluid_audio_manager.clone())
            .with_downstream_fanout(Arc::clone(&voice_downstream_fanout)),
    ));
    let shared_ui_preferences_api = web::Data::new(Arc::new(
        UiPreferencesApi::new(Arc::clone(&realtime_session_registry))
            .with_preferences(Arc::clone(&shared_ui_preferences)),
    ));

    let shutdown_secret_store = Arc::clone(orchestrator.secret_store_resolver());
    let shared_artifact_v2_service = Arc::new(
        ArtifactV2Service::with_workspace(
            storage_workspace.clone(),
            Arc::clone(&orchestrator),
            shared_muij_storage.get_ref().clone(),
        )
        .with_event_broadcaster(Arc::clone(&event_broadcaster))
        .with_voice_downstream_fanout(Arc::clone(&voice_downstream_fanout)),
    );
    shared_artifact_v2_service.register_runtime_canonical_event_observer(
        shared_app_platform_api.canonical_event_behavior_observer(),
    )?;
    // The lifecycle tasks are constructed beside their Artifact service, but
    // their immediate interval tick must not run until every execution
    // authority/configuration seam below has been installed. A CancellationToken
    // is used as a persistent one-shot latch: unlike Notify, opening it before a
    // spawned task first polls cannot lose the wakeup.
    let stateless_loop_lifecycle_ready = CancellationToken::new();
    // A terminal loop segment can commit its journal and lose the process
    // before the boundary projector advances its durable outbox cursor. This
    // lifecycle owner needs no execution composition: it scans committed
    // terminal/event/receipt debt, re-verifies each under lease, and projects
    // the recorded routing envelope. Each page stays bounded, but a non-empty
    // cursor immediately yields into the next page so historical executions do
    // not multiply recovery latency by the steady-state cadence. Only a full
    // sweep sleeps; failures retain the cursor and back off before retrying.
    use magician::magician_v2::execution::agentic::run_loop::terminal_outbox::TerminalProjectionJobRegistry;
    let terminal_projection_jobs = TerminalProjectionJobRegistry::new();
    let terminal_outbox_projector_task = {
        let store =
            magician::magician_v2::execution::agentic::run_loop::store::fs::FsLoopStateStore::new(
                shared_artifact_v2_service.workspace().base_root(),
            );
        let artifact_service = Arc::clone(&shared_artifact_v2_service);
        let broadcaster = Arc::clone(&event_broadcaster);
        let shutdown = supervisor_shutdown.clone();
        let ready = stateless_loop_lifecycle_ready.clone();
        let projection_jobs = terminal_projection_jobs.clone();
        tokio::spawn(async move {
            use magician::magician_v2::execution::agentic::run_loop::{
                state::WorkerId, terminal_outbox::project_terminal_outbox_debt_registered,
            };
            use std::num::NonZeroUsize;

            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ready.cancelled() => {},
            }

            let worker = WorkerId::new(format!(
                "terminal-outbox-projector-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple(),
            ));
            let max_visits = NonZeroUsize::new(128).expect("terminal outbox page is non-zero");
            let mut resume = None;
            let projector_cadence = Duration::from_secs(30);
            let mut scan_immediately = true;
            loop {
                if scan_immediately {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = tokio::task::yield_now() => {},
                    }
                } else {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = tokio::time::sleep(projector_cadence) => {},
                    }
                }

                match project_terminal_outbox_debt_registered(
                    &store,
                    &broadcaster,
                    Some(&artifact_service),
                    &worker,
                    resume.as_ref(),
                    max_visits,
                    &shutdown,
                    &projection_jobs,
                )
                .await
                {
                    Ok(report) => {
                        if report.projected_events > 0
                            || report.store_failures > 0
                            || report.canonical_persistence_failures > 0
                            || report.runtime_settlement_failures > 0
                            || report.steer_ack_failures > 0
                            || report.cursor_save_failures > 0
                            || report.lease_renew_failures > 0
                        {
                            info!(
                                discovered = report.discovered,
                                projected = report.projected_events,
                                deduped = report.deduped_events,
                                store_failures = report.store_failures,
                                canonical_persistence_failures =
                                    report.canonical_persistence_failures,
                                runtime_settlement_failures = report.runtime_settlement_failures,
                                steer_ack_failures = report.steer_ack_failures,
                                cursor_save_failures = report.cursor_save_failures,
                                lease_renew_failures = report.lease_renew_failures,
                                "[STATELESS-LOOP] projected bounded terminal outbox debt"
                            );
                        }
                        resume = report.resume;
                        // A cursor means this sweep has not reached the end.
                        // Yield for scheduler fairness, then continue without a
                        // 30-second-per-page latency multiplier. Exact claim and
                        // journal/cursor revalidation still happen per key.
                        scan_immediately = resume.is_some();
                    },
                    Err(error) => {
                        // The failing call did not publish a successor cursor.
                        // Keep the last known position and use the steady-state
                        // cadence as bounded retry backoff.
                        scan_immediately = false;
                        warn!(
                            error = %error,
                            "[STATELESS-LOOP] terminal outbox projection page failed; cursor retained"
                        );
                    },
                }
            }
        })
    };
    // Park retirement is a lifecycle concern, not work a newly-created user
    // execution should perform on its hot path. Run one bounded page at boot
    // and every 30 minutes thereafter. The cursor survives between passes so a
    // stable non-retirable prefix cannot starve an expired tail; reaching the
    // end resets it to the beginning.
    //
    // BOTH retirable grounds are named. The deadline one reads nothing outside
    // the run. The completed-child one reads the children, and was held at
    // diagnosis-only until its proof stopped being an inference: a refinement
    // pass could commit `Success` before its successor address had been seeded,
    // so an exhausted address chain meant "not started writing yet" as often as
    // it meant "nothing followed". The closing writer now publishes a
    // `ChainClosure` receipt for the segment it finished on and the reconciler
    // proves a `Success` child finished only when it read one — so a park judged
    // inside that window reports `ChildChainNotClosed` and is left alone.
    // Neither ground retires a `Stalled` row, and a park committed before the
    // address chain existed stays permanently unprovable by design.
    let parked_execution_reconciler_task = {
        let store =
            magician::magician_v2::execution::agentic::run_loop::store::fs::FsLoopStateStore::new(
                shared_artifact_v2_service.workspace().base_root(),
            );
        let shutdown = supervisor_shutdown.clone();
        let ready = stateless_loop_lifecycle_ready.clone();
        tokio::spawn(async move {
            use magician::magician_v2::execution::agentic::run_loop::reconciler::{
                LoopReconciler, ReconcilerPolicy, ReconcilerWorker, Recovery, RetirableGround,
            };

            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ready.cancelled() => {},
            }

            let worker = ReconcilerWorker::for_this_process();
            let mut scan_after = None;
            let mut cadence = tokio::time::interval(Duration::from_secs(30 * 60));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = cadence.tick() => {
                        let reconciler = LoopReconciler::new(
                            &store,
                            ReconcilerPolicy {
                                scan_after: scan_after.clone(),
                                recovery: Recovery::RetireProvedParks {
                                    worker: worker.clone(),
                                    grounds: [
                                        RetirableGround::DeadlinePassedWhileParked,
                                        RetirableGround::EveryChildHasFinished,
                                    ]
                                    .into_iter()
                                    .collect(),
                                },
                                ..ReconcilerPolicy::default()
                            },
                        );
                        match reconciler.reconcile().await {
                            Ok(report) => {
                                if report.examined > 0 || report.retired > 0 || report.incomplete {
                                    info!(
                                        parked = report.parked,
                                        examined = report.examined,
                                        retired = report.retired,
                                        incomplete = report.incomplete,
                                        "[STATELESS-LOOP] reconciled bounded parked-execution page"
                                    );
                                }
                                scan_after = report.resume;
                            },
                            Err(error) => warn!(
                                error = %error,
                                "[STATELESS-LOOP] parked-execution reconciliation page failed; cursor retained"
                            ),
                        }
                    },
                }
            }
        })
    };
    shared_artifact_v2_service.set_task_note_publisher(shared_notes_api.get_ref().settings_store());
    shared_notes_api
        .get_ref()
        .set_task_service(Arc::clone(&shared_artifact_v2_service));

    // Verification: config owns the deployment decision, and the env var —
    // when set — stays the process-local kill switch. Both must land before
    // startup recovery or the reconciler can spawn a driver, so a gate never
    // runs under the defaults a configured deployment overrode.
    shared_artifact_v2_service.set_verification_activation(
        magician::magician_v2::execution::verification::activation_from_env_or_config(
            magician_config.verification.mode.as_deref(),
        ),
    );
    shared_artifact_v2_service
        .set_verification_settings(magician_config.verification.runtime_settings());
    shared_artifact_v2_service
        .set_app_processing_trust_settings(magician_config.app_platform.processing.clone());
    shared_artifact_v2_service
        .set_app_resource_policy(magician_config.app_platform.resources.enforcement_policy())?;
    shared_artifact_v2_service.set_app_spend_resolver(Arc::clone(&scoped_authority_resolver));
    shared_app_platform_api
        .get_ref()
        .set_workflow_service(shared_artifact_v2_service.app_workflow_service());

    // `/evals` — lanes derived from the repo Makefile's `## eval:` annotations.
    // Wired here because the executor needs BOTH the orchestrator and the
    // artifact-v2 service: a lane runs as an ordinary internal execution, so
    // cancel, progress, HITL and spend gating are inherited rather than rebuilt.
    // `COVERAGE_BASE_DIR` is what the `report=` annotations are relative to
    // (`Makefile:406`); the repo-root `coverage/` symlink is the fallback.
    let evals_reports_root = std::env::var("COVERAGE_BASE_DIR")
        .ok()
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| repo_root.join("coverage"));
    let shared_evals_api = web::Data::new(
        magician_api::evals_api::EvalsApi::new(
            storage_workspace.clone(),
            repo_root.clone(),
            evals_reports_root,
        )
        .with_probe_targets(magician_surfaces::evals::readiness::ProbeTargets::resolve(
            &magician_config,
            &repo_root,
        ))
        .with_executor(Arc::new(
            magician_surfaces::evals::executor::TaskBackedEvalExecutor::new(
                Arc::clone(&orchestrator),
                Arc::clone(&shared_artifact_v2_service),
            ),
        ))
        .with_llm_analytics_read_service(Arc::clone(&shared_llm_analytics_read_service)),
    );

    // Proactive Resurfacing Engine (Phase 2): spawn the worker here, after the
    // shared artifact-v2 service exists, so the scorer sweep can read the
    // task/episode source and the curator can use the LLM path (deterministic
    // fallback when no router is bound). On by default; RESURFACING_ENABLED=0
    // makes `spawn` return a no-op handle.
    let resurfacing_worker =
        magician_comms::channel_assist::resurfacing::worker::ResurfacingWorker::spawn_with_attention(
            resurfacing_store.clone(),
            AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
            Arc::clone(&shared_artifact_v2_service),
            channel_assist_store.clone(),
            orchestrator.operation_llm_router(),
            magician_comms::channel_assist::resurfacing::curator::CurationRecommendationPolicy::enabled(
                magician_config.resurfacing.contextual_actions_enabled,
                magician_config.resurfacing.recommendation_min_confidence as f32,
            ),
            magician_config.resurfacing.active_repair_enabled,
            magician_config
                .resurfacing
                .active_repair_batch_size
                .clamp(1, 100),
            magician_config.resurfacing.surface_cap,
            magician::magician_v2::attention::resurfacing::centrality::CentralityEmbeddingConfig::from_seconds(
                magician_config
                    .resurfacing
                    .centrality_reference_timeout_secs,
                magician_config.resurfacing.centrality_query_timeout_secs,
                magician_config.resurfacing.centrality_reference_batch_size,
            ),
            magician_config.resurfacing.memory_tiers.clone(),
            Some(attention_funnel_store.clone()),
            Some(Arc::clone(&user_request_service)),
        );

    // Town Square. The corpus lives in the `town-square` app package now
    // (queue item 6), so `/social/*` reads and writes the package's entity
    // store through the governed owner data plane rather than a first-party
    // SQLite engine. The autonomous worker retired with it: agents take turns
    // through the package's `ambient_turn` behavior, which the platform
    // schedules, budgets and can have narrowed.
    //
    // The registry is still constructed because the one-shot corpus migration
    // reads it, and because it stays intact after a migration so a bad run is
    // recoverable rather than terminal.
    // Retained for storage governance (retention/lifecycle of the retired
    // SQLite corpus) and for `town-square-migrate`, which reads it as the
    // migration source. Nothing else may read it: the Town Square package's
    // entity store owns the corpus since the engine retirement, and a second
    // reader of a store that no longer receives writes reports stale rows as
    // healthy ones.
    let social_store = Arc::new(
        magician::magician_v2::social::store::SocialStoreRegistry::new(storage_workspace.clone()),
    );
    let shared_social_api = web::Data::new(magician_api::social_api::SocialApi::new(
        shared_app_platform_api.entity_adapter(),
        shared_app_platform_api.registry(),
        shared_app_platform_api.background_behaviors(),
        magician_config.social.max_post_chars,
    ));

    // The writer that keeps `outward_gate::contact_refusal` from screening every
    // outward act against an empty register. Without it the suppression check
    // passes vacuously for every real recipient — and keeps passing after they
    // bounce or complain, because nothing else can record either.
    //
    // Named bindings, not `let _ = ...`: the JoinHandle would drop immediately.
    //
    // Tier 4: the bounce bridge's source. `None` unless an operator names a
    // mailbox, and `spawn_with_receipts` reports that absence as `no_source` on
    // every tick rather than as an idle worker — a sweep with no receipt source
    // finds nothing because it asked nobody, and that must never render like a
    // quiet week.
    let bounce_puller: Option<
        std::sync::Arc<dyn magician::magician_v2::delivery_hygiene::receipts::ReceiptPuller>,
    > = magician_config
        .delivery_hygiene
        .bounce_mailbox
        .as_ref()
        .map(|mailbox| {
            info!(
                "[DELIVERY-HYGIENE] bounce mailbox for provider `{}` at {}",
                mailbox.provider, mailbox.maildir_path
            );
            std::sync::Arc::new(
                magician::magician_v2::delivery_receipts::pull::DsnPuller::new(
                    mailbox.provider.clone(),
                    std::sync::Arc::new(
                        magician::magician_v2::delivery_receipts::MaildirBounceMailbox::at(
                            &mailbox.maildir_path,
                        ),
                    ),
                    storage_workspace.clone(),
                ),
            )
                as std::sync::Arc<
                    dyn magician::magician_v2::delivery_hygiene::receipts::ReceiptPuller,
                >
        });
    let delivery_hygiene_worker =
        magician::magician_v2::delivery_hygiene::worker::SuppressionSweepWorker::spawn_with_receipts(
            storage_workspace.clone(),
            magician_config.delivery_hygiene.clone(),
            supervisor_shutdown.clone(),
            bounce_puller,
        );
    // What `GET /delivery/watch` reads. Without it the route answers 503 by
    // design: "no watcher is running" must never render as "the watcher found
    // nothing" — a rising unreconciled count is the only early warning that
    // sending has silently broken.
    let shared_delivery_watch_health = web::Data::new(delivery_hygiene_worker.health());
    let _delivery_hygiene_worker = delivery_hygiene_worker;

    // What fills the obligation register from real activity: the data-room
    // follow-up sweep and the scheduling silence sweep. Until now the register
    // an agent's cycle reads was permanently empty, so nothing was ever owed.
    // Hoisted so the sweep and the owner-facing read cannot be given different
    // waiting windows — they would disagree about what is overdue.
    let obligation_sweep_config =
        magician_media::obligation_sweeps::worker::ObligationSweepConfig::default();
    let _obligation_sweep_worker =
        magician_media::obligation_sweeps::worker::ObligationSweepWorker::spawn(
            storage_workspace.clone(),
            obligation_sweep_config.clone(),
            supervisor_shutdown.clone(),
        );

    // The only producer of `silent` outcome observations, and the pass that
    // turns the cohorts it fills into decisions an owner makes. Without the
    // first, the cohort comparison sees replies and nothing else, so a variant
    // everybody ignored is indistinguishable from one nobody tried.
    //
    // Ships OFF: it records a judgement — that somebody who has not answered
    // inside a chosen window has decided — into an append-only store that
    // cannot un-write it.
    let outcome_maturity_worker =
        magician_learning::outcome_learning::spawn_configured_maturity_sweep(
            storage_workspace.clone(),
            magician_config.outcome_maturity.clone(),
            supervisor_shutdown.clone(),
        );
    // The proposal pass reads the maturity worker's HEALTH, not just its config:
    // a cohort from a store that only ever received replies is not evidence
    // about a variant, so a tenant no completed sweep covers has every
    // comparison withheld with its counts rather than proposed.
    let outcome_proposal_worker = magician_learning::outcome_learning::OutcomeProposalWorker::spawn(
        storage_workspace.clone(),
        magician_learning::outcome_learning::OutcomeProposalConfig {
            enabled: magician_config.outcome_proposal.enabled,
            paused: magician_config.outcome_proposal.paused,
            tick_interval_secs: magician_config.outcome_proposal.tick_interval_secs,
            minimum_usable: magician_config.outcome_proposal.minimum_usable,
            minimum_counterparties: magician_config.outcome_proposal.minimum_counterparties,
            candidate_type: magician_config.outcome_proposal.candidate_type.clone(),
        },
        magician_config.outcome_maturity.clone(),
        Some(outcome_maturity_worker.health()),
        supervisor_shutdown.clone(),
    );
    // Both workers' health, readable from outside the process. Attached here for
    // the same reason `shared_delivery_watch_health` is: a worker whose only
    // account of itself is a log line is a worker nobody notices has stopped.
    // `acts_unbound` is the specific number worth a route — the sweep finding
    // acts it cannot bind to any cohort is the config being incomplete, and it
    // is invisible in `matured`, which reads as a healthy zero.
    // The owner surface for observed transcripts and the claims they appear to
    // make. Wired here because it is what makes three built-and-unreachable
    // modules — observed statements, transcript ingestion, and the commitment
    // register — reachable from outside their own tests for the first time.
    let shared_transcript_claims_api = web::Data::new(
        magician_api::transcript_claims_api::TranscriptClaimsApi::new(storage_workspace.clone()),
    );
    // Phase 4's ACTING half. The record half — who we told, on the strength of
    // what — was built and indexed at write time; raising a debt against each
    // affected recipient was built too and had no caller, so the store could
    // answer "who did we tell" and there was no way to ask.
    let shared_corrections_api = web::Data::new(
        magician_api::corrections_api::CorrectionsApi::new(storage_workspace.clone()),
    );
    // The four recipient checks readiness §9B says belong BELOW the agent
    // rather than in its judgement: duplicate contact across programs, a
    // follow-up to somebody already waiting on us, a jurisdiction hold, and a
    // recorded erasure request. Attached here because a gate nothing can reach
    // refuses nothing — and an erasure request with no route to file it is a
    // guard over a store nobody can write to.
    let shared_recipient_compliance_api = web::Data::new(
        magician_api::recipient_compliance_api::RecipientComplianceApi::new(
            storage_workspace.clone(),
        ),
    );
    let shared_outcome_maturity_health = web::Data::new(outcome_maturity_worker.health());
    let shared_outcome_proposal_health = web::Data::new(outcome_proposal_worker.health());
    let _outcome_maturity_worker = outcome_maturity_worker;
    let _outcome_proposal_worker = outcome_proposal_worker;

    // WEG Phase 4: consent surface for the account-based connectors (email, calendar).
    // Constructed here (after the artifact-v2 service) because its PUT handler creates
    // the scheduled writer tasks via the service.
    let shared_observe_api = web::Data::new(magician_api::observe_connectors_api::ObserveApi::new(
        storage_workspace.clone(),
        Arc::clone(&shared_artifact_v2_service),
    ));

    // Channel Assist: sync status, manual-run, annotation, and follow-up APIs,
    // sharing the ChannelAssistStore instance with the background sync worker.
    let shared_channel_assist_api =
        web::Data::new(magician_api::channel_assist_api::ChannelAssistApi::new(
            storage_workspace.clone(),
            channel_assist_store.clone(),
            Arc::clone(&shared_artifact_v2_service),
            orchestrator.operation_llm_router(),
            Some(Arc::clone(&event_broadcaster)),
        ));
    let resurfacing_operation_router = orchestrator.operation_llm_router();
    let resurfacing_interactions =
        magician_comms::channel_assist::resurfacing::interaction::ResurfacingInteractionRegistry::new(
            storage_workspace.clone(),
            channel_assist_store.clone(),
            Arc::clone(&shared_artifact_v2_service),
            AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
            resurfacing_operation_router.clone(),
            magician_config.resurfacing.rich_briefs_enabled,
            magician_config.resurfacing.source_details_enabled,
            magician_config.resurfacing.contextual_actions_enabled,
            magician_config.resurfacing.recommendations_enabled,
            magician_config.resurfacing.recommendation_min_confidence as f32,
        );
    let shared_resurfacing_actions = web::Data::new(
        magician_comms::channel_assist::resurfacing::actions::ResurfacingActionService::new(
            resurfacing_store.clone(),
            resurfacing_interactions.clone(),
            Arc::clone(&shared_artifact_v2_service),
            magician::magician_v2::learning::LearningStore::new(storage_workspace.clone()),
            resurfacing_operation_router,
            Some(Arc::clone(&event_broadcaster)),
            magician_config.resurfacing.action_result_cooldown_days,
            magician_config.resurfacing.contextual_actions_enabled,
        )
        // Opts the follow-up lane into the same contextual-action path, so both
        // lanes share one claim, replay, and error taxonomy instead of drifting
        // apart in parallel implementations.
        .with_channel_store(channel_assist_store.clone()),
    );
    let shared_resurfacing_interactions = web::Data::new(resurfacing_interactions);

    // The per-scope transport registry and writer are created alongside the
    // broadcaster in `MagicianServiceBuilder`, before producer-capable
    // services start. Reuse that same registry for event API backfill.

    // Unified event-stream API — single live tail of the runtime
    // broadcaster + per-execution `events.jsonl` backfill. Exposes
    // `GET /api/magician/v3/events`. See `events_api.rs` for the design.
    let shared_events_api = web::Data::new(
        EventsApi::new(
            Arc::clone(&event_broadcaster),
            Arc::clone(&shared_artifact_v2_service),
        )
        .with_workspace_event_log_registry(workspace_event_log_registry.clone()),
    );
    let shared_vibedev_api = web::Data::new(
        VibeDevApi::new(Arc::clone(&shared_artifact_v2_service))
            .with_workspace_event_log_registry(workspace_event_log_registry)
            .with_secret_store_resolver(Arc::clone(orchestrator.secret_store_resolver()))
            .with_deploy_config(magician_config.vibedev_deploy.clone())
            // M3 code_knowledge P3: the `magician_code_knowledge` citizen tool
            // hybrid-recalls the run agent's distilled code facts. Reuse the
            // shared definition store (built above) + a scoped memory resolver.
            .with_memory_resolver(AgentMemoryResolver::with_workspace_layout(
                storage_workspace.clone(),
            ))
            .with_definition_store(Arc::clone(&shared_agent_api.definition_store)),
    );

    let enrollment_store_resolver =
        magician::magician_v2::chat::enrollment::EnrollmentStoreResolver::new(
            storage_workspace.clone(),
        );

    // Create shared Chat API service and Memory API service
    let (
        shared_chat_api,
        shared_memory_api,
        shared_progress_channel_api,
        shared_feed_api,
        shared_fleet_state_api,
        shared_execution_panel_api,
        shared_ui_thread_api,
        shared_storage_governance_service,
        shared_progress_router,
        shared_task_api_v3,
        chat_turn_event_sink,
        shared_prompt_manager,
        shared_voice_context_compactor,
        shared_chat_store_for_voice,
        shared_operation_llm_router,
        shared_event_broadcaster_for_voice,
        shared_chat_service_for_voice,
        shared_dispatch_queue,
        _shared_cancel_bridge,
        shared_agent_resources_data,
        shared_social_api_data,
        shared_content_acquisition_resolver,
    ) = {
        let chat_store = Arc::clone(&process_chat_store);
        let public_contact_profile_store = Arc::new(FilePublicContactProfileStore::new(
            storage_workspace.clone(),
        ));

        // Note: the chat-store sink is spawned further down — once the
        // progress_router and ProgressChatChannel exist — because the
        // sink now also handles `ProgressEvent` (looks up matching chat
        // subscriptions in the router and invokes the chat-channel's
        // render logic). See the `ChatStoreSink::new(...)` call below
        // the `progress_router` + `chat_channel` constructions.

        // ChatTurnEventSink — projects every bus event carrying a
        // `chat_turn_id` into a per-turn JSONL file AND broadcasts the
        // same projection to live SSE subscribers. Activity card
        // (refresh + live) reads exclusively from this single source so
        // the two views agree by construction (no pagination/ordering/
        // filter drift between SSE and REST).
        let chat_turn_event_sink =
            magician::magician_v2::chat::chat_turn_event_sink::ChatTurnEventSink::new(
                storage_workspace.clone(),
            );
        Arc::clone(&chat_turn_event_sink).spawn(Arc::clone(&event_broadcaster));

        let prompt_manager = orchestrator.prompt_manager();
        // Surface PromptManager as web::Data so HTTP handlers can
        // render canonical prompts (e.g. the voice controller system
        // prompt injected into the realtime session via the control
        // WS handler).
        let shared_prompt_manager = web::Data::new(Arc::clone(&prompt_manager));

        let llm_service = match orchestrator.multi_llm_service() {
            Some(multi_llm) => ChatLlmService::new(Arc::clone(multi_llm))
                .with_workspace_layout(storage_workspace.clone()),
            None => {
                warn!(
                    "[CHAT] MultiLLMService not available; chat will fail on LLM calls. \
                     Ensure magician-config.yaml has LLM configurations."
                );
                // Create with a dummy — will error at runtime if chat is used
                ChatLlmService::new(Arc::new(
                    magician::magician_v2::query_analysis::multi_llm_service::MultiLLMService::new(
                        std::collections::HashMap::new(),
                        std::collections::HashMap::new(),
                    ),
                ))
                .with_workspace_layout(storage_workspace.clone())
            },
        };

        let persona_provider = Arc::new(
            magician::magician_v2::chat::service::DefinitionStorePersonaProvider::new(Arc::clone(
                &shared_agent_api.definition_store,
            )),
        );

        // Chat and progress memory resolve directly into scoped V3 memory.
        let chat_memory_resolver =
            AgentMemoryResolver::with_workspace_layout(storage_workspace.clone());
        {
            let resolver = chat_memory_resolver.clone();
            tokio::spawn(async move {
                if !magician::magician_v2::runtime::startup::wait_for_http().await {
                    return;
                }
                let now = chrono::Utc::now().timestamp();
                let mut scopes = resolver.list_tenant_scopes();
                if scopes.is_empty() {
                    scopes.push(("anonymous".to_string(), "default".to_string()));
                }
                for (principal, workspace) in scopes {
                    let Ok(service) = resolver.resolve_for_scope(&principal, &workspace) else {
                        continue;
                    };
                    match service.backfill_entry_timestamps(now).await {
                        Ok(0) => {},
                        Ok(stamped) => info!(
                            principal,
                            workspace,
                            stamped,
                            "[STARTUP] Backfilled missing user-knowledge updated_at stamps"
                        ),
                        Err(error) => warn!(
                            principal,
                            workspace,
                            error = %error,
                            "[STARTUP] User-knowledge timestamp backfill failed"
                        ),
                    }
                }
            });
        }
        let progress_memory_resolver = Arc::new(chat_memory_resolver.clone());
        orchestrator.set_artifact_v2_service(Arc::clone(&shared_artifact_v2_service));

        // Bring up the global LLM dispatch queue. Routes every LLM call
        // through a priority-lane worker pool with retries, idempotency,
        // cancellation gates, per-provider concurrency caps + circuit
        // breaker, and a viewer at /api/llm/queue/snapshot. See
        // `docs/components/magician/llm-dispatch-queue.md`.
        let (shared_dispatch_queue, shared_cancel_bridge): (
            Option<std::sync::Arc<magicllm::LlmDispatchQueue>>,
            Option<std::sync::Arc<magician::magician_v2::dispatch_glue::CancelBridge>>,
        ) = if let Some(operation_router) = orchestrator.operation_llm_router() {
            if operation_router.shared_configured_router().is_some() {
                // The live router, so a config reload reaches the queue.
                let dispatch_router: std::sync::Arc<dyn magicllm::dispatch::DispatchRouter> =
                    operation_router.live_dispatch_router();
                let dispatch_cfg = magician_config.llm.dispatch.clone();
                let dispatch_enabled = dispatch_cfg.enabled;
                let local_prep_cfg = dispatch_cfg.local_prep.clone();
                let (queue, task_state_view) =
                    magician::magician_v2::dispatch_glue::start_dispatch_queue(
                        dispatch_router,
                        Arc::clone(&shared_artifact_v2_service),
                        storage_root.clone(),
                        dispatch_cfg,
                    )
                    .await;
                tracing::info!(
                    workers = queue.worker_count(),
                    enabled = dispatch_enabled,
                    "LLM dispatch queue started"
                );
                // Persist per-job terminal outcomes (lane, provider, wait/exec
                // ms, attempts, tombstone reason, local-prep savings) to
                // date-partitioned Parquet under the default scope's analytics
                // root (`analytics/llm_dispatch/dt=*`). Subscribe BEFORE any
                // await so events emitted during boot aren't missed. Spawned
                // regardless of `dispatch_enabled` — when the queue is built
                // for observability/cancel plumbing but not wired, it simply
                // sees no traffic. Detached: a lost final batch on shutdown is
                // at most ~30s of telemetry.
                let _llm_dispatch_telemetry =
                    magician::magician_v2::analytics::llm_dispatch_rows::LlmDispatchTelemetrySink::spawn(
                        queue.subscribe_events(),
                        storage_workspace.clone(),
                    );
                // Failed and tombstoned calls do not yield response receipts.
                // Subscribe their canonical bridge before the queue is exposed
                // to any operation router.
                llm_trace_activation.attach_dispatch_events(queue.subscribe_events());
                let _llm_dispatch_retention =
                    magician::magician_v2::analytics::llm_dispatch_rows::LlmDispatchRetention::spawn(
                        storage_workspace.clone(),
                        90,
                    );
                // Route all LLM traffic through the queue by installing it on
                // the OperationLlmRouter (the single chokepoint every LLM call
                // funnels through). Shared `Arc<OnceLock>` so per-agent clones
                // inherit it. Gated by `llm.dispatch.enabled` — the kill-switch:
                // when false, the queue is still built (observability/cancel
                // plumbing) but callers fall back to direct routing.
                if dispatch_enabled {
                    operation_router.set_dispatch_queue(Arc::clone(&queue));
                    if let Some(multi_llm) = orchestrator.multi_llm_service() {
                        multi_llm.set_dispatch_queue(Arc::clone(&queue));
                    }
                    tracing::info!(
                        "LLM dispatch queue wired into OperationLlmRouter and MultiLLMService — operation and chat streams now routed through the queue"
                    );
                } else {
                    tracing::warn!(
                        "llm.dispatch.enabled=false — queue built but NOT wired; LLM calls use direct routing"
                    );
                }
                // Best-effort: warm the local-prep Ollama model so the first
                // local pre-summarization call isn't cold. No-op unless
                // llm.dispatch.local_prep.enabled.
                magician::magician_v2::dispatch_glue::spawn_local_prep_prewarm(local_prep_cfg);
                let bridge =
                    std::sync::Arc::new(magician::magician_v2::dispatch_glue::CancelBridge::new(
                        Arc::clone(&queue),
                        task_state_view,
                    ));
                (Some(queue), Some(bridge))
            } else {
                tracing::warn!(
                    "OperationLlmRouter has no ConfiguredRouter — LLM dispatch queue not started"
                );
                (None, None)
            }
        } else {
            tracing::warn!("OperationLlmRouter not configured — LLM dispatch queue not started");
            (None, None)
        };
        let _ = &shared_dispatch_queue; // route registration follows below
                                        // Wire the cancel bridge into the orchestrator so cancel_execution /
                                        // cancel_execution_tree tombstone the execution's queued LLM jobs and
                                        // abort its in-flight provider call. Jobs carry their execution id as a
                                        // TaskRef via the per-execution OperationLlmRouter clone (see
                                        // `build_action_executors_with_operation_routing_overrides`).
        if let Some(bridge) = &shared_cancel_bridge {
            orchestrator.set_cancel_bridge(Arc::clone(bridge));
        }
        orchestrator
            .screenshot_storage()
            .set_v3_service(Arc::clone(&shared_artifact_v2_service));

        let progress_router = ExecutionProgressRouter::with_workspace_layout(
            storage_workspace.clone(),
            Arc::clone(&shared_artifact_v2_service),
            Arc::clone(&orchestrator),
        )
        .await
        .context("Failed to create progress router")?;
        chat_store.set_archive_observer(Arc::new(
            magician::magician_v2::chat::service::ChatSessionLifecycleArchiveObserver::new(
                progress_router.clone(),
                Arc::clone(&shared_agent_api.definition_store),
            ),
        ));
        let chat_store: Arc<dyn magician::magician_v2::chat::storage::ChatStore> = chat_store;

        // ProgressChatChannel holds the rendering logic that turns
        // ProgressMessages into ChatMessageContent for chat sessions.
        // It is NOT registered on the router (would dispatch via the
        // legacy ProgressChannel path); instead it's invoked from the
        // ChatStoreSink, which subscribes to ProgressEvent on the bus
        // and walks matching chat subscriptions via the router's
        // subscription registry.
        let chat_channel = Arc::new(ProgressChatChannel::new(
            Arc::clone(&chat_store),
            Arc::clone(&event_broadcaster),
            Arc::clone(&shared_artifact_v2_service),
        ));

        // Single-bus migration: chat-store sink. Subscribes to the bus.
        //   - ChatMessageReceived  → append_message into chat_store
        //   - ProgressEvent        → look up matching chat subs, render
        //                            via chat_channel.deliver (which
        //                            emits ChatMessageReceived; loops
        //                            back into the first arm). Single
        //                            sink owns the whole chat-store
        //                            projection.
        // Spawn order: after broadcaster + chat_store + chat_channel +
        // progress_router exist; before anything else that emits.
        magician::magician_v2::chat::chat_store_sink::ChatStoreSink::new(
            Arc::clone(&chat_store),
            Arc::clone(&event_broadcaster),
            progress_router.clone(),
            Arc::clone(&chat_channel),
        )
        .spawn();

        // Ambient Thinking Map session coordinator: subscribe to the bus here
        // (inside the async block, before other emitters) so no early utterance
        // is missed. The shared `web::Data` handle is created at the outer scope
        // (next to `shared_thinking_maps_api`) so it can be moved into the
        // HttpServer factory; here we just spawn the subscription loop.
        Arc::clone(&thinking_map_coordinator).spawn();
        // WebhookChannel + AgentMemoryChannel stay router-dispatched —
        // the router owns the reliability infrastructure (per-channel
        // back-pressure semaphore, persisted retry queue, circuit
        // breaker, terminal-vs-best-effort delivery semantics) that
        // external HTTP delivery to bot URLs depends on. A bus
        // subscriber would lose all of that. The router is correctly
        // homed here for any consumer where delivery semantics matter;
        // direct bus subscribers (ChatStoreSink, persistence sink,
        // SSE handler) are for local, fast, no-retry-needed projections.
        progress_router
            .register_channel(
                Arc::new(
                    ProgressWebhookChannel::new()
                        .context("Failed to construct webhook progress channel")?,
                ),
                8,
            )
            .await;
        progress_router
            .register_channel(
                Arc::new(ProgressAgentMemoryChannel::new(
                    Arc::clone(&progress_memory_resolver),
                    Arc::clone(&shared_agent_api.definition_store),
                )),
                8,
            )
            .await;
        progress_router.start(Arc::clone(&event_broadcaster));
        let chat_progress_sync = ChatProgressSync::new(
            Arc::clone(&chat_store),
            progress_router.clone(),
            Arc::clone(&shared_agent_api.definition_store),
            Arc::clone(&event_broadcaster),
        );
        chat_progress_sync.start_definition_change_listener();
        let progress_router_for_startup_reconcile = progress_router.clone();
        let definition_store_for_startup_reconcile = Arc::clone(&shared_agent_api.definition_store);
        let chat_progress_sync_for_startup_prune = chat_progress_sync.clone();
        tokio::spawn(async move {
            let Some(_startup_permit) =
                magician::magician_v2::runtime::startup::admit_backfill().await
            else {
                return;
            };
            match tokio::time::timeout(
                Duration::from_secs(30),
                progress_router_for_startup_reconcile
                    .reconcile_declarative_subscriptions(&definition_store_for_startup_reconcile),
            )
            .await
            {
                Ok(Ok(())) => {},
                Ok(Err(error)) => warn!(
                    error = %error,
                    "[STARTUP] Failed to reconcile declarative progress subscriptions"
                ),
                Err(_) => warn!(
                    "[STARTUP] Timed out reconciling declarative progress subscriptions; backend startup continues"
                ),
            }

            match tokio::time::timeout(
                Duration::from_secs(30),
                chat_progress_sync_for_startup_prune.prune_stale_materializations(),
            )
            .await
            {
                Ok(Ok(_)) => {},
                Ok(Err(error)) => warn!(
                    error = %error,
                    "[STARTUP] Failed to prune stale chat lifecycle progress subscriptions"
                ),
                Err(_) => warn!(
                    "[STARTUP] Timed out pruning stale chat lifecycle progress subscriptions; backend startup continues"
                ),
            }
        });

        let feed_store = FeedStore::open_workspace(storage_workspace.clone())
            .context("Failed to open feed store")?
            .with_database_maintenance(magician_config.database_maintenance.clone());
        // The taste-profile loader is installed earlier (in the service
        // builder, so embedded harnesses get injection too) than the feed
        // store exists. Hand it the feed now so an over-ceiling profile can
        // raise its owner-facing nudge; injection already works without it.
        if let Some(loader) = magician::magician_v2::taste_profile::global_taste_profile_loader() {
            loader.set_over_ceiling_feed(Arc::new(feed_store.clone()));
            // Capture rides the same loader: it needs the note read path for
            // the mediated write, and the same settings for where proposals
            // are filed. Installed only when capture is enabled, so a
            // deployment that has not opted in has no service to call and the
            // review endpoints truthfully answer "off" rather than empty.
            if loader.settings().capture_enabled {
                // Beside the device policy and audit stores under
                // `<base_root>/system/`. One file per scope is created inside
                // this directory — proposals are distilled from an owner's
                // private transcripts and must not share a store.
                let proposals_dir = storage_workspace.base_root().join("system");
                let service = Arc::new(
                    magician::magician_v2::taste_capture::TasteCaptureService::new(
                        loader.clone(),
                        proposals_dir,
                    ),
                );
                magician::magician_v2::taste_capture::install_global_capture_service(service);
            }
        }
        let feed_store_for_startup_materialize = feed_store.clone();
        tokio::spawn(async move {
            let Some(_startup_permit) =
                magician::magician_v2::runtime::startup::admit_backfill().await
            else {
                return;
            };
            match tokio::time::timeout(
                Duration::from_secs(30),
                feed_store_for_startup_materialize.materialize_existing_scopes(),
            )
            .await
            {
                Ok(Ok(())) => {},
                Ok(Err(error)) => warn!(
                    error = %error,
                    "[STARTUP] Failed to materialize scoped feed DuckDB templates"
                ),
                Err(_) => warn!(
                    "[STARTUP] Timed out materializing scoped feed DuckDB templates; backend startup continues"
                ),
            }
        });
        shared_artifact_v2_service
            .set_feed_projection(feed_store.clone(), Arc::clone(&event_broadcaster));
        shared_artifact_v2_service
            .set_progress_projection(Arc::clone(&event_broadcaster), feed_store.clone());
        // Recurring Monitors Phase 3: monitor notification dedupe rows +
        // access-problem route events land in the SAME durable attention
        // funnel store every other producer uses (§7.4 — UNIQUE event_id).
        shared_artifact_v2_service.set_attention_funnel_store(attention_funnel_store.clone());
        // Task writes reconcile the index in the same call that writes the
        // file, and the list handlers seek into it instead of walking every
        // record. Wired here, after the service exists, for the same reason
        // the funnel store is: the index is opened by the host, not by the
        // service that uses it.
        shared_artifact_v2_service.set_list_index(list_index.clone());
        FeedMaterializer::new(
            feed_store.clone(),
            Arc::clone(&shared_artifact_v2_service),
            Arc::clone(&chat_store) as Arc<dyn magician::magician_v2::chat::storage::ChatStore>,
            progress_router.progress_stream(),
            event_broadcaster.subscribe(),
            Arc::clone(&event_broadcaster),
        )
        .start();

        // Project canonical HITL events into the owning active chat session.
        // The listener emits chat records only; ChatStoreSink owns persistence,
        // while every response still goes through the canonical HITL endpoint.
        // Keeping the projection live is what lets planning clarifications use
        // the same typed contract in Chat as they do in Attention.
        EscalationListener::new(
            Arc::clone(&chat_store) as Arc<dyn magician::magician_v2::chat::storage::ChatStore>,
            Arc::clone(&shared_artifact_v2_service),
            Arc::clone(&orchestrator),
            Arc::clone(&event_broadcaster),
        )
        .start();
        let planning_listener = PlanningListener::new(
            Arc::clone(&chat_store) as Arc<dyn magician::magician_v2::chat::storage::ChatStore>,
            Arc::clone(&event_broadcaster),
        );
        planning_listener.start();
        {
            let service = Arc::clone(&shared_artifact_v2_service);
            tokio::spawn(async move {
                if !magician::magician_v2::runtime::startup::wait_for_http().await {
                    return;
                }
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    service.reconcile_published_surfaces_startup(),
                )
                .await
                {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => warn!(
                        error = %error,
                        "[STARTUP] Failed to reconcile V3 published surfaces and materialized layouts"
                    ),
                    Err(_) => warn!(
                        "[STARTUP] Timed out reconciling V3 published surfaces; backend startup continues"
                    ),
                }
            });
        }

        // Resume verification gates that were still holding a task when this
        // process stopped. A held candidate is a task deliberately kept
        // non-terminal, so if nothing picks the gate back up the task never
        // finishes — the durable outbox exists so a restart can find it.
        // No-op unless the controller is enabled.
        // See `ArtifactV2Service::recover_pending_verification`.
        {
            let service = Arc::clone(&shared_artifact_v2_service);
            tokio::spawn(async move {
                if !magician::magician_v2::runtime::startup::wait_for_http().await {
                    return;
                }
                service.recover_pending_verification().await;
            });
        }

        // Between restarts, the reconciler is what re-offers retryable gates,
        // applies the elapsed budget to a stalled repair, and re-drives a
        // settled-but-unreleased gate — the periodic half of the recovery
        // above. Returns None (spawns nothing) when the controller is
        // disabled or `verification.reconcile_interval_secs` is 0.
        // See `ArtifactV2Service::spawn_verification_reconciler`. The handle is
        // dropped deliberately: the reconciler lives as long as the process,
        // and aborting it is not something any caller should be able to do by
        // dropping a handle it never wanted.
        let _ = shared_artifact_v2_service.spawn_verification_reconciler();

        // Finish admitting every VibeDev run this deployment durably accepted
        // but did not get to start. `@vibedev` records a dispatch intent before
        // it creates anything, so "queued" survives a crash — but only if
        // something claims the leftover intents, and in-process that something
        // is the chat turn the crash removed. Every unclaimed intent leaves
        // here either dispatched or terminally settled.
        // See `vibedev::rail::recover_pending_vibedev_dispatch`.
        {
            let service = Arc::clone(&shared_artifact_v2_service);
            tokio::spawn(async move {
                if !magician::magician_v2::runtime::startup::wait_for_http().await {
                    return;
                }
                magician::magician_v2::vibedev::rail::recover_pending_vibedev_dispatch(&service)
                    .await;
            });
        }

        // Reconcile every Live Thinking Map against its authoritative event
        // log. `apply_and_persist` writes append → snapshot → manifest, so a
        // crash in that window leaves one of two durable injuries, and neither
        // heals on its own:
        //
        //  - a torn trailing line in `events.jsonl`, after which `events_after`
        //    fails PERMANENTLY for that map — taking deterministic replay,
        //    restore-as-branch, and `GET /thinking-maps/{id}/events` with it;
        //  - a stale `manifest.latest_sequence`, after which the next applied
        //    envelope mints a DUPLICATE sequence number.
        //
        // `startup_repair` quarantines the torn tail and rolls the snapshot +
        // manifest forward to the last durable event; this is the only thing
        // that calls it. One damaged map is logged and counted, never fatal to
        // the sweep. See `thinking_map::replay::ThinkingMapStore::startup_repair_all`.
        {
            let workspace = shared_artifact_v2_service.workspace().clone();
            tokio::spawn(async move {
                let store = magician_surfaces::thinking_map::ThinkingMapStore::new(workspace);
                match tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    store.startup_repair_all(),
                )
                .await
                {
                    Ok(sweep) if sweep.touched_anything() => {
                        info!(
                            scopes_scanned = sweep.scopes_scanned,
                            scopes_unreadable = sweep.scopes_unreadable,
                            maps_scanned = sweep.maps_scanned,
                            maps_repaired = sweep.maps_repaired,
                            maps_quarantined = sweep.maps_quarantined,
                            maps_skipped_concurrent_write = sweep.maps_skipped_concurrent_write,
                            maps_failed = sweep.maps_failed,
                            "[STARTUP] Reconciled Live Thinking Map snapshots against their event logs"
                        );
                        if sweep.maps_failed > 0 {
                            warn!(
                                maps_failed = sweep.maps_failed,
                                "[STARTUP] Some thinking maps could not be repaired; their event \
                                 logs still will not replay (see per-map errors above)"
                            );
                        }
                    },
                    Ok(_) => {},
                    Err(_) => warn!(
                        "[STARTUP] Timed out reconciling Live Thinking Maps; backend startup continues"
                    ),
                }
            });
        }

        // Re-spawn synthesis pipelines that were in flight when this
        // process last shut down. Catches two shapes — execution
        // already terminal on disk with `synthesis_pending = true`,
        // OR event log has a terminal event that never got applied to
        // execution.json. Both end up re-running the spawn pipeline
        // from `persist_execution_outcome`'s terminal branch.
        // See `ArtifactV2Service::reconcile_stale_synthesis_at_startup`.
        {
            let service = Arc::clone(&shared_artifact_v2_service);
            // No sweep-wide timeout: the sweep budgets each task itself and
            // is idempotent. A 30 s wrap here dropped the future mid-walk on
            // every busy boot, leaving each task after the cut stranded.
            tokio::spawn(async move {
                if !magician::magician_v2::runtime::startup::wait_for_http().await {
                    return;
                }
                match service.reconcile_stale_synthesis_at_startup().await {
                    Ok(scheduled) if scheduled > 0 => {
                        info!(
                            scheduled,
                            "[STARTUP] Re-spawned in-flight synthesis pipelines"
                        );
                    },
                    Ok(_) => {},
                    Err(error) => {
                        warn!(
                            error = %error,
                            "[STARTUP] Failed to reconcile in-flight synthesis pipelines"
                        );
                    },
                }
            });
        }

        // Task-card/list summaries are auxiliary: answer-ready state does not
        // wait for them. Recover any missing/stale output revisions separately
        // and let the sidecar fingerprint provide the durable idempotency gate.
        {
            let service = Arc::clone(&shared_artifact_v2_service);
            tokio::spawn(async move {
                if !magician::magician_v2::runtime::startup::wait_for_http().await {
                    return;
                }
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    service.reconcile_stale_task_summaries_at_startup(),
                )
                .await
                {
                    Ok(Ok(scheduled)) if scheduled > 0 => info!(
                        scheduled,
                        "[STARTUP] Re-spawned auxiliary task-summary revisions"
                    ),
                    Ok(Ok(_)) => {},
                    Ok(Err(error)) => warn!(
                        error = %error,
                        "[STARTUP] Failed to reconcile auxiliary task summaries"
                    ),
                    Err(_) => warn!(
                        "[STARTUP] Timed out reconciling auxiliary task summaries; backend startup continues"
                    ),
                }
            });
        }

        // LLM Calls Overview demo dashboard — boot-time seed.
        //
        // Idempotently keeps the canonical route backed by a renderable
        // published surface. If the active surface is missing/unrenderable, it
        // mints or reuses a system-owned seed task, writes the MUI-JSON
        // dashboard output, publishes it, then deletes the seed task once the
        // surface is durable. The published surface is the user-facing
        // artifact. See `magician_v2::dashboard_seed` for the payload, route,
        // and canonical id contract.
        let dashboard_seed_service = Arc::clone(&shared_artifact_v2_service);
        deferred_startup_tasks.push(tokio::spawn(async move {
            let Some(_permit) = magician::magician_v2::runtime::startup::admit_backfill().await else { return; };
            let shared_artifact_v2_service = dashboard_seed_service;
            use magician::magician_v2::artifact_v2::{
                ScopeRef, WriteUserOutputBody, WriteUserOutputDirectInput,
            };
            use magician::magician_v2::dashboard_seed;

            let principal = "anonymous";
            let workspace = "default";
            let seed_task_id = dashboard_seed::seed_task_id(principal, workspace);
            let scope = ScopeRef::system_internal_unauthenticated(
                &principal.to_string(),
                &workspace.to_string(),
            );
            let surface_ready = match shared_artifact_v2_service
                .active_published_surface_can_render(&scope, dashboard_seed::SEED_ROUTE)
                .await
            {
                Ok(ready) => ready,
                Err(error) => {
                    warn!(
                        error = %error,
                        route = dashboard_seed::SEED_ROUTE,
                        "[STARTUP] Failed to probe LLM Overview published surface readiness"
                    );
                    false
                },
            };

            let existing_seed_scope = match shared_artifact_v2_service
                .find_task_scope(&seed_task_id)
                .await
            {
                Ok(found) => found,
                Err(error) => {
                    warn!(
                        error = %error,
                        seed_task_id = %seed_task_id,
                        "[STARTUP] Failed to probe LLM Overview seed task; skipping seed publish"
                    );
                    Some(scope.clone())
                },
            };

            if let Some(existing_scope) = existing_seed_scope.as_ref() {
                if surface_ready {
                    match shared_artifact_v2_service
                        .delete_completed_internal_task_if_safe(existing_scope, &seed_task_id)
                        .await
                    {
                        Ok(report) if report.deleted => info!(
                            seed_task_id = %seed_task_id,
                            "[STARTUP] Deleted completed LLM Overview seed task; published surface is durable"
                        ),
                        Ok(_) => {},
                        Err(error) => warn!(
                            error = %error,
                            seed_task_id = %seed_task_id,
                            "[STARTUP] Failed to delete completed LLM Overview seed task"
                        ),
                    }
                }
            }

            if !surface_ready {
                let title = "LLM Calls Overview".to_string();
                let description =
                    "Auto-generated overview dashboard for LLM call telemetry (Parquet-backed)."
                        .to_string();
                let ui_thread_id = "system:llm-overview-thread".to_string();
                let agent_id = "__system__".to_string();
                match shared_artifact_v2_service
                    .ensure_seed_task(
                        &scope,
                        &seed_task_id,
                        &title,
                        &description,
                        &agent_id,
                        &ui_thread_id,
                    )
                    .await
                {
                    Ok(_) => {
                        let dashboard_body = dashboard_seed::build_llm_overview_dashboard();
                        let write_result = shared_artifact_v2_service
                            .write_user_output_direct(
                                &scope,
                                &seed_task_id,
                                WriteUserOutputDirectInput {
                                    media_type: "application/json".to_string(),
                                    body: WriteUserOutputBody::Json(dashboard_body),
                                    dashboard_theme: Some("editorial".to_string()),
                                },
                            )
                            .await;
                        match write_result {
                            Ok(output_ref) => {
                                match magician::magician_v2::artifact_v2::models::build_dashboard_publish_input(
                                    &seed_task_id,
                                    workspace,
                                    &ui_thread_id,
                                    "workspace",
                                    true,
                                    Some(dashboard_seed::SEED_ROUTE.to_string()),
                                    Some(title.clone()),
                                    Some(
                                        "Live LLM call analytics across the workspace.".to_string(),
                                    ),
                                    Some(output_ref.output_id.clone()),
                                ) {
                                    Ok(publish_input) => match shared_artifact_v2_service
                                        .publish_surface_record(&scope, publish_input)
                                        .await
                                    {
                                        Ok(record) => {
                                            info!(
                                                seed_task_id = %seed_task_id,
                                                surface_id = %record.surface_id,
                                                route = %record.route,
                                                "[STARTUP] LLM Overview seed dashboard published"
                                            );
                                            match shared_artifact_v2_service
                                                .delete_completed_internal_task_if_safe(
                                                    &scope,
                                                    &seed_task_id,
                                                )
                                                .await
                                            {
                                                Ok(report) if report.deleted => info!(
                                                    seed_task_id = %seed_task_id,
                                                    surface_id = %record.surface_id,
                                                    "[STARTUP] Deleted completed LLM Overview seed task after durable publish"
                                                ),
                                                Ok(_) => {},
                                                Err(error) => warn!(
                                                    error = %error,
                                                    seed_task_id = %seed_task_id,
                                                    surface_id = %record.surface_id,
                                                    "[STARTUP] Failed to delete completed LLM Overview seed task after publish"
                                                ),
                                            }
                                        },
                                        Err(error) => warn!(
                                            error = %error,
                                            seed_task_id = %seed_task_id,
                                            "[STARTUP] Failed to publish LLM Overview seed dashboard"
                                        ),
                                    },
                                    Err(error) => warn!(
                                        error = %error,
                                        seed_task_id = %seed_task_id,
                                        "[STARTUP] Failed to build LLM Overview publish input"
                                    ),
                                }
                            },
                            Err(error) => warn!(
                                error = %error,
                                seed_task_id = %seed_task_id,
                                "[STARTUP] Failed to write LLM Overview seed user-output"
                            ),
                        }
                    },
                    Err(error) => warn!(
                        error = %error,
                        seed_task_id = %seed_task_id,
                        "[STARTUP] Failed to mint LLM Overview seed task"
                    ),
                }
            }
        }));

        // Belt-and-braces prune of voice session lifecycle entries.
        // The control-WS actor's `stopped()` hook calls
        // `orchestrator.end()` which removes the entry on clean
        // disconnect — this tick exists for the unclean cases:
        // browser crash, network failure with no graceful teardown,
        // process-level WS leak. 30-min idle threshold is well past
        // any normal call's gaps; tick every 5 min so eviction lag
        // stays bounded.
        {
            let lifecycle_store = shared_media_api.get_ref().lifecycle_store();
            tokio::spawn(async move {
                const PRUNE_INTERVAL: Duration = Duration::from_secs(5 * 60);
                const MAX_IDLE_MS: i64 = 30 * 60 * 1000;
                let mut ticker = tokio::time::interval(PRUNE_INTERVAL);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    let evicted = lifecycle_store
                        .prune_idle(chrono::Utc::now().timestamp_millis(), MAX_IDLE_MS);
                    if evicted > 0 {
                        info!(
                            evicted = evicted,
                            "[VOICE-LIFECYCLE] pruned idle voice session entries"
                        );
                    }
                }
            });
        }

        shared_agent_api
            .runtime
            .set_artifact_v2_service(Arc::clone(&shared_artifact_v2_service));
        // Built once and reused: the base registry gets the harness read
        // providers here, and the SAME services are late-bound onto the scoped
        // capability resolver below (see `set_harness_services`) so per-scope
        // registries — which `build_compiled_registry` builds from pack-defs
        // only and without `HarnessServices` — also bind the harness read
        // providers. Without the resolver binding, harness agents' auto-granted
        // introspection tools (`list_episodes`, `magician_work_ledger`, …) fail
        // scoped dispatch with "is not a compiled pack".
        let harness_read_services = magician::magician_v2::harness::HarnessServices {
            definition_store: Arc::clone(&shared_agent_api.definition_store),
            memory_resolver: AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
            artifact_service: Arc::clone(&shared_artifact_v2_service),
            runtime: shared_agent_api.runtime(),
            user_request_service: None,
            control_gate: None,
            scoped_paused_agents: Some(shared_agent_api.paused_agents_index()),
        };
        register_harness_read_providers(
            orchestrator.capability_registry(),
            harness_read_services.clone(),
        );
        {
            use magician::magician_v2::surfaces::auto_publisher::AutoSurfacePublisher;

            let publisher = AutoSurfacePublisher::new()
                .with_publication_workspace(storage_workspace.clone())
                .with_publication_service(Arc::clone(&shared_artifact_v2_service))
                .with_event_broadcaster(Arc::clone(&event_broadcaster));
            orchestrator.set_auto_surface_publisher(Arc::new(publisher));
            info!("[STARTUP] AutoSurfacePublisher wired to orchestrator");
        }
        if let Some(existing_dispatcher) = orchestrator.get_delegation_dispatcher() {
            let v3_dispatcher = Arc::new(V3DelegationDispatcher::new(
                existing_dispatcher,
                Arc::clone(&shared_agent_api.runtime),
                Arc::clone(&orchestrator),
                Arc::clone(&shared_artifact_v2_service),
            ));
            // The verification controller dispatches repair through
            // delegation: a held task still owns a live root execution, so
            // starting a second root execution on it is refused. Wired here
            // rather than passed to the constructor because the dispatcher
            // holds the service — see `set_delegation_dispatcher`.
            shared_artifact_v2_service.set_delegation_dispatcher(v3_dispatcher.clone());
            orchestrator.set_delegation_dispatcher(v3_dispatcher);
        }

        // Create thread service early so chat and thread routes can share it.
        let ui_thread_store = magician::magician_v2::ui_threads::UiThreadStore::open_workspace(
            storage_workspace.clone(),
        )
        .context("Failed to open UI thread store for chat service")?;
        let shared_storage_governance_service = web::Data::new(
            magician_comms::channel_assist::governance::StorageGovernanceService::new(
                storage_workspace.clone(),
                channel_assist_store.clone(),
                ui_thread_store.clone(),
                Arc::clone(&social_store),
            )
            .with_feed_store(feed_store.clone())
            .with_attention_learning(attention_learning_service.clone())
            .with_llm_content_settings(llm_content_settings.clone()),
        );
        let ui_thread_store_for_startup_materialize = ui_thread_store.clone();
        tokio::spawn(async move {
            let Some(_startup_permit) =
                magician::magician_v2::runtime::startup::admit_backfill().await
            else {
                return;
            };
            match tokio::time::timeout(
                Duration::from_secs(30),
                ui_thread_store_for_startup_materialize.materialize_existing_scopes(),
            )
            .await
            {
                Ok(Ok(())) => {},
                Ok(Err(error)) => warn!(
                    error = %error,
                    "[STARTUP] Failed to materialize scoped UI thread DuckDB templates"
                ),
                Err(_) => warn!(
                    "[STARTUP] Timed out materializing scoped UI thread DuckDB templates; backend startup continues"
                ),
            }
        });
        let ui_thread_service_arc =
            Arc::new(magician::magician_v2::ui_threads::UiThreadService::new(
                ui_thread_store,
                Arc::clone(&shared_artifact_v2_service),
                Arc::clone(&chat_store) as Arc<dyn magician::magician_v2::chat::storage::ChatStore>,
                chat_memory_resolver.clone(),
            ));

        // Create shared Memory API service with scoped V3 memory resolution.
        let memory_api = web::Data::new(MemoryApi::new(chat_memory_resolver.clone()));

        // Build the scoped capability resolver up front so we can both hand
        // it to `ChatService` AND keep a handle to wire the agent backend
        // after `ChatService` is wrapped in `Arc` below. The bridge slot uses
        // interior mutability (`Arc<StdRwLock<...>>`), so cloning the resolver
        // here is cheap and shares the same bridge cell with every downstream
        // `registry_for_scope` call.
        let chat_scoped_capability_resolver = orchestrator.build_scoped_capability_resolver();
        chat_scoped_capability_resolver.set_computed_capability_overlay(
            shared_app_platform_api
                .get_ref()
                .computed_capability_overlay(),
        );
        shared_app_platform_api
            .get_ref()
            .set_scoped_capability_resolver(Arc::clone(&chat_scoped_capability_resolver));
        chat_scoped_capability_resolver.configure_cache(
            magician_config
                .agent_surface_runtime
                .cache
                .max_schema_indexes,
            magician_config.agent_surface_runtime.cache.idle_ttl_seconds,
            magician_config.agent_surface_runtime.cache.singleflight,
        );
        orchestrator.configure_agent_surface_runtime(magician_config.agent_surface_runtime.clone());
        let (surface_plan_cache, surface_working_sets) =
            orchestrator.agent_surface_runtime_stores();

        // Resource Authority Layer 2 — build the runtime arcs from the
        // declarative YAML config. While `magician_config.resource_authority.enabled`
        // is `false` (default), `compiled_dispatch::execute_maybe_gated`'s
        // degraded fallback runs the inner action without ledger
        // bookkeeping — existing deployments see no behaviour change.
        // Flipping `enabled: true` activates real spend-gated dispatch
        // through reserve/commit/rollback. See
        // `docs/archive/plans/2026-05-20-resource-authority-layer-2.md`.
        if let Err(error) = magician_config.resource_authority.validate() {
            warn!(
                error = %error,
                "[STARTUP] resource_authority config failed validation; \
                 chat gated dispatch will fall back to degraded mode"
            );
        }
        // The scope resolver is built once at outer scope (see
        // `scoped_authority_resolver` above) so both this dispatch
        // authority and the REST API surface share the same per-
        // (principal, workspace) bundles. Clone the `Arc` here.
        let chat_resource_authority =
            magician::magician_v2::execution::compiled_dispatch::CompiledDispatchAuthority::from_resolver(
                Arc::clone(&scoped_authority_resolver),
            );
        // Phase 5 of the compiled-dispatch refactor: hand the same
        // bundle to the orchestrator so the autonomous outer loop's
        // inner-loop `CompiledProviderDispatcher` charges the same
        // spend ledger chat does. Boot wiring is one call; orchestrator
        // forwards the bundle into every `ActionExecutors` it builds.
        // See `docs/archive/plans/2026-05-20-compiled-dispatch-extraction.md`.
        orchestrator.set_compiled_dispatch_authority(chat_resource_authority.clone());
        let mut chat_service = ChatService::new(
            Arc::clone(&chat_store),
            llm_service,
            Arc::clone(&prompt_manager),
            Arc::clone(&event_broadcaster),
        )
        .with_persona_provider(persona_provider)
        .with_memory_resolver(chat_memory_resolver.clone())
        .with_artifact_v2_service(Arc::clone(&shared_artifact_v2_service))
        .with_capability_workspace(capability_workspace.clone())
        .with_definition_store(Arc::clone(&shared_agent_api.definition_store))
        .with_progress_sync(chat_progress_sync)
        .with_agentic_runtime(ChatAgenticRuntime::new(
            Arc::clone(&shared_agent_api.runtime),
            progress_router.clone(),
            Arc::clone(&event_broadcaster),
        ))
        .with_capability_registry(orchestrator.capability_registry().clone())
        .with_scoped_capability_resolver(Arc::clone(&chat_scoped_capability_resolver))
        .with_agent_surface_runtime_config(magician_config.agent_surface_runtime.clone())
        .with_agent_surface_runtime_stores(surface_plan_cache, surface_working_sets)
        .with_public_chat_config(magician_config.public_chat.clone())
        .with_public_contact_profile_store(Arc::clone(&public_contact_profile_store))
        .with_resource_authority(chat_resource_authority.clone())
        .with_ui_thread_service(Arc::clone(&ui_thread_service_arc))
        .with_user_request_service(Arc::clone(&user_request_service))
        .with_thread_checker(Arc::new((*ui_thread_service_arc).clone())
            as Arc<dyn magician::magician_v2::chat::service::ThreadArchivedChecker>)
        // The ambassador a shared room binds to. Same value guest routing
        // uses, so the outward agent is named once rather than twice.
        .with_room_agent_id(magician_config.envoy.envoy_agent_id.clone());
        if let Some(router) = orchestrator.operation_llm_router() {
            chat_service = chat_service.with_operation_llm_router(router);
        }

        let execution_panel_runtime_store = {
            let runtime_store = ExecutionPanelRuntimeStore::new();
            runtime_store.clone().start(Arc::clone(&event_broadcaster));
            runtime_store
        };
        let execution_panel_event_log = EventLog::with_workspace_layout(storage_workspace.clone())
            .await
            .context("Failed to create execution panel event log")?;
        let execution_panel_adapter =
            V3ExecutionPanelAdapter::new(Arc::clone(&shared_artifact_v2_service))
                .with_runtime_support(
                    orchestrator.screenshot_storage(),
                    Arc::clone(&ask_loop_api),
                    execution_panel_runtime_store.clone(),
                )
                .with_pause_store(Arc::clone(&shared_pause_store))
                .with_progress_event_log(execution_panel_event_log.clone());
        ExecutionPanelProjector::new(
            execution_panel_adapter,
            Arc::clone(&event_broadcaster),
            progress_router.progress_stream(),
        )
        .start();
        // Voice-context compactor — needs chat_store, the
        // VoiceSessionLifecycleStore (lives on shared_media_api), the
        // OperationLlmRouter (optional, falls back to no-op summary
        // when unconfigured), and PromptManager. Constructed here
        // where all the pieces are in scope; surfaced as web::Data so
        // the resume-context handler can render replay payloads.
        let chat_store_dyn: Arc<dyn magician::magician_v2::chat::storage::ChatStore> =
            Arc::clone(&chat_store) as Arc<dyn magician::magician_v2::chat::storage::ChatStore>;
        let voice_compactor_router = match orchestrator.operation_llm_router() {
            Some(router) => (*router).clone(),
            None => {
                // No router config means the LLM-driven paths (chat,
                // voice compaction, all of query_analysis) won't be
                // able to call models. We build an empty router so
                // the type plumbing still wires up; every dependent
                // call surfaces a `NotConfigured` error at runtime.
                // Warn loudly so this isn't silent in production.
                warn!(
                    "[STARTUP] OperationLlmRouter is not configured — voice compaction \
                     + LLM-driven operations will fail with NotConfigured. \
                     Check magician-config.yaml > query_analysis_v2.router."
                );
                magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter::new(
                    None,
                )
            },
        };
        // Publish THE router as the process-global handle the media rails
        // use (their sessions are built in process-global registries with
        // no app_data in scope) — screen narration / understanding and the
        // meeting/observation summarizer route through magician-config.yaml
        // operations + profiles like every other LLM call.
        magician::magician_v2::query_analysis::operation_llm_router::set_global_operation_router(
            std::sync::Arc::new(voice_compactor_router.clone()),
        );
        // Pull compactor knobs from the default realtime voice
        // profile so they live in YAML instead of being baked into
        // Rust consts. Falls back to the compactor defaults when
        // either field is absent.
        let (verbatim_recent_turns, compaction_input_turn_limit) = voice_compactor_router
            .realtime_voice_profile(
                &magician::magician_v2::query_analysis::operation_llm_router::LLMOperation::VoiceController,
            )
            .map(|p| (p.verbatim_recent_turns, p.compaction_input_turn_limit))
            .unwrap_or((None, None));
        let voice_context_compactor = Arc::new(
            magician_media::media_rails::VoiceContextCompactor::new(
                Arc::clone(&chat_store_dyn),
                shared_media_api.get_ref().lifecycle_store(),
                voice_compactor_router.clone(),
                Arc::clone(&prompt_manager),
            )
            .with_broadcaster(Arc::clone(&event_broadcaster))
            .with_profile_overrides(verbatim_recent_turns, compaction_input_turn_limit),
        );
        let chat_api = ChatApi::new(chat_service)
            .with_enrollment_store_resolver(enrollment_store_resolver.clone())
            .with_envoy_config(magician_config.envoy.clone());
        // Phase 0.8c — install the neutral `AgentResources` bundle that
        // compiled providers hold an `Arc` to. All migrated tools reach
        // their deps (memory_resolver, agent_definition_store,
        // artifact_v2_service) through this struct via the
        // `GenericCompiledProvider` handler pattern. The
        // `AgentBackend` trait + per-tool bridge install has been
        // deleted (Phase 0.8c-8 cleanup).
        // Global file sandbox for compiled handlers (e.g. `read_file`). The
        // config default allows only `.`/`/tmp`, but durable artifacts live
        // under `<base_root>/scopes/<p>/<w>/durable_artifacts/...`. Augment the
        // allowed roots with the scopes tree so the global-resources path stays
        // in lockstep with the native-action path
        // (`v2_orchestrator::build_action_executors_with_router_override`).
        let global_file_sandbox = {
            let mut sandbox = magician_config.execution.file_sandbox.clone();
            sandbox.augment_with_scopes_root(storage_workspace.base_root());
            sandbox
        };
        let content_acquisition_resolver_slot = Arc::new(std::sync::RwLock::new(None));
        let agent_resources = Arc::new(
            magician::magician_v2::execution::agent_resources::AgentResources {
                magician_config: Arc::new(std::sync::RwLock::new(magician_config.clone())),
                memory_resolver: Arc::new(chat_memory_resolver.clone()),
                agent_definition_store: Arc::clone(&shared_agent_api.definition_store),
                artifact_workspace: shared_artifact_v2_service.workspace().clone(),
                artifact_v2_service: Some(Arc::clone(&shared_artifact_v2_service)),
                event_broadcaster: Some(Arc::clone(&event_broadcaster)),
                operation_llm_router: orchestrator.operation_llm_router(),
                secret_store_resolver: Some(Arc::clone(orchestrator.secret_store_resolver())),
                content_acquisition_resolver: Arc::clone(&content_acquisition_resolver_slot),
                file_sandbox: global_file_sandbox,
                // Flat-loop tool index — populated lazily at registry-build
                // time (`build_compiled_registry`) once the per-scope pack
                // list is resolved. Starts empty.
                tool_index: Arc::new(std::sync::OnceLock::new()),
                // Owner-relay compiled handlers reach ask() through this.
                user_request_service: Some(Arc::clone(&user_request_service)),
                // Officer mobilization from compiled handlers (CEO P1.5).
                agent_runtime: Some(Arc::clone(&shared_agent_api.runtime)),
            },
        );
        // App behaviors dispatch their reviewed model turns on the permit-gated
        // `app:` route, which re-reads operator admission and the router
        // mapping live — including mid-request, inside the disclosure
        // authorizer. Hand the artifact service the SAME config handle the
        // compiled providers hold so a reload reaches both; without it every
        // behavior-bound turn refuses, because that lane has no generic
        // fallback by design.
        shared_artifact_v2_service
            .set_app_llm_config_authority(Arc::clone(&agent_resources.magician_config));
        // The /meetings API builds the same scope-aware meeting pieces the
        // compiled `meeting` tool does (memory writer, browser joiner, chat-lane
        // scope), so it needs the resources as actix app-data too.
        let shared_agent_resources_data = web::Data::new(Arc::clone(&agent_resources));
        chat_scoped_capability_resolver.set_agent_resources(Arc::clone(&agent_resources));

        // Phase 0.8c — install the compiled-handler registry. Empty
        // at the start of the migration; each tool migration adds
        // one line to `default_compiled_handler_registry()`.
        let compiled_handlers = Arc::new(
            magician::magician_v2::execution::compiled_providers::default_compiled_handler_registry(
            ),
        );
        chat_scoped_capability_resolver.set_compiled_handlers(compiled_handlers);
        // Late-bind the harness READ services (same instance that bound the
        // base registry above) so every per-scope registry built by
        // `registry_for_scope` re-binds the harness introspection providers
        // (`list_episodes`, `magician_work_ledger`, …) whose pack-defs the scope
        // embeds. This is the resolver shared with the orchestrator below, so
        // chat + direct/autonomous scoped dispatch all pick it up.
        chat_scoped_capability_resolver.set_harness_services(harness_read_services);
        shared_skills_api
            .get_ref()
            .set_scoped_capability_resolver(Arc::clone(&chat_scoped_capability_resolver));
        shared_api_mining_api
            .get_ref()
            .set_scoped_capability_resolver(Arc::clone(&chat_scoped_capability_resolver));
        // Share the fully-wired resolver (compiled-handler registry +
        // AgentResources, just installed above) with the orchestrator so
        // direct/autonomous executions reuse it instead of the executor
        // factory's bare resolver. Without this, handler-backed universals
        // (`tool_search`, `edit_file`, `read_file`, `grep`, …) have no provider
        // in the scoped dispatch registry and fail with "not a compiled pack".
        orchestrator.set_scoped_capability_resolver(Arc::clone(&chat_scoped_capability_resolver));
        let _api_mining_recipe_verification_worker =
            magician::magician_v2::api_mining::recipe_verification::spawn_if_enabled(
                storage_workspace.clone(),
                Arc::clone(orchestrator.secret_store_resolver()),
                orchestrator.api_mining_switch().clone(),
                Arc::clone(&chat_scoped_capability_resolver),
                magician_config.api_mining.clone(),
            );
        let content_acquisition_resolver = Arc::new(
            magician::magician_v2::content_sources::ContentAcquisitionResolver::new(
                Arc::clone(&chat_scoped_capability_resolver),
                Arc::clone(&capability_workspace),
                chat_resource_authority.clone(),
                Arc::clone(orchestrator.secret_store_resolver()),
                magician_config.content_acquisition.clone(),
                magician_config.api_mining.clone(),
            ),
        );
        match content_acquisition_resolver_slot.write() {
            Ok(mut slot) => *slot = Some(Arc::clone(&content_acquisition_resolver)),
            Err(poisoned) => {
                *poisoned.into_inner() = Some(Arc::clone(&content_acquisition_resolver));
            },
        }
        shared_skills_api
            .get_ref()
            .set_content_acquisition_resolver(Arc::clone(&content_acquisition_resolver));
        orchestrator.set_content_acquisition_resolver(Arc::clone(&content_acquisition_resolver));
        let shared_content_acquisition_resolver = web::Data::new(content_acquisition_resolver);
        // Start only after every late-bound capability/runtime dependency is
        // visible. Durable due heads may run immediately after spawn, so an
        // earlier start could race compiled-handler or acquisition wiring.
        // Disabled configuration leaves schedule/event execution and its
        // operator surface closed; the same process task still drains durable
        // one-way owner notifications accepted by foreground app workflows.
        let app_background_behaviors = magician_config.app_platform.background_behaviors;
        // `None` is never "nothing to start": the same supervisor owns the
        // always-on notification and terminal-retention lanes named above, so a
        // `None` means only that this process failed to take ownership — an
        // armed component that could not open, or a latch already claimed.
        // Booting past it would strand debt a foreground workflow already
        // accepted, and when armed would report unattended execution as on
        // while nothing could ever fire. A guard that cannot prove its
        // precondition refuses.
        let Some(_app_background_behavior_worker) = shared_app_platform_api
            .spawn_background_behavior_worker(
                Arc::clone(&agent_resources),
                Arc::clone(&user_request_service),
                app_background_behaviors,
            )
        else {
            bail!(
                "app background-behavior supervisor did not take ownership \
                 (app_platform.background_behaviors.enabled = {}); refusing to boot \
                 with app owner-notification debt unowned",
                app_background_behaviors.enabled
            );
        };
        // Say the posture out loud at boot. Arming is a deployment-level
        // decision whose only visible consequence is work happening with nobody
        // present, so an operator must be able to read it off the log rather
        // than infer it from behavior — including the one move that rolls it
        // back.
        if app_background_behaviors.enabled {
            warn!(
                tick_interval_seconds = app_background_behaviors.tick_interval_seconds,
                max_installations_per_scope = app_background_behaviors.max_installations_per_scope,
                max_claims_per_scope_tick = app_background_behaviors.max_claims_per_scope_tick,
                "[STARTUP] app background behaviors ARMED — unattended \
                 schedule/event execution is live; grant revocation, per-scope \
                 pause and operation admission each still stop a behavior, and \
                 setting app_platform.background_behaviors.enabled = false then \
                 restarting rolls the whole lane back"
            );
        } else {
            info!(
                "[STARTUP] app background behaviors disarmed — \
                 owner-notification delivery only; set \
                 app_platform.background_behaviors.enabled = true to arm \
                 unattended schedule/event execution"
            );
        }
        // Surface the `Arc<ChatService>` so the voice control WS can plumb it
        // into VoiceOrchestrator (voice-as-chat-agent dispatches realtime tool
        // calls through ChatService's existing dispatcher — see
        // `docs/archive/plans/2026-05-19-voice-as-chat-agent.md`).
        let chat_service_for_voice = Arc::clone(&chat_api.chat_service);
        (
            web::Data::new(chat_api),
            memory_api,
            web::Data::new(Arc::new(ProgressChannelApi::new(progress_router.clone()))),
            web::Data::new(Arc::new(
                FeedApi::new(feed_store.clone(), Arc::clone(&shared_artifact_v2_service))
                    .with_attention_store(attention_funnel_store.clone())
                    .with_pending_hitl_broadcaster(Arc::clone(&event_broadcaster)),
            )),
            web::Data::new(Arc::new(
                magician_api::fleet_state_api::FleetStateApi::new(storage_workspace.clone())
                    .with_definition_store(Arc::clone(&shared_agent_api.definition_store))
                    .with_task_service(Arc::clone(&shared_artifact_v2_service))
                    .with_feed_store(feed_store.clone())
                    .with_authority_resolver(Arc::clone(&scoped_authority_resolver))
                    .with_square((**shared_social_api).clone()),
            )),
            web::Data::new(Arc::new(ExecutionPanelApi::new(
                Arc::clone(&shared_artifact_v2_service),
                orchestrator.screenshot_storage(),
                Arc::clone(&ask_loop_api),
                execution_panel_runtime_store,
                Arc::clone(&shared_pause_store),
                execution_panel_event_log,
            ))),
            web::Data::new(Arc::new(UiThreadApi::new((*ui_thread_service_arc).clone()))),
            shared_storage_governance_service,
            progress_router,
            web::Data::new(TaskApiV3::from_service(Arc::clone(
                &shared_artifact_v2_service,
            ))),
            chat_turn_event_sink,
            shared_prompt_manager,
            web::Data::new(Arc::clone(&voice_context_compactor)),
            web::Data::new(Arc::clone(&chat_store_dyn)),
            // Surface the operation router + event broadcaster so the
            // voice control WS actor (R4) can compose its per-call
            // VoiceOrchestrator without re-resolving these from
            // global state.
            web::Data::new(voice_compactor_router.clone()),
            web::Data::new(Arc::clone(&event_broadcaster)),
            web::Data::new(chat_service_for_voice),
            shared_dispatch_queue,
            shared_cancel_bridge,
            shared_agent_resources_data,
            shared_social_api,
            shared_content_acquisition_resolver,
        )
    };

    // Phase 10 Observable Sources: exact-action source offers, scoped durable
    // subscriptions, and the deterministic polling scheduler share the same
    // scope-aware acquisition resolver used by research tools.
    // Hybrid interest filter: give the Observe runtime a semantic scorer backed
    // by the SAME shared Ollama embedder the resurfacing centrality path uses
    // (`ollama_lifecycle::embedder()`), with the resurfacing centrality query
    // timeout as the per-batch embed budget. When the embedder isn't available
    // the runtime keeps its keyword-only default (`NoSemanticIntent`).
    let mut observable_runtime =
        magician::magician_v2::content_sources::ObservableSourceRuntime::new(
            storage_workspace.clone(),
            Arc::clone(shared_content_acquisition_resolver.get_ref()),
            magician_config.content_acquisition.observe.clone(),
        )?
        .with_resurfacing_sink(
            std::sync::Arc::new(resurfacing_store.clone())
                as std::sync::Arc<dyn magician::magician_v2::resurfacing_seam::ResurfacingSink>,
            resurfacing_worker.wake_handle(),
        )
        .with_startup_catch_up(Arc::clone(&observe_catch_up_controller));
    if let Some(embedder) = magician::magician_v2::runtime::ollama_lifecycle::embedder() {
        observable_runtime = observable_runtime.with_embedding_intent_scorer(
            embedder,
            std::time::Duration::from_secs(
                magician_config
                    .resurfacing
                    .centrality_query_timeout_secs
                    .max(1),
            ),
        );
    }
    let observable_source_runtime = Arc::new(observable_runtime);
    let _observable_source_scheduler = observable_source_runtime.start_scheduler();
    let shared_observable_source_runtime = web::Data::new(observable_source_runtime);
    let shared_observe_catch_up_controller =
        web::Data::new(Arc::clone(&observe_catch_up_controller));

    // The request service participates in execution composition and recovery.
    // Install it before opening the lifecycle latch so startup re-admission
    // cannot race ahead with a constructor-default dependency. The shared
    // Actix wrapper remains in scope for server wiring below.
    orchestrator.set_user_request_service(Arc::clone(&user_request_service));
    let shared_user_request_service = web::Data::new(Arc::clone(&user_request_service));

    // All execution composition is now installed: Artifact/runtime services,
    // verification and app authority, scoped resources, and the delegation
    // dispatcher, including the user-request service. Open the persistent latch
    // only here so background lifecycle owners and startup recovery cannot
    // re-admit work under constructor defaults.
    deferred_startup_tasks.push(tokio::spawn(async move {
        if magician::magician_v2::runtime::startup::wait_for_http().await {
            stateless_loop_lifecycle_ready.cancel();
        }
    }));

    // Hydration re-arms scoped autonomous goals, and an already-due goal fires
    // immediately into the agent runtime. It therefore belongs behind the same
    // latch as every other lifecycle owner: run before this point it observed
    // `artifact_v2_service` unset and failed each due cycle with
    // "artifact_v2_service not wired for V3 cycle bootstrap", which
    // `record_failed_goal_cycle` then burned rather than deferred — one lost
    // cycle per harness goal per scope, on every boot. Nothing between the old
    // call site and here can depend on it: it was spawned, never awaited to
    // completion, so it only ever guaranteed that the task had started.
    spawn_agent_services_startup_hydration(shared_agent_api.clone()).await;

    // Ordered startup recovery for executions and chat-inline delegate cards.
    //
    // Sweep chat projections once immediately for already-terminal orphans.
    // Then re-admit executions left non-terminal (`Runnable`/`Executing`) by a
    // previous process that died mid-flight. Artifact-backed roots rebuild
    // their exact scoped executors and sealed app authority through the
    // canonical Artifact lifecycle; legacy planning runs require a durable
    // PlanGraph. Only rows whose durable composition is provably absent are
    // fail-marked, while transient and separately-owned rows remain runnable.
    // The terminal projector begins its first bounded page from the same ready
    // latch but does not block this sweep: canonical/runtime ACKs have bounded
    // multi-minute deadlines. Safety comes from the typed per-base authority,
    // which classifies every unacknowledged terminal receipt as
    // `SettlementPending`; generic recovery therefore defers that exact runtime
    // until this independent projector advances its durable receipt cursors.
    // A second projection sweep runs after all jobs admitted by this pass have
    // settled.
    // This preserves prompt startup repair without a guessed sleep and closes
    // the race where the one sweep observed a recovered task as still running.
    // The chain stays off the server-bind path and remains idempotent.
    let stateless_startup_recovery_task = {
        let orchestrator_for_reconcile = Arc::clone(&orchestrator);
        let chat_service_for_sweep = Arc::clone(&shared_chat_api.chat_service);
        let artifact_service_for_reconcile = Arc::clone(&shared_artifact_v2_service);
        let scopes = shared_artifact_v2_service.workspace().list_scopes();
        let shutdown = supervisor_shutdown.clone();
        tokio::spawn(async move {
            if !magician::magician_v2::runtime::startup::wait_for_http().await {
                return;
            }
            let runtime_shutdown = shutdown.clone();
            let planning_shutdown = shutdown.clone();
            let delegation_cleanup_shutdown = shutdown.clone();
            let delegation_cleanup_orchestrator = Arc::clone(&orchestrator_for_reconcile);
            let runtime_scopes = scopes.clone();
            let delegation_cleanup_scopes = scopes;
            let runtime_owner = async move {
                let mut rediscovery_delay_secs = 30_u64;
                loop {
                    let recovery_pass = async {
                        chat_service_for_sweep
                            .recover_orphan_delegate_terminals()
                            .await;
                        // Runtime discovery goes first and is not serialized behind
                        // even the compact planning catalog lookup.
                        let recovery = orchestrator_for_reconcile
                            .recover_interrupted_runnable_executions(&runtime_scopes)
                            .await;
                        if recovery.scheduled > 0 || recovery.failed_nonrecoverable > 0 {
                            info!(
                            "[STARTUP] Interrupted execution recovery scheduled {}, retained {} deferred, and fail-marked {} provably non-recoverable run(s)",
                            recovery.scheduled,
                            recovery.deferred,
                            recovery.failed_nonrecoverable,
                        );
                        }
                        if recovery.scheduled > 0 {
                            orchestrator_for_reconcile
                                .wait_for_interrupted_execution_recoveries()
                                .await;
                            chat_service_for_sweep
                                .recover_orphan_delegate_terminals()
                                .await;
                        }
                        recovery.deferred > 0
                    };
                    let rediscovery_needed = tokio::select! {
                        _ = runtime_shutdown.cancelled() => return,
                        retry = recovery_pass => retry,
                    };
                    if !rediscovery_needed {
                        runtime_shutdown.cancelled().await;
                        return;
                    }
                    tokio::select! {
                        _ = runtime_shutdown.cancelled() => return,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(rediscovery_delay_secs)) => {},
                    }
                    rediscovery_delay_secs = rediscovery_delay_secs.saturating_mul(2).min(300);
                }
            };
            let delegation_progress_cleanup_owner = async move {
                let mut retry_delay_secs = 30_u64;
                loop {
                    let report = delegation_cleanup_orchestrator
                        .cleanup_stale_delegation_round_progress_at_startup(
                            &delegation_cleanup_scopes,
                            &delegation_cleanup_shutdown,
                        )
                        .await;
                    if report.cancelled {
                        return;
                    }
                    if report.removed > 0 || report.invalid > 0 {
                        info!(
                            inspected = report.inspected,
                            removed = report.removed,
                            retained_live = report.retained_live,
                            invalid = report.invalid,
                            deferred = report.deferred,
                            "[STARTUP] B14 delegation progress cleanup completed bounded streaming pass"
                        );
                    }
                    if report.deferred == 0 {
                        delegation_cleanup_shutdown.cancelled().await;
                        return;
                    }
                    tokio::select! {
                        _ = delegation_cleanup_shutdown.cancelled() => return,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(retry_delay_secs)) => {},
                    }
                    retry_delay_secs = retry_delay_secs.saturating_mul(2).min(300);
                }
            };
            let planning_owner = async move {
                const CLEAN_REDISCOVERY_SECS: u64 = 60;
                let mut error_rediscovery_delay_secs = 30_u64;
                loop {
                    let deferred = tokio::select! {
                        _ = planning_shutdown.cancelled() => return,
                        result = artifact_service_for_reconcile.reconcile_task_planning_at_startup() => {
                            match result {
                                Ok(_) => false,
                                Err(error) => {
                                    warn!(%error, "[STARTUP] Task planning recovery pass deferred");
                                    true
                                },
                            }
                        },
                    };
                    let rediscovery_delay_secs = if deferred {
                        let delay = error_rediscovery_delay_secs;
                        error_rediscovery_delay_secs =
                            error_rediscovery_delay_secs.saturating_mul(2).min(300);
                        delay
                    } else {
                        // Catalog publication and local monitor admission are
                        // separate durable/process-local steps. A peer may
                        // commit the former and die before the latter, so a
                        // clean pass still needs a cheap bounded rescan while
                        // the service is live. Active-monitor keys dedupe work.
                        error_rediscovery_delay_secs = 30;
                        CLEAN_REDISCOVERY_SECS
                    };
                    tokio::select! {
                        _ = planning_shutdown.cancelled() => return,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(rediscovery_delay_secs)) => {},
                    }
                }
            };
            tokio::join!(
                runtime_owner,
                planning_owner,
                delegation_progress_cleanup_owner
            );
        })
    };

    // Background clarification-guardrail sweep (#23). The 600s `TimedOut`
    // guardrail only fired on active user paths, so a partially-answered /
    // stuck clarification batch could pin a plan in `CollectingAnswers` forever.
    // This periodic sweep gives the timeout a chance to breach and drive
    // `cancel_questions` → `ReadyToPlan` → resume. Its retained handle
    // joins the supervisor shutdown set before AskLoop/storage teardown.
    let clarification_guardrail_sweep_task =
        magician::magician_v2::ask_loop::api::spawn_clarification_guardrail_sweep(
            Arc::clone(&ask_loop_api),
            magician::magician_v2::ask_loop::api::CLARIFICATION_GUARDRAIL_SWEEP_INTERVAL,
            supervisor_shutdown.clone(),
        );

    // Startup sweep for interrupted meeting captures. A live capture (attendee
    // bot or passive listener) writes a crash marker at spawn and removes it on
    // clean teardown; a marker that survived into THIS boot means the previous
    // process died mid-capture — its transcript just stops, with no final
    // summary or memory write. The sweep drains every scope's markers and posts
    // an in-thread note so the gap is explained where the transcript lives.
    // Spawned (not awaited) like the delegate sweep above.
    // Taste capture (Slice 2): distil finished chat sessions into directives
    // the owner approves. One worker per scope, because proposals are
    // owner-specific and a single worker would need a principal it has no
    // honest way to choose. Present only when capture is enabled — the service
    // is installed at loader wiring under the same condition, so its absence
    // here means the owner has not opted in.
    if let Some(capture_service) = magician::magician_v2::taste_capture::global_capture_service() {
        let chat_for_capture = Arc::clone(&shared_chat_api.chat_service);
        let workspace_for_capture = shared_artifact_v2_service.workspace().clone();
        let router_for_capture = orchestrator.operation_llm_router();
        let watermark_root = storage_workspace.base_root().join("system");
        // ONE worker, enumerating scopes every sweep. Spawning one per scope
        // from a boot snapshot means a workspace created later never gets
        // capture until the next restart — silently.
        magician::magician_v2::taste_capture::TasteCaptureWorker::new(
            Arc::clone(&capture_service),
            Arc::clone(&chat_for_capture),
            router_for_capture.clone(),
            Arc::new(workspace_for_capture.clone()),
            orchestrator.conversation_store(),
            watermark_root,
        )
        .spawn();
    }

    {
        let chat_service_for_sweep = Arc::clone(&shared_chat_api.chat_service);
        let workspace_for_sweep = shared_artifact_v2_service.workspace().clone();
        tokio::spawn(async move {
            use magician_media::media_rails::meeting::{markers, CAPTURE_MARKER_DIR};
            let scopes = match workspace_for_sweep.list_scope_segments_sync() {
                Ok(scopes) => scopes,
                Err(e) => {
                    tracing::warn!(target: "meet_bot", error = %e, "capture-marker sweep: scope listing failed");
                    return;
                },
            };
            for (principal, workspace) in scopes {
                let dir = workspace_for_sweep
                    .capability_workdirs_root(&principal, &workspace)
                    .join(CAPTURE_MARKER_DIR);
                // Markers are NOT consumed by the drain: each is deleted only
                // once handled (note posted, or provably nothing to post), so a
                // transient failure here is retried on the next boot instead of
                // silently losing the interruption record.
                for (marker, marker_path) in markers::drain_markers(&dir) {
                    // A marker whose session is live in THIS process is not an
                    // interruption — it's a capture that started while the sweep
                    // task was still queued. Leave it for its own teardown.
                    let live_here = magician_media::media_rails::meeting::meeting_manager()
                        .status(&marker.session_id)
                        .await
                        .is_some()
                        || magician_media::media_rails::meeting::passive_meeting_manager()
                            .status(&marker.session_id)
                            .await
                            .is_some();
                    if live_here {
                        continue;
                    }
                    tracing::warn!(
                        target: "meet_bot",
                        session_id = %marker.session_id,
                        mode = %marker.mode,
                        thread = ?marker.thread,
                        "interrupted meeting capture found at boot"
                    );
                    let Some(thread) = marker.thread.as_deref().filter(|t| !t.is_empty()) else {
                        // No thread → nowhere to post; the warn above is the
                        // record and a retry could never do better.
                        markers::remove_marker(&marker_path);
                        continue;
                    };
                    // The capture streamed its transcript into this thread, so a
                    // session exists; post into the newest one. The prefix
                    // lookup with the FULL thread id is an exact-thread match.
                    let sessions = match chat_service_for_sweep
                        .list_sessions_for_thread_prefix(
                            &marker.principal,
                            &marker.workspace,
                            thread,
                        )
                        .await
                    {
                        Ok(sessions) => sessions,
                        Err(e) => {
                            // Keep the marker: a store hiccup is retryable next boot.
                            tracing::warn!(target: "meet_bot", error = %e, "capture-marker sweep: session lookup failed");
                            continue;
                        },
                    };
                    // Newest ACTIVE session only — archived sessions are
                    // immutable (the transcript endpoint's rejection of them is
                    // load-bearing for sink rotation), and "+ New" bumps the
                    // archived session's updated_at, so a bare max_by_key could
                    // pick one.
                    let Some(session) = sessions
                        .into_iter()
                        .filter(|s| {
                            s.status
                                == magician::magician_v2::chat::models::ChatSessionStatus::Active
                        })
                        .max_by_key(|s| s.updated_at)
                    else {
                        tracing::warn!(
                            target: "meet_bot",
                            thread = %thread,
                            "capture-marker sweep: no active session for the thread; dropping the note"
                        );
                        markers::remove_marker(&marker_path);
                        continue;
                    };
                    let what = if marker.mode == "passive" {
                        "The passive listener"
                    } else {
                        "The meeting bot's capture"
                    };
                    let note = format!(
                        "⚠️ {what} was interrupted by a server restart — the transcript above may be incomplete and no final summary was written."
                    );
                    match chat_service_for_sweep
                        .persist_meeting_transcript_line(&session, None, note)
                        .await
                    {
                        Ok(()) => markers::remove_marker(&marker_path),
                        Err(e) => {
                            // Keep the marker: retried on the next boot.
                            tracing::warn!(target: "meet_bot", error = %e, "capture-marker sweep: note post failed");
                        },
                    }
                }
            }
        });
    }

    // `user_request_service` is constructed at outer scope above (ahead of the
    // chat let-tuple block), and was installed before lifecycle recovery was
    // released. Register the same instance with compiled harness handlers.
    register_harness_action_providers(
        orchestrator.capability_registry(),
        magician::magician_v2::harness::HarnessServices {
            definition_store: Arc::clone(&shared_agent_api.definition_store),
            memory_resolver: AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
            artifact_service: Arc::clone(&shared_artifact_v2_service),
            runtime: shared_agent_api.runtime(),
            user_request_service: Some(Arc::clone(&user_request_service)),
            control_gate: Some(shared_agent_api.control_gate()),
            scoped_paused_agents: Some(shared_agent_api.paused_agents_index()),
        },
    );

    let (supervisor_runtime, mut supervisor_tasks) = spawn_agent_supervisor_tasks(
        shared_agent_api.clone(),
        Arc::clone(&orchestrator),
        supervisor_shutdown.clone(),
        runtime_plan.background_worker_threads,
    );
    // These owners are spawned on the main runtime because they are built
    // beside the Artifact service, but they share the supervisor shutdown
    // contract. Retain their handles in the same joined set so canonical sinks
    // and storage are never torn down while a terminal projection or park
    // retirement pass is still writing. A wedged pass is aborted by the same
    // bounded five-second shutdown policy; its lease/cursor protocol leaves
    // the unfinished work durable for the next process.
    supervisor_tasks.push((
        "stateless-startup-interrupted-recovery",
        stateless_startup_recovery_task,
    ));
    supervisor_tasks.push((
        "stateless-terminal-outbox-projector",
        terminal_outbox_projector_task,
    ));
    supervisor_tasks.push((
        "stateless-parked-execution-reconciler",
        parked_execution_reconciler_task,
    ));
    supervisor_tasks.push((
        "clarification-guardrail-sweep",
        clarification_guardrail_sweep_task,
    ));
    info!("Started Phase 3 agent supervisor loops");

    // Run the memory-index maintainer (rebuild/compaction/index-build) on the
    // dedicated `magician-bg` runtime rather than the main request runtime.
    // It was missed in the Task 4 supervisor-sweep move; its lance work must
    // not compete with request-path handlers. The periodic sweep is the
    // backstop for any dirty mark emitted before its sender is registered.
    let _memory_index_maintainer = MemoryIndexMaintainer::spawn(
        storage_workspace.clone(),
        shared_agent_api.definition_store.as_ref().clone(),
        AgentMemoryResolver::with_workspace_layout(storage_workspace.clone()),
        MemoryIndexMaintainerConfig::from_env(),
        supervisor_runtime.handle().clone(),
    );

    // Watchdog: log errors if any supervisor task exits unexpectedly before shutdown.
    // A panic or early return in a task would otherwise silently
    // stop scheduling or approvals from functioning with no trace in logs.
    {
        let watchdog_handles: Vec<(String, tokio::task::AbortHandle)> = supervisor_tasks
            .iter()
            .map(|(name, handle)| (name.to_string(), handle.abort_handle()))
            .collect();
        // HTTP cancellation can arrive before core initialization finishes and
        // before the normal supervisor drain is reached. Those gated exits are
        // expected, so the watchdog observes that earlier process signal.
        let watchdog_shutdown = startup_http.shutdown_token();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = watchdog_shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        for (name, abort_handle) in &watchdog_handles {
                            if abort_handle.is_finished() {
                                tracing::error!(
                                    task = %name,
                                    "Supervisor task exited unexpectedly; \
                                     this may stop scheduling or approvals from functioning"
                                );
                            }
                        }
                    }
                }
            }
        });
    }

    magician::magician_v2::query_analysis::operation_llm_router::install_llm_routing_overrides(
        storage_workspace.base_root(),
    );
    let shared_enrollment_api = web::Data::new(EnrollmentApi::new(
        enrollment_store_resolver.clone(),
        magician_config.enrollment.clone(),
    ));
    let shared_auth_runtime = magician_api::auth_api::auth_runtime(
        std::sync::Arc::new(
            magician::magician_v2::auth::AuthStore::open(storage_workspace.base_root())
                .context("opening the auth store under the runtime root")?,
        ),
        magician_config.auth.clone(),
    );
    let bot_runtime = Arc::new(magician_learning::bots::ScopedBotRuntime::new(
        capability_workspace.clone(),
    ));
    // The bot-auth → canonical HITL broker bridges the bot adapter's
    // sidecar-based needs-auth signal to the canonical HITL pipeline so
    // bot sign-in flows surface in the AttentionBar's Requests bucket
    // (same pipeline as every other HITL source). See
    // `bots/auth_hitl_broker.rs` for the transition matrix.
    let auth_hitl_broker = Arc::new(
        magician_learning::bots::auth_hitl_broker::AuthHitlBroker::new(Arc::clone(
            &event_broadcaster,
        ))
        .with_cache_persist_path_in_workspace(
            storage_root.join("bot_auth_hitl_cache.json"),
            storage_workspace.clone(),
        ),
    );
    let shared_bot_api = web::Data::new(
        BotApi::new(Arc::clone(&bot_runtime)).with_auth_hitl_broker(Arc::clone(&auth_hitl_broker)),
    );

    // Share the same `ScopedAuthorityResolver` the dispatch gate
    // carries — guarantees the `/budget` UI and the gate read/write
    // the same in-memory ledger + token store Arcs per scope.
    let shared_resource_authority_api = web::Data::new(
        magician_api::resource_authority_api::ResourceAuthorityApi::with_scoped_resolver(
            storage_workspace.clone(),
            Arc::clone(&scoped_authority_resolver),
        ),
    );

    // Consent per outcome: the owner's view of what standing authority exists.
    let shared_approval_envelope_api = web::Data::new(
        magician_api::approval_envelopes_api::ApprovalEnvelopeApi::new(storage_workspace.clone()),
    );

    // The owner side of the rooms the reader surface serves: open, add, grant,
    // rotate, revoke. Until now a room could be read and never created. It
    // resolves audience membership the same way the reader does — through the
    // counterparty register — so a room and its reader agree on who is in it.
    let shared_data_room_owner = web::Data::new(magician_api::data_room_api::OwnerSurface::new(
        storage_workspace.clone(),
        // The roster is passed, not reached for. It used to be read from
        // the process global at request time, with a fallback that returned
        // the audience UNSTAMPED when nothing resolved — so `is_current`
        // answered true for ever and a revoked engagement kept opening its
        // room. The dependency is now in the type, which also removes the
        // boot-order coupling: this construction no longer depends on
        // `install_global_engagement_store` having run ~2800 lines earlier.
        Arc::new(
            magician_api::data_room_reader_api::CounterpartyAudiences::new(
                magician::magician_v2::counterparties::CounterpartyStore::new(
                    storage_workspace.clone(),
                ),
                engagement_store.clone(),
            ),
        ),
    ));

    // The register `outward_gate::contact_refusal` screens against. It had one
    // reader and no writer, so every real recipient passed vacuously — and kept
    // passing after they bounced or opted out.
    let shared_suppression_api = web::Data::new(
        magician_api::suppression_api::SuppressionApi::new(storage_workspace.clone()),
    );

    // The four composable work modules: durable runs, time negotiations,
    // artifact claim manifests, and the register of what is owed. Every store
    // behind these was empty on disk because nothing had ever written to one.
    let shared_work_modules_api = web::Data::new(
        magician_api::work_modules_api::WorkModulesApi::new(storage_workspace.clone())
            .with_sweep_config(obligation_sweep_config)
            // §5's inbox coupling. `None` leaves the sweep with no source, and
            // the route says so rather than reporting a pass that closed
            // nothing — a pass that asked nobody and one that found nothing are
            // opposite facts.
            .with_run_inbox(magician_config.run_inbox.clone()),
    );

    // The two owner-facing reads of the outcome record: what the market did with
    // the rooms, and whether a claim has gone stale or was re-asserted after a
    // correction. Both derive; neither writes.
    let shared_outcome_learning = web::Data::new(
        magician_learning::outcome_learning::OutcomeLearningSurface::new(storage_workspace.clone()),
    );

    // The room a counterparty actually reads, and the only path that writes an
    // access event — so a document cannot be served without the visit landing
    // first. Audience membership resolves through the counterparty register.
    let shared_data_room_reader =
        web::Data::new(magician_api::data_room_reader_api::ReaderSurface::new(
            storage_workspace.clone(),
            // The roster is passed, not reached for. It used to be read from
            // the process global at request time, with a fallback that returned
            // the audience UNSTAMPED when nothing resolved — so `is_current`
            // answered true for ever and a revoked engagement kept opening its
            // room. The dependency is now in the type, which also removes the
            // boot-order coupling: this construction no longer depends on
            // `install_global_engagement_store` having run ~2800 lines earlier.
            Arc::new(
                magician_api::data_room_reader_api::CounterpartyAudiences::new(
                    magician::magician_v2::counterparties::CounterpartyStore::new(
                        storage_workspace.clone(),
                    ),
                    engagement_store.clone(),
                ),
            ),
        ));

    // M6 Citizen API: record the loopback base URL so coding runs can inject it
    // into the Pi Citizen extension's env (its tools call back over loopback).
    magician::magician_v2::execution::coding_engine::citizen::set_citizen_base_url(format!(
        "http://127.0.0.1:{port}/api/magician/v2/vibedev"
    ));
    magician::magician_v2::execution::plane::install_harness_engine_snapshot(
        magician::magician_v2::execution::plane::HarnessEngineSnapshot {
            engine: magician_config.execution.harness_engine.clone(),
            harness_model: magician_config.execution.harness_model.clone(),
            pi_profile: magician_config.execution.pi_profile.clone(),
            turn_max_tool_calls: magician_config.execution.harness_turn_max_tool_calls,
            turn_max_seconds: magician_config.execution.harness_turn_max_seconds,
            plane_endpoint: format!("http://127.0.0.1:{port}/api/magician/v2/plane/mcp"),
        },
    );
    // Engine affinity (owner rule 2026-08-31): whatever harness drives,
    // the system's non-local calls follow it. The run engine is primary;
    // when it is `magician` but the CHAT mouth persists an external
    // harness, that engine drives instead — matching the PUT semantics
    // (last driver wins) across restarts.
    {
        let run_engine = magician::magician_v2::execution::plane::harness_engine_snapshot();
        let affinity_engine = if run_engine.engine != "magician" {
            run_engine.engine.clone()
        } else {
            magician_config.chat.harness_engine.clone()
        };
        magician::magician_v2::query_analysis::operation_llm_router::set_harness_affinity(Some(
            affinity_engine.as_str(),
        ));
    }
    magician::magician_v2::execution::plane::install_chat_harness_snapshot(
        magician::magician_v2::execution::plane::ChatHarnessSnapshot {
            engine: magician_config.chat.harness_engine.clone(),
            harness_model: magician_config.chat.harness_model.clone(),
            turn_max_tool_calls: magician_config.chat.harness_turn_max_tool_calls,
            turn_max_seconds: magician_config.chat.harness_turn_max_seconds,
            plane_endpoint: format!("http://127.0.0.1:{port}/api/magician/v2/plane/mcp"),
        },
    );
    magician::magician_v2::execution::plane::install_runless_executors(std::sync::Arc::new(
        orchestrator.build_action_executors(),
    ));
    // A changed chat engine reaches every open composer (`chat.engine.updated`),
    // whichever path changed it. Subscribed after the boot install above, so
    // only later changes are announced.
    {
        let broadcaster = Arc::clone(&event_broadcaster);
        let mut changes = magician::magician_v2::execution::plane::subscribe_chat_engine_changes();
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let (engine, model) = changes.borrow_and_update().clone();
                use magician::magician_v2::realtime_events::RuntimeAgentEventType;
                broadcaster.emit_named(
                    RuntimeAgentEventType::ChatEngineUpdated.as_str(),
                    magician::magician_v2::realtime_events::CHAT_ENGINE_UPDATED_AGENT_ID,
                    None,
                    None,
                    serde_json::json!({ "chat_current": engine, "chat_model": model }),
                );
            }
        });
    }

    // Magicutor base URL for the aggregated `/health` probe (per-worker app data).
    let magicutor_health_base_url = magician_config.execution.magicutor_base_url.clone();
    // Plane catalog overlays are request-time authorization input as well as
    // `tools/list` presentation. Mount the exact loaded config into every
    // Actix worker so list and call cannot disagree.
    let plane_config = magician_config.plane.clone();
    // Keep one owner outside Actix's move factory so shutdown can quiesce the
    // queue before the canonical event bridge takes its final receiver drain.
    let dispatch_queue_for_shutdown = shared_dispatch_queue.clone();

    let maintenance_service = shared_storage_governance_service.clone();
    startup_http.publish(move || {
        let api = MagicianV2Api::with_shared_pause_store_and_agent_api(
            Arc::clone(&orchestrator),
            Arc::clone(&shared_pause_store),
            shared_agent_api.clone(),
        )
        .with_progress_router(shared_progress_router.clone());
        // Extract pause store before wrapping api in Data (for WebSocket handler)
        let pause_store = web::Data::new(Arc::clone(api.full_pause_store()));
        let api = web::Data::new(api);
        // Secure HITL P6: an agentic pause answered by a retrieved code runs
        // this API's own resume path; the first worker's instance serves.
        magician_api::verification_codes_api::ApiAgenticAnswerSink::install(api.clone().into_inner());
        let broadcaster = web::Data::new(Arc::clone(&event_broadcaster));
        let chat_turn_event_sink_data = web::Data::new(Arc::clone(&chat_turn_event_sink));
        let ask_loop = web::Data::from(Arc::clone(&ask_loop_api));

        let mut app = App::new()
            .wrap(from_fn(log_http_requests))
            .app_data(api)
            .app_data(broadcaster)
            .app_data(chat_turn_event_sink_data)
            .app_data(ask_loop)
            .app_data(shared_agent_resources_data.clone())
            .app_data(shared_content_acquisition_resolver.clone())
            .app_data(shared_chat_api.clone())
            .app_data(shared_memory_api.clone())
            .app_data(shared_progress_channel_api.clone())
            .app_data(shared_feed_api.clone())
            .app_data(shared_fleet_state_api.clone())
            .app_data(shared_execution_panel_api.clone())
            .app_data(shared_ui_thread_api.clone())
            .app_data(shared_storage_governance_service.clone())
            .app_data(shared_capability_registry.clone())
            .app_data(shared_gaui_api.clone())
            .app_data(dashboard_theme_registry.clone())
            .app_data(shared_muij_storage.clone())
            .app_data(shared_muij_doc_cache.clone())
            .app_data(shared_artifact_api.clone())
            // TaskApiV3 must be App-level (not only on the /api/magician/v3 scope):
            // respond_hitl_handler lives under /api/magician/v2 and its `clarification`
            // arm dispatches V3 task-plan clarifications. Without this it extracts
            // `Option<web::Data<TaskApiV3>>` as None → `missing_hitl_dependency:TaskApiV3`,
            // and clarification approvals never resolve (the pause reappears).
            .app_data(shared_task_api_v3.clone())
            .app_data(shared_evidence_api.clone())
            .app_data(shared_ambient_api.clone())
            .app_data(shared_observe_api.clone())
            .app_data(shared_observable_source_runtime.clone())
            .app_data(shared_observe_catch_up_controller.clone())
            .app_data(shared_channel_assist_api.clone())
            .app_data(shared_resurfacing_interactions.clone())
            .app_data(shared_resurfacing_actions.clone())
            .app_data(web::Data::new(device_bridge_hub.clone()))
            .app_data(web::Data::new(edge_session_registry.clone()))
            .app_data(web::Data::new(device_pairing_store.clone()))
            .app_data(web::Data::new(android_apps_owner_store.clone()))
            .app_data(web::Data::new(mobile_push_store.clone()))
            .app_data(web::Data::new(mobile_push_config.clone()))
            .app_data(mobile_enrollment_config.clone())
            .app_data(web::Data::new(device_policy_store.clone()))
            .app_data(web::Data::new(device_action_audit.clone()))
            // Present only when Access is configured; the middleware is a
            // pass-through without it, which is what keeps local runs working.
            .app_data(web::Data::new(access_verifier.clone()))
            .app_data(web::Data::new(resurfacing_store.clone()))
            .app_data(web::Data::new(channel_assist_store.clone()))
            .app_data(web::Data::new(attention_funnel_store.clone()))
            .app_data(web::Data::new(attention_learning_service.clone()))
            .app_data(web::Data::new(semantic_extraction_worker.clone()))
            .app_data(web::Data::new(rank_recompute_worker.clone()))
            .app_data(web::Data::new(actionability_training_worker.clone()))
            .app_data(web::Data::new(
                magician_api::service_health_api::ServiceHealthConfig {
                    magicutor_base_url: magicutor_health_base_url.clone(),
                },
            ))
            .app_data(shared_api_mining_api.clone())
            .app_data(shared_secret_vault_api.clone())
            .app_data(shared_skills_api.clone())
            .app_data(shared_runtime_env_api.clone())
            .app_data(shared_component_setup_api.clone())
            .app_data(shared_notes_api.clone())
            .app_data(shared_workspace_storage_api.clone())
            .app_data(shared_tutor_api.clone())
            .app_data(shared_thinking_maps_api.clone())
            .app_data(shared_thinking_map_coordinator.clone())
            .app_data(shared_media_api.clone())
            .app_data(shared_mcp_oauth_api.clone())
            .app_data(shared_ui_preferences_api.clone())
            .app_data(shared_vibedev_api.clone())
            .app_data(shared_prompt_manager.clone())
            .app_data(shared_voice_context_compactor.clone())
            .app_data(shared_chat_store_for_voice.clone())
            .app_data(web::Data::new(plane_config.clone()))
            .app_data(shared_operation_llm_router.clone())
            .app_data(shared_event_broadcaster_for_voice.clone())
            .app_data(shared_chat_service_for_voice.clone())
            .app_data(web::Data::new(Arc::clone(&shared_agent_api.definition_store)))
            // `voice_control_ws_handler` extracts `web::Data<Arc<ArtifactV2Service>>`
            // for delegate / handover cancel-cascade and task lookups
            // inside the orchestrator it builds per WS connection. Without
            // this mount, actix can't satisfy the extractor and the WS
            // upgrade fails with a 500 — frontend sees "voice control
            // WebSocket failed to open" with no backend log because the
            // failure happens during extractor resolution, before the
            // handler body runs.
            .app_data(web::Data::new(Arc::clone(&shared_artifact_v2_service)))
            .app_data(pause_store)
            .app_data(shared_enrollment_api.clone())
            .app_data(shared_auth_runtime.clone())
            .app_data(shared_bot_api.clone())
            .app_data(shared_user_request_service.clone())
            .app_data(shared_resource_authority_api.clone())
            .app_data(shared_approval_envelope_api.clone())
            .app_data(shared_data_room_reader.clone())
            .app_data(shared_data_room_owner.clone())
            .app_data(shared_suppression_api.clone())
            .app_data(shared_work_modules_api.clone())
            .app_data(shared_outcome_learning.clone())
            .app_data(shared_delivery_watch_health.clone())
            .app_data(shared_transcript_claims_api.clone())
            .app_data(shared_corrections_api.clone())
            .app_data(shared_recipient_compliance_api.clone())
            .app_data(shared_outcome_maturity_health.clone())
            .app_data(shared_outcome_proposal_health.clone())
            .app_data(shared_learning_api.clone())
            .app_data(shared_storage_root.clone())
            .app_data(shared_storage_runtime.clone())
            .app_data(shared_app_platform_api.clone())
            .app_data(web::Data::new(shared_app_platform_api.scripted_surface_authenticator()))
            .app_data(web::Data::new(llm_content_settings.clone()))
            .app_data(shared_agent_update_journal_config.clone());
        if let Some(ref analytics_api) = shared_analytics_api {
            app = app.app_data(analytics_api.clone());
        }
        app = app.app_data(shared_evals_api.clone());
        if let Some(ref queue) = shared_dispatch_queue {
            app = app
                .app_data(web::Data::new(Arc::clone(queue)))
                .configure(magician_api::llm_queue_api::configure);
        }
        app = app.configure(magician_api::local_resource_governor_api::configure);

        // The counterparty reader surface, mounted at the app ROOT and
        // deliberately outside `/api/magician/v2`: that scope is wrapped in
        // Cloudflare Access, and the people this surface exists for are not
        // team members. Its own authority is the presented capability link,
        // checked against the living audience on every request.
        app = app.configure(magician_api::data_room_reader_api::configure_data_room_reader_routes);
        app = app
            .app_data(
                web::JsonConfig::default()
                    .limit(API_JSON_PAYLOAD_LIMIT_BYTES)
                    .error_handler(json_payload_error_handler),
            )
            .app_data(web::PayloadConfig::new(API_JSON_PAYLOAD_LIMIT_BYTES))
            .route(
                "/health",
                web::get().to(
                    magician_api::service_health_api::service_health_handler,
                ),
            )
            .route("/health/execution-driver", web::get().to(execution_driver_health))
            .route("/health/storage", web::get().to(storage_health))
            .service({
                let scope = web::scope("/api/magician/v2")
                    .wrap(from_fn(api_cors_middleware))
                    // Runs before the handlers so any Access-derived identity
                    // is verified before internal scope context is engraved. Inert unless
                    // MAGICIAN_CF_ACCESS_TEAM_DOMAIN and _AUD are both set.
                    .wrap(from_fn(
                        magician::magician_v2::cloudflare_access::verify_access_middleware,
                    ))
                    // Auth gate — registered last so it runs FIRST (before
                    // the Cloudflare Access layer): resolves the bearer
                    // against the unified store and engraves proven
                    // internal x-principal/x-workspace context. In open mode,
                    // an absent bearer is fixed to anonymous/default.
                    // docs/components/magician/auth.md
                    .wrap(from_fn(
                        magician::magician_v2::auth::middleware::authenticate_request,
                    ))
                    .configure(magician_api::auth_api::configure_auth_routes)
                    .route(
                        "/admin/tasks/reconcile-orphaned-ready",
                        web::post().to(
                            magician_api::web_api::reconcile_orphaned_ready_tasks_handler,
                        ),
                    )
                    .configure(
                        magician_api::hitl_deprecation_metrics::configure_routes,
                    )
                    .configure(
                        magician_api::fleet_state_api::configure_routes,
                    )
                    .configure(magician_api::llm_chunking_api::configure)
                    .configure(magician_api::storage_governance_api::configure)
                    .configure(magician_api::storage_activation_api::configure)
                    .configure(magician_api::mcp_oauth_api::configure_routes)
                    .configure(configure_app_routes)
                    .route(
                        "/updates",
                        web::get().to(
                            magician_api::agent_updates_api::list_agent_updates_handler,
                        ),
                    )
                    // Program documents (Fleet Civilization guild missions; reads + owner revert)
                    .route("/programs", web::get().to(magician_api::programs_api::list_programs_handler))
                    .route(
                        "/programs/{name}",
                        web::get().to(magician_api::programs_api::get_program_handler),
                    )
                    .route(
                        "/programs/{name}/revert",
                        web::post().to(magician_api::programs_api::revert_program_handler),
                    )
                    .route("/agents", web::post().to(create_agent_definition_handler))
                    .route("/agents", web::get().to(list_agent_definitions_handler))
                    .route("/agents/health", web::get().to(get_crew_health_handler))
                    .route(
                        "/agents/refresh-definitions",
                        web::post().to(refresh_agent_definitions_handler),
                    )
                    .route("/agents/{id}", web::get().to(get_agent_definition_handler))
                    .route(
                        "/agents/{id}/health",
                        web::get().to(get_agent_health_handler),
                    )
                    .route(
                        "/agents/{id}/effective-tools",
                        web::get().to(get_agent_effective_tools_handler),
                    )
                    .route(
                        "/agents/{id}/runtime-context-cache",
                        web::get().to(get_agent_runtime_context_cache_handler),
                    )
                    .route(
                        "/agents/{id}/runtime-context-cache/refresh",
                        web::post().to(refresh_agent_runtime_context_cache_handler),
                    )
                    .route(
                        "/agents/{id}/harness-overview",
                        web::get().to(get_agent_harness_overview_handler),
                    )
                    .route(
                        "/agents/{id}",
                        web::put().to(update_agent_definition_handler),
                    )
                    .route(
                        "/agents/{id}",
                        web::patch().to(patch_agent_definition_handler),
                    )
                    .route(
                        "/agents/{id}",
                        web::delete().to(delete_agent_definition_handler),
                    )
                    .route(
                        "/agents/{id}/set-primary",
                        web::post().to(set_primary_agent_handler),
                    )
                    .route(
                        "/agents/{id}/trigger",
                        web::post().to(manual_trigger_agent_handler),
                    )
                    .route(
                        "/agents/{id}/consolidate-user-memory",
                        web::post().to(consolidate_user_memory_handler),
                    )
                    .route(
                        "/memory/search",
                        web::post().to(search_owner_memory_handler),
                    )
                    .route(
                        "/memory/user-knowledge",
                        web::get().to(get_user_knowledge_handler),
                    )
                    .route(
                        "/memory/user-knowledge/{path:.*}",
                        web::delete().to(delete_user_knowledge_key_handler),
                    )
                    .route(
                        "/memory/pending-tasks",
                        web::get().to(memory_pending_tasks_handler),
                    )
                    .route(
                        "/memory/synthesize",
                        web::post().to(memory_synthesize_handler),
                    )
                    .route(
                        "/harness/runtime",
                        web::get().to(get_harness_runtime_status_handler),
                    )
                    .route(
                        "/harness/runtime",
                        web::put().to(update_harness_runtime_handler),
                    )
                    // Deliberately NOT under `/agents/...`. Ordering alone would
                    // make `/agents/outward/pause` win over `/agents/{id}/pause`
                    // — but only until somebody creates an agent whose id is
                    // literally `outward`, at which point pausing that one agent
                    // would silently pause every outward agent instead. A
                    // separate first segment cannot collide with an id at all.
                    .route(
                        "/outward-agents/pause",
                        web::post().to(pause_outward_agents_handler),
                    )
                    .route(
                        "/outward-agents/resume",
                        web::post().to(resume_outward_agents_handler),
                    )
                    .route("/agents/{id}/pause", web::post().to(pause_agent_handler))
                    .route("/agents/{id}/resume", web::post().to(resume_agent_handler))
                    .route(
                        "/agents/{id}/episodes",
                        web::get().to(get_agent_episodes_handler),
                    )
                    .route(
                        "/agents/{id}/artifacts",
                        web::get().to(get_agent_artifacts_handler),
                    )
                    .route(
                        "/agents/{id}/memory",
                        web::get().to(get_agent_memory_handler),
                    )
                    .route(
                        "/agents/{id}/memory/consolidation-health",
                        web::get().to(get_agent_memory_consolidation_health_handler),
                    )
                    .route(
                        "/agents/{id}/memory/{tier}",
                        web::get().to(get_agent_memory_tier_handler),
                    )
                    .route("/triggers", web::get().to(list_triggers_handler))
                    .route("/agents/{id}/layout", web::get().to(get_layout_handler))
                    .route("/agents/{id}/layout", web::put().to(put_layout_handler))
                    .route("/approvals", web::get().to(list_approvals_handler))
                    .route(
                        "/approvals/{approval_id}",
                        web::get().to(get_approval_handler),
                    )
                    .route(
                        "/approvals/{approval_id}/resolve",
                        web::post().to(resolve_approval_handler),
                    )
                    .route("/proposals", web::post().to(create_proposal_handler))
                    .route("/proposals", web::get().to(list_proposals_handler))
                    .route(
                        "/proposals/{proposal_id}",
                        web::get().to(get_proposal_handler),
                    )
                    .route(
                        "/proposals/{proposal_id}/resolve",
                        web::post().to(resolve_proposal_handler),
                    )
                    .route(
                        "/settings/trust-policy",
                        web::get().to(get_trust_policy_handler),
                    )
                    .route(
                        "/settings/trust-policy",
                        web::put().to(update_trust_policy_handler),
                    )
                    .route(
                        "/settings/trust-policy/restore-template",
                        web::post().to(restore_trust_policy_template_handler),
                    )
                    .route(
                        "/settings/magician-config/reload",
                        web::post().to(reload_magician_config_handler),
                    )
                    .route(
                        "/settings/privacy",
                        web::get().to(get_privacy_settings_handler),
                    )
                    .route(
                        "/settings/privacy",
                        web::put().to(put_privacy_settings_handler),
                    )
                    // Critical-request delivery (secure HITL plan §6.1):
                    // the owner's settings + test action, and the channel
                    // bots' claim/report of one delivery each.
                    .route(
                        "/settings/critical-delivery",
                        web::get().to(magician_api::hitl_delivery_api::get_critical_delivery_settings_handler),
                    )
                    .route(
                        "/settings/critical-delivery",
                        web::put().to(magician_api::hitl_delivery_api::put_critical_delivery_settings_handler),
                    )
                    .route(
                        "/settings/critical-delivery/test",
                        web::post().to(magician_api::hitl_delivery_api::test_critical_delivery_handler),
                    )
                    .route(
                        "/hitl/deliveries",
                        web::get().to(magician_api::hitl_delivery_api::delivery_status_handler),
                    )
                    .route(
                        "/hitl/deliveries/{delivery_id}/claim",
                        web::post().to(magician_api::hitl_delivery_api::claim_delivery_handler),
                    )
                    .route(
                        "/hitl/deliveries/{delivery_id}/report",
                        web::post().to(magician_api::hitl_delivery_api::report_delivery_handler),
                    )
                    // Secure HITL P6: automatic verification-code retrieval.
                    .route(
                        "/hitl/{correlation_id}/retrieval",
                        web::get().to(magician_api::verification_codes_api::get_retrieval_status_handler),
                    )
                    .route(
                        "/settings/local-generation",
                        web::get().to(get_local_generation_settings_handler),
                    )
                    .route(
                        "/settings/local-generation",
                        web::put().to(put_local_generation_settings_handler),
                    )
                    .route("/executions", web::post().to(create_execution_v2_handler))
                    .route("/executions", web::get().to(list_executions_v2_handler))
                    .route("/executions/{id}", web::get().to(get_execution_v2_handler))
                    .route(
                        "/executions/{id}",
                        web::delete().to(delete_execution_v2_handler),
                    )
                    .route(
                        "/executions/{id}/turns",
                        web::get().to(list_turns_v2_handler),
                    )
                    .route(
                        "/executions/{id}/status",
                        web::put().to(update_execution_status_handler),
                    )
                    .route(
                        "/executions/{id}/status",
                        web::get().to(get_execution_status_handler),
                    )
                    .route(
                        "/executions/{id}/execution-summary",
                        web::get().to(get_execution_summary_handler),
                    )
                    .route(
                        "/executions/{id}/responsibility",
                        web::get().to(get_execution_responsibility_handler),
                    )
                    .route(
                        "/executions/{id}/message",
                        web::post().to(post_message_v2_handler),
                    )
                    .route(
                        "/executions/{id}/start",
                        web::post().to(start_execution_handler),
                    )
                    .route(
                        "/executions/{execution_id}/clarify/{question_id}/respond",
                        web::post().to(submit_clarification_handler),
                    )
                    .route(
                        "/executions/{id}/execution/agentic-resume",
                        web::post().to(resume_agentic_execution_handler),
                    )
                    .route(
                        "/executions/{id}/execution/agentic-continue",
                        web::post().to(continue_agentic_execution_handler),
                    )
                    .route(
                        "/executions/{id}/execution/agentic-cancel",
                        web::post().to(cancel_paused_execution_handler),
                    )
                    .route(
                        "/executions/{id}/execution",
                        web::delete().to(cancel_active_execution_handler),
                    )
                    .route(
                        "/executions/{id}/cancel",
                        web::post().to(cancel_active_execution_handler),
                    )
                    // Alias for cancel - frontend uses /abort
                    .route(
                        "/executions/{id}/abort",
                        web::post().to(cancel_active_execution_handler),
                    )
                    .route(
                        "/executions/{id}/control-state",
                        web::get().to(get_execution_control_state_handler),
                    )
                    // Pause -> resumable PausedByUser state; Resume -> continue.
                    .route(
                        "/executions/{id}/pause",
                        web::post().to(pause_active_execution_handler),
                    )
                    .route(
                        "/executions/{id}/resume",
                        web::post().to(resume_active_execution_handler),
                    )
                    // Steer -> inject an operator redirect into the next decision turn.
                    .route(
                        "/executions/{id}/steer",
                        web::post().to(steer_active_execution_handler),
                    )
                    // Screenshot fetch-on-demand for execution observability (disk-based)
                    .route(
                        "/executions/{execution_id}/observations/{observation_id}/screenshot",
                        web::get().to(get_observation_screenshot_handler),
                    )
                    // Raw observation JSON for debug UI tree viewer
                    .route(
                        "/executions/{execution_id}/observations/{observation_id}/json",
                        web::get().to(get_observation_json_handler),
                    )
                    // Storage stats for observability
                    .route(
                        "/executions/{execution_id}/storage-stats",
                        web::get().to(get_storage_stats_handler),
                    )
                    // List all observations for an execution (debug UI)
                    .route(
                        "/executions/{execution_id}/observations",
                        web::get().to(list_observations_handler),
                    )
                    // Get pause state (debug UI)
                    .route(
                        "/executions/{execution_id}/pause-state",
                        web::get().to(get_pause_state_handler),
                    )
                    // Durable artifact API
                    .route("/artifacts/durable", web::get().to(list_durable_artifacts))
                    .route(
                        "/artifacts/durable/{namespace}/{name:.*}",
                        web::get().to(read_durable_artifact),
                    )
                    // Work-evidence graph read-path + trust-surface endpoints.
                    .route(
                        "/evidence/review",
                        web::post().to(post_evidence_review_handler),
                    )
                    .route("/evidence", web::get().to(list_evidence_handler))
                    .route(
                        "/evidence/correct",
                        web::post().to(post_evidence_correct_handler),
                    )
                    .route(
                        "/evidence/dashboard",
                        web::get().to(get_evidence_dashboard_handler),
                    )
                    .route(
                        "/evidence/dashboard/publish",
                        web::post().to(post_evidence_dashboard_publish_handler),
                    )
                    .route("/entities", web::get().to(list_entities_handler))
                    .route(
                        "/entities/correct",
                        web::post().to(post_entity_correct_handler),
                    )
                    .route(
                        "/evidence/reviews",
                        web::get().to(list_evidence_reviews_handler),
                    )
                    .route(
                        "/evidence/review/feedback",
                        web::post().to(post_evidence_review_feedback_handler),
                    )
                    .route(
                        "/evidence/utility",
                        web::get().to(get_evidence_utility_handler),
                    )
                    // WEG Phase 2: ambient browser-capture ingestion + consent.
                    .route("/ambient/status", web::get().to(get_ambient_status_handler))
                    .route("/ambient/stats", web::get().to(get_ambient_stats_handler))
                    .route(
                        "/browser/engine-usage",
                        web::get().to(
                            magician_api::browser_engine_analytics_api::list_browser_engine_usage_handler,
                        ),
                    )
                    .route("/ambient/config", web::put().to(put_ambient_config_handler))
                    .route("/ambient/enroll", web::post().to(post_ambient_enroll_handler))
                    // WEG Phase 4 consent surface (email/calendar observe cards):
                    // accounts the cards offer + per-producer consent/cadence config.
                    .route(
                        "/observe/accounts",
                        web::get().to(
                            magician_api::observe_connectors_api::get_observe_accounts_handler,
                        ),
                    )
                    .route(
                        "/observe/{producer}/status",
                        web::get().to(
                            magician_api::observe_connectors_api::get_observe_status_handler,
                        ),
                    )
                    .route(
                        "/observe/{producer}/config",
                        web::put().to(
                            magician_api::observe_connectors_api::put_observe_config_handler,
                        ),
                    )
                    .route(
                        "/observe/catch-up",
                        web::get().to(
                            magician_api::observe_catchup_api::get_observe_catch_up_handler,
                        ),
                    )
                    .route(
                        "/observe/catch-up",
                        web::put().to(
                            magician_api::observe_catchup_api::put_observe_catch_up_handler,
                        ),
                    )
                    .route(
                        "/observe/sources",
                        web::get().to(
                            magician_api::observable_sources_api::list_observable_sources_handler,
                        ),
                    )
                    .route(
                        "/observe/sources/observability",
                        web::get().to(
                            magician_api::observable_sources_api::observable_source_observability_handler,
                        ),
                    )
                    .route(
                        "/observe/subscriptions",
                        web::get().to(
                            magician_api::observable_sources_api::list_observation_subscriptions_handler,
                        ),
                    )
                    .route(
                        "/observe/subscriptions/{id}",
                        web::put().to(
                            magician_api::observable_sources_api::put_observation_subscription_handler,
                        ),
                    )
                    .route(
                        "/observe/subscriptions/{id}",
                        web::delete().to(
                            magician_api::observable_sources_api::delete_observation_subscription_handler,
                        ),
                    )
                    .route(
                        "/observe/subscriptions/{id}/run",
                        web::post().to(
                            magician_api::observable_sources_api::run_observation_subscription_handler,
                        ),
                    )
                    .route(
                        "/observe/subscriptions/{id}/runs",
                        web::get().to(
                            magician_api::observable_sources_api::observation_run_history_handler,
                        ),
                    )
                    .configure(configure_channel_assist_routes)
                    .route(
                        "/attention-funnel/observability",
                        web::get().to(
                            magician_api::attention_funnel_api::get_attention_funnel_observability_handler,
                        ),
                    )
                    .route(
                        "/ambient/signals/batch",
                        web::post().to(post_ambient_signals_batch_handler),
                    )
                    .route(
                        "/ambient/distill",
                        web::post().to(post_ambient_distill_handler),
                    )
                    // API Mining endpoints (API Explorer)
                    .route(
                        "/capability-evolution/summary",
                        web::get().to(get_capability_evolution_summary),
                    )
                    .route(
                        "/api-mining/router-metrics",
                        web::get().to(get_router_metrics),
                    )
                    .route(
                        "/api-mining/projection-metrics",
                        web::get().to(get_projection_metrics),
                    )
                    .route(
                        "/api-mining/passive-validation-metrics",
                        web::get().to(get_passive_validation_metrics),
                    )
                    .route(
                        "/api-mining/registry-health",
                        web::get().to(get_registry_health),
                    )
                    .route(
                        "/api-mining/sequence-metrics",
                        web::get().to(get_sequence_metrics),
                    )
                    .route(
                        "/api-mining/workflow-metrics",
                        web::get().to(get_workflow_metrics),
                    )
                    .route(
                        "/api-mining/recipe-metrics",
                        web::get().to(get_recipe_metrics),
                    )
                    .route(
                        "/api-mining/settings",
                        web::get().to(get_api_mining_settings),
                    )
                    .route(
                        "/api-mining/settings",
                        web::put().to(put_api_mining_settings),
                    )
                    .route(
                        "/api-mining/settings/disable-and-purge",
                        web::post().to(disable_and_purge_api_mining),
                    )
                    .route("/api-mining/overview", web::get().to(get_overview))
                    .route("/api-mining/recipes", web::get().to(list_recipes))
                    .route(
                        "/api-mining/recipes/{recipe_id}",
                        web::get().to(get_recipe),
                    )
                    .route(
                        "/api-mining/recipes/{recipe_id}/runs",
                        web::get().to(list_recipe_runs),
                    )
                    .route(
                        "/api-mining/recipes/{recipe_id}/replay",
                        web::post().to(replay_recipe),
                    )
                    .route(
                        "/api-mining/replay-grants",
                        web::get().to(list_replay_grants),
                    )
                    .route(
                        "/api-mining/replay-grants/{grant_id}",
                        web::delete().to(revoke_replay_grant),
                    )
                    .route(
                        "/api-mining/projections",
                        web::get().to(list_projections),
                    )
                    .route(
                        "/api-mining/projections/query",
                        web::post().to(query_known_resource_handler),
                    )
                    .route(
                        "/api-mining/projections/{id}/approve",
                        web::post().to(approve_projection),
                    )
                    .route(
                        "/api-mining/projections/{id}/purge-rows",
                        web::post().to(purge_projection_rows),
                    )
                    .route("/api-mining/registry", web::get().to(get_registry))
                    .route(
                        "/api-mining/noisy-origins",
                        web::get().to(get_noisy_origins),
                    )
                    .route(
                        "/api-mining/capabilities/{origin_key}/{capability_id}",
                        web::get().to(get_capability),
                    )
                    .route(
                        "/api-mining/capabilities/{origin_key}/{capability_id}/bless",
                        web::post().to(bless_capability),
                    )
                    .route(
                        "/api-mining/sequences/{origin_key}",
                        web::get().to(list_sequences),
                    )
                    .route(
                        "/api-mining/sequences/{origin_key}/{sequence_id}",
                        web::get().to(get_sequence),
                    )
                    .route(
                        "/api-mining/workflows/{origin_key}",
                        web::get().to(list_workflows),
                    )
                    .route(
                        "/api-mining/workflows/{origin_key}/{workflow_id}",
                        web::get().to(get_workflow),
                    )
                    .route(
                        "/api-mining/workflows/{origin_key}/{workflow_id}/replay",
                        web::post().to(replay_workflow),
                    )
                    .route(
                        "/api-mining/replay-metrics",
                        web::get().to(get_replay_metrics),
                    )
                    .route(
                        "/api-mining/replay/{origin_key}/{capability_id}",
                        web::post().to(replay_capability),
                    )
                    .route(
                        "/api-mining/auth-statuses",
                        web::get().to(get_all_auth_statuses),
                    )
                    .route(
                        "/api-mining/auth-status/{origin_key}",
                        web::get().to(get_auth_status),
                    )
                    .route(
                        "/api-mining/openapi/{origin_key}",
                        web::get().to(get_openapi),
                    )
                    .route(
                        "/api-mining/origins/{origin_key}/refresh-auth",
                        web::post().to(refresh_origin_auth),
                    )
                    .route(
                        "/api-mining/origins/{origin_key}/refresh-auth/{refresh_id}",
                        web::get().to(get_origin_auth_refresh_status),
                    )
                    .route(
                        "/api-mining/origins/{origin_key}/allow",
                        web::post().to(allow_origin),
                    )
                    .route(
                        "/api-mining/origins/{origin_key}/block",
                        web::post().to(block_origin),
                    )
                    .route(
                        "/api-mining/origins/{origin_key}/allow-replay",
                        web::post().to(set_origin_allow_replay),
                    )
                    .route(
                        "/api-mining/origins/{origin_key}/replay-mode",
                        web::post().to(set_origin_replay_mode),
                    )
                    // Developer Mode — interactive_process session
                    // surface (stdin write, list live, close). See
                    // docs/plans/2026-05-13-developer-mode-workbench.md.
                    .route(
                        "/interactive-sessions",
                        web::get().to(list_interactive_sessions_handler),
                    )
                    .route(
                        "/interactive-sessions",
                        web::post().to(start_interactive_session_handler),
                    )
                    .route(
                        "/interactive-sessions/cli-runtimes",
                        web::get().to(list_interactive_cli_runtimes_handler),
                    )
                    .route(
                        "/interactive-sessions/{id}/stdin",
                        web::post().to(write_interactive_stdin_handler),
                    )
                    .route(
                        "/interactive-sessions/{id}/buffer",
                        web::get().to(interactive_session_buffer_handler),
                    )
                    .route(
                        "/interactive-sessions/{id}",
                        web::delete().to(close_interactive_session_handler),
                    )
                    .route(
                        "/interactive-sessions/{id}/diff",
                        web::get().to(interactive_session_diff_handler),
                    )
                    .route(
                        "/filesystem/directories",
                        web::get().to(list_interactive_directories_handler),
                    )
                    .route(
                        "/api-mining/origins/{origin_key}/purge",
                        web::post().to(purge_origin),
                    )
                    .route(
                        "/api-mining/origins/{origin_key}/block-and-purge",
                        web::post().to(block_and_purge_origin),
                    )
                    // Meetings surface (passive listener + agent attendee).
                    // Literal segments registered before the {id} routes.
                    .route(
                        "/meetings",
                        web::get().to(magician_api::meetings_api::list_meetings_handler),
                    )
                    .route(
                        "/meetings/active",
                        web::get()
                            .to(magician_api::meetings_api::active_meetings_handler),
                    )
                    .route(
                        "/meetings/upcoming",
                        web::get()
                            .to(magician_api::meetings_api::upcoming_meetings_handler),
                    )
                    .route(
                        "/meetings/listen",
                        web::post()
                            .to(magician_api::meetings_api::listen_meeting_handler),
                    )
                    // Client capture chunk ingest — route-level 2 MiB payload cap
                    // (a 6 s PCM16 chunk is ~192 KB; the handler re-checks).
                    .service(
                        web::resource("/meetings/{id}/audio")
                            .app_data(web::PayloadConfig::new(2 * 1024 * 1024))
                            .route(web::post().to(
                                magician_api::meetings_api::ingest_meeting_audio_handler,
                            )),
                    )
                    .route(
                        "/meetings/join",
                        web::post()
                            .to(magician_api::meetings_api::join_meeting_handler),
                    )
                    .route(
                        "/meetings/{id}",
                        web::get()
                            .to(magician_api::meetings_api::meeting_status_handler),
                    )
                    .route(
                        "/meetings/{id}/stop",
                        web::post()
                            .to(magician_api::meetings_api::stop_meeting_handler),
                    )
                    .route(
                        "/meetings/{id}/pause",
                        web::post()
                            .to(magician_api::meetings_api::pause_meeting_handler),
                    )
                    .route(
                        "/meetings/{id}/resume",
                        web::post()
                            .to(magician_api::meetings_api::resume_meeting_handler),
                    )
                    // Screen capture-and-ask (kbd-shortcut / skill triggered).
                    .route(
                        "/screen/capture",
                        web::post()
                            .to(magician_api::screen_api::screen_capture_handler),
                    )
                    .route(
                        "/screen/capture/discard",
                        web::post().to(
                            magician_api::screen_api::screen_capture_discard_handler,
                        ),
                    )
                    .route(
                        "/screen/clip/toggle",
                        web::post()
                            .to(magician_api::screen_api::screen_clip_toggle_handler),
                    )
                    .route(
                        "/screen/desktop-app/launch",
                        web::post().to(
                            magician_api::screen_api::screen_desktop_app_launch_handler,
                        ),
                    )
                    .route(
                        "/screen/observe/start",
                        web::post()
                            .to(magician_api::screen_api::screen_observe_start_handler),
                    )
                    .route(
                        "/screen/observe/stop",
                        web::post()
                            .to(magician_api::screen_api::screen_observe_stop_handler),
                    )
                    .route(
                        "/screen/observe/status",
                        web::get()
                            .to(magician_api::screen_api::screen_observe_status_handler),
                    )
                    .route(
                        "/screen/observe/toggle",
                        web::post()
                            .to(magician_api::screen_api::screen_observe_toggle_handler),
                    )
                    .route(
                        "/screen/observe/retarget",
                        web::post()
                            .to(magician_api::screen_api::screen_observe_retarget_handler),
                    )
                    // Client-pushed screen observation (iOS broadcast): start a
                    // frame-fed observation narrating into a given thread, and
                    // ingest keyframes for it.
                    .route(
                        "/screen/observe/client/start",
                        web::post().to(
                            magician_api::screen_api::screen_observe_client_start_handler,
                        ),
                    )
                    .service(
                        web::resource("/screen/observe/frame")
                            .app_data(web::PayloadConfig::new(4 * 1024 * 1024))
                            .route(web::post().to(
                                magician_api::screen_api::ingest_observe_frame_handler,
                            )),
                    )
                    .route(
                        "/screen/describe",
                        web::post()
                            .to(magician_api::screen_api::screen_describe_handler),
                    )
                    .route(
                        "/screen/ground",
                        web::post()
                            .to(magician_api::screen_api::screen_ground_handler),
                    )
                    // WEG Phase 4 (desktop connector): distil the screen_observations
                    // tier into user-owned evidence (mirrors /ambient/distill).
                    .route(
                        "/screen/observations/distill",
                        web::post().to(
                            magician_api::screen_api::post_screen_observations_distill_handler,
                        ),
                    )
                    // WEG Phase 4 (generic tier connector): distil ANY producer's
                    // per-(key,day) roll-up tier into user-owned evidence — one handler
                    // for meeting | email | calendar | future lanes, selected by the
                    // `{producer}` path segment (see evidence::tier_distill::producer_spec).
                    .route(
                        "/evidence/distill/{producer}",
                        web::post().to(
                            magician_api::screen_api::post_tier_distill_handler,
                        ),
                    )
                    // Chat API routes (Phase 1 chat mode)
                    .service(
                        web::resource("/contextual-writing/actions")
                            .app_data(
                                web::JsonConfig::default()
                                    .limit(CONTEXTUAL_WRITING_JSON_PAYLOAD_LIMIT_BYTES)
                                    .error_handler(contextual_writing_json_payload_error_handler),
                            )
                            .route(web::post().to(contextual_writing_action_handler)),
                    )
                    .route(
                        "/contextual-writing/catalog",
                        web::get().to(contextual_writing_catalog_handler),
                    )
                    .route(
                        "/contextual-writing/sessions",
                        web::post().to(create_contextual_writing_session_handler),
                    )
                    .route("/chat/active", web::get().to(get_active_session_handler))
                    .route("/chat/new", web::post().to(new_session_handler))
                    .route("/chat/profiles", web::get().to(list_chat_profiles_handler))
                    .route(
                        "/chat/public-chat/status",
                        web::get().to(get_public_chat_status_handler),
                    )
                    .route(
                        "/chat/public-contacts",
                        web::get().to(list_public_contact_profiles_handler),
                    )
                    .route("/coding/profiles", web::get().to(list_coding_profiles_handler))
                    .route(
                        "/coding/engines/codex_app_server/refresh",
                        web::post().to(refresh_codex_app_server_handler),
                    )
                    .route(
                        "/coding/engines/grok_acp/refresh",
                        web::post().to(refresh_grok_acp_handler),
                    )
                    .route(
                        "/coding/engines/claude_code/refresh",
                        web::post().to(refresh_claude_code_handler),
                    )
                    .route(
                        "/coding/engines/agy_cli/refresh",
                        web::post().to(refresh_agy_cli_handler),
                    )
                    .route(
                        "/vibedev/deploy/settings",
                        web::get().to(get_vibedev_deploy_settings_handler),
                    )
                    .route(
                        "/vibedev/deploy/settings",
                        web::put().to(put_vibedev_deploy_settings_handler),
                    )
                    .route(
                        "/vibedev/deploy/settings/check",
                        web::post().to(check_vibedev_deploy_settings_handler),
                    )
                    .route(
                        "/vibedev/projects",
                        web::get().to(list_vibedev_projects_handler),
                    )
                    .route(
                        "/vibedev/projects",
                        web::post().to(create_vibedev_project_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}",
                        web::patch().to(update_vibedev_project_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}",
                        web::delete().to(delete_vibedev_project_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/activate",
                        web::post().to(activate_vibedev_project_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/open-repo",
                        web::post().to(open_vibedev_repo_handler),
                    )
                    .route(
                        "/vibedev/runs/{task_id}/logs",
                        web::get().to(get_vibedev_run_logs_handler),
                    )
                    .route(
                        "/vibedev/runs/{task_id}/coding-events",
                        web::get().to(get_vibedev_run_coding_events_handler),
                    )
                    .route(
                        "/vibedev/runs/{task_id}/proposals",
                        web::get().to(get_vibedev_run_proposals_handler),
                    )
                    .route(
                        "/vibedev/runs/{task_id}/checkpoints",
                        web::get().to(get_vibedev_run_checkpoints_handler),
                    )
                    .route(
                        "/vibedev/runs/{task_id}/checkpoints/{checkpoint_id}/revert",
                        web::post().to(revert_vibedev_checkpoint_handler),
                    )
                    .route(
                        "/vibedev/runs/{run_id}/control",
                        web::post().to(control_vibedev_run_handler),
                    )
                    // The cockpit's ONE way to start a build. It runs through
                    // `VibeDevRunService::start_build`, the same entry the
                    // `@vibedev` chat rail uses, so there is no second creation
                    // path to keep in step (handoff plan §10).
                    .route("/vibedev/runs", web::post().to(start_vibedev_run_handler))
                    .route(
                        "/vibedev/projects/{project_id}/info",
                        web::get().to(get_vibedev_project_info_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/files",
                        web::get().to(get_vibedev_project_files_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/file",
                        web::get().to(get_vibedev_project_file_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/screenshots",
                        web::get().to(get_vibedev_project_screenshots_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/check",
                        web::post().to(run_vibedev_check_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/deploy",
                        web::post().to(deploy_vibedev_project_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/preview",
                        web::get().to(get_vibedev_preview_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/preview/start",
                        web::post().to(start_vibedev_preview_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/preview/stop",
                        web::post().to(stop_vibedev_preview_handler),
                    )
                    // M6 Citizen API — called by the Pi Citizen extension (Bearer
                    // token = scope), not the cockpit.
                    .route(
                        "/vibedev/citizen/preview_url",
                        web::post().to(citizen_preview_url_handler),
                    )
                    .route(
                        "/vibedev/citizen/secret",
                        web::post().to(citizen_secret_handler),
                    )
                    .route(
                        "/vibedev/citizen/code_knowledge",
                        web::post().to(citizen_code_knowledge_handler),
                    )
                    // Magician plane — loopback MCP. Bearer is a `plt_` grant,
                    // not a session. GET holds the SSE stream for list_changed.
                    .route("/plane/mcp", web::post().to(plane_mcp_handler))
                    .route("/plane/mcp", web::get().to(plane_mcp_sse_handler))
                    .route("/plane/mcp", web::delete().to(plane_mcp_delete_handler))
                    .route("/plane/engines", web::get().to(plane_engines_handler))
                    .route("/plane/decision-mode", web::put().to(plane_decision_mode_put_handler))
                    .route("/plane/decision-routing", web::get().to(plane_decision_routing_get_handler))
                    .route("/plane/decision-routing", web::put().to(plane_decision_routing_put_handler))
                    .route(
                        "/plane/chat-engine",
                        web::put().to(plane_chat_engine_put_handler),
                    )
                    .route(
                        "/plane/engine",
                        web::put().to(plane_engine_put_handler),
                    )
                    .route("/plane/grants", web::get().to(plane_grants_list_handler))
                    .route("/plane/grants", web::post().to(plane_grants_mint_handler))
                    .route(
                        "/plane/grants/{id}",
                        web::delete().to(plane_grants_revoke_handler),
                    )
                    .route(
                        "/vibedev/projects/{project_id}/preview/proxy/{tail:.*}",
                        web::route().to(vibedev_preview_proxy_handler),
                    )
                    .route("/chat/sessions", web::get().to(list_sessions_handler))
                    .route(
                        "/chat/sessions/{id}/messages/{message_id}/envoy-delivery",
                        web::post().to(magician_api::chat_api::envoy_delivery_handler),
                    )
                    .route("/chat/sessions/{id}", web::get().to(get_session_handler))
                    .route(
                        "/chat/sessions/{id}/results/read",
                        web::post().to(read_chat_result_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/reference-catalog",
                        web::get().to(get_reference_catalog_handler),
                    )
                    .route(
                        "/chat/sessions/{id}",
                        web::patch().to(update_session_handler),
                    )
                    .route(
                        "/chat/sessions/{id}",
                        web::delete().to(delete_session_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/messages",
                        web::get().to(get_messages_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/messages",
                        web::post().to(send_message_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/tutor/user-action",
                        web::post().to(post_tutor_user_action_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/tutor/cancel",
                        web::post().to(post_tutor_cancel_handler),
                    )
                    // Display-only live transcript append (meeting bot) — persists +
                    // broadcasts a line WITHOUT dispatching the agent.
                    .route(
                        "/chat/sessions/{id}/transcript",
                        web::post().to(post_transcript_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/messages",
                        web::delete().to(clear_messages_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/messages/{message_id}",
                        web::delete().to(delete_message_handler),
                    )
                    // Phase 3.5a follow-up — per-session tailed-task
                    // accessors. GET returns the task_id this chat is
                    // currently tailing (drives "Watch live" vs
                    // "Stop watching" card affordances on reload);
                    // DELETE releases the tail (the "Stop watching"
                    // button).
                    .route(
                        "/chat/sessions/{id}/tailed-task",
                        web::get().to(get_tailed_task_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/tailed-task",
                        web::post().to(subscribe_to_tailed_task_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/tailed-task",
                        web::delete().to(delete_tailed_task_handler),
                    )
                    // Per-chat-turn activity events. Replaces the old
                    // /api/magician/v3/events/page?chat_turn_id=… path —
                    // backed by the per-turn JSONL projection written
                    // by `ChatTurnEventSink`.
                    .route(
                        "/chat/sessions/{session_id}/turns/{chat_turn_id}/events",
                        web::get().to(magician_api::chat_api::list_chat_turn_events_handler),
                    )
                    // Live tail of the same per-turn projection. SSE
                    // consumers subscribe via `ChatTurnEventSink::
                    // subscribe_live()`, demux by chat_turn_id, forward
                    // as NDJSON. Refresh + live agree by construction
                    // (sink is the sole filter applier).
                    .route(
                        "/chat/sessions/{session_id}/turns/{chat_turn_id}/events/stream",
                        web::get().to(magician_api::chat_api::stream_chat_turn_events_handler),
                    )
                    // Phase 1 — cancel in-flight chat turn
                    .route(
                        "/chat/sessions/{id}/run",
                        web::delete().to(cancel_chat_run_handler),
                    )
                    .route("/chat/sessions/{id}/queue", web::post().to(magician_api::chat_api::enqueue_composer_message_handler))
                    .route("/chat/sessions/{id}/queue/{message_id}/action", web::post().to(magician_api::chat_api::queued_message_action_handler))
                    // Phase 2 — pending-replay queue inspect / mutate
                    .route(
                        "/chat/sessions/{id}/queue",
                        web::get().to(list_queued_messages_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/queue",
                        web::delete().to(clear_queued_messages_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/queue/{message_id}",
                        web::delete().to(delete_queued_message_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/attachments",
                        web::post().to(upload_attachment_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/messages/stream",
                        web::post().to(send_message_stream_handler),
                    )
                    .route("/chat/sessions/{id}/voice/requests", web::post().to(submit_concurrent_voice_handler))
                    .route("/media/voice/requests", web::get().to(list_concurrent_voice_handler))
                    .route("/media/voice/requests/{id}/result", web::get().to(concurrent_voice_result_handler))
                    .route("/media/voice/requests/{id}/cancel", web::post().to(cancel_concurrent_voice_handler))
                    .route("/media/voice/delivery", web::post().to(concurrent_voice_delivery_handler))
                    .route(
                        "/chat/sessions/{id}/actions/invoke",
                        web::post().to(post_invoke_server_action_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/outputs/{relative_path:.*}",
                        web::get().to(get_session_output_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/outputs/open-folder",
                        web::post().to(open_output_folder_handler),
                    )
                    .route(
                        "/chat/sessions/{id}/outputs/open-file",
                        web::post().to(open_output_file_handler),
                    )
                    // Memory API routes used by chat/runtime clients.
                    .route("/chat/memory/search", web::post().to(memory_search_handler))
                    .route(
                        "/chat/memory/preference",
                        web::post().to(memory_save_preference_handler),
                    )
                    .route(
                        "/memory/effect-review",
                        web::get().to(get_memory_effect_review_handler),
                    )
                    .route(
                        "/memory/effect-review",
                        web::post().to(post_memory_effect_review_handler),
                    )
                    .route(
                        "/memory/entries",
                        web::get().to(list_user_memory_entries_handler),
                    )
                    .route(
                        "/memory/entries/{tier}/{key}/confirm",
                        web::post().to(confirm_user_memory_entry_handler),
                    )
                    .route(
                        "/memory/entries/{tier}/{key}/keep-conflict",
                        web::post().to(keep_user_memory_entry_conflict_handler),
                    )
                    .route(
                        "/memory/entries/{tier}/{key}/scope",
                        web::patch().to(patch_user_memory_entry_scope_handler),
                    )
                    // .configure(magician::magician_v2::chat::history_api::configure_routes)
                    .configure(magician_api::social_api::configure_social_routes);
                let scope = scope.app_data(shared_social_api_data.clone());
                let scope = scope
                    .configure(configure_secret_vault_routes)
                    .configure(configure_resource_authority_routes)
                    .configure(magician_api::approval_envelopes_api::configure_approval_envelope_routes)
                    .configure(magician_api::engagements_api::configure_engagement_routes)
                    .configure(magician_api::data_room_api::configure_data_room_routes)
                    .configure(magician_api::suppression_api::configure_suppression_routes)
                    .configure(
                        magician_api::transcript_claims_api::configure_transcript_claim_routes,
                    )
                    .configure(magician_api::corrections_api::configure_correction_routes)
                    .configure(
                        magician_api::recipient_compliance_api::configure_recipient_compliance_routes,
                    )
                    .configure(
                        magician_api::outcome_learning_api::configure_outcome_learning_routes,
                    )
                    .configure(magician_api::delivery_receipts_api::configure_delivery_mail_routes)
                    .configure(magician_api::counterparties_api::configure_counterparty_routes)
                    .configure(magician_api::work_modules_api::configure_work_module_routes)
                    .configure(magician_learning::outcome_learning::configure_outcome_learning_routes)
                    .configure(configure_skills_routes)
                    .configure(configure_component_routes)
                    .configure(configure_runtime_env_routes)
                    .configure(configure_notes_routes)
                    // Read-only mirror of the owner's taste profile. Its own
                    // route rather than a `/notes` child: the profile is a
                    // prompt-injection concern that happens to be stored as a
                    // note, and the mirrors ask "what is being injected", not
                    // "what notes exist".
                    .route(
                        "/taste-profile",
                        web::get().to(
                            magician_api::taste_profile_api::get_taste_profile_handler,
                        ),
                    )
                    // The review surface for capture. Sibling of the mirror
                    // above rather than a child of it: that one answers "what
                    // is being injected", these answer "what wants to be".
                    .route(
                        "/taste-proposals",
                        web::get().to(
                            magician_api::taste_capture_api::list_taste_proposals_handler,
                        ),
                    )
                    .route(
                        "/taste-proposals/{id}/approve",
                        web::post().to(
                            magician_api::taste_capture_api::approve_taste_proposal_handler,
                        ),
                    )
                    .route(
                        "/taste-proposals/{id}/reject",
                        web::post().to(
                            magician_api::taste_capture_api::reject_taste_proposal_handler,
                        ),
                    )
                    .configure(configure_workspace_storage_routes)
                    .configure(configure_tutor_routes)
                    // Server-published lane invoke-grammar catalog (plan
                    // 1.2): read-only, stateless, generated from the
                    // parser constants — the wire truth clients diff
                    // against, served beside the other static catalogs.
                    .configure(
                        magician_api::invoke_grammar_api::configure_invoke_grammar_routes,
                    )
                    .configure(magician_api::thinking_maps_api::configure)
                    .route("/realtime/ws", web::get().to(websocket_handler))
                    // Enrollment API routes (consumer channel identity)
                    .route("/llm/routing", web::get().to(llm_routing_overview_handler))
                    .route(
                        "/llm/routing/{operation}",
                        web::put().to(llm_routing_set_handler),
                    )
                    .route(
                        "/llm/routing/{operation}",
                        web::delete().to(llm_routing_clear_handler),
                    )
                    .route(
                        "/llm/routing/{operation}/engine",
                        web::put().to(llm_routing_engine_set_handler),
                    )
                    .route(
                        "/llm/routing/{operation}/engine",
                        web::delete().to(llm_routing_engine_clear_handler),
                    )
                    .route("/chat/enroll", web::post().to(enroll_handler))
                    .route("/chat/enroll/approve", web::post().to(approve_handler))
                    .route("/chat/enroll/revoke", web::post().to(revoke_handler))
                    .route("/chat/enroll/cancel", web::post().to(cancel_handler))
                    .route(
                        "/chat/enroll/status",
                        web::get().to(enrollment_status_handler),
                    )
                    // User-request service API routes (central ask-the-human)
                    .route("/user-requests", web::get().to(list_user_requests_handler))
                    .route(
                        "/user-requests/{id}/respond",
                        web::post().to(respond_user_request_handler),
                    )
                    // Phase H3 — canonical HITL respond endpoint. Body
                    // `{ source, value, channel? }` dispatches internally
                    // to the legacy resolve URLs above based on `source`.
                    // See `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md`.
                    .route(
                        "/hitl/{correlation_id}/respond",
                        web::post().to(respond_hitl_handler),
                    )
                    // Managed bot control plane for consumer channels
                    .route("/bots", web::get().to(list_bots_handler))
                    .route("/bots/auth", web::get().to(list_bots_auth_handler))
                    .route(
                        "/bots/auth/state",
                        web::get().to(get_bot_auth_state_handler),
                    )
                    .route("/bots/{name}/start", web::post().to(start_bot_handler))
                    .route("/bots/{name}/stop", web::post().to(stop_bot_handler))
                    .route("/bots/{name}/restart", web::post().to(restart_bot_handler))
                    .route("/bots/{name}/logs", web::get().to(get_bot_logs_handler))
                    .route("/bots/{name}/qr", web::get().to(get_bot_qr_handler))
                    .route("/bots/{name}/auth", web::get().to(get_bot_auth_handler))
                    .route(
                        "/bots/{name}/auth/start",
                        web::post().to(start_bot_auth_handler),
                    )
                    .route(
                        "/bots/{name}/auth/input",
                        web::post().to(submit_bot_auth_input_handler),
                    )
                    .route("/bots/{name}/env", web::get().to(get_bot_env_handler))
                    .route("/bots/{name}/env", web::put().to(put_bot_env_handler))
                    .route("/bots/{name}/config", web::get().to(get_bot_config_handler))
                    .route("/bots/{name}/config", web::put().to(put_bot_config_handler))
                    .route(
                        "/bots/{name}/config",
                        web::delete().to(delete_bot_config_handler),
                    )
                    // Progress channel subscription API
                    .route(
                        "/progress-channels/subscriptions/webhooks",
                        web::get().to(list_webhook_subscriptions_handler),
                    )
                    .route(
                        "/progress-channels/subscriptions/webhooks",
                        web::post().to(create_webhook_subscription_handler),
                    )
                    .route(
                        "/progress-channels/subscriptions/{id}",
                        web::delete().to(delete_progress_subscription_handler),
                    )
                    .route("/feed", web::get().to(list_feed_handler))
                    .route("/feed/attention", web::get().to(feed_attention_handler))
                    .route(
                        "/feed/attention/dismiss",
                        web::post().to(feed_attention_dismiss_handler),
                    )
                    .route(
                        "/feed/attention/undismiss",
                        web::post().to(feed_attention_undismiss_handler),
                    )
                    .route(
                        "/feed/attention/{item_id:.*}",
                        web::get().to(feed_attention_item_handler),
                    )
                    .route("/feed/counts", web::get().to(feed_counts_handler))
                    .route("/today", web::get().to(today_handler))
                    .route(
                        "/today/items/{item_id}/actions/{action_id}",
                        web::post().to(today_item_action_handler),
                    )
                    .route(
                        "/today/visibility",
                        web::get().to(today_visibility_list_handler),
                    )
                    .route(
                        "/today/items/{item_id}/visibility",
                        web::post().to(today_visibility_handler),
                    )
                    .route("/feed/items", web::delete().to(feed_clear_handler))
                    .route(
                        "/feed/items/{item_id}",
                        web::delete().to(feed_delete_item_handler),
                    )
                    .route(
                        "/feed/learnings/{candidate_id}/confirm",
                        web::post().to(feed_confirm_learning_candidate_handler),
                    )
                    .route(
                        "/feed/learnings/{candidate_id}/edit-confirm",
                        web::post().to(feed_edit_confirm_learning_candidate_handler),
                    )
                    .route(
                        "/feed/learnings/{candidate_id}/archive",
                        web::post().to(feed_archive_learning_candidate_handler),
                    )
                    .route(
                        "/feed/insights/{insight_id}/archive",
                        web::post().to(feed_archive_learning_insight_handler),
                    )
                    .route(
                        "/feed/insights/{insight_id}/save-to-memory",
                        web::post().to(feed_save_learning_insight_handler),
                    )
                    .route(
                        "/feed/insights/{insight_id}/create-follow-up",
                        web::post().to(feed_create_learning_insight_task_handler),
                    )
                    .route(
                        "/feed/purge-orphans",
                        web::post().to(feed_purge_orphans_handler),
                    )
                    // ── Realtime media + control rails (Phase 0) ──
                    .route(
                        "/media/sessions",
                        web::post().to(register_media_session_handler),
                    )
                    .route(
                        "/media/sessions",
                        web::get().to(list_media_sessions_handler),
                    )
                    .route(
                        "/media/sessions/{session_id}",
                        web::get().to(get_media_session_handler),
                    )
                    .route(
                        "/media/sessions/{session_id}",
                        web::patch().to(patch_media_session_handler),
                    )
                    .route(
                        "/media/sessions/{session_id}",
                        web::delete().to(delete_media_session_handler),
                    )
                    .route(
                        "/media/sessions/{session_id}/heartbeat",
                        web::post().to(heartbeat_media_session_handler),
                    )
                    .route(
                        "/media/sessions/{session_id}/events",
                        web::post().to(post_media_event_handler),
                    )
                    // ── Phase 3 — provider TTS / STT ──
                    .route(
                        "/media/providers",
                        web::get().to(list_media_providers_handler),
                    )
                    .route(
                        "/media/preferences",
                        web::get().to(get_media_preferences_handler),
                    )
                    .route(
                        "/media/preferences",
                        web::put().to(put_media_preferences_handler),
                    )
                    .route(
                        "/media/audio-settings",
                        web::get().to(get_audio_settings_handler),
                    )
                    .route(
                        "/media/audio-settings",
                        web::put().to(put_audio_settings_handler),
                    )
                    .route(
                        "/media/audio-engines/{engine_id}/models/{action}",
                        web::post().to(post_audio_engine_model_control_handler),
                    )
                    .route(
                        "/media/surfaces/{surface}/resolved",
                        web::get().to(get_resolved_audio_surface_handler),
                    )
                    .route(
                        "/ui/preferences",
                        web::get().to(get_ui_preferences_handler),
                    )
                    .route(
                        "/ui/preferences",
                        web::put().to(put_ui_preferences_handler),
                    )
                    .route("/media/tts/synthesize", web::post().to(synthesize_tts_handler))
                    .route(
                        "/media/tts/synthesize_message",
                        web::post().to(synthesize_message_handler),
                    )
                    .route(
                        "/media/tts/cache/clear",
                        web::post().to(clear_tts_cache_handler),
                    )
                    .route(
                        "/media/tts/cache/stats",
                        web::get().to(tts_cache_stats_handler),
                    )
                    .route("/media/stt/transcribe", web::post().to(transcribe_stt_handler))
                    .route(
                        "/media/stt/transcribe/stream",
                        web::post().to(transcribe_stt_stream_handler),
                    )
                    .route("/media/voice-notes", web::post().to(submit_voice_note_handler))
                    .route(
                        "/media/voice-notes/events",
                        web::post().to(post_voice_note_event_handler),
                    )
                    // ── Realtime voice control surface (R4) ───────────
                    // ONE bidirectional WebSocket per call. The
                    // backend `VoiceOrchestrator` owns lifecycle
                    // (mint, rotate, compact, reconnect, replay) and
                    // streams events back through this channel.
                    // Provider negotiation lives in
                    // `magicllm::realtime` and the YAML config under
                    // `realtime_voice` (R1 + R2). Frontend sees one
                    // endpoint regardless of provider topology.
                    .route(
                        "/media/voice/{voice_session_id}/control",
                        web::get().to(
                            magician_api::voice_control_handler::voice_control_ws_handler,
                        ),
                    )
                    // ── Tray bridge ──
                    .route(
                        "/media/bridge/{session_id}/ws",
                        web::get().to(tray_bridge_ws_handler),
                    )
                    // ── Device bridge: the socket a companion device opens ──
                    .route(
                        "/devices/bridge",
                        web::get().to(
                            magician_api::device_bridge_handler::device_bridge_ws_handler,
                        ),
                    )
                    // Magician Edge: an enrolled desktop connects outward and
                    // advertises only the local capabilities available on its
                    // current macOS, Windows, or Linux host.
                    .route(
                        "/edge/bridge",
                        web::get().to(
                            magician_api::edge_bridge_handler::edge_bridge_ws_handler,
                        ),
                    )
                    .route(
                        "/edge/devices",
                        web::get().to(
                            magician_api::edge_bridge_handler::edge_devices_handler,
                        ),
                    )
                    .route(
                        "/edge/devices/{device_id}/invoke",
                        web::post().to(
                            magician_api::edge_bridge_handler::edge_invoke_handler,
                        ),
                    )
                    // Enrolment. The socket above refuses anything it does not
                    // recognise, so without these a device can never become
                    // recognised — which is exactly what happened until now.
                    .route(
                        "/devices/pair",
                        web::post()
                            .to(magician_api::device_pairing_api::pair_device_handler),
                    )
                    .route(
                        "/devices/enrollment",
                        web::post().to(
                            magician_api::device_pairing_api::begin_device_enrollment_handler,
                        ),
                    )
                    .route(
                        "/devices/enrollment/exchange",
                        web::post().to(
                            magician_api::device_pairing_api::exchange_device_enrollment_handler,
                        ),
                    )
                    .route(
                        "/devices/enrollment/{enrollment_id}",
                        web::delete().to(
                            magician_api::device_pairing_api::cancel_device_enrollment_handler,
                        ),
                    )
                    .route(
                        "/devices",
                        web::get()
                            .to(magician_api::device_pairing_api::list_devices_handler),
                    )
                    .route(
                        "/devices/me",
                        web::get().to(
                            magician_api::device_pairing_api::get_mobile_device_handler,
                        ),
                    )
                    .route(
                        "/devices/me/push",
                        web::put().to(
                            magician_api::mobile_push_api::put_mobile_push_handler,
                        ),
                    )
                    .route(
                        "/devices/me/push",
                        web::delete().to(
                            magician_api::mobile_push_api::delete_mobile_push_handler,
                        ),
                    )
                    // Phase 5 governance: the scope-wide screenshot switch and
                    // the reviewable trail of device actions. Registered before
                    // the `{device_id}` route so "policy" and "audit" are never
                    // captured as device ids.
                    .route(
                        "/devices/policy",
                        web::get().to(
                            magician_api::device_pairing_api::get_device_policy_handler,
                        ),
                    )
                    .route(
                        "/devices/policy",
                        web::put().to(
                            magician_api::device_pairing_api::put_device_policy_handler,
                        ),
                    )
                    // Secure HITL P6: which paired devices may read
                    // verification codes for a live challenge.
                    .route(
                        "/devices/policy/verification-codes",
                        web::put().to(magician_api::verification_codes_api::put_device_verification_codes_handler),
                    )
                    .route(
                        "/devices/audit",
                        web::get().to(
                            magician_api::device_pairing_api::device_audit_handler,
                        ),
                    )
                    .configure(
                        magician_api::android_apps_owner_api::configure_android_apps_owner_routes,
                    )
                    .route(
                        "/devices/{device_id}",
                        web::delete().to(
                            magician_api::device_pairing_api::unpair_device_handler,
                        ),
                    )
                    .route("/history/search", web::get().to(search_history_handler))
                    .route("/ui-threads", web::get().to(list_ui_threads_handler))
                    .route("/ui-threads", web::post().to(create_ui_thread_handler))
                    .route(
                        "/ui-threads/reorder",
                        web::post().to(reorder_ui_threads_handler),
                    )
                    .route("/ui-threads/{id}", web::get().to(get_ui_thread_handler))
                    .route(
                        "/ui-threads/{id}",
                        web::patch().to(update_ui_thread_handler),
                    )
                    .route(
                        "/ui-threads/{id}",
                        web::delete().to(delete_ui_thread_handler),
                    )
                    .route(
                        "/executions/{id}/execution-panel",
                        web::get().to(get_execution_panel_handler),
                    )
                    // Evals API. The specific paths are registered BEFORE
                    // `/evals/{lane}/run` so a lane can never shadow one of
                    // them, and the report passthrough is what makes a run's
                    // `report_href` a link the browser can follow rather than a
                    // path on the box that produced it.
                    .route(
                        "/evals/lanes",
                        web::get().to(magician_api::evals_api::list_lanes_handler),
                    )
                    .route(
                        "/evals/runs",
                        web::get().to(magician_api::evals_api::list_runs_handler),
                    )
                    .route(
                        "/evals/spend",
                        web::get().to(magician_api::evals_api::spend_handler),
                    )
                    .route(
                        "/evals/report/{tail:.*}",
                        web::get().to(magician_api::evals_api::report_handler),
                    )
                    .route(
                        "/evals/web-researcher/judge",
                        web::post().to(
                            magician_api::evals_api::judge_web_research_answer_handler,
                        ),
                    )
                    .route(
                        "/evals/{lane}/run",
                        web::post().to(magician_api::evals_api::run_lane_handler),
                    )
                    // Analytics query API (returns 503 when analytics is not initialized)
                    .route(
                        "/analytics/schema",
                        web::get()
                            .to(magician_api::analytics_api::get_schema_handler),
                    )
                    .route(
                        "/analytics/query",
                        web::post().to(magician_api::analytics_api::query_handler),
                    )
                    .route(
                        "/analytics/llm_calls/query",
                        web::post().to(
                            magician_api::analytics_api::query_llm_calls_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm_calls/query_batch",
                        web::post().to(
                            magician_api::analytics_api::query_llm_calls_batch_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm_embeddings/query",
                        web::post().to(
                            magician_api::analytics_api::query_llm_embeddings_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/overview",
                        web::get().to(
                            magician_api::analytics_api::llm_observability_overview_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/catalog",
                        web::get().to(
                            magician_api::analytics_api::llm_fact_catalog_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/schema",
                        web::get().to(
                            magician_api::analytics_api::llm_fact_schema_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/calls",
                        web::get().to(
                            magician_api::analytics_api::list_llm_calls_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/traces",
                        web::get().to(
                            magician_api::analytics_api::list_llm_traces_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/traces/{trace_id}",
                        web::get().to(
                            magician_api::analytics_api::read_llm_trace_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/calls/{llm_call_id}",
                        web::get().to(
                            magician_api::analytics_api::read_llm_call_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/provider-attempts",
                        web::get().to(
                            magician_api::analytics_api::list_llm_provider_attempts_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/provider-attempts/{provider_attempt_id}",
                        web::get().to(
                            magician_api::analytics_api::read_llm_provider_attempt_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/facts/query",
                        web::post().to(
                            magician_api::analytics_api::query_llm_facts_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/content/grants",
                        web::post().to(
                            magician_api::analytics_api::issue_llm_content_grant_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/content/calls/{llm_call_id}",
                        web::get().to(
                            magician_api::analytics_api::read_llm_call_content_handler,
                        ),
                    )
                    .route(
                        "/analytics/llm/content/calls/{llm_call_id}",
                        web::delete().to(
                            magician_api::analytics_api::delete_llm_call_content_handler,
                        ),
                    )
                    .route(
                        "/analytics/memory_events/query",
                        web::post().to(
                            magician_api::analytics_api::query_memory_events_handler,
                        ),
                    )
                    .route(
                        "/analytics/memory_events/query_batch",
                        web::post().to(
                            magician_api::analytics_api::query_memory_events_batch_handler,
                        ),
                    )
                    // The activity spine's read path. `/runtime` already uses
                    // these endpoints for durable backfill/seam continuity.
                    // `/llm` repoint is intentionally deferred (the current
                    // economics view is still on `llm_calls`).
                    .route(
                        "/analytics/activity_rows/query",
                        web::post().to(
                            magician_api::analytics_api::query_activity_rows_handler,
                        ),
                    )
                    .route(
                        "/analytics/activity_rows/query_batch",
                        web::post().to(
                            magician_api::analytics_api::query_activity_rows_batch_handler,
                        ),
                    )
                    .route(
                        "/analytics/memory_events/evals/run",
                        web::post().to(
                            magician_api::analytics_api::run_memory_evals_handler,
                        ),
                    )
                    .route(
                        "/memory/regression/status",
                        web::get().to(
                            magician_api::analytics_api::memory_regression_status_handler,
                        ),
                    )
                    .route(
                        "/memory/index/status",
                        web::get().to(
                            magician_api::analytics_api::memory_index_status_handler,
                        ),
                    )
                    .route(
                        "/memory/temperature/status",
                        web::get().to(
                            magician_api::analytics_api::memory_temperature_status_handler,
                        ),
                    )
                    .route(
                        "/memory/temperature/maintain",
                        web::post().to(
                            magician_api::analytics_api::maintain_memory_temperature_handler,
                        ),
                    )
                    .route(
                        "/memory/index/rebuild",
                        web::post().to(
                            magician_api::analytics_api::rebuild_memory_index_handler,
                        ),
                    )
                    .route(
                        "/learning/audit",
                        web::get().to(
                            magician_api::learning_api::get_learning_audit_handler,
                        ),
                    )
                    .route(
                        "/learning/candidates",
                        web::get().to(
                            magician_api::learning_api::list_learning_candidates_handler,
                        ),
                    )
                    .route(
                        "/learning/candidates",
                        web::post().to(
                            magician_api::learning_api::create_learning_candidate_handler,
                        ),
                    )
                    .route(
                        "/learning/candidates/{id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_candidate_handler,
                        ),
                    )
                    .route(
                        "/learning/candidates/{id}/state",
                        web::post().to(
                            magician_api::learning_api::transition_learning_candidate_handler,
                        ),
                    )
                    .route(
                        "/learning/candidates/{id}/harness-profile-evaluation",
                        web::post().to(
                            magician_api::learning_api::record_harness_profile_evaluation_handler,
                        ),
                    )
                    .route(
                        "/learning/evaluations",
                        web::get().to(
                            magician_api::learning_api::list_learning_evaluations_handler,
                        ),
                    )
                    .route(
                        "/learning/evaluations/runs",
                        web::get().to(
                            magician_api::learning_api::list_learning_evaluation_runs_handler,
                        ),
                    )
                    .route(
                        "/learning/evaluations/runs/{candidate_id}/{run_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_evaluation_run_handler,
                        ),
                    )
                    .route(
                        "/learning/evaluations/{candidate_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_evaluation_handler,
                        ),
                    )
                    .route(
                        "/learning/evaluations/{candidate_id}/run",
                        web::post().to(
                            magician_api::learning_api::run_learning_evaluation_handler,
                        ),
                    )
                    .route(
                        "/learning/growth-evaluations",
                        web::get().to(
                            magician_api::learning_api::list_learning_growth_evaluation_runs_handler,
                        ),
                    )
                    .route(
                        "/learning/growth-evaluations/run",
                        web::post().to(
                            magician_api::learning_api::run_learning_growth_evaluation_handler,
                        ),
                    )
                    .route(
                        "/learning/growth-evaluations/{run_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_growth_evaluation_run_handler,
                        ),
                    )
                    .route(
                        "/learning/procedures",
                        web::get().to(
                            magician_api::learning_api::list_learning_procedures_handler,
                        ),
                    )
                    .route(
                        "/learning/procedures",
                        web::post().to(
                            magician_api::learning_api::create_learning_procedure_handler,
                        ),
                    )
                    .route(
                        "/learning/procedures/{procedure_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_procedure_handler,
                        ),
                    )
                    .route(
                        "/learning/procedures/{procedure_id}/status",
                        web::post().to(
                            magician_api::learning_api::transition_learning_procedure_handler,
                        ),
                    )
                    .route(
                        "/learning/procedures/{procedure_id}/skill-promotion",
                        web::post().to(
                            magician_api::learning_api::promote_learning_procedure_to_skill_handler,
                        ),
                    )
                    .service(
                        web::scope("/learning/skill-evolution")
                            .route(
                                "",
                                web::get().to(
                                    magician_api::learning_api::list_learning_capability_evolution_handler,
                                ),
                            )
                            .route(
                                "/steward/run",
                                web::post().to(
                                    magician_api::learning_api::run_learning_capability_evolution_steward_handler,
                                ),
                            )
                            .route(
                                "/steward/runs",
                                web::get().to(
                                    magician_api::learning_api::list_learning_capability_evolution_steward_runs_handler,
                                ),
                            )
                            .route(
                                "/steward/runs/{run_id}",
                                web::get().to(
                                    magician_api::learning_api::get_learning_capability_evolution_steward_run_handler,
                                ),
                            )
                            .route(
                                "/proposals",
                                web::get().to(
                                    magician_api::learning_api::list_learning_capability_evolution_proposals_handler,
                                ),
                            )
                            .route(
                                "/proposals/draft",
                                web::post().to(
                                    magician_api::learning_api::draft_learning_capability_evolution_proposals_handler,
                                ),
                            )
                            .route(
                                "/proposals/{candidate_id}",
                                web::get().to(
                                    magician_api::learning_api::get_learning_capability_evolution_proposal_handler,
                                ),
                            )
                            .route(
                                "/proposals/{candidate_id}/decision",
                                web::post().to(
                                    magician_api::learning_api::decide_learning_capability_evolution_proposal_handler,
                                ),
                            )
                            .route(
                                "/proposals/{candidate_id}/evaluation/generate",
                                web::post().to(
                                    magician_api::learning_api::generate_learning_capability_evolution_evaluation_handler,
                                ),
                            )
                            .route(
                                "/proposals/{candidate_id}/validation",
                                web::post().to(
                                    magician_api::learning_api::record_learning_capability_evolution_validation_handler,
                                ),
                            )
                            .route(
                                "/proposals/{candidate_id}/validation/run",
                                web::post().to(
                                    magician_api::learning_api::run_learning_capability_evolution_validation_handler,
                                ),
                            )
                            .route(
                                "/proposals/{candidate_id}/implementation/draft",
                                web::post().to(
                                    magician_api::learning_api::draft_learning_capability_evolution_implementation_handler,
                                ),
                            )
                            .route(
                                "/proposals/{candidate_id}/implementation",
                                web::post().to(
                                    magician_api::learning_api::record_learning_capability_evolution_implementation_handler,
                                ),
                            )
                            .route(
                                "/proposals/{candidate_id}/promotion",
                                web::post().to(
                                    magician_api::learning_api::record_learning_capability_evolution_promotion_handler,
                                ),
                            )
                            .route(
                                "/validations",
                                web::get().to(
                                    magician_api::learning_api::list_learning_capability_evolution_validations_handler,
                                ),
                            )
                            .route(
                                "/validations/{candidate_id}/{validation_id}",
                                web::get().to(
                                    magician_api::learning_api::get_learning_capability_evolution_validation_handler,
                                ),
                            )
                            .route(
                                "/implementations",
                                web::get().to(
                                    magician_api::learning_api::list_learning_capability_evolution_implementations_handler,
                                ),
                            )
                            .route(
                                "/implementations/{candidate_id}/{implementation_id}",
                                web::get().to(
                                    magician_api::learning_api::get_learning_capability_evolution_implementation_handler,
                                ),
                            )
                            .route(
                                "/implementations/{candidate_id}/{implementation_id}/apply",
                                web::post().to(
                                    magician_api::learning_api::apply_learning_capability_evolution_implementation_handler,
                                ),
                            )
                            .route(
                                "/applications",
                                web::get().to(
                                    magician_api::learning_api::list_learning_capability_evolution_applications_handler,
                                ),
                            )
                            .route(
                                "/applications/{candidate_id}/{application_id}",
                                web::get().to(
                                    magician_api::learning_api::get_learning_capability_evolution_application_handler,
                                ),
                            )
                            .route(
                                "/rollback-recommendations",
                                web::get().to(
                                    magician_api::learning_api::list_learning_capability_evolution_rollback_recommendations_handler,
                                ),
                            )
                            .route(
                                "/rollback-recommendations/{candidate_id}/{recommendation_id}",
                                web::get().to(
                                    magician_api::learning_api::get_learning_capability_evolution_rollback_recommendation_handler,
                                ),
                            )
                            .route(
                                "/rollback-recommendations/{candidate_id}/{recommendation_id}/decision",
                                web::post().to(
                                    magician_api::learning_api::decide_learning_capability_evolution_rollback_recommendation_handler,
                                ),
                            )
                            .route(
                                "/promotions",
                                web::get().to(
                                    magician_api::learning_api::list_learning_capability_evolution_promotions_handler,
                                ),
                            )
                            .route(
                                "/promotions/{promotion_id}",
                                web::get().to(
                                    magician_api::learning_api::get_learning_capability_evolution_promotion_handler,
                                ),
                            )
                            .route(
                                "/post-promotion-monitors",
                                web::get().to(
                                    magician_api::learning_api::list_learning_capability_evolution_post_promotion_monitors_handler,
                                ),
                            )
                            .route(
                                "/post-promotion-monitors/{promotion_id}",
                                web::get().to(
                                    magician_api::learning_api::get_learning_capability_evolution_post_promotion_monitor_handler,
                                ),
                            )
                            .route(
                                "/promotions/{promotion_id}/monitor",
                                web::post().to(
                                    magician_api::learning_api::run_learning_capability_evolution_post_promotion_monitor_handler,
                                ),
                            )
                            .route(
                                "/{candidate_id}",
                                web::get().to(
                                    magician_api::learning_api::get_learning_capability_evolution_handler,
                                ),
                            )
                            .route(
                                "/{candidate_id}/proposal",
                                web::post().to(
                                    magician_api::learning_api::upsert_learning_capability_evolution_proposal_handler,
                                ),
                            ),
                    )
                    .route(
                        "/learning/capability-evolution",
                        web::get().to(
                            magician_api::learning_api::list_learning_capability_evolution_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/steward/run",
                        web::post().to(
                            magician_api::learning_api::run_learning_capability_evolution_steward_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/steward/runs",
                        web::get().to(
                            magician_api::learning_api::list_learning_capability_evolution_steward_runs_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/steward/runs/{run_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_capability_evolution_steward_run_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals",
                        web::get().to(
                            magician_api::learning_api::list_learning_capability_evolution_proposals_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals/draft",
                        web::post().to(
                            magician_api::learning_api::draft_learning_capability_evolution_proposals_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals/{candidate_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_capability_evolution_proposal_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals/{candidate_id}/decision",
                        web::post().to(
                            magician_api::learning_api::decide_learning_capability_evolution_proposal_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals/{candidate_id}/evaluation/generate",
                        web::post().to(
                            magician_api::learning_api::generate_learning_capability_evolution_evaluation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals/{candidate_id}/validation",
                        web::post().to(
                            magician_api::learning_api::record_learning_capability_evolution_validation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals/{candidate_id}/validation/run",
                        web::post().to(
                            magician_api::learning_api::run_learning_capability_evolution_validation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals/{candidate_id}/implementation/draft",
                        web::post().to(
                            magician_api::learning_api::draft_learning_capability_evolution_implementation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals/{candidate_id}/implementation",
                        web::post().to(
                            magician_api::learning_api::record_learning_capability_evolution_implementation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/proposals/{candidate_id}/promotion",
                        web::post().to(
                            magician_api::learning_api::record_learning_capability_evolution_promotion_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/validations",
                        web::get().to(
                            magician_api::learning_api::list_learning_capability_evolution_validations_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/validations/{candidate_id}/{validation_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_capability_evolution_validation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/implementations",
                        web::get().to(
                            magician_api::learning_api::list_learning_capability_evolution_implementations_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/implementations/{candidate_id}/{implementation_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_capability_evolution_implementation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/implementations/{candidate_id}/{implementation_id}/apply",
                        web::post().to(
                            magician_api::learning_api::apply_learning_capability_evolution_implementation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/applications",
                        web::get().to(
                            magician_api::learning_api::list_learning_capability_evolution_applications_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/applications/{candidate_id}/{application_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_capability_evolution_application_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/rollback-recommendations",
                        web::get().to(
                            magician_api::learning_api::list_learning_capability_evolution_rollback_recommendations_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/rollback-recommendations/{candidate_id}/{recommendation_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_capability_evolution_rollback_recommendation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/rollback-recommendations/{candidate_id}/{recommendation_id}/decision",
                        web::post().to(
                            magician_api::learning_api::decide_learning_capability_evolution_rollback_recommendation_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/promotions",
                        web::get().to(
                            magician_api::learning_api::list_learning_capability_evolution_promotions_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/promotions/{promotion_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_capability_evolution_promotion_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/post-promotion-monitors",
                        web::get().to(
                            magician_api::learning_api::list_learning_capability_evolution_post_promotion_monitors_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/post-promotion-monitors/{promotion_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_capability_evolution_post_promotion_monitor_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/promotions/{promotion_id}/monitor",
                        web::post().to(
                            magician_api::learning_api::run_learning_capability_evolution_post_promotion_monitor_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/{candidate_id}",
                        web::get().to(
                            magician_api::learning_api::get_learning_capability_evolution_handler,
                        ),
                    )
                    .route(
                        "/learning/capability-evolution/{candidate_id}/proposal",
                        web::post().to(
                            magician_api::learning_api::upsert_learning_capability_evolution_proposal_handler,
                        ),
                    )
                    .route(
                        "/learning/events",
                        web::get().to(
                            magician_api::learning_api::list_learning_events_handler,
                        ),
                    )
                    .route(
                        "/learning/events",
                        web::post().to(
                            magician_api::learning_api::create_learning_event_handler,
                        ),
                    )
                    .route(
                        "/harness/cycles",
                        web::get().to(
                            magician_api::learning_api::list_harness_cycles_handler,
                        ),
                    )
                    .route(
                        "/harness/anomalies",
                        web::get().to(
                            magician_api::learning_api::list_harness_anomalies_handler,
                        ),
                    )
                    .route(
                        "/harness/program-state/revert",
                        web::post().to(
                            magician_api::learning_api::revert_harness_program_state_handler,
                        ),
                    )
                    .route(
                        "/learning/teaching",
                        web::post().to(
                            magician_api::learning_api::record_learning_teaching_feedback_handler,
                        ),
                    )
                    .route(
                        "/dashboard_themes",
                        web::get().to(
                            magician_api::dashboard_themes_api::list_themes_handler,
                        ),
                    )
                    .route(
                        "/dashboard_themes/{id}",
                        web::get().to(
                            magician_api::dashboard_themes_api::get_theme_handler,
                        ),
                    );
                scope
            })
            .service(
                web::scope("/api/magician/v3")
                    .wrap(from_fn(api_cors_middleware))
                    .wrap(from_fn(
                        magician::magician_v2::cloudflare_access::verify_access_middleware,
                    ))
                    // Auth gate — same wrap, same order as the v2 scope
                    // above: registered last so it runs FIRST (before the
                    // Cloudflare Access layer), resolves the bearer
                    // against the unified store, and engraves proven
                    // internal x-principal/x-workspace context. The whole v3
                    // task plane therefore consumes proven scope context, not
                    // caller-controlled identity headers. In open mode, an
                    // absent bearer is fixed to anonymous/default.
                    // docs/components/magician/auth.md
                    .wrap(from_fn(
                        magician::magician_v2::auth::middleware::authenticate_request,
                    ))
                    .app_data(shared_task_api_v3.clone())
                    .app_data(shared_events_api.clone())
                    .route("/events", web::get().to(list_events_v3_handler))
                    // DEBUG-only synthetic notification injector (gated to
                    // debug builds or MAGICIAN_DEBUG_EVENTS) — feeds the
                    // desktop notify-overlay via the live broadcaster.
                    .route(
                        "/events/debug-emit",
                        web::post().to(debug_emit_event_handler),
                    )
                    // /events/page deleted — chat surfaces now use
                    // /api/magician/v2/chat/sessions/{sid}/turns/{cid}/events
                    // (backed by the per-chat-turn projection sink).
                    .route("/tasks", web::get().to(list_tasks_v3_handler))
                    .route("/tasks", web::post().to(create_task_v3_handler))
                    .route(
                        "/tasks/internal",
                        web::get().to(list_internal_tasks_v3_handler),
                    )
                    .route(
                        "/tasks/internal/{task_id}",
                        web::delete().to(delete_internal_task_v3_handler),
                    )
                    // ── Recurring Monitors (Phase 1) ──
                    // Scoped product routes over the SAME task service:
                    // create/edit go through create_task/update_task (the
                    // spec arm owns monitor_revision), pause/resume mutate
                    // TaskSchedule.paused, run/delete reuse the exact
                    // execute/delete task paths. See
                    // docs/components/magician/recurring-monitors.md.
                    .route("/monitors", web::post().to(create_monitor_v3_handler))
                    .route("/monitors", web::get().to(list_monitors_v3_handler))
                    .route(
                        "/monitors/{task_id}",
                        web::get().to(get_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}",
                        web::patch().to(update_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}",
                        web::delete().to(delete_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/pause",
                        web::post().to(pause_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/resume",
                        web::post().to(resume_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/run",
                        web::post().to(run_monitor_v3_handler),
                    )
                    // Phase 7: explicit conversion of an eligible existing
                    // task into a monitor (spec attach through the same
                    // update_task arm; the task keeps its id, schedule, and
                    // history). User-explicit only — never inferred.
                    .route(
                        "/monitors/{task_id}/convert",
                        web::post().to(convert_task_to_monitor_v3_handler),
                    )
                    // Phase 2 change-ledger read seam: accepted
                    // MonitorRunResultV1 records, newest first.
                    .route(
                        "/monitors/{task_id}/runs",
                        web::get().to(get_monitor_runs_v3_handler),
                    )
                    // Phase 3 notification ledger: durable
                    // MonitorUpdateDetailV1 records (per-monitor + scope-wide),
                    // newest first.
                    .route(
                        "/monitors/{task_id}/updates",
                        web::get().to(get_monitor_updates_v3_handler),
                    )
                    .route(
                        "/monitor-updates",
                        web::get().to(list_monitor_updates_v3_handler),
                    )
                    // Phase 6: useful/not-relevant feedback (durable
                    // per-task ledger, plan §10) + aggregate metrics (§12).
                    .route(
                        "/monitors/{task_id}/updates/{update_id}/feedback",
                        web::post().to(post_monitor_feedback_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/feedback",
                        web::get().to(get_monitor_feedback_v3_handler),
                    )
                    .route(
                        "/monitors-metrics",
                        web::get().to(get_monitors_metrics_v3_handler),
                    )
                    .route("/tasks/{id}", web::put().to(update_task_v3_handler))
                    .route("/tasks/{id}", web::delete().to(delete_task_v3_handler))
                    .route(
                        "/tasks/{task_id}/details",
                        web::get().to(get_task_details_v3_handler),
                    )
                    .route(
                        "/published-surfaces",
                        web::get().to(list_published_surfaces_v3_handler),
                    )
                    .route(
                        "/published-surfaces",
                        web::post().to(publish_surface_v3_handler),
                    )
                    .route(
                        "/published-surfaces/projections",
                        web::get().to(list_published_surface_projections_v3_handler),
                    )
                    .route(
                        "/published-surfaces/top-feed",
                        web::get().to(get_published_surface_top_feed_v3_handler),
                    )
                    .route(
                        "/published-surfaces/{surface_id}",
                        web::get().to(get_published_surface_v3_handler),
                    )
                    .route(
                        "/published-surfaces/{surface_id}/render",
                        web::get().to(get_published_surface_render_v3_handler),
                    )
                    .route(
                        "/published-surfaces/{surface_id}/unpublish",
                        web::post().to(unpublish_surface_v3_handler),
                    )
                    .route(
                        "/published-surfaces/{surface_id}/republish",
                        web::post().to(republish_surface_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/resynthesize_user_output",
                        web::post().to(resynthesize_task_user_output_v3_handler),
                    )
                    .route("/tasks/{id}", web::get().to(get_task_v3_handler))
                    .route(
                        "/tasks/{id}/status",
                        web::put().to(update_task_status_v3_handler),
                    )
                    // ── PlanGraph per-task persistence endpoints ──
                    // Naming uses the legacy "plan" segment; the actual
                    // payload is a `PlanGraph` wrapped in a `TaskPlanRecord`
                    // envelope. See `artifact_v2/models.rs` for the full
                    // story behind the naming. These endpoints serve the
                    // Plan-mode UI (Plan / Replan / Approve / Reject /
                    // edit-plan-graph). Do-mode delegations never produce
                    // a record — `GET …/plan` returns 404
                    // `task_plan_not_found:<id>` for those, and the UI
                    // guards on `task.hasPlan` before fetching.
                    .route(
                        "/tasks/{id}/plan",
                        web::post().to(start_task_planning_v3_handler),
                    )
                    .route("/tasks/{id}/plan", web::get().to(get_task_plan_v3_handler))
                    .route(
                        "/tasks/{id}/plan",
                        web::put().to(update_task_plan_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/versions",
                        web::get().to(list_task_plan_versions_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/versions/{epoch}",
                        web::get().to(get_task_plan_version_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/versions/{epoch}",
                        web::delete().to(delete_task_plan_version_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/versions/{epoch}/restore",
                        web::post().to(restore_task_plan_version_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/approve",
                        web::post().to(approve_task_plan_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/reject",
                        web::post().to(reject_task_plan_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/replan",
                        web::post().to(replan_task_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/analyze",
                        web::post().to(analyze_task_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/analysis",
                        web::get().to(get_task_plan_analysis_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/slots",
                        web::get().to(get_task_plan_slots_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/attempts",
                        web::get().to(get_task_plan_attempts_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/clarifications",
                        web::get().to(get_task_plan_clarifications_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/clarifications/pending",
                        web::get().to(get_task_plan_pending_questions_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/plan/clarifications/resume",
                        web::post().to(resume_task_plan_clarifications_v3_handler),
                    )
                    .route(
                        "/tasks/{task_id}/plan/clarifications/{question_id}/respond",
                        web::post().to(submit_task_plan_clarification_v3_handler),
                    )
                    .route(
                        "/plans/clarifications/pending",
                        web::get().to(list_pending_task_plan_clarifications_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/approve",
                        web::post().to(approve_task_v3_handler),
                    )
                    .route("/tasks/{id}/refs", web::get().to(get_task_refs_v3_handler))
                    .route(
                        "/tasks/{id}/results/read",
                        web::post().to(read_task_result_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/outputs",
                        web::get().to(get_task_outputs_v3_handler),
                    )
                    // Portable Export — ZIP bundle / single self-contained HTML
                    // of the task's shareable outputs. Distinct `/export` segment,
                    // so it can't be swallowed by the `/outputs/{…}` catchall below.
                    .route(
                        "/tasks/{id}/export",
                        web::get().to(export_task_outputs_v3_handler),
                    )
                    // POST routes for OS-level file ops MUST be
                    // registered before the catchall download GET below
                    // — Actix matches in registration order and the
                    // catchall `{artifact_path:.*}` would otherwise
                    // swallow `/open-folder` / `/open-file` paths too.
                    // File path comes in the JSON body, not the URL.
                    .route(
                        "/tasks/{id}/outputs/open-folder",
                        web::post().to(open_task_output_folder_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/outputs/open-file",
                        web::post().to(open_task_output_file_v3_handler),
                    )
                    .route(
                        "/tasks/{task_id}/outputs/{artifact_path:.*}",
                        web::get().to(download_task_output_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/progress",
                        web::get().to(get_task_progress_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/executions",
                        web::get().to(list_executions_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/execution-tree",
                        web::get().to(get_execution_tree_v3_handler),
                    )
                    .route(
                        "/tasks/{id}/execution-panel",
                        web::get().to(get_task_execution_panel_handler),
                    )
                    .route(
                        "/tasks/{id}/execute",
                        web::post().to(execute_task_v3_handler),
                    )
                    .route(
                        "/tasks/{task_id}/executions/{execution_id}",
                        web::get().to(get_execution_v3_handler),
                    )
                    .route(
                        "/executions/{id}/execution-panel",
                        web::get().to(get_execution_panel_handler),
                    )
                    .route(
                        "/executions/{id}/cancel",
                        web::post().to(cancel_execution_v3_handler),
                    )
                    .route(
                        "/tasks/{task_id}/executions/{execution_id}/retry-synthesis",
                        web::post().to(retry_synthesis_v3_handler),
                    )
                    .route(
                        "/executions/{execution_id}/artifacts/{artifact_path:.*}",
                        web::get().to(download_artifact_v3_handler),
                    )
                    .route(
                        "/tasks/{task_id}/executions/{execution_id}/refs",
                        web::get().to(get_execution_refs_v3_handler),
                    )
                    .route(
                        "/tasks/{task_id}/executions/{execution_id}/outputs",
                        web::get().to(get_execution_outputs_v3_handler),
                    )
                    .route(
                        "/tasks/{task_id}/executions/{execution_id}/delegations",
                        web::get().to(get_execution_delegations_v3_handler),
                    )
                    .route(
                        "/tasks/{task_id}/executions/{execution_id}/schedule",
                        web::get().to(get_execution_schedule_v3_handler),
                    ),
            )
            .service(web::scope("/api/magician").route("/mode", web::get().to(get_mode_handler)));

        if let Some(dir) = static_dir_for_server.clone() {
            let index_fallback = dir.join("index.html");
            app = app.service(
                Files::new("/", dir.as_ref())
                    .index_file("index.html")
                    .prefer_utf8(true)
                    .default_handler(move |req: ServiceRequest| {
                        let index = index_fallback.clone();
                        async move {
                            match NamedFile::open(index.clone()) {
                                Ok(file) => {
                                    let (http_req, _payload) = req.into_parts();
                                    let res = file
                                        .set_content_type(TEXT_HTML_UTF_8)
                                        .into_response(&http_req);
                                    Ok(ServiceResponse::new(http_req, res))
                                },
                                Err(err) => Err(actix_web::error::ErrorNotFound(err)),
                            }
                        }
                    }),
            );
        }

        app
    });

    // A bound socket is not readiness. All worker applications must be usable
    // before bots, recovery, or heavy first-pass background jobs are released.
    let startup_result = startup_http.ready().await;
    let database_maintenance_runtime = maintenance_service
        .spawn_database_maintenance(magician_config.database_maintenance.clone())?;

    let bot_startup_runtime = Arc::clone(&bot_runtime);
    let bot_startup_task = tokio::spawn(async move {
        if !magician::magician_v2::runtime::startup::wait_for_http().await {
            return;
        }
        if let Err(error) = bot_startup_runtime.start_enabled_existing_scopes().await {
            warn!(error = %error, "Failed to start enabled scoped bots during startup");
        }
    });

    let server_handle = startup_http.handle();
    let shutdown_token = startup_http.shutdown_token();
    let shutdown = async move {
        shutdown_token.cancelled().await;
        info!("🛑 Shutdown signal received, stopping HTTP server");
        server_handle.stop(true).await;
    };

    let server_result: std::io::Result<()> = if let Err(error) = startup_result {
        if startup_http.shutdown_token().is_cancelled() {
            // An operator stop during startup is a normal shutdown, not a
            // failed boot that a service manager should restart as a failure.
            Ok(())
        } else {
            Err(std::io::Error::other(error.to_string()))
        }
    } else {
        tokio::select! {
            res = startup_http.finished() => res,
            _ = shutdown => Ok(()),
        }
    };
    startup_http.stop().await;
    for task in &deferred_startup_tasks {
        task.abort();
    }
    for task in deferred_startup_tasks {
        let _ = task.await;
    }
    bot_startup_task.abort();
    let _ = bot_startup_task.await;

    // Gracefully end live meeting captures BEFORE the runtime dies: each
    // stop/leave runs the session's teardown (tail drain + final summary +
    // memory write + browser hang-up). Without this, a restart mid-meeting
    // silently discards the summary and memory write, and the attendee tile
    // ghosts in the call until Meet's dead-client timeout.
    {
        let passive = magician_media::media_rails::meeting::passive_meeting_manager();
        for row in passive.list().await {
            info!(session_id = %row.session_id, "shutdown: stopping passive meeting listener");
            let _ = passive.stop(&row.session_id).await;
        }
        let attendee = magician_media::media_rails::meeting::meeting_manager();
        for row in attendee.list().await {
            info!(session_id = %row.session_id, "shutdown: leaving attendee meeting session");
            let _ = attendee.leave(&row.session_id).await;
        }
    }

    if let Some(manager) = fluid_audio_manager.as_ref() {
        manager.shutdown().await;
    }

    // Quiesce every background LLM producer before taking the canonical
    // bridge's final receiver snapshot. Cancelling without joining would leave
    // a race where a late response is emitted after the bridge observes an
    // empty queue.
    supervisor_shutdown.cancel();
    for (task_name, handle) in supervisor_tasks.iter_mut() {
        match tokio::time::timeout(AGENT_SUPERVISOR_SHUTDOWN_TIMEOUT, &mut *handle).await {
            Ok(Ok(())) => {},
            Ok(Err(err)) => {
                warn!(
                    task = %task_name,
                    error = %err,
                    "Agent supervisor task exited with join error"
                );
            },
            Err(_) => {
                warn!(
                    task = %task_name,
                    timeout_secs = AGENT_SUPERVISOR_SHUTDOWN_TIMEOUT.as_secs(),
                    "Timed out waiting for agent supervisor task; aborting"
                );
                handle.abort();
            },
        }
    }

    // `spawn_blocking` jobs cannot be aborted after they start. The retained
    // projector handle above may therefore finish cancellation before its
    // synchronous rejoin/HITL-journal work has released broadcaster and sink
    // clones. Close that descendant registry and join it before any canonical
    // receiver or storage owner is torn down.
    let terminal_projection_jobs_observed = terminal_projection_jobs.quiesce().await;
    if terminal_projection_jobs_observed > 0 {
        info!(
            observed_jobs = terminal_projection_jobs_observed,
            "Terminal projection blocking jobs quiesced before sink teardown"
        );
    }

    // Detached agentic jobs run on the dedicated execution runtime rather than
    // inside their startup/scheduler parent tasks. Close that runtime boundary
    // after producers are cancelled and before any event/storage sink is torn
    // down. Aborting these in-memory futures does not terminalize their durable
    // executions; stateless checkpoints remain restart authority.
    let execution_jobs =
        magician::magician_v2::execution::runtime_boundary::quiesce_execution_jobs(
            AGENT_SUPERVISOR_SHUTDOWN_TIMEOUT,
        )
        .await;
    if execution_jobs.timed_out || execution_jobs.remaining_jobs > 0 {
        warn!(
            observed_jobs = execution_jobs.observed_jobs,
            abort_signalled = execution_jobs.abort_signalled,
            remaining_jobs = execution_jobs.remaining_jobs,
            "Execution-job shutdown remained incomplete before sink teardown"
        );
    } else {
        info!(
            observed_jobs = execution_jobs.observed_jobs,
            abort_signalled = execution_jobs.abort_signalled,
            "Execution jobs quiesced before sink teardown"
        );
    }

    // Stop the dedicated agent supervisor runtime without blocking the async
    // shutdown context (dropping a Runtime in async context panics).
    supervisor_runtime.shutdown_background();
    bot_runtime.shutdown_all().await;

    if let Some(queue) = dispatch_queue_for_shutdown.as_ref() {
        let stats = queue.shutdown(Duration::from_secs(10)).await;
        if stats.in_flight_at_shutdown > 0 || stats.pending_at_shutdown > 0 {
            warn!(
                ?stats,
                "LLM dispatch queue retained jobs after bounded shutdown"
            );
        } else {
            info!(?stats, "LLM dispatch queue drained before trace shutdown");
        }
    }

    // Quiesce compaction/retention before the canonical and compatibility
    // pipelines publish their final batches. This preserves those final files
    // as an untouched generation and keeps analytics shutdown single-owned.
    database_maintenance_runtime.shutdown().await;
    storage_maintenance.shutdown().await;

    // Stop accepting raw in-process observations and drain the sanitizer
    // worker before shutting down the journal/materializer it submits to.
    let llm_content_report = llm_content_capture.shutdown();
    if llm_content_report.rejected > 0 || llm_content_report.sanitization_failures > 0 {
        warn!(
            report = ?llm_content_report,
            "Restricted LLM content capture shut down with visible degradation"
        );
    } else {
        info!(
            accepted = llm_content_report.accepted,
            sanitized_records = llm_content_report.sanitized_records,
            "Restricted LLM content capture drained cleanly"
        );
    }
    let restricted_llm_report = restricted_llm_pipeline.shutdown().await;
    if restricted_llm_report.timed_out
        || restricted_llm_report.flush_error.is_some()
        || restricted_llm_report.remaining_buffered_records > 0
    {
        warn!(
            report = ?restricted_llm_report,
            "Restricted LLM content journal shutdown was incomplete"
        );
    }

    // With response producers quiescent, drain the canonical event bridge and
    // durable journal/materializer before cancelling broader analytics.
    let llm_trace_report = llm_trace_activation.shutdown().await;
    if llm_trace_report.durable_pipeline.timed_out
        || llm_trace_report.durable_pipeline.flush_error.is_some()
        || llm_trace_report.durable_pipeline.remaining_buffered_records > 0
        || llm_trace_report.durable_pipeline.remaining_missing_records > 0
        || llm_trace_report.durable_pipeline.rejected_after_shutdown > 0
        || llm_trace_report.activation.records_rejected > 0
    {
        warn!(
            report = ?llm_trace_report,
            "Canonical LLM trace shutdown was incomplete; replay/gap state remains visible"
        );
    } else {
        info!(
            response_events = llm_trace_report.activation.response_events_seen,
            reused_responses_ignored = llm_trace_report.activation.reused_responses_ignored,
            external_aggregate_events_ignored = llm_trace_report
                .activation
                .external_aggregate_events_ignored,
            records_accepted = llm_trace_report.activation.records_accepted,
            gap_records_emitted = llm_trace_report.activation.gap_records_emitted,
            broadcast_events_lost = llm_trace_report.activation.broadcast_events_lost,
            dispatch_terminal_events = llm_trace_report.activation.dispatch_terminal_events_seen,
            dispatch_events_lost = llm_trace_report.activation.dispatch_events_lost,
            "Canonical LLM trace pipeline drained cleanly"
        );
    }

    // The mirror is intentionally non-authoritative, but a clean shutdown
    // should still flush its final compatibility batch so post-restart
    // reconciliation does not report an avoidable tail mismatch.
    _llm_parquet_sink.shutdown().await;
    // Flush the final embedding-batch telemetry before exit (mirrors the
    // llm_calls sink). shutdown() takes &self and is idempotent across clones.
    _llm_embeddings_sink.shutdown().await;

    analytics_shutdown.cancel();

    // `cancel()` returns immediately: it asks the forwarder to stop, it does
    // not observe that it has. Awaiting its handle is what turns "asked" into
    // "stopped", and the sink's contract says to drain only after the producer
    // has stopped — otherwise a close still in flight lands in a queue nobody
    // will read again. Bounded, because a shutdown path must not be able to
    // hang: the forwarder selects on this token, so the timeout is a backstop
    // for a wedged reactor, not an expected outcome.
    //
    // The await also covers the forwarder's own drain: on cancellation it
    // empties its bounded queue onto the bus and into the spine before
    // returning, so the closes already handed to it are not the ones lost.
    match tokio::time::timeout(std::time::Duration::from_secs(5), activity_forwarder).await {
        Ok(Ok(())) => {},
        Ok(Err(error)) => warn!(
            error = %error,
            "Activity forwarder task failed; the spine's final flush may be short"
        ),
        Err(_) => warn!(
            "Activity forwarder did not stop within 5s of cancellation; \
             draining the spine anyway, so its last closes may be lost"
        ),
    }

    // Then the spine, now that the producer is known to have stopped rather
    // than merely asked to. The sink flushes on a sixty-second timer, so
    // skipping this entirely would punch a hole of up to a minute into a store
    // whose whole job is to still be right about last month.
    // `storage_maintenance` was stopped further up, so compaction cannot race
    // this last write.
    activity_rows_sink.shutdown().await;
    // Stop Ollama lifecycle (SIGTERM child if magician spawned it; leave
    // pre-existing user daemons alone). Cancels the background health
    // probe task. Best-effort — never blocks shutdown.
    magician::magician_v2::runtime::ollama_lifecycle::stop().await;

    if let Err(error) = shutdown_secret_store
        .drain_all_captured_tasks_and_flush()
        .await
    {
        warn!(
            error = %error,
            "Failed to drain and flush pending captured session tasks during shutdown"
        );
    }

    server_result?;

    info!("👋 Magician V2 service shutting down");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{
        http::{Method, StatusCode},
        test as actix_test, App,
    };
    use tempfile::tempdir;

    fn base_cli() -> Cli {
        Cli {
            config: PathBuf::from("tool-runtime-config.yaml"),
            log_level: "info".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3002,
            frontend_dir: None,
            frontend_mode: FrontendModeFlag::Auto,
            reindex: false,
            storage_bootstrap: None,
            command: None,
        }
    }

    fn base_config() -> MagicianConfig {
        MagicianConfig::default()
    }

    #[test]
    fn planning_and_runtime_resume_catalogs_remain_periodically_discoverable() {
        let source = include_str!("main.rs");
        let owner = source
            .split("let planning_owner = async move")
            .nth(1)
            .and_then(|tail| tail.split("tokio::join!(").next())
            .expect("planning recovery supervisor");
        assert!(owner.contains("CLEAN_REDISCOVERY_SECS"));
        assert!(owner.contains("loop {"));
        assert!(owner.contains("reconcile_task_planning_at_startup"));
        assert!(owner.contains("error_rediscovery_delay_secs.saturating_mul(2).min(300)"));
        assert!(
            !owner.contains("planning_shutdown.cancelled().await;\n                        return")
        );
    }

    #[test]
    fn stateless_loop_cutover_cli_requires_explicit_scope_deployment_and_drain_ack() {
        let cli = Cli::try_parse_from([
            "magician",
            "seal-stateless-loop-cutover",
            "--principal",
            "anonymous",
            "--workspace",
            "default",
            "--deployment-id",
            "rollout-42",
            "--confirm-legacy-writers-drained",
        ])
        .expect("explicit cutover command parses");
        let Some(CliCommand::SealStatelessLoopCutover(args)) = cli.command else {
            panic!("cutover subcommand was not retained")
        };
        assert_eq!(args.principal, "anonymous");
        assert_eq!(args.workspace, "default");
        assert_eq!(args.deployment_id, "rollout-42");
        assert!(args.confirm_legacy_writers_drained);

        let missing_scope = Cli::try_parse_from([
            "magician",
            "seal-stateless-loop-cutover",
            "--deployment-id",
            "rollout-42",
            "--confirm-legacy-writers-drained",
        ]);
        assert!(missing_scope.is_err());
    }

    #[test]
    fn stateless_loop_cutover_rejects_scope_sanitizer_aliases() {
        for value in [
            "",
            " anonymous",
            "anonymous ",
            ".",
            "..",
            "a..b",
            "a/b",
            "a\\b",
            "a:b",
        ] {
            assert!(
                validate_stateless_loop_cutover_scope_component("principal", value).is_err(),
                "cutover must reject scope value {value:?} when storage would rewrite it"
            );
        }
        assert!(validate_stateless_loop_cutover_scope_component("principal", "anonymous").is_ok());
        assert!(
            validate_stateless_loop_cutover_scope_component("workspace", "team.default").is_ok()
        );
        assert!(validate_stateless_loop_cutover_deployment_id(" rollout-42").is_err());
        assert!(validate_stateless_loop_cutover_deployment_id("rollout-42 ").is_err());
        assert!(validate_stateless_loop_cutover_deployment_id("rollout-42").is_ok());
    }

    #[test]
    fn stateless_lifecycle_first_tick_waits_for_full_execution_composition() {
        let source = include_str!("main.rs")
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production main source");
        let artifact_service_source =
            include_str!("../../magician/src/magician_v2/artifact_v2/service.rs");
        let store_source =
            include_str!("../../magician/src/magician_v2/execution/agentic/run_loop/store/fs.rs");
        let latch = source
            .find("let stateless_loop_lifecycle_ready = CancellationToken::new()")
            .expect("persistent lifecycle readiness latch");
        assert!(
            source[latch..].matches("_ = ready.cancelled()").count() >= 2,
            "terminal projector and reconciler must both await composition"
        );
        let artifact_runtime = source
            .find("orchestrator.set_artifact_v2_service")
            .expect("Artifact execution service binding");
        let runtime_service = source
            .find(".set_artifact_v2_service(Arc::clone(&shared_artifact_v2_service))")
            .expect("runtime Artifact binding");
        let delegation = source
            .find("orchestrator.set_delegation_dispatcher(v3_dispatcher)")
            .expect("delegation dispatcher binding");
        let user_request_service = source
            .find("orchestrator.set_user_request_service")
            .expect("user-request service binding");
        let opened = source
            .find("stateless_loop_lifecycle_ready.cancel();")
            .expect("lifecycle readiness opening");
        let startup_recovery = source
            .find("// Ordered startup recovery for executions")
            .expect("canonical startup recovery");
        assert!(artifact_runtime < opened);
        assert!(runtime_service < opened);
        assert!(delegation < opened);
        assert!(user_request_service < opened);
        assert!(opened < startup_recovery);
        // Startup hydration re-arms scoped autonomous goals, and an already-due
        // goal dispatches immediately. Spawned before the latch it ran against
        // an unwired `artifact_v2_service` and burned one due cycle per harness
        // goal per scope on every boot, so it must stay behind composition.
        let hydration = source
            .find("spawn_agent_services_startup_hydration(shared_agent_api.clone()).await;")
            .expect("agent startup hydration spawn");
        assert!(
            opened < hydration,
            "startup hydration must not re-arm scoped goals before composition is installed"
        );
        let startup_owner = source
            .find("let stateless_startup_recovery_task =")
            .expect("retained startup recovery owner");
        let startup_retention = source[startup_owner..]
            .find("stateless-startup-interrupted-recovery")
            .expect("startup recovery supervisor registration");
        assert!(
            source[startup_owner..startup_owner + startup_retention]
                .matches(".cancelled() => return")
                .count()
                >= 4,
            "startup recovery must be cancellation-aware before its supervisor registration"
        );
        let startup_window = &source[startup_owner..startup_owner + startup_retention];
        assert!(startup_window.contains("delegation_progress_cleanup_owner"));
        assert!(startup_window.contains(".cleanup_stale_delegation_round_progress_at_startup("));
        assert!(startup_window.contains("tokio::join!("));
        let guardrail_owner = source
            .find("let clarification_guardrail_sweep_task =")
            .expect("retained clarification guardrail owner");
        let guardrail_retention = source[guardrail_owner..]
            .find("\"clarification-guardrail-sweep\"")
            .expect("guardrail owner supervisor registration");
        assert!(
            source[guardrail_owner..guardrail_owner + guardrail_retention]
                .contains("supervisor_shutdown.clone()")
        );
        let projector = source
            .find("let terminal_outbox_projector_task =")
            .expect("terminal projector owner");
        let retained_cursor = source[projector..]
            .find("resume = report.resume;")
            .expect("terminal projector retains its bounded scan cursor");
        let catch_up = source[projector..]
            .find("scan_immediately = resume.is_some();")
            .expect("unfinished terminal sweeps immediately schedule their next bounded page");
        let fair_yield = source[projector..]
            .find("tokio::task::yield_now()")
            .expect("terminal catch-up yields between bounded pages");
        let full_sweep_cadence = source[projector..]
            .find("tokio::time::sleep(projector_cadence)")
            .expect("only completed sweeps and failures wait for the steady-state cadence");
        let failure_backoff = source[projector..]
            .find("scan_immediately = false;")
            .expect("terminal scan failures back off without advancing their cursor");
        let artifact_settlement = source[projector..]
            .find("Some(&artifact_service)")
            .expect("terminal projector receives Artifact settlement authority");
        let cancellation = source[projector..]
            .find("&shutdown,")
            .expect("terminal projector receives the shared cancellation token");
        assert!(artifact_settlement < retained_cursor);
        assert!(cancellation < retained_cursor);
        assert!(fair_yield < retained_cursor);
        assert!(retained_cursor < catch_up);
        assert!(full_sweep_cadence < retained_cursor);
        assert!(retained_cursor < failure_backoff);
        assert!(
            artifact_service_source.contains("BaseExecutionRecoveryAuthority::SettlementPending"),
            "generic Artifact recovery must defer terminal settlement debt"
        );
        assert!(
            store_source.contains("return Ok(BaseExecutionRecoveryAuthority::SettlementPending"),
            "the per-base classifier must preserve pending terminal receipts"
        );
        for retained_owner in [
            "stateless-startup-interrupted-recovery",
            "stateless-terminal-outbox-projector",
            "stateless-parked-execution-reconciler",
        ] {
            assert!(
                source.contains(retained_owner),
                "stateless lifecycle owner must be retained for bounded shutdown joining"
            );
        }
        let shutdown = source
            .rfind("supervisor_shutdown.cancel();")
            .expect("shared lifecycle cancellation");
        let joined = source
            .rfind("for (task_name, handle) in supervisor_tasks.iter_mut()")
            .expect("bounded lifecycle join loop");
        assert!(shutdown < joined);
        let execution_jobs_quiesced = source
            .find("runtime_boundary::quiesce_execution_jobs")
            .expect("process-wide execution-job shutdown boundary");
        let projection_jobs_quiesced = source
            .find("terminal_projection_jobs.quiesce().await")
            .expect("unabortable terminal projection work shutdown boundary");
        let storage_teardown = source
            .find("storage_maintenance.shutdown().await")
            .expect("storage maintenance teardown");
        assert!(joined < projection_jobs_quiesced);
        assert!(projection_jobs_quiesced < execution_jobs_quiesced);
        assert!(execution_jobs_quiesced < storage_teardown);
    }

    #[test]
    fn boot_registers_every_chunk_adapter_required_by_the_shipped_config() {
        register_builtin_chunk_adapters_for_boot().expect("register built-in chunk adapters");
        let config: MagicianConfig =
            serde_yaml::from_str(&magician::config::shipped_repo_config_yaml())
                .expect("shipped config parses");
        let router = config.router_config().expect("shipped router config");

        magician::magician_v2::llm_chunking::validate_router_chunking_config(router)
            .expect("every configured adapter is registered before router construction");
    }

    #[test]
    fn app_lifecycle_cli_requires_replay_evidence() {
        let parsed = Cli::try_parse_from([
            "magician",
            "app",
            "--json",
            "disable",
            "install-one",
            "--expected-generation",
            "7",
            "--request-id",
            "cli-request:one",
        ])
        .expect("generation-bound lifecycle command parses");
        let Some(CliCommand::App {
            json: true,
            command: magician::magician_v2::apps::authoring::AppAuthoringCommand::Disable(args),
        }) = parsed.command
        else {
            panic!("expected app disable command");
        };
        assert_eq!(args.installation_id, "install-one");
        assert_eq!(args.expected_generation, 7);
        assert_eq!(args.request_id, "cli-request:one");
        assert_eq!(args.live.api_base, "http://127.0.0.1:3002");
        assert!(
            Cli::try_parse_from(["magician", "app", "--json", "disable", "install-one",]).is_err()
        );
        assert!(Cli::try_parse_from(["magician", "app", "approve", "install-one"]).is_err());
    }

    #[test]
    fn live_app_url_is_loopback_origin_only_and_encodes_path_identities() {
        let live = magician::magician_v2::apps::authoring::AppLiveScopeArgs {
            api_base: "http://127.0.0.1:3002".to_owned(),
        };
        let url = live_app_url(&live, &["purges", "blake3:abc"]).expect("safe URL");
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:3002/api/magician/v2/apps/purges/blake3:abc"
        );
        for api_base in [
            "https://example.test",
            "http://127.0.0.1:3002/prefix",
            "http://user@127.0.0.1:3002",
        ] {
            let mut refused = live.clone();
            refused.api_base = api_base.to_owned();
            assert!(live_app_url(&refused, &["directory"]).is_err());
        }
    }

    #[test]
    fn lifecycle_receipts_reject_generation_status_and_installation_substitution() {
        let valid = serde_json::json!({
            "installation_id": "install-one",
            "generation": 8,
            "status": "disabled",
        });
        validate_lifecycle_receipt(&valid, "install-one", 7, Some("disabled"))
            .expect("correlated receipt");
        for substituted in [
            serde_json::json!({"installation_id":"install-two","generation":8,"status":"disabled"}),
            serde_json::json!({"installation_id":"install-one","generation":9,"status":"disabled"}),
            serde_json::json!({"installation_id":"install-one","generation":8,"status":"enabled"}),
        ] {
            assert!(
                validate_lifecycle_receipt(&substituted, "install-one", 7, Some("disabled"))
                    .is_err()
            );
        }
    }

    #[test]
    fn package_export_is_create_new_and_never_leaves_a_partial_destination() {
        let directory = tempdir().expect("temp dir");
        let destination = directory.path().join("app.zip");
        write_create_new_atomic(&destination, b"first").expect("first create-only publish");
        assert_eq!(fs::read(&destination).expect("published bytes"), b"first");
        assert!(write_create_new_atomic(&destination, b"second").is_err());
        assert_eq!(fs::read(&destination).expect("original bytes"), b"first");
        assert!(fs::read_dir(directory.path())
            .expect("directory rows")
            .all(|entry| !entry
                .expect("directory row")
                .file_name()
                .to_string_lossy()
                .ends_with(".partial")));
    }

    #[test]
    fn bounded_cli_file_reader_rejects_oversize_inputs() {
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("archive.zip");
        fs::write(&path, b"four").expect("fixture");
        assert!(read_bounded_regular_file(&path, 3, "fixture").is_err());
        assert_eq!(
            read_bounded_regular_file(&path, 4, "fixture").expect("bounded read"),
            b"four"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_cli_file_reader_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temp dir");
        let target = directory.path().join("target.zip");
        let link = directory.path().join("link.zip");
        fs::write(&target, b"package").expect("fixture");
        symlink(&target, &link).expect("symlink fixture");
        assert!(read_bounded_regular_file(&link, 32, "fixture").is_err());
    }

    /// **The boot gate, at the call site that owns it.**
    ///
    /// `list_index_rebuild_owed` is the whole of the startup decision, so this
    /// is that decision under the state it used to get wrong: an index left
    /// `in_progress` by a crash, reopened at the current schema version and
    /// therefore REUSED. Gated on `opened().needs_rebuild()` — which this
    /// asserts is false — the rebuild never runs again on any later boot, the
    /// resume path is unreachable in production, and every list serves from
    /// the walk the index exists to remove.
    #[test]
    fn the_boot_gate_rebuilds_an_index_a_crash_left_unfinished() {
        use magician::magician_v2::storage::{ListIndex, ListIndexOpen};

        let dir = tempdir().expect("temp dir");
        let index = ListIndex::open(dir.path()).expect("opening a fresh list index");
        assert!(
            list_index_rebuild_owed(&index),
            "a fresh index holds nothing, so boot must walk the disk"
        );
        drop(index);

        // A first boot that walked the disk and finished. Nothing to do.
        {
            let index = ListIndex::open(dir.path()).expect("reopening the index");
            index
                .rebuild_from_disk(&dir.path().join("scopes"))
                .expect("a rebuild over an empty store");
            assert!(
                !list_index_rebuild_owed(&index),
                "a finished index must not be re-walked on every boot"
            );
        }

        // A boot whose rebuild started and did not finish, produced through the
        // ordinary entry point: `rebuild_from_disk` writes its in-progress
        // marker before it walks anything, and this walk cannot complete
        // because the scopes root is a file rather than a directory. The
        // marker is left on disk exactly as a crash would leave it.
        let unwalkable_root = dir.path().join("scopes-that-are-a-file");
        std::fs::write(&unwalkable_root, b"not a directory").expect("writing an unwalkable root");
        {
            let index = ListIndex::open(dir.path()).expect("reopening the index");
            index
                .rebuild_from_disk(&unwalkable_root)
                .expect_err("walking a file as if it were the scopes root must fail");
            assert!(
                !index.is_ready().expect("readiness"),
                "a rebuild that failed partway must leave the index unfinished"
            );
        }

        let restarted = ListIndex::open(dir.path()).expect("reopening after a crash mid-rebuild");
        assert_eq!(
            restarted.opened(),
            &ListIndexOpen::Reused,
            "the file is at this build's schema version, so the open reuses it"
        );
        assert!(
            !restarted.opened().needs_rebuild(),
            "and the open outcome alone reports no rebuild needed — the bug this gate had"
        );
        assert!(
            list_index_rebuild_owed(&restarted),
            "boot must resume the interrupted rebuild instead of serving a list index \
             that can never become ready"
        );
    }

    /// `--reindex` is a whole run of the binary, not a flag the server also
    /// accepts: it must parse with no subcommand, and it must not be on by
    /// accident for an ordinary start.
    #[test]
    fn the_reindex_flag_parses_as_a_standalone_run() {
        let cli = Cli::try_parse_from(["magician", "--reindex"]).expect("valid --reindex CLI");
        assert!(cli.reindex);
        assert!(
            cli.command.is_none(),
            "`--reindex` takes no subcommand; it IS the command"
        );

        let ordinary = Cli::try_parse_from(["magician"]).expect("valid default CLI");
        assert!(
            !ordinary.reindex,
            "an ordinary start must never discard the index"
        );
    }

    #[test]
    fn contextual_writing_payload_limit_allows_max_screenshot_base64_overhead() {
        let max_contextual_screenshot_bytes: usize = 20 * 1024 * 1024;
        let max_base64_bytes = max_contextual_screenshot_bytes.div_ceil(3) * 4;
        assert!(CONTEXTUAL_WRITING_JSON_PAYLOAD_LIMIT_BYTES > max_base64_bytes);
        assert!(CONTEXTUAL_WRITING_JSON_PAYLOAD_LIMIT_BYTES > API_JSON_PAYLOAD_LIMIT_BYTES);
    }

    #[test]
    fn actionability_snapshot_install_cli_has_no_activation_option() {
        let cli = Cli::try_parse_from([
            "magician",
            "attention-learning",
            "install-actionability-snapshot",
            "--snapshot",
            "/tmp/actionability-snapshot.json",
        ])
        .expect("valid actionability snapshot installation CLI");
        let Some(CliCommand::AttentionLearning { command }) = cli.command else {
            panic!("expected attention-learning command");
        };
        let AttentionLearningCommand::InstallActionabilitySnapshot(args) = command else {
            panic!("expected actionability snapshot installer");
        };
        assert_eq!(
            args.snapshot,
            PathBuf::from("/tmp/actionability-snapshot.json")
        );
        assert!(Cli::try_parse_from([
            "magician",
            "attention-learning",
            "install-actionability-snapshot",
            "--snapshot",
            "/tmp/actionability-snapshot.json",
            "--activate",
        ])
        .is_err());

        let mut config = MagicianConfig::default();
        config.attention_learning.actionability.mode =
            magician::config::AttentionActionabilityMode::Enforced;
        config.attention_learning.actionability.snapshot_id = Some("snapshot-1".to_string());
        assert!(
            ensure_actionability_snapshot_install_is_nonactivating(&config, "snapshot-1").is_err()
        );
        assert!(
            ensure_actionability_snapshot_install_is_nonactivating(&config, "snapshot-2").is_ok()
        );
    }

    #[test]
    fn actionability_train_cli_parses_scope_and_out() {
        let cli = Cli::try_parse_from([
            "magician",
            "attention-learning",
            "train-actionability",
            "--principal",
            "anonymous",
            "--workspace",
            "default",
            "--cutoff",
            "2026-08-01",
            "--split",
            "temporal",
            "--out",
            "/tmp/actionability-snapshot.json",
        ])
        .expect("valid actionability train CLI");
        let Some(CliCommand::AttentionLearning { command }) = cli.command else {
            panic!("expected attention-learning command");
        };
        let AttentionLearningCommand::TrainActionability(args) = command else {
            panic!("expected train-actionability");
        };
        assert_eq!(args.principal, "anonymous");
        assert_eq!(args.out, PathBuf::from("/tmp/actionability-snapshot.json"));
        assert!(!args.install);
        let cutoff = parse_attention_training_cutoff(Some("2026-08-01")).expect("date cutoff");
        let start = chrono::NaiveDate::from_ymd_opt(2026, 8, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let next = chrono::NaiveDate::from_ymd_opt(2026, 8, 2)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        assert!(cutoff >= start && cutoff < next);
    }

    #[test]
    fn reconcile_cli_is_dry_run_by_default_and_scope_comes_from_the_bearer() {
        let cli = Cli::try_parse_from(["magician", "reconcile-orphaned-tasks"])
            .expect("valid reconcile CLI");
        let Some(CliCommand::ReconcileOrphanedTasks(args)) = cli.command else {
            panic!("expected reconcile-orphaned-tasks command");
        };
        assert_eq!(
            args.batch_cap,
            magician::magician_v2::execution::task_reconcile::DEFAULT_RECONCILE_BATCH_CAP
        );
        assert!(!args.apply);

        assert!(Cli::try_parse_from([
            "magician",
            "reconcile-orphaned-tasks",
            "--principal",
            "anonymous"
        ])
        .is_err());
    }

    #[test]
    fn pair_model_snapshot_install_cli_has_no_activation_option() {
        let cli = Cli::try_parse_from([
            "magician",
            "attention-learning",
            "install-pair-model-snapshot",
            "--snapshot",
            "/tmp/pair-model.json",
        ])
        .expect("valid pair-model snapshot installation CLI");
        let Some(CliCommand::AttentionLearning { command }) = cli.command else {
            panic!("expected attention-learning command");
        };
        let AttentionLearningCommand::InstallPairModelSnapshot(args) = command else {
            panic!("expected pair-model installer");
        };
        assert_eq!(args.snapshot, PathBuf::from("/tmp/pair-model.json"));
        assert!(Cli::try_parse_from([
            "magician",
            "attention-learning",
            "install-pair-model-snapshot",
            "--snapshot",
            "/tmp/pair-model.json",
            "--activate",
        ])
        .is_err());
    }

    #[test]
    fn routing_policy_snapshot_install_cli_has_no_activation_option() {
        let cli = Cli::try_parse_from([
            "magician",
            "attention-learning",
            "install-routing-policy-snapshot",
            "--snapshot",
            "/tmp/routing-policy.json",
        ])
        .expect("valid routing-policy snapshot installation CLI");
        let Some(CliCommand::AttentionLearning { command }) = cli.command else {
            panic!("expected attention-learning command");
        };
        let AttentionLearningCommand::InstallRoutingPolicySnapshot(args) = command else {
            panic!("expected routing-policy installer");
        };
        assert_eq!(args.snapshot, PathBuf::from("/tmp/routing-policy.json"));
        assert!(Cli::try_parse_from([
            "magician",
            "attention-learning",
            "install-routing-policy-snapshot",
            "--snapshot",
            "/tmp/routing-policy.json",
            "--activate",
        ])
        .is_err());

        let mut config = MagicianConfig::default();
        config.attention_learning.routing.mode = magician::config::AttentionRoutingMode::Shadow;
        config.attention_learning.routing.snapshot_id = Some("route-v1".to_string());
        assert!(ensure_routing_snapshot_install_is_nonactivating(&config, "route-v1").is_err());
        assert!(ensure_routing_snapshot_install_is_nonactivating(&config, "route-v2").is_ok());
    }

    #[test]
    fn bandit_policy_snapshot_install_cli_has_no_activation_option() {
        let cli = Cli::try_parse_from([
            "magician",
            "attention-learning",
            "install-bandit-policy-snapshot",
            "--snapshot",
            "/tmp/bandit-policy.json",
        ])
        .expect("valid bandit-policy snapshot installation CLI");
        let Some(CliCommand::AttentionLearning { command }) = cli.command else {
            panic!("expected attention-learning command");
        };
        let AttentionLearningCommand::InstallBanditPolicySnapshot(args) = command else {
            panic!("expected bandit-policy installer");
        };
        assert_eq!(args.snapshot, PathBuf::from("/tmp/bandit-policy.json"));
        assert!(Cli::try_parse_from([
            "magician",
            "attention-learning",
            "install-bandit-policy-snapshot",
            "--snapshot",
            "/tmp/bandit-policy.json",
            "--activate",
        ])
        .is_err());
        let mut config = MagicianConfig::default();
        config.attention_learning.bandit.mode = magician::config::AttentionBanditMode::Shadow;
        config.attention_learning.bandit.snapshot_id = Some("bandit-v1".to_string());
        assert!(ensure_bandit_snapshot_install_is_nonactivating(&config, "bandit-v1").is_err());
        assert!(ensure_bandit_snapshot_install_is_nonactivating(&config, "bandit-v2").is_ok());
    }

    #[test]
    fn resolve_frontend_delivery_defaults_to_disabled_when_unconfigured() {
        let cli = base_cli();
        let config = MagicianConfig::default();

        let delivery =
            resolve_frontend_delivery(&cli, &config).expect("default frontend resolution");

        match delivery {
            FrontendDelivery::ApiOnly => {},
            other => panic!("expected disabled frontend delivery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_frontend_delivery_prefers_cli_static_dir() {
        let temp_dir = tempdir().unwrap();
        let mut cli = base_cli();
        cli.frontend_dir = Some(temp_dir.path().to_path_buf());

        let mut config = base_config();
        config.frontend.mode = MagicianFrontendMode::ApiOnly;

        let delivery =
            resolve_frontend_delivery(&cli, &config).expect("static frontend resolution");
        match delivery {
            FrontendDelivery::Static(dir) => {
                let expected = temp_dir.path().canonicalize().unwrap();
                assert_eq!(*dir, expected);
            },
            other => panic!("expected static delivery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_frontend_delivery_uses_config_filesystem() {
        let temp_dir = tempdir().unwrap();
        let cli = base_cli();
        let mut config = base_config();
        config.frontend.mode = MagicianFrontendMode::Filesystem;
        config.frontend.directory = Some(temp_dir.path().to_string_lossy().to_string());

        let delivery =
            resolve_frontend_delivery(&cli, &config).expect("filesystem frontend resolution");
        match delivery {
            FrontendDelivery::Static(dir) => {
                let expected = temp_dir.path().canonicalize().unwrap();
                assert_eq!(*dir, expected);
            },
            other => panic!("expected static delivery, got {:?}", other),
        }
    }

    #[actix_web::test]
    async fn channel_assist_routes_are_registered() {
        let app =
            actix_test::init_service(App::new().configure(configure_channel_assist_routes)).await;

        let routes = [
            (Method::GET, "/channel-assist/sync/status"),
            (Method::POST, "/channel-assist/sync/run"),
            (Method::GET, "/channel-assist/stats"),
            (Method::GET, "/channel-assist/distill/recent"),
            (Method::POST, "/channel-assist/distill/backfill"),
            (Method::GET, "/channel-assist/channels"),
            (Method::PUT, "/channel-assist/channels"),
            (Method::GET, "/channel-assist/annotations"),
            (Method::POST, "/channel-assist/annotations/seed"),
            (Method::POST, "/channel-assist/annotations/test-id/dismiss"),
            (Method::POST, "/channel-assist/annotations/test-id/feedback"),
            (
                Method::GET,
                "/channel-assist/annotations/test-id/writing-preferences",
            ),
            (
                Method::POST,
                "/channel-assist/annotations/test-id/writing-preferences",
            ),
            (
                Method::POST,
                "/channel-assist/writing-preferences/test-id/promote",
            ),
            (
                Method::POST,
                "/channel-assist/writing-preferences/test-id/dismiss",
            ),
            (Method::GET, "/channel-assist/needs-you"),
            (Method::GET, "/channel-assist/follow-ups"),
            (Method::POST, "/channel-assist/annotations/test-id/approve"),
            (Method::POST, "/channel-assist/annotations/test-id/snooze"),
            (
                Method::POST,
                "/channel-assist/annotations/test-id/action/reply/compose",
            ),
            (
                Method::POST,
                "/channel-assist/annotations/test-id/action/reply/commit",
            ),
            (Method::POST, "/channel-assist/annotations/test-id/review"),
            (
                Method::POST,
                "/channel-assist/annotations/test-id/acknowledge",
            ),
            (Method::GET, "/channel-assist/annotations/test-id/message"),
            (Method::GET, "/channel-assist/resurfacing/today"),
            (
                Method::GET,
                "/channel-assist/resurfacing/test-candidate/detail",
            ),
            (
                Method::GET,
                "/channel-assist/resurfacing/test-candidate/original",
            ),
            (
                Method::POST,
                "/channel-assist/resurfacing/test-candidate/action",
            ),
            (Method::GET, "/channel-assist/resurfacing/stats"),
            (Method::GET, "/channel-assist/resurfacing/observability"),
        ];

        for (method, uri) in routes {
            let req = actix_test::TestRequest::default()
                .method(method.clone())
                .uri(uri)
                .to_request();
            let resp = actix_test::call_service(&app, req).await;
            assert_ne!(resp.status(), StatusCode::NOT_FOUND, "{method} {uri}");
        }
    }
}
