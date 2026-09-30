//! Native tool catalog builder for execution-native tool calls.
//!
//! This module is the **single source of truth** for which tools the model sees
//! as native provider tools during execution. It builds `NativeExecutionTool`
//! specifications for control tools, lane-shaped built-in tools, and pack
//! capability tools.

use serde_json::{json, Map, Value};

use super::native_types::NativeExecutionTool;

const RELAY_SOURCE_AGENT_ID: &str = "harness-sre";
const RELAY_DELEGATION_TARGETS_MAX_ITEMS: usize = 1;

// =============================================================================
// Constants
// =============================================================================

/// Built-in lane tool names. Any pack-defined capability whose name
/// matches one of these is skipped during catalog assembly to avoid
/// collisions.
///
/// Phase 0.8c-11: `file` / `http` / `bash` migrated to compiled packs
/// (`files` / `http` / `shell` respectively). The pack names are not
/// in this list — they're meant to flow through the
/// `direct_capabilities` path. `browser` and `duckdb` are inner-loop
/// packs and also flow that way. Empty list today; kept as the
/// collision-detection mechanism for any future native lanes.
pub const BUILTIN_LANE_NAMES: &[&str] = &[];

// =============================================================================
// CatalogBuildContext — decoupled input for catalog assembly
// =============================================================================

/// Input context for building the native tool catalog.
/// Decoupled from `AgenticContext` to keep the module testable.
#[derive(Debug, Clone)]
pub struct CatalogBuildContext {
    /// Which built-in action types are allowed (`None` = all allowed).
    pub allowed_action_types: Option<Vec<String>>,
    /// Whether provisioned secret sidecars are enabled.
    pub credentials_enabled: bool,
    /// Whether delegation targets are available.
    pub has_delegation_targets: bool,
    /// Direct pack capabilities (name, description, parameter schema).
    /// Only includes tools where `providing_agent_id` is `None` (not delegate-owned).
    pub direct_capabilities: Vec<(String, String, Value)>,
    /// BUG-4: when `true`, the catalog is being built for the chat
    /// inline outer loop (`process_chat_inline_turn`). Sole remaining
    /// job after Phase 0.8c-11: **schema stripping** — removes the
    /// autonomous-loop common metadata fields (`task_state_action`,
    /// `request_hover_discovery`, `step_completed`, etc.) from each
    /// tool's parameter schema since chat doesn't consume them, and
    /// the LLM was paying a per-call schema-fill tax for them.
    ///
    /// All named tools (memory, introspection, task ops, filesystem,
    /// HTTP, shell, …) reach both chat and autonomous identically
    /// through the universal-pack rail (compiled-handler path) —
    /// there's no chat-vs-autonomous catalog difference anymore.
    /// Strip is purely a per-tool-schema concern.
    #[doc(alias = "is_chat_outer_loop")]
    pub is_chat_mode: bool,
    /// Procedure skills the agent has allowlisted via its `tools:` block
    /// (resolved through `agent_procedure_skill_catalog`). Drives the
    /// dynamic `## AVAILABLE PROCEDURE SKILLS` user-prompt block
    /// rendered in `decision.rs` so the LLM knows which slugs are
    /// valid arguments to `activate_skill`.
    ///
    /// Each entry is `(name, description)`. `name` is the kebab-case slug
    /// the LLM passes to `activate_skill`; `description` is the SKILL.md
    /// frontmatter `description` field (one-line summary the LLM uses to
    /// pick). When empty, the `## AVAILABLE PROCEDURE SKILLS` prompt
    /// section is omitted entirely.
    pub available_procedure_skills: Vec<(String, String)>,
    /// Whole-tool exclusions/denies applied after structural controls are
    /// assembled, so controls cannot bypass pack-level policy.
    pub denied_tool_names: Vec<String>,
    /// Execution-agent policy propagated by
    /// `native_integration::build_catalog_context`. It carries the source agent
    /// identity needed for source-specific schema constraints and, for VibeDev
    /// coding coordinators, the optional direct-grant gate used by the flat
    /// catalog. A policy without a grant gate allows every tool, preserving the
    /// existing catalog for Relay and all unrelated agents.
    pub delegate_only_grant_gate: Option<CatalogAgentPolicy>,
}

/// Source-agent policy needed while assembling provider-visible tool schemas.
///
/// `granted_capability_names` retains the existing VibeDev coordinator gate.
/// Keeping source identity in the same optional carrier avoids adding a new
/// required field to `CatalogBuildContext` literals owned by chat and flat-loop
/// callers.
#[derive(Debug, Clone)]
pub struct CatalogAgentPolicy {
    source_agent_id: Option<String>,
    granted_capability_names: Option<std::collections::HashSet<String>>,
}

impl CatalogAgentPolicy {
    pub fn from_execution(
        source_agent_id: Option<&str>,
        granted_capability_names: Option<std::collections::HashSet<String>>,
    ) -> Option<Self> {
        if source_agent_id.is_none() && granted_capability_names.is_none() {
            return None;
        }
        Some(Self {
            source_agent_id: source_agent_id.map(str::to_string),
            granted_capability_names,
        })
    }

    /// Preserve the previous grant-set interface consumed by the flat catalog.
    /// A source identity without a grant gate is deliberately permissive.
    pub fn contains(&self, capability_name: &str) -> bool {
        self.granted_capability_names
            .as_ref()
            .is_none_or(|granted| granted.contains(capability_name))
    }

    fn delegation_targets_max_items(&self) -> Option<usize> {
        (self.source_agent_id.as_deref() == Some(RELAY_SOURCE_AGENT_ID))
            .then_some(RELAY_DELEGATION_TARGETS_MAX_ITEMS)
    }
}

impl CatalogBuildContext {
    pub fn delegation_targets_max_items(&self) -> Option<usize> {
        self.delegate_only_grant_gate
            .as_ref()
            .and_then(CatalogAgentPolicy::delegation_targets_max_items)
    }
}

// =============================================================================
// Common metadata helper
// =============================================================================

/// Returns the common metadata properties that every execution tool must
/// include in its JSON Schema. Sparse execution signals share one compact
/// `decision_metadata` sidecar; the complete field contract is rendered once
/// in the decision prompt instead of repeated across every tool. The rationale
/// schema intentionally has no repeated description or provider-side length
/// keyword: runtime lowering applies the authoritative bound without paying
/// those tokens once per tool.
pub(super) const MAX_DECISION_RATIONALE_CHARS: usize = 240;

pub(super) const DECISION_METADATA_FIELD: &str = "decision_metadata";

/// Historical flat signal names accepted indefinitely by lowering. They are
/// deliberately absent from new provider-visible schemas, but retaining the
/// decoder keeps persisted tool calls and older provider responses replayable.
pub(super) const LEGACY_DECISION_METADATA_FIELDS: &[&str] = &[
    "request_hover_discovery",
    "request_vision",
    "vision_reason",
    "step_completed",
    "step_failed",
    "needs_plan_revision",
];

pub fn common_metadata_properties() -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("thinking".to_string(), json!({"type": "string"}));
    map.insert(
        DECISION_METADATA_FIELD.to_string(),
        decision_metadata_schema(),
    );
    map.insert("task_state_action".to_string(), task_state_action_schema());
    map
}

fn decision_metadata_schema() -> Value {
    // The complete signal contract is rendered once in the decision prompt.
    // Lowering validates the outer shape and supports legacy flat fields.
    json!({"type": "object"})
}

fn task_state_action_schema() -> Value {
    // The complete mutation contract is rendered once in the decision prompt.
    // Keeping this open object in each tool schema preserves inline atomic
    // updates without repeating that contract across the whole catalog.
    json!({"type": "object"})
}

/// Merge common metadata properties into an existing properties map.
fn merge_common_metadata(props: &mut Map<String, Value>) {
    for (k, v) in common_metadata_properties() {
        props.insert(k, v);
    }
}

/// Build a JSON Schema `parameters` object from properties, required fields,
/// and automatically merged common metadata.
fn build_parameters_from_names(mut properties: Map<String, Value>, required: Vec<String>) -> Value {
    merge_common_metadata(&mut properties);
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn build_parameters(properties: Map<String, Value>, required: Vec<&str>) -> Value {
    build_parameters_from_names(
        properties,
        required.into_iter().map(str::to_string).collect(),
    )
}

/// BUG-4: strip the autonomous-loop common metadata properties (and the
/// `task_state_action` entry) from a tool schema. Chat doesn't
/// consume these fields, and forcing the LLM to emit them on every call
/// burns tokens. Mutates the schema in place.
fn strip_common_metadata_for_chat(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    let mut common_keys: Vec<&'static str> =
        vec!["thinking", DECISION_METADATA_FIELD, "task_state_action"];
    // Defensive compatibility: custom/older schemas can still expose the
    // historical flat fields. Chat consumes none of these signals.
    common_keys.extend_from_slice(LEGACY_DECISION_METADATA_FIELDS);
    if let Some(Value::Object(props)) = obj.get_mut("properties") {
        for key in &common_keys {
            props.remove(*key);
        }
    }
    if let Some(Value::Array(required)) = obj.get_mut("required") {
        required.retain(|value| match value.as_str() {
            Some(name) => !common_keys.iter().any(|key| key == &name),
            None => true,
        });
        if required.is_empty() {
            obj.remove("required");
        }
    }
    common_keys.clear();
}

// =============================================================================
// Control tool builders (is_control_tool: true)
// =============================================================================

/// Build the `need_user_input` control tool.
pub fn build_need_user_input_tool() -> NativeExecutionTool {
    let mut props = Map::new();
    props.insert(
        "question".to_string(),
        json!({"type": "string", "description": "Question to ask the user"}),
    );
    props.insert(
        "input_type".to_string(),
        json!({
            "type": "string",
            "description": "Type of input expected. Use `password` for a password or API key and `otp` for a one-time verification code (SMS, email, authenticator): both are masked, held in secure custody and returned to you only as a placeholder reference, never as the value. For one-time browser login credentials use browser__secure_prompt_fill; never ask for a secret as `text`.",
            "enum": ["text", "password", "otp", "choice", "multi_choice", "confirmation", "external_action", "file_path", "guidance", "form"]
        }),
    );
    props.insert(
        "hint".to_string(),
        json!({"type": "string", "description": "Hint for the user to help answer"}),
    );
    props.insert(
        "questions".to_string(),
        json!({
            "type": "array",
            "description": "When asking several questions in one pause (input_type=form). Cap 3.",
            "items": {
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "prompt": {"type": "string"},
                    "input_type": {"type": "string", "enum": ["text", "password", "otp", "choice", "multi_choice"]},
                    "options": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "label": {"type": "string"}
                            }
                        }
                    }
                },
                "required": ["id", "prompt"]
            }
        }),
    );
    props.insert(
        "options".to_string(),
        json!({
            "type": "array",
            "description": "Options for choice/multi_choice input types",
            "items": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Option identifier"},
                    "label": {"type": "string", "description": "Display label"},
                    "description": {"type": "string", "description": "Optional description"}
                },
                "required": ["id", "label"]
            }
        }),
    );

    NativeExecutionTool {
        name: "need_user_input".to_string(),
        description: "Request input from the user before continuing.".to_string(),
        parameters: build_parameters(props, vec!["question", "input_type"]),
        is_control_tool: true,
    }
}

/// Build the `yield` control tool — unified terminal outcome report
/// (see `docs/plans/2026-05-27-yield-decision-migration.md`).
///
/// `yield` is for "I am done with this turn" outcome reports —
/// completed, partial, or blocked. The orchestrator's `dispose_yield`
/// computes the disposition (success / partial-success / failed /
/// retry-transient) from the structured fields; the LLM doesn't
/// self-grade.
///
/// **Use `need_user_input` instead** when the next action is "ask the
/// user a question and continue." Asking is a separate primitive
/// (different lifecycle — the conversation continues rather than
/// terminates; different UI affordance). `yield` is strictly terminal.
///
/// The unified LLM terminal — replaces the legacy `goal_reached` /
/// `cannot_proceed` LLM tools (deleted in Phase 0.8c-12). The
/// orchestrator runs `dispose_yield` on the structured payload and
/// maps it to `AgenticOutcome::Success` (Completed disposition),
/// `Success` with `partial_findings.md` (PartialSuccess), or
/// `Failed` (Failed / RetryTransient).
pub fn build_yield_tool() -> NativeExecutionTool {
    let mut props = Map::new();
    props.insert(
        "summary".to_string(),
        json!({
            "type": "string",
            "description": "The answer the reader receives. Always required. State the results themselves — every value, identifier, quotation and count the goal asked for, verbatim — then, briefly, what was done. The reader sees only this and any artifacts; a summary that says the values were written or returned without repeating them has not answered."
        }),
    );
    props.insert(
        "completed".to_string(),
        json!({
            "type": "array",
            "description": "Concrete things finished. Include already-satisfied final states here. Empty when nothing completed.",
            "items": {"type": "string"}
        }),
    );
    props.insert(
        "open".to_string(),
        json!({
            "type": "array",
            "description": "Concrete things still undone. Bullet list — one item per remaining requirement. Empty when the task is fully done.",
            "items": {"type": "string"}
        }),
    );
    props.insert(
        "blockers".to_string(),
        json!({
            "type": "array",
            "description": "Concrete blockers preventing progress on `open` items. Do not use data_missing when the desired final state is already verified.",
            "items": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["auth", "data_missing", "permission", "transient", "external", "other"],
                        "description": "Blocker class: `transient` (retry-safe), `auth` (re-auth needed), `permission` (HTTP 403 / scope mismatch), `data_missing` (no matching record), `external` (upstream error we can't fix), `other` (catch-all)."
                    },
                    "description": {"type": "string", "description": "What specifically blocked — include error codes and identifiers where useful."}
                },
                "required": ["kind", "description"]
            }
        }),
    );
    props.insert(
        "artifacts".to_string(),
        json!({
            "type": "array",
            "description": "Output artifacts this run produced (text, JSON, file paths). If you produced a written deliverable (script, report, draft, analysis, copy) attach its FULL text here as a {name, content_type, data} item — text that lives only inside a prior tool argument (e.g. an image-generation prompt) is NOT a captured deliverable and will be lost. Empty only when nothing durable was produced.",
            "items": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "content_type": {"type": "string"},
                    "data": {"type": "string"},
                    "artifact_type": {"type": "string"}
                },
                "required": ["name", "content_type", "data"]
            }
        }),
    );
    props.insert(
        "next_step_hint".to_string(),
        json!({
            "type": "string",
            "description": "Optional advisory — what you think should happen next. Pure information; orchestrator decides whether to act on it."
        }),
    );
    props.insert(
        "self_classification".to_string(),
        json!({
            "type": "string",
            "enum": ["done", "partial", "blocked"],
            "description": "Your own assessment of the outcome class. Advisory only — the orchestrator computes the real disposition from the structured fields above."
        }),
    );
    props.insert(
        "keep_browser_window_open".to_string(),
        json!({
            "type": "boolean",
            "description": "Browser tasks only. Set true to HAND THE VISIBLE CHROME WINDOW OFF TO THE USER on terminal yield: the runtime detaches its CDP connection and exits the agent-browser daemon, leaving Chromium open as a normal user-owned window for inspection (e.g. SoTA Pass/Fail evidence, a generated form, search results to read). Default false closes the window. Use this when a human should look at the final page; do NOT use it just to 'be safe'."
        }),
    );
    props.insert(
        "keep_browser_cdp_connection_alive".to_string(),
        json!({
            "type": "boolean",
            "description": "Browser tasks only. Set true to KEEP THE AGENT'S BROWSER SESSION ALIVE (CDP socket + daemon + Chromium) so a FOLLOW-UP agent execution can reattach to the same authenticated/logged-in session. Default false closes everything. Mutually exclusive with keep_browser_window_open; if both are set this wins. (Legacy name: keep_browser_session_alive.)"
        }),
    );

    NativeExecutionTool {
        name: "yield".to_string(),
        description: "Yield control back to the orchestrator with a structured terminal outcome report — done / partial / blocked. The single terminal primitive: describe what happened in structured fields (`completed`, `open`, `blockers`, `artifacts`) and let the orchestrator classify the disposition. If the observed final state satisfies the goal, yield completed with empty open/blockers. To ASK the user a question, use `need_user_input` instead — that is a separate primitive with a different lifecycle.".to_string(),
        parameters: build_parameters(props, vec!["summary"]),
        is_control_tool: true,
    }
}

/// Build the `delegate_to_agent` control tool.
pub fn build_delegate_to_agent_tool() -> NativeExecutionTool {
    let mut props = Map::new();
    props.insert(
        "delegation_targets".to_string(),
        json!({
            "type": "array",
            "description": "Agents to delegate work to",
            "items": {
                "type": "object",
                "properties": {
                    "target_agent_id": {"type": "string", "description": "ID of the target agent"},
                    "context": {"type": "string", "description": "Context/instructions for the delegate"},
                    "input_artifact_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Artifact IDs to pass as input"
                    },
                    "input_data": {"type": "string", "description": "Inline data to pass"},
                    "tutor_action": {
                        "type": "object",
                        "description": "Optional. Required when delegating a Personal Tutor guided UI action to `mac-operator`. The chat runtime validates this envelope against the active TutorRun before spawning the delegate.",
                        "properties": {
                            "run_id": {
                                "type": "string",
                                "description": "Tutor run id returned by start_tutor_run."
                            },
                            "step_kind": {
                                "type": "string",
                                "enum": ["click", "type_text", "hotkey", "scroll", "verify"],
                                "description": "The macOS UI action or verification step being delegated."
                            },
                            "storyboard_step_id": {
                                "type": "string",
                                "description": "For UI-changing actions, the storyboard_step_id from the immediately preceding successful screen-draw preview for this exact target."
                            },
                            "storyboard_step_label": {
                                "type": "string",
                                "description": "Fallback binding for UI-changing actions when only the preceding screen-draw tutor_step_label/step_label is available."
                            },
                            "target": {
                                "type": "string",
                                "description": "Resolved UI target, e.g. new note button or title field."
                            },
                            "expected_state": {
                                "type": "string",
                                "description": "Visible state expected after the delegated action."
                            },
                            "safety": {
                                "type": "string",
                                "enum": ["visual_only", "reversible_action", "session_owned_destructive", "destructive_requires_confirmation"],
                                "description": "Safety level for the action. UI-changing steps cannot be visual_only."
                            },
                            "observation_evidence": {
                                "type": "string",
                                "description": "What was observed/resolved before delegation, including screen/AX evidence."
                            },
                            "action_instruction": {
                                "type": "string",
                                "description": "Exact action instruction for mac-operator."
                            },
                            "created_object": {
                                "type": "object",
                                "description": "Optional object expected to be created by this action; cleanup must still record success after verification.",
                                "properties": {
                                    "label": {"type": "string"},
                                    "object_type": {"type": "string"},
                                    "evidence": {"type": "string"}
                                },
                                "required": ["label"]
                            }
                        },
                        "required": [
                            "run_id",
                            "step_kind",
                            "target",
                            "expected_state",
                            "safety",
                            "observation_evidence",
                            "action_instruction"
                        ]
                    },
                    "reference_task_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Completed task ids whose outputs/artifacts should seed this delegation. Use this for follow-ups like 'continue from task_...' after calling get_task_details; the child task receives the referenced task's continuation pack and linked artifacts."
                    },
                    "depth": {
                        "type": "string",
                        "enum": ["normal", "deep", "thorough"],
                        "description": "Active-work budget tier for the delegated run: normal 300 s, deep 900 s, thorough 1800 s. Interactive work — a browser, a desktop app, anything driven through a UI — is deep or thorough; normal is for a quick lookup."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Soft active-work budget in seconds, only to EXTEND the depth tier beyond its default; a value below the tier is raised to it. Omit unless the user stated a time limit. When it elapses, the current provider/tool operation may finish, no new work starts, and normal result synthesis continues. This is not a hard cancellation timeout."
                    },
                    "spend_token_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Resource tokens to spend"
                    },
                    "required_capability": {
                        "type": "string",
                        "description": "Optional. The capability this stage needs — a skill/tool name from the target's listed capabilities (e.g. `comic-strip`). Set it for capability-specific stages: the runtime verifies the target agent owns it and, on mismatch, returns the real owner(s) so you can re-issue against an agent that can actually do the work. Omit for general work."
                    },
                    "expected_artifacts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "content_type": {"type": "string"}
                            },
                            "required": ["name"]
                        },
                        "description": "Optional. Named text deliverables this stage MUST produce as written artifacts (e.g. {name: 'comic_script.md', content_type: 'text/markdown'}). Declare them when a stage's job is to author a script / report / draft / analysis whose TEXT must survive into the final deliverable — the runtime then re-runs the child until an artifact with each exact name exists, so the agent cannot bury the text inside another tool's argument (e.g. an image-generation prompt) and finish without it. Omit for stages with no named text deliverable."
                    },
                    "track_as_task": {
                        "type": "boolean",
                        "description": "Set true ONLY when the user asked to track / persist this work as a task in /tasks (e.g. 'create a task for executive-assistant to do X'). Default false: chat delegation is transient/internal: progress still streams to the chat card, successful outputs are preserved, and the backing task is deleted, while failed/cancelled runs archive for debugging. Has no effect outside chat."
                    },
                    "personality_mode": {
                        "type": "string",
                        "description": "Optional ephemeral personality override applied to this delegation only. Pass `current` to make the target adopt the calling agent's currently-active personality_profile (so the caller's voice carries into the artifact). Pass a named preset (e.g. `witty`, `brutal`, `true-friend`) to load that personality-mode skill for this cycle. Omit to let the target use its own persisted personality. The target's saved personality_profile tier is NEVER modified — the override expires when the cycle ends."
                    }
                },
                "required": ["target_agent_id", "context"]
            }
        }),
    );

    NativeExecutionTool {
        name: "delegate_to_agent".to_string(),
        description: "USE ONLY when the work requires a tool or capability YOU do not have. \
First check your own tools list — if you can do the work directly with `browser`, `files`, `shell`, \
`http`, `gmail`, `treasurer`, `internal_data`, or any other tool you already own, call THAT tool. \
Delegating to another agent that has the same tool just spawns a fresh tool session (e.g. a new \
headed Chrome window) and breaks session continuity. When the user explicitly names a tool in the \
request (\"use browser to…\", \"send via gmail…\"), that is a STRONG signal to call it directly, \
NOT to delegate. Each delegation hop adds 5-15s of latency (full LLM round + scope setup + child \
runtime initialization) and creates a new browser/shell/etc. session, abandoning the current one. \
Default chat mode is transient/internal: a backing task is created for execution, progress still \
streams to the chat card, successful outputs are preserved, and the task is deleted; failed/cancelled \
runs archive for debugging. Set per-target `track_as_task: true` ONLY when the user explicitly asked \
for this work to live in /tasks; otherwise prefer `create_task` for explicit tracked work. Set \
per-target `personality_mode` to `current` to carry your own voice into the delegation, or to a \
named preset (e.g. `witty`, `brutal`) for a one-cycle persona switch — the override is ephemeral \
and never touches the target's saved personality_profile. For continuation work, first inspect the \
source task with `get_task_details`, then pass its completed task id in per-target \
`reference_task_ids` so the delegate starts with the prior continuation pack/artifact index instead \
of rediscovering raw task files. \
\
RETURN SEMANTICS — IMPORTANT: this tool returns immediately when the work is **enqueued**, NOT when \
it is complete. The delegated agent may take seconds to minutes to actually finish; output \
synthesis takes another 5–60s after that. The receipt you get back is just confirmation that the \
work was handed off — it carries NO information about success, failure, or any produced output. \
**Do not claim the task succeeded in your reply, do not summarise the outcome, do not quote any \
expected result.** The user sees real-time status, completion, and the synthesized output in the \
chat's activity card; that surface carries the truth, not your reply. A brief acknowledgment \
(\"On it.\" / \"Handed to <agent>.\") is fine; silence is also fine. Reply with the final answer \
ONLY when the user explicitly asks you to wait for the result on this same chat turn.\n\n\
PARALLEL FAN-OUT — `delegation_targets` is an array, not a single target. When a task \
decomposes into N independent sub-questions (e.g. \"answer Q1, Q2, Q3, Q4, Q5\" against the same \
data source; \"summarise these 3 unrelated documents\"; \"check pricing on 4 vendor sites\"), pass \
ALL N as separate entries in a single `delegate_to_agent` call. The runtime spawns the children in \
parallel and resumes the parent when every child terminates; their outputs merge into the parent's \
artifact set. This is dramatically more reliable than running the sub-questions sequentially in one \
agent because each child gets fresh context (no question-N is poisoned by question-1's verbose \
output) and each child gets a full iteration budget. STRONG signal to fan out: the user's request \
lists distinct items joined by AND / commas / numbered list AND each item can be answered without \
the answers to the others. For DEPENDENT stages (\"do A, then B using A's result\") do NOT fan out \
in one call — run ORDERED ROUNDS instead: delegate stage A now; when its child finishes the runtime \
resumes you and surfaces A's results + produced artifact ids under `## RECENT DELEGATION RESULTS`; \
then issue a follow-up `delegate_to_agent` for stage B with A's artifact ids in B's \
`input_artifact_ids` so B builds on A's output rather than redoing it. This is how a deliverable \
that spans capabilities living on different agents (e.g. research → explain → illustrate) gets \
composed end to end. Per-target context strings should be specific \
(\"Answer Q3: <full Q3 text>\") rather than generic (\"one of the questions\").".to_string(),
        parameters: build_parameters(props, vec!["delegation_targets"]),
        is_control_tool: true,
    }
}

/// Apply source-specific constraints to the provider-visible delegation tool.
///
/// Runtime durably enforces Relay's one delegated child total per root. The
/// schema cannot express the historical total, but `maxItems: 1` prevents a
/// single model call from bypassing that boundary with a fan-out array. The
/// runtime remains authoritative for later calls after the root used its slot.
pub fn apply_catalog_context_to_tools(
    tools: &mut [NativeExecutionTool],
    ctx: &CatalogBuildContext,
) {
    let Some(max_items) = ctx.delegation_targets_max_items() else {
        return;
    };
    let Some(tool) = tools
        .iter_mut()
        .find(|tool| tool.name == "delegate_to_agent")
    else {
        return;
    };
    let Some(delegation_targets) = tool
        .parameters
        .pointer_mut("/properties/delegation_targets")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    delegation_targets.insert("minItems".to_string(), json!(1));
    delegation_targets.insert("maxItems".to_string(), json!(max_items));

    if let Some(fan_out_start) = tool.description.find("\n\nPARALLEL FAN-OUT") {
        tool.description.truncate(fan_out_start);
    }
    if !tool.description.contains("RELAY ROOT LIMIT") {
        tool.description.push_str(
            "\n\nRELAY ROOT LIMIT - This Relay root may create at most one delegated child total. Pass exactly one entry in `delegation_targets`; do not attempt another delegation after the root has used its child slot. The runtime durably enforces the total across calls.",
        );
    }
}

/// Build the `handover_to_agent` control tool.
pub fn build_handover_to_agent_tool() -> NativeExecutionTool {
    let mut props = Map::new();
    props.insert(
        "target_agent_id".to_string(),
        json!({"type": "string", "description": "ID of the agent to hand over to"}),
    );
    props.insert(
        "context".to_string(),
        json!({"type": "string", "description": "Context for the handover"}),
    );
    props.insert(
        "preserve_live_execution_context".to_string(),
        json!({"type": "boolean", "description": "Whether to preserve the live execution context"}),
    );
    props.insert(
        "personality_mode".to_string(),
        json!({
            "type": "string",
            "description": "Optional ephemeral personality override applied to this handover only. Pass `current` to make the receiving agent adopt your currently-active personality_profile (so your voice carries into the cycle). Pass a named preset (e.g. `witty`, `brutal`, `true-friend`) to load that personality-mode skill for this cycle. Omit to let the target use its own persisted personality. The target's saved personality_profile tier is NEVER modified — the override expires when the cycle ends."
        }),
    );
    props.insert(
        "reference_task_ids".to_string(),
        json!({
            "type": "array",
            "items": {"type": "string"},
            "description": "Completed task ids whose outputs/artifacts should seed the handed-over task. Use for continuation handovers after get_task_details confirms the source task is completed."
        }),
    );

    NativeExecutionTool {
        name: "handover_to_agent".to_string(),
        description: "USE ONLY for permanent role transfer — when the remaining work genuinely belongs \
to another agent (e.g. a long-running async task migration where the receiving agent owns the domain). \
Do NOT use handover to gain access to a tool: if you need `browser`, `files`, `shell`, `http`, \
`gmail`, etc., call that tool directly. Each handover spawns a fresh execution context for the \
receiving agent including a new tool session (a new headed Chrome window for browser work, a new \
shell, etc.), so it breaks session continuity. When the user explicitly names a tool in the request, \
that is a STRONG signal to call it directly, NOT to hand over. Hand over execution entirely to \
another agent. Set `personality_mode` to `current` to carry your own voice into the handoff, or to a \
named preset (e.g. `witty`) for a one-cycle persona switch — the override is ephemeral and never \
modifies the target's saved personality_profile. For continuation handovers, pass completed source \
task ids in `reference_task_ids` so the receiving agent gets the linked continuation pack/artifact \
index at execution start.".to_string(),
        parameters: build_parameters(
            props,
            vec![
                "target_agent_id",
                "context",
                "preserve_live_execution_context",
            ],
        ),
        is_control_tool: true,
    }
}

/// Build the `orchestrate_pipeline` chat control tool.
///
/// The structural fix for multi-stage chat asks. A single `delegate_to_agent`
/// from a chat turn is fire-and-forget: it fires ONE stage and the turn ends —
/// it cannot run an ordered pipeline (delegate A → read A's result → delegate B
/// with A's output) because a chat turn is not a task-backed loop. This action
/// creates a task owned by the CALLING orchestrator (e.g. personal-assistant)
/// and runs its full agentic loop, where ordered-rounds delegation DOES work
/// (root execution, `owner_stack` empty → the orchestrator can sub-delegate to
/// each specialist in sequence and thread each stage's artifacts into the next
/// via `## RECENT DELEGATION RESULTS`). Chat-only; in a task-backed loop the
/// orchestrator already uses `delegate_to_agent` directly.
pub fn build_orchestrate_pipeline_tool() -> NativeExecutionTool {
    let mut props = Map::new();
    props.insert(
        "goal".to_string(),
        json!({
            "type": "string",
            "description": "The full multi-stage request, verbatim. Pass the WHOLE ask (all stages), not one stage — the orchestration task decomposes it and delegates each stage to the right specialist in order."
        }),
    );
    props.insert(
        "allow_duplicate".to_string(),
        json!({
            "type": "boolean",
            "description": "Leave unset/false normally. The system blocks running a pipeline identical to one already created in THIS conversation (a common symptom of a request carried over from earlier context rather than a fresh ask) and asks you to confirm with the user first. Set true ONLY to deliberately run another, separate pipeline identical to an existing one, and ONLY after the user has explicitly confirmed they want a fresh re-run."
        }),
    );

    NativeExecutionTool {
        name: "orchestrate_pipeline".to_string(),
        description: "USE for a request that needs SEVERAL capabilities living on DIFFERENT agents \
in a dependent ORDER — e.g. research → explain → illustrate, where the research specialist and the \
comic/image specialist are different agents and each stage feeds the next. A single \
`delegate_to_agent` from chat only fires ONE stage and the turn ends, so it cannot pipeline \
multi-stage work; that is exactly why such asks come back with only the first stage done. \
`orchestrate_pipeline` instead creates a task owned by YOU and runs your full orchestration loop: \
it decomposes the ask, delegates each stage to the agent that owns the needed capability (consult \
your delegation roster / `find_agents_for_capability`), threads each stage's produced artifacts into \
the next stage, and aggregates everything into one deliverable. Pass the COMPLETE request as `goal`. \
RETURN SEMANTICS: this returns a receipt when the pipeline is ENQUEUED, not when it finishes — \
progress and the final deliverable stream to the chat's activity card. Do NOT claim success, \
summarise the outcome, or quote a result in your reply; a brief acknowledgement is fine. Prefer a \
single `delegate_to_agent` for genuinely one-shot, single-capability work; use this only for \
multi-stage / multi-capability asks."
            .to_string(),
        parameters: build_parameters(props, vec!["goal"]),
        is_control_tool: true,
    }
}

/// Build the `spawn_sub_goal` control tool.
pub fn build_spawn_sub_goal_tool() -> NativeExecutionTool {
    let mut props = Map::new();
    props.insert(
        "goal".to_string(),
        json!({
            "type": "string",
            "description": "A strictly narrower sub-problem than the current goal — one discrete prerequisite whose answer the parent loop will consume to continue. MUST be measurably smaller in scope (one entity, one query, one decision) and have a concrete success artifact (a value, a row set, a confirmed fact). Do NOT paraphrase or restate the parent goal here — that creates a loop, not decomposition."
        }),
    );
    props.insert(
        "unblocks".to_string(),
        json!({
            "type": "string",
            "description": "The specific step in the parent goal that this sub-goal will resolve. Must name a concrete blocked step, not a high-level outcome. If you cannot articulate what this sub-goal unblocks, do not spawn it — continue in the current loop, or call `yield` with accurate open work and blockers when genuinely blocked."
        }),
    );
    props.insert(
        "budget_iterations".to_string(),
        json!({"type": "integer", "description": "Max iterations budget for the sub-goal"}),
    );
    props.insert(
        "reference_task_ids".to_string(),
        json!({
            "type": "array",
            "items": {"type": "string"},
            "description": "Completed task ids whose outputs/artifacts should seed this internal sub-goal. Use for continuation work after get_task_details confirms the source task is completed."
        }),
    );

    NativeExecutionTool {
        name: "spawn_sub_goal".to_string(),
        description: "Spawn an internal sub-goal that is a strict, narrower subset of the current goal — a single discrete prerequisite. Use this for genuine decomposition, never as a soft reset or a retry of the current goal with different wording. For continuation work, pass completed source task ids in `reference_task_ids` so the internal task receives the prior continuation pack/artifact index.".to_string(),
        parameters: build_parameters(props, vec!["goal", "unblocks"]),
        is_control_tool: true,
    }
}

// =============================================================================
// Lane-shaped built-in tool builders (is_control_tool: false)
// =============================================================================

pub fn build_duckdb_tool() -> NativeExecutionTool {
    let mut props = Map::new();
    props.insert(
        "sql".to_string(),
        json!({"type": "string", "description": "SQL query to execute"}),
    );
    props.insert(
        "database".to_string(),
        json!({"type": "string", "description": "Database file path"}),
    );
    props.insert(
        "output_format".to_string(),
        json!({
            "type": "string",
            "description": "Output format for results",
            "enum": ["json", "table", "csv", "tsv"]
        }),
    );
    props.insert(
        "timeout_secs".to_string(),
        json!({"type": "integer", "description": "Query timeout in seconds"}),
    );

    NativeExecutionTool {
        name: "duckdb".to_string(),
        description: "Execute a DuckDB SQL query.".to_string(),
        parameters: build_parameters(props, vec!["sql"]),
        is_control_tool: false,
    }
}

/// Platform-owned continuation for canonical tool results. It is structural,
/// not a capability pack: the executor resolves it against the current task
/// owner and the immutable authority binding that created the reference.
pub fn build_read_result_tool() -> NativeExecutionTool {
    let mut props = Map::new();
    props.insert(
        "result_ref".to_string(),
        json!({"type": "string", "description": "Opaque full_result_ref from a previous bounded tool result."}),
    );
    props.insert(
        "cursor".to_string(),
        json!({"type": "string", "description": "Optional next_cursor from the previous page."}),
    );
    props.insert(
        "field_paths".to_string(),
        json!({
            "type": "array",
            "items": {"type": "string"},
            "maxItems": 32,
            "description": "Optional RFC 6901 JSON pointers selecting complete fields or record collections."
        }),
    );
    props.insert(
        "max_records".to_string(),
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": 1000,
            "description": "Maximum complete records in this page. Defaults to 20."
        }),
    );
    NativeExecutionTool {
        name: "read_result".to_string(),
        description: "Read a bounded authorized page or selected fields from a prior complete tool result. The opaque reference is not a credential; scope, task/run owner, original tool grant, retention, and current authority revision are rechecked on every call.".to_string(),
        parameters: build_parameters(props, vec!["result_ref"]),
        is_control_tool: false,
    }
}

// =============================================================================
// Granular file / search / web / memory tools.
//
// These are separate first-class tools for individual filesystem and web
// operations, sitting alongside the bundled `file` lane. Universal substrate —
// always present regardless of the agent's `tools:` allowlist (Phase 0.6 of
// the flatten plan).
// =============================================================================

// =============================================================================
// Pack capability tool builder
// =============================================================================

/// Build a `NativeExecutionTool` from a pack capability's metadata.
///
/// The `parameter_schema` should be a JSON Schema object. Common metadata
/// fields are merged into its `properties`.
///
/// # `None` is a withheld tool, and it is why this returns an `Option`
///
/// This is the one function that turns a pack capability into something the
/// model can see, so it is where §4A's *"an autonomous agent receives only the
/// restricted form"* is applied — see
/// [`crate::magician_v2::execution::restricted_toolset`]. An outward capability
/// with no bindable form is refused at dispatch on every single call, so
/// offering it costs an iteration and teaches nothing; the honest projection is
/// not to offer it.
///
/// The return type carries that rather than a flag a caller could ignore. Every
/// call site pushes into a `Vec`, and `extend` takes an `Option` directly, so
/// the withheld case cannot be forgotten by writing `push` out of habit.
///
/// **The dispatch gate stays.** This narrows what is offered; `restrict` still
/// decides what runs. A projection is not an authority, and one that let
/// anything through would be a second opinion the gate would have to agree with.
pub fn build_pack_capability_tool(
    name: &str,
    description: &str,
    parameter_schema: &Value,
) -> Option<NativeExecutionTool> {
    use crate::magician_v2::execution::restricted_toolset::{
        project_capability_tool, ToolProjection,
    };

    let mut description = description.to_string();
    let projected = project_capability_tool(name, parameter_schema);
    let parameter_schema = match projected {
        ToolProjection::AsDeclared => parameter_schema.clone(),
        ToolProjection::Restricted { parameters, note } => {
            // Said out loud, not merely enforced. A field that vanished without
            // explanation reads as a schema bug and invites the model to work
            // around it; a sentence naming the closed set is what makes the next
            // attempt succeed rather than repeat.
            if !description.trim().is_empty() {
                description.push_str("\n\n");
            }
            description.push_str(&note);
            parameters
        },
        ToolProjection::Withheld { reason } => {
            tracing::debug!(
                tool = %name,
                "[TOOLSET-WITHHELD] {reason}"
            );
            return None;
        },
    };
    let mut params = parameter_schema;

    // Ensure the schema is an object with properties we can merge into.
    if let Some(obj) = params.as_object_mut() {
        if let Some(props_val) = obj.get_mut("properties") {
            if let Some(props_map) = props_val.as_object_mut() {
                merge_common_metadata(props_map);
            }
        } else {
            // No properties key yet — add one with just common metadata.
            let meta = common_metadata_properties();
            obj.insert("properties".to_string(), Value::Object(meta));
        }
        // Ensure type is set.
        obj.entry("type".to_string())
            .or_insert_with(|| json!("object"));
    } else {
        // Schema wasn't an object — wrap it.
        let meta = common_metadata_properties();
        params = json!({
            "type": "object",
            "properties": meta,
        });
    }

    Some(NativeExecutionTool {
        name: name.to_string(),
        description,
        parameters: params,
        is_control_tool: false,
    })
}

// =============================================================================
// Top-level catalog assembly
// =============================================================================

/// Build the complete native execution tool catalog.
///
/// This is the single source of truth for which tools the model sees during
/// execution. Assembly logic:
///
/// 1. **Direct pack capabilities** — every compiled pack reachable through
///    `direct_capabilities` (the agent's `tools:` allowlist +
///    `UNIVERSAL_BACKEND_PACKS` injection).
/// 2. **Control tools** — autonomous: `need_user_input`, `yield`, and
///    `spawn_sub_goal`; chat: `spawn_sub_goal` only because asking/terminating
///    happens through ordinary conversation. (`goal_reached` /
///    `cannot_proceed` retired in Phase 0.8c-12.)
/// 3. **Delegation tools** — delegate_to_agent and handover_to_agent only when
///    `has_delegation_targets` is true.
pub fn build_execution_native_catalog(ctx: &CatalogBuildContext) -> Vec<NativeExecutionTool> {
    let mut tools = Vec::new();

    // 1. Direct pack capabilities — every compiled pack reachable
    //    through `direct_capabilities` (the agent's `tools:`
    //    allowlist + `UNIVERSAL_BACKEND_PACKS` injection). Every
    //    work-shaped tool now flows through here: memory CRUD,
    //    introspection, task ops, workspace artifacts, filesystem
    //    (`files` / `read_file` / `write_file` / `edit_file` /
    //    `glob` / `grep`), HTTP (`http` / `web_fetch` /
    //    `web_search`), shell (`shell`), meta (`tool_search` /
    //    `switch_personality` / `activate_skill` /
    //    `deactivate_skill`). `browser` and `duckdb` are inner-loop
    //    packs and also flow through here. The previous native-lane
    //    block (Rail A: `file` / `http` / `bash` / `read_file` /
    //    `write_file`) is gone — Phase 0.8c-11 completed the
    //    migration to the generic compiled-handler rail.
    for (name, description, schema) in &ctx.direct_capabilities {
        if BUILTIN_LANE_NAMES.contains(&name.as_str()) {
            continue;
        }
        if tools
            .iter()
            .any(|tool: &NativeExecutionTool| tool.name == name.as_str())
        {
            continue;
        }
        // Runtime gating: hide `vector` when the Ollama daemon (which
        // backs embedding/search) is not reachable. Self-heals — when
        // Ollama comes back, the next catalog build re-exposes the tool.
        // Per-call defense-in-depth is in
        // `compiled_providers::execute_vector_action`.
        if name == "vector" && !crate::magician_v2::runtime::ollama_lifecycle::is_available() {
            continue;
        }
        tools.extend(build_pack_capability_tool(name, description, schema));
    }

    // 2. Control tools. Autonomous loops receive terminal / pause controls;
    //    chat terminates with a normal text reply and can ask the user inline,
    //    so advertising `yield` / `need_user_input` there would expose tools
    //    its dispatcher cannot meaningfully execute. `spawn_sub_goal` is shared
    //    because chat has a concrete handler for it.
    //    `yield` is the
    //    unified terminal — the legacy `goal_reached` / `cannot_proceed`
    //    LLM-facing tools were aliases for it (lower_goal_reached /
    //    lower_cannot_proceed) and have been removed in Phase 0.8c-12
    //    so the LLM sees one terminal instead of three.
    //    `need_user_input` is intentionally separate (different
    //    lifecycle — pauses the agent for user input rather than
    //    terminating).
    if !ctx.is_chat_mode {
        tools.push(build_need_user_input_tool());
        tools.push(build_yield_tool());
        tools.push(build_read_result_tool());
    }
    tools.push(build_spawn_sub_goal_tool());

    // 3. Delegation tools (conditional)
    if ctx.has_delegation_targets {
        tools.push(build_delegate_to_agent_tool());
        tools.push(build_handover_to_agent_tool());
        // `orchestrate_pipeline` is a CHAT-ONLY escape hatch: it puts a
        // multi-stage ask onto a task-backed orchestration loop (where ordered
        // rounds work). Inside a task-backed loop the orchestrator already
        // pipelines via `delegate_to_agent` directly, so it is not offered
        // there.
        if ctx.is_chat_mode {
            tools.push(build_orchestrate_pipeline_tool());
        }
    }

    // BUG-4: chat doesn't consume the autonomous-loop common metadata
    // (`task_state_action`, hover/vision flags, plan-revision bookkeeping,
    // step_completed/failed). Strip them from every tool's schema in chat
    // mode so the LLM doesn't have to fill them in on each call.
    if ctx.is_chat_mode {
        for tool in tools.iter_mut() {
            strip_common_metadata_for_chat(&mut tool.parameters);
        }
    }

    tools.retain(|tool| {
        !ctx.denied_tool_names.iter().any(|denied| {
            crate::magician_v2::agents::types::tool_name_matches_block_entry(&tool.name, denied)
        })
    });

    apply_catalog_context_to_tools(&mut tools, ctx);

    tools
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn default_ctx() -> CatalogBuildContext {
        CatalogBuildContext {
            allowed_action_types: None,
            credentials_enabled: false,
            has_delegation_targets: true,
            direct_capabilities: vec![],
            is_chat_mode: false,
            available_procedure_skills: Vec::new(),
            denied_tool_names: Vec::new(),
            delegate_only_grant_gate: None,
        }
    }

    /// Helper: extract the properties map from a tool's parameters.
    fn props(tool: &NativeExecutionTool) -> &Map<String, Value> {
        tool.parameters["properties"].as_object().unwrap()
    }

    /// Helper: extract the required array from a tool's parameters.
    fn required(tool: &NativeExecutionTool) -> Vec<String> {
        tool.parameters["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    // -- Common metadata --

    const COMMON_METADATA_KEYS: &[&str] =
        &["thinking", DECISION_METADATA_FIELD, "task_state_action"];

    fn assert_has_common_metadata(tool: &NativeExecutionTool) {
        let p = props(tool);
        for key in COMMON_METADATA_KEYS {
            assert!(
                p.contains_key(*key),
                "Tool '{}' missing common metadata field '{}'",
                tool.name,
                key
            );
        }
        assert!(
            !required(tool).contains(&"task_state_action".to_string()),
            "Tool '{}' must keep task_state_action optional",
            tool.name
        );
    }

    #[test]
    fn task_state_action_schema_is_minimal_and_optional() {
        let schema = task_state_action_schema();
        assert_eq!(schema, json!({"type": "object"}));

        let encoded_len = serde_json::to_vec(&schema).unwrap().len();
        assert!(
            encoded_len <= 20,
            "minimal task-state schema regressed to {encoded_len} bytes"
        );
    }

    #[test]
    fn decision_metadata_schema_is_minimal_optional_and_replaces_flat_fields() {
        let metadata = common_metadata_properties();
        assert_eq!(metadata[DECISION_METADATA_FIELD], json!({"type": "object"}));
        for field in LEGACY_DECISION_METADATA_FIELDS {
            assert!(
                !metadata.contains_key(*field),
                "legacy flat metadata field `{field}` must not be advertised"
            );
        }
    }

    /// Provider-free payload eval used by
    /// `scripts/eval-decision-metadata-compaction.sh`.
    #[test]
    fn decision_metadata_schema_compaction_eval() {
        const REPRESENTATIVE_PACK_TOOLS: usize = 24;
        const MIN_PER_TOOL_REDUCTION_PCT: f64 = 85.0;
        const MIN_CATALOG_SAVINGS_BYTES: usize = 12_000;

        let ctx = CatalogBuildContext {
            direct_capabilities: (0..REPRESENTATIVE_PACK_TOOLS)
                .map(|index| {
                    (
                        format!("eval_tool_{index}"),
                        "Representative direct capability".to_string(),
                        json!({
                            "type": "object",
                            "properties": {
                                "query": {"type": "string"},
                                "limit": {"type": "integer"}
                            },
                            "required": ["query"],
                            "additionalProperties": false
                        }),
                    )
                })
                .collect(),
            ..default_ctx()
        };
        let compact_catalog = build_execution_native_catalog(&ctx);
        let mut legacy_catalog = compact_catalog.clone();
        let legacy_properties = [
            (
                "request_hover_discovery",
                json!({"type": "boolean", "description": "Request hover probing for next observation"}),
            ),
            (
                "request_vision",
                json!({"type": "boolean", "description": "TEXT-FIRST: request vision escalation when text insufficient"}),
            ),
            (
                "vision_reason",
                json!({"type": "string", "description": "Reason for vision escalation request"}),
            ),
            (
                "step_completed",
                json!({"type": "string", "description": "Step ID completed by this action"}),
            ),
            (
                "step_failed",
                json!({"type": "string", "description": "Step ID that failed/blocked"}),
            ),
            (
                "needs_plan_revision",
                json!({"type": "boolean", "description": "Set true when plan needs revision"}),
            ),
        ];
        for tool in &mut legacy_catalog {
            let properties = tool.parameters["properties"]
                .as_object_mut()
                .expect("tool properties");
            properties.remove(DECISION_METADATA_FIELD);
            for (name, schema) in &legacy_properties {
                properties.insert((*name).to_string(), schema.clone());
            }
        }

        let compact_sidecar_bytes = serde_json::to_vec(&json!({
            DECISION_METADATA_FIELD: decision_metadata_schema()
        }))
        .unwrap()
        .len();
        let legacy_fields_bytes = serde_json::to_vec(
            &legacy_properties
                .iter()
                .map(|(name, schema)| ((*name).to_string(), schema.clone()))
                .collect::<Map<String, Value>>(),
        )
        .unwrap()
        .len();
        let compact_catalog_bytes = serde_json::to_vec(&compact_catalog).unwrap().len();
        let legacy_catalog_bytes = serde_json::to_vec(&legacy_catalog).unwrap().len();
        let per_tool_reduction_pct = 100.0 * (legacy_fields_bytes - compact_sidecar_bytes) as f64
            / legacy_fields_bytes as f64;
        let catalog_savings_bytes = legacy_catalog_bytes - compact_catalog_bytes;

        println!(
            "decision_metadata_schema_eval tools={} legacy_fields_bytes={} compact_sidecar_bytes={} per_tool_reduction_pct={:.1} legacy_catalog_bytes={} compact_catalog_bytes={} catalog_savings_bytes={}",
            compact_catalog.len(),
            legacy_fields_bytes,
            compact_sidecar_bytes,
            per_tool_reduction_pct,
            legacy_catalog_bytes,
            compact_catalog_bytes,
            catalog_savings_bytes,
        );

        assert!(per_tool_reduction_pct >= MIN_PER_TOOL_REDUCTION_PCT);
        assert!(catalog_savings_bytes >= MIN_CATALOG_SAVINGS_BYTES);
    }

    #[test]
    fn decision_rationale_schema_is_optional_and_runtime_bounded() {
        let metadata = common_metadata_properties();
        let thinking = &metadata["thinking"];
        assert_eq!(thinking["type"], "string");
        assert!(thinking.get("maxLength").is_none());
        assert!(thinking.get("description").is_none());
    }

    /// Offline payload guard used by
    /// `scripts/eval-agentic-decision-rationale.sh`.
    ///
    /// Keep the compact rationale property bounded in the representative
    /// production catalog. The legacy description is deliberately absent.
    #[test]
    fn decision_rationale_schema_payload_eval() {
        const REPRESENTATIVE_PACK_TOOLS: usize = 24;
        const MAX_SCHEMA_BYTES_PER_TOOL: usize = 48;

        let ctx = CatalogBuildContext {
            direct_capabilities: (0..REPRESENTATIVE_PACK_TOOLS)
                .map(|index| {
                    (
                        format!("eval_tool_{index}"),
                        "Representative direct capability".to_string(),
                        json!({
                            "type": "object",
                            "properties": {
                                "query": {"type": "string"},
                                "limit": {"type": "integer"}
                            },
                            "required": ["query"],
                            "additionalProperties": false
                        }),
                    )
                })
                .collect(),
            ..default_ctx()
        };
        let catalog = build_execution_native_catalog(&ctx);
        let with_rationale_bytes = serde_json::to_vec(&catalog).unwrap().len();
        let mut without_rationale = catalog.clone();
        for tool in &mut without_rationale {
            tool.parameters["properties"]
                .as_object_mut()
                .expect("tool properties")
                .remove("thinking");
        }
        let without_rationale_bytes = serde_json::to_vec(&without_rationale).unwrap().len();
        let total_overhead_bytes = with_rationale_bytes - without_rationale_bytes;
        let overhead_bytes_per_tool = if catalog.is_empty() {
            0
        } else {
            (total_overhead_bytes + catalog.len() - 1) / catalog.len()
        };

        println!(
            "decision_rationale_schema_eval tools={} with_rationale_bytes={} \
             without_rationale_bytes={} total_overhead_bytes={} \
             overhead_bytes_per_tool={}",
            catalog.len(),
            with_rationale_bytes,
            without_rationale_bytes,
            total_overhead_bytes,
            overhead_bytes_per_tool
        );

        assert!(
            overhead_bytes_per_tool <= MAX_SCHEMA_BYTES_PER_TOOL,
            "decision-rationale schema overhead regressed to \
             {overhead_bytes_per_tool} bytes/tool (limit {MAX_SCHEMA_BYTES_PER_TOOL})"
        );
    }

    /// Offline payload eval used by `scripts/eval-task-state-schema-compaction.sh`.
    ///
    /// The 1,207-byte baseline is the compact-JSON length of the full schema
    /// shipped through v0.6.1031. It is intentionally retained as a stable
    /// comparison fixture instead of rebuilding production's removed schema.
    #[test]
    fn task_state_schema_compaction_eval() {
        const LEGACY_TASK_STATE_SCHEMA_BYTES: usize = 1_207;
        const REPRESENTATIVE_PACK_TOOLS: usize = 24;

        let ctx = CatalogBuildContext {
            direct_capabilities: (0..REPRESENTATIVE_PACK_TOOLS)
                .map(|index| {
                    (
                        format!("eval_tool_{index}"),
                        "Representative direct capability".to_string(),
                        json!({
                            "type": "object",
                            "properties": {
                                "query": {"type": "string"},
                                "limit": {"type": "integer"}
                            },
                            "required": ["query"],
                            "additionalProperties": false
                        }),
                    )
                })
                .collect(),
            ..default_ctx()
        };
        let catalog = build_execution_native_catalog(&ctx);
        let compact_schema_bytes = serde_json::to_vec(&task_state_action_schema())
            .unwrap()
            .len();
        let compact_catalog_bytes = serde_json::to_vec(&catalog).unwrap().len();
        // Every representative tool already has at least one required
        // parameter, so the legacy required entry cost is one comma plus the
        // serialized field name.
        let legacy_required_entry_bytes = 1 + serde_json::to_vec(&json!("task_state_action"))
            .unwrap()
            .len();
        let legacy_catalog_bytes = compact_catalog_bytes
            + catalog.len()
                * (LEGACY_TASK_STATE_SCHEMA_BYTES - compact_schema_bytes
                    + legacy_required_entry_bytes);
        let schema_reduction_pct = 100.0
            * (LEGACY_TASK_STATE_SCHEMA_BYTES - compact_schema_bytes) as f64
            / LEGACY_TASK_STATE_SCHEMA_BYTES as f64;
        let catalog_reduction_pct = 100.0 * (legacy_catalog_bytes - compact_catalog_bytes) as f64
            / legacy_catalog_bytes as f64;

        println!(
            "task_state_schema_eval tools={} legacy_schema_bytes={} compact_schema_bytes={} schema_reduction_pct={:.1} legacy_catalog_bytes={} compact_catalog_bytes={} catalog_reduction_pct={:.1}",
            catalog.len(),
            LEGACY_TASK_STATE_SCHEMA_BYTES,
            compact_schema_bytes,
            schema_reduction_pct,
            legacy_catalog_bytes,
            compact_catalog_bytes,
            catalog_reduction_pct,
        );

        assert!(schema_reduction_pct >= 98.0);
        assert!(catalog_reduction_pct >= 25.0);
    }

    #[test]
    fn all_control_tools_have_common_metadata_fields() {
        let control_tools = vec![
            build_need_user_input_tool(),
            build_yield_tool(),
            build_delegate_to_agent_tool(),
            build_handover_to_agent_tool(),
            build_spawn_sub_goal_tool(),
        ];
        for tool in &control_tools {
            assert!(
                tool.is_control_tool,
                "{} should be a control tool",
                tool.name
            );
            assert_has_common_metadata(tool);
        }
    }

    // Phase 0.8c-12: `build_goal_reached_tool` / `build_cannot_proceed_tool`
    // deleted. The LLM-facing terminal is now exclusively `yield` —
    // see `yield_tool_schema_has_summary_and_self_classification`.

    #[test]
    fn need_user_input_schema_has_question_input_type_hint_options() {
        let tool = build_need_user_input_tool();
        let p = props(&tool);
        assert!(p.contains_key("question"));
        assert!(p.contains_key("input_type"));
        assert!(p.contains_key("hint"));
        assert!(p.contains_key("options"));

        // input_type has enum
        let input_enum = p["input_type"]["enum"].as_array().unwrap();
        assert!(input_enum.contains(&json!("text")));
        assert!(input_enum.contains(&json!("password")));
        assert!(input_enum.contains(&json!("guidance")));

        // required
        let req = required(&tool);
        assert!(req.contains(&"question".to_string()));
        assert!(req.contains(&"input_type".to_string()));

        // options items have id, label
        let opt_required = &p["options"]["items"]["required"];
        let opt_req: Vec<String> = opt_required
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(opt_req.contains(&"id".to_string()));
        assert!(opt_req.contains(&"label".to_string()));
    }

    #[test]
    fn model_facing_control_schemas_do_not_advertise_retired_terminals() {
        let controls = [
            build_need_user_input_tool(),
            build_yield_tool(),
            build_spawn_sub_goal_tool(),
        ];
        for tool in controls {
            let rendered = serde_json::to_string(&tool).expect("control tool serializes");
            assert!(!rendered.contains("goal_reached"), "{}", tool.name);
            assert!(!rendered.contains("cannot_proceed"), "{}", tool.name);
        }
    }

    #[test]
    fn delegate_to_agent_has_full_target_shape() {
        let tool = build_delegate_to_agent_tool();
        let p = props(&tool);
        assert!(p.contains_key("delegation_targets"));

        let target_props = &p["delegation_targets"]["items"]["properties"];
        assert!(target_props.get("target_agent_id").is_some());
        assert!(target_props.get("context").is_some());
        assert!(target_props.get("input_artifact_ids").is_some());
        assert!(target_props.get("input_data").is_some());
        assert!(target_props.get("tutor_action").is_some());
        assert!(target_props.get("depth").is_some());
        assert!(target_props.get("timeout_secs").is_some());
        assert!(target_props.get("spend_token_ids").is_some());

        // depth enum
        let depth_enum = target_props["depth"]["enum"].as_array().unwrap();
        assert!(depth_enum.contains(&json!("normal")));
        assert!(depth_enum.contains(&json!("deep")));
        assert!(depth_enum.contains(&json!("thorough")));

        // item required
        let item_req: Vec<String> = p["delegation_targets"]["items"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(item_req.contains(&"target_agent_id".to_string()));
        assert!(item_req.contains(&"context".to_string()));

        // top-level required
        assert!(required(&tool).contains(&"delegation_targets".to_string()));
    }

    #[test]
    fn relay_catalog_caps_delegation_to_one_child_without_dropping_tools() {
        let mut relay_ctx = default_ctx();
        relay_ctx.direct_capabilities.push((
            "relay_work".to_string(),
            "Relay work tool".to_string(),
            json!({"type": "object", "properties": {}}),
        ));
        relay_ctx.delegate_only_grant_gate =
            CatalogAgentPolicy::from_execution(Some("harness-sre"), None);

        let mut unrelated_ctx = relay_ctx.clone();
        unrelated_ctx.delegate_only_grant_gate =
            CatalogAgentPolicy::from_execution(Some("cto"), None);

        let relay_catalog = build_execution_native_catalog(&relay_ctx);
        let unrelated_catalog = build_execution_native_catalog(&unrelated_ctx);
        let relay_names: Vec<&str> = relay_catalog
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        let unrelated_names: Vec<&str> = unrelated_catalog
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(
            relay_names, unrelated_names,
            "Relay keeps the full tool set"
        );
        assert!(relay_names.contains(&"relay_work"));

        let relay_delegate = relay_catalog
            .iter()
            .find(|tool| tool.name == "delegate_to_agent")
            .unwrap();
        let unrelated_delegate = unrelated_catalog
            .iter()
            .find(|tool| tool.name == "delegate_to_agent")
            .unwrap();
        assert_eq!(
            props(relay_delegate)["delegation_targets"]["maxItems"],
            json!(1)
        );
        assert_eq!(
            props(relay_delegate)["delegation_targets"]["minItems"],
            json!(1)
        );
        assert!(relay_delegate.description.contains("RELAY ROOT LIMIT"));
        assert_eq!(
            relay_delegate
                .description
                .matches("RELAY ROOT LIMIT")
                .count(),
            1
        );
        assert!(!relay_delegate.description.contains("PARALLEL FAN-OUT"));
        assert!(props(unrelated_delegate)["delegation_targets"]
            .get("maxItems")
            .is_none());
        assert!(unrelated_delegate.description.contains("PARALLEL FAN-OUT"));
    }

    #[test]
    fn handover_has_target_context_preserve() {
        let tool = build_handover_to_agent_tool();
        let p = props(&tool);
        assert!(p.contains_key("target_agent_id"));
        assert!(p.contains_key("context"));
        assert!(p.contains_key("preserve_live_execution_context"));

        let req = required(&tool);
        assert!(req.contains(&"target_agent_id".to_string()));
        assert!(req.contains(&"context".to_string()));
        assert!(req.contains(&"preserve_live_execution_context".to_string()));
    }

    #[test]
    fn spawn_sub_goal_has_goal_and_budget() {
        let tool = build_spawn_sub_goal_tool();
        let p = props(&tool);
        assert!(p.contains_key("goal"));
        assert!(p.contains_key("budget_iterations"));
        assert!(p.contains_key("reference_task_ids"));
        assert_eq!(p["goal"]["type"], "string");
        assert_eq!(p["budget_iterations"]["type"], "integer");
        assert_eq!(p["reference_task_ids"]["type"], "array");

        let req = required(&tool);
        assert!(req.contains(&"goal".to_string()));
        // budget_iterations is optional
        assert!(!req.contains(&"budget_iterations".to_string()));
    }

    // `create_task` is now a compiled capability pack
    // (see `embedded_pack_defs/create_task.yaml`); the schema is
    // validated at boot via the `embedded_compiled_pack_defs` parse —
    // a malformed YAML panics the binary on startup, which subsumes
    // this property-shape unit test.

    // Native `file` / `http` / `bash` builders deleted in Phase
    // 0.8c-11; surfaces moved to compiled packs (`files`, `http`,
    // `shell`) via `UNIVERSAL_BACKEND_PACKS`. Pack-schema validation
    // happens at boot via `embedded_compiled_pack_defs` parsing.

    #[test]
    fn duckdb_tool_has_sql_database_format() {
        let tool = build_duckdb_tool();
        let p = props(&tool);
        assert!(!tool.is_control_tool);
        assert!(p.contains_key("sql"));
        assert!(p.contains_key("database"));
        assert!(p.contains_key("output_format"));
        assert!(p.contains_key("timeout_secs"));

        let fmt_enum = p["output_format"]["enum"].as_array().unwrap();
        assert!(fmt_enum.contains(&json!("json")));
        assert!(fmt_enum.contains(&json!("csv")));

        assert!(required(&tool).contains(&"sql".to_string()));
        assert_has_common_metadata(&tool);
    }

    // -- Catalog assembly --

    #[test]
    fn catalog_chat_mode_excludes_rail_a_work_tools() {
        // Strict chat-as-face contract: chat does no work via Rail A
        // (ExecutableAction enum: file/http/bash + granular file + web).
        // Tools that have migrated to the generic compiled rail
        // (edit_file, glob, grep, tool_search) arrive via
        // `direct_capabilities` in both chat and autonomous — the
        // fixture here passes empty direct_capabilities so they're
        // absent from this catalog regardless of mode.
        let ctx = CatalogBuildContext {
            allowed_action_types: Some(vec!["browser".to_string(), "bash".to_string()]),
            credentials_enabled: false,
            has_delegation_targets: false,
            direct_capabilities: vec![],
            is_chat_mode: true,
            available_procedure_skills: Vec::new(),
            denied_tool_names: Vec::new(),
            delegate_only_grant_gate: None,
        };
        let catalog = build_execution_native_catalog(&ctx);
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();

        // Chat terminates/asks through normal text, so autonomous-only pause
        // and terminal controls must not be advertised here.
        assert!(!names.contains(&"yield"));
        assert!(!names.contains(&"need_user_input"));
        assert!(names.contains(&"spawn_sub_goal"));
        // Phase 0.8c-12: legacy LLM-facing terminals retired.
        assert!(!names.contains(&"goal_reached"));
        assert!(!names.contains(&"cannot_proceed"));

        // Browser is not a built-in lane (it's a pack capability).
        assert!(!names.contains(&"browser"));
        // `duckdb` is a pack, not surfaced via this catalog.
        assert!(!names.contains(&"duckdb"));

        // Rail A is fully retired (Phase 0.8c-11). Migrated lanes
        // arrive via direct_capabilities, not via this catalog
        // builder — empty fixture below confirms they're absent here.
        assert!(!names.contains(&"files"));
        assert!(!names.contains(&"http"));
        assert!(!names.contains(&"shell"));
        assert!(!names.contains(&"read_file"));
        assert!(!names.contains(&"write_file"));
        // Legacy native names (`file` / `bash`) deleted entirely.
        assert!(!names.contains(&"file"));
        assert!(!names.contains(&"bash"));

        // Rail C migrants: not in this catalog when
        // direct_capabilities is empty; arrive via direct_capabilities
        // for both chat and autonomous (universal-pack mechanism).
        assert!(!names.contains(&"edit_file"));
        assert!(!names.contains(&"glob"));
        assert!(!names.contains(&"grep"));
        assert!(!names.contains(&"tool_search"));
        assert!(!names.contains(&"web_fetch"));
        assert!(!names.contains(&"web_search"));
    }

    #[test]
    fn catalog_autonomous_mode_has_no_rail_a_lanes() {
        // Phase 0.8c-11: Rail A is fully retired. The autonomous
        // catalog builder no longer pushes any of the legacy native
        // lanes — they all arrive via direct_capabilities through
        // the universal-pack mechanism. Control tools + delegation
        // tools are the only universal-by-builder surface left.
        let ctx = CatalogBuildContext {
            allowed_action_types: None,
            credentials_enabled: false,
            has_delegation_targets: false,
            direct_capabilities: vec![],
            is_chat_mode: false,
            available_procedure_skills: Vec::new(),
            denied_tool_names: Vec::new(),
            delegate_only_grant_gate: None,
        };
        let catalog = build_execution_native_catalog(&ctx);
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        // Migrated lanes NOT pushed by this catalog builder.
        assert!(!names.contains(&"file"));
        assert!(!names.contains(&"http"));
        assert!(!names.contains(&"bash"));
        assert!(!names.contains(&"files"));
        assert!(!names.contains(&"shell"));
        assert!(!names.contains(&"read_file"));
        assert!(!names.contains(&"write_file"));
        // Rail C migrants NOT pushed by this catalog builder.
        assert!(!names.contains(&"edit_file"));
        assert!(!names.contains(&"glob"));
        assert!(!names.contains(&"grep"));
        assert!(!names.contains(&"tool_search"));
        assert!(!names.contains(&"web_fetch"));
        assert!(!names.contains(&"web_search"));
        // Control tools (universal-by-builder). `yield` is the unified terminal
        // since Phase 0.8c-12 retired `goal_reached`/`cannot_proceed`.
        assert!(names.contains(&"yield"));
    }

    #[test]
    fn catalog_includes_browser_when_registered_as_direct_pack() {
        let ctx = CatalogBuildContext {
            allowed_action_types: Some(vec!["browser".to_string()]),
            credentials_enabled: false,
            has_delegation_targets: false,
            direct_capabilities: vec![(
                "browser".to_string(),
                "Drive a browser through the inner loop".to_string(),
                json!({"type":"object","properties":{},"required":[]}),
            )],
            is_chat_mode: false,
            available_procedure_skills: Vec::new(),
            denied_tool_names: Vec::new(),
            delegate_only_grant_gate: None,
        };
        let catalog = build_execution_native_catalog(&ctx);
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();

        assert!(names.contains(&"browser"));
        assert!(!names.contains(&"browser_scroll"));
        assert!(!names.contains(&"browser_click"));
        assert!(names.contains(&"yield"));
    }

    #[test]
    fn catalog_excludes_delegation_when_no_targets() {
        let ctx = CatalogBuildContext {
            has_delegation_targets: false,
            ..default_ctx()
        };
        let catalog = build_execution_native_catalog(&ctx);
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        assert!(!names.contains(&"delegate_to_agent"));
        assert!(!names.contains(&"handover_to_agent"));
    }

    #[test]
    fn catalog_includes_delegation_when_targets_present() {
        let ctx = CatalogBuildContext {
            has_delegation_targets: true,
            ..default_ctx()
        };
        let catalog = build_execution_native_catalog(&ctx);
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"delegate_to_agent"));
        assert!(names.contains(&"handover_to_agent"));
    }

    #[test]
    fn catalog_includes_direct_pack_tools() {
        let ctx = CatalogBuildContext {
            direct_capabilities: vec![(
                "custom_search".to_string(),
                "Search custom index".to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"}
                    },
                    "required": ["query"]
                }),
            )],
            ..default_ctx()
        };
        let catalog = build_execution_native_catalog(&ctx);
        let tool = catalog.iter().find(|t| t.name == "custom_search").unwrap();
        assert!(!tool.is_control_tool);
        assert_eq!(tool.description, "Search custom index");
    }

    #[test]
    fn catalog_dedupes_direct_capabilities_by_name() {
        // Phase 0.8c-11: `BUILTIN_LANE_NAMES` is empty now (the
        // previously-built-in `file` / `http` / `bash` lanes
        // migrated to compiled packs `files` / `http` / `shell` and
        // arrive via `direct_capabilities` themselves). Catalog
        // assembly's only collision-prevention is the
        // `tools.iter().any(|tool| tool.name == name)` check —
        // verify a second direct-capability entry with a duplicate
        // name doesn't double-add.
        let ctx = CatalogBuildContext {
            direct_capabilities: vec![
                (
                    "my_tool".to_string(),
                    "First registration".to_string(),
                    json!({"type": "object"}),
                ),
                (
                    "my_tool".to_string(),
                    "Should be skipped (duplicate name)".to_string(),
                    json!({"type": "object"}),
                ),
            ],
            ..default_ctx()
        };
        let catalog = build_execution_native_catalog(&ctx);
        let my_tools: Vec<&NativeExecutionTool> =
            catalog.iter().filter(|t| t.name == "my_tool").collect();
        assert_eq!(my_tools.len(), 1);
        assert_eq!(my_tools[0].description, "First registration");
    }

    #[test]
    fn pack_tool_merges_common_metadata() {
        let tool = build_pack_capability_tool(
            "my_cap",
            "My capability",
            &json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                },
                "required": ["query"]
            }),
        )
        .expect("`my_cap` reaches nobody, so it is never withheld");
        let p = props(&tool);
        // Original property preserved
        assert!(p.contains_key("query"));
        // Common metadata merged in
        assert_has_common_metadata(&tool);
        assert!(!tool.is_control_tool);
    }

    /// BUG-8: Phase 2.5 promoted tools must NOT appear in autonomous catalogs.
    /// They no-op silently in the autonomous executor, which would mislead
    /// LLMs into believing the call succeeded with empty results.
    #[test]
    fn autonomous_catalog_excludes_phase_2_5_chat_only_tools() {
        let ctx = default_ctx();
        assert!(!ctx.is_chat_mode);
        let catalog = build_execution_native_catalog(&ctx);
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        // The chat-only native catalog block is now empty (all promoted
        // tools live in the registry as compiled packs). Keep the test
        // harness wiring for any future chat-only natives.
        for chat_only in &[] as &[&str] {
            assert!(
                !names.contains(chat_only),
                "Autonomous catalog must exclude chat-only tool `{chat_only}`",
            );
        }
        // The non-promoted control tools should still be present.
        // `create_task` and other task-management tools moved to
        // compiled packs (see `embedded_pack_defs/`); they no longer
        // live in this native catalog. `yield` is the unified terminal
        // (Phase 0.8c-12 retired `goal_reached`).
        assert!(names.contains(&"yield"));
    }

    /// BUG-8: chat-mode catalogs include the Phase 2.5 promoted tools so the
    /// chat dispatcher (which has the real handlers) can route them.
    #[test]
    fn chat_catalog_includes_phase_2_5_tools() {
        let ctx = CatalogBuildContext {
            is_chat_mode: true,
            ..default_ctx()
        };
        let catalog = build_execution_native_catalog(&ctx);
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        // The chat-only native catalog block is now empty (all promoted
        // tools live in the registry as compiled packs). Keep the test
        // harness wiring for any future chat-only natives.
        for chat_only in &[] as &[&str] {
            assert!(
                names.contains(chat_only),
                "Chat catalog must include chat-only tool `{chat_only}`",
            );
        }
    }

    /// BUG-4: chat-mode tool schemas must not include the autonomous-loop
    /// common metadata fields (`task_state_action`, hover/vision flags, etc.).
    /// Saves the LLM from having to fill them in on every call.
    #[test]
    fn chat_catalog_strips_common_metadata() {
        let ctx = CatalogBuildContext {
            is_chat_mode: true,
            ..default_ctx()
        };
        let catalog = build_execution_native_catalog(&ctx);
        for tool in &catalog {
            let p = props(tool);
            for key in COMMON_METADATA_KEYS {
                assert!(
                    !p.contains_key(*key),
                    "Chat-mode tool `{}` should not have common metadata field `{}`",
                    tool.name,
                    key,
                );
            }
            for key in LEGACY_DECISION_METADATA_FIELDS {
                assert!(
                    !p.contains_key(*key),
                    "Chat-mode tool `{}` should not have legacy metadata field `{}`",
                    tool.name,
                    key,
                );
            }
            // `task_state_action` should also be absent from `required`.
            let required = tool
                .parameters
                .get("required")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                !required
                    .iter()
                    .any(|value| value.as_str() == Some("task_state_action")),
                "Chat-mode tool `{}` should not require task_state_action",
                tool.name
            );
        }
    }
}

#[cfg(test)]
mod yield_contract_tests {
    use super::*;

    /// The yield summary is what the reader receives. Asked for "what
    /// happened", the model described; asked for the results, it states them.
    #[test]
    fn the_yield_summary_asks_for_the_results_not_a_description() {
        let tool = build_yield_tool();
        let description = tool.parameters["properties"]["summary"]["description"]
            .as_str()
            .expect("summary description");
        assert!(description.contains("verbatim"), "{description}");
        assert!(description.contains("State the results"), "{description}");
        assert!(!description.contains("what happened, in"), "{description}");
    }
}
