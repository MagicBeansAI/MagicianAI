//! `submit_plan` for harness engines in agent runs.
//!
//! Planning is optional. A harness (Claude Code, Codex, …) usually acts with
//! several tool calls in one turn. When it already knows a longer sequence it
//! may hand it over with `submit_plan` and end its turn. The agent loop then
//! runs the steps as a sticky plan: each is validated by the Decision Engine
//! and run without another harness turn, until a step fails, no longer fits
//! the tools, or the run is steered (`decision_rail::maybe_decide`).
//!
//! Chat harness turns do not get the tool: one harness turn is the whole chat
//! reply, so there is no loop between turns to run a plan in.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use decision_engine_contract::action::{ActionCandidate, ActionPlan, ToolCall};
use serde_json::{json, Value};

use super::grant::PlaneGrant;
use crate::magician_v2::execution::agentic::native_catalog::{
    SUBMIT_PLAN_MAX_STEPS, SUBMIT_PLAN_TOOL,
};

/// Plans submitted this turn, by grant session. Taken when the turn settles.
static SUBMITTED: LazyLock<Mutex<HashMap<String, ActionPlan>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Offered on a live harness turn of an agent run whose engine is on the
/// Decision Engine rail: a plan runs only through the rail, so without it a
/// submitted plan could never start.
pub(crate) fn offers_submit_plan(grant: &PlaneGrant) -> bool {
    agent_run_harness_turn(grant)
        && crate::magician_v2::decision_host::decision_backend_for(
            &super::turn_engine::run_engine_for(&grant.ctx),
        )
        .is_some()
}

/// A live harness turn of an agent run. Conversation grants carry a chat
/// turn ledger; terminal grants are no live harness turn.
fn agent_run_harness_turn(grant: &PlaneGrant) -> bool {
    grant.live_harness_turn && grant.turn_ledger.is_none()
}

/// The tool as `tools/list` advertises it to a harness.
pub(crate) fn tool_entry() -> Value {
    json!({
        "name": SUBMIT_PLAN_TOOL,
        "description": "Optional. Hand Magician a sequence of tool calls to run after this turn, \
    without another turn of yours between steps. Use it only when you already know every step and its \
    arguments (no step needs an earlier step's result); otherwise call the tools yourself. After \
    submitting, end your turn: Magician runs the steps in order, each checked by the Decision Engine, \
    and gives you a new turn when the plan finishes, a step fails or no longer fits, or the run is \
    steered. Tool calls after submitting are refused.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": SUBMIT_PLAN_MAX_STEPS,
                    "description": "The calls to run, in order. Each names a tool you have now and gives its complete arguments.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "tool": {"type": "string", "description": "Exact name of a tool in your tool list, without any MCP server prefix (e.g. `agentmail-send`, not `mcp__magician-plane__agentmail-send`)."},
                            "arguments": {"type": "object", "description": "The tool's complete arguments."},
                            "reason": {"type": "string", "description": "Why this step advances the task."}
                        },
                        "required": ["tool", "arguments"]
                    }
                }
            },
            "required": ["steps"]
        }
    })
}

/// Record a harness's plan for its turn. The shape rules are the native
/// tool's (`parse_submit_plan_steps`); each step is validated against the
/// tools only when it runs.
pub(crate) fn submit(grant: &PlaneGrant, arguments: &Value) -> Result<String, String> {
    let mut steps =
        crate::magician_v2::execution::agentic::native_lowering::parse_submit_plan_steps(
            arguments,
        )?;
    // A harness names tools through Magician's MCP server, and a CLI may write
    // that prefix into a plan (`mcp__magician-plane__agentmail-send`). The plan
    // runs on Magician's own names, so each step must name a tool in this
    // turn's list, prefix or not. Anything else is refused now, inside the
    // turn, where the harness can fix it. On 2026-10-13 such a name failed as
    // `tool_not_authorized` only after the turn had ended.
    let listed: HashMap<String, Value> = super::catalog::plane_tools_list(grant)
        .into_iter()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(Value::as_str)?.to_string();
            Some((
                name,
                entry.get("inputSchema").cloned().unwrap_or(Value::Null),
            ))
        })
        .collect();
    for (index, (tool, arguments, _)) in steps.iter_mut().enumerate() {
        let bare = without_server_prefix(tool);
        let Some(schema) = listed.get(bare).filter(|_| bare != SUBMIT_PLAN_TOOL) else {
            return Err(format!(
                "step {}: `{tool}` is not one of your tools. Use the exact tool name from your \
                 tool list, without any MCP server prefix.",
                index + 1
            ));
        };
        // The rail checks each step against the same schema when it runs; a
        // mismatch found then ends the plan after the turn, where the harness
        // cannot fix it.
        if let Err(problem) = arguments_fit(bare, arguments, schema) {
            return Err(format!("step {}: {problem}", index + 1));
        }
        *tool = bare.to_string();
    }
    let count = steps.len();
    tracing::info!(
        session = %grant.session_id,
        tools = ?steps.iter().map(|(tool, _, _)| tool.as_str()).collect::<Vec<_>>(),
        "[PLANE] harness submitted a plan"
    );
    let plan = ActionPlan {
        scope: None,
        steps: steps
            .into_iter()
            .enumerate()
            .map(|(index, (tool, arguments, reason))| ActionCandidate {
                id: format!("plan-step-{}", index + 1),
                call: ToolCall { tool, arguments },
                bindings: Vec::new(),
                reason,
            })
            .collect(),
    };
    SUBMITTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(grant.session_id.clone(), plan);
    Ok(format!(
        "Plan recorded: {count} step{}. End your turn now; Magician runs the steps next.",
        if count == 1 { "" } else { "s" }
    ))
}

/// Whether `arguments` fit the schema `tools/list` offered for `tool`: the
/// Decision Engine's own check, with the keys that broke it named.
fn arguments_fit(tool: &str, arguments: &Value, schema: &Value) -> Result<(), String> {
    use decision_engine_contract::action::ActionTool;
    let call = ToolCall {
        tool: tool.to_string(),
        arguments: arguments.clone(),
    };
    let offered = [ActionTool {
        name: tool.to_string(),
        description: String::new(),
        parameters: schema.clone(),
    }];
    let Err(code) = magician_decision::action::validation::validate_call(&call, &offered) else {
        return Ok(());
    };
    let properties = schema.get("properties").and_then(Value::as_object);
    let given = arguments.as_object();
    let unknown: Vec<&str> = given
        .into_iter()
        .flat_map(|args| args.keys())
        .filter(|key| properties.is_some_and(|props| !props.contains_key(key.as_str())))
        .map(String::as_str)
        .collect();
    let missing: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|key| given.is_some_and(|args| !args.contains_key(*key)))
        .collect();
    let mut detail = Vec::new();
    if !unknown.is_empty() {
        detail.push(format!("not accepted: {}", unknown.join(", ")));
    }
    if !missing.is_empty() {
        detail.push(format!("missing: {}", missing.join(", ")));
    }
    if detail.is_empty() {
        detail.push(code);
    }
    Err(format!(
        "the arguments for `{tool}` do not match its schema in your tool list ({}).",
        detail.join("; ")
    ))
}

/// Magician's tool name without the MCP server prefix a CLI may show its model:
/// Claude Code's `mcp__magician-plane__<tool>`, Agy's `magician_plane`, or a
/// dotted or colon form.
fn without_server_prefix(tool: &str) -> &str {
    let tool = tool.trim();
    let tool = tool.strip_prefix("mcp__").unwrap_or(tool);
    for server in ["magician-plane", "magician_plane"] {
        for separator in ["__", ".", ":", "/"] {
            if let Some(bare) = tool
                .strip_prefix(server)
                .and_then(|rest| rest.strip_prefix(separator))
            {
                return bare;
            }
        }
    }
    tool
}

/// Whether this turn already submitted a plan.
pub(crate) fn pending(grant: &PlaneGrant) -> bool {
    SUBMITTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(&grant.session_id)
}

/// Take the plan a turn submitted, if any. Public for `turn_engine.rs`,
/// which `magician-bin`'s contract test compiles by path.
pub fn take(session_id: &str) -> Option<ActionPlan> {
    SUBMITTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_grant(id: &str) -> PlaneGrant {
        let mut grant = PlaneGrant::for_test(id);
        grant.live_harness_turn = true;
        grant
    }

    #[test]
    fn only_an_agent_runs_harness_turn_qualifies_for_submit_plan() {
        assert!(agent_run_harness_turn(&run_grant("exec-offer")));
        let terminal = PlaneGrant::for_test("exec-terminal");
        assert!(
            !agent_run_harness_turn(&terminal),
            "terminal grants are no harness turn"
        );
        let mut chat = run_grant("exec-chat");
        chat.turn_ledger = Some(Default::default());
        assert!(
            !agent_run_harness_turn(&chat),
            "a chat harness turn has no loop to run a plan in"
        );
        assert_eq!(tool_entry()["name"], SUBMIT_PLAN_TOOL);
    }

    /// A grant that lists `agentmail-send` and `read_file`, the way a turn's
    /// tools/list does.
    fn listing_grant(id: &str) -> PlaneGrant {
        let mut grant = run_grant(id);
        for name in ["agentmail-send", "read_file"] {
            grant.bridged_tools.insert(
                name.to_string(),
                magicllm::types::LLMToolSpec {
                    name: name.to_string(),
                    description: format!("{name} fixture"),
                    parameters: json!({"type": "object"}),
                },
            );
        }
        grant
    }

    /// A step whose arguments the rail would refuse is refused now, inside
    /// the turn, naming the keys: on 2026-10-13 two plans failed only after
    /// the turn, as `arguments_do_not_match_schema`.
    #[test]
    fn a_step_that_does_not_fit_its_tool_schema_is_refused_inside_the_turn() {
        let mut grant = run_grant("exec-schema");
        grant.bridged_tools.insert(
            "send_note".to_string(),
            magicllm::types::LLMToolSpec {
                name: "send_note".to_string(),
                description: "send_note fixture".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {"to": {"type": "string"}, "text": {"type": "string"}},
                    "required": ["to"],
                    "additionalProperties": false
                }),
            },
        );
        let extra = submit(
            &grant,
            &json!({"steps": [{"tool": "send_note", "arguments": {"to": "a@x.com", "inbox_id": "me"}}]}),
        )
        .expect_err("an undeclared key");
        assert!(
            extra.contains("step 1") && extra.contains("not accepted: inbox_id"),
            "{extra}"
        );
        let missing = submit(
            &grant,
            &json!({"steps": [{"tool": "send_note", "arguments": {"text": "hi"}}]}),
        )
        .expect_err("a missing required key");
        assert!(missing.contains("missing: to"), "{missing}");
        assert!(!pending(&grant), "a refused plan records nothing");
        submit(
            &grant,
            &json!({"steps": [{"tool": "send_note", "arguments": {"to": "a@x.com", "text": "hi"}}]}),
        )
        .expect("a fitting plan");
        assert!(take(&grant.session_id).is_some());
    }

    #[test]
    fn a_submitted_plan_is_recorded_for_its_turn_and_taken_once() {
        let grant = listing_grant("exec-submit");
        let text = submit(
            &grant,
            &json!({"steps": [
                {"tool": "read_file", "arguments": {"path": "a.txt"}, "reason": "Read it"},
                {"tool": "mcp__magician-plane__agentmail-send", "arguments": {"command": "Send it"}}
            ]}),
        )
        .expect("a valid plan");
        assert!(text.contains("2 steps"), "{text}");
        assert!(pending(&grant));
        let plan = take(&grant.session_id).expect("the plan");
        assert_eq!(
            plan.steps
                .iter()
                .map(|s| (s.id.as_str(), s.call.tool.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("plan-step-1", "read_file"),
                ("plan-step-2", "agentmail-send")
            ],
            "the MCP server prefix a CLI shows its model is stripped"
        );
        assert!(
            !pending(&grant) && take(&grant.session_id).is_none(),
            "taken once"
        );

        assert!(submit(&grant, &json!({"steps": []})).is_err());
        assert!(!pending(&grant), "a malformed plan records nothing");
    }

    /// A step naming a tool the turn does not list is refused inside the
    /// turn, so the harness can fix it, not after the turn has ended.
    #[test]
    fn a_plan_step_outside_the_turns_tools_is_refused_at_submission() {
        let grant = listing_grant("exec-unlisted");
        let error = submit(
            &grant,
            &json!({"steps": [{"tool": "shell", "arguments": {"cmd": "ls"}}]}),
        )
        .expect_err("an unlisted tool");
        assert!(
            error.contains("`shell` is not one of your tools"),
            "{error}"
        );
        assert!(!pending(&grant));
    }

    #[test]
    fn a_cli_server_prefix_is_stripped_from_a_tool_name() {
        for named in [
            "mcp__magician-plane__agentmail-send",
            "mcp__magician_plane__agentmail-send",
            "magician_plane.agentmail-send",
            "magician-plane:agentmail-send",
            "agentmail-send",
        ] {
            assert_eq!(without_server_prefix(named), "agentmail-send", "{named}");
        }
        assert_eq!(without_server_prefix("mcp__other__tool"), "other__tool");
    }
}
