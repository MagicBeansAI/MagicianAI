//! Wire contract for opening a HITL prompt without consulting a feed projection.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HitlOpenIdentifiers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_state_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HitlOpenScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Source-specific workflow key used by clarification/plan responders.
    /// This is intentionally distinct from the durable runtime execution id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

/// Complete prompt payload carried by projections that are not the Attention
/// feed itself. `id` is the canonical `/hitl/{id}/respond` key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HitlOpenTarget {
    pub id: String,
    pub source: String,
    pub input_type: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default = "empty_object")]
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub identifiers: HitlOpenIdentifiers,
    #[serde(default)]
    pub scope: HitlOpenScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    /// How to answer this ask: the canonical respond call, built by
    /// [`hitl_respond_handle`]. Set by [`HitlOpenTarget::with_respond_handle`];
    /// absent from targets written by older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub respond: Option<HitlRespondHandle>,
}

impl HitlOpenTarget {
    /// The canonical respond handle for this target.
    pub fn respond_handle(&self) -> Option<HitlRespondHandle> {
        hitl_respond_handle(
            &self.source,
            &self.input_type,
            &self.identifiers,
            Some(self.id.as_str()),
            &self.scope,
            &self.input_schema,
        )
    }

    /// This target with its [`HitlRespondHandle`] attached. Every producer of
    /// an open target calls this, so no client derives the respond call itself.
    #[must_use]
    pub fn with_respond_handle(mut self) -> Self {
        self.respond = self.respond_handle();
        self
    }
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

// ─── Canonical respond handle ─────────────────────────────────────────────

/// The one respond route: `POST {HITL_RESPOND_PATH_PREFIX}{id}/respond`.
pub const HITL_RESPOND_PATH_PREFIX: &str = "/api/magician/v2/hitl/";

/// One option an ask offers, as the respond body names it (`selected_id`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HitlRespondOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requires_input: bool,
}

/// How to answer one HITL ask, decided by the server.
///
/// Every surface that offers an answer (Attention rows, `HitlRequested`
/// frames, open targets, run-completion notices and diff reviews) carries
/// this so a client posts exactly:
///
/// ```text
/// POST {url}
/// { "source": source, "input_type": input_type, "value": <answer>,
///   "channel": <client>, "execution_id"?: execution_id,
///   "task_id"?: task_id, "selected_paths"?: [...] }
/// ```
///
/// It is built only by [`hitl_respond_handle`], which encodes the id each
/// `respond_hitl_handler` source arm resolves; clients keep their own
/// per-source derivation only as a fallback for servers that predate it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HitlRespondHandle {
    /// `/api/magician/v2/hitl/{correlation_id}/respond`, id percent-encoded.
    pub url: String,
    /// Always `POST`.
    pub method: String,
    /// The id in the URL, unencoded: what `HitlResolved` names.
    pub correlation_id: String,
    /// The respond body's `source` (the handler's dispatch key).
    pub source: String,
    /// The respond body's `input_type`.
    pub input_type: String,
    /// The respond body's `execution_id` (for a delegated child's ask, the
    /// child's run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// The respond body's `task_id`: the planning responder for
    /// `clarification` / `plan_approval`; absent for every other source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The choices the ask offers (`value.selected_id`), when it offers any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<HitlRespondOption>,
    /// Statuses that mean "already answered or gone": drop the card, tell
    /// the reader their answer did not land, do not report a failure.
    pub soft_success_statuses: Vec<u16>,
}

/// The source the respond handler dispatches on, or `None` for one it does
/// not accept. `primitive` / `inner_loop` are legacy spellings of `agentic`.
pub fn canonical_hitl_respond_source(source: &str) -> Option<&'static str> {
    Some(match source.trim() {
        "agentic" | "primitive" | "inner_loop" => "agentic",
        "escalation" => "escalation",
        "user_request" => "user_request",
        "approval" => "approval",
        "plan_approval" => "plan_approval",
        "clarification" => "clarification",
        "diff_approval" => "diff_approval",
        "service_health" => "service_health",
        "mcp_oauth" => "mcp_oauth",
        "bot_auth" => "bot_auth",
        _ => return None,
    })
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn schema_str<'a>(schema: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    non_empty(schema.get(key).and_then(serde_json::Value::as_str))
}

/// `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` is
/// percent-encoded as UTF-8.
pub fn encode_hitl_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The respond URL for `correlation_id`.
pub fn hitl_respond_url(correlation_id: &str) -> String {
    format!(
        "{}{}/respond",
        HITL_RESPOND_PATH_PREFIX,
        encode_hitl_path_segment(correlation_id)
    )
}

fn respond_options(schema: &serde_json::Value) -> Vec<HitlRespondOption> {
    schema
        .get("options")
        .and_then(serde_json::Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let id = schema_str(option, "id").or_else(|| schema_str(option, "value"))?;
                    let label = schema_str(option, "label").unwrap_or(id);
                    Some(HitlRespondOption {
                        id: id.to_string(),
                        label: label.to_string(),
                        requires_input: option
                            .get("requires_input")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Build the canonical respond handle for an ask.
///
/// This is the single place that knows which id each `respond_hitl_handler`
/// source arm resolves:
///
/// | source | URL id |
/// |---|---|
/// | `agentic` / `escalation` | `correlation_id` (revision-bound), else `pause_state_id` |
/// | `user_request` | `request_id`, else `correlation_id`, else `pause_state_id` |
/// | `approval` | `approval_id`, else `correlation_id` |
/// | `diff_approval` | `input_schema.proposal_id` / `transaction_id`, else `correlation_id` / `pause_state_id` |
/// | others | `correlation_id` |
///
/// `fallback_id` (an open target's `id`) is used when no identifier names
/// the source's own key. `None` when the source is not one the handler
/// accepts, the input type is empty, or no id is known.
pub fn hitl_respond_handle(
    source: &str,
    input_type: &str,
    identifiers: &HitlOpenIdentifiers,
    fallback_id: Option<&str>,
    scope: &HitlOpenScope,
    input_schema: &serde_json::Value,
) -> Option<HitlRespondHandle> {
    let source = canonical_hitl_respond_source(source)?;
    let declared_input_type = non_empty(Some(input_type))
        .or_else(|| schema_str(input_schema, "type"))
        .map(str::to_string);
    let input_type = match source {
        // The approval arm reads a confirmation (or an approve/reject choice).
        "approval" => match declared_input_type.as_deref() {
            Some("choice") => "choice".to_string(),
            _ => "confirmation".to_string(),
        },
        "diff_approval" => "diff_approval".to_string(),
        // A sign-in is only ever finished in the browser.
        "mcp_oauth" => "external_action".to_string(),
        _ => declared_input_type?,
    };
    let correlation = non_empty(identifiers.correlation_id.as_deref());
    let pause = non_empty(identifiers.pause_state_id.as_deref());
    let request = non_empty(identifiers.request_id.as_deref());
    let approval = non_empty(identifiers.approval_id.as_deref());
    let fallback = non_empty(fallback_id);
    let correlation_id = match source {
        "agentic" | "escalation" => correlation.or(pause).or(fallback),
        "user_request" => request.or(correlation).or(pause).or(fallback),
        "approval" => approval.or(correlation).or(fallback),
        "diff_approval" => schema_str(input_schema, "proposal_id")
            .or_else(|| schema_str(input_schema, "transaction_id"))
            .or(correlation)
            .or(pause)
            .or(fallback),
        _ => correlation.or(fallback),
    }?
    .to_string();
    let task_id = matches!(source, "clarification" | "plan_approval")
        .then(|| {
            non_empty(scope.task_id.as_deref())
                .or(non_empty(scope.workflow_id.as_deref()))
                .map(str::to_string)
        })
        .flatten();
    Some(HitlRespondHandle {
        url: hitl_respond_url(&correlation_id),
        method: "POST".to_string(),
        correlation_id,
        source: source.to_string(),
        input_type,
        execution_id: non_empty(scope.execution_id.as_deref()).map(str::to_string),
        task_id,
        options: respond_options(input_schema),
        soft_success_statuses: if matches!(source, "agentic" | "escalation") {
            vec![404, 409, 410]
        } else {
            vec![409]
        },
    })
}

/// The handle for a `HitlRequested` event's fields (the `RuntimeTransportEvent`
/// variant; its `correlation_id` is the canonical id).
pub fn hitl_requested_respond_handle(
    correlation_id: &str,
    source: &str,
    input_type: &str,
    input_schema: Option<&serde_json::Value>,
    task_id: Option<&str>,
    execution_id: Option<&str>,
) -> Option<HitlRespondHandle> {
    let empty = empty_object();
    hitl_respond_handle(
        source,
        input_type,
        &HitlOpenIdentifiers {
            correlation_id: Some(correlation_id.to_string()),
            ..Default::default()
        },
        None,
        &HitlOpenScope {
            task_id: task_id.map(str::to_string),
            execution_id: execution_id.map(str::to_string),
            ..Default::default()
        },
        input_schema.unwrap_or(&empty),
    )
}

fn value_identifiers(record: &serde_json::Value) -> HitlOpenIdentifiers {
    let read = |key: &str| schema_str(record, key).map(str::to_string);
    HitlOpenIdentifiers {
        pause_state_id: read("pause_state_id"),
        approval_id: read("approval_id"),
        correlation_id: read("correlation_id"),
        request_id: read("request_id"),
    }
}

/// The source a feed row without an embedded open target names by its
/// `attention_kind`, for rows written before `metadata.source` existed.
fn attention_kind_source(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "input.requested" | "waiting_for_confirmation" => "agentic",
        "user_request.pending" => "user_request",
        "max_iterations_reached" => "escalation",
        "diff_approval" => "diff_approval",
        "clarification" => "clarification",
        "plan_approval" => "plan_approval",
        _ => return None,
    })
}

/// The handle for a feed / Attention row, read from its `metadata`.
///
/// The embedded open target (`metadata.hitl_request`) is authoritative when
/// present; flat metadata (`source`, `attention_kind`, the id aliases,
/// `execution_id`) answers for rows written without one. `None` for a row
/// that is not an answerable ask (a stalled wait, a failure, a running row).
pub fn feed_row_respond_handle(
    item_is_approval: bool,
    row_task_id: Option<&str>,
    metadata: &serde_json::Value,
) -> Option<HitlRespondHandle> {
    if !metadata.is_object()
        || schema_str(metadata, "attention_reason") == Some("waiting_without_question")
    {
        return None;
    }
    let empty = empty_object();
    let flat_schema = metadata
        .get("input_schema")
        .filter(|schema| schema.is_object())
        .unwrap_or(&empty);
    if let Some(target) = metadata
        .get("hitl_request")
        .filter(|target| target.is_object())
    {
        let scope = target.get("scope").filter(|scope| scope.is_object());
        let scope_str = |key: &str| scope.and_then(|scope| schema_str(scope, key));
        let embedded_schema = target
            .get("input_schema")
            .filter(|schema| {
                schema.is_object() && !schema.as_object().is_some_and(|map| map.is_empty())
            })
            .unwrap_or(flat_schema);
        let mut identifiers = target
            .get("identifiers")
            .filter(|ids| ids.is_object())
            .map(value_identifiers)
            .unwrap_or_default();
        // The flat C2 spelling names `correlation_id` beside the target.
        if identifiers.correlation_id.is_none() {
            identifiers.correlation_id = schema_str(target, "correlation_id").map(str::to_string);
        }
        let source = if item_is_approval {
            Some("approval")
        } else {
            schema_str(target, "source").or_else(|| schema_str(metadata, "source"))
        };
        let input_type = schema_str(target, "input_type")
            .or_else(|| schema_str(embedded_schema, "type"))
            .or_else(|| schema_str(metadata, "input_type"))
            .unwrap_or_default();
        let task_id = scope_str("task_id")
            .or(row_task_id)
            .or_else(|| schema_str(metadata, "task_id"));
        let handle = source.and_then(|source| {
            hitl_respond_handle(
                source,
                input_type,
                &identifiers,
                schema_str(target, "id"),
                &HitlOpenScope {
                    workflow_id: scope_str("workflow_id").map(str::to_string),
                    task_id: task_id.map(str::to_string),
                    execution_id: scope_str("execution_id")
                        .or_else(|| schema_str(target, "execution_id"))
                        .or_else(|| schema_str(metadata, "execution_id"))
                        .map(str::to_string),
                    ..Default::default()
                },
                embedded_schema,
            )
        });
        if handle.is_some() {
            return handle;
        }
    }
    let attention_kind = schema_str(metadata, "attention_kind");
    let source = if item_is_approval {
        "approval"
    } else if let Some(source) = schema_str(metadata, "source") {
        source
    } else {
        attention_kind.and_then(attention_kind_source)?
    };
    if !item_is_approval && attention_kind.is_none() {
        return None;
    }
    let identifiers = value_identifiers(metadata);
    if identifiers == HitlOpenIdentifiers::default() {
        return None;
    }
    let input_type = schema_str(flat_schema, "type")
        .or_else(|| schema_str(metadata, "input_type"))
        .or_else(|| {
            matches!(
                attention_kind,
                Some("waiting_for_confirmation" | "max_iterations_reached")
            )
            .then_some("confirmation")
        })
        .unwrap_or_default();
    let mut schema = flat_schema.clone();
    if let (Some(record), Some(options)) = (schema.as_object_mut(), metadata.get("options")) {
        record
            .entry("options".to_string())
            .or_insert_with(|| options.clone());
    }
    for key in ["proposal_id", "transaction_id"] {
        if let (Some(record), Some(value)) = (schema.as_object_mut(), metadata.get(key)) {
            record
                .entry(key.to_string())
                .or_insert_with(|| value.clone());
        }
    }
    hitl_respond_handle(
        source,
        input_type,
        &identifiers,
        None,
        &HitlOpenScope {
            workflow_id: schema_str(metadata, "workflow_id").map(str::to_string),
            task_id: row_task_id
                .or_else(|| schema_str(metadata, "task_id"))
                .map(str::to_string),
            execution_id: schema_str(metadata, "execution_id").map(str::to_string),
            ..Default::default()
        },
        &schema,
    )
}

/// Stamp a feed / Attention row's `metadata.respond` (and the embedded
/// `metadata.hitl_request.respond`) with [`feed_row_respond_handle`], or
/// remove a stale one when the row is not answerable. Applied to every row
/// the feed endpoints serve, persisted rows included.
pub fn stamp_feed_row_respond_handle(
    item_is_approval: bool,
    row_task_id: Option<&str>,
    metadata: &mut serde_json::Value,
) {
    let handle = feed_row_respond_handle(item_is_approval, row_task_id, metadata)
        .and_then(|handle| serde_json::to_value(handle).ok());
    let Some(record) = metadata.as_object_mut() else {
        return;
    };
    match handle {
        Some(handle) => {
            if let Some(target) = record
                .get_mut("hitl_request")
                .and_then(serde_json::Value::as_object_mut)
            {
                target.insert("respond".to_string(), handle.clone());
            }
            record.insert("respond".to_string(), handle);
        },
        None => {
            record.remove("respond");
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_roundtrips_as_the_frontend_open_contract() {
        let target = HitlOpenTarget {
            id: "question-7".to_string(),
            source: "clarification".to_string(),
            input_type: "choice".to_string(),
            prompt: "Which release channel?".to_string(),
            hint: None,
            input_schema: serde_json::json!({
                "options": [{"id": "stable", "label": "Stable"}]
            }),
            identifiers: HitlOpenIdentifiers {
                correlation_id: Some("question-7".to_string()),
                ..Default::default()
            },
            scope: HitlOpenScope {
                principal: Some("principal-a".to_string()),
                workspace: Some("workspace-a".to_string()),
                workflow_id: Some("task-4".to_string()),
                task_id: Some("task-4".to_string()),
                execution_id: Some("planexec-4".to_string()),
                ..Default::default()
            },
            at: Some(7),
            respond: None,
        }
        .with_respond_handle();

        let value = serde_json::to_value(&target).expect("serialize target");
        assert_eq!(value["id"], "question-7");
        assert_eq!(value["scope"]["principal"], "principal-a");
        assert_eq!(value["scope"]["workspace"], "workspace-a");
        assert_eq!(value["scope"]["workflow_id"], "task-4");
        assert_eq!(value["scope"]["execution_id"], "planexec-4");
        assert_eq!(value["input_schema"]["options"][0]["id"], "stable");
        assert_eq!(
            serde_json::from_value::<HitlOpenTarget>(value).expect("deserialize target"),
            target
        );
    }
    #[test]
    fn a_user_request_named_only_by_its_pause_id_still_has_a_respond_handle() {
        // The owner-briefing row that rendered "No resolver available":
        // flat `pause_state_id`, the request id only inside the envelope.
        let metadata = serde_json::json!({
            "attention_kind": "user_request.pending",
            "source": "user_request",
            "pause_state_id": "run-ended:task_1:exec_1",
            "input_type": "choice",
            "execution_id": "exec_1",
            "hitl_request": {
                "id": "run-ended:task_1:exec_1",
                "source": "user_request",
                "input_type": "choice",
                "prompt": "Run finished",
                "input_schema": {"options": [{"id": "acknowledge", "label": "Acknowledge"}]},
                "identifiers": {"request_id": "run-ended:task_1:exec_1"},
                "scope": {"task_id": "task_1", "execution_id": "exec_1"}
            }
        });
        let handle = feed_row_respond_handle(false, Some("task_1"), &metadata).expect("handle");
        assert_eq!(
            handle.url,
            "/api/magician/v2/hitl/run-ended%3Atask_1%3Aexec_1/respond"
        );
        assert_eq!(handle.source, "user_request");
        assert_eq!(handle.input_type, "choice");
        assert_eq!(handle.options[0].id, "acknowledge");
        assert_eq!(handle.task_id, None, "only planning sources send task_id");
        assert_eq!(handle.soft_success_statuses, vec![409]);

        // The same row without the envelope (an older projection).
        let mut flat = metadata.clone();
        flat.as_object_mut().unwrap().remove("hitl_request");
        let flat = feed_row_respond_handle(false, Some("task_1"), &flat).expect("flat handle");
        assert_eq!(flat.correlation_id, "run-ended:task_1:exec_1");
    }

    #[test]
    fn source_rules_pick_the_id_each_respond_arm_resolves() {
        let scope = HitlOpenScope {
            task_id: Some("task-1".into()),
            execution_id: Some("exec-1".into()),
            ..Default::default()
        };
        let ids = HitlOpenIdentifiers {
            correlation_id: Some("pause-1~0123456789ab".into()),
            pause_state_id: Some("pause-1".into()),
            ..Default::default()
        };
        let agentic = hitl_respond_handle(
            "inner_loop",
            "text",
            &ids,
            None,
            &scope,
            &serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(agentic.source, "agentic");
        assert_eq!(agentic.correlation_id, "pause-1~0123456789ab");
        assert_eq!(agentic.execution_id.as_deref(), Some("exec-1"));
        assert_eq!(agentic.soft_success_statuses, vec![404, 409, 410]);

        let diff = hitl_respond_handle(
            "diff_approval",
            "",
            &HitlOpenIdentifiers {
                correlation_id: Some("ccp-1".into()),
                ..Default::default()
            },
            None,
            &scope,
            &serde_json::json!({"proposal_id": "ccp-1"}),
        )
        .unwrap();
        assert_eq!(diff.input_type, "diff_approval");
        assert_eq!(diff.correlation_id, "ccp-1");

        let plan = hitl_respond_handle(
            "plan_approval",
            "confirmation",
            &HitlOpenIdentifiers {
                correlation_id: Some("plan-1".into()),
                ..Default::default()
            },
            None,
            &scope,
            &serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(plan.task_id.as_deref(), Some("task-1"));

        assert!(hitl_respond_handle(
            "unknown",
            "text",
            &ids,
            None,
            &scope,
            &serde_json::json!({})
        )
        .is_none());
    }

    #[test]
    fn a_taskless_approval_row_answers_by_its_correlation_id() {
        let metadata = serde_json::json!({
            "attention_kind": "hitl.requested",
            "source": "approval",
            "input_type": "confirmation",
            "correlation_id": "approval-9",
            "request_id": "approval-9",
            "taskless": true
        });
        let handle = feed_row_respond_handle(true, None, &metadata).unwrap();
        assert_eq!(handle.correlation_id, "approval-9");
        assert_eq!(handle.input_type, "confirmation");
    }

    #[test]
    fn a_stalled_wait_is_not_answerable_and_loses_a_stale_handle() {
        let mut metadata = serde_json::json!({
            "attention_kind": "waiting_without_question",
            "attention_reason": "waiting_without_question",
            "pause_state_id": "p",
            "respond": {"url": "stale"}
        });
        stamp_feed_row_respond_handle(false, Some("t"), &mut metadata);
        assert!(metadata.get("respond").is_none());
    }

    #[test]
    fn path_segments_encode_like_encode_uri_component() {
        assert_eq!(
            encode_hitl_path_segment("bot_auth:p a:w/s:x~y(1)"),
            "bot_auth%3Ap%20a%3Aw%2Fs%3Ax~y(1)"
        );
    }
}
