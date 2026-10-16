use std::collections::{BTreeSet, HashMap, HashSet};

use super::{
    validate_logical_context, ChunkDescriptor, ChunkDomainAdapter, ChunkError, ChunkPlan,
    LogicalItem, LogicalItemIdentity, LogicalLlmRequest, TokenEstimator,
};

const MAX_SEMANTIC_SPLIT_DEPTH: usize = 64;

struct PlannedLeaf {
    item: LogicalItem,
    estimated_tokens: u32,
}

/// Build a deterministic, dispatch-free plan for one structured operation.
pub fn plan_logical_request(
    adapter: &dyn ChunkDomainAdapter,
    request: &LogicalLlmRequest,
    budget: super::ChunkBudget,
    estimator: &dyn TokenEstimator,
) -> Result<ChunkPlan, ChunkError> {
    if !adapter
        .supported_operations()
        .contains(&request.operation.as_str())
    {
        return Err(ChunkError::UnsupportedOperation {
            adapter: adapter.id().to_string(),
            operation: request.operation.clone(),
        });
    }

    let source_items = adapter.logical_items(&request.input)?;
    if source_items.is_empty() {
        return Err(ChunkError::EmptyLogicalInput {
            adapter: adapter.id().to_string(),
        });
    }
    validate_source_items(&source_items)?;

    let model = request.base_request.model.as_str();
    let mut logical_tokens = budget.estimated_static_overhead_tokens;
    for item in &source_items {
        logical_tokens =
            logical_tokens.saturating_add(adapter.estimate_item_tokens(item, estimator, model)?);
    }
    validate_logical_context(logical_tokens, budget.logical_window_tokens)?;

    let mut seen_identities = source_items
        .iter()
        .map(|item| item.identity.id.clone())
        .collect::<HashSet<_>>();
    let mut leaves = Vec::new();
    for item in &source_items {
        plan_item_recursive(
            adapter,
            item.clone(),
            &budget,
            estimator,
            model,
            0,
            &mut seen_identities,
            &mut leaves,
        )?;
    }

    let mut chunks = Vec::new();
    let mut current_items = Vec::new();
    let mut current_tokens = 0u32;
    let max_items_per_chunk = adapter.max_items_per_chunk().filter(|limit| *limit > 0);
    for leaf in &leaves {
        if !current_items.is_empty()
            && (current_tokens.saturating_add(leaf.estimated_tokens)
                > budget.effective_payload_tokens
                || max_items_per_chunk.is_some_and(|limit| current_items.len() >= limit))
        {
            chunks.push(ChunkDescriptor {
                index: chunks.len() as u32,
                estimated_payload_tokens: current_tokens,
                items: std::mem::take(&mut current_items),
            });
            current_tokens = 0;
        }
        current_tokens = current_tokens.saturating_add(leaf.estimated_tokens);
        current_items.push(leaf.item.identity.clone());
    }
    if !current_items.is_empty() {
        chunks.push(ChunkDescriptor {
            index: chunks.len() as u32,
            estimated_payload_tokens: current_tokens,
            items: current_items,
        });
    }

    let leaf_identities = leaves
        .iter()
        .map(|leaf| leaf.item.identity.clone())
        .collect();
    let leaf_items = leaves.into_iter().map(|leaf| leaf.item).collect();
    let plan = ChunkPlan {
        adapter_id: adapter.id().to_string(),
        adapter_version: adapter.version().to_string(),
        operation: request.operation.clone(),
        model: request.base_request.model.clone(),
        estimator: estimator.id().to_string(),
        estimated_logical_tokens: logical_tokens,
        budget,
        source_identities: source_items
            .iter()
            .map(|item| item.identity.clone())
            .collect(),
        leaf_identities,
        leaf_items,
        chunks,
    };
    validate_plan_completeness(&plan)?;
    Ok(plan)
}

#[allow(clippy::too_many_arguments)]
fn plan_item_recursive(
    adapter: &dyn ChunkDomainAdapter,
    item: LogicalItem,
    budget: &super::ChunkBudget,
    estimator: &dyn TokenEstimator,
    model: &str,
    depth: usize,
    seen_identities: &mut HashSet<String>,
    leaves: &mut Vec<PlannedLeaf>,
) -> Result<(), ChunkError> {
    let estimated_tokens = adapter.estimate_item_tokens(&item, estimator, model)?;
    if estimated_tokens <= budget.effective_payload_tokens {
        leaves.push(PlannedLeaf {
            item,
            estimated_tokens,
        });
        return Ok(());
    }
    if depth >= MAX_SEMANTIC_SPLIT_DEPTH {
        return Err(ChunkError::SplitDepthExceeded {
            adapter: adapter.id().to_string(),
            item_id: item.identity.id,
        });
    }

    let children = match adapter.split_oversized_item(&item, budget) {
        Ok(children) => children,
        Err(ChunkError::ChunkItemExceedsContextWindow { .. }) => {
            return Err(ChunkError::ChunkItemExceedsContextWindow {
                adapter: adapter.id().to_string(),
                item_id: item.identity.id,
                estimated_tokens,
                effective_payload_tokens: budget.effective_payload_tokens,
            })
        },
        Err(error) => return Err(error),
    };
    if children.is_empty() {
        return Err(ChunkError::EmptyItemSplit {
            adapter: adapter.id().to_string(),
            item_id: item.identity.id,
        });
    }

    for (index, child) in children.iter().enumerate() {
        validate_child_identity(&item.identity, &child.identity, index)?;
        if !seen_identities.insert(child.identity.id.clone()) {
            return Err(ChunkError::DuplicateItemIdentity {
                item_id: child.identity.id.clone(),
            });
        }
        let child_tokens = adapter.estimate_item_tokens(child, estimator, model)?;
        if child_tokens >= estimated_tokens {
            return Err(ChunkError::InvalidItemSplit {
                adapter: adapter.id().to_string(),
                item_id: item.identity.id.clone(),
                reason: format!(
                    "child `{}` estimate {} does not reduce parent estimate {}",
                    child.identity.id, child_tokens, estimated_tokens
                ),
            });
        }
    }

    for child in children {
        plan_item_recursive(
            adapter,
            child,
            budget,
            estimator,
            model,
            depth + 1,
            seen_identities,
            leaves,
        )?;
    }
    Ok(())
}

fn validate_source_items(items: &[LogicalItem]) -> Result<(), ChunkError> {
    let mut ids = HashSet::new();
    let mut previous_order = None;
    for item in items {
        let identity = &item.identity;
        if identity.id.trim().is_empty() {
            return Err(ChunkError::InvalidItemIdentity {
                item_id: identity.id.clone(),
                reason: "identity must not be empty".to_string(),
            });
        }
        if identity.id != identity.root_id
            || identity.parent_id.is_some()
            || !identity.segment_path.is_empty()
        {
            return Err(ChunkError::InvalidItemIdentity {
                item_id: identity.id.clone(),
                reason: "top-level source must be a root identity".to_string(),
            });
        }
        if previous_order.is_some_and(|order| identity.source_order <= order) {
            return Err(ChunkError::InvalidItemIdentity {
                item_id: identity.id.clone(),
                reason: "source_order must be strictly increasing".to_string(),
            });
        }
        previous_order = Some(identity.source_order);
        if !ids.insert(identity.id.clone()) {
            return Err(ChunkError::DuplicateItemIdentity {
                item_id: identity.id.clone(),
            });
        }
    }
    Ok(())
}

fn validate_child_identity(
    parent: &LogicalItemIdentity,
    child: &LogicalItemIdentity,
    returned_index: usize,
) -> Result<(), ChunkError> {
    let expected_parent = Some(parent.id.as_str());
    if child.id.trim().is_empty()
        || child.root_id != parent.root_id
        || child.parent_id.as_deref() != expected_parent
        || child.source_order != parent.source_order
        || child.segment_path.len() != parent.segment_path.len() + 1
        || !child.segment_path.starts_with(&parent.segment_path)
        || child.segment_path.last().copied() != Some(returned_index as u32)
    {
        return Err(ChunkError::InvalidItemIdentity {
            item_id: child.id.clone(),
            reason: format!(
                "split child must preserve root/source order and record parent `{}` at segment index {}",
                parent.id, returned_index
            ),
        });
    }
    Ok(())
}

/// Recheck that every terminal item appears exactly once and every original
/// source root remains represented by at least one terminal leaf.
pub fn validate_plan_completeness(plan: &ChunkPlan) -> Result<(), ChunkError> {
    let mut expected = BTreeSet::new();
    let mut duplicate_declarations = BTreeSet::new();
    let mut expected_by_id = HashMap::new();
    for identity in &plan.leaf_identities {
        if !expected.insert(identity.id.clone()) {
            duplicate_declarations.insert(identity.id.clone());
        }
        expected_by_id.insert(identity.id.as_str(), identity);
    }
    if !duplicate_declarations.is_empty() {
        return Err(ChunkError::DuplicatePlannedItems {
            item_ids: duplicate_declarations.into_iter().collect(),
        });
    }

    let mut payload_ids = BTreeSet::new();
    let mut duplicate_payloads = BTreeSet::new();
    let mut mismatched_payloads = BTreeSet::new();
    for item in &plan.leaf_items {
        if !payload_ids.insert(item.identity.id.clone()) {
            duplicate_payloads.insert(item.identity.id.clone());
        }
        if expected_by_id
            .get(item.identity.id.as_str())
            .is_some_and(|expected_identity| *expected_identity != &item.identity)
        {
            mismatched_payloads.insert(item.identity.id.clone());
        }
    }
    if !duplicate_payloads.is_empty() {
        return Err(ChunkError::DuplicatePlannedItems {
            item_ids: duplicate_payloads.into_iter().collect(),
        });
    }
    if !mismatched_payloads.is_empty() {
        return Err(ChunkError::MismatchedPlannedItems {
            item_ids: mismatched_payloads.into_iter().collect(),
        });
    }
    let missing_payloads = expected
        .difference(&payload_ids)
        .cloned()
        .collect::<Vec<_>>();
    if !missing_payloads.is_empty() {
        return Err(ChunkError::MissingPlannedItems {
            item_ids: missing_payloads,
        });
    }
    let unexpected_payloads = payload_ids
        .difference(&expected)
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected_payloads.is_empty() {
        return Err(ChunkError::UnexpectedPlannedItems {
            item_ids: unexpected_payloads,
        });
    }
    let payload_order = plan
        .leaf_items
        .iter()
        .map(|item| item.identity.id.as_str())
        .collect::<Vec<_>>();
    let declared_order = plan
        .leaf_identities
        .iter()
        .map(|identity| identity.id.as_str())
        .collect::<Vec<_>>();
    if payload_order != declared_order {
        return Err(ChunkError::InvalidPlannedOrder);
    }

    let mut actual = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    let mut mismatched_identities = BTreeSet::new();
    let mut covered_roots = BTreeSet::new();
    let mut actual_order = Vec::new();
    for (expected_index, chunk) in plan.chunks.iter().enumerate() {
        if chunk.index != expected_index as u32 {
            return Err(ChunkError::InvalidChunkDescriptor {
                chunk_index: chunk.index,
                reason: format!("expected sequential index {expected_index}"),
            });
        }
        if chunk.estimated_payload_tokens > plan.budget.effective_payload_tokens {
            return Err(ChunkError::InvalidChunkDescriptor {
                chunk_index: chunk.index,
                reason: format!(
                    "estimated payload {} exceeds effective payload {}",
                    chunk.estimated_payload_tokens, plan.budget.effective_payload_tokens
                ),
            });
        }
        for identity in &chunk.items {
            actual_order.push(identity.id.as_str());
            if !actual.insert(identity.id.clone()) {
                duplicates.insert(identity.id.clone());
            }
            if expected_by_id
                .get(identity.id.as_str())
                .is_some_and(|expected_identity| *expected_identity != identity)
            {
                mismatched_identities.insert(identity.id.clone());
            }
            covered_roots.insert(identity.root_id.clone());
        }
    }
    if !duplicates.is_empty() {
        return Err(ChunkError::DuplicatePlannedItems {
            item_ids: duplicates.into_iter().collect(),
        });
    }
    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(ChunkError::MissingPlannedItems { item_ids: missing });
    }
    let unexpected = actual.difference(&expected).cloned().collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(ChunkError::UnexpectedPlannedItems {
            item_ids: unexpected,
        });
    }
    if !mismatched_identities.is_empty() {
        return Err(ChunkError::MismatchedPlannedItems {
            item_ids: mismatched_identities.into_iter().collect(),
        });
    }
    if actual_order != declared_order {
        return Err(ChunkError::InvalidPlannedOrder);
    }
    if let Some(chunk) = plan.chunks.iter().find(|chunk| chunk.items.is_empty()) {
        return Err(ChunkError::InvalidChunkDescriptor {
            chunk_index: chunk.index,
            reason: "chunk must own at least one terminal item".to_string(),
        });
    }
    let mut expected_roots = BTreeSet::new();
    let mut duplicate_roots = BTreeSet::new();
    for identity in &plan.source_identities {
        if !expected_roots.insert(identity.root_id.clone()) {
            duplicate_roots.insert(identity.root_id.clone());
        }
    }
    if !duplicate_roots.is_empty() {
        return Err(ChunkError::DuplicatePlannedItems {
            item_ids: duplicate_roots.into_iter().collect(),
        });
    }
    let invalid_leaf_roots = plan
        .leaf_identities
        .iter()
        .filter(|identity| !expected_roots.contains(&identity.root_id))
        .map(|identity| identity.id.clone())
        .collect::<Vec<_>>();
    if !invalid_leaf_roots.is_empty() {
        return Err(ChunkError::UnexpectedPlannedItems {
            item_ids: invalid_leaf_roots,
        });
    }
    let missing_roots = expected_roots
        .difference(&covered_roots)
        .cloned()
        .collect::<Vec<_>>();
    if !missing_roots.is_empty() {
        return Err(ChunkError::MissingSourceCoverage {
            item_ids: missing_roots,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use crate::{
        types::LLMRequest, ChunkBudget, ChunkDomainAdapter, ChunkError, ChunkValidationError,
        ConservativeOllamaEstimator, FinalValidationContract, LogicalItem, LogicalLlmRequest,
    };

    use super::{plan_logical_request, validate_plan_completeness};

    struct FakeAdapter;

    impl ChunkDomainAdapter for FakeAdapter {
        fn id(&self) -> &'static str {
            "fake_items_v1"
        }

        fn version(&self) -> &'static str {
            "1"
        }

        fn supported_operations(&self) -> &'static [&'static str] {
            &["fake_operation"]
        }

        fn final_validation_contract(&self) -> FinalValidationContract {
            FinalValidationContract::Available
        }

        fn logical_items(&self, input: &Value) -> Result<Vec<LogicalItem>, ChunkError> {
            let values = input.as_array().ok_or_else(|| ChunkError::Adapter {
                adapter: self.id().to_string(),
                reason: "input must be an array".to_string(),
            })?;
            values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let id = value.get("id").and_then(Value::as_str).ok_or_else(|| {
                        ChunkError::Adapter {
                            adapter: self.id().to_string(),
                            reason: "item id is required".to_string(),
                        }
                    })?;
                    Ok(LogicalItem::root(id, index as u32, value.clone()))
                })
                .collect()
        }

        fn estimate_item_tokens(
            &self,
            item: &LogicalItem,
            _estimator: &dyn crate::TokenEstimator,
            _model: &str,
        ) -> Result<u32, ChunkError> {
            item.value
                .get("tokens")
                .and_then(Value::as_u64)
                .and_then(|tokens| u32::try_from(tokens).ok())
                .ok_or_else(|| ChunkError::Adapter {
                    adapter: self.id().to_string(),
                    reason: format!("item `{}` needs a u32 token estimate", item.identity.id),
                })
        }

        fn split_oversized_item(
            &self,
            item: &LogicalItem,
            budget: &ChunkBudget,
        ) -> Result<Vec<LogicalItem>, ChunkError> {
            let Some(parts) = item.value.get("parts").and_then(Value::as_array) else {
                return Err(ChunkError::ChunkItemExceedsContextWindow {
                    adapter: self.id().to_string(),
                    item_id: item.identity.id.clone(),
                    estimated_tokens: item
                        .value
                        .get("tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default() as u32,
                    effective_payload_tokens: budget.effective_payload_tokens,
                });
            };
            Ok(parts
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    LogicalItem::child(
                        format!("{}/{}", item.identity.id, index),
                        item,
                        index as u32,
                        value.clone(),
                    )
                })
                .collect())
        }

        fn validate_final(&self, _value: &Value) -> Result<(), ChunkValidationError> {
            Ok(())
        }

        fn validate_map_value(
            &self,
            _value: &Value,
            _chunk: &crate::ChunkDescriptor,
        ) -> Result<(), ChunkValidationError> {
            Ok(())
        }
    }

    fn budget() -> ChunkBudget {
        ChunkBudget::new(1_000, 20_000, 1_000, 0, 0, 0).unwrap()
    }

    fn request(input: Value) -> LogicalLlmRequest {
        LogicalLlmRequest {
            operation: "fake_operation".to_string(),
            input,
            base_request: LLMRequest {
                model: "gemma4:12b".to_string(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn stable_packing_derives_one_chunk_without_reordering() {
        let plan = plan_logical_request(
            &FakeAdapter,
            &request(json!([
                {"id": "a", "tokens": 200},
                {"id": "b", "tokens": 300},
                {"id": "c", "tokens": 400}
            ])),
            budget(),
            &ConservativeOllamaEstimator,
        )
        .unwrap();

        assert_eq!(plan.chunks.len(), 1);
        assert_eq!(
            plan.chunks[0]
                .items
                .iter()
                .map(|identity| identity.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn stable_packing_derives_two_chunks_at_item_boundaries() {
        let plan = plan_logical_request(
            &FakeAdapter,
            &request(json!([
                {"id": "a", "tokens": 600},
                {"id": "b", "tokens": 600}
            ])),
            budget(),
            &ConservativeOllamaEstimator,
        )
        .unwrap();

        assert_eq!(plan.chunks.len(), 2);
        assert_eq!(plan.chunks[0].items[0].id, "a");
        assert_eq!(plan.chunks[1].items[0].id, "b");
    }

    #[test]
    fn derived_count_supports_approximately_eleven_chunks_without_a_chunk_cap() {
        let input = Value::Array(
            (0..11)
                .map(|index| json!({"id": format!("source-{index}"), "tokens": 900}))
                .collect(),
        );
        let plan = plan_logical_request(
            &FakeAdapter,
            &request(input),
            budget(),
            &ConservativeOllamaEstimator,
        )
        .unwrap();

        assert_eq!(plan.chunks.len(), 11);
        assert_eq!(plan.source_identities.len(), 11);
        assert_eq!(plan.leaf_identities.len(), 11);
        validate_plan_completeness(&plan).unwrap();
    }

    #[test]
    fn oversized_items_split_recursively_and_preserve_root_identity() {
        let plan = plan_logical_request(
            &FakeAdapter,
            &request(json!([{
                "id": "episode-1",
                "tokens": 2500,
                "parts": [
                    {"tokens": 1200, "parts": [{"tokens": 600}, {"tokens": 600}]},
                    {"tokens": 1200, "parts": [{"tokens": 600}, {"tokens": 600}]}
                ]
            }])),
            budget(),
            &ConservativeOllamaEstimator,
        )
        .unwrap();

        assert_eq!(plan.chunks.len(), 4);
        assert_eq!(plan.leaf_identities.len(), 4);
        assert!(plan
            .leaf_identities
            .iter()
            .all(|identity| identity.root_id == "episode-1"));
        assert_eq!(
            plan.leaf_identities
                .iter()
                .map(|identity| identity.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "episode-1/0/0",
                "episode-1/0/1",
                "episode-1/1/0",
                "episode-1/1/1"
            ]
        );
    }

    #[test]
    fn completeness_validation_rejects_missing_and_duplicate_leaves() {
        let plan = plan_logical_request(
            &FakeAdapter,
            &request(json!([
                {"id": "a", "tokens": 600},
                {"id": "b", "tokens": 600}
            ])),
            budget(),
            &ConservativeOllamaEstimator,
        )
        .unwrap();

        let mut missing = plan.clone();
        missing.chunks[1].items.clear();
        assert!(matches!(
            validate_plan_completeness(&missing),
            Err(ChunkError::MissingPlannedItems { .. })
        ));

        let mut duplicate = plan;
        let repeated = duplicate.chunks[0].items[0].clone();
        duplicate.chunks[1].items.push(repeated);
        assert!(matches!(
            validate_plan_completeness(&duplicate),
            Err(ChunkError::DuplicatePlannedItems { .. })
        ));
    }
}
