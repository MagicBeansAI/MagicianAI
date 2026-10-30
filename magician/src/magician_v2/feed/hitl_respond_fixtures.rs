//! The HITL respond contract the clients pin: one Attention row per HITL kind.
//!
//! `magician/tests/fixtures/hitl/attention_rows.json` is read by the web
//! (`respondHandleFixture.test.ts`), iOS (`HitlRespondFixtureTests`) and
//! Android (`HitlRespondFixtureTest`) tests. Each entry is a row exactly as
//! `/api/magician/v2/feed/attention` serves it — built here by the same
//! builders the projections use (`build_attention_feed_item`, the taskless
//! `taskless_hitl_attention_row`) and stamped by the same
//! `stamp_feed_row_respond_handle` the feed endpoints apply — plus the call
//! a client must make: `expect.url` and the `expect.body` keys (`source`,
//! `input_type`, `execution_id`, `task_id`) beside `value: answer` and its
//! own `channel`.
//!
//! When the shape changes on purpose, regenerate with
//! `UPDATE_HITL_FIXTURES=1` and update the clients in the same change.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::magician_v2::{
    artifact_v2::{
        progress::{V3AttentionSummary, V3TaskProgressSummary},
        service::build_attention_feed_item,
    },
    feed::{materializer::taskless_hitl_attention_row, FeedItem, FeedItemType},
    hitl::stamp_feed_row_respond_handle,
    realtime_events::RuntimeTransportEvent,
};

const PRINCIPAL: &str = "owner";
const WORKSPACE: &str = "home";
const TASK: &str = "task_1";
const UPDATED_AT: &str = "2026-10-20T09:00:00Z";
const AT_MS: i64 = 1_792_486_800_000;
const PROPOSAL: &str = "ccp-6f1d2a90-3b7c-4e55-9a10-2f4c8d7e1b30";

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hitl/attention_rows.json")
}

fn check_fixture(value: &Value) {
    let path = fixture_path();
    if std::env::var_os("UPDATE_HITL_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("fixture dir");
        let rendered = format!(
            "{}\n",
            serde_json::to_string_pretty(value).expect("fixture json")
        );
        std::fs::write(&path, rendered).expect("write fixture");
        return;
    }
    let stored = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error} (regenerate with UPDATE_HITL_FIXTURES=1)",
            path.display()
        )
    });
    let stored: Value = serde_json::from_str(&stored).expect("stored fixture parses");
    assert_eq!(
        &stored, value,
        "attention_rows.json no longer matches what the server sends; regenerate with UPDATE_HITL_FIXTURES=1 and update the web, iOS and Android clients"
    );
}

fn task() -> V3TaskProgressSummary {
    serde_json::from_value(json!({
        "task_id": TASK,
        "principal": PRINCIPAL,
        "workspace": WORKSPACE,
        "ui_thread_id": "thread_1",
        "agent_id": "atlas",
        "status": "waiting",
        "summary": null,
        "active_root_execution_id": null,
        "latest_root_execution_id": null,
        "last_completed_root_execution_id": null,
        "updated_at": UPDATED_AT,
    }))
    .expect("task summary")
}

fn attention(fields: Value) -> V3AttentionSummary {
    let mut base = json!({
        "task_id": TASK,
        "detail": null,
        "entries": [],
        "item_count": 1,
        "updated_at": UPDATED_AT,
    });
    for (key, value) in fields.as_object().expect("attention fields") {
        base[key] = value.clone();
    }
    serde_json::from_value(base).expect("attention summary")
}

#[allow(clippy::too_many_arguments)]
fn taskless(
    correlation_id: &str,
    source: &str,
    input_type: &str,
    prompt: &str,
    hint: Option<&str>,
    input_schema: Value,
    execution_id: Option<&str>,
    agent_id: Option<&str>,
) -> FeedItem {
    let event = RuntimeTransportEvent::HitlRequested {
        correlation_id: correlation_id.to_string(),
        source: source.to_string(),
        input_type: input_type.to_string(),
        prompt: prompt.to_string(),
        hint: hint.map(str::to_string),
        input_schema: Some(input_schema),
        task_id: None,
        execution_id: execution_id.map(str::to_string),
        agent_id: agent_id.map(str::to_string),
        principal: Some(PRINCIPAL.to_string()),
        workspace: Some(WORKSPACE.to_string()),
        timestamp: AT_MS,
    };
    taskless_hitl_attention_row(&event)
        .expect("taskless row is projected")
        .4
}

fn stamped(mut item: FeedItem) -> FeedItem {
    stamp_feed_row_respond_handle(
        item.item_type == FeedItemType::Approval,
        item.task_id.as_deref(),
        &mut item.metadata,
    );
    item
}

struct Case {
    kind: &'static str,
    answer: Value,
    url: &'static str,
    body: Value,
    row: FeedItem,
}

fn cases() -> Vec<Case> {
    let task = task();
    let v3 = |fields: Value| stamped(build_attention_feed_item(&task, &attention(fields)));
    let briefing_options =
        json!([{"id": "acknowledge", "label": "Acknowledge", "requires_input": false}]);
    let reply_options = json!([{"id": "send", "label": "Send"}, {"id": "hold", "label": "Hold"}]);
    vec![
        Case {
            kind: "agentic_pause",
            answer: json!({"type": "text", "value": "The staging account"}),
            url: "/api/magician/v2/hitl/pause_agentic_1/respond",
            body: json!({"source": "agentic", "input_type": "text", "execution_id": "exec_agentic"}),
            row: v3(json!({
                "attention_id": "att_agentic",
                "attention_kind": "input.requested",
                "execution_id": "exec_agentic",
                "summary": "Input requested",
                "entries": ["Which account should I use?"],
                "pause_state_id": "pause_agentic_1",
                "input_type": "text",
                "input_schema": {"type": "text", "placeholder": "Account name"},
                "source": "agentic",
            })),
        },
        Case {
            kind: "user_request_choice",
            answer: json!({"type": "choice", "selected_id": "send"}),
            url: "/api/magician/v2/hitl/ur_reply_1/respond",
            body: json!({"source": "user_request", "input_type": "choice", "execution_id": "exec_reply"}),
            row: v3(json!({
                "attention_id": "att_user_choice",
                "attention_kind": "user_request.pending",
                "execution_id": "exec_reply",
                "summary": "1 pending user request(s).",
                "detail": "Send the reply to Dana?",
                "entries": ["Send the reply to Dana?"],
                "pause_state_id": "ur_reply_1",
                "input_type": "choice",
                "options": reply_options.clone(),
                "input_schema": {"options": reply_options},
                "source": "user_request",
            })),
        },
        // The owner briefing an Autopilot run files when it ends: the row that
        // rendered "No resolver available" when clients read only a flat
        // `request_id`.
        Case {
            kind: "user_request_acknowledge",
            answer: json!({"type": "choice", "selected_id": "acknowledge"}),
            url: "/api/magician/v2/hitl/vibedev-run-ended%3Atask_1%3Aexec_d278/respond",
            body: json!({"source": "user_request", "input_type": "choice", "execution_id": "exec_d278"}),
            row: v3(json!({
                "attention_id": "att_briefing",
                "attention_kind": "user_request.pending",
                "execution_id": "exec_d278",
                "summary": "1 pending user request(s).",
                "detail": "Autopilot run finished: Pi 1.3.0 live test.",
                "entries": ["Autopilot run finished: Pi 1.3.0 live test."],
                "pause_state_id": "vibedev-run-ended:task_1:exec_d278",
                "input_type": "choice",
                "options": briefing_options.clone(),
                "input_schema": {"options": briefing_options},
                "source": "user_request",
            })),
        },
        Case {
            kind: "diff_approval",
            answer: json!({"type": "choice", "selected_id": "apply"}),
            url: "/api/magician/v2/hitl/ccp-6f1d2a90-3b7c-4e55-9a10-2f4c8d7e1b30/respond",
            body: json!({"source": "diff_approval", "input_type": "diff_approval", "execution_id": "exec_code"}),
            row: v3(json!({
                "attention_id": "att_diff",
                "attention_kind": "input.requested",
                "execution_id": "exec_code",
                "summary": "Input requested",
                "entries": ["Apply the staged change to src/search.ts?"],
                "pause_state_id": PROPOSAL,
                "input_type": "diff_approval",
                "input_schema": {
                    "type": "diff_approval",
                    "proposal_id": PROPOSAL,
                    "approval_source": "proposal",
                    "files": [{
                        "path": "src/search.ts",
                        "status": "M",
                        "additions": 3,
                        "deletions": 1,
                        "unified_diff": "@@ -1 +1 @@\n-old\n+new\n"
                    }]
                },
                "source": "diff_approval",
            })),
        },
        Case {
            kind: "plan_approval",
            answer: json!({"type": "confirmation", "confirmed": true}),
            url: "/api/magician/v2/hitl/plan_7/respond",
            body: json!({
                "source": "plan_approval",
                "input_type": "confirmation",
                "execution_id": "planexec_1",
                "task_id": TASK
            }),
            row: v3(json!({
                "attention_id": "att_plan",
                "attention_kind": "hitl.requested",
                "execution_id": "planexec_1",
                "summary": "Plan ready for review",
                "entries": ["This task plan is ready for review."],
                "pause_state_id": "plan_7",
                "input_type": "confirmation",
                "input_schema": {"type": "confirmation", "plan_id": "plan_7", "task_id": TASK},
                "source": "plan_approval",
            })),
        },
        Case {
            kind: "clarification",
            answer: json!({"type": "text", "value": "The board"}),
            url: "/api/magician/v2/hitl/question_1/respond",
            body: json!({
                "source": "clarification",
                "input_type": "text",
                "execution_id": "planexec_1",
                "task_id": TASK
            }),
            row: v3(json!({
                "attention_id": "att_clarify",
                "attention_kind": "hitl.requested",
                "execution_id": "planexec_1",
                "summary": "Planning clarification",
                "detail": "Who is the audience?",
                "entries": ["Who is the audience?"],
                "pause_state_id": "question_1",
                "input_type": "text",
                "input_schema": {"type": "text"},
                "source": "clarification",
            })),
        },
        Case {
            kind: "approval",
            answer: json!({"type": "confirmation", "confirmed": true}),
            url: "/api/magician/v2/hitl/approval_9/respond",
            body: json!({"source": "approval", "input_type": "confirmation", "execution_id": "exec_mail"}),
            row: stamped(taskless(
                "approval_9",
                "approval",
                "confirmation",
                "Allow Atlas to send email?",
                Some("gmail.send to dana@example.com"),
                json!({"confirm_label": "Approve", "deny_label": "Reject"}),
                Some("exec_mail"),
                Some("atlas"),
            )),
        },
        Case {
            kind: "service_health",
            answer: json!({"type": "choice", "selected_id": "dismiss"}),
            url: "/api/magician/v2/hitl/service_health%3Aopenrouter%3Acredits/respond",
            body: json!({"source": "service_health", "input_type": "choice"}),
            row: stamped(taskless(
                "service_health:openrouter:credits",
                "service_health",
                "choice",
                "OpenRouter is out of credits",
                None,
                json!({
                    "type": "choice",
                    "options": [{"id": "dismiss", "label": "Dismiss"}],
                    "allow_other": false,
                    "service": "openrouter",
                    "health_issue": "credits"
                }),
                None,
                None,
            )),
        },
        Case {
            kind: "mcp_oauth",
            answer: json!({"type": "external_action_completed"}),
            url: "/api/magician/v2/hitl/mcp_oauth%3Alinear%3Aflow_1/respond",
            body: json!({"source": "mcp_oauth", "input_type": "external_action"}),
            row: stamped(taskless(
                "mcp_oauth:linear:flow_1",
                "mcp_oauth",
                "external_action",
                "Sign in to Linear",
                None,
                json!({"type": "external_action", "done_label": "I've finished"}),
                None,
                None,
            )),
        },
        Case {
            kind: "bot_auth",
            answer: json!({"type": "choice", "selected_id": "open_auth_flow"}),
            url: "/api/magician/v2/hitl/bot_auth%3Aowner%3Ahome%3Agmail/respond",
            body: json!({"source": "bot_auth", "input_type": "choice"}),
            row: stamped(taskless(
                "bot_auth:owner:home:gmail",
                "bot_auth",
                "choice",
                "Connect Gmail",
                Some("Sign in to continue"),
                json!({"type": "choice", "options": [{"id": "open_auth_flow", "label": "Connect"}]}),
                None,
                None,
            )),
        },
    ]
}

#[test]
fn every_hitl_kind_row_names_its_respond_call() {
    let mut rows = Vec::new();
    for case in cases() {
        let respond = &case.row.metadata["respond"];
        assert_eq!(respond["url"], case.url, "{}: respond url", case.kind);
        assert_eq!(respond["method"], "POST", "{}", case.kind);
        for key in ["source", "input_type", "execution_id", "task_id"] {
            assert_eq!(
                respond.get(key),
                case.body.get(key),
                "{}: respond.{key}",
                case.kind
            );
        }
        if let Some(target) = case
            .row
            .metadata
            .get("hitl_request")
            .filter(|v| v.is_object())
        {
            assert_eq!(
                &target["respond"], respond,
                "{}: the embedded open target carries the same handle",
                case.kind
            );
        }
        rows.push(json!({
            "kind": case.kind,
            "answer": case.answer,
            "expect": {"url": case.url, "body": case.body},
            "row": case.row,
        }));
    }
    check_fixture(&json!({ "rows": rows }));
}
