//! Chat-runtime tools injected onto the chat tool
//! surface (and only there). They manipulate chat-local surface state
//! (sessions, threads, personality, active playbooks) or explicit
//! chat-origin learning signals, so they're not part of `native_catalog` and
//! not opted-in via any agent YAML.
//!
//! Trait-driven so dispatch is a name lookup against the slice; each
//! implementation is small (parse args → call service → return JSON).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use magicllm::types::LLMToolSpec;
use serde_json::{json, Value};

use crate::magician_v2::agents::definition_store::AgentDefinitionStore;
use crate::magician_v2::agents::{AgentMemoryResolver, FeatureMode};
use crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord;
use crate::magician_v2::artifact_v2::service::ArtifactV2Service;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::learning::{
    record_teaching_feedback, CreateLearningTeachingFeedbackRequest, LearningScope, LearningStore,
    LearningTeachingAction, LearningTeachingTarget,
};
use crate::magician_v2::realtime_events::{RuntimeAgentEventType, RuntimeTransportBroadcaster};
use crate::magician_v2::tool_result_materialization::{
    CanonicalRawResultStore, CanonicalResultError, RawResultOwner, RawResultReadContext,
    RawResultReadRequest, ScopedResultReadAuthority, ScopedResultRef, DEFAULT_RESULT_PAGE_BYTES,
};
use crate::magician_v2::tutor::{
    classify_tutor_or_app_copilot_canvas_mode_for_lane,
    classify_tutor_or_app_copilot_turn_mode_for_lane, parse_tutor_canvas_mode, tutor_run_store,
    validate_visual_entity_map, TutorCanvasMode, TutorCreatedObject, TutorRun, TutorRunMode,
    TutorRunScope, TutorRunStatus, TutorSafetyLevel, TutorStep, TutorStepKind, TutorStepStatus,
    TutorVisualEntityMap,
};
use crate::magician_v2::ui_threads::UiThreadService;

use super::models::ChatSessionStatus;
use super::storage::ChatStore;

/// Per-turn context passed to every chat-runtime tool dispatch.
pub struct ChatRuntimeToolContext {
    pub session_id: String,
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub chat_store: Arc<dyn ChatStore>,
    pub ui_thread_service: Arc<UiThreadService>,
    pub memory_resolver: AgentMemoryResolver,
    pub agent_definition_store: Arc<AgentDefinitionStore>,
    pub workspace_layout: ArtifactV2Workspace,
    pub event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    pub chat_turn_id: Option<String>,
    pub current_user_text: Option<String>,
    /// Typed, server-authenticated feature lane for this decision boundary.
    /// Runtime tools must not infer authorization or lane semantics from text.
    pub feature_mode: FeatureMode,
    /// AgentSkills v1 per-scope skills root. Personality-mode skills
    /// installed here win over `paths`-declared extras on collision.
    pub workspace_skills_dir: PathBuf,
    /// V3 artifact / task service. Optional because tests can build a
    /// context without a configured service; tools that need it should
    /// return a structured error when it is absent.
    pub artifact_v2_service: Option<Arc<ArtifactV2Service>>,
    /// Current, freshly revalidated surface authority for canonical result
    /// continuation. The companion allowlist is the exact dispatch ceiling
    /// advertised for this turn; neither value originates with the model.
    pub result_authority_revision: String,
    pub authorized_tool_names: BTreeSet<String>,
    pub result_policy_guard:
        Option<Arc<dyn crate::magician_v2::tool_result_materialization::ResultReadPolicyGuard>>,
}

/// What a chat-runtime tool is on the plane when the chat mouth is an
/// external harness. Declared on the tool itself so the compiler refuses a
/// new runtime tool that has not decided; a posture living in a comment or a
/// side table is what let the harness allowlist drift.
///
/// Three postures: a `Counterpart` is a plane-executed twin (the plane runs
/// its own compiled implementation under other names); a `Bridged` tool is
/// the native implementation itself, reached through the chat service's
/// dispatcher over the mouth bridge (`execution::plane::ChatMouthBridge`)
/// with the turn's exact context; `MouthOnly` is withheld from a swapped
/// mouth altogether.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanePosture {
    /// Its own implementation runs on the plane under these plane names.
    /// Every name must be a compiled tool the plane can lower (enforced by
    /// the exhaustiveness test in `plane_posture_tests`).
    Counterpart(&'static [&'static str]),
    /// The mouth's own implementation, reached from a swapped mouth through
    /// the chat service's dispatcher with the turn's exact context.
    Bridged,
    /// Never offered to a swapped mouth.
    MouthOnly,
}

#[async_trait]
pub trait ChatRuntimeTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;

    /// See [`PlanePosture`]. Required on purpose: no default, so every
    /// runtime tool states what a swapped chat mouth gets for it.
    fn plane_posture(&self) -> PlanePosture;

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value>;

    fn to_tool_spec(&self) -> LLMToolSpec {
        LLMToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}

/// Build the canonical set of chat-runtime tools. Order is irrelevant; the
/// chat path looks up by name.
///
/// **Intentionally NOT registered here — these are agent operations,
/// not chat-surface ones, and reach the LLM via the universal compiled
/// pack rail (`UNIVERSAL_BACKEND_PACKS` in `native_integration.rs`):**
/// - `switch_personality` — compiled pack (`switch_personality.yaml`).
/// - `activate_skill` / `deactivate_skill` — compiled packs writing to
///   the agent-scope `active_procedure_skill` memory tier
///   (`activate_skill.yaml` / `deactivate_skill.yaml`). Phase 0.8c-7
///   unified chat + autonomous reads from that tier.
///
/// What stays here: tools that genuinely manipulate the chat surface
/// itself (sessions, threads, teaching feedback).
pub fn build_chat_runtime_tools() -> Vec<Arc<dyn ChatRuntimeTool>> {
    vec![
        Arc::new(ArchiveChatSessionTool),
        Arc::new(DeleteChatSessionTool),
        Arc::new(UnarchiveChatSessionTool),
        Arc::new(ListChatSessionsTool),
        Arc::new(SwitchChatSessionTool),
        Arc::new(ArchiveChatThreadTool),
        Arc::new(CreateChatThreadTool),
        Arc::new(DeleteChatThreadTool),
        Arc::new(ListChatThreadsTool),
        Arc::new(SwitchChatThreadTool),
        Arc::new(GetCurrentChatContextTool),
        Arc::new(RecordChatTeachingFeedbackTool),
        Arc::new(GetTaskDetailsForChatTool),
        Arc::new(SubscribeToTaskForChatTool),
        Arc::new(DescribeAgentsForChatTool),
        Arc::new(ListToolsForChatTool),
        Arc::new(ReadResultTool),
        Arc::new(StartTutorRunTool),
        Arc::new(GetTutorRunTool),
        Arc::new(CheckForCopilotUserActionTool),
        Arc::new(WaitForCopilotUserActionTool),
        Arc::new(ProposeTutorStepTool),
        Arc::new(RecordTutorStepFailureTool),
        Arc::new(RecordTutorCreatedObjectTool),
        Arc::new(CompleteTutorRunTool),
    ]
}

/// Name → posture for every built runtime tool. The harness grant builder in
/// `execution::plane::chat_turn` consumes this so it never has to know the
/// trait objects: a `Counterpart` maps onto the plane allowlist under its
/// plane names, a `Bridged` tool is advertised under its own name and
/// dispatched back through the mouth bridge, and `MouthOnly` is withheld.
pub fn plane_posture_index(tools: &[Arc<dyn ChatRuntimeTool>]) -> HashMap<String, PlanePosture> {
    tools
        .iter()
        .map(|tool| (tool.name().to_string(), tool.plane_posture()))
        .collect()
}

// ---------------------------------------------------------------------------
// Canonical tool-result continuation
// ---------------------------------------------------------------------------

struct ReadResultTool;

#[async_trait]
impl ChatRuntimeTool for ReadResultTool {
    fn name(&self) -> &str {
        "read_result"
    }

    fn description(&self) -> &str {
        "Read the next authorized page or selected fields from a complete tool result when a prior bounded result includes a full_result_ref. The reference is not a credential: current scope, agent, original tool authorization, retention, and authority revision are checked on every call."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "result_ref": {
                    "type": "string",
                    "description": "Opaque full_result_ref returned by a previous tool result."
                },
                "cursor": {
                    "type": "string",
                    "description": "Optional next_cursor from the previous read_result page."
                },
                "field_paths": {
                    "type": "array",
                    "description": "Optional RFC 6901 JSON pointers selecting fields or record collections.",
                    "items": {"type": "string"},
                    "maxItems": 32
                },
                "max_records": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 1000,
                    "description": "Maximum complete records in this bounded page. Defaults to 20."
                }
            },
            "required": ["result_ref"],
            "additionalProperties": false
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(result_ref) = text_arg(args, "result_ref") else {
            return Ok(json!({
                "status": "error",
                "error_code": "invalid_result_request",
                "reason": "read_result requires `result_ref`"
            }));
        };
        let content_ref = match ScopedResultRef::parse(result_ref) {
            Ok(reference) => reference,
            Err(error) => return Ok(result_read_error(error)),
        };
        let field_paths = match args.get("field_paths") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(values)) => {
                let mut paths = Vec::with_capacity(values.len());
                for value in values {
                    let Some(path) = value.as_str() else {
                        return Ok(json!({
                            "status": "error",
                            "error_code": "invalid_result_request",
                            "reason": "field_paths must contain only JSON-pointer strings"
                        }));
                    };
                    paths.push(path.to_string());
                }
                paths
            },
            Some(_) => {
                return Ok(json!({
                    "status": "error",
                    "error_code": "invalid_result_request",
                    "reason": "field_paths must be an array of JSON-pointer strings"
                }));
            },
        };
        let owner = RawResultOwner::Chat {
            session_id: ctx.session_id.clone(),
        };
        let scope = crate::magician_v2::artifact_v2::ScopeRef::system_internal_unauthenticated(
            &ctx.principal.clone(),
            &ctx.workspace.clone(),
        );
        let authority = match ScopedResultReadAuthority::for_current_policy(
            scope.clone(),
            owner.clone(),
            ctx.agent_id.clone(),
            ctx.authorized_tool_names.iter().cloned(),
            ctx.result_authority_revision.clone(),
        ) {
            Ok(authority) => match ctx.result_policy_guard.as_ref() {
                Some(guard) => authority.with_policy_guard(Arc::clone(guard)),
                None => authority,
            },
            Err(error) => return Ok(result_read_error(error)),
        };
        let request = RawResultReadRequest {
            content_ref,
            cursor: args
                .get("cursor")
                .and_then(Value::as_str)
                .map(str::to_string),
            field_paths,
            max_records: args
                .get("max_records")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(20),
            max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
        };
        let store = CanonicalRawResultStore::new(ctx.workspace_layout.clone(), Arc::new(authority));
        match store
            .read(
                &RawResultReadContext {
                    scope,
                    owner,
                    agent_id: ctx.agent_id.clone(),
                },
                &request,
            )
            .await
        {
            Ok(page) => Ok(
                crate::magician_v2::tool_result_materialization::model_lossless_read_success_payload(
                    page,
                ),
            ),
            Err(error) => Ok(result_read_error(error)),
        }
    }
}

fn result_read_error(error: CanonicalResultError) -> Value {
    let error_code = match error {
        CanonicalResultError::NotFound => "result_not_found",
        CanonicalResultError::Revoked => "result_revoked",
        CanonicalResultError::Expired => "result_expired",
        CanonicalResultError::CursorExpired => "result_cursor_expired",
        CanonicalResultError::Corrupt => "result_corrupt",
        CanonicalResultError::InvalidCursor => "result_cursor_invalid",
        CanonicalResultError::RecordTooLarge => "result_record_too_large",
        CanonicalResultError::AuthorityUnavailable => "result_authority_unavailable",
        CanonicalResultError::StorageUnavailable => "result_storage_unavailable",
        CanonicalResultError::IdentityConflict | CanonicalResultError::InvalidRequest { .. } => {
            "invalid_result_request"
        },
    };
    json!({
        "status": "error",
        "error_code": error_code,
        "reason": error.to_string()
    })
}

// ---------------------------------------------------------------------------
// Personal Tutor run tools
// ---------------------------------------------------------------------------

struct StartTutorRunTool;

#[async_trait]
impl ChatRuntimeTool for StartTutorRunTool {
    fn name(&self) -> &str {
        "start_tutor_run"
    }

    fn description(&self) -> &str {
        "Start a Personal Tutor or App Copilot run for the current chat/HUD session. Use this first for @tutor/@tutur/hey tutor/hey tutur concept-teaching flows and @copilot/@app-copilot/hey copilot app-help flows, then record each observe/resolve/draw/action/verify step with propose_tutor_step. Action-oriented app prompts belong to App Copilot and should use guided_action, while visible math/physics/computer-science diagrams, questions, formulas, code, or paused frames should use concept_explainer, guided_solution, or concept_demo."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "The user's tutor goal in plain language."
                },
                "mode": {
                    "type": "string",
                    "enum": ["explain_only", "guided_action", "demo_and_cleanup", "concept_explainer", "guided_solution", "concept_demo"],
                    "description": "explain_only draws/explains only; guided_action draws then delegates reversible app actions for prompts like show me how/walk me through an app; demo_and_cleanup may clean up only objects recorded as created in this run; concept_explainer explains visible math/physics/CS concepts; guided_solution works through a visible question step by step; concept_demo uses temporary overlays for visual intuition."
                },
                "initial_observation": {
                    "type": "boolean",
                    "description": "Set true when the current HUD/screen prompt already includes a fresh screenshot/observation. Defaults true."
                },
                "canvas_mode": {
                    "type": "string",
                    "enum": ["screen_overlay", "blackboard"],
                    "description": "screen_overlay annotates the user's current screen and requires observed/resolved targets. blackboard creates a synthetic teaching canvas for generic concepts with no relevant visible source. Defaults from the current tutor prompt."
                },
                "visual_entity_map": {
                    "type": "object",
                    "description": "Optional per-observation VisualEntityMap extracted from the current screenshot. Use only when initial_observation is true. Include observation_id, coordinate_space {width,height,unit}, and entities with ids, kinds, geometry, labels/text/evidence, and optional confidence 0-100."
                }
            },
            "required": ["goal"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(goal) = text_arg(args, "goal") else {
            return Ok(json!({"status": "error", "reason": "start_tutor_run requires `goal`"}));
        };
        let explicit_mode = match optional_tutor_mode(args.get("mode")) {
            Ok(mode) => mode,
            Err(reason) => return Ok(json!({"status": "error", "reason": reason})),
        };
        let app_copilot_lane = ctx.feature_mode == FeatureMode::AppCopilot;
        let inferred_mode = ctx
            .current_user_text
            .as_deref()
            .map(|text| classify_tutor_or_app_copilot_turn_mode_for_lane(text, app_copilot_lane))
            .unwrap_or_else(|| {
                classify_tutor_or_app_copilot_turn_mode_for_lane(&goal, app_copilot_lane)
            });
        let concept_tutor_prompt = ctx.feature_mode == FeatureMode::Tutor;
        let mode = match (explicit_mode, inferred_mode) {
            (Some(TutorRunMode::GuidedAction | TutorRunMode::DemoAndCleanup), inferred)
                if concept_tutor_prompt =>
            {
                inferred
            },
            (Some(TutorRunMode::ExplainOnly), inferred)
                if inferred != TutorRunMode::ExplainOnly =>
            {
                inferred
            },
            (Some(TutorRunMode::GuidedAction), inferred) if inferred.is_concept_mode() => inferred,
            (Some(TutorRunMode::DemoAndCleanup), inferred) if inferred.is_concept_mode() => {
                inferred
            },
            (Some(mode), _) => mode,
            (None, inferred) => inferred,
        };
        let explicit_initial_observation = args.get("initial_observation").and_then(Value::as_bool);
        let explicit_canvas_mode = match optional_tutor_canvas_mode(args.get("canvas_mode")) {
            Ok(mode) => mode,
            Err(reason) => return Ok(json!({"status": "error", "reason": reason})),
        };
        let visual_entity_map = match parse_visual_entity_map_arg(args.get("visual_entity_map")) {
            Ok(map) => map,
            Err(reason) => return Ok(json!({"status": "error", "reason": reason})),
        };
        let canvas_mode = explicit_canvas_mode.unwrap_or_else(|| {
            ctx.current_user_text
                .as_deref()
                .map(|text| {
                    classify_tutor_or_app_copilot_canvas_mode_for_lane(
                        text,
                        visual_entity_map.is_some(),
                        app_copilot_lane,
                    )
                })
                .unwrap_or_else(|| {
                    classify_tutor_or_app_copilot_canvas_mode_for_lane(
                        &goal,
                        visual_entity_map.is_some(),
                        app_copilot_lane,
                    )
                })
        });
        let initial_observation =
            explicit_initial_observation.unwrap_or(!canvas_mode.is_blackboard());
        if canvas_mode == TutorCanvasMode::Blackboard
            && explicit_initial_observation == Some(true)
            && visual_entity_map.is_none()
        {
            return Ok(json!({
                "status": "error",
                "reason": "blackboard tutor runs must use initial_observation=false; use screen_overlay for visible screen sources"
            }));
        }
        if canvas_mode == TutorCanvasMode::Blackboard && visual_entity_map.is_some() {
            return Ok(json!({
                "status": "error",
                "reason": "blackboard tutor runs cannot include visual_entity_map; use screen_overlay for visible screen sources"
            }));
        }
        if visual_entity_map.is_some() && !initial_observation {
            return Ok(json!({
                "status": "error",
                "reason": "visual_entity_map can be supplied only when initial_observation is true"
            }));
        }
        let scope = tutor_scope_from_chat(ctx);
        match tutor_run_store().active_run(&scope) {
            Ok(Some(run)) if run.status == TutorRunStatus::Running => {
                return Ok(tutor_run_ok("already_active", run));
            },
            Ok(_) => {},
            Err(reason) => return Ok(json!({"status": "error", "reason": reason})),
        }
        match tutor_run_store().start_run(
            scope,
            mode,
            canvas_mode,
            goal,
            initial_observation,
            visual_entity_map,
        ) {
            Ok(run) => {
                emit_tutor_run_progress(ctx, RuntimeAgentEventType::TutorRunStarted, &run, None);
                Ok(tutor_run_ok("started", run))
            },
            Err(reason) => Ok(json!({"status": "error", "reason": reason})),
        }
    }
}

struct GetTutorRunTool;

#[async_trait]
impl ChatRuntimeTool for GetTutorRunTool {
    fn name(&self) -> &str {
        "get_tutor_run"
    }

    fn description(&self) -> &str {
        "Return the active Personal Tutor run for this chat session, or a specific run by id."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "Optional tutor run id. Defaults to the active run for this chat session."
                }
            }
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let store = tutor_run_store();
        let run = if let Some(run_id) = text_arg(args, "run_id") {
            store.get_run(&run_id)
        } else {
            store.active_run(&tutor_scope_from_chat(ctx))
        };
        match run {
            Ok(Some(run)) => Ok(tutor_run_ok("ok", run)),
            Ok(None) => Ok(json!({"status": "not_found"})),
            Err(reason) => Ok(json!({"status": "error", "reason": reason})),
        }
    }
}

fn copilot_user_action_check_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "run_id": {
                "type": "string",
                "description": "Tutor run id. Defaults to the active run for this chat session."
            },
            "storyboard_step_id": {
                "type": "string",
                "description": "Storyboard step id from the immediately preceding screen-draw preview."
            },
            "storyboard_step_label": {
                "type": "string",
                "description": "Storyboard step label from the immediately preceding screen-draw preview. Used when no step id is available."
            },
            "action_kind": {
                "type": "string",
                "enum": ["click", "type_text", "hotkey", "scroll"],
                "description": "UI action the user may already have performed. Defaults to click."
            },
            "target": {
                "type": "string",
                "description": "Resolved UI target highlighted for the user."
            },
            "expected_state": {
                "type": "string",
                "description": "Visible state expected after the user or automation performs the step."
            }
        },
        "required": ["target", "expected_state"]
    })
}

async fn dispatch_copilot_user_action_check(
    args: &Value,
    ctx: &ChatRuntimeToolContext,
) -> Result<Value> {
    let run_id = match resolve_tutor_run_id(args, ctx) {
        Ok(run_id) => run_id,
        Err(value) => return Ok(value),
    };
    let target = text_arg(args, "target")
        .unwrap_or_else(|| "the highlighted App Copilot target".to_string());
    let expected_state = text_arg(args, "expected_state")
        .unwrap_or_else(|| "The highlighted App Copilot step is complete".to_string());
    let storyboard_step_id = text_arg(args, "storyboard_step_id");
    let storyboard_step_label = text_arg(args, "storyboard_step_label");
    let action_kind = match args
        .get("action_kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("click")
    {
        "click" => TutorStepKind::Click,
        "type_text" => TutorStepKind::TypeText,
        "hotkey" => TutorStepKind::Hotkey,
        "scroll" => TutorStepKind::Scroll,
        other => {
            return Ok(json!({
                "status": "error",
                "reason": format!("unsupported action_kind `{other}`")
            }));
        },
    };
    // Exact storyboard identities are run-scoped and single-use, so they do
    // not need a fragile sub-second timing window. Preserve the short window
    // only for legacy untagged overlay events.
    let since_ms = if storyboard_step_id.is_some() || storyboard_step_label.is_some() {
        0
    } else {
        Utc::now().timestamp_millis().saturating_sub(1_500)
    };
    match tutor_run_store().take_matching_user_action_event(
        &run_id,
        storyboard_step_id.as_deref(),
        storyboard_step_label.as_deref(),
        since_ms,
    ) {
        Ok(Some(event)) => {
            let action_step = TutorStep {
                kind: action_kind,
                label: format!("user performed {target}"),
                target: Some(target.clone()),
                expected_state: Some(expected_state.clone()),
                safety: TutorSafetyLevel::ReversibleAction,
                source_entity_ids: Vec::new(),
                visual_entity_map: None,
            };
            match tutor_run_store().apply_user_completed_copilot_step(&event, action_step.clone()) {
                Ok(run) => {
                    emit_tutor_step_progress(
                        ctx,
                        RuntimeAgentEventType::TutorStepActionDelegated,
                        &run,
                        &action_step,
                        TutorStepStatus::Succeeded,
                        Some("User already performed the highlighted Copilot step manually; skip automation for this step."),
                    );
                    Ok(json!({
                        "status": "user_acted",
                        "should_automate": false,
                        "run_id": run.run_id,
                        "target": target,
                        "expected_state": expected_state,
                        "evidence": event.evidence,
                        "storyboard_step_id": event.storyboard_step_id,
                        "storyboard_step_label": event.storyboard_step_label,
                        "instruction": "Do not delegate this action. The storyboard-bound user event has advanced the run through action, observation, and verification; continue to the next step or complete."
                    }))
                },
                Err(reason) => Ok(json!({
                    "status": "error",
                    "run_id": run_id,
                    "reason": reason
                })),
            }
        },
        Ok(None) => match tutor_run_store().record_copilot_action_check(
            &run_id,
            storyboard_step_id.clone(),
            storyboard_step_label.clone(),
        ) {
            Ok(run) => Ok(json!({
                "status": "no_user_action",
                "should_automate": true,
                "run_id": run.run_id,
                "target": target,
                "expected_state": expected_state,
                "storyboard_step_id": storyboard_step_id,
                "storyboard_step_label": storyboard_step_label,
                "authorization": "fresh_storyboard_bound_no_user_action_check",
                "instruction": "No user action was reported for this storyboard step. Delegate immediately to mac-operator with the same tutor_action storyboard identity; the receipt is single-use and expires quickly."
            })),
            Err(reason) => Ok(json!({
                "status": "error",
                "run_id": run_id,
                "reason": reason
            })),
        },
        Err(reason) => Ok(json!({
            "status": "error",
            "run_id": run_id,
            "reason": reason
        })),
    }
}

struct CheckForCopilotUserActionTool;

#[async_trait]
impl ChatRuntimeTool for CheckForCopilotUserActionTool {
    fn name(&self) -> &str {
        "check_for_copilot_user_action"
    }

    fn description(&self) -> &str {
        "App Copilot only. Immediately check whether the user already performed the just-highlighted step. This does not wait. If no recent user action is present, delegate to mac-operator immediately; later user clicks are handled by backend preemption."
    }

    fn parameters_schema(&self) -> Value {
        copilot_user_action_check_schema()
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        dispatch_copilot_user_action_check(args, ctx).await
    }
}

struct WaitForCopilotUserActionTool;

#[async_trait]
impl ChatRuntimeTool for WaitForCopilotUserActionTool {
    fn name(&self) -> &str {
        "wait_for_copilot_user_action"
    }

    fn description(&self) -> &str {
        "Compatibility alias for check_for_copilot_user_action. It does not wait; App Copilot should proceed with automation immediately when no recent user action is present."
    }

    fn parameters_schema(&self) -> Value {
        copilot_user_action_check_schema()
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        dispatch_copilot_user_action_check(args, ctx).await
    }
}

struct ProposeTutorStepTool;

#[async_trait]
impl ChatRuntimeTool for ProposeTutorStepTool {
    fn name(&self) -> &str {
        "propose_tutor_step"
    }

    fn description(&self) -> &str {
        "Validate and record the next Personal Tutor step. Call before/around screen-draw, macos-ui-automation, or delegated mac-operator actions so Rust enforces observe -> resolve -> draw/act -> observe -> verify ordering."
    }

    fn parameters_schema(&self) -> Value {
        tutor_step_parameters_schema(false)
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let run_id = match resolve_tutor_run_id(args, ctx) {
            Ok(run_id) => run_id,
            Err(value) => return Ok(value),
        };
        let step = match tutor_step_from_args(args) {
            Ok(step) => step,
            Err(reason) => return Ok(json!({"status": "error", "reason": reason})),
        };
        match tutor_run_store().propose_step(&run_id, step.clone()) {
            Ok(run) => {
                emit_tutor_step_progress(
                    ctx,
                    tutor_step_event_type(step.kind, TutorStepStatus::Succeeded),
                    &run,
                    &step,
                    TutorStepStatus::Succeeded,
                    None,
                );
                Ok(tutor_run_ok("recorded", run))
            },
            Err(reason) => Ok(json!({"status": "error", "run_id": run_id, "reason": reason})),
        }
    }
}

struct RecordTutorStepFailureTool;

#[async_trait]
impl ChatRuntimeTool for RecordTutorStepFailureTool {
    fn name(&self) -> &str {
        "record_tutor_step_failure"
    }

    fn description(&self) -> &str {
        "Record that a tutor step failed or could not be verified. Use this when a target is missing, a UI action fails, or verification does not match the expected state."
    }

    fn parameters_schema(&self) -> Value {
        tutor_step_parameters_schema(false)
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let run_id = match resolve_tutor_run_id(args, ctx) {
            Ok(run_id) => run_id,
            Err(value) => return Ok(value),
        };
        let step = match tutor_step_from_args(args) {
            Ok(step) => step,
            Err(reason) => return Ok(json!({"status": "error", "reason": reason})),
        };
        match tutor_run_store().record_step_failure(&run_id, step.clone()) {
            Ok(run) => {
                emit_tutor_step_progress(
                    ctx,
                    RuntimeAgentEventType::TutorStepFailed,
                    &run,
                    &step,
                    TutorStepStatus::Failed,
                    None,
                );
                if run.status == TutorRunStatus::Failed {
                    emit_tutor_run_progress(
                        ctx,
                        RuntimeAgentEventType::TutorRunFailed,
                        &run,
                        run.terminal_reason.as_deref(),
                    );
                }
                Ok(tutor_run_ok("recorded", run))
            },
            Err(reason) => Ok(json!({"status": "error", "run_id": run_id, "reason": reason})),
        }
    }
}

struct RecordTutorCreatedObjectTool;

#[async_trait]
impl ChatRuntimeTool for RecordTutorCreatedObjectTool {
    fn name(&self) -> &str {
        "record_tutor_created_object"
    }

    fn description(&self) -> &str {
        "Record an object created during this Personal Tutor run. Cleanup/delete can be automatic only for objects recorded here."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "Tutor run id. Defaults to the active run for this chat session."
                },
                "label": {
                    "type": "string",
                    "description": "Human-readable object label, e.g. temporary item."
                },
                "object_type": {
                    "type": "string",
                    "description": "Optional object type, e.g. item, draft, file, record."
                },
                "evidence": {
                    "type": "string",
                    "description": "Optional visible evidence that this object was created in the current run."
                }
            },
            "required": ["label"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let run_id = match resolve_tutor_run_id(args, ctx) {
            Ok(run_id) => run_id,
            Err(value) => return Ok(value),
        };
        let Some(label) = text_arg(args, "label") else {
            return Ok(
                json!({"status": "error", "reason": "record_tutor_created_object requires `label`"}),
            );
        };
        let object = TutorCreatedObject {
            label,
            object_type: text_arg(args, "object_type"),
            evidence: text_arg(args, "evidence"),
        };
        match tutor_run_store().record_created_object(&run_id, object) {
            Ok(run) => Ok(tutor_run_ok("recorded", run)),
            Err(reason) => Ok(json!({"status": "error", "run_id": run_id, "reason": reason})),
        }
    }
}

struct CompleteTutorRunTool;

#[async_trait]
impl ChatRuntimeTool for CompleteTutorRunTool {
    fn name(&self) -> &str {
        "complete_tutor_run"
    }

    fn description(&self) -> &str {
        "Complete the active Personal Tutor run only after its lesson plan is ready and every planned milestone is satisfied, a storyboard-bound drawing remains after the latest overlay clear, no recorded failure remains unresolved, and all pending UI-changing actions have been verified. Normal Tutor uses an adaptive progressive plan with a 1/2/3 minimum floor; Tutor Quick uses the essential condensed range returned in its contract. Narrated screen-draw storyboard ids must match planned milestone ids; unplanned ids and text-only Say steps do not satisfy coverage. App Copilot retains its independent action-completion policy."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "Optional tutor run id. Defaults to the active run for this chat session."
                }
            }
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let run_id = match resolve_tutor_run_id(args, ctx) {
            Ok(run_id) => run_id,
            Err(value) => return Ok(value),
        };
        match tutor_run_store().complete_run(&run_id) {
            Ok(run) => {
                emit_tutor_run_progress(ctx, RuntimeAgentEventType::TutorRunCompleted, &run, None);
                Ok(tutor_run_ok("completed", run))
            },
            Err(reason) => Ok(json!({"status": "error", "run_id": run_id, "reason": reason})),
        }
    }
}

fn emit_tutor_run_progress(
    ctx: &ChatRuntimeToolContext,
    event_type: RuntimeAgentEventType,
    run: &TutorRun,
    note: Option<&str>,
) {
    let event_type_str = event_type.as_str();
    ctx.event_broadcaster.emit_named(
        event_type_str,
        &ctx.agent_id,
        Some(&ctx.principal),
        Some(&ctx.workspace),
        json!({
            "event_id": tutor_run_event_id(event_type_str, run),
            "chat_turn_id": ctx.chat_turn_id.as_deref(),
            "chat_session_id": ctx.session_id.as_str(),
            "run_id": run.run_id.as_str(),
            "goal": run.goal.as_str(),
            "mode": run.mode.as_str(),
            "canvas_mode": run.canvas_mode.as_str(),
            "status": run.status.as_str(),
            "step_count": run.step_history.len(),
            "note": note,
        }),
    );
}

fn emit_tutor_step_progress(
    ctx: &ChatRuntimeToolContext,
    event_type: RuntimeAgentEventType,
    run: &TutorRun,
    step: &TutorStep,
    step_status: TutorStepStatus,
    note: Option<&str>,
) {
    let event_type_str = event_type.as_str();
    let step_id = tutor_step_event_id(run, step);
    ctx.event_broadcaster.emit_named(
        event_type_str,
        &ctx.agent_id,
        Some(&ctx.principal),
        Some(&ctx.workspace),
        json!({
            "event_id": tutor_step_event_id_for_type(event_type_str, run, step),
            "step_id": step_id,
            "chat_turn_id": ctx.chat_turn_id.as_deref(),
            "chat_session_id": ctx.session_id.as_str(),
            "run_id": run.run_id.as_str(),
            "goal": run.goal.as_str(),
            "mode": run.mode.as_str(),
            "canvas_mode": run.canvas_mode.as_str(),
            "run_status": run.status.as_str(),
            "step_kind": step.kind.as_str(),
            "step_label": step.label.as_str(),
            "step_status": step_status.as_str(),
            "target": step.target.as_deref(),
            "expected_state": step.expected_state.as_deref(),
            "source_entity_ids": &step.source_entity_ids,
            "visual_entity_observation_id": run.latest_visual_entity_map.as_ref().map(|map| map.observation_id.as_str()),
            "visual_entity_count": run.latest_visual_entity_map.as_ref().map(|map| map.entities.len()).unwrap_or(0),
            "step_count": run.step_history.len(),
            "note": note,
        }),
    );
}

fn tutor_run_event_id(event_type: &str, run: &TutorRun) -> String {
    format!(
        "tutor-run:{}:{}:{}",
        run.run_id.as_str(),
        event_type,
        run.step_history.len()
    )
}

fn tutor_step_event_id(run: &TutorRun, step: &TutorStep) -> String {
    format!(
        "tutor-step:{}:{}:{}",
        run.run_id.as_str(),
        run.step_history.len(),
        step.kind.as_str()
    )
}

fn tutor_step_event_id_for_type(event_type: &str, run: &TutorRun, step: &TutorStep) -> String {
    format!("{}:{}", tutor_step_event_id(run, step), event_type)
}

fn tutor_step_event_type(kind: TutorStepKind, status: TutorStepStatus) -> RuntimeAgentEventType {
    if status == TutorStepStatus::Failed {
        return RuntimeAgentEventType::TutorStepFailed;
    }
    match kind {
        TutorStepKind::Observe => RuntimeAgentEventType::TutorStepObserved,
        TutorStepKind::ResolveTarget => RuntimeAgentEventType::TutorStepTargetResolved,
        TutorStepKind::Draw => RuntimeAgentEventType::TutorStepDrawing,
        TutorStepKind::Verify => RuntimeAgentEventType::TutorStepVerified,
        TutorStepKind::Recover => RuntimeAgentEventType::TutorStepRecovering,
        TutorStepKind::ClearDrawings => RuntimeAgentEventType::TutorStepClearing,
        TutorStepKind::Click
        | TutorStepKind::TypeText
        | TutorStepKind::Hotkey
        | TutorStepKind::Scroll => RuntimeAgentEventType::TutorStepActionDelegated,
        TutorStepKind::Say | TutorStepKind::Wait | TutorStepKind::Confirm => {
            RuntimeAgentEventType::TutorStepObserved
        },
    }
}

fn tutor_scope_from_chat(ctx: &ChatRuntimeToolContext) -> TutorRunScope {
    TutorRunScope::new(
        ctx.principal.clone(),
        ctx.workspace.clone(),
        ctx.session_id.clone(),
    )
}

fn resolve_tutor_run_id(
    args: &Value,
    ctx: &ChatRuntimeToolContext,
) -> std::result::Result<String, Value> {
    if let Some(run_id) = text_arg(args, "run_id") {
        return Ok(run_id);
    }
    match tutor_run_store().active_run(&tutor_scope_from_chat(ctx)) {
        Ok(Some(run)) => Ok(run.run_id),
        Ok(None) => Err(json!({
            "status": "error",
            "reason": "No active tutor run for this chat session. Call start_tutor_run first."
        })),
        Err(reason) => Err(json!({"status": "error", "reason": reason})),
    }
}

fn tutor_run_ok(status: &str, run: TutorRun) -> Value {
    let visual_entity_context = run.visual_entity_prompt_context();
    let lesson_progress = run.lesson_progress();
    // Thinking Map → Tutor adapter: surface the owner-registered map digest at
    // top level (like visual_entity_context) so the model reads it as
    // grounding reference. Absent for every run without a binding, keeping the
    // pre-existing tool-result shape byte-identical.
    let thinking_map_context = run.thinking_map_prompt_context().map(str::to_string);
    let mut value = json!({
        "status": status,
        "run_id": run.run_id,
        "canvas_mode": run.canvas_mode.as_str(),
        "visual_entity_context": visual_entity_context,
        "run": run,
    });
    if let Some(map_context) = thinking_map_context {
        value["thinking_map_context"] = Value::String(map_context);
    }
    if let Some(progress) = lesson_progress {
        value["lesson_progress"] = json!(progress);
    }
    value
}

fn tutor_step_parameters_schema(run_id_required: bool) -> Value {
    let mut required = vec!["kind", "label"];
    if run_id_required {
        required.insert(0, "run_id");
    }
    json!({
        "type": "object",
        "properties": {
            "run_id": {
                "type": "string",
                "description": "Tutor run id. Defaults to the active run for this chat session."
            },
            "kind": {
                "type": "string",
                "enum": [
                    "observe",
                    "resolve_target",
                    "draw",
                    "say",
                    "wait",
                    "click",
                    "type_text",
                    "hotkey",
                    "scroll",
                    "verify",
                    "clear_drawings",
                    "confirm",
                    "recover"
                ]
            },
            "label": {
                "type": "string",
                "description": "Short human-readable description of this tutor step."
            },
            "target": {
                "type": "string",
                "description": "Optional target UI element/object, e.g. primary action button."
            },
            "expected_state": {
                "type": "string",
                "description": "Optional expected visible state after the step."
            },
            "source_entity_ids": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional VisualEntityMap entity ids from the latest fresh observation that ground this resolve/draw/say/verify step. These ids become invalid after any UI-changing action or recover step."
            },
            "visual_entity_map": {
                "type": "object",
                "description": "Optional VisualEntityMap for observe steps only. Include observation_id, coordinate_space {width,height,unit}, and entities [{id,kind,label,text,confidence,geometry,source_evidence}]. Valid kinds include point, line_segment, ray, angle, polygon, circle, arc, axis, vector, region, text_region, formula_region, code_region, table_region, diagram_node, and diagram_edge."
            },
            "safety": {
                "type": "string",
                "enum": [
                    "visual_only",
                    "reversible_action",
                    "session_owned_destructive",
                    "destructive_requires_confirmation"
                ],
                "description": "Safety level. Defaults to visual_only for visual steps and reversible_action for UI-changing steps."
            }
        },
        "required": required
    })
}

fn tutor_step_from_args(args: &Value) -> std::result::Result<TutorStep, String> {
    let kind = parse_tutor_step_kind(
        args.get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| "tutor step requires `kind`".to_string())?,
    )?;
    let label = text_arg(args, "label")
        .ok_or_else(|| "tutor step requires non-empty `label`".to_string())?;
    let safety = match args.get("safety") {
        Some(value) => parse_tutor_safety(
            value
                .as_str()
                .ok_or_else(|| "tutor `safety` must be a string".to_string())?,
        )?,
        None if kind.changes_ui_state() => TutorSafetyLevel::ReversibleAction,
        None => TutorSafetyLevel::VisualOnly,
    };
    let source_entity_ids = parse_source_entity_ids(args.get("source_entity_ids"))?;
    let visual_entity_map = parse_visual_entity_map_arg(args.get("visual_entity_map"))?;
    Ok(TutorStep {
        kind,
        label,
        target: text_arg(args, "target"),
        expected_state: text_arg(args, "expected_state"),
        safety,
        source_entity_ids,
        visual_entity_map,
    })
}

fn parse_source_entity_ids(value: Option<&Value>) -> std::result::Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err("source_entity_ids must be an array of strings".to_string());
    };
    let mut ids = Vec::new();
    for value in values {
        let Some(id) = value.as_str().map(str::trim).filter(|id| !id.is_empty()) else {
            return Err("source_entity_ids must contain only non-empty strings".to_string());
        };
        if !ids.iter().any(|existing| existing == id) {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

fn parse_visual_entity_map_arg(
    value: Option<&Value>,
) -> std::result::Result<Option<TutorVisualEntityMap>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let map: TutorVisualEntityMap = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid visual_entity_map: {error}"))?;
    validate_visual_entity_map(&map)
        .map_err(|reason| format!("invalid visual_entity_map: {reason}"))?;
    Ok(Some(map))
}

fn optional_tutor_mode(value: Option<&Value>) -> std::result::Result<Option<TutorRunMode>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(raw) = value.as_str() else {
        return Err("tutor `mode` must be a string".to_string());
    };
    Ok(Some(parse_tutor_mode(raw)?))
}

fn optional_tutor_canvas_mode(
    value: Option<&Value>,
) -> std::result::Result<Option<TutorCanvasMode>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(raw) = value.as_str() else {
        return Err("tutor `canvas_mode` must be a string".to_string());
    };
    match raw.trim() {
        "" => Ok(None),
        value => parse_tutor_canvas_mode(value).map(Some),
    }
}

fn parse_tutor_mode(raw: &str) -> std::result::Result<TutorRunMode, String> {
    match raw.trim() {
        "explain_only" => Ok(TutorRunMode::ExplainOnly),
        "guided_action" => Ok(TutorRunMode::GuidedAction),
        "demo_and_cleanup" => Ok(TutorRunMode::DemoAndCleanup),
        "concept_explainer" => Ok(TutorRunMode::ConceptExplainer),
        "guided_solution" => Ok(TutorRunMode::GuidedSolution),
        "concept_demo" => Ok(TutorRunMode::ConceptDemo),
        other => Err(format!(
            "unsupported tutor mode `{other}`; expected explain_only, guided_action, demo_and_cleanup, concept_explainer, guided_solution, or concept_demo"
        )),
    }
}

fn parse_tutor_step_kind(raw: &str) -> std::result::Result<TutorStepKind, String> {
    match raw.trim() {
        "observe" => Ok(TutorStepKind::Observe),
        "resolve_target" => Ok(TutorStepKind::ResolveTarget),
        "draw" => Ok(TutorStepKind::Draw),
        "say" => Ok(TutorStepKind::Say),
        "wait" => Ok(TutorStepKind::Wait),
        "click" => Ok(TutorStepKind::Click),
        "type_text" => Ok(TutorStepKind::TypeText),
        "hotkey" => Ok(TutorStepKind::Hotkey),
        "scroll" => Ok(TutorStepKind::Scroll),
        "verify" => Ok(TutorStepKind::Verify),
        "clear_drawings" => Ok(TutorStepKind::ClearDrawings),
        "confirm" => Ok(TutorStepKind::Confirm),
        "recover" => Ok(TutorStepKind::Recover),
        other => Err(format!("unsupported tutor step kind `{other}`")),
    }
}

fn parse_tutor_safety(raw: &str) -> std::result::Result<TutorSafetyLevel, String> {
    match raw.trim() {
        "visual_only" => Ok(TutorSafetyLevel::VisualOnly),
        "reversible_action" => Ok(TutorSafetyLevel::ReversibleAction),
        "session_owned_destructive" => Ok(TutorSafetyLevel::SessionOwnedDestructive),
        "destructive_requires_confirmation" => {
            Ok(TutorSafetyLevel::DestructiveRequiresConfirmation)
        },
        other => Err(format!("unsupported tutor safety level `{other}`")),
    }
}

fn text_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

// ---------------------------------------------------------------------------
// Session lifecycle tools
// ---------------------------------------------------------------------------

struct ArchiveChatSessionTool;

#[async_trait]
impl ChatRuntimeTool for ArchiveChatSessionTool {
    fn name(&self) -> &str {
        "archive_chat_session"
    }

    fn description(&self) -> &str {
        "Archive a chat session. Archived chats are hidden from the active list but remain in history."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The chat session ID to archive. Defaults to the current session."
                }
            }
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let session_id = args
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&ctx.session_id)
            .to_string();
        let session = ctx
            .chat_store
            .get_session(&session_id)
            .await
            .context("archive_chat_session session lookup failed")?;
        let Some(session) = session else {
            return Ok(
                json!({"status": "error", "reason": format!("Chat session '{session_id}' not found.")}),
            );
        };
        if session.principal != ctx.principal || session.workspace != ctx.workspace {
            return Ok(
                json!({"status": "error", "reason": format!("Chat session '{session_id}' is not in this scope.")}),
            );
        }
        ctx.chat_store
            .update_session_status(&session_id, "archived")
            .await
            .context("archive_chat_session update failed")?;
        Ok(json!({"status": "ok", "session_id": session_id}))
    }
}

struct DeleteChatSessionTool;

#[async_trait]
impl ChatRuntimeTool for DeleteChatSessionTool {
    fn name(&self) -> &str {
        "delete_chat_session"
    }

    fn description(&self) -> &str {
        "Permanently delete a chat session and all of its messages. This cannot be undone."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The chat session ID to delete."
                }
            },
            "required": ["session_id"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(session_id) = args
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(
                json!({"status": "error", "reason": "delete_chat_session requires `session_id`"}),
            );
        };
        let session = ctx
            .chat_store
            .get_session(session_id)
            .await
            .context("delete_chat_session session lookup failed")?;
        let Some(session) = session else {
            return Ok(
                json!({"status": "error", "reason": format!("Chat session '{session_id}' not found.")}),
            );
        };
        if session.principal != ctx.principal || session.workspace != ctx.workspace {
            return Ok(
                json!({"status": "error", "reason": format!("Chat session '{session_id}' is not in this scope.")}),
            );
        }
        let cleanup = CanonicalRawResultStore::for_lifecycle_cleanup(ctx.workspace_layout.clone())
            .cleanup_owner(
                &crate::magician_v2::artifact_v2::service::ScopeRef::system_internal_unauthenticated(&session.principal.clone(), &session.workspace.clone()),
                &RawResultOwner::Chat {
                    session_id: session.id.clone(),
                },
            )
            .await
            .context("delete_chat_session canonical result cleanup failed")?;
        if cleanup.failed > 0 {
            anyhow::bail!(
                "delete_chat_session refused to remove its owner while {} canonical result cleanup operation(s) remain incomplete",
                cleanup.failed
            );
        }
        ctx.chat_store
            .delete_session(session_id)
            .await
            .context("delete_chat_session failed")?;
        Ok(json!({"status": "ok", "session_id": session_id}))
    }
}

struct UnarchiveChatSessionTool;

#[async_trait]
impl ChatRuntimeTool for UnarchiveChatSessionTool {
    fn name(&self) -> &str {
        "unarchive_chat_session"
    }

    fn description(&self) -> &str {
        "Restore a previously archived chat session to the active list."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The chat session ID to unarchive."
                }
            },
            "required": ["session_id"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(session_id) = args
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(
                json!({"status": "error", "reason": "unarchive_chat_session requires `session_id`"}),
            );
        };
        let session = ctx
            .chat_store
            .get_session(session_id)
            .await
            .context("unarchive_chat_session session lookup failed")?;
        let Some(session) = session else {
            return Ok(
                json!({"status": "error", "reason": format!("Chat session '{session_id}' not found.")}),
            );
        };
        if session.principal != ctx.principal || session.workspace != ctx.workspace {
            return Ok(
                json!({"status": "error", "reason": format!("Chat session '{session_id}' is not in this scope.")}),
            );
        }
        ctx.chat_store
            .update_session_status(session_id, "active")
            .await
            .context("unarchive_chat_session update failed")?;
        Ok(json!({"status": "ok", "session_id": session_id}))
    }
}

struct ListChatSessionsTool;

#[async_trait]
impl ChatRuntimeTool for ListChatSessionsTool {
    fn name(&self) -> &str {
        "list_chat_sessions"
    }

    fn description(&self) -> &str {
        "List chat sessions in the current scope. Optionally filter by thread or include archived sessions."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thread_id": {
                    "type": "string",
                    "description": "Optional: filter to sessions in this thread."
                },
                "include_archived": {
                    "type": "boolean",
                    "description": "Include archived sessions (default: false)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of sessions to return (default: 50)."
                }
            }
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let thread_filter = args
            .get("thread_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let include_archived = args
            .get("include_archived")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(50);
        let sessions = ctx
            .chat_store
            .list_sessions(&ctx.principal, &ctx.workspace)
            .await
            .context("list_chat_sessions failed")?;
        let filtered: Vec<Value> = sessions
            .into_iter()
            .filter(|session| {
                if !include_archived && session.status == ChatSessionStatus::Archived {
                    return false;
                }
                if let Some(thread) = thread_filter.as_deref() {
                    return session.ui_thread_id == thread;
                }
                true
            })
            .take(limit)
            .map(|session| {
                json!({
                    "session_id": session.id,
                    "title": session.title,
                    "thread_id": session.ui_thread_id,
                    "status": format!("{:?}", session.status).to_ascii_lowercase(),
                    "agent_id": session.agent_id,
                    "updated_at": session.updated_at,
                })
            })
            .collect();
        Ok(json!({
            "status": "ok",
            "count": filtered.len(),
            "sessions": filtered,
        }))
    }
}

struct SwitchChatSessionTool;

#[async_trait]
impl ChatRuntimeTool for SwitchChatSessionTool {
    fn name(&self) -> &str {
        "switch_chat_session"
    }

    fn description(&self) -> &str {
        "Signal the UI to switch to a different chat session. The session id is returned so the front-end can navigate."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The chat session ID to switch to."
                }
            },
            "required": ["session_id"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(session_id) = args
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(
                json!({"status": "error", "reason": "switch_chat_session requires `session_id`"}),
            );
        };
        let session = ctx
            .chat_store
            .get_session(session_id)
            .await
            .context("switch_chat_session session lookup failed")?;
        let Some(session) = session else {
            return Ok(
                json!({"status": "error", "reason": format!("Chat session '{session_id}' not found.")}),
            );
        };
        if session.principal != ctx.principal || session.workspace != ctx.workspace {
            return Ok(
                json!({"status": "error", "reason": format!("Chat session '{session_id}' is not in this scope.")}),
            );
        }
        Ok(json!({
            "status": "ok",
            "intent": "switch_chat_session",
            "session_id": session.id,
            "thread_id": session.ui_thread_id,
            "agent_id": session.agent_id,
        }))
    }
}

// ---------------------------------------------------------------------------
// Thread lifecycle tools
// ---------------------------------------------------------------------------

struct ArchiveChatThreadTool;

#[async_trait]
impl ChatRuntimeTool for ArchiveChatThreadTool {
    fn name(&self) -> &str {
        "archive_chat_thread"
    }

    fn description(&self) -> &str {
        "Archive a thread. The #general thread cannot be archived."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thread_id": {
                    "type": "string",
                    "description": "The thread ID to archive."
                }
            },
            "required": ["thread_id"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(thread_id) = args
            .get("thread_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(
                json!({"status": "error", "reason": "archive_chat_thread requires `thread_id`"}),
            );
        };
        match ctx
            .ui_thread_service
            .update_thread(
                &ctx.principal,
                &ctx.workspace,
                thread_id,
                None,
                Some(true),
                None,
                None,
                None,
            )
            .await
        {
            Ok(Some(detail)) => Ok(json!({
                "status": "ok",
                "thread_id": detail.record.id,
                "name": detail.record.name,
                "archived": detail.record.archived,
            })),
            Ok(None) => {
                Ok(json!({"status": "error", "reason": format!("Thread '{thread_id}' not found.")}))
            },
            Err(error) => Ok(
                json!({"status": "error", "reason": format!("archive_chat_thread failed: {error}")}),
            ),
        }
    }
}

struct CreateChatThreadTool;

#[async_trait]
impl ChatRuntimeTool for CreateChatThreadTool {
    fn name(&self) -> &str {
        "create_chat_thread"
    }

    fn description(&self) -> &str {
        "Create a new thread for organizing chat sessions and tasks."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thread_id": {
                    "type": "string",
                    "description": "Stable thread id (lowercase alphanumeric/hyphen)."
                },
                "name": {
                    "type": "string",
                    "description": "Optional display name; defaults to the thread id."
                }
            },
            "required": ["thread_id"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(thread_id) = args
            .get("thread_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(
                json!({"status": "error", "reason": "create_chat_thread requires `thread_id`"}),
            );
        };
        let name = args.get("name").and_then(Value::as_str);
        match ctx
            .ui_thread_service
            .create_thread(&ctx.principal, &ctx.workspace, thread_id, name)
            .await
        {
            Ok(record) => Ok(json!({
                "status": "ok",
                "thread_id": record.id,
                "name": record.name,
                "archived": record.archived,
            })),
            Err(error) => Ok(
                json!({"status": "error", "reason": format!("create_chat_thread failed: {error}")}),
            ),
        }
    }
}

struct DeleteChatThreadTool;

#[async_trait]
impl ChatRuntimeTool for DeleteChatThreadTool {
    fn name(&self) -> &str {
        "delete_chat_thread"
    }

    fn description(&self) -> &str {
        "Permanently archive a thread (no UiThreadService delete API yet — archives instead). Cannot be applied to #general."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thread_id": {
                    "type": "string",
                    "description": "The thread ID to delete."
                }
            },
            "required": ["thread_id"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(thread_id) = args
            .get("thread_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(
                json!({"status": "error", "reason": "delete_chat_thread requires `thread_id`"}),
            );
        };
        if thread_id == "general" {
            return Ok(json!({"status": "error", "reason": "Cannot delete the #general thread."}));
        }
        match ctx
            .ui_thread_service
            .update_thread(
                &ctx.principal,
                &ctx.workspace,
                thread_id,
                None,
                Some(true),
                None,
                None,
                None,
            )
            .await
        {
            Ok(Some(detail)) => Ok(json!({
                "status": "ok",
                "thread_id": detail.record.id,
                "name": detail.record.name,
                "archived": detail.record.archived,
                "note": "Archived; full deletion with task migration is not yet implemented.",
            })),
            Ok(None) => {
                Ok(json!({"status": "error", "reason": format!("Thread '{thread_id}' not found.")}))
            },
            Err(error) => Ok(
                json!({"status": "error", "reason": format!("delete_chat_thread failed: {error}")}),
            ),
        }
    }
}

struct ListChatThreadsTool;

#[async_trait]
impl ChatRuntimeTool for ListChatThreadsTool {
    fn name(&self) -> &str {
        "list_chat_threads"
    }

    fn description(&self) -> &str {
        "List all threads in the current scope with their archived flag and name."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, _args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        match ctx
            .ui_thread_service
            .list_threads(&ctx.principal, &ctx.workspace)
            .await
        {
            Ok(threads) => {
                let items: Vec<Value> = threads
                    .into_iter()
                    .map(|thread| {
                        json!({
                            "id": thread.id,
                            "name": thread.name,
                            "archived": thread.archived,
                        })
                    })
                    .collect();
                Ok(json!({"status": "ok", "count": items.len(), "threads": items}))
            },
            Err(error) => Ok(
                json!({"status": "error", "reason": format!("list_chat_threads failed: {error}")}),
            ),
        }
    }
}

struct SwitchChatThreadTool;

#[async_trait]
impl ChatRuntimeTool for SwitchChatThreadTool {
    fn name(&self) -> &str {
        "switch_chat_thread"
    }

    fn description(&self) -> &str {
        "Move the current chat session to a different thread. All subsequent messages and tasks are scoped to the new thread."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thread_id": {
                    "type": "string",
                    "description": "The thread ID to switch to."
                }
            },
            "required": ["thread_id"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(thread_id) = args
            .get("thread_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(
                json!({"status": "error", "reason": "switch_chat_thread requires `thread_id`"}),
            );
        };
        let session = ctx
            .chat_store
            .get_session(&ctx.session_id)
            .await
            .context("switch_chat_thread session lookup failed")?;
        let Some(session) = session else {
            return Ok(json!({"status": "error", "reason": "Active chat session not found."}));
        };
        if session.ui_thread_id == thread_id {
            return Ok(
                json!({"status": "ok", "thread_id": thread_id, "note": "Already in this thread."}),
            );
        }
        ctx.chat_store
            .update_session_thread(&ctx.session_id, thread_id)
            .await
            .context("switch_chat_thread update failed")?;
        Ok(json!({
            "status": "ok",
            "intent": "switch_chat_thread",
            "session_id": ctx.session_id,
            "thread_id": thread_id,
        }))
    }
}

// ---------------------------------------------------------------------------
// Introspection / personality
// ---------------------------------------------------------------------------

struct GetCurrentChatContextTool;

#[async_trait]
impl ChatRuntimeTool for GetCurrentChatContextTool {
    fn name(&self) -> &str {
        "get_current_chat_context"
    }

    fn description(&self) -> &str {
        "Return the current chat context: session id, thread, agent, principal, and workspace."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, _args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let session = ctx
            .chat_store
            .get_session(&ctx.session_id)
            .await
            .context("get_current_chat_context session lookup failed")?;
        let session_block = match session.as_ref() {
            Some(session) => json!({
                "session_id": session.id,
                "title": session.title,
                "agent_id": session.agent_id,
                "thread_id": session.ui_thread_id,
                "status": format!("{:?}", session.status).to_ascii_lowercase(),
                "updated_at": session.updated_at,
            }),
            None => json!({"session_id": ctx.session_id, "missing": true}),
        };
        let thread_id = session
            .as_ref()
            .map(|session| session.ui_thread_id.clone())
            .unwrap_or_else(|| "general".to_string());
        let thread_block = match ctx
            .ui_thread_service
            .get_thread(&ctx.principal, &ctx.workspace, &thread_id)
            .await
        {
            Ok(Some(detail)) => json!({
                "thread_id": detail.record.id,
                "name": detail.record.name,
                "archived": detail.record.archived,
            }),
            _ => json!({"thread_id": thread_id, "name": null, "archived": false}),
        };
        Ok(json!({
            "status": "ok",
            "principal": ctx.principal,
            "workspace": ctx.workspace,
            "agent_id": ctx.agent_id,
            "session": session_block,
            "thread": thread_block,
        }))
    }
}

/// `SwitchPersonalityTool` exposes the chat-runtime entry point for
/// personality swaps. Made `pub` so the `AgentBackend::switch_personality`
/// impl on `ChatService` can reuse the same dispatch logic for the
/// compiled-pack / universal-substrate path (autonomous etc.) instead
/// of duplicating the ~80 lines of skill lookup + memory tier write.
pub struct SwitchPersonalityTool;

#[async_trait]
impl ChatRuntimeTool for SwitchPersonalityTool {
    fn name(&self) -> &str {
        "switch_personality"
    }

    fn description(&self) -> &str {
        "Switch the agent's personality preset. Loads the named template and writes it to the personality_profile memory tier. Presets are discovered from the personality templates directory at runtime — call with mode=\"list\" to receive the current `available_modes` array, then call again with the chosen name. If an unknown mode is requested, the error response also includes `available_modes`."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "description": "Personality preset name (file stem under the personality templates dir)."
                }
            },
            "required": ["mode"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(mode) = args
            .get("mode")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
        else {
            return Ok(json!({"status": "error", "reason": "switch_personality requires `mode`"}));
        };
        if mode == "list" || mode == "?" {
            let available = available_modes(ctx).await;
            return Ok(json!({
                "status": "ok",
                "action": "list",
                "available_modes": available,
            }));
        }
        // Personality-mode names mirror AgentSkills frontmatter `name`
        // rules (lowercase + hyphens), but legacy YAMLs use underscores.
        // Allow both so already-onboarded operators don't see breakage.
        if !mode
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Ok(json!({
                "status": "error",
                "reason": format!(
                    "Invalid mode '{mode}'. Use letters, digits, hyphens, and underscores only."
                ),
            }));
        }

        // Skills are the only source. Workspace-layer personality skills
        // shadow `paths`-declared extras on collision. The legacy
        // `system/capability_templates/personality/*.yaml` files were
        // deleted in the personality-decommission cleanup.
        let extra_skills_dirs = crate::magician_v2::config_extras::extra_skills_dirs();
        let mut personality_search: Vec<&Path> = vec![ctx.workspace_skills_dir.as_path()];
        for dir in &extra_skills_dirs {
            personality_search.push(dir.as_path());
        }
        let parsed =
            match crate::magician_v2::skills::lookup_personality_mode(&personality_search, &mode) {
                Some(spec) => crate::magician_v2::skills::personality_spec_to_fields(&spec),
                None => {
                    let available = available_modes(ctx).await;
                    return Ok(json!({
                        "status": "error",
                        "reason": format!("Personality preset '{mode}' not found."),
                        "available_modes": available,
                    }));
                },
            };
        let resolved_source = "skill";

        let scoped = ctx
            .agent_definition_store
            .for_scope(&ctx.principal, &ctx.workspace);
        let definition = match scoped.get_definition(&ctx.agent_id).await {
            Ok(Some(record)) => record.definition,
            Ok(None) => {
                return Ok(json!({
                    "status": "error",
                    "reason": format!("Agent '{}' has no definition in this scope.", ctx.agent_id),
                }));
            },
            Err(error) => {
                return Ok(json!({
                    "status": "error",
                    "reason": format!("Failed to load agent definition: {error}"),
                }));
            },
        };

        let mut fields = HashMap::new();
        for (key, value) in &parsed {
            fields.insert(key.clone(), Value::String(value.clone()));
        }
        let record = V3MemoryTierRecord {
            schema_version: "v3".to_string(),
            record_type: "memory_tier".to_string(),
            principal: Some(ctx.principal.clone()),
            workspace: Some(ctx.workspace.clone()),
            agent_id: Some(ctx.agent_id.clone()),
            tier_name: "personality_profile".to_string(),
            tier_scope: crate::magician_v2::agents::memory_tiers::TierScope::Agent,
            goal_id: None,
            last_updated: Utc::now(),
            fields,
        };

        let memory_service = ctx
            .memory_resolver
            .resolve_for_scope(&ctx.principal, &ctx.workspace)
            .context("switch_personality memory resolver failed")?;
        match memory_service
            .save_native_tier_by_name(
                &ctx.agent_id,
                "personality_profile",
                &definition.memory_tiers,
                None,
                &record,
            )
            .await
        {
            Ok(()) => {
                let active_mode = parsed
                    .get("active_mode")
                    .cloned()
                    .unwrap_or_else(|| mode.clone());
                Ok(json!({
                    "status": "ok",
                    "mode": active_mode,
                    "agent_id": ctx.agent_id,
                    "source": resolved_source,
                }))
            },
            Err(error) => Ok(json!({
                "status": "error",
                "reason": format!("Failed to save personality profile: {error}"),
            })),
        }
    }
}

struct RecordChatTeachingFeedbackTool;

#[async_trait]
impl ChatRuntimeTool for RecordChatTeachingFeedbackTool {
    fn name(&self) -> &str {
        "record_chat_teaching_feedback"
    }

    fn description(&self) -> &str {
        "Record explicit user teaching/correction as durable learning input. Use only when the user clearly asks to remember, forget, correct, make reusable, improve a tool, never do something, mark something useful, or mark something wrong. This creates a learning event and routes the resulting candidate through memory/eval/skill-evolution review."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "remember",
                        "forget",
                        "correct",
                        "make_reusable",
                        "improve_tool",
                        "never_do_this",
                        "this_was_useful",
                        "this_was_wrong"
                    ],
                    "description": "The teaching action the user explicitly requested."
                },
                "content": {
                    "type": "string",
                    "description": "The fact, correction, failure, reusable pattern, or improvement request to preserve."
                },
                "correction": {
                    "type": "string",
                    "description": "Correct replacement or expected behavior, when the action is correct or this_was_wrong."
                },
                "target_kind": {
                    "type": "string",
                    "description": "Optional target type, such as memory, skill, tool, task, execution, chat, file, page, or workflow."
                },
                "target_id": {
                    "type": "string",
                    "description": "Optional stable target id, message id, memory key, candidate id, task id, execution id, tool name, or skill name."
                },
                "target_name": {
                    "type": "string",
                    "description": "Optional human-readable target name."
                },
                "target_path": {
                    "type": "string",
                    "description": "Optional file or artifact path used as evidence."
                },
                "target_uri": {
                    "type": "string",
                    "description": "Optional URL or artifact URI used as evidence."
                },
                "target_summary": {
                    "type": "string",
                    "description": "Optional short explanation of what the target is."
                },
                "confidence": {
                    "type": "number",
                    "description": "Optional confidence from 0 to 1. Explicit user memory defaults high."
                },
                "payload": {
                    "type": "object",
                    "description": "Optional structured hints, such as target_tier, memory_type, key, proposed_fix_type, case_kind, or priority."
                }
            },
            "required": ["action", "content"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(action_value) = args.get("action").cloned() else {
            return Ok(json!({
                "status": "error",
                "reason": "record_chat_teaching_feedback requires `action`"
            }));
        };
        let action: LearningTeachingAction = match serde_json::from_value(action_value) {
            Ok(value) => value,
            Err(error) => {
                return Ok(json!({
                    "status": "error",
                    "reason": format!("invalid teaching action: {error}"),
                }));
            },
        };
        let Some(content) = args
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(json!({
                "status": "error",
                "reason": "record_chat_teaching_feedback requires non-empty `content`"
            }));
        };
        let target = teaching_target_from_args(args);
        let request = CreateLearningTeachingFeedbackRequest {
            principal: None,
            workspace: None,
            action,
            content: content.to_string(),
            correction: optional_arg_string(args, "correction"),
            target,
            source_agent_id: Some(ctx.agent_id.clone()),
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: Some(ctx.session_id.clone()),
            confidence: args.get("confidence").and_then(Value::as_f64),
            evidence_refs: Vec::new(),
            payload: args
                .get("payload")
                .filter(|value| value.is_object())
                .cloned()
                .unwrap_or_else(|| json!({})),
        };
        let scope = LearningScope::new(ctx.principal.clone(), ctx.workspace.clone());
        let store = LearningStore::new(ctx.workspace_layout.clone());
        let response =
            record_teaching_feedback(&store, &ctx.workspace_layout, scope, request).await?;
        Ok(json!({
            "status": "ok",
            "event_id": response.event.id,
            "candidate_ids": response.candidates.iter().map(|candidate| candidate.id.clone()).collect::<Vec<_>>(),
            "durable_change_count": response.durable_change_count,
            "review_required_count": response.review_required_count,
            "route_outcomes": response.route_outcomes,
        }))
    }
}

fn teaching_target_from_args(args: &Value) -> Option<LearningTeachingTarget> {
    let target = LearningTeachingTarget {
        kind: optional_arg_string(args, "target_kind"),
        id: optional_arg_string(args, "target_id"),
        name: optional_arg_string(args, "target_name"),
        path: optional_arg_string(args, "target_path"),
        uri: optional_arg_string(args, "target_uri"),
        summary: optional_arg_string(args, "target_summary"),
    };
    if target.kind.is_none()
        && target.id.is_none()
        && target.name.is_none()
        && target.path.is_none()
        && target.uri.is_none()
        && target.summary.is_none()
    {
        None
    } else {
        Some(target)
    }
}

fn optional_arg_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Build the dynamic enrichment block appended to `activate_skill`'s
/// description. Lists the procedure playbooks the agent is allowed to
/// activate (from its `tools:` allowlist after the standard
/// literal→kebab→`tool_schema.yaml::name` bridge) and surfaces the
/// currently-active skill if any. Mirrors `render_personality_descriptor_block`'s
/// pattern so the chat outer-loop builder applies both enrichments
/// side-by-side.
pub fn render_activate_skill_descriptor_block(
    catalog: &[(String, String)],
    active: Option<&str>,
) -> String {
    let mut block = String::new();
    if let Some(name) = active {
        block.push_str(&format!(
            "Currently active: `{name}` — its body is re-injected into the system prompt every turn until you call `deactivate_skill` or activate a different one.\n\n"
        ));
    }
    if catalog.is_empty() {
        block.push_str(
            "Available playbooks: (none — the agent's `tools:` list doesn't reference any installed procedure skill yet).",
        );
        return block;
    }
    block.push_str("Available playbooks (pass exact `name` to activate):\n");
    for (name, description) in catalog {
        let summary = summarize_skill_description(description);
        block.push_str(&format!("- `{name}` — {summary}\n"));
    }
    block.trim_end().to_string()
}

fn summarize_skill_description(description: &str) -> String {
    let first_line = description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string();
    const MAX: usize = 200;
    if first_line.chars().count() <= MAX {
        return first_line;
    }
    let truncated: String = first_line.chars().take(MAX).collect();
    format!("{}…", truncated.trim_end())
}

/// Provenance of a personality preset — useful for rendering source
/// labels in the LLM-visible tool description and in switch responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonalitySource {
    /// Per-scope `<scope>/skills/` install.
    Workspace,
    /// `paths`-declared extras (`tool-runtime-config.yaml :: registry.paths`).
    Extras,
}

impl PersonalitySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PersonalitySource::Workspace => "workspace",
            PersonalitySource::Extras => "extras",
        }
    }
}

/// Available personality modes from installed personality-mode skills.
/// Sorted. Used by the `switch_personality(mode="list")` response and
/// by error responses to surface the valid choices alongside the "not
/// found" message.
async fn available_modes(ctx: &ChatRuntimeToolContext) -> Vec<String> {
    let extra_skills_dirs = crate::magician_v2::config_extras::extra_skills_dirs();
    let mut search: Vec<&Path> = vec![ctx.workspace_skills_dir.as_path()];
    for dir in &extra_skills_dirs {
        search.push(dir.as_path());
    }
    let mut out: Vec<String> = crate::magician_v2::skills::list_personality_mode_names(&search);
    out.sort();
    out
}

/// Scan installed personality-mode skills (workspace layer first, then
/// `paths`-declared extras) and return `(mode_name, summary, source)`
/// triples sorted by name. Workspace skills shadow same-named extras.
/// Used to inject a discoverable preset list into the `switch_personality`
/// tool description at render time so the LLM can pick a preset on first
/// try without a `mode="list"` round-trip.
pub async fn read_personality_summaries(
    workspace_skills_dir: &PathBuf,
) -> Vec<(String, String, PersonalitySource)> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(String, String, PersonalitySource)> = Vec::new();
    let extra_dirs = crate::magician_v2::config_extras::extra_skills_dirs();
    let workspace_entry = (workspace_skills_dir.clone(), PersonalitySource::Workspace);
    let extras_entries: Vec<(PathBuf, PersonalitySource)> = extra_dirs
        .into_iter()
        .map(|d| (d, PersonalitySource::Extras))
        .collect();
    let iter = std::iter::once(workspace_entry).chain(extras_entries);
    for (skills_dir, source) in iter {
        for (name, summary) in read_personality_summaries_from_skills_dir(&skills_dir) {
            if seen.insert(name.clone()) {
                out.push((name, summary, source));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn read_personality_summaries_from_skills_dir(skills_dir: &PathBuf) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let manifests =
        match crate::magician_v2::skills::SkillLoader::new(vec![skills_dir.clone()]).discover() {
            Ok(m) => m,
            Err(_) => return out,
        };
    for m in manifests {
        if m.inferred_kind() != crate::magician_v2::skills::InferredKind::PersonalityMode {
            continue;
        }
        // Description is required and capped at 1024 chars per AgentSkills spec —
        // it doubles as the preset summary for the LLM tool description.
        let summary = if m.description.is_empty() {
            String::from("(no description)")
        } else {
            m.description.clone()
        };
        out.push((m.name.clone(), summary));
    }
    out
}

/// Render the `(name, summary, source)` triples as a markdown bullet
/// list suitable for appending to the `switch_personality` tool
/// description. Workspace-private presets are tagged so the LLM (and a
/// human reading the prompt) can see which presets are private to this
/// scope vs. shared across workspaces.
pub fn render_personality_descriptor_block(
    summaries: &[(String, String, PersonalitySource)],
) -> String {
    if summaries.is_empty() {
        return String::from("(no personality presets available)");
    }
    let mut s = String::from("Available presets:\n");
    for (name, summary, source) in summaries {
        let tag = match source {
            PersonalitySource::Workspace => " *(workspace)*",
            PersonalitySource::Extras => " *(extras)*",
        };
        s.push_str(&format!("- `{name}`{tag}: {summary}\n"));
    }
    s.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Task introspection (chat-surface only)
// ---------------------------------------------------------------------------

/// `GetTaskDetailsForChatTool` is chat-runtime-only — it returns task metadata
/// alongside chat-coupled previews (download URLs, projected output
/// blocks, continuation-pack indexes) that only make sense inside a
/// live chat session. Autonomous agents reach the minimal compiled
/// `get_task_details` pack instead (basic metadata only, no previews).
struct GetTaskDetailsForChatTool;

#[async_trait]
impl ChatRuntimeTool for GetTaskDetailsForChatTool {
    fn name(&self) -> &str {
        "get_task_details_for_chat"
    }

    fn description(&self) -> &str {
        "Fetch a single task's manifest, state, final user summary, task outputs, execution artifacts, downloadable paths, and bounded text previews for result-bearing files — including chat-projected output cards and continuation-pack indexes. Use when the user asks 'what was that task about?', 'what was the result?', 'show me the artifact', or before refining / updating it. Prefer primary_user_summary for user-facing answers and continuation_context_previews for follow-up work; cite output/artifact download_url when the full file is needed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Task id to fetch details for."
                }
            },
            "required": ["task_id"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Counterpart(&["get_task_details"])
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let Some(service) = ctx.artifact_v2_service.as_ref() else {
            return Ok(json!({
                "status": "error",
                "reason": "ArtifactV2Service is not configured for the chat-runtime tool surface",
            }));
        };
        let session = ctx
            .chat_store
            .get_session(&ctx.session_id)
            .await
            .context("get_task_details session lookup failed")?;
        let session = session.unwrap_or_else(|| super::models::ChatSession {
            internal_voice: None,
            id: ctx.session_id.clone(),
            principal: ctx.principal.clone(),
            workspace: ctx.workspace.clone(),
            agent_id: ctx.agent_id.clone(),
            ui_thread_id: crate::magician_v2::storage::task_models::default_ui_thread_id(),
            title: None,
            origin_channel: super::models::ChatChannel::web(),
            status: ChatSessionStatus::Active,
            history_lane: crate::magician_v2::history::HistoryLane::Personal,
            is_default_session: false,
            created_at: 0,
            updated_at: 0,
        });
        Ok(super::service::build_get_task_details_response(service.as_ref(), &session, args).await)
    }
}

/// `SubscribeToTaskForChatTool` attaches the chat session to a task's
/// live event stream (fan-out + progress subscription). Single-slot
/// per chat session; auto-detaches on terminal status. The actual
/// dispatch reaches deeply into `ChatService` internals
/// (`current_tailed_task_id` slot, `event_broadcaster`, persistence,
/// termination watcher), so this tool is short-circuited in
/// `ChatService::dispatch_chat_tool_call` and routed directly to
/// `dispatch_subscribe_to_task` — the trait's `dispatch(args, ctx)`
/// here is unreachable in normal operation. The struct still lives in
/// the chat-runtime tools list so the LLM sees the proper tool spec
/// (name + description + parameters_schema) alongside the rest of the
/// chat-runtime surface.
struct SubscribeToTaskForChatTool;

#[async_trait]
impl ChatRuntimeTool for SubscribeToTaskForChatTool {
    fn name(&self) -> &str {
        "subscribe_to_task_for_chat"
    }

    fn description(&self) -> &str {
        "Attach the chat session to a task's live event stream so the chat surface tails its activity (tool calls, LLM turns, reasoning, HITL pauses, output files) and renders a TaskStatusUpdate card that updates in place. Use when the user asks about an existing task's progress and wants to watch it. Single-slot per chat session — subscribing to a new task automatically detaches from the prior one; auto-detaches on terminal status. Do NOT call for tasks you just created with create_task (auto-subscribed) or for terminal tasks (use get_task_details_for_chat for a status snapshot)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Task id to subscribe to."
                }
            },
            "required": ["task_id"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, _args: &Value, _ctx: &ChatRuntimeToolContext) -> Result<Value> {
        // Unreachable in normal operation — dispatch_chat_tool_call
        // short-circuits this tool by name and routes directly to
        // `ChatService::dispatch_subscribe_to_task` because the work
        // touches per-session state + several ChatService methods
        // that aren't reachable from `ChatRuntimeToolContext`.
        Ok(json!({
            "status": "error",
            "reason": "subscribe_to_task_for_chat must be dispatched via ChatService::dispatch_chat_tool_call — trait-path dispatch is not wired.",
        }))
    }
}

/// `DescribeAgentsForChatTool` is chat-runtime-only — autonomous agents
/// already receive delegation_target_summaries baked into their
/// `delegate_to_agent` prompt block, so a separate describe call from
/// autonomous mode would just re-fetch information already in context.
/// The chat surface uses it for user-facing introspection ("who are
/// the agents?", "what can the executive-assistant do?"). Dispatch
/// reaches `ChatService::delegation_target_summaries` +
/// `capability_registry`, so it short-circuits via
/// `dispatch_chat_tool_call` (same pattern as
/// `subscribe_to_task_for_chat`).
struct DescribeAgentsForChatTool;

#[async_trait]
impl ChatRuntimeTool for DescribeAgentsForChatTool {
    fn name(&self) -> &str {
        "describe_agents_for_chat"
    }

    fn description(&self) -> &str {
        "Fetch each named agent's name, description, and effective tool catalog (denied/excluded filtered out). Use for user-facing introspection — 'what agents do we have?', 'what can the executive-assistant do?', or before recommending a specialist to the user. Pass `agent_ids` as a non-empty array of agent ids. For runtime delegate selection from autonomous mode, the delegate_to_agent prompt already includes summaries; this tool is for chat surfaces only."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Non-empty array of agent ids to describe."
                }
            },
            "required": ["agent_ids"]
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Counterpart(&["get_agent_details"])
    }

    async fn dispatch(&self, _args: &Value, _ctx: &ChatRuntimeToolContext) -> Result<Value> {
        // Unreachable in normal operation — see SubscribeToTaskForChatTool.
        Ok(json!({
            "status": "error",
            "reason": "describe_agents_for_chat must be dispatched via ChatService::dispatch_chat_tool_call — trait-path dispatch is not wired.",
        }))
    }
}

/// `ListToolsForChatTool` is chat-runtime-only — autonomous agents
/// already see their full direct + delegate-reachable tool catalog
/// rendered into the system prompt. Calling this from autonomous mode
/// would enumerate what the LLM already has in context. Chat uses it
/// for user-facing introspection ("what tools do you have?", "can
/// the personal-assistant make a presentation?"). Dispatch reaches
/// `ChatService::load_agent_definition`,
/// `resolve_delegation_targets`, `delegation_target_summaries`, and
/// the `capability_registry`, so it short-circuits via
/// `dispatch_chat_tool_call`.
struct ListToolsForChatTool;

#[async_trait]
impl ChatRuntimeTool for ListToolsForChatTool {
    fn name(&self) -> &str {
        "list_tools_for_chat"
    }

    fn description(&self) -> &str {
        "List every tool the active agent can reach from the current authorization snapshot. The response separates direct, runtime, structural, deferred, and via-delegation grants and includes the snapshot id for the active agent. Delegate entries carry `supported_by` / `supported_by_name`. Use for user-facing introspection — 'what tools do you have?', 'can the personal-assistant make a presentation?'. Optional `agent_id` overrides whose delegate catalog to describe; omitted = current chat agent."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Optional agent id whose tool catalog to enumerate. Omit to use the current chat agent."
                }
            }
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, _args: &Value, _ctx: &ChatRuntimeToolContext) -> Result<Value> {
        // Unreachable in normal operation — see SubscribeToTaskForChatTool.
        Ok(json!({
            "status": "error",
            "reason": "list_tools_for_chat must be dispatched via ChatService::dispatch_chat_tool_call — trait-path dispatch is not wired.",
        }))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod activate_skill_tests {
    use super::*;

    #[test]
    fn descriptor_block_lists_available_playbooks_and_active_tag() {
        let catalog = vec![
            (
                "email-etiquette".to_string(),
                "How to draft polished email messages with tone discipline.".to_string(),
            ),
            (
                "meeting-prep-brief-format".to_string(),
                "Briefing structure for a 1:1 or external meeting.".to_string(),
            ),
        ];
        let out = render_activate_skill_descriptor_block(&catalog, Some("email-etiquette"));
        assert!(out.contains("Currently active: `email-etiquette`"));
        assert!(out.contains("- `email-etiquette` — How to draft polished email"));
        assert!(out.contains("- `meeting-prep-brief-format` — Briefing structure"));
    }

    #[test]
    fn descriptor_block_handles_empty_catalog() {
        let out = render_activate_skill_descriptor_block(&[], None);
        assert!(out.contains("Available playbooks: (none"));
    }

    #[test]
    fn descriptor_block_omits_active_tag_when_none() {
        let catalog = vec![("alpha".to_string(), "First".to_string())];
        let out = render_activate_skill_descriptor_block(&catalog, None);
        assert!(!out.contains("Currently active"));
        assert!(out.contains("- `alpha` — First"));
    }

    #[test]
    fn summarize_skill_description_takes_first_nonempty_line() {
        let desc = "\nFirst real line\nSecond line\n";
        assert_eq!(summarize_skill_description(desc), "First real line");
    }

    #[test]
    fn summarize_skill_description_truncates_long_lines() {
        let long: String = "x".repeat(400);
        let out = summarize_skill_description(&long);
        // Ellipsis marker indicates truncation happened.
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 201);
    }
}

#[cfg(test)]
mod plane_posture_tests {
    use super::*;
    use crate::magician_v2::execution::compiled_providers::default_compiled_handler_registry;
    use crate::magician_v2::execution::plane::catalog::builtin_hot_names;
    use crate::magician_v2::execution::plane::chat_turn::chat_harness_floor_rejects;
    use crate::magician_v2::execution::plane::grant::PlaneCatalogProfile;

    /// A counterpart the plane cannot lower, or does not advertise hot for a
    /// spawned mouth, would be refused at the plane door on every swapped
    /// turn ("is not available on the plane"). A chat-runtime tool is always
    /// on for the native mouth, so its counterpart must be hot, not merely
    /// loadable. Catch it here so a rename or a hot-list edit cannot orphan a
    /// posture.
    #[test]
    fn every_counterpart_is_a_compiled_tool_the_plane_can_lower() {
        let registry = default_compiled_handler_registry();
        let known: std::collections::HashSet<&str> = registry.names().collect();
        let hot: std::collections::HashSet<&str> =
            builtin_hot_names(PlaneCatalogProfile::SpawnedBare)
                .into_iter()
                .collect();
        for tool in build_chat_runtime_tools() {
            let PlanePosture::Counterpart(names) = tool.plane_posture() else {
                continue;
            };
            assert!(
                !names.is_empty(),
                "{} declares an empty counterpart; declare Bridged or MouthOnly instead",
                tool.name()
            );
            for name in names {
                assert!(
                    known.contains(name),
                    "{} -> {name} is not a compiled tool the plane can lower",
                    tool.name()
                );
                assert!(
                    hot.contains(name),
                    "{} -> {name} is not hot for a spawned mouth; the door would refuse it",
                    tool.name()
                );
                assert!(
                    !chat_harness_floor_rejects(name),
                    "{} -> {name} is rejected by the chat-harness floor",
                    tool.name()
                );
            }
        }
    }

    #[test]
    fn task_and_agent_reads_have_plane_counterparts() {
        let index = plane_posture_index(&build_chat_runtime_tools());
        assert_eq!(index.len(), build_chat_runtime_tools().len());
        assert_eq!(
            index["get_task_details_for_chat"],
            PlanePosture::Counterpart(&["get_task_details"])
        );
        assert_eq!(
            index["describe_agents_for_chat"],
            PlanePosture::Counterpart(&["get_agent_details"])
        );
        assert_eq!(index["subscribe_to_task_for_chat"], PlanePosture::Bridged);
        assert_eq!(index["list_tools_for_chat"], PlanePosture::Bridged);
        // Not registered by the builder (it rides the compiled pack rail),
        // so its posture never enters the index; pinned here so a future
        // registration is a deliberate change to the bridged list below.
        assert!(!index.contains_key("switch_personality"));
        assert_eq!(SwitchPersonalityTool.plane_posture(), PlanePosture::Bridged);
    }

    /// Every registered runtime tool that is not a plane-executed twin is
    /// bridged: a swapped mouth reaches it through the chat service's own
    /// dispatcher. `MouthOnly` stays in the enum for a tool that must be
    /// withheld, but no registered tool claims it today; a new tool that
    /// does, or a tool dropped from the bridged list, changes this pin.
    #[test]
    fn bridged_set_is_every_registered_non_counterpart_tool() {
        let index = plane_posture_index(&build_chat_runtime_tools());
        let mouth_only = index
            .values()
            .filter(|posture| **posture == PlanePosture::MouthOnly)
            .count();
        assert_eq!(
            mouth_only, 0,
            "no registered runtime tool is MouthOnly today"
        );

        let mut counterparts: Vec<&str> = index
            .iter()
            .filter(|(_, posture)| matches!(posture, PlanePosture::Counterpart(_)))
            .map(|(name, _)| name.as_str())
            .collect();
        counterparts.sort_unstable();
        assert_eq!(
            counterparts,
            vec!["describe_agents_for_chat", "get_task_details_for_chat"]
        );

        let mut bridged: Vec<&str> = index
            .iter()
            .filter(|(_, posture)| **posture == PlanePosture::Bridged)
            .map(|(name, _)| name.as_str())
            .collect();
        bridged.sort_unstable();
        assert_eq!(
            bridged,
            vec![
                "archive_chat_session",
                "archive_chat_thread",
                "check_for_copilot_user_action",
                "complete_tutor_run",
                "create_chat_thread",
                "delete_chat_session",
                "delete_chat_thread",
                "get_current_chat_context",
                "get_tutor_run",
                "list_chat_sessions",
                "list_chat_threads",
                "list_tools_for_chat",
                "propose_tutor_step",
                "read_result",
                "record_chat_teaching_feedback",
                "record_tutor_created_object",
                "record_tutor_step_failure",
                "start_tutor_run",
                "subscribe_to_task_for_chat",
                "switch_chat_session",
                "switch_chat_thread",
                "unarchive_chat_session",
                "wait_for_copilot_user_action",
            ]
        );
        assert_eq!(bridged.len() + counterparts.len(), index.len());
    }
}
