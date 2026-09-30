//! Phase 0.8c — compiled-tool handler implementations.
//!
//! Each submodule here exposes a single `pub async fn handle(...)`
//! that takes `(Arc<AgentResources>, Value) -> Result<Value, ExecutionError>`
//! and contains the tool's actual logic — no struct, no trait impl,
//! no constructor boilerplate. Adding a new tool:
//!
//! 1. Drop a YAML in `embedded_pack_defs/<name>.yaml`
//! 2. Create a new submodule here with `pub async fn handle(...)`
//! 3. Add a `registry.register("<name>", compiled_handler!(<name>::handle))`
//!    line to `default_compiled_handler_registry()` in
//!    `compiled_providers.rs`.
//! 4. Delete the old struct + impl from `compiled_providers.rs`.
//!
//! The shared `GenericCompiledProvider` handles all the
//! `CapabilityProvider` trait boilerplate uniformly.

pub mod activate_skill;
pub mod android_device;
pub mod app_action_compose;
pub mod app_action_invoke;
pub mod app_data;
pub mod app_data_compose;
pub mod app_data_query;
pub mod app_data_search;
pub mod app_discover;
pub mod app_memory_propose;
pub mod append_note;
pub mod apply_code_proposal;
pub mod apply_patch;
pub mod ask_owner;
pub mod authorize_content_read;
pub mod capture_reference;
pub mod content_batch;
pub mod content_read;
pub mod content_search;
pub mod contribute_to_project;
pub mod create_dashboard;
pub mod create_monitor;
pub mod create_note;
pub mod create_task;
pub mod deactivate_skill;
pub mod delete_task;
pub mod deploy_app;
pub mod distill_evidence;
pub mod edit_file;
pub mod find_agents_for_capability;
pub mod forget_memory;
pub mod get_active_executions;
pub mod get_agent_details;
pub mod get_execution_history;
pub mod get_task_details;
pub mod glob;
pub mod grep;
pub mod imessage_read;
pub mod imessage_send;
pub mod list_agents;
pub mod list_artifacts;
pub mod list_memory_tiers;
pub mod list_scheduled_tasks;
pub mod list_tasks;
pub mod macos_automation;
pub mod monitor_tools;
pub mod notes_shared;
pub mod notify_owner;
pub mod open_note;
pub mod open_pr;
pub mod owner_relay;
pub mod preview_monitor;
pub mod propose_meeting;
pub mod propose_program_missions;
pub mod publish_task_to_note;
pub mod read_file;
pub mod refine_task;
pub mod replay_recipe;
pub mod request_owner_action;
pub mod review_program_missions;
pub mod run_coding_task;
pub mod run_project_checks;
pub mod run_task;
pub mod save_preference;
pub mod save_selection_to_note;
pub mod screenshot_preview;
pub mod search_memory;
pub mod search_notes;
pub mod shared;
pub mod staged_file_edit;
pub mod stop_task;
pub mod switch_personality;
pub mod tool_search;
pub mod unpublish_dashboard;
pub mod update_memory_tier;
pub mod update_monitor;
pub mod update_task;
pub mod web_answer;
pub mod web_fetch;
pub mod web_search;
pub mod working_sets;
pub mod write_file;
