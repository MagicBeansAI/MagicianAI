//! Deterministic, authority-bound lowering for the closed recipe IR.
//!
//! This module compiles only the node kinds for which Magician already has an
//! exact owner: Artifact V3 control, the app entity read service, and local
//! bounded value validation/pass-through. The result is an inert plan, not an
//! executor. Durable adoption and the production caller must retain this exact
//! identity; unsupported/effectful declarations cannot cross this boundary.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{self, Write},
    sync::OnceLock,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    models::{AppContractError, AppDigest, AppName, AppReference, AppRevision},
    recipe_ir::{
        compile_recipe_ir, AppCompiledRecipeIr, AppCompiledWorkflowValueSchema,
        AppRecipeAuthorityContract, AppRecipeCancellationContract, AppRecipeEffectClass,
        AppRecipeEffectContract, AppRecipeIrError, AppRecipeNode, AppRecipeNodeKind,
        AppRecipeNodeResourceCeiling, AppRecipeOutputContract, AppRecipeRetryContract,
        AppRecipeUncertaintyContract, APP_RECIPE_IR_VERSION,
    },
};

pub(crate) const APP_RECIPE_LOWERED_PLAN_VERSION: &str = "magician.app-recipe-lowered-plan.v1";
const MAX_LOWERED_PLAN_BYTES: usize = 384 * 1024;
const APP_RECIPE_RUNTIME_IMPLEMENTATION_PARTS: &[(&str, &[u8])] = &[
    (
        "apps/concurrent_progress.rs",
        include_bytes!("concurrent_progress.rs"),
    ),
    (
        "apps/linked_text_rows.rs",
        include_bytes!("linked_text_rows.rs"),
    ),
    (
        "apps/runtime_contract.rs",
        include_bytes!("runtime_contract.rs"),
    ),
    (
        "apps/store_transaction.rs",
        include_bytes!("store_transaction.rs"),
    ),
    (
        "apps/recipe_store_transaction.rs",
        include_bytes!("recipe_store_transaction.rs"),
    ),
    ("apps/recipe_ir.rs", include_bytes!("recipe_ir.rs")),
    ("apps/entity_store.rs", include_bytes!("entity_store.rs")),
    ("apps/value_mapping.rs", include_bytes!("value_mapping.rs")),
    (
        "apps/recipe_lowering.rs",
        include_bytes!("recipe_lowering.rs"),
    ),
    (
        "apps/recipe_lifecycle.rs",
        include_bytes!("recipe_lifecycle.rs"),
    ),
    ("apps/manifest.rs", include_bytes!("manifest.rs")),
    ("apps/package_lock.rs", include_bytes!("package_lock.rs")),
    ("apps/workflows.rs", include_bytes!("workflows.rs")),
    (
        "apps/contextual_round.rs",
        include_bytes!("contextual_round.rs"),
    ),
    (
        "apps/contextual_round_program.rs",
        include_bytes!("contextual_round_program.rs"),
    ),
    (
        "apps/contextual_round_declaration.rs",
        include_bytes!("contextual_round_declaration.rs"),
    ),
    (
        "apps/workflow_commits.rs",
        include_bytes!("workflow_commits.rs"),
    ),
    (
        "apps/workflow_rounds.rs",
        include_bytes!("workflow_rounds.rs"),
    ),
    (
        "apps/workflow_model_context.rs",
        include_bytes!("workflow_model_context.rs"),
    ),
    (
        "apps/processing_boundary.rs",
        include_bytes!("processing_boundary.rs"),
    ),
    (
        "artifact_v2/app_recipe_model.rs",
        include_bytes!("../artifact_v2/app_recipe_model.rs"),
    ),
    (
        "apps/scheduled_input.rs",
        include_bytes!("scheduled_input.rs"),
    ),
    (
        "apps/query_contract.rs",
        include_bytes!("query_contract.rs"),
    ),
    ("apps/llm_dispatch.rs", include_bytes!("llm_dispatch.rs")),
    (
        "apps/model_output_schema.rs",
        include_bytes!("model_output_schema.rs"),
    ),
    (
        "apps/reconciliation.rs",
        include_bytes!("reconciliation.rs"),
    ),
    (
        "apps/reconciliation-result-schema.json",
        include_bytes!("reconciliation-result-schema.json"),
    ),
    (
        "apps/reconciliation-input-schema.json",
        include_bytes!("reconciliation-input-schema.json"),
    ),
    (
        "apps/recipe_reconciliation.rs",
        include_bytes!("recipe_reconciliation.rs"),
    ),
    (
        "apps/recipe_contextual_round.rs",
        include_bytes!("recipe_contextual_round.rs"),
    ),
    (
        "artifact_v2/models.rs",
        include_bytes!("../artifact_v2/models.rs"),
    ),
    (
        "artifact_v2/events.rs",
        include_bytes!("../artifact_v2/events.rs"),
    ),
    (
        "artifact_v2/reducer.rs",
        include_bytes!("../artifact_v2/reducer.rs"),
    ),
    (
        "artifact_v2/service.rs",
        include_bytes!("../artifact_v2/service.rs"),
    ),
];

/// Central activation boundary for the exact Recipe v1 vertical. Installation,
/// launch and retained-lock revalidation all require this predicate in addition
/// to the immutable member, schema, topology, plan and implementation bindings.
/// This does not admit any node outside `supported_recipe_node_set`.
pub(crate) const fn app_recipe_runner_ready() -> bool {
    true
}

/// Exact semantic compatibility identity for the admitted recipe vertical.
/// Current build source evidence is deliberately separate from package locks.
pub(crate) fn app_recipe_runtime_implementation_digest() -> Result<AppDigest, AppRecipeLoweringError>
{
    // Wire field retained for existing locks; its value names compatible
    // semantics, while the current build's exact bytes remain auditable.
    AppDigest::parse(super::runtime_contract::RECIPE_CONTRACT_V1)
        .map_err(AppRecipeLoweringError::Contract)
}

pub(crate) fn app_recipe_runtime_source_digest() -> Result<AppDigest, AppRecipeLoweringError> {
    static IMPLEMENTATION_DIGEST: OnceLock<String> = OnceLock::new();
    let encoded = IMPLEMENTATION_DIGEST.get_or_init(|| {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"magician.app-recipe-runtime-implementation.v1\0");
        for (name, bytes) in APP_RECIPE_RUNTIME_IMPLEMENTATION_PARTS {
            hasher.update(&(name.len() as u64).to_le_bytes());
            hasher.update(name.as_bytes());
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        format!("blake3:{}", hasher.finalize().to_hex())
    });
    AppDigest::parse(encoded.clone()).map_err(AppRecipeLoweringError::Contract)
}

/// Immutable workflow authority selected before lowering. Every field is
/// copied into the plan digest. Recovery must compare all of them with the
/// current installed source and must never silently recompile on drift.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppRecipeLoweringFence {
    package_revision_ref: AppReference,
    package_lock_digest: AppDigest,
    grant_revision: AppRevision,
    grant_digest: AppDigest,
    workflow_authority_digest: AppDigest,
    fence_digest: AppDigest,
}

impl fmt::Debug for AppRecipeLoweringFence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppRecipeLoweringFence")
            .field("package_revision_ref", &self.package_revision_ref)
            .field("package_lock_digest", &self.package_lock_digest)
            .field("grant_revision", &self.grant_revision)
            .field("grant_digest", &self.grant_digest)
            .field("workflow_authority_digest", &self.workflow_authority_digest)
            .field("fence_digest", &self.fence_digest)
            .finish()
    }
}

#[derive(Serialize)]
struct AppRecipeLoweringFenceIdentity<'a> {
    schema: &'static str,
    package_revision_ref: &'a AppReference,
    package_lock_digest: &'a AppDigest,
    grant_revision: AppRevision,
    grant_digest: &'a AppDigest,
    workflow_authority_digest: &'a AppDigest,
}

impl AppRecipeLoweringFence {
    /// Minted only by the live workflow authority owner after package-lock and
    /// grant revalidation. A persisted fence is data, never reusable authority.
    pub(crate) fn seal(
        package_revision_ref: AppReference,
        package_lock_digest: AppDigest,
        grant_revision: AppRevision,
        grant_digest: AppDigest,
        workflow_authority_digest: AppDigest,
    ) -> Result<Self, AppRecipeLoweringError> {
        if !is_package_revision_ref(&package_revision_ref) {
            return Err(AppRecipeLoweringError::InvalidAuthorityFence);
        }
        let fence_digest = stream_identity(
            "recipe lowering authority fence",
            &AppRecipeLoweringFenceIdentity {
                schema: "magician.app-recipe-lowering-fence.v1",
                package_revision_ref: &package_revision_ref,
                package_lock_digest: &package_lock_digest,
                grant_revision,
                grant_digest: &grant_digest,
                workflow_authority_digest: &workflow_authority_digest,
            },
            16 * 1024,
        )?
        .0;
        Ok(Self {
            package_revision_ref,
            package_lock_digest,
            grant_revision,
            grant_digest,
            workflow_authority_digest,
            fence_digest,
        })
    }

    pub(crate) fn matches_exactly(&self, current: &Self) -> bool {
        self == current
    }

    fn validate(&self) -> Result<(), AppRecipeLoweringError> {
        let expected = Self::seal(
            self.package_revision_ref.clone(),
            self.package_lock_digest.clone(),
            self.grant_revision,
            self.grant_digest.clone(),
            self.workflow_authority_digest.clone(),
        )?;
        if &expected != self {
            return Err(AppRecipeLoweringError::InvalidAuthorityFence);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppRecipeExistingOwner {
    ArtifactV3Sequence,
    ArtifactV3Parallel,
    ArtifactV3Switch,
    AppEntityQuery,
    AppEntityGet,
    AppEntityReconciliation,
    AppContextualRound,
    AppStoreTransaction,
    WorkflowValueMapper,
    WorkflowValueValidator,
    WorkflowValuePassThrough,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AppRecipeValueMapping {
    Identity {
        schema_ref: AppReference,
    },
    SequenceEdge {
        source_schema_ref: AppReference,
        target_schema_ref: AppReference,
    },
    ParallelBranch {
        branch: AppName,
        input_schema_ref: AppReference,
        output_schema_ref: AppReference,
    },
    TaggedVariant {
        discriminator: AppName,
        tag: AppName,
        tagged_schema_ref: AppReference,
        payload_schema_ref: AppReference,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppRecipeChildBinding {
    ordinal: u16,
    semantic_key: Option<AppName>,
    child: AppName,
    input_mapping: AppRecipeValueMapping,
}

impl AppRecipeChildBinding {
    pub(crate) fn semantic_key(&self) -> Option<&AppName> {
        self.semantic_key.as_ref()
    }

    pub(crate) fn child(&self) -> &AppName {
        &self.child
    }

    #[cfg(test)]
    pub(crate) fn input_mapping(&self) -> &AppRecipeValueMapping {
        &self.input_mapping
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppRecipeReceiptOwner {
    None,
    AppWorkflowTerminalCommit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppRecipeLoweredSchemaBinding {
    schema_ref: AppReference,
    schema_digest: AppDigest,
    canonical_encoded_len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppRecipeLoweredNodeBinding {
    node_id: AppName,
    owner: AppRecipeExistingOwner,
    operation: AppRecipeNodeKind,
    input_schema_ref: AppReference,
    output: AppRecipeOutputContract,
    effect: AppRecipeEffectContract,
    authority: AppRecipeAuthorityContract,
    resources: AppRecipeNodeResourceCeiling,
    retry: AppRecipeRetryContract,
    cancellation: AppRecipeCancellationContract,
    uncertainty: AppRecipeUncertaintyContract,
    receipt_owner: AppRecipeReceiptOwner,
    children: Vec<AppRecipeChildBinding>,
    binding_digest: AppDigest,
}

impl AppRecipeLoweredNodeBinding {
    pub(crate) fn owner(&self) -> &AppRecipeExistingOwner {
        &self.owner
    }

    pub(crate) fn operation(&self) -> &AppRecipeNodeKind {
        &self.operation
    }

    pub(crate) fn input_schema_ref(&self) -> &AppReference {
        &self.input_schema_ref
    }

    pub(crate) fn output(&self) -> &AppRecipeOutputContract {
        &self.output
    }

    pub(crate) fn authority(&self) -> &AppRecipeAuthorityContract {
        &self.authority
    }

    pub(crate) fn resources(&self) -> &AppRecipeNodeResourceCeiling {
        &self.resources
    }

    pub(crate) fn permits_workflow_mutation(&self) -> bool {
        self.effect.class == AppRecipeEffectClass::InternalMutation
            && self.receipt_owner == AppRecipeReceiptOwner::AppWorkflowTerminalCommit
    }

    pub(crate) fn cancellation(&self) -> &AppRecipeCancellationContract {
        &self.cancellation
    }

    pub(crate) fn uncertainty(&self) -> AppRecipeUncertaintyContract {
        self.uncertainty
    }

    pub(crate) fn children(&self) -> &[AppRecipeChildBinding] {
        &self.children
    }

    pub(crate) fn binding_digest(&self) -> &AppDigest {
        &self.binding_digest
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppLoweredRecipePlanSource {
    schema: String,
    recipe_contract_version: String,
    implementation_digest: AppDigest,
    recipe_ref: AppReference,
    topology_revision: AppRevision,
    topology_digest: AppDigest,
    compiled_plan_digest: AppDigest,
    authority_fence: AppRecipeLoweringFence,
    input_schema_ref: AppReference,
    output: AppRecipeOutputContract,
    aggregate_max_active_millis: u64,
    root: AppName,
    execution_order: Vec<AppName>,
    schemas: BTreeMap<AppReference, AppRecipeLoweredSchemaBinding>,
    nodes: BTreeMap<AppName, AppRecipeLoweredNodeBinding>,
}

impl fmt::Debug for AppLoweredRecipePlanSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppLoweredRecipePlanSource")
            .field("recipe_ref", &self.recipe_ref)
            .field("topology_revision", &self.topology_revision)
            .field("topology_digest", &self.topology_digest)
            .field("compiled_plan_digest", &self.compiled_plan_digest)
            .field("implementation_digest", &self.implementation_digest)
            .field("authority_fence_digest", &self.authority_fence.fence_digest)
            .field("root", &self.root)
            .field("schema_count", &self.schemas.len())
            .field("node_count", &self.nodes.len())
            .finish()
    }
}

/// Non-deserializable proof that a recipe has a complete, closed mapping to
/// existing owners. It intentionally exposes no dispatch or readiness method.
#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppLoweredRecipePlan {
    plan_ref: AppReference,
    plan_digest: AppDigest,
    canonical_encoded_len: u64,
    source: AppLoweredRecipePlanSource,
}

impl fmt::Debug for AppLoweredRecipePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppLoweredRecipePlan")
            .field("plan_ref", &self.plan_ref)
            .field("plan_digest", &self.plan_digest)
            .field("canonical_encoded_len", &self.canonical_encoded_len)
            .field("recipe_ref", &self.source.recipe_ref)
            .field("topology_revision", &self.source.topology_revision)
            .field("node_count", &self.source.nodes.len())
            .finish()
    }
}

impl AppLoweredRecipePlan {
    pub(crate) fn plan_ref(&self) -> &AppReference {
        &self.plan_ref
    }

    pub(crate) fn plan_digest(&self) -> &AppDigest {
        &self.plan_digest
    }

    pub(crate) fn canonical_encoded_len(&self) -> u64 {
        self.canonical_encoded_len
    }

    pub(crate) fn recipe_ref(&self) -> &AppReference {
        &self.source.recipe_ref
    }

    pub(crate) fn topology_digest(&self) -> &AppDigest {
        &self.source.topology_digest
    }

    pub(crate) fn compiled_plan_digest(&self) -> &AppDigest {
        &self.source.compiled_plan_digest
    }

    pub(crate) fn implementation_digest(&self) -> &AppDigest {
        &self.source.implementation_digest
    }

    pub(crate) fn input_schema_ref(&self) -> &AppReference {
        &self.source.input_schema_ref
    }

    pub(crate) fn output_contract(&self) -> &AppRecipeOutputContract {
        &self.source.output
    }

    pub(crate) fn aggregate_max_active_millis(&self) -> u64 {
        self.source.aggregate_max_active_millis
    }

    pub(crate) fn root(&self) -> &AppName {
        &self.source.root
    }

    pub(crate) fn execution_order(&self) -> &[AppName] {
        &self.source.execution_order
    }

    pub(crate) fn node(&self, node_id: &AppName) -> Option<&AppRecipeLoweredNodeBinding> {
        self.source.nodes.get(node_id)
    }

    pub(crate) fn parent_node(&self, node_id: &AppName) -> Option<&AppName> {
        self.source.nodes.iter().find_map(|(candidate, binding)| {
            binding
                .children
                .iter()
                .any(|child| child.child == *node_id)
                .then_some(candidate)
        })
    }

    /// Deterministic same-task V3 execution identity for one exact lowered
    /// node. It binds the canonical root, plan and node binding without
    /// exposing a source name or accepting a caller-selected execution id.
    pub(crate) fn node_execution_id(
        &self,
        root_execution_id: &str,
        node_id: &AppName,
    ) -> Result<String, AppRecipeLoweringError> {
        let binding = self
            .node(node_id)
            .ok_or(AppRecipeLoweringError::CorruptRecipeTopology)?;
        let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "schema": "magician.app-recipe-node-execution.v1",
            "root_execution_id": root_execution_id,
            "plan_digest": &self.plan_digest,
            "node_id": node_id,
            "binding_digest": binding.binding_digest(),
        }))
        .map_err(|error| AppRecipeLoweringError::Encoding(error.to_string()))?;
        Ok(format!(
            "app_recipe_node_{}",
            digest.as_str().trim_start_matches("blake3:")
        ))
    }

    pub(crate) fn node_plan_id(&self, node_id: &AppName) -> Result<String, AppRecipeLoweringError> {
        let binding = self
            .node(node_id)
            .ok_or(AppRecipeLoweringError::CorruptRecipeTopology)?;
        Ok(format!(
            "app_recipe_binding_{}",
            binding
                .binding_digest()
                .as_str()
                .trim_start_matches("blake3:")
        ))
    }

    pub(crate) fn source(&self) -> &AppLoweredRecipePlanSource {
        &self.source
    }

    pub(crate) fn validate_recovery(
        &self,
        current_recipe: &AppCompiledRecipeIr,
        current_fence: &AppRecipeLoweringFence,
    ) -> Result<(), AppRecipeLoweringError> {
        current_fence.validate()?;
        if self.source.recipe_contract_version != APP_RECIPE_IR_VERSION
            || self.source.implementation_digest != app_recipe_runtime_implementation_digest()?
            || self.source.recipe_ref != *current_recipe.recipe_ref()
            || self.source.topology_digest != *current_recipe.topology_digest()
            || self.source.topology_revision != current_recipe.source().evolution.topology_revision
            || !self.source.authority_fence.matches_exactly(current_fence)
            || self.source.input_schema_ref != current_recipe.source().input_schema_ref
            || self.source.output != current_recipe.source().output
            || self.source.aggregate_max_active_millis
                != current_recipe.source().ceilings.max_active_millis
        {
            return Err(AppRecipeLoweringError::RecoveryDrift);
        }
        let (digest, encoded_len) =
            stream_identity("lowered recipe plan", &self.source, MAX_LOWERED_PLAN_BYTES)?;
        if digest != self.plan_digest || encoded_len as u64 != self.canonical_encoded_len {
            return Err(AppRecipeLoweringError::CorruptPlan);
        }
        Ok(())
    }

    pub(crate) fn restore(
        plan_ref: AppReference,
        plan_digest: AppDigest,
        canonical_encoded_len: u64,
        source: AppLoweredRecipePlanSource,
    ) -> Result<Self, AppRecipeLoweringError> {
        let (observed_digest, observed_len) =
            stream_identity("lowered recipe plan", &source, MAX_LOWERED_PLAN_BYTES)?;
        let observed_ref =
            AppReference::parse(format!("recipe-plan:{}", observed_digest.as_str()))?;
        if source.schema != APP_RECIPE_LOWERED_PLAN_VERSION
            || source.recipe_contract_version != APP_RECIPE_IR_VERSION
            || source.implementation_digest != app_recipe_runtime_implementation_digest()?
            || source.authority_fence.validate().is_err()
            || observed_digest != plan_digest
            || observed_ref != plan_ref
            || observed_len as u64 != canonical_encoded_len
            || source.execution_order.len() != source.nodes.len()
            || source.execution_order.iter().collect::<BTreeSet<_>>().len() != source.nodes.len()
            || !source.nodes.contains_key(&source.root)
        {
            return Err(AppRecipeLoweringError::CorruptPlan);
        }
        Ok(Self {
            plan_ref,
            plan_digest,
            canonical_encoded_len,
            source,
        })
    }
}

/// Compile the exact normalized recipe into a deterministic owner plan. Input
/// schema declaration order cannot affect identity; semantic child order does.
pub(crate) fn lower_recipe_ir(
    recipe: &AppCompiledRecipeIr,
    schemas: &[&AppCompiledWorkflowValueSchema],
    authority_fence: AppRecipeLoweringFence,
) -> Result<AppLoweredRecipePlan, AppRecipeLoweringError> {
    authority_fence.validate()?;

    let recompiled = compile_recipe_ir(recipe.source().clone(), schemas)?;
    if recompiled.recipe_ref() != recipe.recipe_ref()
        || recompiled.topology_digest() != recipe.topology_digest()
        || recompiled.canonical_encoded_len() != recipe.canonical_encoded_len()
    {
        return Err(AppRecipeLoweringError::RecipeSubstitution);
    }

    let required_schema_refs = required_schema_refs(recipe);
    let mut schema_catalog = BTreeMap::new();
    for schema in schemas {
        if !required_schema_refs.contains(schema.schema_ref()) {
            continue;
        }
        let binding = AppRecipeLoweredSchemaBinding {
            schema_ref: schema.schema_ref().clone(),
            schema_digest: schema.content_digest().clone(),
            canonical_encoded_len: schema.canonical_encoded_len(),
        };
        if schema_catalog
            .insert(schema.schema_ref().clone(), binding)
            .is_some()
        {
            return Err(AppRecipeLoweringError::DuplicateSchema(
                schema.schema_ref().to_string(),
            ));
        }
    }
    if schema_catalog.len() != required_schema_refs.len() {
        return Err(AppRecipeLoweringError::MissingSchema);
    }

    let execution_order = semantic_preorder(recipe)?;
    validate_parallel_execution_width(recipe)?;
    let mut nodes = BTreeMap::new();
    for node_id in &execution_order {
        let node = recipe
            .source()
            .nodes
            .get(node_id)
            .ok_or(AppRecipeLoweringError::CorruptRecipeTopology)?;
        let (owner, receipt_owner) = existing_owner(node_id, node)?;
        let children = lower_children(recipe, node)?;
        let binding_digest = stream_identity(
            "recipe node binding",
            &serde_json::json!({
                "schema": "magician.app-recipe-node-binding.v1",
                "recipe_ref": recipe.recipe_ref(),
                "topology_revision": recipe.source().evolution.topology_revision,
                "node_id": node_id,
                "owner": &owner,
                "operation": &node.node,
                "input_schema_ref": &node.input_schema_ref,
                "output": &node.output,
                "effect": &node.effect,
                "authority": &node.authority,
                "resources": &node.resources,
                "retry": &node.retry,
                "cancellation": &node.cancellation,
                "uncertainty": node.effect.uncertainty,
                "receipt_owner": &receipt_owner,
                "children": &children,
            }),
            64 * 1024,
        )?
        .0;
        let binding = AppRecipeLoweredNodeBinding {
            node_id: node_id.clone(),
            owner,
            operation: node.node.clone(),
            input_schema_ref: node.input_schema_ref.clone(),
            output: node.output.clone(),
            effect: node.effect.clone(),
            authority: node.authority.clone(),
            resources: node.resources.clone(),
            retry: node.retry.clone(),
            cancellation: node.cancellation.clone(),
            uncertainty: node.effect.uncertainty,
            receipt_owner,
            children,
            binding_digest,
        };
        if nodes.insert(node_id.clone(), binding).is_some() {
            return Err(AppRecipeLoweringError::CorruptRecipeTopology);
        }
    }

    let compiled_plan_digest = stream_identity(
        "compiled recipe plan",
        &serde_json::json!({
            "schema": "magician.app-recipe-compiled-plan.v1",
            "recipe_contract_version": APP_RECIPE_IR_VERSION,
            "recipe_ref": recipe.recipe_ref(),
            "topology_revision": recipe.source().evolution.topology_revision,
            "topology_digest": recipe.topology_digest(),
            "input_schema_ref": &recipe.source().input_schema_ref,
            "output": &recipe.source().output,
            "aggregate_max_active_millis": recipe.source().ceilings.max_active_millis,
            "root": &recipe.source().root,
            "execution_order": &execution_order,
            "schemas": &schema_catalog,
            "nodes": &nodes,
        }),
        MAX_LOWERED_PLAN_BYTES,
    )?
    .0;
    let source = AppLoweredRecipePlanSource {
        schema: APP_RECIPE_LOWERED_PLAN_VERSION.to_owned(),
        recipe_contract_version: APP_RECIPE_IR_VERSION.to_owned(),
        implementation_digest: app_recipe_runtime_implementation_digest()?,
        recipe_ref: recipe.recipe_ref().clone(),
        topology_revision: recipe.source().evolution.topology_revision,
        topology_digest: recipe.topology_digest().clone(),
        compiled_plan_digest,
        authority_fence,
        input_schema_ref: recipe.source().input_schema_ref.clone(),
        output: recipe.source().output.clone(),
        aggregate_max_active_millis: recipe.source().ceilings.max_active_millis,
        root: recipe.source().root.clone(),
        execution_order,
        schemas: schema_catalog,
        nodes,
    };
    let (plan_digest, canonical_encoded_len) =
        stream_identity("lowered recipe plan", &source, MAX_LOWERED_PLAN_BYTES)?;
    let plan_ref = AppReference::parse(format!("recipe-plan:{}", plan_digest.as_str()))?;
    Ok(AppLoweredRecipePlan {
        plan_ref,
        plan_digest,
        canonical_encoded_len: canonical_encoded_len as u64,
        source,
    })
}

/// Immutable install-time identity of the exact supported lowering. It omits
/// the live grant fence; the later run plan includes this digest and then adds
/// the current authority fence to its own identity.
pub(crate) fn compile_recipe_plan_digest(
    recipe: &AppCompiledRecipeIr,
    schemas: &[&AppCompiledWorkflowValueSchema],
) -> Result<AppDigest, AppRecipeLoweringError> {
    let placeholder = AppDigest::blake3(b"recipe-install-time-authority-placeholder");
    let fence = AppRecipeLoweringFence::seal(
        AppReference::parse("package:recipe-install-review")?,
        placeholder.clone(),
        AppRevision::new(1)?,
        placeholder.clone(),
        placeholder,
    )?;
    Ok(lower_recipe_ir(recipe, schemas, fence)?
        .compiled_plan_digest()
        .clone())
}

pub(crate) fn supported_recipe_node_set() -> Result<BTreeSet<AppName>, AppRecipeLoweringError> {
    [
        "validate",
        "query",
        "get",
        "map",
        "emit_value",
        "sequence",
        "parallel",
        "switch",
        "reconcile",
        "contextual_round",
        "store_transaction",
    ]
    .into_iter()
    .map(|name| AppName::parse(name).map_err(AppRecipeLoweringError::from))
    .collect()
}

/// Preserve the original complete v1 node-set binding for pre-round recipes.
/// An additive host feature must not rewrite their immutable lock identity.
pub(crate) fn recipe_contract_node_set(
    recipe: &AppCompiledRecipeIr,
) -> Result<BTreeSet<AppName>, AppRecipeLoweringError> {
    let mut supported = supported_recipe_node_set()?;
    if !recipe
        .source()
        .nodes
        .values()
        .any(|node| matches!(node.node, AppRecipeNodeKind::StoreTransaction { .. }))
    {
        supported.remove(&AppName::parse("store_transaction")?);
    }
    if !recipe
        .source()
        .nodes
        .values()
        .any(|node| matches!(node.node, AppRecipeNodeKind::ContextualRound { .. }))
    {
        supported.remove(&AppName::parse("contextual_round")?);
    }
    Ok(supported)
}

fn required_schema_refs(recipe: &AppCompiledRecipeIr) -> BTreeSet<AppReference> {
    let mut refs = BTreeSet::new();
    refs.insert(recipe.source().input_schema_ref.clone());
    refs.insert(recipe.source().output.schema_ref.clone());
    for node in recipe.source().nodes.values() {
        refs.insert(node.input_schema_ref.clone());
        refs.insert(node.output.schema_ref.clone());
    }
    refs
}

fn semantic_preorder(recipe: &AppCompiledRecipeIr) -> Result<Vec<AppName>, AppRecipeLoweringError> {
    let mut order = Vec::with_capacity(recipe.source().nodes.len());
    let mut seen = BTreeSet::new();
    let mut stack = vec![recipe.source().root.clone()];
    while let Some(node_id) = stack.pop() {
        if !seen.insert(node_id.clone()) {
            return Err(AppRecipeLoweringError::CorruptRecipeTopology);
        }
        let node = recipe
            .source()
            .nodes
            .get(&node_id)
            .ok_or(AppRecipeLoweringError::CorruptRecipeTopology)?;
        order.push(node_id);
        let mut children = semantic_children(&node.node);
        children.reverse();
        stack.extend(children);
    }
    if seen.len() != recipe.source().nodes.len() {
        return Err(AppRecipeLoweringError::CorruptRecipeTopology);
    }
    Ok(order)
}

fn semantic_children(node: &AppRecipeNodeKind) -> Vec<AppName> {
    match node {
        AppRecipeNodeKind::Sequence { steps } => steps.clone(),
        AppRecipeNodeKind::Parallel { branches } => branches.values().cloned().collect(),
        AppRecipeNodeKind::Switch { cases, .. } => cases.values().cloned().collect(),
        AppRecipeNodeKind::Retry { child } | AppRecipeNodeKind::MarkUncertain { child } => {
            vec![child.clone()]
        },
        _ => Vec::new(),
    }
}

/// The current owner runs every branch concurrently. Refuse a Parallel below
/// another Parallel so the reviewed graph-wide parallelism ceiling remains a
/// real runtime bound instead of multiplying at nested fan-out boundaries.
fn validate_parallel_execution_width(
    recipe: &AppCompiledRecipeIr,
) -> Result<(), AppRecipeLoweringError> {
    for (parallel_id, node) in &recipe.source().nodes {
        let AppRecipeNodeKind::Parallel { branches } = &node.node else {
            continue;
        };
        let mut stack = branches.values().cloned().collect::<Vec<_>>();
        while let Some(node_id) = stack.pop() {
            let descendant = recipe
                .source()
                .nodes
                .get(&node_id)
                .ok_or(AppRecipeLoweringError::CorruptRecipeTopology)?;
            if matches!(&descendant.node, AppRecipeNodeKind::Parallel { .. }) {
                return Err(AppRecipeLoweringError::NestedParallel {
                    parent: parallel_id.to_string(),
                    node: node_id.to_string(),
                });
            }
            stack.extend(semantic_children(&descendant.node));
        }
    }
    Ok(())
}

fn existing_owner(
    node_id: &AppName,
    node: &AppRecipeNode,
) -> Result<(AppRecipeExistingOwner, AppRecipeReceiptOwner), AppRecipeLoweringError> {
    let owner = match &node.node {
        AppRecipeNodeKind::StoreTransaction { .. } => (
            AppRecipeExistingOwner::AppStoreTransaction,
            AppRecipeReceiptOwner::AppWorkflowTerminalCommit,
        ),
        AppRecipeNodeKind::ContextualRound { .. } => (
            AppRecipeExistingOwner::AppContextualRound,
            AppRecipeReceiptOwner::AppWorkflowTerminalCommit,
        ),
        AppRecipeNodeKind::Reconcile { .. } => (
            AppRecipeExistingOwner::AppEntityReconciliation,
            AppRecipeReceiptOwner::AppWorkflowTerminalCommit,
        ),
        AppRecipeNodeKind::Query { .. } => (
            AppRecipeExistingOwner::AppEntityQuery,
            AppRecipeReceiptOwner::None,
        ),
        AppRecipeNodeKind::Get { .. } => (
            AppRecipeExistingOwner::AppEntityGet,
            AppRecipeReceiptOwner::None,
        ),
        AppRecipeNodeKind::Map { operations, .. } if !operations.is_empty() => (
            AppRecipeExistingOwner::WorkflowValueMapper,
            AppRecipeReceiptOwner::None,
        ),
        AppRecipeNodeKind::Validate => (
            AppRecipeExistingOwner::WorkflowValueValidator,
            AppRecipeReceiptOwner::None,
        ),
        AppRecipeNodeKind::EmitValue => (
            AppRecipeExistingOwner::WorkflowValuePassThrough,
            AppRecipeReceiptOwner::None,
        ),
        AppRecipeNodeKind::Sequence { .. } => (
            AppRecipeExistingOwner::ArtifactV3Sequence,
            AppRecipeReceiptOwner::None,
        ),
        AppRecipeNodeKind::Parallel { .. } => (
            AppRecipeExistingOwner::ArtifactV3Parallel,
            AppRecipeReceiptOwner::None,
        ),
        AppRecipeNodeKind::Switch { .. } => (
            AppRecipeExistingOwner::ArtifactV3Switch,
            AppRecipeReceiptOwner::None,
        ),
        // Retry requires a canonical attempt owner which can prove whether a
        // prior read settled or is safe to repeat. The recipe evidence
        // sidecar is deliberately not that owner, so v1 keeps Retry denied.
        AppRecipeNodeKind::Retry { .. } => {
            return Err(AppRecipeLoweringError::UnsupportedNode {
                node: node_id.to_string(),
                kind: "retry",
            });
        },
        unsupported => {
            return Err(AppRecipeLoweringError::UnsupportedNode {
                node: node_id.to_string(),
                kind: node_kind_name(unsupported),
            });
        },
    };
    if !matches!(
        &node.node,
        AppRecipeNodeKind::Reconcile { .. }
            | AppRecipeNodeKind::ContextualRound { .. }
            | AppRecipeNodeKind::StoreTransaction { .. }
    ) && (node.effect.class > AppRecipeEffectClass::ReadOnly
        || node.effect.uncertainty != AppRecipeUncertaintyContract::Impossible)
    {
        return Err(AppRecipeLoweringError::UnsupportedEffectfulNode(
            node_id.to_string(),
        ));
    }
    Ok(owner)
}

fn lower_children(
    recipe: &AppCompiledRecipeIr,
    node: &AppRecipeNode,
) -> Result<Vec<AppRecipeChildBinding>, AppRecipeLoweringError> {
    match &node.node {
        AppRecipeNodeKind::Sequence { steps } => {
            let mut bindings = Vec::with_capacity(steps.len());
            let mut source_schema_ref = node.input_schema_ref.clone();
            for (ordinal, child_id) in steps.iter().enumerate() {
                let child = recipe
                    .source()
                    .nodes
                    .get(child_id)
                    .ok_or(AppRecipeLoweringError::CorruptRecipeTopology)?;
                bindings.push(AppRecipeChildBinding {
                    ordinal: u16::try_from(ordinal)
                        .map_err(|_| AppRecipeLoweringError::CorruptRecipeTopology)?,
                    semantic_key: None,
                    child: child_id.clone(),
                    input_mapping: AppRecipeValueMapping::SequenceEdge {
                        source_schema_ref,
                        target_schema_ref: child.input_schema_ref.clone(),
                    },
                });
                source_schema_ref = child.output.schema_ref.clone();
            }
            Ok(bindings)
        },
        AppRecipeNodeKind::Parallel { branches } => branches
            .iter()
            .enumerate()
            .map(|(ordinal, (branch, child_id))| {
                let child = recipe
                    .source()
                    .nodes
                    .get(child_id)
                    .ok_or(AppRecipeLoweringError::CorruptRecipeTopology)?;
                Ok(AppRecipeChildBinding {
                    ordinal: u16::try_from(ordinal)
                        .map_err(|_| AppRecipeLoweringError::CorruptRecipeTopology)?,
                    semantic_key: Some(branch.clone()),
                    child: child_id.clone(),
                    input_mapping: AppRecipeValueMapping::ParallelBranch {
                        branch: branch.clone(),
                        input_schema_ref: child.input_schema_ref.clone(),
                        output_schema_ref: child.output.schema_ref.clone(),
                    },
                })
            })
            .collect(),
        AppRecipeNodeKind::Switch {
            discriminator,
            cases,
        } => cases
            .iter()
            .enumerate()
            .map(|(ordinal, (tag, child_id))| {
                let child = recipe
                    .source()
                    .nodes
                    .get(child_id)
                    .ok_or(AppRecipeLoweringError::CorruptRecipeTopology)?;
                Ok(AppRecipeChildBinding {
                    ordinal: u16::try_from(ordinal)
                        .map_err(|_| AppRecipeLoweringError::CorruptRecipeTopology)?,
                    semantic_key: Some(tag.clone()),
                    child: child_id.clone(),
                    input_mapping: AppRecipeValueMapping::TaggedVariant {
                        discriminator: discriminator.clone(),
                        tag: tag.clone(),
                        tagged_schema_ref: node.input_schema_ref.clone(),
                        payload_schema_ref: child.input_schema_ref.clone(),
                    },
                })
            })
            .collect(),
        AppRecipeNodeKind::Retry { child } => Ok(vec![AppRecipeChildBinding {
            ordinal: 0,
            semantic_key: None,
            child: child.clone(),
            input_mapping: AppRecipeValueMapping::Identity {
                schema_ref: node.input_schema_ref.clone(),
            },
        }]),
        _ => Ok(Vec::new()),
    }
}

fn node_kind_name(node: &AppRecipeNodeKind) -> &'static str {
    match node {
        AppRecipeNodeKind::Query { .. } => "query",
        AppRecipeNodeKind::Get { .. } => "get",
        AppRecipeNodeKind::Map { .. } => "map",
        AppRecipeNodeKind::Validate => "validate",
        AppRecipeNodeKind::Reconcile { .. } => "reconcile",
        AppRecipeNodeKind::ContextualRound { .. } => "contextual_round",
        AppRecipeNodeKind::StoreTransaction { .. } => "store_transaction",
        AppRecipeNodeKind::CallTool => "call_tool",
        AppRecipeNodeKind::InvokeAction => "invoke_action",
        AppRecipeNodeKind::Mutate { .. } => "mutate",
        AppRecipeNodeKind::RunProcedure => "run_procedure",
        AppRecipeNodeKind::AgentAsTool => "agent_as_tool",
        AppRecipeNodeKind::Sequence { .. } => "sequence",
        AppRecipeNodeKind::Parallel { .. } => "parallel",
        AppRecipeNodeKind::Switch { .. } => "switch",
        AppRecipeNodeKind::EmitValue => "emit_value",
        AppRecipeNodeKind::EmitArtifact => "emit_artifact",
        AppRecipeNodeKind::EmitReceipt => "emit_receipt",
        AppRecipeNodeKind::Retry { .. } => "retry",
        AppRecipeNodeKind::MarkUncertain { .. } => "mark_uncertain",
        AppRecipeNodeKind::Deferred { .. } => "deferred",
    }
}

fn is_package_revision_ref(reference: &AppReference) -> bool {
    let raw = reference.as_str();
    (raw.starts_with("package:") || raw.starts_with("package-revision:"))
        && !raw.contains('/')
        && !raw.contains('\\')
        && !raw.contains("..")
        && !raw.contains("//")
}

struct BoundedDigestWriter {
    hasher: blake3::Hasher,
    bytes: usize,
    limit: usize,
    exceeded: bool,
}

impl BoundedDigestWriter {
    fn new(limit: usize) -> Self {
        Self {
            hasher: blake3::Hasher::new(),
            bytes: 0,
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedDigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "canonical byte ceiling",
            ));
        };
        if next > self.limit {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "canonical byte ceiling",
            ));
        }
        self.hasher.update(bytes);
        self.bytes = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn stream_identity<T: Serialize>(
    domain: &'static str,
    value: &T,
    limit: usize,
) -> Result<(AppDigest, usize), AppRecipeLoweringError> {
    let mut sink = BoundedDigestWriter::new(limit);
    if let Err(error) = serde_json::to_writer(&mut sink, value) {
        if sink.exceeded {
            return Err(AppRecipeLoweringError::CanonicalByteLimit { domain, limit });
        }
        return Err(AppRecipeLoweringError::Encoding(error.to_string()));
    }
    let digest = AppDigest::parse(format!("blake3:{}", sink.hasher.finalize().to_hex()))?;
    Ok((digest, sink.bytes))
}

#[derive(Debug, Error)]
pub(crate) enum AppRecipeLoweringError {
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Recipe(#[from] AppRecipeIrError),
    #[error("recipe lowering authority fence is invalid")]
    InvalidAuthorityFence,
    #[error("compiled recipe identity was substituted during lowering")]
    RecipeSubstitution,
    #[error("recipe schema `{0}` is duplicated")]
    DuplicateSchema(String),
    #[error("recipe lowering is missing a referenced exact schema")]
    MissingSchema,
    #[error("recipe topology changed after validation")]
    CorruptRecipeTopology,
    #[error("recipe node `{node}` uses unsupported v1 kind `{kind}`")]
    UnsupportedNode { node: String, kind: &'static str },
    #[error(
        "parallel recipe node `{parent}` contains nested parallel node `{node}` and would exceed \
         the graph-wide execution-width owner"
    )]
    NestedParallel { parent: String, node: String },
    #[error("recipe node `{0}` has no common effect/receipt owner and cannot be lowered")]
    UnsupportedEffectfulNode(String),
    #[error("lowered recipe recovery detected source, lock, grant, topology or schema drift")]
    RecoveryDrift,
    #[error("persisted lowered recipe plan is corrupt")]
    CorruptPlan,
    #[error("{domain} exceeds the {limit}-byte canonical ceiling")]
    CanonicalByteLimit { domain: &'static str, limit: usize },
    #[error("recipe lowering canonical encoding failed: {0}")]
    Encoding(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::apps::{
        models::{AppDataClassification, AppFieldPath, AppModelProcessing},
        recipe_ir::{
            compile_recipe_value_mapping, compile_workflow_value_schema, AppRecipeCancellationMode,
            AppRecipeEvolutionContract, AppRecipeGraphCeiling, AppRecipeIdempotencyContract,
            AppRecipeIrSource, AppRecipeMigrationPolicy, AppRecipeOutputAuthority,
            AppRecipeOutputKind, AppRecipeProvenanceJoin, AppRecipeVersion,
            AppWorkflowHandlingFloor, AppWorkflowRecordField, AppWorkflowValueSchemaSource,
            AppWorkflowValueSchemaVersion, AppWorkflowValueTypeNode,
        },
        value_mapping::AppValueMappingOperation,
    };

    fn name(value: &str) -> AppName {
        AppName::parse(value).unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn digest(seed: &str) -> AppDigest {
        AppDigest::blake3(seed.as_bytes())
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).unwrap()
    }

    #[test]
    fn ready_runner_has_exact_closed_node_set_and_load_bearing_admission_sources() {
        assert!(app_recipe_runner_ready());
        let expected_supported = [
            "contextual_round",
            "emit_value",
            "get",
            "map",
            "parallel",
            "query",
            "reconcile",
            "sequence",
            "store_transaction",
            "switch",
            "validate",
        ]
        .into_iter()
        .map(name)
        .collect::<BTreeSet<_>>();
        assert_eq!(supported_recipe_node_set().unwrap(), expected_supported);
        let names = APP_RECIPE_RUNTIME_IMPLEMENTATION_PARTS
            .iter()
            .map(|(name, _)| *name)
            .collect::<BTreeSet<_>>();
        assert!(names.contains("apps/manifest.rs"));
        assert!(names.contains("apps/package_lock.rs"));
        assert!(names.contains("apps/workflow_rounds.rs"));
        assert!(names.contains("apps/workflow_commits.rs"));
        assert!(names.contains("apps/workflow_model_context.rs"));
        assert!(names.contains("artifact_v2/app_recipe_model.rs"));
    }

    fn schema(seed: &str) -> AppCompiledWorkflowValueSchema {
        compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([(
                        name("value"),
                        AppWorkflowRecordField {
                            value_type: 1,
                            required: true,
                        },
                    )]),
                },
                AppWorkflowValueTypeNode::Text {
                    max_bytes: u32::try_from(seed.len().max(8) * 8).unwrap(),
                },
            ],
        })
        .unwrap()
    }

    fn typed_output(schema: &AppCompiledWorkflowValueSchema) -> AppRecipeOutputContract {
        AppRecipeOutputContract {
            kind: AppRecipeOutputKind::TypedValue,
            schema_ref: schema.schema_ref().clone(),
            authority: AppRecipeOutputAuthority::Derived,
        }
    }

    fn resources() -> AppRecipeNodeResourceCeiling {
        AppRecipeNodeResourceCeiling {
            max_active_millis: 1_000,
            max_input_bytes: 4_096,
            max_output_bytes: 4_096,
            max_cost_microusd: 0,
            max_tool_calls: 0,
            max_parallelism: 1,
        }
    }

    fn pure_node(
        kind: AppRecipeNodeKind,
        input: &AppCompiledWorkflowValueSchema,
        output: &AppCompiledWorkflowValueSchema,
    ) -> AppRecipeNode {
        AppRecipeNode {
            input_schema_ref: input.schema_ref().clone(),
            output: typed_output(output),
            node: kind,
            effect: AppRecipeEffectContract {
                class: AppRecipeEffectClass::None,
                idempotency: AppRecipeIdempotencyContract::NotApplicable,
                uncertainty: AppRecipeUncertaintyContract::Impossible,
            },
            authority: AppRecipeAuthorityContract {
                primitive_ref: None,
                action_ref: None,
                target_app_ref: None,
                required_grant_refs: BTreeSet::new(),
                resource_scope_refs: BTreeSet::new(),
            },
            resources: resources(),
            retry: AppRecipeRetryContract::None,
            cancellation: AppRecipeCancellationContract {
                mode: AppRecipeCancellationMode::Propagate,
                acknowledgement_timeout_millis: 100,
            },
            provenance_join: AppRecipeProvenanceJoin::Preserve,
        }
    }

    fn recipe_source(
        schema: &AppCompiledWorkflowValueSchema,
        nodes: BTreeMap<AppName, AppRecipeNode>,
    ) -> AppRecipeIrSource {
        AppRecipeIrSource {
            version: AppRecipeVersion::V1,
            input_schema_ref: schema.schema_ref().clone(),
            output: typed_output(schema),
            root: name("root"),
            nodes,
            ceilings: AppRecipeGraphCeiling {
                max_nodes: 16,
                max_edges: 16,
                max_depth: 8,
                max_fan_out: 8,
                max_parallelism: 1,
                max_payload_bytes: 64 * 1024,
                max_active_millis: 16_000,
                max_cost_microusd: 0,
                max_tool_calls: 0,
            },
            evolution: AppRecipeEvolutionContract {
                topology_revision: revision(1),
                migration: AppRecipeMigrationPolicy::RecompileRequired,
                predecessor_recipe_ref: None,
            },
        }
    }

    fn fence(seed: &str) -> AppRecipeLoweringFence {
        AppRecipeLoweringFence::seal(
            reference(&format!("package-revision:{seed}")),
            digest(&format!("lock-{seed}")),
            revision(1),
            digest(&format!("grant-{seed}")),
            digest(&format!("workflow-{seed}")),
        )
        .unwrap()
    }

    fn sequence_recipe(schema: &AppCompiledWorkflowValueSchema) -> AppCompiledRecipeIr {
        let validate = name("validate");
        let emit = name("emit");
        let root = pure_node(
            AppRecipeNodeKind::Sequence {
                steps: vec![validate.clone(), emit.clone()],
            },
            schema,
            schema,
        );
        compile_recipe_ir(
            recipe_source(
                schema,
                BTreeMap::from([
                    (name("root"), root),
                    (
                        validate,
                        pure_node(AppRecipeNodeKind::Validate, schema, schema),
                    ),
                    (
                        emit,
                        pure_node(AppRecipeNodeKind::EmitValue, schema, schema),
                    ),
                ]),
            ),
            &[schema],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn recurring_recipe_control_addresses_exact_runs_beyond_eight_occurrences() {
        use crate::magician_v2::{
            apps::{
                models::AppHandlingLabels,
                recipe_ir::{
                    validate_workflow_value, AppWorkflowValueNode, AppWorkflowValueSource,
                },
                recipe_lifecycle::{
                    AppRecipeCancellationDisposition, AppRecipeCancellationReason,
                    AppRecipeLifecycleError, AppRecipeLifecycleStore,
                },
            },
            artifact_v2::{service::ScopeRef, workspace::ArtifactV2Workspace},
        };
        let temp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path().canonicalize().unwrap());
        let scope = ScopeRef::system_internal_unauthenticated("anonymous", "default");
        let task_id = format!("task_app_{}", "a".repeat(64));
        let store = AppRecipeLifecycleStore::new(workspace.clone());
        let schema = schema("recurring");
        let recipe = sequence_recipe(&schema);
        let plan = lower_recipe_ir(&recipe, &[&schema], fence("recurring")).unwrap();
        let value = validate_workflow_value(
            &schema,
            AppWorkflowValueSource {
                version: AppWorkflowValueSchemaVersion::V1,
                schema_ref: schema.schema_ref().clone(),
                root: 1,
                nodes: vec![
                    AppWorkflowValueNode::Text {
                        value: "fixture".into(),
                    },
                    AppWorkflowValueNode::Record {
                        fields: BTreeMap::from([(name("value"), 0)]),
                    },
                ],
                provenance: BTreeMap::new(),
                resources: BTreeMap::new(),
                handling_labels: AppHandlingLabels {
                    classification: AppDataClassification::Ordinary,
                    model_processing: AppModelProcessing::LocalOnly,
                    policy_digest: digest("policy"),
                    provenance_digest: digest("provenance"),
                },
            },
        )
        .unwrap();
        let mut locators = Vec::new();
        for round in 0..12 {
            locators.push(
                store
                    .reserve(
                        scope.clone(),
                        &task_id,
                        digest(&format!("run-{round}")),
                        plan.clone(),
                        value.clone(),
                    )
                    .await
                    .unwrap(),
            );
        }
        let pending = store.reserved_for_task(&scope, &task_id).await;
        assert!(
            matches!(&pending, Err(AppRecipeLifecycleError::ReservationLimit)),
            "unexpected pending reservation result: {pending:?}"
        );
        for locator in &locators[1..11] {
            store.abandon_before_root(locator).await.unwrap();
        }
        // Historical directories do not count toward pending reservation capacity.
        assert_eq!(
            store
                .reserved_for_task(&scope, &task_id)
                .await
                .unwrap()
                .len(),
            2
        );
        let run_root = workspace
            .task_dir(&scope.principal(), &scope.workspace(), &task_id)
            .join("app_recipe_runs");
        let unrelated = run_root.join("unrelated-corrupt-history");
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(unrelated.join("lifecycle.json"), b"invalid").unwrap();
        let reopened = AppRecipeLifecycleStore::new(workspace.clone());
        for index in [0, 11] {
            let control = reopened
                .control_run_for_execution(&scope, &task_id, &locators[index].execution_id())
                .await
                .unwrap();
            assert_eq!(
                reopened
                    .request_cancellation_for_control(
                        &control,
                        digest(&format!("cancel-{index}")),
                        AppRecipeCancellationReason::CallerCancelled,
                        1
                    )
                    .await
                    .unwrap(),
                AppRecipeCancellationDisposition::CancelledBeforeDispatch
            );
            assert_eq!(
                reopened
                    .request_cancellation_for_control(
                        &control,
                        digest(&format!("cancel-{index}")),
                        AppRecipeCancellationReason::CallerCancelled,
                        1
                    )
                    .await
                    .unwrap(),
                AppRecipeCancellationDisposition::AlreadyRequested
            );
        }
        let missing_segment = "f".repeat(64);
        assert!(reopened
            .control_run_for_execution(&scope, &task_id, &format!("exec_recipe_{missing_segment}"))
            .await
            .is_err());
        assert!(!run_root.join(&missing_segment).exists());
        assert!(reopened
            .control_run_for_execution(&scope, &task_id, "exec_recipe_not_a_digest")
            .await
            .is_err());
        let other_scope = ScopeRef::system_internal_unauthenticated("anonymous", "other");
        assert!(reopened
            .control_run_for_execution(&other_scope, &task_id, &locators[0].execution_id())
            .await
            .is_err());
    }

    #[test]
    fn lowers_sequence_to_closed_existing_owners_deterministically() {
        let input = schema("primary");
        let unrelated = schema("unrelated-schema-with-a-different-ceiling");
        let recipe = sequence_recipe(&input);
        let left = lower_recipe_ir(&recipe, &[&input, &unrelated], fence("same")).unwrap();
        let right = lower_recipe_ir(&recipe, &[&unrelated, &input], fence("same")).unwrap();

        assert_eq!(left.plan_ref(), right.plan_ref());
        assert_eq!(left.plan_digest(), right.plan_digest());
        assert_eq!(left.canonical_encoded_len(), right.canonical_encoded_len());
        assert_eq!(
            left.source.execution_order,
            vec![name("root"), name("validate"), name("emit")]
        );
        assert!(matches!(
            left.source.nodes.get(&name("root")).map(|node| &node.owner),
            Some(AppRecipeExistingOwner::ArtifactV3Sequence)
        ));
    }

    #[test]
    fn lowers_parallel_to_semantic_key_order_and_exact_v3_owner() {
        let item = schema("parallel-item");
        let output = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![
                AppWorkflowValueTypeNode::Array {
                    items: 1,
                    min_items: 2,
                    max_items: 2,
                },
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([(
                        name("value"),
                        AppWorkflowRecordField {
                            value_type: 2,
                            required: true,
                        },
                    )]),
                },
                AppWorkflowValueTypeNode::Text { max_bytes: 104 },
            ],
        })
        .unwrap();
        let mut root = pure_node(
            AppRecipeNodeKind::Parallel {
                branches: BTreeMap::from([
                    (name("alpha"), name("left")),
                    (name("omega"), name("right")),
                ]),
            },
            &item,
            &output,
        );
        root.resources.max_parallelism = 2;
        let mut source = recipe_source(
            &item,
            BTreeMap::from([
                (name("root"), root),
                (
                    name("left"),
                    pure_node(AppRecipeNodeKind::Validate, &item, &item),
                ),
                (
                    name("right"),
                    pure_node(AppRecipeNodeKind::EmitValue, &item, &item),
                ),
            ]),
        );
        source.output = typed_output(&output);
        source.ceilings.max_parallelism = 2;
        let recipe = compile_recipe_ir(source, &[&item, &output]).unwrap();
        let plan = lower_recipe_ir(&recipe, &[&output, &item], fence("parallel")).unwrap();
        let binding = plan.node(&name("root")).unwrap();

        assert_eq!(binding.owner(), &AppRecipeExistingOwner::ArtifactV3Parallel);
        assert_eq!(
            binding
                .children()
                .iter()
                .map(|child| child.semantic_key().unwrap().as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "omega"]
        );
        assert!(binding.children().iter().all(|child| matches!(
            child.input_mapping(),
            AppRecipeValueMapping::ParallelBranch { .. }
        )));
    }

    #[test]
    fn query_binds_only_to_the_existing_read_owner() {
        let input = schema("read-input");
        let projection = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![AppWorkflowValueTypeNode::EntityProjectionRef {
                entity: name("entry"),
                value_schema_ref: input.schema_ref().clone(),
            }],
        })
        .unwrap();
        for (kind, expected) in [
            (
                AppRecipeNodeKind::Query {
                    entity: name("entry"),
                },
                AppRecipeExistingOwner::AppEntityQuery,
            ),
            (
                AppRecipeNodeKind::Get {
                    entity: name("entry"),
                },
                AppRecipeExistingOwner::AppEntityGet,
            ),
        ] {
            let is_get = matches!(&kind, AppRecipeNodeKind::Get { .. });
            let node_input = if is_get { &projection } else { &input };
            let output = AppRecipeOutputContract {
                kind: AppRecipeOutputKind::EntityProjection,
                schema_ref: projection.schema_ref().clone(),
                authority: AppRecipeOutputAuthority::Authoritative,
            };
            let node = AppRecipeNode {
                input_schema_ref: node_input.schema_ref().clone(),
                output: output.clone(),
                node: kind,
                effect: AppRecipeEffectContract {
                    class: AppRecipeEffectClass::ReadOnly,
                    idempotency: AppRecipeIdempotencyContract::Intrinsic,
                    uncertainty: AppRecipeUncertaintyContract::Impossible,
                },
                authority: AppRecipeAuthorityContract {
                    primitive_ref: None,
                    action_ref: None,
                    target_app_ref: None,
                    required_grant_refs: BTreeSet::from([reference("grant:entity-read")]),
                    resource_scope_refs: BTreeSet::new(),
                },
                resources: resources(),
                retry: AppRecipeRetryContract::None,
                cancellation: AppRecipeCancellationContract {
                    mode: AppRecipeCancellationMode::Propagate,
                    acknowledgement_timeout_millis: 100,
                },
                provenance_join: AppRecipeProvenanceJoin::Preserve,
            };
            let source = AppRecipeIrSource {
                version: AppRecipeVersion::V1,
                input_schema_ref: node_input.schema_ref().clone(),
                output,
                root: name("root"),
                nodes: BTreeMap::from([(name("root"), node)]),
                ceilings: AppRecipeGraphCeiling {
                    max_nodes: 4,
                    max_edges: 4,
                    max_depth: 4,
                    max_fan_out: 4,
                    max_parallelism: 1,
                    max_payload_bytes: 64 * 1024,
                    max_active_millis: 4_000,
                    max_cost_microusd: 0,
                    max_tool_calls: 0,
                },
                evolution: AppRecipeEvolutionContract {
                    topology_revision: revision(1),
                    migration: AppRecipeMigrationPolicy::RecompileRequired,
                    predecessor_recipe_ref: None,
                },
            };
            let recipe = compile_recipe_ir(source, &[&input, &projection]).unwrap();
            let plan =
                lower_recipe_ir(&recipe, &[&projection, &input], fence("entity-read")).unwrap();
            assert_eq!(
                plan.source.nodes.get(&name("root")).map(|node| &node.owner),
                Some(&expected)
            );
        }
    }

    #[test]
    fn retry_stays_typed_unsupported_without_a_canonical_attempt_owner() {
        let schema = schema("retry");
        let mut root = pure_node(
            AppRecipeNodeKind::Retry {
                child: name("child"),
            },
            &schema,
            &schema,
        );
        root.retry = AppRecipeRetryContract::Bounded {
            max_attempts: 2,
            initial_backoff_millis: 10,
            max_backoff_millis: 20,
        };
        let recipe = compile_recipe_ir(
            recipe_source(
                &schema,
                BTreeMap::from([
                    (name("root"), root),
                    (
                        name("child"),
                        pure_node(AppRecipeNodeKind::Validate, &schema, &schema),
                    ),
                ]),
            ),
            &[&schema],
        )
        .unwrap();

        assert!(matches!(
            lower_recipe_ir(&recipe, &[&schema], fence("retry")),
            Err(AppRecipeLoweringError::UnsupportedNode { kind: "retry", .. })
        ));
    }

    #[test]
    fn installed_mapping_body_acquires_only_the_pure_mapping_owner() {
        let schema = schema("map");
        let operations = vec![AppValueMappingOperation::Select {
            source: AppFieldPath::parse("value").unwrap(),
            target: AppFieldPath::parse("value").unwrap(),
        }];
        let mapping_digest = compile_recipe_value_mapping(&schema, &schema, operations.clone())
            .unwrap()
            .mapping_digest()
            .clone();
        let recipe = compile_recipe_ir(
            recipe_source(
                &schema,
                BTreeMap::from([(
                    name("root"),
                    pure_node(
                        AppRecipeNodeKind::Map {
                            mapping_digest,
                            operations,
                        },
                        &schema,
                        &schema,
                    ),
                )]),
            ),
            &[&schema],
        )
        .unwrap();

        let plan = lower_recipe_ir(&recipe, &[&schema], fence("map")).unwrap();
        assert_eq!(
            plan.node(&name("root")).map(|node| node.owner()),
            Some(&AppRecipeExistingOwner::WorkflowValueMapper)
        );
    }

    #[test]
    fn recovery_rejects_lock_or_grant_substitution() {
        let schema = schema("recovery");
        let recipe = sequence_recipe(&schema);
        let plan = lower_recipe_ir(&recipe, &[&schema], fence("original")).unwrap();

        assert!(matches!(
            plan.validate_recovery(&recipe, &fence("substituted")),
            Err(AppRecipeLoweringError::RecoveryDrift)
        ));
    }

    #[test]
    fn recipe_topology_revision_is_part_of_plan_identity() {
        let schema = schema("revision");
        let recipe = sequence_recipe(&schema);
        let first = lower_recipe_ir(&recipe, &[&schema], fence("revision")).unwrap();
        let mut revised_source = recipe.source().clone();
        revised_source.evolution.topology_revision = revision(2);
        revised_source.evolution.predecessor_recipe_ref = Some(recipe.recipe_ref().clone());
        let revised = compile_recipe_ir(revised_source, &[&schema]).unwrap();
        let second = lower_recipe_ir(&revised, &[&schema], fence("revision")).unwrap();

        assert_ne!(first.plan_ref(), second.plan_ref());
        assert_ne!(first.topology_digest(), second.topology_digest());
    }
}
