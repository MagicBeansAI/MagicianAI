//! Bounded fan-out for provider-neutral content discovery and reading.
//!
//! Each branch still executes the existing single-item handler, so policy,
//! authority, caching, receipts, fallback order, and telemetry have one source
//! of truth. This module owns only orchestration: stable ordering, bounded
//! concurrency, per-host read limits, a shared deadline, and sibling-only
//! cancellation after an explicitly requested evidence threshold is reached.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    sync::Arc,
    time::Instant,
};

use futures_util::{stream, StreamExt};
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::magician_v2::{
    content_sources::{ContentAcquisitionResolver, ProgressiveRetrievalSettings},
    execution::error::ExecutionError,
};

use super::shared::require_scope_str;

const SEARCH_BATCH_MAX_ITEMS: usize = 8;
const READ_BATCH_MAX_ITEMS: usize = 6;
const SEARCH_BATCH_HARD_CONCURRENCY: usize = 4;
const READ_BATCH_HARD_CONCURRENCY: usize = 5;
const READ_BATCH_HARD_PER_HOST_CONCURRENCY: usize = 2;
const SEARCH_EVIDENCE_PER_BRANCH: usize = 3;
const SEARCH_EVIDENCE_TEXT_CHARS: usize = 500;
const READ_EVIDENCE_TEXT_CHARS: usize = 800;

#[derive(Debug, Clone, Copy)]
enum BatchKind {
    Search,
    Read,
}

impl BatchKind {
    fn tool_name(self) -> &'static str {
        match self {
            Self::Search => "content_search",
            Self::Read => "content_read",
        }
    }

    fn item_limit(self) -> usize {
        match self {
            Self::Search => SEARCH_BATCH_MAX_ITEMS,
            Self::Read => READ_BATCH_MAX_ITEMS,
        }
    }

    fn hard_concurrency(self) -> usize {
        match self {
            Self::Search => SEARCH_BATCH_HARD_CONCURRENCY,
            Self::Read => READ_BATCH_HARD_CONCURRENCY,
        }
    }

    /// Scalar-shaped fields that have an unambiguous vector meaning when
    /// applied to every branch. The canonical model contract places these in
    /// `common`, but accepting their top-level spelling makes unified tools
    /// resilient to models that combine `requests` with a familiar scalar
    /// option such as `fresh`. Branch identity/targets stay strict.
    fn shared_branch_fields(self) -> &'static [&'static str] {
        match self {
            Self::Search => &[
                "intent",
                "limit",
                "min_candidates",
                "min_sources",
                "targets",
                "allowed_actions",
                "options",
                "fresh",
                "relevance_threshold",
                "max_attempts",
                "cost_budget_microunits",
                // The pack executor may add this action-level budget after the
                // model has already emitted a canonical vector envelope.
                "timeout_secs",
            ],
            Self::Read => &[
                "depth",
                "output_kind",
                "fresh",
                "required_metadata",
                "min_chars",
                "allowed_actions",
                "maximum_authority",
                "max_attempts",
                "cost_budget_microunits",
                // See the search variant above. Treat the runtime-added budget
                // as shared instead of rejecting an otherwise valid vector.
                "timeout_secs",
            ],
        }
    }
}

#[derive(Debug)]
struct BatchItem {
    index: usize,
    id: String,
    args: Value,
    host_key: Option<String>,
}

#[derive(Debug)]
struct BatchRequest {
    kind: BatchKind,
    principal: String,
    workspace: String,
    items: Vec<BatchItem>,
    max_concurrency: usize,
    per_host_concurrency: usize,
    minimum_successes: usize,
    shared_deadline_ms: u64,
}

#[derive(Debug, Serialize)]
struct BatchItemResult {
    index: usize,
    id: String,
    /// Original read target used only to explain redirects in the compact
    /// model-facing evidence lane. Runtime-owned scope arguments never enter
    /// the serialized branch result.
    #[serde(skip)]
    requested_url: Option<String>,
    outcome: &'static str,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl BatchItemResult {
    fn cancelled(item: BatchItem, started: Instant, reason: impl Into<String>) -> Self {
        let requested_url = requested_url(&item.args);
        Self {
            index: item.index,
            id: item.id,
            requested_url,
            outcome: "cancelled",
            duration_ms: elapsed_ms(started),
            result: None,
            error: Some(reason.into()),
        }
    }

    fn is_complete(&self) -> bool {
        self.outcome == "returned"
            && self
                .result
                .as_ref()
                .and_then(|result| result.get("status"))
                .and_then(Value::as_str)
                == Some("complete")
    }

    fn is_usable(&self) -> bool {
        self.outcome == "returned"
            && matches!(
                self.result
                    .as_ref()
                    .and_then(|result| result.get("status"))
                    .and_then(Value::as_str),
                Some("complete" | "degraded" | "handoff_required" | "approval_required")
            )
    }
}

pub async fn execute_search_vector_with_runtime_and_cancellation(
    resolver: Arc<ContentAcquisitionResolver>,
    settings: ProgressiveRetrievalSettings,
    args: Value,
    cancellation: CancellationToken,
) -> Result<Value, ExecutionError> {
    let batch_started = Instant::now();
    let batch = parse_batch(&args, &settings, BatchKind::Search)?;
    let service = resolver
        .resolve(batch.principal.clone(), batch.workspace.clone())
        .await
        .map_err(|error| ExecutionError::Step(format!("resolving content acquisition: {error}")))?;
    // The vector scheduler owns the concurrency budget. Prevent each branch from
    // multiplying it again inside an eligible-parallel controller rung.
    let mut run_settings = settings.clone();
    run_settings.max_parallel_actions = 1;
    // Both the batch driver and each branch body are heap-owned: a branch runs the
    // full single-search chain, and nesting that inline put it inside this frame,
    // inside `run_batch`'s, and inside the stream machinery between them.
    Box::pin(run_batch(
        batch,
        batch_started,
        cancellation,
        move |branch_args, cancellation| {
            let service = Arc::clone(&service);
            let settings = run_settings.clone();
            Box::pin(async move {
                super::content_search::execute_with_service(
                    service,
                    settings,
                    branch_args,
                    cancellation,
                )
                .await
            })
        },
    ))
    .await
}

pub async fn execute_read_vector_with_runtime_and_cancellation(
    resolver: Arc<ContentAcquisitionResolver>,
    settings: ProgressiveRetrievalSettings,
    args: Value,
    cancellation: CancellationToken,
) -> Result<Value, ExecutionError> {
    let batch_started = Instant::now();
    let batch = parse_batch(&args, &settings, BatchKind::Read)?;
    let service = resolver
        .resolve(batch.principal.clone(), batch.workspace.clone())
        .await
        .map_err(|error| ExecutionError::Step(format!("resolving content acquisition: {error}")))?;
    // Keep the outer branch/per-host semaphores authoritative even if a future
    // read rung becomes eligible-parallel.
    let mut run_settings = settings.clone();
    run_settings.max_parallel_actions = 1;
    // Heap-owned for the same reason as the search lane above.
    Box::pin(run_batch(
        batch,
        batch_started,
        cancellation,
        move |branch_args, cancellation| {
            let service = Arc::clone(&service);
            let settings = run_settings.clone();
            Box::pin(async move {
                super::content_read::execute_with_service(
                    service,
                    settings,
                    branch_args,
                    cancellation,
                )
                .await
            })
        },
    ))
    .await
}

fn parse_batch(
    args: &Value,
    settings: &ProgressiveRetrievalSettings,
    kind: BatchKind,
) -> Result<BatchRequest, ExecutionError> {
    let tool_name = kind.tool_name();
    validate_batch_envelope(args, kind)?;
    let principal = require_scope_str(args, "__principal", tool_name)?;
    let workspace = require_scope_str(args, "__workspace", tool_name)?;
    let raw_items = args
        .get("requests")
        .and_then(Value::as_array)
        .ok_or_else(|| ExecutionError::Step(format!("{tool_name} requires `requests` array")))?;
    if raw_items.is_empty() || raw_items.len() > kind.item_limit() {
        return Err(ExecutionError::Step(format!(
            "{tool_name} requires 1-{} requests",
            kind.item_limit()
        )));
    }

    let mut common = match args.get("common") {
        None => Map::new(),
        Some(Value::Object(common)) => checked_public_args(common, tool_name, "common")?,
        Some(_) => {
            return Err(ExecutionError::Step(format!(
                "{tool_name} `common` must be an object"
            )));
        },
    };
    for key in kind.shared_branch_fields() {
        let Some(value) = args.get(*key) else {
            continue;
        };
        if let Some(existing) = common.get(*key) {
            if existing != value {
                return Err(ExecutionError::Step(format!(
                    "{tool_name} vector form sets shared `{key}` both top-level and in `common` with different values"
                )));
            }
        } else {
            common.insert((*key).to_string(), value.clone());
        }
    }
    validate_common_args(kind, &common)?;
    let mut ids = HashSet::new();
    let mut items = Vec::with_capacity(raw_items.len());
    for (index, raw_item) in raw_items.iter().enumerate() {
        let object = raw_item.as_object().ok_or_else(|| {
            ExecutionError::Step(format!("{tool_name} request #{index} must be an object"))
        })?;
        let mut item_args = common.clone();
        for (key, value) in checked_public_args(object, tool_name, "request")? {
            item_args.insert(key, value);
        }
        let id = branch_id(item_args.remove("id"), index, tool_name)?;
        if !ids.insert(id.clone()) {
            return Err(ExecutionError::Step(format!(
                "{tool_name} request id `{id}` is duplicated"
            )));
        }
        if matches!(kind, BatchKind::Read)
            && !item_args.contains_key("depth")
            && !item_args.contains_key("output_kind")
        {
            // A single explicit read keeps its historical full-text default.
            // Batch reads are evidence fan-out, where multiplying full bodies
            // is both slower and more likely to exceed raw-result bounds.
            item_args.insert("depth".into(), json!("gist"));
        }
        item_args.insert("__principal".into(), Value::String(principal.clone()));
        item_args.insert("__workspace".into(), Value::String(workspace.clone()));
        let host_key = (matches!(kind, BatchKind::Read))
            .then(|| read_host_key(&item_args, index))
            .flatten();
        items.push(BatchItem {
            index,
            id,
            args: Value::Object(item_args),
            host_key,
        });
    }

    let policy_concurrency = settings.max_parallel_actions.max(1);
    let maximum_concurrency = policy_concurrency.min(kind.hard_concurrency());
    let requested_concurrency = positive_usize(args, "max_concurrency", maximum_concurrency)?;
    let max_concurrency = requested_concurrency
        .min(maximum_concurrency)
        .min(items.len());
    let per_host_concurrency = if matches!(kind, BatchKind::Read) {
        positive_usize(args, "per_host_concurrency", 1)?
            .min(READ_BATCH_HARD_PER_HOST_CONCURRENCY)
            .min(max_concurrency)
    } else {
        max_concurrency
    };
    let minimum_successes = positive_usize(args, "minimum_successes", items.len())?;
    if minimum_successes > items.len() {
        return Err(ExecutionError::Step(format!(
            "{tool_name} `minimum_successes` cannot exceed request count"
        )));
    }
    let requested_deadline = args
        .get("deadline_ms")
        .map(|value| {
            value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                ExecutionError::Step(format!(
                    "{tool_name} `deadline_ms` must be a positive integer"
                ))
            })
        })
        .transpose()?
        .unwrap_or(settings.default_deadline_ms);

    Ok(BatchRequest {
        kind,
        principal,
        workspace,
        items,
        max_concurrency,
        per_host_concurrency,
        minimum_successes,
        shared_deadline_ms: requested_deadline.min(settings.default_deadline_ms),
    })
}

fn validate_batch_envelope(args: &Value, kind: BatchKind) -> Result<(), ExecutionError> {
    let object = args.as_object().ok_or_else(|| {
        ExecutionError::Step(format!("{} arguments must be an object", kind.tool_name()))
    })?;
    if let Some(key) = object.keys().find(|key| {
        !key.starts_with("__")
            && !matches!(
                key.as_str(),
                "requests"
                    | "common"
                    | "max_concurrency"
                    | "minimum_successes"
                    | "deadline_ms"
                    | "working_set_title"
            )
            && !kind.shared_branch_fields().contains(&key.as_str())
            && !(matches!(kind, BatchKind::Read) && key.as_str() == "per_host_concurrency")
    }) {
        return Err(ExecutionError::Step(format!(
            "{} vector form does not accept top-level `{key}`; place shared fields in `common` or branch fields in `requests`",
            kind.tool_name()
        )));
    }
    Ok(())
}

fn branch_id(
    value: Option<Value>,
    index: usize,
    tool_name: &str,
) -> Result<String, ExecutionError> {
    let Some(value) = value else {
        return Ok(format!("branch-{}", index + 1));
    };
    let raw = value.as_str().ok_or_else(|| {
        ExecutionError::Step(format!(
            "{tool_name} request #{index} `id` must be a string"
        ))
    })?;
    let id = raw.trim();
    if id.is_empty() {
        return Ok(format!("branch-{}", index + 1));
    }
    if id.chars().count() > 128 {
        return Err(ExecutionError::Step(format!(
            "{tool_name} request #{index} `id` exceeds 128 characters"
        )));
    }
    Ok(id.to_string())
}

fn checked_public_args(
    values: &Map<String, Value>,
    tool_name: &str,
    label: &str,
) -> Result<Map<String, Value>, ExecutionError> {
    if let Some(key) = values.keys().find(|key| key.starts_with("__")) {
        return Err(ExecutionError::Step(format!(
            "{tool_name} {label} cannot set runtime-owned `{key}`"
        )));
    }
    Ok(values.clone())
}

fn validate_common_args(
    kind: BatchKind,
    common: &Map<String, Value>,
) -> Result<(), ExecutionError> {
    let branch_owned: &[&str] = match kind {
        BatchKind::Search => &["id", "query"],
        BatchKind::Read => &[
            "id",
            "candidate",
            "selection_receipt",
            "url",
            "title",
            "inline_text",
            "authority_grant_id",
        ],
    };
    if let Some(key) = branch_owned.iter().find(|key| common.contains_key(**key)) {
        return Err(ExecutionError::Step(format!(
            "{} `{key}` must be set per request, not in `common`",
            kind.tool_name()
        )));
    }
    Ok(())
}

fn positive_usize(args: &Value, key: &str, default: usize) -> Result<usize, ExecutionError> {
    match args.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| ExecutionError::Step(format!("`{key}` must be a positive integer"))),
    }
}

fn read_host_key(args: &Map<String, Value>, index: usize) -> Option<String> {
    let raw_url = args
        .get("candidate")
        .and_then(Value::as_object)
        .and_then(|candidate| candidate.get("canonical_url"))
        .and_then(Value::as_str)
        .or_else(|| args.get("url").and_then(Value::as_str));
    raw_url
        .and_then(|raw_url| url::Url::parse(raw_url).ok())
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        // Invalid branches must not serialize unrelated hosts behind one
        // synthetic semaphore; the single read handler reports the real error.
        .or_else(|| Some(format!("unknown-{index}")))
}

async fn run_batch<F, Fut>(
    batch: BatchRequest,
    started: Instant,
    parent_cancellation: CancellationToken,
    execute: F,
) -> Result<Value, ExecutionError>
where
    F: Fn(Value, CancellationToken) -> Fut + Clone,
    Fut: Future<Output = Result<Value, ExecutionError>>,
{
    let child_cancellation = parent_cancellation.child_token();
    let host_limits = batch
        .items
        .iter()
        .filter_map(|item| item.host_key.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|host| (host, Arc::new(Semaphore::new(batch.per_host_concurrency))))
        .collect::<HashMap<_, _>>();
    let global_limit = Arc::new(Semaphore::new(batch.max_concurrency));
    let max_concurrency = batch.max_concurrency;
    let per_host_concurrency = batch.per_host_concurrency;
    let minimum_successes = batch.minimum_successes;
    let shared_deadline_ms = batch.shared_deadline_ms;
    let requested_count = batch.items.len();

    let futures = batch.items.into_iter().map(|mut item| {
        let execute = execute.clone();
        let cancellation = child_cancellation.child_token();
        let host_limit = item
            .host_key
            .as_ref()
            .and_then(|host| host_limits.get(host))
            .cloned();
        let global_limit = Arc::clone(&global_limit);
        async move {
            let branch_started = Instant::now();
            if cancellation.is_cancelled() {
                return BatchItemResult::cancelled(
                    item,
                    branch_started,
                    "batch branch cancelled before start",
                );
            }
            let _host_permit = if let Some(host_limit) = host_limit {
                tokio::select! {
                    _ = cancellation.cancelled() => {
                        return BatchItemResult::cancelled(
                            item,
                            branch_started,
                            "batch branch cancelled while waiting for its host limit",
                        );
                    }
                    permit = host_limit.acquire_owned() => permit.ok(),
                }
            } else {
                None
            };
            // Acquire the global permit after the per-host permit. Waiting
            // branches for one busy host therefore do not occupy every global
            // slot and block an unrelated host from progressing.
            let _global_permit = tokio::select! {
                _ = cancellation.cancelled() => {
                    return BatchItemResult::cancelled(
                        item,
                        branch_started,
                        "batch branch cancelled while waiting for global concurrency",
                    );
                }
                permit = global_limit.acquire_owned() => permit.ok(),
            };
            // A permit and cancellation can become ready in the same poll.
            // Recheck after both queues so an already-cancelled sibling never
            // starts provider work (or spends) merely because select chose the
            // permit branch in that race.
            if cancellation.is_cancelled() {
                return BatchItemResult::cancelled(
                    item,
                    branch_started,
                    "batch branch cancelled before provider dispatch",
                );
            }
            let elapsed = elapsed_ms(started);
            if elapsed >= shared_deadline_ms {
                return BatchItemResult::cancelled(
                    item,
                    branch_started,
                    "shared batch deadline elapsed before branch start",
                );
            }
            let remaining = shared_deadline_ms - elapsed;
            if let Some(item_args) = item.args.as_object_mut() {
                let branch_deadline = item_args
                    .get("deadline_ms")
                    .and_then(Value::as_u64)
                    .filter(|value| *value > 0)
                    .unwrap_or(remaining)
                    .min(remaining);
                item_args.insert("deadline_ms".into(), json!(branch_deadline));
            }
            let requested_url = requested_url(&item.args);
            match Box::pin(execute(item.args, cancellation)).await {
                Ok(result) => BatchItemResult {
                    index: item.index,
                    id: item.id,
                    requested_url,
                    outcome: "returned",
                    duration_ms: elapsed_ms(branch_started),
                    result: Some(result),
                    error: None,
                },
                Err(error) => BatchItemResult {
                    index: item.index,
                    id: item.id,
                    requested_url,
                    outcome: "error",
                    duration_ms: elapsed_ms(branch_started),
                    result: None,
                    error: Some(error.to_string()),
                },
            }
        }
    });

    // Poll every bounded branch (at most eight); semaphores, rather than the
    // stream buffer, enforce global/per-host execution. This avoids
    // head-of-line blocking when the first several requests share one host.
    let mut running = stream::iter(futures).buffer_unordered(requested_count);
    let mut results = Vec::with_capacity(requested_count);
    let mut complete_count = 0usize;
    while let Some(result) = running.next().await {
        if result.is_complete() {
            complete_count += 1;
            if complete_count >= minimum_successes {
                child_cancellation.cancel();
            }
        }
        results.push(result);
    }
    results.sort_by_key(|result| result.index);

    let error_count = results
        .iter()
        .filter(|result| result.outcome == "error")
        .count();
    let cancelled_count = results
        .iter()
        .filter(|result| result.outcome == "cancelled")
        .count();
    let returned_count = results
        .iter()
        .filter(|result| result.outcome == "returned")
        .count();
    let usable_count = results.iter().filter(|result| result.is_usable()).count();
    let evidence = bounded_evidence(batch.kind, &results);
    let total_cost_microunits = aggregate_costs(&results);
    let serial_branch_duration_ms = results
        .iter()
        .map(|result| result.duration_ms)
        .fold(0u64, u64::saturating_add);
    let duration_ms = elapsed_ms(started);
    let parallel_overlap_saved_ms = serial_branch_duration_ms.saturating_sub(duration_ms);
    let status = if parent_cancellation.is_cancelled() {
        "cancelled"
    } else if complete_count >= minimum_successes {
        "complete"
    } else if usable_count > 0 {
        "partial"
    } else {
        "failed"
    };

    let output = json!({
        "schema_version": 1,
        "status": status,
        "operation": match batch.kind { BatchKind::Search => "discover", BatchKind::Read => "read" },
        "requested_count": requested_count,
        "returned_count": returned_count,
        "usable_count": usable_count,
        "complete_count": complete_count,
        "error_count": error_count,
        "cancelled_count": cancelled_count,
        "minimum_successes": minimum_successes,
        "max_concurrency": max_concurrency,
        "per_host_concurrency": per_host_concurrency,
        "shared_deadline_ms": shared_deadline_ms,
        "duration_ms": duration_ms,
        "serial_branch_duration_ms": serial_branch_duration_ms,
        "parallel_overlap_saved_ms": parallel_overlap_saved_ms,
        "total_cost_microunits": total_cost_microunits,
        "evidence_count": evidence.len(),
        "evidence": evidence,
        "results": results,
    });
    tracing::info!(
        operation = match batch.kind {
            BatchKind::Search => "discover",
            BatchKind::Read => "read",
        },
        requested_count,
        returned_count,
        usable_count,
        complete_count,
        error_count,
        cancelled_count,
        max_concurrency,
        per_host_concurrency,
        duration_ms,
        serial_branch_duration_ms,
        parallel_overlap_saved_ms,
        evidence_count = output["evidence_count"].as_u64().unwrap_or_default(),
        "content acquisition batch completed"
    );
    Ok(output)
}

/// Keep a compact, deliberately bounded evidence lane next to the lossless raw
/// branch results. The canonical result materializer retains `results`; this
/// lane ensures the model projection can still see useful complete records
/// when a page body or nested search response is too large to admit atomically.
fn bounded_evidence(kind: BatchKind, results: &[BatchItemResult]) -> Vec<Value> {
    let mut evidence = Vec::new();
    match kind {
        BatchKind::Search => {
            // Round-robin candidate ranks across branches. The projector keeps
            // complete records atomically, so this ordering gives every
            // comparison entity its best candidate before admitting second or
            // third candidates from any entity.
            for candidate_index in 0..SEARCH_EVIDENCE_PER_BRANCH {
                for branch in results {
                    if let Some(record) = search_evidence_record(branch, candidate_index) {
                        evidence.push(record);
                    }
                }
            }
        },
        BatchKind::Read => {
            for branch in results {
                if !branch.is_usable() {
                    continue;
                }
                let Some(result) = branch.result.as_ref() else {
                    continue;
                };
                let result_status = result
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let Some(document) = result.get("document").and_then(Value::as_object) else {
                    continue;
                };
                let Some(text) = document.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let inline_only = document
                    .get("media_type")
                    .and_then(Value::as_str)
                    .is_some_and(|media_type| media_type.contains("source=inline"))
                    || document
                        .get("metadata")
                        .and_then(|metadata| metadata.get("fetch_status"))
                        .and_then(Value::as_str)
                        == Some("not_fetched");
                let (excerpt, excerpt_complete) =
                    bounded_text_excerpt(text, READ_EVIDENCE_TEXT_CHARS);
                let mut record = Map::from_iter([
                    ("branch_id".into(), json!(branch.id)),
                    ("branch_index".into(), json!(branch.index)),
                    ("result_status".into(), json!(result_status)),
                    (
                        "fetch_status".into(),
                        json!(if inline_only {
                            "not_fetched"
                        } else if result_status == "complete" {
                            "complete"
                        } else {
                            "partial"
                        }),
                    ),
                    (
                        "evidence_role".into(),
                        json!(if inline_only {
                            "discovery_only"
                        } else {
                            "opened_page"
                        }),
                    ),
                    ("claim_eligible".into(), json!(!inline_only)),
                    ("excerpt".into(), json!(excerpt)),
                    ("excerpt_complete".into(), json!(excerpt_complete)),
                ]);
                copy_if_present_map(document, &mut record, "title", "title");
                copy_if_present_map(document, &mut record, "canonical_url", "url");
                copy_if_present_map(document, &mut record, "canonical_url", "final_url");
                copy_if_present_map(document, &mut record, "fetched_at_ms", "fetched_at_ms");
                if let Some(requested_url) = branch.requested_url.as_deref() {
                    record.insert("requested_url".into(), json!(requested_url));
                    if let Some(final_url) = document.get("canonical_url").and_then(Value::as_str) {
                        record.insert(
                            "redirected".into(),
                            json!(!urls_equivalent(requested_url, final_url)),
                        );
                    }
                }
                if let Some(source) = document
                    .get("provenance")
                    .and_then(|value| value.get("source_label"))
                {
                    record.insert("source".into(), source.clone());
                }
                evidence.push(Value::Object(record));
            }
        },
    }
    evidence
}

fn search_evidence_record(branch: &BatchItemResult, candidate_index: usize) -> Option<Value> {
    if !branch.is_usable() {
        return None;
    }
    let result = branch.result.as_ref()?;
    let retrieved = result.get("candidates")?.as_array()?.get(candidate_index)?;
    let candidate = retrieved.get("candidate").unwrap_or(retrieved);
    let snippet = candidate.get("cheap_text")?.as_str()?;
    let (snippet, snippet_complete) = bounded_text_excerpt(snippet, SEARCH_EVIDENCE_TEXT_CHARS);
    let mut record = Map::from_iter([
        ("branch_id".into(), json!(branch.id)),
        ("branch_index".into(), json!(branch.index)),
        (
            "result_status".into(),
            json!(result
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")),
        ),
        ("candidate_index".into(), json!(candidate_index)),
        ("evidence_role".into(), json!("discovery_only")),
        ("claim_eligible".into(), json!(false)),
        ("snippet".into(), json!(snippet)),
        ("snippet_complete".into(), json!(snippet_complete)),
    ]);
    copy_if_present(candidate, &mut record, "title", "title");
    copy_if_present(candidate, &mut record, "canonical_url", "url");
    copy_if_present(candidate, &mut record, "published_at_ms", "published_at_ms");
    copy_if_present(retrieved, &mut record, "relevance_score", "relevance_score");
    if let Some(receipt) = retrieved
        .get("selection_receipt")
        .and_then(Value::as_object)
    {
        copy_if_present_map(receipt, &mut record, "id", "selection_receipt");
        copy_if_present_map(
            receipt,
            &mut record,
            "expires_at_ms",
            "selection_receipt_expires_at_ms",
        );
    }
    if let Some(source) = candidate
        .get("provenance")
        .and_then(|value| value.get("source_label"))
    {
        record.insert("source".into(), source.clone());
    }
    Some(Value::Object(record))
}

fn requested_url(args: &Value) -> Option<String> {
    args.get("candidate")
        .and_then(Value::as_object)
        .and_then(|candidate| candidate.get("canonical_url"))
        .and_then(Value::as_str)
        .or_else(|| args.get("url").and_then(Value::as_str))
        .map(str::to_string)
}

fn urls_equivalent(left: &str, right: &str) -> bool {
    match (url::Url::parse(left), url::Url::parse(right)) {
        (Ok(mut left), Ok(mut right)) => {
            left.set_fragment(None);
            right.set_fragment(None);
            left == right
        },
        _ => left == right,
    }
}

fn copy_if_present(source: &Value, target: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = source.get(from).filter(|value| !value.is_null()) {
        target.insert(to.to_string(), value.clone());
    }
}

fn copy_if_present_map(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    from: &str,
    to: &str,
) {
    if let Some(value) = source.get(from).filter(|value| !value.is_null()) {
        target.insert(to.to_string(), value.clone());
    }
}

pub(super) fn bounded_text_excerpt(text: &str, max_chars: usize) -> (String, bool) {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return (text.to_string(), true);
    }
    let mut excerpt = text.chars().take(max_chars).collect::<String>();
    let minimum_boundary = excerpt.len() / 2;
    if let Some(boundary) = excerpt
        .char_indices()
        .rev()
        .find(|(index, ch)| *index >= minimum_boundary && matches!(ch, '.' | '!' | '?' | '\n'))
        .map(|(index, ch)| index + ch.len_utf8())
    {
        excerpt.truncate(boundary);
    }
    let excerpt = excerpt.trim_end();
    (format!("{excerpt}…"), false)
}

fn aggregate_costs(results: &[BatchItemResult]) -> Map<String, Value> {
    let mut totals = BTreeMap::<String, u64>::new();
    for result in results.iter().filter_map(|result| result.result.as_ref()) {
        let Some(costs) = result
            .get("total_cost_microunits")
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (commodity, value) in costs {
            if let Some(amount) = value.as_u64() {
                totals
                    .entry(commodity.clone())
                    .and_modify(|total| *total = total.saturating_add(amount))
                    .or_insert(amount);
            }
        }
    }
    totals
        .into_iter()
        .map(|(commodity, total)| (commodity, json!(total)))
        .collect()
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    use super::*;
    use crate::magician_v2::{
        execution::capability::CapabilityPackDefinition,
        tool_result_projection::{
            ConservativeTokenEstimator, DisplayResultProjection, ProjectionBudget,
            ProjectionContractRegistry, ProjectionStrategy, RawResultDescriptor,
            ResultRetentionClass, ScopedResultRef, ToolOutcome, ToolResultIdentity,
            ToolResultProjectionRequest, ToolResultProjector,
        },
    };

    fn settings(max_parallel_actions: usize) -> ProgressiveRetrievalSettings {
        ProgressiveRetrievalSettings {
            max_parallel_actions,
            default_deadline_ms: 1_000,
            max_attempts: max_parallel_actions.max(1),
            ..ProgressiveRetrievalSettings::default()
        }
    }

    fn scoped_args(requests: Value) -> Value {
        json!({
            "__principal": "owner",
            "__workspace": "default",
            "requests": requests,
        })
    }

    #[test]
    fn parsing_preserves_order_forces_scope_and_clamps_concurrency_to_policy() {
        let args = json!({
            "__principal": "owner",
            "__workspace": "default",
            "common": {"limit": 2},
            // Defensive compatibility: shared scalar-shaped fields are folded
            // into common instead of wasting an LLM iteration. `timeout_secs`
            // is also injected by the pack executor after model generation.
            "fresh": true,
            "timeout_secs": 25,
            "requests": [
                {"id": "openai", "query": "OpenAI prices"},
                {"id": "sarvam", "query": "Sarvam prices", "limit": 1}
            ],
            "max_concurrency": 4,
        });
        let batch = parse_batch(&args, &settings(2), BatchKind::Search).unwrap();
        assert_eq!(batch.max_concurrency, 2);
        assert_eq!(batch.items[0].id, "openai");
        assert_eq!(batch.items[1].id, "sarvam");
        assert_eq!(batch.items[0].args["limit"], json!(2));
        assert_eq!(batch.items[1].args["limit"], json!(1));
        assert_eq!(batch.items[0].args["fresh"], json!(true));
        assert_eq!(batch.items[1].args["fresh"], json!(true));
        assert_eq!(batch.items[0].args["timeout_secs"], json!(25));
        assert_eq!(batch.items[1].args["timeout_secs"], json!(25));
        assert_eq!(batch.items[0].args["__principal"], json!("owner"));
    }

    #[test]
    fn one_vector_item_is_serial_while_branch_owned_scalar_fields_stay_strict() {
        let batch = parse_batch(
            &scoped_args(json!([{"id": "only", "query": "one need"}])),
            &settings(4),
            BatchKind::Search,
        )
        .unwrap();
        assert_eq!(batch.items.len(), 1);
        assert_eq!(batch.max_concurrency, 1);
        assert_eq!(batch.per_host_concurrency, 1);

        let mixed = json!({
            "__principal": "owner",
            "__workspace": "default",
            "query": "ambiguous scalar",
            "requests": [{"query": "vector"}]
        });
        assert!(parse_batch(&mixed, &settings(2), BatchKind::Search).is_err());
        assert!(parse_batch(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "requests": [{"query": "one"}],
                "per_host_concurrency": 1
            }),
            &settings(2),
            BatchKind::Search,
        )
        .is_err());

        assert!(parse_batch(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "url": "https://scalar.example",
                "requests": [{"url": "https://vector.example"}]
            }),
            &settings(2),
            BatchKind::Read,
        )
        .is_err());
    }

    #[test]
    fn conflicting_top_level_and_common_shared_values_fail_closed() {
        let conflicting = json!({
            "__principal": "owner",
            "__workspace": "default",
            "fresh": true,
            "common": {"fresh": false},
            "requests": [{"query": "one"}, {"query": "two"}]
        });
        let error = parse_batch(&conflicting, &settings(2), BatchKind::Search)
            .expect_err("conflicting shared values must not pick one silently");
        assert!(error.to_string().contains("different values"));
    }

    #[tokio::test]
    async fn one_vector_item_runs_once_and_reports_no_parallel_overlap() {
        let batch = parse_batch(
            &scoped_args(json!([{"id": "only", "query": "one need"}])),
            &settings(4),
            BatchKind::Search,
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let output = run_batch(batch, Instant::now(), CancellationToken::new(), {
            let calls = Arc::clone(&calls);
            move |_args, _| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"status": "complete"}))
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(output["requested_count"], 1);
        assert_eq!(output["max_concurrency"], 1);
        assert_eq!(output["per_host_concurrency"], 1);
        assert_eq!(output["parallel_overlap_saved_ms"], 0);
        assert_eq!(output["status"], "complete");
    }

    #[test]
    fn read_batch_defaults_to_gist_but_preserves_explicit_full_text() {
        let batch = parse_batch(
            &scoped_args(json!([
                {"url": "https://one.test"},
                {"url": "https://two.test", "depth": "full_text"},
                {"url": "https://three.test", "output_kind": "full_text"}
            ])),
            &settings(2),
            BatchKind::Read,
        )
        .unwrap();
        assert_eq!(batch.items[0].args["depth"], "gist");
        assert_eq!(batch.items[1].args["depth"], "full_text");
        assert!(batch.items[2].args.get("depth").is_none());
        assert_eq!(batch.items[2].args["output_kind"], "full_text");
    }

    #[test]
    fn read_batch_keeps_working_set_capture_at_the_outer_result_boundary() {
        let batch = parse_batch(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "working_set_title": "Rust release comparison",
                "requests": [{"url": "https://one.test"}, {"url": "https://two.test"}],
            }),
            &settings(2),
            BatchKind::Read,
        )
        .unwrap();
        assert!(batch
            .items
            .iter()
            .all(|item| item.args.get("working_set_title").is_none()));
    }

    #[test]
    fn parsing_rejects_runtime_scope_injection_and_duplicate_ids() {
        assert!(parse_batch(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "common": {"__principal": "attacker"},
                "requests": [{"query": "one"}]
            }),
            &settings(2),
            BatchKind::Search,
        )
        .is_err());
        assert!(parse_batch(
            &scoped_args(json!([
                {"id": "same", "query": "one"},
                {"id": "same", "query": "two"}
            ])),
            &settings(2),
            BatchKind::Search,
        )
        .is_err());
    }

    #[test]
    fn parsing_keeps_branch_identity_and_authority_grants_out_of_common() {
        for (kind, common, request) in [
            (
                BatchKind::Search,
                json!({"query": "shared query"}),
                json!({"query": "branch query"}),
            ),
            (
                BatchKind::Read,
                json!({"url": "https://shared.test"}),
                json!({"url": "https://branch.test"}),
            ),
            (
                BatchKind::Read,
                json!({"authority_grant_id": "grant-for-one-origin"}),
                json!({"url": "https://branch.test"}),
            ),
        ] {
            assert!(parse_batch(
                &json!({
                    "__principal": "owner",
                    "__workspace": "default",
                    "common": common,
                    "requests": [request],
                }),
                &settings(2),
                kind,
            )
            .is_err());
        }
    }

    #[tokio::test]
    async fn fanout_is_bounded_and_results_remain_in_request_order() {
        let batch = parse_batch(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "requests": [
                    {"id": "slow", "query": "one"},
                    {"id": "fast", "query": "two"},
                    {"id": "last", "query": "three"}
                ],
                "minimum_successes": 3,
                "max_concurrency": 2,
            }),
            &settings(2),
            BatchKind::Search,
        )
        .unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let output = run_batch(batch, Instant::now(), CancellationToken::new(), {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            move |args, _| {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    let delay = if args["query"] == "one" { 30 } else { 5 };
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(json!({"status": "complete", "query": args["query"]}))
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(output["results"][0]["id"], "slow");
        assert_eq!(output["results"][1]["id"], "fast");
        assert_eq!(output["results"][2]["id"], "last");
        assert_eq!(output["status"], "complete");
    }

    #[tokio::test]
    async fn minimum_successes_cancels_only_batch_siblings() {
        let batch = parse_batch(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "requests": [
                    {"query": "one"}, {"query": "two"}, {"query": "three"}
                ],
                "minimum_successes": 1,
                "max_concurrency": 1,
            }),
            &settings(2),
            BatchKind::Search,
        )
        .unwrap();
        let parent = CancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let output = run_batch(batch, Instant::now(), parent.clone(), {
            let calls = Arc::clone(&calls);
            move |_args, _| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"status": "complete"}))
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(output["complete_count"], 1);
        assert_eq!(output["cancelled_count"], 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!parent.is_cancelled());
    }

    #[tokio::test]
    async fn returned_failed_envelopes_do_not_misreport_batch_as_partial() {
        let failed = parse_batch(
            &scoped_args(json!([{"query": "one"}, {"query": "two"}])),
            &settings(2),
            BatchKind::Search,
        )
        .unwrap();
        let output = run_batch(
            failed,
            Instant::now(),
            CancellationToken::new(),
            |_args, _| async { Ok(json!({"status": "failed"})) },
        )
        .await
        .unwrap();
        assert_eq!(output["returned_count"], 2);
        assert_eq!(output["usable_count"], 0);
        assert_eq!(output["status"], "failed");

        let degraded = parse_batch(
            &scoped_args(json!([{"query": "one"}, {"query": "two"}])),
            &settings(2),
            BatchKind::Search,
        )
        .unwrap();
        let output = run_batch(
            degraded,
            Instant::now(),
            CancellationToken::new(),
            |_args, _| async { Ok(json!({"status": "degraded"})) },
        )
        .await
        .unwrap();
        assert_eq!(output["usable_count"], 2);
        assert_eq!(output["status"], "partial");
    }

    #[tokio::test]
    async fn parent_cancellation_wins_over_a_racing_complete_branch() {
        let batch = parse_batch(
            &scoped_args(json!([{"query": "one"}])),
            &settings(1),
            BatchKind::Search,
        )
        .unwrap();
        let parent = CancellationToken::new();
        let output = run_batch(batch, Instant::now(), parent.clone(), {
            let parent = parent.clone();
            move |_args, _| {
                let parent = parent.clone();
                async move {
                    parent.cancel();
                    Ok(json!({"status": "complete"}))
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(output["complete_count"], 1);
        assert_eq!(output["status"], "cancelled");
    }

    #[tokio::test]
    async fn large_read_results_retain_bounded_model_visible_evidence() {
        let batch = parse_batch(
            &scoped_args(json!([{"id": "official", "url": "https://example.test/pricing"}])),
            &settings(2),
            BatchKind::Read,
        )
        .unwrap();
        let full_text = format!("Official pricing. {}", "large evidence body ".repeat(400));
        let expected_full_text = full_text.clone();
        let output = run_batch(
            batch,
            Instant::now(),
            CancellationToken::new(),
            move |_args, _| {
                let full_text = full_text.clone();
                async move {
                    Ok(json!({
                        "status": "complete",
                        "total_cost_microunits": {"usd": 7},
                        "document": {
                            "title": "Official pricing",
                            "canonical_url": "https://example.test/pricing",
                            "fetched_at_ms": 1_700_000_000_000i64,
                            "text": full_text,
                            "provenance": {"source_label": "Example"}
                        }
                    }))
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(
            output["results"][0]["result"]["document"]["text"],
            expected_full_text
        );
        assert_eq!(output["total_cost_microunits"]["usd"], 7);
        assert_eq!(output["evidence_count"], 1);
        assert_eq!(output["evidence"][0]["branch_id"], "official");
        assert_eq!(
            output["evidence"][0]["requested_url"],
            "https://example.test/pricing"
        );
        assert_eq!(
            output["evidence"][0]["final_url"],
            "https://example.test/pricing"
        );
        assert_eq!(output["evidence"][0]["redirected"], false);
        assert_eq!(output["evidence"][0]["fetch_status"], "complete");
        assert_eq!(output["evidence"][0]["evidence_role"], "opened_page");
        assert_eq!(output["evidence"][0]["claim_eligible"], true);
        assert_eq!(output["evidence"][0]["excerpt_complete"], false);
        assert!(
            output["evidence"][0]["excerpt"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= READ_EVIDENCE_TEXT_CHARS + 1
        );

        let pack: CapabilityPackDefinition =
            serde_yaml::from_str(include_str!("../embedded_pack_defs/content_read.yaml")).unwrap();
        let contract = pack.result_projection.expect("vector projection contract");
        let mut registry = ProjectionContractRegistry::default();
        registry.register_override(contract.clone()).unwrap();
        let projector = ToolResultProjector::new(registry, ConservativeTokenEstimator);
        let raw = RawResultDescriptor {
            content_ref: ScopedResultRef {
                result_ref: "result_ref_v1_fixture".into(),
                cursor: None,
            },
            content_hash: "fixture-hash".into(),
            media_type: "application/json".into(),
            size_bytes: serde_json::to_vec(&output).unwrap().len() as u64,
            retention_class: ResultRetentionClass::TaskExecution,
        };
        let mut budget = ProjectionBudget::default();
        budget.max_serialized_bytes = 3_000;
        let projection = projector
            .project(ToolResultProjectionRequest {
                identity: ToolResultIdentity {
                    tool_name: "content_read".into(),
                    tool_call_id: "call-fixture".into(),
                    execution_id: Some("exec-fixture".into()),
                    task_id: Some("task-fixture".into()),
                    scope_digest: "scope-fixture".into(),
                    authority_revision: "authority-fixture".into(),
                },
                outcome: ToolOutcome::succeeded(),
                raw_result: &output,
                display: DisplayResultProjection::referenced(&raw),
                raw,
                spoken_hint: None,
                contract_id: Some(&contract.contract_id),
                budget,
            })
            .unwrap();
        assert_ne!(projection.model.strategy, ProjectionStrategy::ReferenceOnly);
        assert!(projection.model.value["data"]["evidence"]
            .as_array()
            .is_some_and(|records| !records.is_empty()));
    }

    #[test]
    fn inline_read_projection_remains_discovery_only_and_not_claim_eligible() {
        let results = vec![BatchItemResult {
            index: 0,
            id: "snippet".into(),
            requested_url: Some("https://example.test/pricing".into()),
            outcome: "returned",
            duration_ms: 1,
            result: Some(json!({
                "status": "complete",
                "document": {
                    "title": "Search snippet",
                    "canonical_url": "https://example.test/pricing",
                    "media_type": "text/plain; source=inline",
                    "text": "A receipt-verified discovery snippet that was not fetched.",
                    "metadata": {
                        "fetch_status": "not_fetched",
                        "evidence_role": "discovery_only",
                        "claim_eligible": false
                    }
                }
            })),
            error: None,
        }];

        let evidence = bounded_evidence(BatchKind::Read, &results);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0]["fetch_status"], "not_fetched");
        assert_eq!(evidence[0]["evidence_role"], "discovery_only");
        assert_eq!(evidence[0]["claim_eligible"], false);
    }

    #[test]
    fn bounded_excerpt_is_unicode_safe_and_marks_partial_text() {
        let text = "🧠".repeat(2_000);
        let (excerpt, complete) = bounded_text_excerpt(&text, 101);
        assert!(!complete);
        assert!(excerpt.ends_with('…'));
        assert_eq!(excerpt.chars().count(), 102);
    }

    #[test]
    fn search_evidence_round_robins_branches_before_lower_ranked_candidates() {
        let results = ["alpha", "beta", "gamma"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| BatchItemResult {
                index,
                id: id.into(),
                requested_url: None,
                outcome: "returned",
                duration_ms: 1,
                result: Some(json!({
                    "status": "complete",
                    "candidates": [
                        {
                            "candidate": {"title": format!("{id} first"), "canonical_url": format!("https://{id}.test/1"), "cheap_text": "first evidence"},
                            "selection_receipt": {"id": format!("receipt-{id}-1"), "expires_at_ms": 2_000_000_000_000i64}
                        },
                        {
                            "candidate": {"title": format!("{id} second"), "canonical_url": format!("https://{id}.test/2"), "cheap_text": "second evidence"},
                            "selection_receipt": {"id": format!("receipt-{id}-2"), "expires_at_ms": 2_000_000_000_000i64}
                        }
                    ]
                })),
                error: None,
            })
            .collect::<Vec<_>>();

        let evidence = bounded_evidence(BatchKind::Search, &results);
        let order = evidence
            .iter()
            .map(|record| record["branch_id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(order, ["alpha", "beta", "gamma", "alpha", "beta", "gamma"]);
        assert_eq!(evidence[0]["selection_receipt"], "receipt-alpha-1");
        assert_eq!(evidence[0]["evidence_role"], "discovery_only");
        assert_eq!(evidence[0]["claim_eligible"], false);
        assert_eq!(
            evidence[0]["selection_receipt_expires_at_ms"],
            2_000_000_000_000i64
        );
    }

    #[test]
    fn failed_or_cancelled_envelopes_never_enter_model_visible_evidence() {
        let branch = |index, id: &str, status: &str| BatchItemResult {
            index,
            id: id.into(),
            requested_url: Some(format!("https://{id}.test/requested")),
            outcome: "returned",
            duration_ms: 1,
            result: Some(json!({
                "status": status,
                "candidates": [{
                    "candidate": {
                        "title": format!("{id} title"),
                        "canonical_url": format!("https://{id}.test"),
                        "cheap_text": format!("{id} evidence")
                    }
                }],
                "document": {
                    "title": format!("{id} title"),
                    "canonical_url": format!("https://{id}.test"),
                    "text": format!("{id} evidence")
                }
            })),
            error: None,
        };
        let results = vec![
            branch(0, "complete", "complete"),
            branch(1, "degraded", "degraded"),
            branch(2, "failed", "failed"),
            branch(3, "cancelled", "cancelled"),
        ];

        for kind in [BatchKind::Search, BatchKind::Read] {
            let evidence = bounded_evidence(kind, &results);
            let ids = evidence
                .iter()
                .map(|record| record["branch_id"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(ids, ["complete", "degraded"]);
        }
    }

    #[test]
    fn unified_pack_contracts_publish_scalar_and_vector_schemas() {
        let search: CapabilityPackDefinition =
            serde_yaml::from_str(include_str!("../embedded_pack_defs/content_search.yaml"))
                .unwrap();
        let read: CapabilityPackDefinition =
            serde_yaml::from_str(include_str!("../embedded_pack_defs/content_read.yaml")).unwrap();
        let search_contract = search.result_projection.as_ref().unwrap();
        let read_contract = read.result_projection.as_ref().unwrap();
        assert_ne!(search_contract.contract_id, read_contract.contract_id);
        assert!(read_contract.atomic_field_groups.contains(&vec![
            "fetch_status".into(),
            "evidence_role".into(),
            "claim_eligible".into(),
        ]));

        for (pack, required_field) in [(&search, "query"), (&read, "url")] {
            let requests = pack
                .parameters
                .iter()
                .find(|parameter| parameter.name == "requests")
                .unwrap();
            assert_eq!(requests.schema["type"], "array");
            assert_eq!(requests.schema["items"]["type"], "object");
            assert!(requests.schema["items"]["properties"]
                .get(required_field)
                .is_some());
            assert_eq!(requests.schema["items"]["additionalProperties"], false);
            let common = pack
                .parameters
                .iter()
                .find(|parameter| parameter.name == "common")
                .unwrap();
            assert!(common.schema["properties"].get(required_field).is_none());
            assert!(pack
                .parameters
                .iter()
                .any(|parameter| parameter.name == required_field));
            assert!(pack
                .parameters
                .iter()
                .any(|parameter| parameter.name == "max_concurrency"));
            assert!(pack
                .parameters
                .iter()
                .any(|parameter| parameter.name == "minimum_successes"));
        }
        assert!(read
            .parameters
            .iter()
            .any(|parameter| parameter.name == "per_host_concurrency"));
        assert!(!read
            .parameters
            .iter()
            .any(|parameter| parameter.name == "inline_text"));
        let read_requests = read
            .parameters
            .iter()
            .find(|parameter| parameter.name == "requests")
            .unwrap();
        assert!(read_requests.schema["items"]["properties"]
            .get("inline_text")
            .is_none());
    }

    #[tokio::test]
    async fn shared_deadline_includes_work_before_branch_dispatch() {
        let batch = parse_batch(
            &scoped_args(json!([{"query": "one"}, {"query": "two"}])),
            &settings(2),
            BatchKind::Search,
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let output = run_batch(
            batch,
            Instant::now()
                .checked_sub(std::time::Duration::from_millis(1_001))
                .unwrap(),
            CancellationToken::new(),
            {
                let calls = Arc::clone(&calls);
                move |_args, _| {
                    let calls = Arc::clone(&calls);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(json!({"status": "complete"}))
                    }
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(output["cancelled_count"], 2);
        assert_eq!(output["status"], "failed");
    }

    #[test]
    fn read_batches_group_canonical_urls_by_host() {
        let batch = parse_batch(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "requests": [
                    {"url": "https://EXAMPLE.com/one"},
                    {"url": "https://example.com/two"},
                    {"url": "https://other.example/three"}
                ],
                "per_host_concurrency": 9,
            }),
            &settings(4),
            BatchKind::Read,
        )
        .unwrap();
        assert_eq!(batch.items[0].host_key.as_deref(), Some("example.com"));
        assert_eq!(batch.items[1].host_key.as_deref(), Some("example.com"));
        assert_eq!(batch.items[2].host_key.as_deref(), Some("other.example"));
        assert_eq!(batch.per_host_concurrency, 2);
    }

    #[tokio::test]
    async fn busy_host_does_not_block_an_independent_host() {
        let batch = parse_batch(
            &json!({
                "__principal": "owner",
                "__workspace": "default",
                "requests": [
                    {"url": "https://same.example/one"},
                    {"url": "https://same.example/two"},
                    {"url": "https://other.example/three"}
                ],
                "max_concurrency": 2,
                "per_host_concurrency": 1,
            }),
            &settings(2),
            BatchKind::Read,
        )
        .unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let host_active = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let host_peak = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let output = run_batch(batch, Instant::now(), CancellationToken::new(), {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            let host_active = Arc::clone(&host_active);
            let host_peak = Arc::clone(&host_peak);
            move |args, _| {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                let host_active = Arc::clone(&host_active);
                let host_peak = Arc::clone(&host_peak);
                async move {
                    let host = url::Url::parse(args["url"].as_str().unwrap())
                        .unwrap()
                        .host_str()
                        .unwrap()
                        .to_string();
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    {
                        let mut by_host = host_active.lock().unwrap();
                        let host_now = by_host.entry(host.clone()).or_default();
                        *host_now += 1;
                        let mut peaks = host_peak.lock().unwrap();
                        peaks
                            .entry(host.clone())
                            .and_modify(|peak| *peak = (*peak).max(*host_now))
                            .or_insert(*host_now);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    {
                        let mut by_host = host_active.lock().unwrap();
                        *by_host.get_mut(&host).unwrap() -= 1;
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(json!({"status": "complete"}))
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(output["status"], "complete");
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(host_peak.lock().unwrap()["same.example"], 1);
    }
}
