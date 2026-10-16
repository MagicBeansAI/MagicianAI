//! `app_data_search` — personal-agent contains search over one enabled installation.

use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;
use tracing::warn;

use super::app_data::require_direct_personal_agent_context;
use super::app_data_query::{
    parse_optional_typed, parse_query_request, parse_select, preflight_args,
    publish_app_data_unavailable, reject_unknown_args,
};
use super::shared::require_scope_str;
use crate::magician_v2::apps::entity_adapter::AppGenericDataToolAdapter;
use crate::magician_v2::apps::models::{
    AppComparisonOperator, AppContractLimits, AppFieldPath, AppPredicate, AppPredicateNode,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::compiled_dispatch::publish_current_compiled_app_result_guard;
use crate::magician_v2::execution::error::ExecutionError;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    preflight_args(&args, "app_data_search")?;
    reject_unknown_args(
        &args,
        "app_data_search",
        &[
            "installation_id",
            "entity",
            "query",
            "field",
            "select",
            "predicate",
            "order",
            "cursor",
            "limit",
            "relation_expansions",
        ],
    )?;
    let query = require_scope_str(&args, "query", "app_data_search")?;
    let field = AppFieldPath::parse(match args.get("field") {
        None => "title".to_owned(),
        Some(Value::String(field)) if !field.is_empty() => field.clone(),
        Some(_) => {
            return Err(ExecutionError::Step(
                "app_data_search field must be a non-empty string".into(),
            ))
        },
    })
    .map_err(|error| ExecutionError::Step(format!("app_data_search field: {error}")))?;
    let select =
        parse_select(&args, "app_data_search", false)?.unwrap_or_else(|| vec![field.clone()]);
    let filter: Option<AppPredicate> = parse_optional_typed(&args, "predicate", "app_data_search")?;
    let contains = AppPredicate {
        root: 0,
        nodes: vec![AppPredicateNode::Compare {
            field,
            operator: AppComparisonOperator::Contains,
            value: Value::String(query),
        }],
    };
    let predicate = Some(combine_search_predicate(filter, contains)?);
    let request = parse_query_request(
        &args,
        "app_data_search",
        select,
        predicate,
        "personal_agent_search",
    )?;
    let context =
        require_direct_personal_agent_context(Some(resources.as_ref()), &args, "app_data_search")?;
    let processing_class = context.authority.processing_class();
    let Some(artifact_service) = resources.artifact_v2_service.as_ref() else {
        warn!("app_data_search registry runtime is unavailable");
        return publish_app_data_unavailable(
            resources.as_ref(),
            &context.authenticated,
            &context.calling_profile_name,
            &context.publication_fence,
            processing_class,
            "app_data_search",
        );
    };
    let workflow = artifact_service.app_workflow_service();
    let adapter = AppGenericDataToolAdapter::new(workflow.registry_service());
    let execution_ref = context.authority.execution_ref().clone();
    let now = Utc::now();
    let governed = match adapter
        .search_governed(
            &context.authenticated,
            context.authority,
            request.clone(),
            now,
        )
        .await
    {
        Ok(governed) => governed,
        Err(error) => {
            warn!(error = ?error, "app_data_search governed read is unavailable");
            return publish_app_data_unavailable(
                resources.as_ref(),
                &context.authenticated,
                &context.calling_profile_name,
                &context.publication_fence,
                processing_class,
                "app_data_search",
            );
        },
    };
    let completion_now = Utc::now();
    super::app_data::reattest_personal_agent_publication(
        resources.as_ref(),
        &context.authenticated,
        &context.calling_profile_name,
        &context.publication_fence,
        "app_data_search",
        completion_now,
    )?;
    let (result, policy) = match workflow.register_personal_agent_projection(
        &context.authenticated,
        &execution_ref,
        request,
        governed,
        context.publication_fence.expires_at(),
        completion_now,
    ) {
        Ok(result) => result,
        Err(error) => {
            warn!(error = ?error, "app_data_search projection handle is unavailable");
            return publish_app_data_unavailable(
                resources.as_ref(),
                &context.authenticated,
                &context.calling_profile_name,
                &context.publication_fence,
                processing_class,
                "app_data_search",
            );
        },
    };
    let projection_handle = result.projection_handle.clone();
    let value = match serde_json::to_value(result) {
        Ok(value) => value,
        Err(error) => {
            warn!(error = ?error, "app_data_search result serialization is unavailable");
            return publish_app_data_unavailable(
                resources.as_ref(),
                &context.authenticated,
                &context.calling_profile_name,
                &context.publication_fence,
                processing_class,
                "app_data_search",
            );
        },
    };
    publish_current_compiled_app_result_guard(policy, &value, Some(projection_handle))?;
    Ok(value)
}

fn combine_search_predicate(
    filter: Option<AppPredicate>,
    search: AppPredicate,
) -> Result<AppPredicate, ExecutionError> {
    let Some(mut filter) = filter else {
        return Ok(search);
    };
    let max_filter_nodes = AppContractLimits::default()
        .max_predicate_nodes()
        .saturating_sub(2);
    if filter.nodes.len() > max_filter_nodes {
        return Err(ExecutionError::Step(format!(
            "app_data_search filter exceeds the {max_filter_nodes}-node ceiling"
        )));
    }
    let search_node = search
        .nodes
        .into_iter()
        .next()
        .ok_or_else(|| ExecutionError::Step("app_data_search predicate is empty".into()))?;
    let search_index = u16::try_from(filter.nodes.len()).map_err(|_| {
        ExecutionError::Step("app_data_search predicate contains too many nodes".into())
    })?;
    filter.nodes.push(search_node);
    let conjunction_index = u16::try_from(filter.nodes.len()).map_err(|_| {
        ExecutionError::Step("app_data_search predicate contains too many nodes".into())
    })?;
    filter.nodes.push(AppPredicateNode::All {
        children: vec![filter.root, search_index],
    });
    filter.root = conjunction_index;
    Ok(filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::apps::models::ValidateAppContract;

    #[test]
    fn search_combines_text_match_with_a_canonical_filter_arena() {
        let filter: AppPredicate = serde_json::from_value(serde_json::json!({
            "root": 0,
            "nodes": [{"kind": "is_null", "field": "archived_at", "negated": false}]
        }))
        .unwrap();
        let search = AppPredicate {
            root: 0,
            nodes: vec![AppPredicateNode::Compare {
                field: AppFieldPath::parse("title").unwrap(),
                operator: AppComparisonOperator::Contains,
                value: Value::String("planning".to_owned()),
            }],
        };
        let combined = combine_search_predicate(Some(filter), search).unwrap();
        assert_eq!(usize::from(combined.root), combined.nodes.len() - 1);
        let request_args = serde_json::json!({
            "installation_id": "install-1",
            "entity": "notes"
        });
        let request = parse_query_request(
            &request_args,
            "app_data_search",
            vec![AppFieldPath::parse("title").unwrap()],
            Some(combined),
            "personal_agent_search",
        )
        .unwrap();
        request
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();
    }

    #[test]
    fn search_rejects_forged_authority_fields() {
        let args = serde_json::json!({"schema_revision": 7});
        assert!(reject_unknown_args(
            &args,
            "app_data_search",
            &[
                "installation_id",
                "entity",
                "query",
                "field",
                "select",
                "predicate",
                "order",
                "cursor",
                "limit",
                "relation_expansions",
            ]
        )
        .is_err());
    }

    #[test]
    fn search_reserves_two_canonical_predicate_nodes_for_contains_and_all() {
        let field = AppFieldPath::parse("title").unwrap();
        let filter = AppPredicate {
            root: 0,
            nodes: (0..127)
                .map(|_| AppPredicateNode::IsNull {
                    field: field.clone(),
                    negated: false,
                })
                .collect(),
        };
        let search = AppPredicate {
            root: 0,
            nodes: vec![AppPredicateNode::Compare {
                field,
                operator: AppComparisonOperator::Contains,
                value: Value::String("planning".to_owned()),
            }],
        };
        assert!(combine_search_predicate(Some(filter), search).is_err());
    }
}
