//! Agent-facing access to durable research working sets.
//!
//! Creation is deliberately not a public tool. Only the content-read controller
//! may materialize evidence, after it has returned a validated document. Search
//! and chunk reads remain generic scoped tools so any research workflow can
//! consume a handoff without depending on the web-researcher implementation.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use crate::magician_v2::{
    artifact_v2::{
        workspace::ArtifactV2Workspace, CreateWorkingSetRequest, WorkingSetExecutionIndex,
        WorkingSetSourceInput, WorkingSetStore,
    },
    content_sources::{
        ContentDocument, ContentPrivacy, WorkingSetActivationDecision, WorkingSetActivationProbe,
        WorkingSetActivationSettings, WorkingSetCaptureSettings,
    },
    execution::{agent_resources::AgentResources, error::ExecutionError},
};

use super::shared::require_scope_str;

const DEFAULT_AUTO_CAPTURE_WORKING_SET_TITLE: &str = "Research evidence";
const MAX_WORKING_SET_TITLE_CHARS: usize = 240;
static WORKING_SET_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ContentReadCaptureRequest {
    title: String,
    principal: String,
    workspace: String,
    created_by: String,
    /// The execution this read belongs to. Without it a capture is a snapshot
    /// with no task to accrue on, and routing has nothing to decide about.
    execution_id: Option<String>,
    agent_id: Option<String>,
}

/// The model-facing head of a page once an execution has taken the
/// working-set path. Short on purpose: the page is in the working set, the
/// model has the index and a search, and the head is for telling what was
/// opened — a title and a first paragraph — not for reading it. Six hundred
/// was too short for that in the first live A/B; the researcher could not
/// tell one page from another and stopped opening them. Below the threshold
/// the ordinary 6,000-character window stays.
pub(crate) const ACTIVATED_PAGE_EXCERPT_CHARS: usize = 1_200;

/// Resolve the server-owned capture request before retrieval begins. Deployment
/// policy selects automatic-capture agents; other callers opt in with a title,
/// keeping this capability reusable without coupling every reader to research
/// retention.
pub fn content_read_capture_request(
    args: &Value,
    capture_policy: &WorkingSetCaptureSettings,
) -> Result<Option<ContentReadCaptureRequest>, ExecutionError> {
    let requested_title = match args.get("working_set_title") {
        None => None,
        Some(Value::String(title)) => Some(title.trim()),
        Some(_) => {
            return Err(ExecutionError::Step(
                "content_read `working_set_title` must be a string".into(),
            ));
        },
    };
    let is_auto_capture_agent = args
        .get("__agent_id")
        .and_then(Value::as_str)
        .is_some_and(|agent_id| capture_policy.automatically_captures(agent_id));
    let Some(title) =
        requested_title.or(is_auto_capture_agent.then_some(DEFAULT_AUTO_CAPTURE_WORKING_SET_TITLE))
    else {
        return Ok(None);
    };
    if title.is_empty() || title.chars().count() > MAX_WORKING_SET_TITLE_CHARS {
        return Err(ExecutionError::Step(format!(
            "content_read `working_set_title` must contain 1-{MAX_WORKING_SET_TITLE_CHARS} characters"
        )));
    }
    let principal = require_scope_str(args, "__principal", "content_read")?;
    let workspace = require_scope_str(args, "__workspace", "content_read")?;
    let created_by = args
        .get("__agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|agent_id| !agent_id.is_empty())
        .unwrap_or("content-read")
        .to_string();
    let optional_scope = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Ok(Some(ContentReadCaptureRequest {
        title: title.to_string(),
        principal,
        workspace,
        created_by,
        execution_id: optional_scope("__execution_id"),
        agent_id: optional_scope("__agent_id"),
    }))
}

/// Add a bounded, durable reference without changing the already-successful
/// content-read result. Capture failure is observable but never turns a usable
/// retrieval into a failed one.
pub async fn materialize_content_read_result(
    workspace: ArtifactV2Workspace,
    capture: Option<ContentReadCaptureRequest>,
    activation: &WorkingSetActivationSettings,
    result: &mut Value,
) {
    let Some(capture) = capture else {
        return;
    };
    let documents = eligible_documents(result);
    if documents.is_empty() {
        attach_capture_status(
            result,
            json!({
                "status": "empty",
                "title": capture.title,
                "source_count": 0,
            }),
        );
        return;
    }
    let working_set_id = generated_working_set_id(&capture, &documents);
    let sources = documents
        .into_iter()
        .enumerate()
        .map(|(index, document)| WorkingSetSourceInput {
            source_id: format!("source-{}", index + 1),
            document,
        })
        .collect();
    let store = WorkingSetStore::new(workspace);
    let materialized = store
        .create(
            &capture.principal,
            &capture.workspace,
            CreateWorkingSetRequest {
                working_set_id,
                title: capture.title.clone(),
                created_by: capture.created_by.clone(),
                sources,
            },
        )
        .await;
    match materialized {
        Ok(manifest) => {
            attach_capture_status(
                result,
                json!({
                    "status": "created",
                    "working_set_id": manifest.working_set_id,
                    "title": manifest.title,
                    "source_count": manifest.sources.len(),
                    "chunk_count": manifest.chunks.len(),
                }),
            );
            // Boundary B. The capture accrues on the execution's index, the
            // rule decides from what has accrued, and the decision — with the
            // sentence that justified it — is written on the result so it can
            // be read back. Only an activated execution changes what the
            // model sees.
            if let Some(execution_id) = capture.execution_id.as_deref() {
                match store
                    .record_execution_capture(
                        &capture.principal,
                        &capture.workspace,
                        execution_id,
                        &manifest,
                        super::content_read::OPENED_PAGE_EXCERPT_CHARS as u64,
                    )
                    .await
                {
                    Ok(index) => {
                        let (routing, index) =
                            route_execution(&store, &capture, activation, index).await;
                        let activated = routing
                            .get("activated")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        attach_routing(result, routing);
                        if activated && apply_activated_projection(result, &index) {
                            if let Err(error) = store
                                .mark_read_narrowed(
                                    &capture.principal,
                                    &capture.workspace,
                                    execution_id,
                                )
                                .await
                            {
                                tracing::warn!(
                                    execution_id,
                                    error = %error,
                                    "a narrowed read could not be counted on the execution index"
                                );
                            }
                        }
                    },
                    Err(error) => {
                        tracing::warn!(
                            principal = %capture.principal,
                            workspace = %capture.workspace,
                            execution_id,
                            error = %error,
                            "working-set capture succeeded but its execution index was unavailable"
                        );
                    },
                }
            }
        },
        Err(error) => {
            tracing::warn!(
                principal = %capture.principal,
                workspace = %capture.workspace,
                error = %error,
                "content read completed but working-set materialization was unavailable"
            );
            attach_capture_status(
                result,
                json!({
                    "status": "unavailable",
                    "title": capture.title,
                    "reason": "durable evidence storage was unavailable",
                }),
            );
        },
    }
}

pub async fn search(
    resources: std::sync::Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "working_set_search")?;
    let workspace = require_scope_str(&args, "__workspace", "working_set_search")?;
    let query = required_string(&args, "query", "working_set_search")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(10);
    let store = WorkingSetStore::new(resources.artifact_workspace.clone());
    let working_set_id = args
        .get("working_set_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(working_set_id) = working_set_id {
        let matches = store
            .search(&principal, &workspace, working_set_id, &query, limit)
            .await
            .map_err(|error| ExecutionError::Step(format!("working-set search failed: {error}")))?;
        return Ok(json!({
            "status": "complete",
            "working_set_id": working_set_id,
            "matches": matches,
        }));
    }
    // No id: search everything this execution captured. The model never has
    // to know which of its reads a fact landed in.
    let Some(execution_id) = args
        .get("__execution_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(ExecutionError::Step(
            "working_set_search needs a `working_set_id`, or an execution to search across".into(),
        ));
    };
    let found = store
        .search_execution(&principal, &workspace, execution_id, &query, limit)
        .await
        .map_err(|error| ExecutionError::Step(format!("working-set search failed: {error}")))?;
    Ok(json!({
        "status": "complete",
        "execution_id": found.execution_id,
        "members_searched": found.members_searched,
        "members_evicted": found.members_evicted,
        "matches": found.matches,
    }))
}

pub async fn read(
    resources: std::sync::Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "working_set_read")?;
    let workspace = require_scope_str(&args, "__workspace", "working_set_read")?;
    let working_set_id = required_string(&args, "working_set_id", "working_set_read")?;
    let source_id = required_string(&args, "source_id", "working_set_read")?;
    let chunk_index = args
        .get("chunk_index")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            ExecutionError::Step("working_set_read requires `chunk_index` as a u32".into())
        })?;
    let chunk = WorkingSetStore::new(resources.artifact_workspace.clone())
        .read_chunk(
            &principal,
            &workspace,
            &working_set_id,
            &source_id,
            chunk_index,
        )
        .await
        .map_err(|error| ExecutionError::Step(format!("working-set read failed: {error}")))?;
    Ok(json!({"status": "complete", "chunk": chunk}))
}

fn required_string(args: &Value, key: &str, tool_name: &str) -> Result<String, ExecutionError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ExecutionError::Step(format!("{tool_name} requires `{key}`")))
}

fn eligible_documents(result: &Value) -> Vec<ContentDocument> {
    let values: Vec<&Value> = result
        .get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|branch| branch.get("result"))
                .collect()
        })
        .unwrap_or_else(|| vec![result]);
    values
        .into_iter()
        .filter(|value| value.get("claim_eligible").and_then(Value::as_bool) == Some(true))
        .filter_map(|value| value.get("document"))
        .filter_map(|document| serde_json::from_value::<ContentDocument>(document.clone()).ok())
        .filter(|document| {
            document.validate().is_ok() && document.privacy == ContentPrivacy::Public
        })
        .collect()
}

fn generated_working_set_id(
    capture: &ContentReadCaptureRequest,
    documents: &[ContentDocument],
) -> String {
    let sequence = WORKING_SET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let source_hashes = documents
        .iter()
        .map(|document| document.content_hash.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let seed = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        capture.principal,
        capture.workspace,
        capture.created_by,
        capture.title,
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        format!("{sequence}\n{source_hashes}"),
    );
    let hash = blake3::hash(seed.as_bytes()).to_hex().to_string();
    format!("ws-{}", &hash[..24])
}

/// Decide, or keep the decision already made. Sticky per execution: once the
/// path is open a later read stays on it even if no threshold is crossed any
/// more, because the point is a stable way of working, not a per-call flicker.
async fn route_execution(
    store: &WorkingSetStore,
    capture: &ContentReadCaptureRequest,
    activation: &WorkingSetActivationSettings,
    index: WorkingSetExecutionIndex,
) -> (Value, WorkingSetExecutionIndex) {
    if let Some(existing) = index.activation.as_ref() {
        let routing = json!({
            "lane": existing.lane,
            "activated": true,
            "sticky": true,
            "reason": existing.reason,
            "activated_at": existing.activated_at,
        });
        return (routing, index);
    }
    let Some(lane) = capture
        .agent_id
        .as_deref()
        .and_then(|agent_id| activation.lane_for_agent(agent_id))
    else {
        let reason = "the reading agent is in no working-set lane";
        let index = keep_decision(store, capture, index, None, false, false, reason).await;
        let routing = json!({
            "lane": Value::Null,
            "activated": false,
            "sticky": false,
            "reason": reason,
        });
        return (routing, index);
    };
    let gate_open = activation.lane_is_open(lane);
    let decision = activation.decide(&WorkingSetActivationProbe {
        lane,
        total_source_bytes: index.total_source_bytes,
        beyond_window_bytes: index.beyond_window_bytes,
    });
    match decision {
        WorkingSetActivationDecision::Activate { reason } => {
            match store
                .activate_execution(
                    &capture.principal,
                    &capture.workspace,
                    &index.execution_id,
                    lane,
                    &reason,
                )
                .await
            {
                Ok(index) => {
                    let index =
                        keep_decision(store, capture, index, Some(lane), true, true, &reason).await;
                    let routing = json!({
                        "lane": lane,
                        "activated": true,
                        "sticky": false,
                        "reason": reason,
                    });
                    (routing, index)
                },
                Err(error) => {
                    tracing::warn!(
                        execution_id = %index.execution_id,
                        error = %error,
                        "working-set routing decided to activate but the index could not record it"
                    );
                    let routing = json!({
                        "lane": lane,
                        "activated": false,
                        "sticky": false,
                        "reason": format!("activation could not be recorded: {error}"),
                    });
                    (routing, index)
                },
            }
        },
        WorkingSetActivationDecision::Stay { reason } => {
            let index =
                keep_decision(store, capture, index, Some(lane), gate_open, false, &reason).await;
            let routing = json!({
                "lane": lane,
                "activated": false,
                "sticky": false,
                "reason": reason,
            });
            (routing, index)
        },
    }
}

/// Write the decision on the index so it can be read back either way; a
/// refusal that cannot be recorded is logged and the read goes on — the
/// decision itself is already on the result.
async fn keep_decision(
    store: &WorkingSetStore,
    capture: &ContentReadCaptureRequest,
    index: WorkingSetExecutionIndex,
    lane: Option<&str>,
    gate_open: bool,
    activated: bool,
    reason: &str,
) -> WorkingSetExecutionIndex {
    match store
        .record_routing_decision(
            &capture.principal,
            &capture.workspace,
            &index.execution_id,
            lane,
            gate_open,
            activated,
            reason,
        )
        .await
    {
        Ok(index) => index,
        Err(error) => {
            tracing::warn!(
                execution_id = %index.execution_id,
                error = %error,
                "working-set routing decision could not be kept on the index"
            );
            index
        },
    }
}

fn attach_routing(result: &mut Value, routing: Value) {
    if let Some(working_set) = result
        .as_object_mut()
        .and_then(|object| object.get_mut("working_set"))
        .and_then(Value::as_object_mut)
    {
        working_set.insert("routing".into(), routing);
    }
}

/// What the model gets from a read once the execution is on the working-set
/// path: a short head of the page, the task's index, and how to search it.
/// The raw record — `document`, its text, its content hash — is untouched;
/// the projection is the only thing that narrows, in two places. The
/// `excerpt` field is cut here. The full `document.text`, which the
/// content-read contract projects a head of on its own, is capped through a
/// projection hint the projector honours at excerpt paths — the record cannot
/// be shortened without breaking its hash, so the projection is asked to.
///
/// Only a page larger than the ORDINARY window is narrowed — strictly
/// non-lossy: the model never sees less of a page than it would have seen
/// without routing. The first live A/B cut a 3.2 KB page to a head when the
/// whole page would have fit the 6,000-character window; that took away
/// content and gave nothing back, because the working set held nothing the
/// window did not already show. Narrowing pays only when it holds more.
///
/// The same rule governs what the result SAYS. The index and the guidance
/// ride only a read that was narrowed, or any read after one: until then
/// the set holds nothing the window did not show, and a steer with nothing
/// behind it is a cost with no return.
///
/// Returns whether any read was narrowed, so the index can count it and the
/// prompt notice can wait for the first one.
fn apply_activated_projection(result: &mut Value, index: &WorkingSetExecutionIndex) -> bool {
    let page_exceeds_cap = |record: &Value| {
        record
            .get("document")
            .and_then(|document| document.get("text"))
            .and_then(Value::as_str)
            .is_some_and(|text| {
                text.chars().count() > super::content_read::OPENED_PAGE_EXCERPT_CHARS
            })
    };
    let bound = |record: &mut Value| -> bool {
        if !page_exceeds_cap(record) {
            return false;
        }
        let Some(object) = record.as_object_mut() else {
            return false;
        };
        if let Some(Value::String(excerpt)) = object.get("excerpt") {
            if excerpt.chars().count() > ACTIVATED_PAGE_EXCERPT_CHARS {
                let (bounded, _) = super::content_batch::bounded_text_excerpt(
                    excerpt,
                    ACTIVATED_PAGE_EXCERPT_CHARS,
                );
                object.insert("excerpt".into(), json!(bounded));
                object.insert("excerpt_complete".into(), json!(false));
            }
        }
        object.insert("excerpt_bounded_by_routing".into(), json!(true));
        true
    };
    let mut any_bounded = false;
    if let Some(branches) = result.get_mut("results").and_then(Value::as_array_mut) {
        for branch in branches.iter_mut() {
            if let Some(record) = branch.get_mut("result") {
                any_bounded |= bound(record);
            }
        }
    } else {
        any_bounded |= bound(result);
    }
    if any_bounded {
        if let Some(object) = result.as_object_mut() {
            object.insert(
                crate::magician_v2::tool_result_projection::PROJECTION_HINTS_FIELD.into(),
                json!({ "excerpt_max_bytes": ACTIVATED_PAGE_EXCERPT_CHARS }),
            );
        }
    }
    // The decision is recorded on every read (`routing`); the steer is not.
    // Until a read has been narrowed the working set holds nothing the
    // ordinary window did not already show, and telling the model its pages
    // are routed then only sends it searching for what is in front of it —
    // the open-baseline A/B turned search-heavy and read-light on exactly
    // that. Once one read is narrowed, every later read in the task says so.
    if !any_bounded && index.narrowed_reads == 0 {
        return false;
    }
    let members: Vec<Value> = index
        .members
        .iter()
        .map(|member| {
            json!({
                "working_set_id": member.working_set_id,
                "title": member.title,
                "source_count": member.source_count,
                "source_bytes": member.source_bytes,
            })
        })
        .collect();
    if let Some(working_set) = result
        .as_object_mut()
        .and_then(|object| object.get_mut("working_set"))
        .and_then(Value::as_object_mut)
    {
        working_set.insert(
            "execution_index".into(),
            json!({
                "execution_id": index.execution_id,
                "members": members,
                "total_source_bytes": index.total_source_bytes,
                "distinct_sources": index.distinct_sources,
                "read_rounds": index.read_rounds,
            }),
        );
        working_set.insert("guidance".into(), json!(ACTIVATED_GUIDANCE));
    }
    any_bounded
}

/// How to work the evidence once the path is open. Lifted from the
/// `research-working-sets` procedure; the live evaluation showed the query is
/// what decides a hit — a specific phrase or value finds the chunk, a vague
/// one does not.
const ACTIVATED_GUIDANCE: &str = "ROUTED THROUGH THE WORKING SET. Every page this task opens is \
captured whole and searchable. A page larger than the ordinary window is shown here only by its \
head (`excerpt_bounded_by_routing: true`); the rest of that page is in the working set, not lost. \
A page that fits is shown whole. Keep opening NEW pages with content_read as your research needs them — that is still \
how evidence gets in. What changes: to find a specific fact on a page you have ALREADY opened, \
do not read it again and do not search the web for it; call `working_set_search` with no \
`working_set_id` and a precise query (the exact phrase, value, number or identifier you \
expect on the page — not the topic), then `working_set_read` the cited chunk. One precise \
search beats several vague ones. Cite the working_set_id, source and chunk you read.";

fn attach_capture_status(result: &mut Value, capture: Value) {
    if let Some(object) = result.as_object_mut() {
        object.insert("working_set".into(), capture);
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{collections::BTreeMap, fs};

    use super::*;
    use crate::magician_v2::{
        artifact_v2::WorkingSetStore,
        content_sources::{
            ContentPrivacy, ContentProvenance, SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION,
        },
    };

    fn document(text: &str, item_id: &str) -> ContentDocument {
        ContentDocument {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity::new("test", item_id).expect("identity"),
            title: format!("Source {item_id}"),
            text: text.to_string(),
            canonical_url: Some(format!("https://example.test/{item_id}")),
            media_type: Some("text/plain".into()),
            fetched_at_ms: 1,
            privacy: ContentPrivacy::Public,
            content_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            provenance: ContentProvenance {
                source_label: "test".into(),
                source_url: Some(format!("https://example.test/{item_id}")),
                retrieved_by: "test-reader".into(),
            },
            metadata: BTreeMap::new(),
        }
    }

    fn scalar_result(document: ContentDocument, claim_eligible: bool) -> Value {
        json!({
            "status": "complete",
            "claim_eligible": claim_eligible,
            "document": document,
        })
    }

    #[test]
    fn configured_agents_capture_automatically_while_other_agents_opt_in() {
        // Spread the default so a new capture-policy field cannot break a test
        // that is only about which agents capture automatically.
        let policy = WorkingSetCaptureSettings {
            auto_capture_agents: vec!["web-researcher".into(), "evidence-reviewer".into()],
            ..WorkingSetCaptureSettings::default()
        };
        let web = content_read_capture_request(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "__agent_id": "web-researcher",
            }),
            &policy,
        )
        .expect("web capture request")
        .expect("web researcher capture");
        assert_eq!(web.title, DEFAULT_AUTO_CAPTURE_WORKING_SET_TITLE);
        assert!(content_read_capture_request(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "__agent_id": "generalist",
            }),
            &policy
        )
        .expect("other agent request")
        .is_none());
        assert!(content_read_capture_request(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "__agent_id": "evidence-reviewer",
            }),
            &policy
        )
        .expect("configured agent request")
        .is_some());
        assert!(content_read_capture_request(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "working_set_title": "  ",
            }),
            &policy
        )
        .is_err());
    }

    #[test]
    fn capture_uses_only_claim_eligible_documents_from_scalar_and_vector_results() {
        let first = document("first exact evidence", "one");
        let second = document("second exact evidence", "two");
        let vector = json!({
            "results": [
                {"result": scalar_result(first, true)},
                {"result": scalar_result(second, false)},
            ],
        });
        let documents = eligible_documents(&vector);
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].identity.item_id, "one");

        let mut private = document("private evidence", "private");
        private.privacy = ContentPrivacy::Private;
        assert!(eligible_documents(&scalar_result(private, true)).is_empty());
    }

    #[tokio::test]
    async fn materialization_returns_a_reference_and_preserves_the_read_result() {
        let root = std::env::temp_dir().join(format!(
            "magician-working-set-handler-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&root);
        let workspace = ArtifactV2Workspace::new(&root);
        let capture = content_read_capture_request(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "__agent_id": "web-researcher",
                "working_set_title": "Release research",
            }),
            &WorkingSetCaptureSettings::default(),
        )
        .expect("capture request");
        let mut result = scalar_result(document("durable cited evidence", "one"), true);
        materialize_content_read_result(
            workspace.clone(),
            capture,
            &WorkingSetActivationSettings::default(),
            &mut result,
        )
        .await;
        assert_eq!(result["status"], "complete");
        assert_eq!(result["working_set"]["status"], "created");
        let working_set_id = result["working_set"]["working_set_id"]
            .as_str()
            .expect("working set id");
        let manifest = WorkingSetStore::new(workspace)
            .get("owner", "default", working_set_id)
            .await
            .expect("persisted working set");
        assert_eq!(manifest.title, "Release research");
        assert_eq!(manifest.sources.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    fn routed_read(
        execution_id: &str,
        agent_id: &str,
        text: &str,
        item_id: &str,
    ) -> (ContentReadCaptureRequest, Value) {
        let capture = content_read_capture_request(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "__agent_id": agent_id,
                "__execution_id": execution_id,
                "working_set_title": "Routed research",
            }),
            &WorkingSetCaptureSettings::default(),
        )
        .expect("capture request")
        .expect("captures");
        let mut result = scalar_result(document(text, item_id), true);
        result["excerpt"] = json!(text.chars().take(6_000).collect::<String>());
        result["excerpt_complete"] = json!(text.chars().count() <= 6_000);
        (capture, result)
    }

    /// An open lane with a total-bytes threshold low enough that one page
    /// crosses it; the beyond-the-window threshold keeps its default.
    fn open_lane() -> WorkingSetActivationSettings {
        WorkingSetActivationSettings {
            enabled_lanes: vec!["web-research".to_string()],
            min_total_source_bytes: 1_000,
            ..WorkingSetActivationSettings::default()
        }
    }

    fn temp_workspace(tag: &str) -> (ArtifactV2Workspace, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "magician-working-set-routing-{tag}-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&root);
        (ArtifactV2Workspace::new(&root), root)
    }

    /// The gate, recorded. A lane that is not enabled never activates, and the
    /// result says so in the rule's own sentence rather than silently.
    #[tokio::test]
    async fn a_closed_lane_records_its_refusal_and_changes_nothing() {
        let (workspace, root) = temp_workspace("closed");
        let big = "x".repeat(20_000);
        let (capture, mut result) = routed_read("exec-closed", "web-researcher", &big, "one");
        let before_excerpt = result["excerpt"].clone();
        materialize_content_read_result(
            workspace.clone(),
            Some(capture),
            &WorkingSetActivationSettings::default(),
            &mut result,
        )
        .await;
        let routing = &result["working_set"]["routing"];
        assert_eq!(routing["lane"], "web-research");
        assert_eq!(routing["activated"], false);
        assert!(routing["reason"]
            .as_str()
            .expect("reason")
            .contains("routing gate is closed"));
        assert_eq!(
            result["excerpt"], before_excerpt,
            "below the gate nothing changes"
        );
        assert!(result["working_set"].get("execution_index").is_none());
        // The refusal is on the index too, marked as a shut gate, so a check
        // can tell "closed" from "below the threshold".
        let index = crate::magician_v2::artifact_v2::WorkingSetStore::new(workspace)
            .execution_index("owner", "default", "exec-closed")
            .await
            .expect("index")
            .expect("present");
        let decision = index.decision.as_ref().expect("decision kept");
        assert!(!decision.gate_open);
        assert!(!decision.activated);
        let _ = fs::remove_dir_all(root);
    }

    /// An agent in no lane has no lane; it can read all it likes.
    #[tokio::test]
    async fn an_agent_in_no_lane_never_activates() {
        let (workspace, root) = temp_workspace("nolane");
        let big = "x".repeat(20_000);
        let (capture, mut result) = routed_read("exec-nolane", "evidence-reviewer", &big, "one");
        materialize_content_read_result(workspace, Some(capture), &open_lane(), &mut result).await;
        let routing = &result["working_set"]["routing"];
        assert!(routing["lane"].is_null());
        assert_eq!(routing["activated"], false);
        let _ = fs::remove_dir_all(root);
    }

    /// The path opens when a threshold is crossed in an enabled lane, the
    /// projection narrows to the working set, and the next read stays on the
    /// path with the sentence that opened it.
    #[tokio::test]
    async fn crossing_a_threshold_in_an_open_lane_activates_narrows_and_sticks() {
        let (workspace, root) = temp_workspace("open");
        let settings = open_lane();
        let page = "durable evidence ".repeat(1_000); // ~17 KB, past the 1,000-byte threshold

        let (capture, mut first) = routed_read("exec-open", "web-researcher", &page, "one");
        materialize_content_read_result(workspace.clone(), Some(capture), &settings, &mut first)
            .await;
        let routing = &first["working_set"]["routing"];
        assert_eq!(routing["activated"], true, "reason: {}", routing["reason"]);
        assert_eq!(routing["sticky"], false);
        assert!(routing["reason"]
            .as_str()
            .expect("reason")
            .contains("activation threshold"));
        // The projection narrowed; the raw record did not.
        assert!(
            first["excerpt"].as_str().expect("excerpt").chars().count()
                <= ACTIVATED_PAGE_EXCERPT_CHARS + 1
        );
        assert_eq!(first["excerpt_bounded_by_routing"], true);
        assert_eq!(
            first["document"]["text"].as_str().map(str::len),
            Some(page.len())
        );
        assert_eq!(first["working_set"]["execution_index"]["read_rounds"], 1);
        assert!(first["working_set"]["guidance"]
            .as_str()
            .expect("guidance")
            .contains("working_set_search"));

        // A second, tiny read in the same execution: no threshold crossed by
        // it alone, and it still rides the path with the original reason.
        let (capture, mut second) =
            routed_read("exec-open", "web-researcher", "a tiny note", "two");
        materialize_content_read_result(workspace.clone(), Some(capture), &settings, &mut second)
            .await;
        let routing = &second["working_set"]["routing"];
        assert_eq!(routing["activated"], true);
        assert_eq!(routing["sticky"], true);
        assert_eq!(routing["reason"], first["working_set"]["routing"]["reason"]);
        assert_eq!(second["working_set"]["execution_index"]["read_rounds"], 2);
        assert_eq!(
            second["working_set"]["execution_index"]["members"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        assert!(
            second.get("excerpt_bounded_by_routing").is_none(),
            "a tiny page is shown whole"
        );

        // A page that fits the ordinary window whole is never narrowed, even
        // though it is larger than the head — the first live A/B cut a 3.2 KB
        // page to a head and lost content the model would otherwise have had.
        let mid = "m".repeat(3_200);
        let (capture, mut third) = routed_read("exec-open", "web-researcher", &mid, "three");
        materialize_content_read_result(workspace.clone(), Some(capture), &settings, &mut third)
            .await;
        assert_eq!(third["working_set"]["routing"]["activated"], true);
        assert!(third.get("excerpt_bounded_by_routing").is_none());
        assert_eq!(third["excerpt"].as_str().map(str::len), Some(3_200));
        assert!(third
            .get(crate::magician_v2::tool_result_projection::PROJECTION_HINTS_FIELD)
            .is_none());

        // Only the first (17 KB) read was narrowed, and the index knows.
        let index = crate::magician_v2::artifact_v2::WorkingSetStore::new(workspace)
            .execution_index("owner", "default", "exec-open")
            .await
            .expect("index")
            .expect("present");
        assert_eq!(index.narrowed_reads, 1);
        let _ = fs::remove_dir_all(root);
    }

    /// Activation alone changes nothing the model sees. The open-baseline
    /// A/B on 2026-09-19 activated a task by depth on pages that all fit the
    /// ordinary window, and every read still carried the "routed through the
    /// working set — search it" guidance and a growing member list: a steer
    /// with nothing behind it, which turned one run search-heavy and read-light
    /// and cost 17 % more. The decision is recorded on every read; the guidance
    /// and the index appear only once a read has actually been narrowed —
    /// then the working set holds something the window did not show, and
    /// every later read in the task says so.
    #[tokio::test]
    async fn an_activated_task_whose_pages_all_fit_is_told_nothing_until_a_read_is_narrowed() {
        let (workspace, root) = temp_workspace("silent");
        let settings = open_lane();

        let (capture, mut fits) =
            routed_read("exec-silent", "web-researcher", &"m".repeat(3_200), "one");
        materialize_content_read_result(workspace.clone(), Some(capture), &settings, &mut fits)
            .await;
        assert_eq!(
            fits["working_set"]["routing"]["activated"], true,
            "reason: {}",
            fits["working_set"]["routing"]["reason"]
        );
        assert!(fits.get("excerpt_bounded_by_routing").is_none());
        assert!(
            fits["working_set"].get("guidance").is_none(),
            "nothing was withheld, so nothing tells the model to search for it"
        );
        assert!(fits["working_set"].get("execution_index").is_none());

        let (capture, mut large) = routed_read(
            "exec-silent",
            "web-researcher",
            &"durable evidence ".repeat(1_000),
            "two",
        );
        materialize_content_read_result(workspace.clone(), Some(capture), &settings, &mut large)
            .await;
        assert_eq!(large["excerpt_bounded_by_routing"], true);
        assert!(large["working_set"]["guidance"]
            .as_str()
            .expect("guidance")
            .contains("working_set_search"));
        assert_eq!(large["working_set"]["execution_index"]["read_rounds"], 2);

        // From here on the set holds a page the window did not show, so even
        // a read that fits carries the guidance and the index.
        let (capture, mut later) =
            routed_read("exec-silent", "web-researcher", "a tiny note", "three");
        materialize_content_read_result(workspace.clone(), Some(capture), &settings, &mut later)
            .await;
        assert!(later.get("excerpt_bounded_by_routing").is_none());
        assert!(later["working_set"]["guidance"].is_string());
        assert_eq!(later["working_set"]["execution_index"]["read_rounds"], 3);
        let _ = fs::remove_dir_all(root);
    }

    /// Ordinary research in an open lane stays on the ordinary path. Three
    /// pages over three rounds, each shown whole or nearly so, is what the
    /// web researcher does on a pricing comparison; under the shipped
    /// thresholds the set would hold nothing worth the path, and the refusal
    /// says what it measured. The index keeps that decision so an operator's
    /// check can read it.
    #[tokio::test]
    async fn ordinary_research_in_an_open_lane_stays_below_the_threshold() {
        let (workspace, root) = temp_workspace("ordinary");
        let settings = WorkingSetActivationSettings {
            enabled_lanes: vec!["web-research".to_string()],
            ..WorkingSetActivationSettings::default()
        };
        let pages = ["p".repeat(8_900), "q".repeat(3_200), "r".repeat(14_100)];
        let mut last = Value::Null;
        for (i, page) in pages.iter().enumerate() {
            let (capture, mut result) = routed_read(
                "exec-ordinary",
                "web-researcher",
                page,
                &format!("page-{i}"),
            );
            materialize_content_read_result(
                workspace.clone(),
                Some(capture),
                &settings,
                &mut result,
            )
            .await;
            last = result;
        }
        let routing = &last["working_set"]["routing"];
        assert_eq!(routing["activated"], false, "reason: {}", routing["reason"]);
        assert_eq!(routing["lane"], "web-research");
        assert!(routing["reason"]
            .as_str()
            .expect("reason")
            .contains("beyond the window"));
        assert!(last.get("excerpt_bounded_by_routing").is_none());
        assert!(last["working_set"].get("guidance").is_none());

        let index = crate::magician_v2::artifact_v2::WorkingSetStore::new(workspace)
            .execution_index("owner", "default", "exec-ordinary")
            .await
            .expect("index")
            .expect("present");
        assert!(!index.is_activated());
        assert_eq!(
            index.beyond_window_bytes,
            (8_900 - 6_000) + (14_100 - 6_000)
        );
        let decision = index
            .decision
            .as_ref()
            .expect("the refusal is on the index");
        assert!(decision.gate_open);
        assert!(!decision.activated);
        assert_eq!(decision.lane.as_deref(), Some("web-research"));
        let _ = fs::remove_dir_all(root);
    }

    /// A read with no execution is a snapshot with no task to accrue on.
    #[tokio::test]
    async fn a_read_outside_any_execution_captures_but_does_not_route() {
        let (workspace, root) = temp_workspace("noexec");
        let capture = content_read_capture_request(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "__agent_id": "web-researcher",
                "working_set_title": "Loose read",
            }),
            &WorkingSetCaptureSettings::default(),
        )
        .expect("capture request");
        let mut result = scalar_result(document(&"y".repeat(20_000), "one"), true);
        materialize_content_read_result(workspace, capture, &open_lane(), &mut result).await;
        assert_eq!(result["working_set"]["status"], "created");
        assert!(result["working_set"].get("routing").is_none());
        let _ = fs::remove_dir_all(root);
    }

    /// What the model is actually shown, not what the handler wrote. The
    /// first live A/B activated the path and the researcher never searched:
    /// the contract's excerpt path projected a head of the full text no
    /// matter what the handler did to `excerpt`, and `working_set` was not a
    /// priority field. This projects a routed read through the real
    /// content-read contract at the autonomous budget and asserts the model
    /// gets the guidance, the decision, and a capped page — and that a page
    /// which fits is shown whole.
    #[tokio::test]
    async fn what_the_model_sees_from_a_routed_read_is_the_guidance_and_a_capped_page() {
        use crate::magician_v2::{
            execution::capability::CapabilityPackDefinition,
            tool_result_projection::{
                ConservativeTokenEstimator, DisplayResultProjection, ProjectionBudget,
                ProjectionContractRegistry, RawResultDescriptor, ResultRetentionClass,
                ScopedResultRef, ToolOutcome, ToolResultIdentity, ToolResultProjectionRequest,
                ToolResultProjector,
            },
        };

        let definition: CapabilityPackDefinition =
            serde_yaml::from_str(include_str!("../embedded_pack_defs/content_read.yaml"))
                .expect("the embedded content_read pack parses");
        let contract = definition
            .result_projection
            .expect("content_read declares a projection contract");
        let mut registry = ProjectionContractRegistry::default();
        registry
            .register_override(contract.clone())
            .expect("the real contract registers");
        let projector = ToolResultProjector::new(registry, ConservativeTokenEstimator);
        // The autonomous surface, as shipped in the config seed.
        let budget = ProjectionBudget {
            max_serialized_bytes: 24_576,
            max_estimated_tokens: 6_144,
            max_records: 20,
            max_depth: 8,
            max_scalar_bytes: 8_192,
            max_spoken_chars: 420,
        };
        let descriptor = RawResultDescriptor {
            content_ref: ScopedResultRef {
                result_ref: "result_ref".to_owned(),
                cursor: None,
            },
            content_hash: "raw-hash".to_owned(),
            media_type: "application/json".to_owned(),
            size_bytes: 0,
            retention_class: ResultRetentionClass::TaskExecution,
        };
        let project = |raw: &Value| {
            projector
                .project(ToolResultProjectionRequest {
                    identity: ToolResultIdentity {
                        tool_name: "content_read".to_owned(),
                        tool_call_id: "call-1".to_owned(),
                        execution_id: Some("exec-projection".to_owned()),
                        task_id: None,
                        scope_digest: "scope".to_owned(),
                        authority_revision: "authority".to_owned(),
                    },
                    outcome: ToolOutcome::succeeded(),
                    raw_result: raw,
                    display: DisplayResultProjection::referenced(&descriptor),
                    raw: descriptor.clone(),
                    spoken_hint: None,
                    contract_id: Some(&contract.contract_id),
                    budget: budget.clone(),
                })
                .expect("projection")
                .model
                .value
        };

        let (workspace, root) = temp_workspace("projection");
        let settings = open_lane();
        // A page four times the ordinary window, so the working set genuinely
        // holds more than any excerpt shows.
        let page = "Pricing detail sentence with a number 42. ".repeat(600); // ~25 KB
        let (capture, mut routed) = routed_read("exec-projection", "web-researcher", &page, "big");
        materialize_content_read_result(workspace.clone(), Some(capture), &settings, &mut routed)
            .await;
        assert_eq!(routed["working_set"]["routing"]["activated"], true);

        let shown = project(&routed);
        let data = &shown["data"];
        assert_eq!(
            data["working_set"]["routing"]["activated"], true,
            "the decision reaches the model"
        );
        assert!(
            data["working_set"]["guidance"]
                .as_str()
                .is_some_and(|g| g.contains("working_set_search")),
            "the guidance reaches the model: {}",
            serde_json::to_string(data).unwrap_or_default()
        );
        assert!(data["working_set"]["execution_index"]["members"].is_array());
        assert!(
            data["document"].get("text").is_none(),
            "the full text does not reach the model whole"
        );
        let excerpt_len = data["excerpt"].as_str().map(str::len).unwrap_or(0);
        assert!(excerpt_len <= ACTIVATED_PAGE_EXCERPT_CHARS * 4 && excerpt_len > 0);
        assert!(data
            .get(crate::magician_v2::tool_result_projection::PROJECTION_HINTS_FIELD)
            .is_none());
        // And the raw record still carries the whole page and its hash.
        assert_eq!(
            routed["document"]["text"].as_str().map(str::len),
            Some(page.len())
        );

        // A small page on the same activated execution is shown whole: there
        // is nothing more in the working set to search for.
        let small = "A short note with the number 7.";
        let (capture, mut small_read) =
            routed_read("exec-projection", "web-researcher", small, "small");
        materialize_content_read_result(workspace, Some(capture), &settings, &mut small_read).await;
        assert_eq!(small_read["working_set"]["routing"]["sticky"], true);
        assert!(small_read.get("excerpt_bounded_by_routing").is_none());
        let shown = project(&small_read);
        assert_eq!(
            shown["data"]["document"]["text"], small,
            "a page that fits is shown whole"
        );
        let _ = fs::remove_dir_all(root);
    }
}
