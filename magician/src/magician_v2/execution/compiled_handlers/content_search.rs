use std::{collections::BTreeMap, sync::Arc};

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::magician_v2::{
    content_sources::{
        ContentAcquisitionResolver, ContentAcquisitionService, DiscoveryEvidenceGoal, EvidenceGoal,
        FreshnessPolicy, ProgressiveRetrievalSettings, RemoteDataPolicy, RetrievalAuthority,
        RetrievalNeed, RetrievalOperation, RetrievalTarget, RETRIEVAL_NEED_SCHEMA_VERSION,
    },
    execution::{agent_resources::AgentResources, error::ExecutionError},
};

use super::shared::require_scope_str;

// Every hop in this file heap-owns the next one. These are thin pass-through
// wrappers, but each awaited its successor inline, so the whole retrieval-ladder
// state machine underneath was nested once per hop and every frame in the chain
// carried it. Boxing makes each hop's frame independent of what it calls.
pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    Box::pin(execute(resources, args)).await
}

pub async fn execute(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let resolver = resources.content_acquisition_resolver().ok_or_else(|| {
        ExecutionError::Step("content acquisition resolver is not configured".into())
    })?;
    let settings = resources
        .magician_config_snapshot()
        .content_acquisition
        .progressive_retrieval;
    Box::pin(execute_with_runtime(resolver, settings, args)).await
}

/// Execute the compiled handler against an explicitly supplied production
/// resolver. This keeps CLI/live evaluators on the same argument parsing,
/// ladder, authority, and result path as normal compiled-tool dispatch.
pub async fn execute_with_runtime(
    resolver: Arc<ContentAcquisitionResolver>,
    settings: ProgressiveRetrievalSettings,
    args: Value,
) -> Result<Value, ExecutionError> {
    Box::pin(execute_with_runtime_and_cancellation(
        resolver,
        settings,
        args,
        execution_cancellation_token(),
    ))
    .await
}

/// Run one search with an explicit cancellation scope. Vector acquisition uses
/// a child token so it can stop interchangeable branches without cancelling
/// the owning execution; ordinary callers retain the compiled-dispatch token.
pub async fn execute_with_runtime_and_cancellation(
    resolver: Arc<ContentAcquisitionResolver>,
    settings: ProgressiveRetrievalSettings,
    args: Value,
    cancellation: CancellationToken,
) -> Result<Value, ExecutionError> {
    if uses_vector_contract(&args) {
        return Box::pin(
            super::content_batch::execute_search_vector_with_runtime_and_cancellation(
                resolver,
                settings,
                args,
                cancellation,
            ),
        )
        .await;
    }
    let principal = require_scope_str(&args, "__principal", "content_search")?;
    let workspace = require_scope_str(&args, "__workspace", "content_search")?;
    let service = resolver
        .resolve(principal, workspace)
        .await
        .map_err(|error| ExecutionError::Step(format!("resolving content acquisition: {error}")))?;
    Box::pin(execute_with_service(service, settings, args, cancellation)).await
}

fn uses_vector_contract(args: &Value) -> bool {
    [
        "requests",
        "common",
        "max_concurrency",
        "minimum_successes",
        "per_host_concurrency",
    ]
    .iter()
    .any(|field| args.get(*field).is_some())
}

pub async fn execute_with_service(
    service: Arc<ContentAcquisitionService>,
    settings: ProgressiveRetrievalSettings,
    args: Value,
    cancellation: CancellationToken,
) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "content_search")?;
    let workspace = require_scope_str(&args, "__workspace", "content_search")?;
    let Some(query) = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
    else {
        return Ok(json!({"status": "error", "reason": "content_search requires `query`"}));
    };
    let limit = usize_arg(&args, "limit", 10).clamp(1, 50);
    let min_candidates = usize_arg(&args, "min_candidates", 3).clamp(1, limit);
    let min_sources = usize_arg(&args, "min_sources", 1).clamp(1, min_candidates);
    let need = RetrievalNeed {
        schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
        principal,
        workspace,
        operation: RetrievalOperation::Discover,
        target: RetrievalTarget::Query {
            query: query.to_string(),
            intent: args
                .get("intent")
                .and_then(Value::as_str)
                .map(str::to_string),
            targets: string_list(&args, "targets"),
            inline_candidates: Vec::new(),
            limit,
            cursor: None,
            options: discovery_options(&args)?,
        },
        goal: EvidenceGoal::Discovery(DiscoveryEvidenceGoal {
            min_candidates,
            min_relevant_candidates: min_candidates,
            min_independent_sources: min_sources,
            relevance_threshold: args
                .get("relevance_threshold")
                .and_then(Value::as_f64)
                .unwrap_or(0.15),
        }),
        freshness: if args.get("fresh").and_then(Value::as_bool) == Some(true) {
            FreshnessPolicy::Fresh
        } else {
            FreshnessPolicy::CachedOk
        },
        remote_query_policy: RemoteDataPolicy::Allow,
        remote_content_policy: RemoteDataPolicy::Deny,
        invocation_source:
            crate::magician_v2::content_sources::ContentInvocationSource::InteractiveRead,
        maximum_authority: RetrievalAuthority::PublicBrowserInteract,
        authority_grant_id: None,
        deadline_ms: args.get("deadline_ms").and_then(Value::as_u64),
        max_attempts: args
            .get("max_attempts")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        cost_budget_microunits: cost_budget(&args)?,
        allowed_actions: string_list(&args, "allowed_actions"),
    };
    // The ladder below this call is the deepest part of the whole dispatch, so it
    // is heap-owned like every other hop in this file.
    let result = Box::pin(
        service
            .retrieval_controller(settings)
            .retrieve(need, cancellation),
    )
    .await
    .map_err(|error| ExecutionError::Step(format!("content search failed: {error}")))?;
    serde_json::to_value(result)
        .map_err(|error| ExecutionError::Step(format!("serializing content search: {error}")))
}

fn execution_cancellation_token() -> CancellationToken {
    crate::magician_v2::execution::compiled_dispatch::EXECUTION_CANCEL_TOKEN
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn usize_arg(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

fn string_list(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn discovery_options(args: &Value) -> Result<BTreeMap<String, Value>, ExecutionError> {
    let mut options = match args.get("options") {
        None => BTreeMap::new(),
        Some(Value::Object(values)) => values
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        Some(_) => {
            return Err(ExecutionError::Step(
                "content_search `options` must be an object".into(),
            ));
        },
    };
    for key in [
        "allowed_domains",
        "blocked_domains",
        "days",
        "subreddit",
        "category",
        "mode",
        "search_depth",
        "topic",
        "time_range",
        "start_date",
        "end_date",
        "include_domains",
        "exclude_domains",
    ] {
        if let Some(value) = args.get(key) {
            options
                .entry(key.to_string())
                .or_insert_with(|| value.clone());
        }
    }
    Ok(options)
}

fn cost_budget(args: &Value) -> Result<BTreeMap<String, u64>, ExecutionError> {
    let Some(value) = args.get("cost_budget_microunits") else {
        return Ok(BTreeMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        ExecutionError::Step("content_search cost budget must be an object".into())
    })?;
    object
        .iter()
        .map(|(commodity, value)| {
            value
                .as_u64()
                .filter(|amount| *amount > 0)
                .map(|amount| (commodity.clone(), amount))
                .ok_or_else(|| {
                    ExecutionError::Step(format!(
                        "content_search cost budget `{commodity}` must be a positive integer"
                    ))
                })
        })
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn request_shape_selects_scalar_or_vector_scheduler_without_tool_renaming() {
        assert!(!uses_vector_contract(&json!({"query": "one"})));
        assert!(uses_vector_contract(
            &json!({"requests": [{"query": "one"}]})
        ));
        assert!(uses_vector_contract(&json!({"common": {"fresh": true}})));
        assert!(uses_vector_contract(&json!({"max_concurrency": 2})));
        assert!(uses_vector_contract(&json!({"minimum_successes": 1})));
    }

    #[test]
    fn generic_options_preserve_manifest_extension_without_overwriting_nested_values() {
        let args = json!({
            "options": {"future_option": "yes", "days": 7},
            "days": 30,
            "allowed_domains": ["example.com"]
        });
        let options = discovery_options(&args).unwrap();
        assert_eq!(options["future_option"], json!("yes"));
        assert_eq!(options["days"], json!(7));
        assert_eq!(options["allowed_domains"], json!(["example.com"]));
    }

    #[test]
    fn cost_budget_rejects_non_positive_or_non_integer_values() {
        assert!(cost_budget(&json!({"cost_budget_microunits": {"usd": 0}})).is_err());
        assert!(cost_budget(&json!({"cost_budget_microunits": {"usd": "5"}})).is_err());
        assert_eq!(
            cost_budget(&json!({"cost_budget_microunits": {"usd": 5000}})).unwrap()["usd"],
            5000
        );
    }

    #[tokio::test]
    async fn retrieval_inherits_compiled_dispatch_cancellation() {
        let parent = CancellationToken::new();
        crate::magician_v2::execution::compiled_dispatch::EXECUTION_CANCEL_TOKEN
            .scope(Some(parent.clone()), async move {
                let inherited = execution_cancellation_token();
                assert!(!inherited.is_cancelled());
                parent.cancel();
                assert!(inherited.is_cancelled());
            })
            .await;
    }
}
