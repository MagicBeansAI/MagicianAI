//! Durable Artifact V3 sidecar for one exact lowered recipe run.
//!
//! This is not a task or execution state machine. Artifact V3 remains the sole
//! owner of task/execution status. The sidecar seals the recipe plan and typed
//! input before the canonical root is created, journals per-node dispatch and
//! output evidence, and gives restart recovery an exact adoption target.

use std::{
    collections::BTreeMap,
    fmt,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, Weak},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    entity_store::{AppEntityStoreError, AppRecipeRecordLocator},
    models::{AppContractError, AppDigest, AppName, AppReference},
    recipe_ir::{
        validate_workflow_value, AppCompiledRecipeIr, AppCompiledWorkflowValueSchema,
        AppRecipeIrError, AppRecipeUncertaintyContract, AppValidatedWorkflowValue,
        AppWorkflowValueSource,
    },
    recipe_lowering::{
        AppLoweredRecipePlan, AppLoweredRecipePlanSource, AppRecipeExistingOwner,
        AppRecipeLoweringError, AppRecipeLoweringFence,
    },
};
use crate::magician_v2::{
    agents::storage::AgentStorage,
    artifact_v2::{
        models::ExecutionState,
        service::{ArtifactV2Error, ScopeRef},
        workspace::ArtifactV2Workspace,
    },
};

const LIFECYCLE_SCHEMA: &str = "magician.app-recipe-artifact-lifecycle.v1";
const BINDING_SCHEMA: &str = "magician.app-recipe-run-binding.v1";
const NODE_OUTPUT_SCHEMA: &str = "magician.app-recipe-node-output.v2";
const LIFECYCLE_FILE: &str = "lifecycle.json";
const BINDING_FILE: &str = "binding.json";
const NODE_OUTPUT_FILE: &str = "output.json";
const RECORD_LOCATOR_FILE: &str = "record-locator.json";
const MAX_LIFECYCLE_BYTES: usize = 2 * 1024 * 1024;
const MAX_LIFECYCLE_DEPTH: usize = 48;
const MAX_LIFECYCLE_NODES: usize = 40_000;
const MAX_NODE_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_NODE_OUTPUT_DEPTH: usize = 48;
const MAX_NODE_OUTPUT_NODES: usize = 24_000;
const MAX_EXECUTION_STATE_BYTES: u64 = 512 * 1024;
const MAX_EXECUTION_STATE_DEPTH: usize = 32;
const MAX_EXECUTION_STATE_NODES: usize = 16_384;

fn process_lifecycle_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<BTreeMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AppRecipeLaunchPhase {
    Reserved,
    Attached,
    CompletionPrepared,
    AbandonedBeforeRoot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AppRecipeNodePhase {
    Pending,
    DispatchIntent,
    Settled,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppRecipeNodeProgressRecord {
    binding_digest: AppDigest,
    phase: AppRecipeNodePhase,
    attempt: u16,
    output_digest: Option<AppDigest>,
    output_encoded_len: Option<u64>,
    owner_receipt_digest: Option<AppDigest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppRecipeCancellationReason {
    CallerCancelled,
    Deadline,
    GrantRevoked,
    SourceDrift,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppRecipeCancellationIntent {
    request_digest: AppDigest,
    reason: AppRecipeCancellationReason,
    requested_at_ms: i64,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppRecipeLifecycleDocument {
    schema: String,
    run_ref: AppReference,
    run_digest: AppDigest,
    run_key_digest: AppDigest,
    plan_ref: AppReference,
    plan_digest: AppDigest,
    plan_encoded_len: u64,
    input_schema_ref: AppReference,
    input_digest: AppDigest,
    input_encoded_len: u64,
    phase: AppRecipeLaunchPhase,
    execution_id: Option<String>,
    nodes: BTreeMap<AppName, AppRecipeNodeProgressRecord>,
    cancellation: Option<AppRecipeCancellationIntent>,
    document_digest: AppDigest,
}

impl fmt::Debug for AppRecipeLifecycleDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppRecipeLifecycleDocument")
            .field("run_ref", &self.run_ref)
            .field("run_digest", &self.run_digest)
            .field("plan_digest", &self.plan_digest)
            .field("input_schema_ref", &self.input_schema_ref)
            .field("input_digest", &self.input_digest)
            .field("phase", &self.phase)
            .field("node_count", &self.nodes.len())
            .field("has_cancellation", &self.cancellation.is_some())
            .field("document_digest", &self.document_digest)
            .finish()
    }
}

#[derive(Serialize)]
struct AppRecipeLifecycleIdentity<'a> {
    schema: &'a str,
    run_ref: &'a AppReference,
    run_digest: &'a AppDigest,
    run_key_digest: &'a AppDigest,
    plan_ref: &'a AppReference,
    plan_digest: &'a AppDigest,
    plan_encoded_len: u64,
    input_schema_ref: &'a AppReference,
    input_digest: &'a AppDigest,
    input_encoded_len: u64,
    phase: AppRecipeLaunchPhase,
    execution_id: &'a Option<String>,
    nodes: &'a BTreeMap<AppName, AppRecipeNodeProgressRecord>,
    cancellation: &'a Option<AppRecipeCancellationIntent>,
}

impl AppRecipeLifecycleDocument {
    fn identity(&self) -> AppRecipeLifecycleIdentity<'_> {
        AppRecipeLifecycleIdentity {
            schema: &self.schema,
            run_ref: &self.run_ref,
            run_digest: &self.run_digest,
            run_key_digest: &self.run_key_digest,
            plan_ref: &self.plan_ref,
            plan_digest: &self.plan_digest,
            plan_encoded_len: self.plan_encoded_len,
            input_schema_ref: &self.input_schema_ref,
            input_digest: &self.input_digest,
            input_encoded_len: self.input_encoded_len,
            phase: self.phase,
            execution_id: &self.execution_id,
            nodes: &self.nodes,
            cancellation: &self.cancellation,
        }
    }

    fn refresh_digest(&mut self) -> Result<(), AppRecipeLifecycleError> {
        self.document_digest =
            stream_identity("recipe lifecycle", &self.identity(), MAX_LIFECYCLE_BYTES)?.0;
        Ok(())
    }

    fn validate_integrity(&self) -> Result<(), AppRecipeLifecycleError> {
        if self.schema != LIFECYCLE_SCHEMA
            || self.document_digest
                != stream_identity("recipe lifecycle", &self.identity(), MAX_LIFECYCLE_BYTES)?.0
            || self.nodes.is_empty()
        {
            return Err(AppRecipeLifecycleError::CorruptLifecycle);
        }
        let expected_run =
            run_identity(&self.run_key_digest, &self.plan_digest, &self.input_digest)?;
        if expected_run.0 != self.run_ref || expected_run.1 != self.run_digest {
            return Err(AppRecipeLifecycleError::CorruptLifecycle);
        }
        Ok(())
    }
}

/// Immutable plan/input bytes are separate from the hot lifecycle journal so
/// each node transition rewrites only bounded metadata, never the full input.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppRecipeRunBindingDocument {
    schema: String,
    run_ref: AppReference,
    run_digest: AppDigest,
    plan_ref: AppReference,
    plan_digest: AppDigest,
    plan_encoded_len: u64,
    plan: AppLoweredRecipePlanSource,
    input_schema_ref: AppReference,
    input_digest: AppDigest,
    input_encoded_len: u64,
    input: AppWorkflowValueSource,
    document_digest: AppDigest,
}

#[derive(Serialize)]
struct AppRecipeRunBindingIdentity<'a> {
    schema: &'a str,
    run_ref: &'a AppReference,
    run_digest: &'a AppDigest,
    plan_ref: &'a AppReference,
    plan_digest: &'a AppDigest,
    plan_encoded_len: u64,
    plan: &'a AppLoweredRecipePlanSource,
    input_schema_ref: &'a AppReference,
    input_digest: &'a AppDigest,
    input_encoded_len: u64,
    input: &'a AppWorkflowValueSource,
}

impl AppRecipeRunBindingDocument {
    fn identity(&self) -> AppRecipeRunBindingIdentity<'_> {
        AppRecipeRunBindingIdentity {
            schema: &self.schema,
            run_ref: &self.run_ref,
            run_digest: &self.run_digest,
            plan_ref: &self.plan_ref,
            plan_digest: &self.plan_digest,
            plan_encoded_len: self.plan_encoded_len,
            plan: &self.plan,
            input_schema_ref: &self.input_schema_ref,
            input_digest: &self.input_digest,
            input_encoded_len: self.input_encoded_len,
            input: &self.input,
        }
    }

    fn refresh_digest(&mut self) -> Result<(), AppRecipeLifecycleError> {
        self.document_digest =
            stream_identity("recipe run binding", &self.identity(), MAX_LIFECYCLE_BYTES)?.0;
        Ok(())
    }

    fn validate_integrity(&self) -> Result<AppLoweredRecipePlan, AppRecipeLifecycleError> {
        if self.schema != BINDING_SCHEMA
            || self.input.schema_ref != self.input_schema_ref
            || self.document_digest
                != stream_identity("recipe run binding", &self.identity(), MAX_LIFECYCLE_BYTES)?.0
        {
            return Err(AppRecipeLifecycleError::CorruptLifecycle);
        }
        Ok(AppLoweredRecipePlan::restore(
            self.plan_ref.clone(),
            self.plan_digest.clone(),
            self.plan_encoded_len,
            self.plan.clone(),
        )?)
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppRecipeNodeOutputDocument {
    schema: String,
    plan_digest: AppDigest,
    node_id: AppName,
    binding_digest: AppDigest,
    attempt: u16,
    claim_owner_id: String,
    claim_epoch: u64,
    value_schema_ref: AppReference,
    value_digest: AppDigest,
    value_encoded_len: u64,
    settled_at_ms: i64,
    persisted_observed_at_ms: Option<i64>,
    value: AppWorkflowValueSource,
    owner_receipt_digest: Option<AppDigest>,
    document_digest: AppDigest,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct AppRecipeRecordLocatorDocument {
    schema: String,
    plan_digest: AppDigest,
    node_id: AppName,
    binding_digest: AppDigest,
    locator: AppRecipeRecordLocator,
    document_digest: AppDigest,
}

#[derive(Serialize)]
struct AppRecipeRecordLocatorDocumentIdentity<'a> {
    schema: &'a str,
    plan_digest: &'a AppDigest,
    node_id: &'a AppName,
    binding_digest: &'a AppDigest,
    locator: &'a AppRecipeRecordLocator,
}

impl AppRecipeRecordLocatorDocument {
    fn identity(&self) -> AppRecipeRecordLocatorDocumentIdentity<'_> {
        AppRecipeRecordLocatorDocumentIdentity {
            schema: &self.schema,
            plan_digest: &self.plan_digest,
            node_id: &self.node_id,
            binding_digest: &self.binding_digest,
            locator: &self.locator,
        }
    }

    fn refresh_digest(&mut self) -> Result<(), AppRecipeLifecycleError> {
        self.document_digest = stream_identity(
            "recipe record locator",
            &self.identity(),
            MAX_NODE_OUTPUT_BYTES,
        )?
        .0;
        Ok(())
    }

    fn validate_integrity(&self) -> Result<(), AppRecipeLifecycleError> {
        self.locator.validate_integrity()?;
        if self.schema != "magician.app-recipe-record-locator-evidence.v1"
            || self.document_digest
                != stream_identity(
                    "recipe record locator",
                    &self.identity(),
                    MAX_NODE_OUTPUT_BYTES,
                )?
                .0
        {
            return Err(AppRecipeLifecycleError::CorruptRecordLocator);
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct AppRecipeNodeOutputIdentity<'a> {
    schema: &'a str,
    plan_digest: &'a AppDigest,
    node_id: &'a AppName,
    binding_digest: &'a AppDigest,
    attempt: u16,
    claim_owner_id: &'a str,
    claim_epoch: u64,
    value_schema_ref: &'a AppReference,
    value_digest: &'a AppDigest,
    value_encoded_len: u64,
    settled_at_ms: i64,
    persisted_observed_at_ms: Option<i64>,
    value: &'a AppWorkflowValueSource,
    owner_receipt_digest: &'a Option<AppDigest>,
}

/// Move-only proof that exact typed bytes were atomically persisted and then
/// observed for one canonical claim. Only the lifecycle store can mint it;
/// the Artifact reducer bridge consumes it with the matching permit.
pub(crate) struct AppRecipePersistedNodeOutput {
    claim_owner_id: String,
    claim_epoch: u64,
    persisted_observed_at_ms: i64,
    value_digest: AppDigest,
    value_encoded_len: u64,
}

impl AppRecipePersistedNodeOutput {
    pub(crate) fn matches_claim(&self, claim_owner_id: &str, claim_epoch: u64) -> bool {
        self.claim_owner_id == claim_owner_id && self.claim_epoch == claim_epoch
    }

    pub(crate) fn claim_owner_id(&self) -> &str {
        &self.claim_owner_id
    }

    pub(crate) fn claim_epoch(&self) -> u64 {
        self.claim_epoch
    }

    pub(crate) fn persisted_observed_at_ms(&self) -> i64 {
        self.persisted_observed_at_ms
    }

    pub(crate) fn value_digest(&self) -> &AppDigest {
        &self.value_digest
    }

    pub(crate) fn value_encoded_len(&self) -> u64 {
        self.value_encoded_len
    }
}

impl AppRecipeNodeOutputDocument {
    fn identity(&self) -> AppRecipeNodeOutputIdentity<'_> {
        AppRecipeNodeOutputIdentity {
            schema: &self.schema,
            plan_digest: &self.plan_digest,
            node_id: &self.node_id,
            binding_digest: &self.binding_digest,
            attempt: self.attempt,
            claim_owner_id: &self.claim_owner_id,
            claim_epoch: self.claim_epoch,
            value_schema_ref: &self.value_schema_ref,
            value_digest: &self.value_digest,
            value_encoded_len: self.value_encoded_len,
            settled_at_ms: self.settled_at_ms,
            persisted_observed_at_ms: self.persisted_observed_at_ms,
            value: &self.value,
            owner_receipt_digest: &self.owner_receipt_digest,
        }
    }

    fn refresh_digest(&mut self) -> Result<(), AppRecipeLifecycleError> {
        self.document_digest = stream_identity(
            "recipe node output",
            &self.identity(),
            MAX_NODE_OUTPUT_BYTES,
        )?
        .0;
        Ok(())
    }

    fn validate_integrity(&self) -> Result<(), AppRecipeLifecycleError> {
        if self.schema != NODE_OUTPUT_SCHEMA
            || self.attempt == 0
            || self.claim_owner_id.is_empty()
            || self.claim_owner_id.len() > 160
            || self.claim_owner_id.chars().any(char::is_control)
            || self.claim_epoch == 0
            || self.settled_at_ms <= 0
            || self
                .persisted_observed_at_ms
                .is_some_and(|observed| observed < self.settled_at_ms)
            || self.value.schema_ref != self.value_schema_ref
            || self.document_digest
                != stream_identity(
                    "recipe node output",
                    &self.identity(),
                    MAX_NODE_OUTPUT_BYTES,
                )?
                .0
        {
            return Err(AppRecipeLifecycleError::CorruptNodeOutput);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AppRecipeRunLocator {
    scope: ScopeRef,
    task_id: String,
    run_segment: String,
    run_ref: AppReference,
    run_digest: AppDigest,
}

impl AppRecipeRunLocator {
    pub(crate) fn scope(&self) -> &ScopeRef {
        &self.scope
    }

    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    pub(crate) fn run_ref(&self) -> &AppReference {
        &self.run_ref
    }

    pub(crate) fn run_digest(&self) -> &AppDigest {
        &self.run_digest
    }

    /// Canonical Artifact root identity for this exact plan/input/run-key
    /// tuple. The digest segment is already path-safe and replay-stable.
    pub(crate) fn execution_id(&self) -> String {
        format!("exec_recipe_{}", self.run_segment)
    }

    pub(crate) fn binding_relative_path(&self) -> String {
        format!("app_recipe_runs/{}/{}", self.run_segment, BINDING_FILE)
    }

    pub(crate) fn node_output_relative_path(&self, node_id: &AppName) -> String {
        format!(
            "app_recipe_runs/{}/nodes/{}/{}",
            self.run_segment,
            node_id.as_str(),
            NODE_OUTPUT_FILE
        )
    }
}

pub(crate) struct AppRecipeAttachedRun {
    locator: AppRecipeRunLocator,
    execution_id: String,
    plan: AppLoweredRecipePlan,
    input: AppValidatedWorkflowValue,
}

/// Sealed launch material reopened before the canonical root commit. It has no
/// node-dispatch methods; Artifact V3 consumes it only to atomically install
/// the immutable recipe schedule beside the deterministic root.
pub(crate) struct AppRecipeReservedRun {
    locator: AppRecipeRunLocator,
    plan: AppLoweredRecipePlan,
}

pub(crate) struct AppRecipeControlRun {
    locator: AppRecipeRunLocator,
    execution_id: String,
}

impl AppRecipeReservedRun {
    pub(crate) fn locator(&self) -> &AppRecipeRunLocator {
        &self.locator
    }

    pub(crate) fn plan(&self) -> &AppLoweredRecipePlan {
        &self.plan
    }
}

impl fmt::Debug for AppRecipeAttachedRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppRecipeAttachedRun")
            .field("run_ref", self.locator.run_ref())
            .field("plan_digest", self.plan.plan_digest())
            .field("input_digest", self.input.value_digest())
            .finish()
    }
}

impl AppRecipeAttachedRun {
    pub(crate) fn locator(&self) -> &AppRecipeRunLocator {
        &self.locator
    }

    pub(crate) fn plan(&self) -> &AppLoweredRecipePlan {
        &self.plan
    }

    pub(crate) fn input(&self) -> &AppValidatedWorkflowValue {
        &self.input
    }

    pub(crate) fn execution_id(&self) -> &str {
        &self.execution_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppRecipeNodeRecoveryAction {
    Dispatch,
    RetryIdenticalRead,
    AdoptPersistedOutput,
    AlreadySettled,
    Cancelled,
    OutcomeUncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppRecipeCancellationDisposition {
    CancelledBeforeDispatch,
    AwaitingReadSettlement,
    AlreadyRequested,
}

pub(crate) struct AppRecipePreparedCompletion {
    value: AppValidatedWorkflowValue,
    recipe_ref: AppReference,
    topology_digest: AppDigest,
    plan_digest: AppDigest,
}

impl fmt::Debug for AppRecipePreparedCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppRecipePreparedCompletion")
            .field("recipe_ref", &self.recipe_ref)
            .field("topology_digest", &self.topology_digest)
            .field("plan_digest", &self.plan_digest)
            .field("value_digest", self.value.value_digest())
            .finish()
    }
}

impl AppRecipePreparedCompletion {
    pub(crate) fn value(&self) -> &AppValidatedWorkflowValue {
        &self.value
    }

    pub(crate) fn recipe_ref(&self) -> &AppReference {
        &self.recipe_ref
    }

    pub(crate) fn topology_digest(&self) -> &AppDigest {
        &self.topology_digest
    }

    pub(crate) fn plan_digest(&self) -> &AppDigest {
        &self.plan_digest
    }
}

pub(crate) struct AppRecipeLifecycleStore {
    workspace: ArtifactV2Workspace,
}

impl AppRecipeLifecycleStore {
    pub(crate) fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    /// Persist exact plan and input before Artifact creates a root execution.
    /// Identical replay returns the same locator; substitution fails closed.
    pub(crate) async fn reserve(
        &self,
        scope: ScopeRef,
        task_id: &str,
        run_key_digest: AppDigest,
        plan: AppLoweredRecipePlan,
        input: AppValidatedWorkflowValue,
    ) -> Result<AppRecipeRunLocator, AppRecipeLifecycleError> {
        ArtifactV2Workspace::validate_task_id(task_id)?;
        if input.schema_ref() != plan.input_schema_ref() {
            return Err(AppRecipeLifecycleError::InputSchemaSubstitution);
        }
        let root = plan
            .node(plan.root())
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        if input.canonical_encoded_len() > root.resources().max_input_bytes {
            return Err(AppRecipeLifecycleError::InputByteLimit);
        }
        let (run_ref, run_digest) =
            run_identity(&run_key_digest, plan.plan_digest(), input.value_digest())?;
        let run_segment = digest_segment(&run_digest)?;
        let locator = AppRecipeRunLocator {
            scope,
            task_id: task_id.to_owned(),
            run_segment,
            run_ref,
            run_digest,
        };
        let path = self.lifecycle_path(&locator);
        if let Some(parent) = path.parent() {
            self.workspace.create_dir_all_path(parent).await?;
        }
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        if let Some(existing) = self.read_document(&path).await? {
            let binding = self.read_binding(&self.binding_path(&locator)).await?;
            validate_binding_pair(&existing, &binding)?;
            if existing.phase == AppRecipeLaunchPhase::AbandonedBeforeRoot
                || existing.run_ref != locator.run_ref
                || existing.run_digest != locator.run_digest
                || existing.run_key_digest != run_key_digest
                || existing.plan_digest != *plan.plan_digest()
                || existing.input_digest != *input.value_digest()
                || existing.input_encoded_len != input.canonical_encoded_len()
                || binding.run_ref != locator.run_ref
                || binding.run_digest != locator.run_digest
                || binding.plan_digest != *plan.plan_digest()
                || binding.input_digest != *input.value_digest()
                || binding.input_encoded_len != input.canonical_encoded_len()
                || binding.input != *input.source()
            {
                return Err(AppRecipeLifecycleError::RunSubstitution);
            }
            return Ok(locator);
        }

        let mut nodes = BTreeMap::new();
        for node_id in plan.execution_order() {
            let binding = plan
                .node(node_id)
                .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
            nodes.insert(
                node_id.clone(),
                AppRecipeNodeProgressRecord {
                    binding_digest: binding.binding_digest().clone(),
                    phase: AppRecipeNodePhase::Pending,
                    attempt: 0,
                    output_digest: None,
                    output_encoded_len: None,
                    owner_receipt_digest: None,
                },
            );
        }
        let binding_path = self.binding_path(&locator);
        let input_schema_ref = input.schema_ref().clone();
        let input_digest = input.value_digest().clone();
        let input_encoded_len = input.canonical_encoded_len();
        if self.workspace.exists_path(&binding_path).await? {
            let existing = self.read_binding(&binding_path).await?;
            if existing.run_ref != locator.run_ref
                || existing.run_digest != locator.run_digest
                || existing.plan_ref != *plan.plan_ref()
                || existing.plan_digest != *plan.plan_digest()
                || existing.plan_encoded_len != plan.canonical_encoded_len()
                || existing.plan != *plan.source()
                || existing.input_schema_ref != input_schema_ref
                || existing.input_digest != input_digest
                || existing.input_encoded_len != input_encoded_len
                || existing.input != *input.source()
            {
                return Err(AppRecipeLifecycleError::RunSubstitution);
            }
        } else {
            let mut binding = AppRecipeRunBindingDocument {
                schema: BINDING_SCHEMA.to_owned(),
                run_ref: locator.run_ref.clone(),
                run_digest: locator.run_digest.clone(),
                plan_ref: plan.plan_ref().clone(),
                plan_digest: plan.plan_digest().clone(),
                plan_encoded_len: plan.canonical_encoded_len(),
                plan: plan.source().clone(),
                input_schema_ref: input_schema_ref.clone(),
                input_digest: input_digest.clone(),
                input_encoded_len,
                input: input.into_source(),
                document_digest: AppDigest::blake3(b"pending-recipe-run-binding"),
            };
            binding.refresh_digest()?;
            self.write_binding(&binding_path, binding).await?;
        }
        let mut document = AppRecipeLifecycleDocument {
            schema: LIFECYCLE_SCHEMA.to_owned(),
            run_ref: locator.run_ref.clone(),
            run_digest: locator.run_digest.clone(),
            run_key_digest,
            plan_ref: plan.plan_ref().clone(),
            plan_digest: plan.plan_digest().clone(),
            plan_encoded_len: plan.canonical_encoded_len(),
            input_schema_ref,
            input_digest,
            input_encoded_len,
            phase: AppRecipeLaunchPhase::Reserved,
            execution_id: None,
            nodes,
            cancellation: None,
            document_digest: AppDigest::blake3(b"pending-recipe-lifecycle"),
        };
        document.refresh_digest()?;
        self.write_document(&path, document).await?;
        Ok(locator)
    }

    /// Reopen a pristine reservation for atomic V3 root/schedule creation.
    /// This validates both sealed documents but deliberately cannot attach or
    /// mint a node permit.
    pub(crate) async fn reopen_reserved(
        &self,
        locator: AppRecipeRunLocator,
    ) -> Result<AppRecipeReservedRun, AppRecipeLifecycleError> {
        let path = self.lifecycle_path(&locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_locator(&locator, &document)?;
        if document.phase != AppRecipeLaunchPhase::Reserved || document.execution_id.is_some() {
            return Err(AppRecipeLifecycleError::ExecutionSubstitution);
        }
        let binding = self.read_binding(&self.binding_path(&locator)).await?;
        validate_binding_pair(&document, &binding)?;
        Ok(AppRecipeReservedRun {
            locator,
            plan: binding.validate_integrity()?,
        })
    }

    /// Enumerate the small, bounded set of pre-root intents for startup
    /// adoption. Directory names are never trusted as identity: the sealed
    /// lifecycle digest reconstructs and validates every returned locator.
    pub(crate) async fn reserved_for_task(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<Vec<AppRecipeRunLocator>, AppRecipeLifecycleError> {
        ArtifactV2Workspace::validate_task_id(task_id)?;
        let root = self
            .workspace
            .task_dir(&scope.principal(), &scope.workspace(), task_id)
            .join("app_recipe_runs");
        let entries = self.workspace.read_dir_path_or_empty(&root).await?;
        let mut locators = Vec::new();
        for entry in entries {
            if !entry.is_dir {
                continue;
            }
            let path = root.join(&entry.file_name).join(LIFECYCLE_FILE);
            let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
            let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
                .await
                .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
            let Some(document) = self.read_document(&path).await? else {
                continue;
            };
            if document.phase != AppRecipeLaunchPhase::Reserved || document.execution_id.is_some() {
                continue;
            }
            let run_segment = digest_segment(&document.run_digest)?;
            if run_segment != entry.file_name {
                return Err(AppRecipeLifecycleError::RunSubstitution);
            }
            let locator = AppRecipeRunLocator {
                scope: scope.clone(),
                task_id: task_id.to_owned(),
                run_segment,
                run_ref: document.run_ref.clone(),
                run_digest: document.run_digest.clone(),
            };
            let binding = self.read_binding(&self.binding_path(&locator)).await?;
            validate_binding_pair(&document, &binding)?;
            locators.push(locator);
            // Retained history does not consume pending-admission capacity.
            if locators.len() > 8 {
                return Err(AppRecipeLifecycleError::ReservationLimit);
            }
        }
        Ok(locators)
    }

    pub(crate) async fn abandon_before_root(
        &self,
        locator: &AppRecipeRunLocator,
    ) -> Result<(), AppRecipeLifecycleError> {
        let path = self.lifecycle_path(locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_locator(locator, &document)?;
        if document.phase == AppRecipeLaunchPhase::AbandonedBeforeRoot {
            return Ok(());
        }
        if document.phase != AppRecipeLaunchPhase::Reserved || document.execution_id.is_some() {
            return Err(AppRecipeLifecycleError::ExecutionSubstitution);
        }
        document.phase = AppRecipeLaunchPhase::AbandonedBeforeRoot;
        document.refresh_digest()?;
        self.write_document(&path, document).await
    }

    /// Reopen only the exact accepted sidecar identity needed for historical
    /// cancellation. No mutable package/source/grant bytes are consulted and
    /// this handle has no plan, value, node or disclosure authority.
    pub(crate) async fn control_run_for_execution(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> Result<AppRecipeControlRun, AppRecipeLifecycleError> {
        validate_runtime_segment(execution_id)?;
        ArtifactV2Workspace::validate_task_id(task_id)?;
        let root = self
            .workspace
            .task_dir(&scope.principal(), &scope.workspace(), task_id)
            .join("app_recipe_runs");
        let entries = if let Some(segment) = execution_id.strip_prefix("exec_recipe_") {
            // Canonical roots embed the exact run digest. Read only that run:
            // a recurring task may retain arbitrarily many earlier occurrences.
            if segment.len() != 64
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(AppRecipeLifecycleError::ExecutionSubstitution);
            }
            vec![(segment.to_owned(), root.join(segment).join(LIFECYCLE_FILE))]
        } else {
            // Compatibility for pre-canonical attached roots. Such legacy tasks
            // retain the original bounded directory search.
            let entries = self.workspace.read_dir_path_or_empty(&root).await?;
            if entries.len() > 8 {
                return Err(AppRecipeLifecycleError::ReservationLimit);
            }
            entries
                .into_iter()
                .filter(|entry| entry.is_dir)
                .map(|entry| {
                    let path = root.join(&entry.file_name).join(LIFECYCLE_FILE);
                    (entry.file_name, path)
                })
                .collect()
        };
        let mut found = None;
        for (run_segment, path) in entries {
            // Unknown roots are reads, and must not create lock directories.
            if self.workspace.metadata_path(&path).await?.is_none() {
                continue;
            }
            let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
            let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
                .await
                .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
            let Some(document) = self.read_document(&path).await? else {
                continue;
            };
            let locator = AppRecipeRunLocator {
                scope: scope.clone(),
                task_id: task_id.to_owned(),
                run_segment: digest_segment(&document.run_digest)?,
                run_ref: document.run_ref.clone(),
                run_digest: document.run_digest.clone(),
            };
            let execution_matches = document.execution_id.as_deref() == Some(execution_id)
                || (document.phase == AppRecipeLaunchPhase::Reserved
                    && document.execution_id.is_none()
                    && locator.execution_id() == execution_id);
            if !execution_matches {
                continue;
            }
            if !matches!(
                document.phase,
                AppRecipeLaunchPhase::Reserved
                    | AppRecipeLaunchPhase::Attached
                    | AppRecipeLaunchPhase::CompletionPrepared
            ) {
                return Err(AppRecipeLifecycleError::ExecutionSubstitution);
            }
            if locator.run_segment != run_segment || found.is_some() {
                return Err(AppRecipeLifecycleError::RunSubstitution);
            }
            let binding = self.read_binding(&self.binding_path(&locator)).await?;
            validate_binding_pair(&document, &binding)?;
            found = Some(AppRecipeControlRun {
                locator,
                execution_id: execution_id.to_owned(),
            });
        }
        found.ok_or(AppRecipeLifecycleError::ExecutionSubstitution)
    }

    pub(crate) async fn request_cancellation_for_control(
        &self,
        run: &AppRecipeControlRun,
        request_digest: AppDigest,
        reason: AppRecipeCancellationReason,
        requested_at_ms: i64,
    ) -> Result<AppRecipeCancellationDisposition, AppRecipeLifecycleError> {
        if requested_at_ms <= 0 {
            return Err(AppRecipeLifecycleError::InvalidCancellation);
        }
        let path = self.lifecycle_path(&run.locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_locator(&run.locator, &document)?;
        let execution_matches = document.execution_id.as_deref() == Some(run.execution_id.as_str())
            || (document.phase == AppRecipeLaunchPhase::Reserved
                && document.execution_id.is_none()
                && run.locator.execution_id() == run.execution_id);
        if !execution_matches
            || !matches!(
                document.phase,
                AppRecipeLaunchPhase::Reserved
                    | AppRecipeLaunchPhase::Attached
                    | AppRecipeLaunchPhase::CompletionPrepared
            )
        {
            return Err(AppRecipeLifecycleError::ExecutionSubstitution);
        }
        if let Some(existing) = &document.cancellation {
            if existing.request_digest != request_digest || existing.reason != reason {
                return Err(AppRecipeLifecycleError::ConflictingCancellation);
            }
            return Ok(AppRecipeCancellationDisposition::AlreadyRequested);
        }
        document.cancellation = Some(AppRecipeCancellationIntent {
            request_digest,
            reason,
            requested_at_ms,
        });
        let disposition = if document
            .nodes
            .values()
            .any(|progress| progress.phase == AppRecipeNodePhase::DispatchIntent)
        {
            AppRecipeCancellationDisposition::AwaitingReadSettlement
        } else {
            AppRecipeCancellationDisposition::CancelledBeforeDispatch
        };
        document.refresh_digest()?;
        self.write_document(&path, document).await?;
        Ok(disposition)
    }

    /// Attach the reservation to the canonical Artifact root row and reopen
    /// exact plan/input under the current source, lock, grant and schemas.
    pub(crate) async fn attach(
        &self,
        locator: AppRecipeRunLocator,
        execution_id: &str,
        current_recipe: &AppCompiledRecipeIr,
        current_fence: &AppRecipeLoweringFence,
        input_schema: &AppCompiledWorkflowValueSchema,
    ) -> Result<AppRecipeAttachedRun, AppRecipeLifecycleError> {
        validate_runtime_segment(execution_id)?;
        let path = self.lifecycle_path(&locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_locator(&locator, &document)?;
        let binding = self.read_binding(&self.binding_path(&locator)).await?;
        validate_binding_pair(&document, &binding)?;
        let plan = binding.validate_integrity()?;
        plan.validate_recovery(current_recipe, current_fence)?;
        if input_schema.schema_ref() != plan.input_schema_ref() {
            return Err(AppRecipeLifecycleError::InputSchemaSubstitution);
        }
        let input = validate_workflow_value(input_schema, binding.input)?;
        if input.value_digest() != &document.input_digest
            || input.canonical_encoded_len() != document.input_encoded_len
        {
            return Err(AppRecipeLifecycleError::InputSubstitution);
        }
        self.validate_execution(&locator, execution_id).await?;
        match (&document.execution_id, document.phase) {
            (None, AppRecipeLaunchPhase::Reserved) => {
                document.execution_id = Some(execution_id.to_owned());
                document.phase = AppRecipeLaunchPhase::Attached;
                document.refresh_digest()?;
                self.write_document(&path, document).await?;
            },
            (Some(existing), AppRecipeLaunchPhase::Attached)
            | (Some(existing), AppRecipeLaunchPhase::CompletionPrepared)
                if existing == execution_id => {},
            _ => return Err(AppRecipeLifecycleError::ExecutionSubstitution),
        }
        Ok(AppRecipeAttachedRun {
            locator,
            execution_id: execution_id.to_owned(),
            plan,
            input,
        })
    }

    /// Recovery uses the persisted locator and canonical root id. It performs
    /// the same exact revalidation as first attachment and never recompiles a
    /// changed recipe into an already-running execution.
    pub(crate) async fn reopen_attached(
        &self,
        locator: AppRecipeRunLocator,
        execution_id: &str,
        current_recipe: &AppCompiledRecipeIr,
        current_fence: &AppRecipeLoweringFence,
        input_schema: &AppCompiledWorkflowValueSchema,
    ) -> Result<AppRecipeAttachedRun, AppRecipeLifecycleError> {
        self.attach(
            locator,
            execution_id,
            current_recipe,
            current_fence,
            input_schema,
        )
        .await
    }

    /// Persist a node dispatch intent before invoking its owner. A crash with
    /// an output sidecar adopts that output; a read-only in-flight node retries
    /// the identical request; no effectful node is admitted by v1 lowering.
    pub(crate) async fn begin_or_recover_node(
        &self,
        run: &AppRecipeAttachedRun,
        node_id: &AppName,
    ) -> Result<AppRecipeNodeRecoveryAction, AppRecipeLifecycleError> {
        let binding = run
            .plan
            .node(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        let path = self.lifecycle_path(&run.locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_attached(run, &document)?;
        if document.cancellation.is_some() {
            return Ok(AppRecipeNodeRecoveryAction::Cancelled);
        }
        let progress = document
            .nodes
            .get(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        match progress.phase {
            AppRecipeNodePhase::Settled => return Ok(AppRecipeNodeRecoveryAction::AlreadySettled),
            AppRecipeNodePhase::Skipped => {
                return Err(AppRecipeLifecycleError::NodeNotDispatched);
            },
            AppRecipeNodePhase::DispatchIntent => {
                let output_path = self.node_output_path(&run.locator, node_id);
                if self.workspace.exists_path(&output_path).await? {
                    let retained = self.read_output(&output_path).await?;
                    if retained.persisted_observed_at_ms.is_some() {
                        return Ok(AppRecipeNodeRecoveryAction::AdoptPersistedOutput);
                    }
                }
                if matches!(
                    binding.owner(),
                    super::recipe_lowering::AppRecipeExistingOwner::AppContextualRound
                        | super::recipe_lowering::AppRecipeExistingOwner::AppStoreTransaction
                ) {
                    // This owner resumes its sealed participant state and exact
                    // mutation receipts. It never blindly repeats provider I/O.
                    return Ok(AppRecipeNodeRecoveryAction::Dispatch);
                }
                if binding.uncertainty() == AppRecipeUncertaintyContract::Impossible {
                    return Ok(AppRecipeNodeRecoveryAction::RetryIdenticalRead);
                }
                return Ok(AppRecipeNodeRecoveryAction::OutcomeUncertain);
            },
            AppRecipeNodePhase::Pending => {},
        }
        let progress = document
            .nodes
            .get_mut(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        progress.attempt = progress
            .attempt
            .checked_add(1)
            .ok_or(AppRecipeLifecycleError::AttemptLimit)?;
        progress.phase = AppRecipeNodePhase::DispatchIntent;
        document.refresh_digest()?;
        self.write_document(&path, document).await?;
        Ok(AppRecipeNodeRecoveryAction::Dispatch)
    }

    /// Persist typed output bytes before marking the node settled. The output
    /// document binds the exact plan/node/attempt and is independently
    /// adoptable if the process crashes before the lifecycle transition.
    pub(crate) async fn settle_node(
        &self,
        run: &AppRecipeAttachedRun,
        node_id: &AppName,
        output: AppValidatedWorkflowValue,
        claim_owner_id: &str,
        claim_epoch: u64,
        owner_receipt_digest: Option<AppDigest>,
    ) -> Result<AppRecipePersistedNodeOutput, AppRecipeLifecycleError> {
        let binding = run
            .plan
            .node(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        if output.schema_ref() != &binding.output().schema_ref
            || output.canonical_encoded_len() > binding.resources().max_output_bytes
            || claim_owner_id.is_empty()
            || claim_owner_id.len() > 160
            || claim_owner_id.chars().any(char::is_control)
            || claim_epoch == 0
            || owner_receipt_digest.is_some()
                != matches!(
                    binding.owner(),
                    super::recipe_lowering::AppRecipeExistingOwner::AppEntityReconciliation
                        | super::recipe_lowering::AppRecipeExistingOwner::AppContextualRound
                        | super::recipe_lowering::AppRecipeExistingOwner::AppStoreTransaction
                )
        {
            return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
        }
        let path = self.lifecycle_path(&run.locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_attached(run, &document)?;
        let progress = document
            .nodes
            .get(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        if !matches!(
            progress.phase,
            AppRecipeNodePhase::DispatchIntent | AppRecipeNodePhase::Settled
        ) || progress.attempt == 0
        {
            return Err(AppRecipeLifecycleError::NodeNotDispatched);
        }
        let mut output_document = AppRecipeNodeOutputDocument {
            schema: NODE_OUTPUT_SCHEMA.to_owned(),
            plan_digest: run.plan.plan_digest().clone(),
            node_id: node_id.clone(),
            binding_digest: binding.binding_digest().clone(),
            attempt: progress.attempt,
            claim_owner_id: claim_owner_id.to_owned(),
            claim_epoch,
            value_schema_ref: output.schema_ref().clone(),
            value_digest: output.value_digest().clone(),
            value_encoded_len: output.canonical_encoded_len(),
            settled_at_ms: Utc::now().timestamp_millis(),
            persisted_observed_at_ms: None,
            value: output.into_source(),
            owner_receipt_digest,
            document_digest: AppDigest::blake3(b"pending-recipe-node-output"),
        };
        output_document.refresh_digest()?;
        let output_path = self.node_output_path(&run.locator, node_id);
        if let Some(parent) = output_path.parent() {
            self.workspace.create_dir_all_path(parent).await?;
        }
        let mut write_pending = true;
        if self.workspace.exists_path(&output_path).await? {
            let existing = self.read_output(&output_path).await?;
            if existing.plan_digest != output_document.plan_digest
                || existing.node_id != output_document.node_id
                || existing.binding_digest != output_document.binding_digest
                || existing.attempt != output_document.attempt
                || existing.owner_receipt_digest != output_document.owner_receipt_digest
            {
                return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
            }
            let same_claim = existing.claim_owner_id == output_document.claim_owner_id
                && existing.claim_epoch == output_document.claim_epoch;
            let same_value = existing.value_schema_ref == output_document.value_schema_ref
                && existing.value_digest == output_document.value_digest
                && existing.value_encoded_len == output_document.value_encoded_len
                && existing.value == output_document.value;
            if existing.persisted_observed_at_ms.is_some() {
                if same_claim && same_value {
                    output_document = existing;
                    write_pending = false;
                } else if existing.claim_epoch >= claim_epoch {
                    return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
                }
            } else if existing.claim_epoch > claim_epoch
                || (existing.claim_epoch == claim_epoch && (!same_claim || !same_value))
            {
                return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
            }
        }
        if write_pending {
            self.workspace
                .write_json_value_atomic_stream_path(
                    &output_path,
                    output_document,
                    MAX_NODE_OUTPUT_BYTES,
                )
                .await?;
            output_document = self.read_output(&output_path).await?;
        }
        if output_document.persisted_observed_at_ms.is_none() {
            // This second sealed write is the load-bearing persistence
            // witness. A crash after the first write leaves an explicitly
            // non-adoptable document; only bytes observed after the atomic
            // write returned can be adopted by a later claim epoch.
            output_document.persisted_observed_at_ms = Some(Utc::now().timestamp_millis());
            output_document.refresh_digest()?;
            self.workspace
                .write_json_value_atomic_stream_path(
                    &output_path,
                    output_document,
                    MAX_NODE_OUTPUT_BYTES,
                )
                .await?;
        }
        let retained = self.read_output(&output_path).await?;
        let progress = document
            .nodes
            .get_mut(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        progress.phase = AppRecipeNodePhase::Settled;
        progress.output_digest = Some(retained.value_digest.clone());
        progress.output_encoded_len = Some(retained.value_encoded_len);
        progress.owner_receipt_digest = retained.owner_receipt_digest.clone();
        let persisted_observed_at_ms = retained
            .persisted_observed_at_ms
            .ok_or(AppRecipeLifecycleError::CorruptNodeOutput)?;
        let witness = AppRecipePersistedNodeOutput {
            claim_owner_id: retained.claim_owner_id.clone(),
            claim_epoch: retained.claim_epoch,
            persisted_observed_at_ms,
            value_digest: retained.value_digest.clone(),
            value_encoded_len: retained.value_encoded_len,
        };
        document.refresh_digest()?;
        self.write_document(&path, document).await?;
        Ok(witness)
    }

    /// Seal control-flow branches which the canonical V3 Switch step proved
    /// unreachable. This stores no execution truth or output; the Artifact
    /// schedule is written first and remains authoritative for `skipped`.
    pub(crate) async fn retain_skipped_nodes(
        &self,
        run: &AppRecipeAttachedRun,
        node_ids: &[AppName],
    ) -> Result<(), AppRecipeLifecycleError> {
        let path = self.lifecycle_path(&run.locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_attached(run, &document)?;
        for node_id in node_ids {
            let progress = document
                .nodes
                .get_mut(node_id)
                .ok_or(AppRecipeLifecycleError::UnknownNode)?;
            match progress.phase {
                AppRecipeNodePhase::Pending | AppRecipeNodePhase::Skipped => {
                    progress.phase = AppRecipeNodePhase::Skipped;
                },
                _ => return Err(AppRecipeLifecycleError::NodeNotDispatched),
            }
        }
        document.refresh_digest()?;
        self.write_document(&path, document).await
    }

    /// Finish the crash window where output bytes exist but lifecycle still
    /// says dispatch-intent. Exact typed validation happens before adoption.
    pub(crate) async fn adopt_node_output(
        &self,
        run: &AppRecipeAttachedRun,
        node_id: &AppName,
        output_schema: &AppCompiledWorkflowValueSchema,
    ) -> Result<AppValidatedWorkflowValue, AppRecipeLifecycleError> {
        let binding = run
            .plan
            .node(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        if output_schema.schema_ref() != &binding.output().schema_ref {
            return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
        }
        let path = self.lifecycle_path(&run.locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_attached(run, &document)?;
        let output_path = self.node_output_path(&run.locator, node_id);
        let retained = self.read_output(&output_path).await?;
        let progress = document
            .nodes
            .get(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        if retained.plan_digest != *run.plan.plan_digest()
            || retained.node_id != *node_id
            || retained.binding_digest != *binding.binding_digest()
            || retained.attempt != progress.attempt
            || retained.persisted_observed_at_ms.is_none()
            || retained.owner_receipt_digest.is_some()
                != matches!(
                    binding.owner(),
                    super::recipe_lowering::AppRecipeExistingOwner::AppEntityReconciliation
                        | super::recipe_lowering::AppRecipeExistingOwner::AppContextualRound
                        | super::recipe_lowering::AppRecipeExistingOwner::AppStoreTransaction
                )
        {
            return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
        }
        let retained_value_digest = retained.value_digest.clone();
        let retained_value_encoded_len = retained.value_encoded_len;
        let retained_owner_receipt_digest = retained.owner_receipt_digest.clone();
        let output = validate_workflow_value(output_schema, retained.value)?;
        if output.value_digest() != &retained_value_digest
            || output.canonical_encoded_len() != retained_value_encoded_len
        {
            return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
        }
        let progress = document
            .nodes
            .get_mut(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        match progress.phase {
            AppRecipeNodePhase::DispatchIntent | AppRecipeNodePhase::Settled => {},
            _ => return Err(AppRecipeLifecycleError::NodeNotDispatched),
        }
        progress.phase = AppRecipeNodePhase::Settled;
        progress.output_digest = Some(retained_value_digest);
        progress.output_encoded_len = Some(retained_value_encoded_len);
        progress.owner_receipt_digest = retained_owner_receipt_digest;
        document.refresh_digest()?;
        self.write_document(&path, document).await?;
        Ok(output)
    }

    /// Read and validate exact output evidence without treating it as
    /// scheduler truth. The timestamp is part of the sealed output identity;
    /// Artifact V3 may adopt it only after the prior worker lease expires and
    /// only when it proves the bytes landed within the reviewed deadline.
    pub(crate) async fn adopt_node_output_if_present(
        &self,
        run: &AppRecipeAttachedRun,
        node_id: &AppName,
        output_schema: &AppCompiledWorkflowValueSchema,
    ) -> Result<
        Option<(AppValidatedWorkflowValue, AppRecipePersistedNodeOutput)>,
        AppRecipeLifecycleError,
    > {
        let output_path = self.node_output_path(&run.locator, node_id);
        if !self.workspace.exists_path(&output_path).await? {
            return Ok(None);
        }
        let binding = run
            .plan
            .node(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        if output_schema.schema_ref() != &binding.output().schema_ref {
            return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
        }
        let path = self.lifecycle_path(&run.locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_attached(run, &document)?;
        let retained = self.read_output(&output_path).await?;
        let Some(persisted_observed_at_ms) = retained.persisted_observed_at_ms else {
            return Ok(None);
        };
        let progress = document
            .nodes
            .get(node_id)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        if retained.plan_digest != *run.plan.plan_digest()
            || retained.node_id != *node_id
            || retained.binding_digest != *binding.binding_digest()
            || retained.attempt != progress.attempt
            || retained.owner_receipt_digest.is_some()
                != matches!(
                    binding.owner(),
                    super::recipe_lowering::AppRecipeExistingOwner::AppEntityReconciliation
                        | super::recipe_lowering::AppRecipeExistingOwner::AppContextualRound
                        | super::recipe_lowering::AppRecipeExistingOwner::AppStoreTransaction
                )
        {
            return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
        }
        let claim_owner_id = retained.claim_owner_id.clone();
        let claim_epoch = retained.claim_epoch;
        let retained_value_digest = retained.value_digest.clone();
        let retained_value_encoded_len = retained.value_encoded_len;
        let output = validate_workflow_value(output_schema, retained.value)?;
        if output.value_digest() != &retained_value_digest
            || output.canonical_encoded_len() != retained_value_encoded_len
        {
            return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
        }
        Ok(Some((
            output,
            AppRecipePersistedNodeOutput {
                claim_owner_id,
                claim_epoch,
                persisted_observed_at_ms,
                value_digest: retained_value_digest,
                value_encoded_len: retained_value_encoded_len,
            },
        )))
    }

    /// Persist one entity-store-minted locator outside the typed/public value
    /// sidecar. Only a lowered Query owner may write this evidence, and an
    /// existing locator is immutable under retries or crash recovery.
    pub(crate) async fn retain_record_locator(
        &self,
        run: &AppRecipeAttachedRun,
        node_id: &AppName,
        locator: AppRecipeRecordLocator,
    ) -> Result<(), AppRecipeLifecycleError> {
        locator.validate_integrity()?;
        let binding = run
            .plan
            .node(node_id)
            .filter(|binding| binding.owner() == &AppRecipeExistingOwner::AppEntityQuery)
            .ok_or(AppRecipeLifecycleError::UnknownNode)?;
        let path = self.record_locator_path(&run.locator, node_id);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let lifecycle = self
            .read_document(&self.lifecycle_path(&run.locator))
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_attached(run, &lifecycle)?;
        let mut document = AppRecipeRecordLocatorDocument {
            schema: "magician.app-recipe-record-locator-evidence.v1".to_owned(),
            plan_digest: run.plan.plan_digest().clone(),
            node_id: node_id.clone(),
            binding_digest: binding.binding_digest().clone(),
            locator,
            document_digest: AppDigest::blake3(b"pending-recipe-record-locator-evidence"),
        };
        document.refresh_digest()?;
        if self.workspace.exists_path(&path).await? {
            let existing = self.read_record_locator(&path).await?;
            if existing != document {
                return Err(AppRecipeLifecycleError::RecordLocatorConflict);
            }
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            self.workspace.create_dir_all_path(parent).await?;
        }
        self.workspace
            .write_json_value_atomic_stream_path(&path, document, MAX_NODE_OUTPUT_BYTES)
            .await?;
        Ok(())
    }

    /// Resolve an opaque projection reference only through exact, sealed
    /// Query evidence from this run. The scan is bounded by the lowered node
    /// ceiling; duplicate locators fail closed instead of choosing one.
    pub(crate) async fn resolve_record_locator(
        &self,
        run: &AppRecipeAttachedRun,
        logical_ref: &AppReference,
    ) -> Result<AppRecipeRecordLocator, AppRecipeLifecycleError> {
        let lifecycle = self
            .read_document(&self.lifecycle_path(&run.locator))
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_attached(run, &lifecycle)?;
        let mut found = None;
        for node_id in run.plan.execution_order() {
            let Some(binding) = run.plan.node(node_id) else {
                return Err(AppRecipeLifecycleError::UnknownNode);
            };
            if binding.owner() != &AppRecipeExistingOwner::AppEntityQuery {
                continue;
            }
            let path = self.record_locator_path(&run.locator, node_id);
            if !self.workspace.exists_path(&path).await? {
                continue;
            }
            let document = self.read_record_locator(&path).await?;
            if document.plan_digest != *run.plan.plan_digest()
                || document.node_id != *node_id
                || document.binding_digest != *binding.binding_digest()
            {
                return Err(AppRecipeLifecycleError::CorruptRecordLocator);
            }
            if document.locator.logical_ref() == logical_ref {
                if found.is_some() {
                    return Err(AppRecipeLifecycleError::RecordLocatorConflict);
                }
                found = Some(document.locator);
            }
        }
        found.ok_or(AppRecipeLifecycleError::MissingRecordLocator)
    }

    /// Prepare the typed root result for the canonical Artifact completion
    /// reducer. This does not mark Artifact execution terminal; the caller must
    /// persist the V3 result/receipt first and only then consume this proof.
    pub(crate) async fn prepare_completion(
        &self,
        run: &AppRecipeAttachedRun,
        output_schema: &AppCompiledWorkflowValueSchema,
    ) -> Result<AppRecipePreparedCompletion, AppRecipeLifecycleError> {
        if output_schema.schema_ref() != &run.plan.output_contract().schema_ref {
            return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
        }
        let path = self.lifecycle_path(&run.locator);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppRecipeLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppRecipeLifecycleError::CorruptLifecycle)?;
        validate_attached(run, &document)?;
        if document.cancellation.is_some()
            || document.nodes.values().any(|progress| {
                !matches!(
                    progress.phase,
                    AppRecipeNodePhase::Settled | AppRecipeNodePhase::Skipped
                )
            })
        {
            return Err(AppRecipeLifecycleError::RunNotSettled);
        }
        let retained = self
            .read_output(&self.node_output_path(&run.locator, run.plan.root()))
            .await?;
        let value = validate_workflow_value(output_schema, retained.value)?;
        if value.value_digest() != &retained.value_digest
            || value.canonical_encoded_len() != retained.value_encoded_len
        {
            return Err(AppRecipeLifecycleError::NodeOutputSubstitution);
        }
        document.phase = AppRecipeLaunchPhase::CompletionPrepared;
        document.refresh_digest()?;
        self.write_document(&path, document).await?;
        Ok(AppRecipePreparedCompletion {
            value,
            recipe_ref: run.plan.recipe_ref().clone(),
            topology_digest: run.plan.topology_digest().clone(),
            plan_digest: run.plan.plan_digest().clone(),
        })
    }

    fn lifecycle_path(&self, locator: &AppRecipeRunLocator) -> PathBuf {
        self.workspace
            .task_dir(
                &locator.scope.principal(),
                &locator.scope.workspace(),
                &locator.task_id,
            )
            .join("app_recipe_runs")
            .join(&locator.run_segment)
            .join(LIFECYCLE_FILE)
    }

    fn binding_path(&self, locator: &AppRecipeRunLocator) -> PathBuf {
        self.workspace
            .task_dir(
                &locator.scope.principal(),
                &locator.scope.workspace(),
                &locator.task_id,
            )
            .join("app_recipe_runs")
            .join(&locator.run_segment)
            .join(BINDING_FILE)
    }

    fn node_output_path(&self, locator: &AppRecipeRunLocator, node_id: &AppName) -> PathBuf {
        self.workspace
            .task_dir(
                &locator.scope.principal(),
                &locator.scope.workspace(),
                &locator.task_id,
            )
            .join("app_recipe_runs")
            .join(&locator.run_segment)
            .join("nodes")
            .join(node_id.as_str())
            .join(NODE_OUTPUT_FILE)
    }

    fn record_locator_path(&self, locator: &AppRecipeRunLocator, node_id: &AppName) -> PathBuf {
        self.workspace
            .task_dir(
                &locator.scope.principal(),
                &locator.scope.workspace(),
                &locator.task_id,
            )
            .join("app_recipe_runs")
            .join(&locator.run_segment)
            .join("nodes")
            .join(node_id.as_str())
            .join(RECORD_LOCATOR_FILE)
    }

    async fn validate_execution(
        &self,
        locator: &AppRecipeRunLocator,
        execution_id: &str,
    ) -> Result<(), AppRecipeLifecycleError> {
        let path = self.workspace.execution_state_path(
            &locator.scope.principal(),
            &locator.scope.workspace(),
            &locator.task_id,
            execution_id,
        );
        let state = self
            .workspace
            .read_json_bounded_stream_path::<ExecutionState, _>(
                path,
                MAX_EXECUTION_STATE_BYTES,
                MAX_EXECUTION_STATE_DEPTH,
                MAX_EXECUTION_STATE_NODES,
            )
            .await?;
        if state.execution_id != execution_id
            || state.task_id != locator.task_id
            || state.root_execution_id.as_deref() != Some(execution_id)
            || state.parent_execution_id.is_some()
            || state.relationship_type != "root"
            || !matches!(
                state.status.as_str(),
                "running" | "completed" | "failed" | "cancelled" | "canceled"
            )
        {
            return Err(AppRecipeLifecycleError::ExecutionSubstitution);
        }
        Ok(())
    }

    async fn read_document(
        &self,
        path: &Path,
    ) -> Result<Option<AppRecipeLifecycleDocument>, AppRecipeLifecycleError> {
        if self.workspace.metadata_path(path).await?.is_none() {
            return Ok(None);
        }
        let document = self
            .workspace
            .read_json_bounded_stream_path::<AppRecipeLifecycleDocument, _>(
                path,
                u64::try_from(MAX_LIFECYCLE_BYTES).unwrap_or(u64::MAX),
                MAX_LIFECYCLE_DEPTH,
                MAX_LIFECYCLE_NODES,
            )
            .await?;
        document.validate_integrity()?;
        Ok(Some(document))
    }

    async fn write_document(
        &self,
        path: &Path,
        document: AppRecipeLifecycleDocument,
    ) -> Result<(), AppRecipeLifecycleError> {
        document.validate_integrity()?;
        self.workspace
            .write_json_value_atomic_stream_path(path, document, MAX_LIFECYCLE_BYTES)
            .await?;
        Ok(())
    }

    async fn read_binding(
        &self,
        path: &Path,
    ) -> Result<AppRecipeRunBindingDocument, AppRecipeLifecycleError> {
        let binding = self
            .workspace
            .read_json_bounded_stream_path::<AppRecipeRunBindingDocument, _>(
                path,
                u64::try_from(MAX_LIFECYCLE_BYTES).unwrap_or(u64::MAX),
                MAX_LIFECYCLE_DEPTH,
                MAX_LIFECYCLE_NODES,
            )
            .await?;
        binding.validate_integrity()?;
        Ok(binding)
    }

    async fn write_binding(
        &self,
        path: &Path,
        binding: AppRecipeRunBindingDocument,
    ) -> Result<(), AppRecipeLifecycleError> {
        binding.validate_integrity()?;
        self.workspace
            .write_json_value_atomic_stream_path(path, binding, MAX_LIFECYCLE_BYTES)
            .await?;
        Ok(())
    }

    async fn read_output(
        &self,
        path: &Path,
    ) -> Result<AppRecipeNodeOutputDocument, AppRecipeLifecycleError> {
        let document = self
            .workspace
            .read_json_bounded_stream_path::<AppRecipeNodeOutputDocument, _>(
                path,
                u64::try_from(MAX_NODE_OUTPUT_BYTES).unwrap_or(u64::MAX),
                MAX_NODE_OUTPUT_DEPTH,
                MAX_NODE_OUTPUT_NODES,
            )
            .await?;
        document.validate_integrity()?;
        Ok(document)
    }

    async fn read_record_locator(
        &self,
        path: &Path,
    ) -> Result<AppRecipeRecordLocatorDocument, AppRecipeLifecycleError> {
        let document = self
            .workspace
            .read_json_bounded_stream_path::<AppRecipeRecordLocatorDocument, _>(
                path,
                u64::try_from(MAX_NODE_OUTPUT_BYTES).unwrap_or(u64::MAX),
                MAX_NODE_OUTPUT_DEPTH,
                MAX_NODE_OUTPUT_NODES,
            )
            .await?;
        document.validate_integrity()?;
        Ok(document)
    }
}

fn validate_locator(
    locator: &AppRecipeRunLocator,
    document: &AppRecipeLifecycleDocument,
) -> Result<(), AppRecipeLifecycleError> {
    if locator.run_ref != document.run_ref
        || locator.run_digest != document.run_digest
        || locator.run_segment != digest_segment(&document.run_digest)?
    {
        return Err(AppRecipeLifecycleError::RunSubstitution);
    }
    Ok(())
}

fn validate_binding_pair(
    lifecycle: &AppRecipeLifecycleDocument,
    binding: &AppRecipeRunBindingDocument,
) -> Result<(), AppRecipeLifecycleError> {
    if lifecycle.run_ref != binding.run_ref
        || lifecycle.run_digest != binding.run_digest
        || lifecycle.plan_ref != binding.plan_ref
        || lifecycle.plan_digest != binding.plan_digest
        || lifecycle.plan_encoded_len != binding.plan_encoded_len
        || lifecycle.input_schema_ref != binding.input_schema_ref
        || lifecycle.input_digest != binding.input_digest
        || lifecycle.input_encoded_len != binding.input_encoded_len
    {
        return Err(AppRecipeLifecycleError::CorruptLifecycle);
    }
    let plan = binding.validate_integrity()?;
    if plan.execution_order().len() != lifecycle.nodes.len()
        || plan.execution_order().iter().any(|node_id| {
            let Some(progress) = lifecycle.nodes.get(node_id) else {
                return true;
            };
            match plan.node(node_id) {
                Some(node) => node.binding_digest() != &progress.binding_digest,
                None => true,
            }
        })
    {
        return Err(AppRecipeLifecycleError::CorruptLifecycle);
    }
    Ok(())
}

fn validate_attached(
    run: &AppRecipeAttachedRun,
    document: &AppRecipeLifecycleDocument,
) -> Result<(), AppRecipeLifecycleError> {
    validate_locator(&run.locator, document)?;
    if document.execution_id.as_deref() != Some(run.execution_id.as_str())
        || document.plan_digest != *run.plan.plan_digest()
        || document.input_digest != *run.input.value_digest()
        || !matches!(
            document.phase,
            AppRecipeLaunchPhase::Attached | AppRecipeLaunchPhase::CompletionPrepared
        )
    {
        return Err(AppRecipeLifecycleError::ExecutionSubstitution);
    }
    Ok(())
}

fn run_identity(
    run_key_digest: &AppDigest,
    plan_digest: &AppDigest,
    input_digest: &AppDigest,
) -> Result<(AppReference, AppDigest), AppRecipeLifecycleError> {
    let run_digest = stream_identity(
        "recipe run identity",
        &serde_json::json!({
            "schema": "magician.app-recipe-run.v1",
            "run_key_digest": run_key_digest,
            "plan_digest": plan_digest,
            "input_digest": input_digest,
        }),
        16 * 1024,
    )?
    .0;
    let run_ref = AppReference::parse(format!("recipe-run:{}", run_digest.as_str()))?;
    Ok((run_ref, run_digest))
}

fn digest_segment(digest: &AppDigest) -> Result<String, AppRecipeLifecycleError> {
    digest
        .as_str()
        .strip_prefix("blake3:")
        .map(ToOwned::to_owned)
        .ok_or(AppRecipeLifecycleError::CorruptLifecycle)
}

fn validate_runtime_segment(value: &str) -> Result<(), AppRecipeLifecycleError> {
    if value.is_empty()
        || value.len() > 192
        || value.trim() != value
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains(':')
        || value.chars().any(char::is_control)
    {
        return Err(AppRecipeLifecycleError::ExecutionSubstitution);
    }
    Ok(())
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
) -> Result<(AppDigest, usize), AppRecipeLifecycleError> {
    let mut sink = BoundedDigestWriter::new(limit);
    if let Err(error) = serde_json::to_writer(&mut sink, value) {
        if sink.exceeded {
            return Err(AppRecipeLifecycleError::CanonicalByteLimit { domain, limit });
        }
        return Err(AppRecipeLifecycleError::Encoding(error.to_string()));
    }
    let digest = AppDigest::parse(format!("blake3:{}", sink.hasher.finalize().to_hex()))?;
    Ok((digest, sink.bytes))
}

#[derive(Debug, Error)]
pub(crate) enum AppRecipeLifecycleError {
    #[error(transparent)]
    Artifact(#[from] ArtifactV2Error),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Recipe(#[from] AppRecipeIrError),
    #[error(transparent)]
    Lowering(#[from] AppRecipeLoweringError),
    #[error(transparent)]
    EntityStore(#[from] AppEntityStoreError),
    #[error("recipe lifecycle lock failed: {0}")]
    Lock(String),
    #[error("recipe lifecycle document is corrupt")]
    CorruptLifecycle,
    #[error("recipe run identity was substituted")]
    RunSubstitution,
    #[error("recipe input schema was substituted")]
    InputSchemaSubstitution,
    #[error("recipe input bytes exceed the root node ceiling")]
    InputByteLimit,
    #[error("recipe input identity was substituted")]
    InputSubstitution,
    #[error("recipe Artifact execution identity was substituted")]
    ExecutionSubstitution,
    #[error("recipe node is not present in the exact lowered plan")]
    UnknownNode,
    #[error("recipe node attempt ceiling was exceeded")]
    AttemptLimit,
    #[error("recipe node was not durably marked dispatched")]
    NodeNotDispatched,
    #[error("recipe node output was substituted")]
    NodeOutputSubstitution,
    #[error("recipe node output document is corrupt")]
    CorruptNodeOutput,
    #[error("recipe private record locator document is corrupt")]
    CorruptRecordLocator,
    #[error("recipe private record locator is missing")]
    MissingRecordLocator,
    #[error("recipe private record locator conflicts with retained evidence")]
    RecordLocatorConflict,
    #[error("recipe cancellation request is invalid")]
    InvalidCancellation,
    #[error("recipe cancellation request conflicts with the retained intent")]
    ConflictingCancellation,
    #[error("recipe run has not settled every exact node")]
    RunNotSettled,
    #[error("recipe task has too many retained pre-root reservations")]
    ReservationLimit,
    #[error("{domain} exceeds the {limit}-byte canonical ceiling")]
    CanonicalByteLimit { domain: &'static str, limit: usize },
    #[error("recipe lifecycle canonical encoding failed: {0}")]
    Encoding(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn persisted_output_witness_is_fenced_by_exact_claim_owner_and_epoch() {
        let witness = AppRecipePersistedNodeOutput {
            claim_owner_id: "recipe-worker-a".to_owned(),
            claim_epoch: 7,
            persisted_observed_at_ms: 1,
            value_digest: AppDigest::blake3(b"exact-value"),
            value_encoded_len: 32,
        };

        assert!(witness.matches_claim("recipe-worker-a", 7));
        assert!(!witness.matches_claim("recipe-worker-b", 7));
        assert!(!witness.matches_claim("recipe-worker-a", 8));
    }
}
