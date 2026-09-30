//! The Magician HTTP/WS surface, extracted from the `magician` monolith as a
//! satellite crate depending on the `magician` lib. Lib-side consumers import
//! their shared substrate from `magician_v2` (vibedev::projects, task_lanes,
//! hitl_deprecation_metrics, monitor_support, observe_connectors,
//! task_run_factory, screen_capture, realtime_events visibility) — this crate
//! re-exports those where handlers need them.

// MagicianV2 API Module
// Enhanced API endpoints with query analysis integration

pub mod agent_updates_api;
// scope + today_projection_cache moved lib-side; re-exported for handlers.
pub use magician::magician_v2::api_scope as scope;
pub use magician::magician_v2::mcp_oauth as mcp_oauth_api;
pub use magician::magician_v2::today_projection_cache;
pub use magician_comms::channel_assist::canonical_attention as canonical_attention_api;
pub use magician_comms::channel_assist::memory_api;
pub mod ambient_api;
pub mod analytics_api;
mod android_apps_attestation;
pub mod android_apps_owner_api;
mod android_automation_trust;
mod android_play_integrity;
pub mod api_mining_api;
pub mod approval_envelopes_api;
pub mod apps_api;
pub mod artifact_api;
pub mod attention_funnel_api;
pub mod attention_learning_api;
pub mod auth_api;
pub mod bot_api;
pub mod browser_engine_analytics_api;
pub mod channel_assist_api;
pub mod chat_api;
pub mod components_api;
pub mod contextual_writing_api;
pub mod corrections_api;
pub mod counterparties_api;
pub mod crew_health_api;
pub mod crew_surface;
pub mod dashboard_themes_api;
pub mod data_room_api;
pub mod data_room_reader_api;
pub mod delivery_receipts_api;
pub mod device_bridge_handler;
pub mod device_pairing_api;
pub mod edge_bridge_handler;
pub mod engagements_api;
pub mod enrollment_api;
pub mod evals_api;
pub mod events_api;
pub mod evidence_api;
pub mod execution_panel_api;
pub mod feed_api;
pub mod fleet_state_api;
pub mod gaui_api;
pub mod llm_routing_api;
pub mod local_generation_api;
pub use magician::magician_v2::hitl_deprecation_metrics;
pub mod hitl_delivery_api;
pub mod interactive_process_api;
/// Server-published lane invoke-grammar catalog (plan 1.2): read-only,
/// stateless, generated from the parser constants.
pub mod invoke_grammar_api;
pub mod learning_api;
pub mod llm_chunking_api;
pub mod llm_queue_api;
pub mod local_resource_governor_api;
pub mod media_api;
pub mod media_ux;
pub mod meetings_api;
pub mod mobile_push_api;
pub mod monitors_api;
pub mod notes_api;
pub mod observable_sources_api;
pub mod observe_catchup_api;
pub mod observe_connectors_api;
pub mod outcome_learning_api;
pub mod plane_api;
pub mod privacy_api;
pub mod programs_api;
pub mod progress_channel_api;
pub mod resource_authority_api;
pub mod resurfacing_api;
pub mod runtime_env_api;
pub mod screen_api;
pub mod secret_vault_api;
pub mod service_health_api;
pub mod skills_api;
pub mod social_api;
pub mod storage_activation_api;
pub mod storage_governance_api;
pub mod suppression_api;
pub mod task_api_v3;
pub mod verification_codes_api;
pub use magician::magician_v2::task_lanes;
pub mod recipient_compliance_api;
/// Review surface for taste proposals: list pending, approve, reject.
pub mod taste_capture_api;
/// Read-only mirror of the owner's taste profile (the note stays the one
/// writable copy).
pub mod taste_profile_api;
pub mod thinking_maps_api;
pub mod transcript_claims_api;
pub mod tray_bridge_handler;
pub mod tutor_api;
pub mod ui_preferences_api;
pub mod ui_thread_api;
pub mod user_request_api;
pub mod vibedev_api;
pub mod vibedev_preview_proxy;
pub mod voice_control_handler;
pub mod web_api;
pub mod websocket_handler;
pub mod work_modules_api;
pub mod workspace_storage_api;

pub(crate) fn is_expected_websocket_disconnect_message(message: &str) -> bool {
    message.contains("payload reached EOF before completing")
}

pub use enrollment_api::{
    approve_handler, enroll_handler, enrollment_status_handler, EnrollmentApi,
};
pub use monitors_api::{
    create_monitor_v3_handler, delete_monitor_v3_handler, get_monitor_v3_handler,
    list_monitors_v3_handler, pause_monitor_v3_handler, resume_monitor_v3_handler,
    run_monitor_v3_handler, update_monitor_v3_handler,
};
pub use task_api_v3::{
    approve_task_plan_v3_handler, approve_task_v3_handler, cancel_execution_v3_handler,
    create_task_v3_handler, delete_task_plan_version_v3_handler, delete_task_v3_handler,
    download_task_output_v3_handler, execute_task_v3_handler, export_task_outputs_v3_handler,
    get_execution_delegations_v3_handler, get_execution_outputs_v3_handler,
    get_execution_refs_v3_handler, get_execution_schedule_v3_handler,
    get_execution_tree_v3_handler, get_execution_v3_handler, get_task_outputs_v3_handler,
    get_task_plan_analysis_v3_handler, get_task_plan_attempts_v3_handler,
    get_task_plan_clarifications_v3_handler, get_task_plan_pending_questions_v3_handler,
    get_task_plan_slots_v3_handler, get_task_plan_v3_handler, get_task_plan_version_v3_handler,
    get_task_progress_v3_handler, get_task_refs_v3_handler, get_task_v3_handler,
    list_executions_v3_handler, list_task_plan_versions_v3_handler, list_tasks_v3_handler,
    open_task_output_file_v3_handler, open_task_output_folder_v3_handler,
    reject_task_plan_v3_handler, replan_task_v3_handler, restore_task_plan_version_v3_handler,
    start_task_planning_v3_handler, submit_task_plan_clarification_v3_handler,
    update_task_plan_v3_handler, update_task_status_v3_handler, update_task_v3_handler,
    CreateTaskV3Request, ExecutionListResponseV3, ScopeQuery, TaskApiV3, TaskListResponseV3,
    UpdateTaskStatusV3Request, UpdateTaskV3Request,
};
pub use ui_preferences_api::{
    get_ui_preferences_handler, put_ui_preferences_handler, UiPreferencesApi,
};
pub use vibedev_api::{
    activate_vibedev_project_handler, check_vibedev_deploy_settings_handler,
    control_vibedev_run_handler, create_vibedev_project_handler, delete_vibedev_project_handler,
    deploy_vibedev_project_handler, get_vibedev_deploy_settings_handler,
    get_vibedev_preview_handler, get_vibedev_project_info_handler,
    get_vibedev_run_coding_events_handler, get_vibedev_run_logs_handler,
    get_vibedev_run_proposals_handler, list_vibedev_projects_handler, open_vibedev_repo_handler,
    put_vibedev_deploy_settings_handler, run_vibedev_check_handler, start_vibedev_preview_handler,
    stop_vibedev_preview_handler, update_vibedev_project_handler, VibeDevApi,
};
pub use vibedev_preview_proxy::vibedev_preview_proxy_handler;
pub use web_api::{
    create_agent_definition_handler, create_execution_v2_handler, create_slot_handler,
    delete_agent_definition_handler, delete_execution_v2_handler, get_agent_definition_handler,
    get_agent_effective_tools_handler, get_agent_harness_overview_handler,
    get_agent_health_handler, get_agent_runtime_context_cache_handler, get_crew_health_handler,
    get_execution_control_state_handler, get_execution_responsibility_handler,
    get_execution_status_handler, get_execution_summary_handler, get_execution_v2_handler,
    get_observation_json_handler, get_observation_screenshot_handler, get_pause_state_handler,
    get_pending_slots_handler, get_slot_handler, get_slots_handler, get_storage_stats_handler,
    list_agent_definitions_handler, list_executions_v2_handler, list_observations_handler,
    list_turns_v2_handler, manual_trigger_agent_handler, pause_agent_handler,
    post_message_v2_handler, refresh_agent_runtime_context_cache_handler, resume_agent_handler,
    set_primary_agent_handler, start_execution_handler, update_agent_definition_handler,
    update_execution_status_handler, update_slot_answer_handler, update_slot_status_handler,
    AgentDefinitionListResponse, AgentDefinitionRecordResponse, AgentPauseResumeResponse,
    CreateExecutionRequest, CreateSlotRequest, ExecutionControlStateResponse,
    ExecutionResponsibilityResponse, ExecutionSummaryResponse, ListAgentDefinitionsQuery,
    ListExecutionsQuery, ListTurnsQuery, MagicianV2Api, MagicianV2PostMessageRequest,
    ManualAgentTriggerRequest, ManualAgentTriggerResponse, StartExecutionRequest,
    UpdateExecutionStatusRequest, UpdateSlotAnswerRequest, UpdateSlotStatusRequest,
};
pub use websocket_handler::websocket_handler;
pub use workspace_storage_api::{
    configure_workspace_storage_routes, get_workspace_storage_settings_handler,
    put_workspace_storage_settings_handler, WorkspaceStorageApi,
};

#[cfg(test)]
mod disconnect_tests {
    use crate::is_expected_websocket_disconnect_message;

    #[test]
    fn classifies_incomplete_payload_eof_as_expected_disconnect() {
        assert!(is_expected_websocket_disconnect_message(
            "payload reached EOF before completing: stream closed"
        ));
        assert!(!is_expected_websocket_disconnect_message(
            "invalid websocket continuation frame"
        ));
    }
}

// GAUI Layout API exports
pub use gaui_api::{get_layout_handler, put_layout_handler, GauiApi};

// API Mining exports
pub use api_mining_api::{
    get_auth_status, get_capability, get_openapi, get_origin_auth_refresh_status, get_registry,
    refresh_origin_auth, replay_capability, ApiMiningApi, AuthRefreshPhase, AuthRefreshStatus,
    ReplayRequestBody, ReplayResponse,
};
pub use approval_envelopes_api::{configure_approval_envelope_routes, ApprovalEnvelopeApi};
pub use bot_api::{
    delete_bot_config_handler, get_bot_auth_handler, get_bot_auth_state_handler,
    get_bot_config_handler, get_bot_env_handler, get_bot_logs_handler, get_bot_qr_handler,
    list_bots_auth_handler, list_bots_handler, put_bot_config_handler, put_bot_env_handler,
    restart_bot_handler, start_bot_auth_handler, start_bot_handler, stop_bot_handler,
    submit_bot_auth_input_handler, BotApi,
};
pub use counterparties_api::configure_counterparty_routes;
pub use delivery_receipts_api::configure_delivery_mail_routes;
pub use device_bridge_handler::device_bridge_ws_handler;
pub use device_pairing_api::{
    begin_device_enrollment_handler, cancel_device_enrollment_handler,
    exchange_device_enrollment_handler, get_mobile_device_handler, list_devices_handler,
    pair_device_handler, put_device_automation_review_handler,
    revoke_device_automation_review_handler, unpair_device_handler,
};
pub use edge_bridge_handler::{edge_bridge_ws_handler, edge_devices_handler, edge_invoke_handler};
pub use engagements_api::configure_engagement_routes;
pub use execution_panel_api::{
    get_execution_panel_handler, get_task_execution_panel_handler, ExecutionPanelApi,
};
pub use feed_api::{
    feed_attention_dismiss_handler, feed_attention_handler, feed_attention_item_handler,
    feed_attention_undismiss_handler, feed_counts_handler, feed_purge_orphans_handler,
    list_feed_handler, today_handler, today_item_action_handler, today_visibility_handler,
    today_visibility_list_handler, FeedApi,
};
pub use interactive_process_api::{
    close_session_handler as close_interactive_session_handler,
    list_cli_runtimes_handler as list_interactive_cli_runtimes_handler,
    list_directories_handler as list_interactive_directories_handler,
    list_sessions_handler as list_interactive_sessions_handler,
    session_buffer_handler as interactive_session_buffer_handler,
    session_diff_handler as interactive_session_diff_handler,
    start_session_handler as start_interactive_session_handler,
    write_stdin_handler as write_interactive_stdin_handler,
};
pub use invoke_grammar_api::{configure_invoke_grammar_routes, get_invoke_grammar_handler};
pub use learning_api::{
    apply_learning_capability_evolution_implementation_handler, create_learning_candidate_handler,
    create_learning_event_handler, create_learning_procedure_handler,
    decide_learning_capability_evolution_proposal_handler,
    decide_learning_capability_evolution_rollback_recommendation_handler,
    draft_learning_capability_evolution_implementation_handler,
    draft_learning_capability_evolution_proposals_handler, get_learning_audit_handler,
    get_learning_candidate_handler, get_learning_capability_evolution_application_handler,
    get_learning_capability_evolution_handler,
    get_learning_capability_evolution_implementation_handler,
    get_learning_capability_evolution_post_promotion_monitor_handler,
    get_learning_capability_evolution_promotion_handler,
    get_learning_capability_evolution_proposal_handler,
    get_learning_capability_evolution_rollback_recommendation_handler,
    get_learning_capability_evolution_steward_run_handler,
    get_learning_capability_evolution_validation_handler, get_learning_evaluation_handler,
    get_learning_evaluation_run_handler, get_learning_growth_evaluation_run_handler,
    get_learning_procedure_handler, list_learning_candidates_handler,
    list_learning_capability_evolution_applications_handler,
    list_learning_capability_evolution_handler,
    list_learning_capability_evolution_implementations_handler,
    list_learning_capability_evolution_post_promotion_monitors_handler,
    list_learning_capability_evolution_promotions_handler,
    list_learning_capability_evolution_proposals_handler,
    list_learning_capability_evolution_rollback_recommendations_handler,
    list_learning_capability_evolution_steward_runs_handler,
    list_learning_capability_evolution_validations_handler, list_learning_evaluation_runs_handler,
    list_learning_evaluations_handler, list_learning_events_handler,
    list_learning_growth_evaluation_runs_handler, list_learning_procedures_handler,
    promote_learning_procedure_to_skill_handler,
    record_learning_capability_evolution_implementation_handler,
    record_learning_capability_evolution_promotion_handler,
    record_learning_capability_evolution_validation_handler,
    record_learning_teaching_feedback_handler,
    run_learning_capability_evolution_post_promotion_monitor_handler,
    run_learning_capability_evolution_steward_handler,
    run_learning_capability_evolution_validation_handler, run_learning_evaluation_handler,
    run_learning_growth_evaluation_handler, transition_learning_candidate_handler,
    transition_learning_procedure_handler, upsert_learning_capability_evolution_proposal_handler,
    LearningApi,
};
pub use media_api::{
    clear_tts_cache_handler, delete_media_session_handler, get_audio_settings_handler,
    get_media_preferences_handler, get_media_session_handler, get_resolved_audio_surface_handler,
    heartbeat_media_session_handler, list_media_providers_handler, list_media_sessions_handler,
    patch_media_session_handler, post_media_event_handler, post_voice_note_event_handler,
    put_audio_settings_handler, put_media_preferences_handler, register_media_session_handler,
    submit_voice_note_handler, synthesize_message_handler, synthesize_tts_handler,
    transcribe_stt_handler, transcribe_stt_stream_handler, tts_cache_stats_handler, MediaApi,
};
pub use memory_api::{
    confirm_user_memory_entry_handler, get_memory_effect_review_handler,
    keep_user_memory_entry_conflict_handler, list_user_memory_entries_handler,
    memory_save_preference_handler, memory_search_handler, patch_user_memory_entry_scope_handler,
    post_memory_effect_review_handler, MemoryApi,
};
pub use notes_api::{
    append_note_handler, backfill_task_notes_handler, configure_notes_routes,
    create_audio_note_handler, create_note_handler, delete_audio_note_handler,
    get_audio_note_handler, get_audio_note_recording_handler, get_notes_settings_handler,
    get_task_note_handler, list_audio_notes_handler, list_task_notes_handler,
    notes_provider_status_handler, open_notes_provider_root_handler,
    promote_task_note_to_memory_handler, publish_task_note_handler, put_notes_settings_handler,
    NotesApi,
};
pub use plane_api::{plane_mcp_delete_handler, plane_mcp_handler, plane_mcp_sse_handler};
pub use progress_channel_api::{
    create_webhook_subscription_handler, delete_progress_subscription_handler,
    list_webhook_subscriptions_handler, ProgressChannelApi,
};
pub use resource_authority_api::{configure_resource_authority_routes, ResourceAuthorityApi};
pub use secret_vault_api::{configure_secret_vault_routes, SecretVaultApi};
pub use suppression_api::{configure_suppression_routes, SuppressionApi};
pub use tray_bridge_handler::{tray_bridge_ws_handler, TrayDownstreamFrame};
pub use tutor_api::{configure_tutor_routes, get_tutor_primitives_handler, TutorApi};
pub use ui_thread_api::{
    create_ui_thread_handler, delete_ui_thread_handler, get_ui_thread_handler,
    list_ui_threads_handler, reorder_ui_threads_handler, update_ui_thread_handler, UiThreadApi,
};
pub use work_modules_api::{configure_work_module_routes, WorkModulesApi};
