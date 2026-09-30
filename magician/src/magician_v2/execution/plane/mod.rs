//! The Magician plane — a governed surface for foreign harnesses and MCP clients.
//!
//! Tasks 2–7 of `docs/plans/2026-08-23-magician-plane-vertical-slice-plan.md`
//! plus the Task 8 turn engine, Task 9 per-action events, and Task 6b
//! delegated runs (`run_ownership`). Direct tools need a live
//! `ActionExecutors` on the grant (Task 11 supplies the runless set).

pub mod catalog;
pub(crate) mod chat_decision_rail;
pub mod chat_turn;
pub mod decision_planner;
pub mod decision_planner_harness;
pub mod dispatch;
pub mod engine;
pub mod engine_pin;
pub mod engines;
pub mod grant;
pub mod input;
pub mod mouth_bridge;
pub mod run_ownership;
pub mod terminal_ledger;
pub mod terminal_session;
pub mod turn_engine;
pub(crate) mod usage;

pub use catalog::{
    advertised_names, plane_runnable_tools_list_configured, plane_tool_search, plane_tools_list,
    plane_tools_list_configured, PLANE_CONTROL_HOT, PLANE_CONTROL_VERBS, PLANE_SPAWNED_BARE_LEAVES,
};
pub use chat_turn::{
    abandon_chat_harness_resume, bridged_deferred_hands, chat_harness_snapshot,
    coerce_chat_mouth_engine, decode_chat_harness_choice, encode_chat_harness_choice,
    forget_chat_harness_conversation, harness_transcript_batches, harness_usage_to_chat_tokens,
    install_chat_harness_snapshot, maybe_harness_chat_turn, subscribe_chat_engine_changes,
    validated_client_chat_harness_choice, ChatHarnessChoice, ChatHarnessSnapshot,
    ChatHarnessTurnOutcome, ChatHarnessTurnRequest, HARNESS_TRANSCRIPT_CHUNK_CALLS,
    PERSONA_CHANGING_TOOLS,
};
pub(crate) use chat_turn::{chat_turn_parent_engine, harness_chat_usage_event};
pub use dispatch::{
    action_result_text, plane_execute_pending_approval, plane_tools_call,
    plane_tools_call_configured,
};
pub use engine::{
    mcp_config_document, revoke_session_grant, HarnessEngine, HarnessError, HarnessSession,
    HarnessSessionRequest, HarnessStopReason, HarnessStreamSink, HarnessTurnInput,
    HarnessTurnSettled, HarnessUsage, NativeToolPosture,
};
pub use engine_pin::{
    current_launching_run_engine_pin, resolve_launch_pin, with_launching_run_engine_pin,
    RunEnginePin,
};
pub use engines::{
    harness_engine_for, roster_with_install_status, AgyEngine, ClaudeCodeEngine, CodexExecEngine,
    GrokEngine,
};
pub use grant::{
    install_plane_runtime_forget, mint_invocation_ref, plane_grant_registry, PlaneCatalogProfile,
    PlaneGrant, PlaneGrantRegistry, PlanePauseDisposition, PlaneRunAuthority, PlaneTurnLedger,
    PlaneTurnStopReason, PlaneTurnToolCall, PLANE_TURN_LEDGER_BUDGET_SPENT,
    PLANE_TURN_LEDGER_MAX_BYTES, PLANE_TURN_RESULT_CUT_MARK, PLANE_TURN_RESULT_MAX_BYTES,
};
pub use mouth_bridge::ChatMouthBridge;
pub use terminal_ledger::{
    entries, ledger_as_mcp_result, record, take, TerminalLedgerEntry, TerminalLedgerOutcome,
};
pub use terminal_session::{
    decorate_runless_refusal, install_runless_executors, runless_executors,
    scoped_runless_executors, take_runless_executors, TerminalSession,
};
pub use turn_engine::{
    harness_engine_snapshot, install_harness_engine_snapshot, resolve_turn_engine, run_engine_for,
    run_engine_for_pin, run_parent_engine, run_parent_engine_for_pin, settings_pin,
    HarnessEngineSnapshot, TurnEngine,
};
