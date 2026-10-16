//! Metered invocation of existing governed tools inside the native Quality
//! harness. There is no provider, media format, planner or regeneration loop
//! here: the existing actor decides which admitted tool to call and why.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{ensure, Context, Result};
use chrono::Utc;
use magician_relentless::{
    budget::{OperationIntent, Settlement},
    contract::valid_hash,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    execution::{
        actions::ActionResult,
        capability::{CapabilityPackDefinition, CapabilityRegistry, ImplementationType},
        primitive_dispatch::PrimitiveExecCtx,
    },
    relentless_harness::HarnessResources,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolPricing {
    /// Successful provider receipt multiplied by an owner-reviewed fixed tariff.
    /// This is tariff-accounted usage, not a fetched account invoice.
    Fixed { cost_microusd: u64, version: String },
    /// Variable-price adapters must return integer microusd in their receipt.
    Reported {
        maximum_cost_microusd: u64,
        cost_pointer: String,
        version: String,
    },
}

impl ToolPricing {
    pub fn maximum(&self) -> u64 {
        match self {
            Self::Fixed { cost_microusd, .. } => *cost_microusd,
            Self::Reported {
                maximum_cost_microusd,
                ..
            } => *maximum_cost_microusd,
        }
    }
    fn cost(&self, receipt: &Value) -> Result<u64> {
        let cost = match self {
            Self::Fixed { cost_microusd, .. } => *cost_microusd,
            Self::Reported { cost_pointer, .. } => receipt
                .pointer(cost_pointer)
                .and_then(Value::as_u64)
                .context("tool receipt has no integer microusd charge")?,
        };
        ensure!(
            cost <= self.maximum(),
            "tool charge exceeds its admitted quote"
        );
        Ok(cost)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPathAccess {
    Read,
    Write,
}

/// Explicit resource contract for any installed governed CLI skill. Pricing
/// dimensions are fixed parameters; all other parameters need an explicit grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeteredToolPolicy {
    pub capability: String,
    pub action: String,
    pub maximum_calls: u32,
    pub pricing: ToolPricing,
    pub allowed_parameters: BTreeSet<String>,
    #[serde(default)]
    pub fixed_parameters: BTreeMap<String, Value>,
    #[serde(default)]
    pub path_parameters: BTreeMap<String, ToolPathAccess>,
    /// JSON pointer into the actual tool output identifying its provider receipt.
    pub receipt_pointer: String,
    /// Filled from the scoped catalog at owner preview, then revalidated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_hash: Option<String>,
}

impl MeteredToolPolicy {
    pub fn validate(&self) -> Result<()> {
        let name = |value: &str| {
            !value.is_empty()
                && value.len() <= 256
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        };
        ensure!(
            name(&self.capability)
                && name(&self.action)
                && !self.capability.contains("__")
                && self.maximum_calls > 0,
            "invalid metered tool identity or ceiling"
        );
        ensure!(
            self.capability_hash.as_deref().is_some_and(valid_hash),
            "metered tool has no frozen capability pin"
        );
        ensure!(
            self.pricing.maximum() > 0 && self.maximum_cost()?.is_some(),
            "invalid metered tool quote"
        );
        let version = match &self.pricing {
            ToolPricing::Fixed { version, .. } | ToolPricing::Reported { version, .. } => version,
        };
        ensure!(
            !version.trim().is_empty() && version.len() <= 2048,
            "tool pricing requires a version or tariff reference"
        );
        ensure!(
            self.receipt_pointer.starts_with('/') && self.receipt_pointer.len() <= 512,
            "invalid tool receipt pointer"
        );
        if let ToolPricing::Reported { cost_pointer, .. } = &self.pricing {
            ensure!(
                cost_pointer.starts_with('/') && cost_pointer.len() <= 512,
                "invalid tool cost pointer"
            );
        }
        ensure!(
            self.allowed_parameters
                .iter()
                .all(|key| name(key) && !key.starts_with("__"))
                && self
                    .fixed_parameters
                    .keys()
                    .chain(self.path_parameters.keys())
                    .all(|key| self.allowed_parameters.contains(key)),
            "invalid metered tool argument grants"
        );
        Ok(())
    }
    fn maximum_cost(&self) -> Result<Option<u64>> {
        Ok(self
            .pricing
            .maximum()
            .checked_mul(u64::from(self.maximum_calls)))
    }
    pub fn hash(&self) -> Result<String> {
        self.validate()?;
        hash(self)
    }
    fn arguments(&self, input: &Value, root: &Path) -> Result<Value> {
        let mut values = input
            .as_object()
            .context("metered tool arguments must be an object")?
            .clone();
        ensure!(
            values
                .keys()
                .all(|key| self.allowed_parameters.contains(key)),
            "tool argument was not admitted"
        );
        for (key, value) in &self.fixed_parameters {
            ensure!(
                values.get(key).is_none_or(|actual| actual == value),
                "tool changed an admitted pricing parameter"
            );
            values.insert(key.clone(), value.clone());
        }
        for key in self.path_parameters.keys() {
            if let Some(value) = values.get_mut(key) {
                let path = confined_path(
                    root,
                    value
                        .as_str()
                        .context("tool file parameter must be a path")?,
                )?;
                if matches!(self.path_parameters[key], ToolPathAccess::Read) {
                    ensure!(path.is_file(), "tool input is not a regular file");
                }
                *value = json!(path);
            }
        }
        Ok(Value::Object(values))
    }
    fn charge(&self, result: &ActionResult) -> Result<u64> {
        let output: Value = serde_json::from_str(
            result
                .as_text()
                .context("metered tool has no JSON receipt")?,
        )?;
        let receipt = output
            .pointer(&self.receipt_pointer)
            .context("metered tool omitted provider receipt")?;
        ensure!(
            receipt
                .as_str()
                .is_some_and(|id| !id.trim().is_empty() && id.len() <= 1024)
                || receipt.as_u64().is_some_and(|id| id > 0),
            "metered tool provider receipt is invalid"
        );
        self.pricing.cost(&output)
    }
}

fn hash(value: &impl Serialize) -> Result<String> {
    // JSON Value canonicalizes nested maps, including registry HashMaps.
    Ok(
        blake3::hash(&serde_json::to_vec(&serde_json::to_value(value)?)?)
            .to_hex()
            .to_string(),
    )
}

fn file_hash(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= 64 * 1024 * 1024,
        "metered tool pin file is too large or irregular"
    );
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file.take(64 * 1024 * 1024 + 1), &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Qualification is structural, never a provider-name allowlist. The existing
/// governed runtime still owns credentials, executable snapshots and OS jail.
pub fn capability_pin(pack: &CapabilityPackDefinition, action: &str) -> Result<String> {
    ensure!(
        pack.execution
            .as_ref()
            .is_none_or(|execution| execution.spend.is_none()),
        "metered tool cannot also charge another spend ledger"
    );
    let ImplementationType::Primitive {
        provider_name: None,
        runtime_package: Some(runtime),
        ..
    } = &pack.implementation
    else {
        anyhow::bail!("metered tool requires the governed typed CLI resource boundary");
    };
    ensure!(
        matches!(&runtime.package.contract.runtime,
        tool_runtime_core::manifest::RuntimeProtocol::Cli { working_directory, .. }
            if working_directory.mode == tool_runtime_core::manifest::WorkingDirectoryMode::Workspace),
        "metered tool must support an admitted working directory"
    );
    let action = runtime
        .cli_actions()
        .map_err(anyhow::Error::msg)?
        .actions
        .get(action)
        .context("metered tool action is unavailable")?;
    let directory = runtime
        .executable_directory
        .as_ref()
        .context("metered tool has no skill-owned executable")?;
    let executable = action.invocation.executable.as_str();
    ensure!(
        Path::new(executable).components().count() == 1
            && matches!(
                Path::new(executable).components().next(),
                Some(Component::Normal(_))
            ),
        "metered tool executable is not a declared skill program"
    );
    let source = directory
        .parent()
        .context("metered tool skill root missing")?
        .join("SKILL.md");
    hash(&json!({"version":"quality-metered-tool-v1", "pack":pack,
        "source":file_hash(&source)?, "executable":file_hash(&directory.join(executable))?}))
}

fn confined_path(root: &Path, raw: &str) -> Result<PathBuf> {
    let raw = Path::new(raw);
    let relative = if raw.is_absolute() {
        raw.strip_prefix(root)
            .context("tool file escaped admitted work area")?
    } else {
        raw
    };
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "invalid tool work-area path"
    );
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "tool path traverses a symlink"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolReceipt {
    parent: OperationIntent,
    operation: OperationIntent,
    policy_hash: String,
    result: ActionResult,
    #[serde(default)]
    output_files: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_error: Option<String>,
}

#[derive(Debug)]
pub struct MeteredTools {
    pub(super) root: PathBuf,
    resources: Arc<HarnessResources>,
    policies: Vec<MeteredToolPolicy>,
    maximum_output_bytes: u64,
    gate: tokio::sync::Mutex<()>,
}

impl MeteredTools {
    pub fn admits(&self, name: &str) -> bool {
        let (capability, _) = super::execution::flat_loop::parse_flat_tool_name(name);
        self.policies
            .iter()
            .any(|policy| policy.capability == capability)
    }
    pub fn admit(
        root: PathBuf,
        resources: Arc<HarnessResources>,
        policies: Vec<MeteredToolPolicy>,
        maximum_output_bytes: u64,
    ) -> Result<Arc<Self>> {
        let root = root.canonicalize()?;
        ensure!(root.is_dir(), "metered work area is not a directory");
        ensure!(
            maximum_output_bytes > 0 && maximum_output_bytes < u64::MAX,
            "invalid metered output byte ceiling"
        );
        for policy in &policies {
            policy.validate()?;
        }
        Ok(Arc::new(Self {
            root,
            resources,
            policies,
            maximum_output_bytes,
            gate: Default::default(),
        }))
    }

    pub async fn dispatch(
        self: &Arc<Self>,
        name: &str,
        arguments: &Value,
        registry: Arc<CapabilityRegistry>,
        mut context: PrimitiveExecCtx,
    ) -> Result<ActionResult> {
        ensure!(
            context.task_id.as_deref() == Some(self.resources.binding.task_id.as_str())
                && context.execution_id.as_deref()
                    == Some(self.resources.binding.execution_id.as_str()),
            "metered tool changed canonical task or execution identity"
        );
        let (capability, action) = super::execution::flat_loop::parse_flat_tool_name(name);
        let policy = self
            .policies
            .iter()
            .find(|policy| {
                policy.capability == capability && Some(policy.action.as_str()) == action
            })
            .context("tool action has no admitted metering contract")?
            .clone();
        let pack = registry
            .get_pack_definition(capability)
            .context("metered tool disappeared")?;
        ensure!(
            Some(capability_pin(&pack, &policy.action)?.as_str())
                == policy.capability_hash.as_deref(),
            "metered tool changed since admission"
        );
        let arguments = policy.arguments(arguments, &self.root)?;
        let mut dispatch_arguments = arguments.clone();
        let mut workspace_paths = Vec::new();
        if let ImplementationType::Primitive {
            runtime_package: Some(runtime),
            ..
        } = &pack.implementation
        {
            ensure!(
                context.secret_store.is_some(),
                "metered governed tool audit authority unavailable before dispatch"
            );
            let compiled = runtime
                .cli_actions()
                .map_err(anyhow::Error::msg)?
                .actions
                .get(&policy.action)
                .context("tool action missing")?;
            // The ledger binds confined absolute paths, while the governed
            // workspace-path authority accepts relative paths and binds them
            // itself. String-valued CLI paths retain their absolute spelling.
            for (name, parameter) in &compiled.parameters {
                let tool_runtime_core::action_overrides::TypedActionParameter::WorkspacePath {
                    access,
                    ..
                } = parameter
                else {
                    continue;
                };
                let Some(value) = dispatch_arguments.get_mut(name) else {
                    continue;
                };
                ensure!(
                    matches!(
                        (policy.path_parameters.get(name), access),
                        (
                            Some(ToolPathAccess::Read),
                            tool_runtime_core::action_overrides::WorkspacePathAccess::ReadFile
                        ) | (
                            Some(ToolPathAccess::Write),
                            tool_runtime_core::action_overrides::WorkspacePathAccess::CreateFile
                        )
                    ),
                    "workspace path requires a matching admitted access grant"
                );
                let path = confined_path(
                    &self.root,
                    value.as_str().context("tool workspace path must be text")?,
                )?;
                let relative = path
                    .strip_prefix(&self.root)?
                    .to_string_lossy()
                    .into_owned();
                *value = json!(relative);
                workspace_paths.push((relative, *access));
            }
            tool_runtime_core::action_overrides::lower_typed_action_invocation(
                compiled,
                &dispatch_arguments,
            )
            .map_err(|error| {
                anyhow::anyhow!("metered tool input rejected before dispatch: {error}")
            })?;
        }
        let effect = context
            .effect_id
            .as_deref()
            .context("metered tool requires a runtime invocation identity")?;
        ensure!(
            !effect.is_empty() && effect.len() <= 4096,
            "invalid metered invocation identity"
        );
        let policy_hash = policy.hash()?;
        let bucket = format!("tool:{policy_hash}:");
        let child = OperationIntent {
            id: format!("{bucket}{}", blake3::hash(effect.as_bytes()).to_hex()),
            input_hash: hash(&json!({"policy":policy_hash,"arguments":arguments,"effect":effect}))?,
            class: self.resources.parent.class,
            maximum_cost_microusd: policy.pricing.maximum(),
        };
        context.governed_work_root = Some(self.root.clone());
        let cancellation = self.resources.cancel.child_token();
        context.cancellation_token = Some(cancellation.clone());
        let _cancel_waiter = cancellation.clone().drop_guard();
        let boundary = self.clone();
        let name = name.to_owned();
        let active = self.resources.enter_tool()?;
        super::execution::runtime_boundary::spawn_execution_job(move || async move {
            let _active = active;
            let _serial = tokio::select! {
                guard = boundary.gate.lock() => guard,
                _ = cancellation.cancelled() => anyhow::bail!("metered dispatch cancelled before admission"),
            };
            let resources = boundary.resources.clone();
            let owner = resources.clone();
            let intent = child.clone();
            let namespace = bucket.clone();
            let replay_policy_hash = policy_hash.clone();
            let principal = context.principal.clone();
            let workspace = context.workspace.clone();
            let replay_root = boundary.root.clone();
            let prepared = tokio::task::spawn_blocking(move || -> Result<(Option<ActionResult>, chrono::DateTime<Utc>)> {
                ensure!(!owner.cancel.is_cancelled() && !cancellation.is_cancelled(), "metered dispatch cancelled");
                let state = owner.store.load(owner.run_id)?;
                ensure!(principal.as_deref() == Some(state.scope.principal.as_str())
                    && workspace.as_deref() == Some(state.scope.workspace.as_str()),
                    "metered tool changed canonical owner scope");
                let budget = owner.store.adapter_budget(owner.run_id, &owner.parent)?;
                if let Some(existing) = budget.ledger.operations().get(&intent.id) {
                    ensure!(existing.intent == intent, "metered replay input changed");
                    let Settlement::Settled { receipt_hash, .. } = &existing.settlement else { anyhow::bail!("paid tool outcome requires reconciliation; dispatch will not repeat"); };
                    let receipt: ToolReceipt = serde_json::from_slice(&owner.store.read_blob(owner.run_id, receipt_hash)?)?;
                    ensure!(receipt.parent == owner.parent && receipt.operation == intent && receipt.policy_hash == replay_policy_hash, "metered replay receipt binding changed");
                    ensure!(receipt.output_error.is_none(), "sealed tool result has an invalid output; paid call will not repeat");
                    for (name, hash) in &receipt.output_files {
                        let path = confined_path(&replay_root, name)?;
                        let bytes = owner.store.read_blob(owner.run_id, hash)?;
                        match std::fs::File::open(&path) {
                            Ok(file) => {
                                ensure!(file.metadata()?.is_file() && file.metadata()?.len() == bytes.len() as u64, "sealed tool output changed; paid call will not repeat");
                                let mut current = Vec::new();
                                file.take(bytes.len() as u64 + 1).read_to_end(&mut current)?;
                                ensure!(blake3::hash(&current).to_hex().as_str() == hash, "sealed tool output changed; paid call will not repeat");
                            },
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => { std::fs::write(path, bytes)?; },
                            Err(error) => return Err(error.into()),
                        }
                    }
                    return Ok((Some(receipt.result), Utc::now()));
                }
                ensure!(!budget.ledger.operations().values().any(|operation| operation.intent.id.starts_with("tool:") && matches!(operation.settlement, Settlement::Reserved | Settlement::OutcomeUnknown)), "paid tool outcome requires reconciliation before regeneration");
                // Check known path failures only for fresh dispatch: a settled
                // replay may already have its sealed create-file output.
                for (relative, access) in &workspace_paths {
                    super::execution::primitive_dispatch::bind_scoped_workspace_path(
                        &replay_root, relative, *access,
                    ).map_err(anyhow::Error::msg)?;
                }
                owner.store.reserve_adapter_child_bounded(owner.run_id, &owner.worker_id, &owner.parent, intent.clone(), Some((&namespace, policy.maximum_calls)), Utc::now())?;
                owner.store.start_adapter_child(owner.run_id, &owner.worker_id, &owner.parent, &intent, Utc::now())?;
                Ok((None, owner.store.load(owner.run_id)?.deadline_at))
            }).await??;
            if let Some(result) = prepared.0 { return Ok(result); }
            let remaining = (prepared.1 - Utc::now()).to_std().unwrap_or_default();
            let result = tokio::time::timeout(remaining, Box::pin(super::execution::flat_loop::dispatch_flat_action(&name, &dispatch_arguments, &registry, &context, context.cancellation_token.clone()))).await;
            let result = match result { Ok(result) => result.map_err(anyhow::Error::from), Err(_) => Err(anyhow::anyhow!("metered tool exceeded the run deadline")) };
            let charge = match &result {
                Ok(result) => policy.charge(result),
                Err(error) => Err(anyhow::anyhow!("{error:#}")),
            };
            let cost = charge.as_ref().ok().copied();
            let receipt = result.as_ref().ok().map(|result| ToolReceipt { parent: resources.parent.clone(), operation: child.clone(), policy_hash: policy_hash.clone(), result: result.clone(), output_files: BTreeMap::new(), output_error: None });
            let owner = resources.clone();
            let root = boundary.root.clone();
            let maximum_output_bytes = boundary.maximum_output_bytes;
            let output_error = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
                let mut receipt = receipt;
                if let Some(receipt) = &mut receipt {
                    let output = (|| -> Result<()> {
                        let mut remaining = maximum_output_bytes;
                        for (key, access) in &policy.path_parameters {
                            if !matches!(access, ToolPathAccess::Write) { continue; }
                            let Some(raw) = arguments.get(key).and_then(Value::as_str) else { continue; };
                            let path = confined_path(&root, raw)?;
                            let file = std::fs::File::open(&path)?;
                            ensure!(file.metadata()?.is_file() && file.metadata()?.len() <= remaining, "metered output exceeds the admitted byte ceiling");
                            let mut bytes = Vec::new();
                            file.take(remaining + 1).read_to_end(&mut bytes)?;
                            remaining = remaining.checked_sub(bytes.len() as u64).context("metered output grew past its byte ceiling")?;
                            let hash = owner.store.put_blob(owner.run_id, &bytes)?;
                            receipt.output_files.insert(path.strip_prefix(&root)?.to_string_lossy().into_owned(), hash);
                        }
                        Ok(())
                    })();
                    receipt.output_error = output.err().map(|error| format!("{error:#}"));
                }
                let output_error = receipt.as_ref().and_then(|receipt| receipt.output_error.clone());
                let bytes = serde_json::to_vec(&receipt)?;
                let receipt_hash = owner.store.put_blob(owner.run_id, &bytes)?;
                owner.store.settle_adapter_child(owner.run_id, &owner.parent, &child, cost, receipt_hash)?;
                Ok(output_error)
            }).await??;
            ensure!(cost.is_some(), "paid tool outcome or charge is unknown; reservation retained for reconciliation: {}", charge.unwrap_err());
            ensure!(output_error.is_none(), "paid tool output requires repair: {}", output_error.unwrap_or_default());
            result
        }).await.context("metered tool owner stopped")?
    }
}

pub fn totals(policies: &[MeteredToolPolicy]) -> Result<(u32, u64)> {
    let mut identities = BTreeSet::new();
    let mut count = 0_u32;
    let mut cost = 0_u64;
    for policy in policies {
        policy.validate()?;
        ensure!(
            identities.insert((&policy.capability, &policy.action)),
            "duplicate metered tool contract"
        );
        count = count
            .checked_add(policy.maximum_calls)
            .context("tool count overflow")?;
        cost = cost
            .checked_add(policy.maximum_cost()?.context("tool cost overflow")?)
            .context("tool quote overflow")?;
    }
    Ok((count, cost))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::relentless_harness::tests::Fixture;
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

    const SOURCE: &str = r#"---
name: asset-tool
description: Generic governed asset fixture
metadata:
  magician:
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [render-asset]
        entrypoint: render-asset
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory: {mode: workspace}
        limits: {timeout_secs: 10, stdin_bytes: 4096, stdout_bytes: 4096, stderr_bytes: 4096}
      auth: {kind: none, requirement: none}
      policy_floor: {approval: ordinary, resource_scopes: [workspace]}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Produce the declared asset
          parameters:
            prompt: {type: string, description: Asset request, required: true, max_length: 256}
            model: {type: string, description: Frozen generator, required: true, max_length: 256}
            output_path: {type: string, description: Declared output file, required: true, max_length: 4096}
          timeout_secs: 10
    runtime_catalog:
      categories: [generation]
      timeout_default_secs: 10
---
Generic fixture, without a provider or media-format-specific adapter.
"#;

    fn policy() -> MeteredToolPolicy {
        MeteredToolPolicy {
            capability: "asset-tool".into(),
            action: "run".into(),
            maximum_calls: 2,
            pricing: ToolPricing::Reported {
                maximum_cost_microusd: 40,
                cost_pointer: "/charged_microusd".into(),
                version: "fixture-tariff-v1".into(),
            },
            allowed_parameters: BTreeSet::from([
                "prompt".into(),
                "model".into(),
                "output_path".into(),
            ]),
            fixed_parameters: BTreeMap::from([("model".into(), json!("configured-generator"))]),
            path_parameters: BTreeMap::from([("output_path".into(), ToolPathAccess::Write)]),
            receipt_pointer: "/provider_ref".into(),
            capability_hash: Some("a".repeat(64)),
        }
    }

    #[test]
    fn tariff_and_reported_charges_require_real_bounded_receipts() {
        let mut policy = policy();
        policy.validate().unwrap();
        for charge in [json!(-1), json!(41), json!(3.5), json!("31"), Value::Null] {
            assert!(policy
                .charge(&ActionResult::text(
                    json!({"provider_ref":"receipt", "charged_microusd":charge}).to_string()
                ))
                .is_err());
        }
        assert_eq!(
            policy
                .charge(&ActionResult::text(
                    r#"{"provider_ref":"receipt","charged_microusd":31}"#
                ))
                .unwrap(),
            31
        );
        assert!(policy
            .charge(&ActionResult::text(r#"{"charged_microusd":31}"#))
            .is_err());
        policy.pricing = ToolPricing::Fixed {
            cost_microusd: 28,
            version: "reviewed-tariff".into(),
        };
        assert_eq!(
            policy
                .charge(&ActionResult::text(r#"{"provider_ref":"receipt"}"#))
                .unwrap(),
            28
        );
        assert!(policy
            .charge(&ActionResult::text(r#"{"provider_ref":null}"#))
            .is_err());
    }

    #[test]
    fn pricing_parameters_and_paths_cannot_widen_the_contract() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let policy = policy();
        let good = policy
            .arguments(&json!({"prompt":"create","output_path":"asset.bin"}), &root)
            .unwrap();
        assert_eq!(good["model"], "configured-generator");
        for bad in [
            json!({"prompt":"create", "model":"more-expensive"}),
            json!({"prompt":"create","extra_args":["--raw"]}),
            json!({"output_path":"../outside"}),
            json!({"output_path":"/tmp/outside"}),
        ] {
            assert!(policy.arguments(&bad, &root).is_err());
        }
        std::os::unix::fs::symlink("/tmp", root.join("escape")).unwrap();
        assert!(policy
            .arguments(&json!({"output_path":"escape/asset"}), &root)
            .is_err());
        assert!(totals(&[policy.clone(), policy]).is_err());
    }

    pub(crate) struct ToolFixture {
        _temp: tempfile::TempDir,
        pub(crate) harness: Fixture,
        pub(crate) boundary: Arc<MeteredTools>,
        pub(crate) registry: Arc<CapabilityRegistry>,
        pub(crate) context: PrimitiveExecCtx,
    }
    impl ToolFixture {
        pub(crate) async fn new(known_receipt: bool) -> Self {
            Self::with_workspace_paths(known_receipt, false).await
        }

        async fn with_workspace_paths(known_receipt: bool, workspace_paths: bool) -> Self {
            Self::with_path_access(known_receipt, workspace_paths.then_some("create_file")).await
        }

        async fn with_path_access(known_receipt: bool, path_access: Option<&str>) -> Self {
            let harness = Fixture::with_tools(2, true, Duration::ZERO, 2, 80).await;
            let temp = tempfile::tempdir().unwrap();
            // The governed scope root rejects symlink aliases. macOS TMPDIR
            // often uses /var rather than its canonical /private/var spelling.
            let fixture_root = temp.path().canonicalize().unwrap();
            let storage = fixture_root.join("storage");
            let skill = fixture_root.join("asset-tool");
            let bin = skill.join("bin");
            fs::create_dir_all(&bin).unwrap();
            let source = if let Some(access) = path_access {
                SOURCE.replace(
                    "output_path: {type: string,",
                    &format!("output_path: {{type: workspace_path, access: {access},"),
                )
            } else {
                SOURCE.to_owned()
            };
            fs::write(skill.join("SKILL.md"), &source).unwrap();
            let script = if known_receipt {
                "#!/bin/sh\nprintf x >> counter\nprintf asset > asset.bin\nprintf '{\"provider_ref\":\"receipt-%s\",\"charged_microusd\":31}\\n' \"$$\"\n"
            } else {
                "#!/bin/sh\nprintf x >> counter\nprintf asset > asset.bin\nprintf '{\"path\":\"asset.bin\"}\\n'\n"
            };
            fs::write(bin.join("render-asset"), script).unwrap();
            fs::set_permissions(bin.join("render-asset"), fs::Permissions::from_mode(0o700))
                .unwrap();
            let package = tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
                .unwrap()
                .unwrap();
            let pack =
                super::super::execution::compiled_providers::project_runtime_package_to_pack(
                    "asset-tool",
                    "Generic asset fixture",
                    None,
                    &skill,
                    &skill,
                    package,
                )
                .unwrap();
            let mut policy = policy();
            policy.capability_hash = Some(capability_pin(&pack, "run").unwrap());
            let registry = Arc::new(CapabilityRegistry::new());
            registry.set_pack_definition("asset-tool", pack);
            for path in [
                &storage,
                &storage.join("scopes"),
                &storage.join("scopes/owner"),
                &storage.join("scopes/owner/work"),
            ] {
                fs::create_dir_all(path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let work = fixture_root.join("actor-work");
            fs::create_dir(&work).unwrap();
            let boundary =
                MeteredTools::admit(work, harness.owner.clone(), vec![policy], 4096).unwrap();
            let mut context = PrimitiveExecCtx::default_for_runtime();
            context.storage_base_path = storage.clone();
            context.artifact_workspace =
                Some(super::super::artifact_v2::workspace::ArtifactV2Workspace::new(&storage));
            context.principal = Some("owner".into());
            context.workspace = Some("work".into());
            context.task_id = Some("task-fixture".into());
            context.execution_id = Some("exec-fixture".into());
            context.secret_store = Some(Arc::new(super::super::secrets::SecretStore::new_empty(
                Box::new(super::super::secrets::InMemoryKeyProvider::new()),
                storage.join("audit"),
            )));
            Self {
                _temp: temp,
                harness,
                boundary,
                registry,
                context,
            }
        }
        async fn call(&self, id: &str, prompt: &str) -> Result<ActionResult> {
            let context = self.context.clone().with_effect_id(Some(id.into()));
            self.boundary
                .dispatch(
                    "asset-tool__run",
                    &json!({"prompt":prompt,"output_path":"asset.bin"}),
                    self.registry.clone(),
                    context,
                )
                .await
        }
    }

    #[tokio::test]
    async fn workspace_path_actions_replay_exact_output_and_reject_fresh_overwrites() {
        let fixture = ToolFixture::with_workspace_paths(true, true).await;
        let output = fixture.boundary.root.join("asset.bin");
        fs::write(&output, b"existing owner file").unwrap();
        assert!(fixture
            .call("blocked-create", "create")
            .await
            .unwrap_err()
            .to_string()
            .contains("output path already exists"));
        let empty = fixture
            .harness
            .owner
            .store
            .adapter_budget(fixture.harness.owner.run_id, &fixture.harness.owner.parent)
            .unwrap();
        assert!(empty.ledger.operations().is_empty());
        assert!(empty.physical_starts.is_empty());
        assert!(!fixture.boundary.root.join("counter").exists());
        fs::remove_file(&output).unwrap();

        let first = fixture.call("typed-create", "create").await.unwrap();
        let replay = fixture.call("typed-create", "create").await.unwrap();
        assert_eq!(first.as_text(), replay.as_text());
        fs::remove_file(&output).unwrap();
        fixture.call("typed-create", "create").await.unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"asset");
        assert!(fixture.call("fresh-overwrite", "create").await.is_err());
        assert_eq!(
            fs::read(fixture.boundary.root.join("counter")).unwrap(),
            b"x"
        );
        let budget = fixture
            .harness
            .owner
            .store
            .adapter_budget(fixture.harness.owner.run_id, &fixture.harness.owner.parent)
            .unwrap();
        assert_eq!(budget.physical_starts.len(), 1);
        assert_eq!(budget.ledger.spend().unwrap().settled_microusd, 31);
        assert_eq!(budget.ledger.spend().unwrap().reserved_microusd, 0);
        fixture.harness.owner.close_and_drain().await;
        fixture.harness.queue.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn workspace_path_action_requires_its_exact_access_grant_before_reservation() {
        let mut fixture = ToolFixture::with_workspace_paths(true, true).await;
        let mut policies = fixture.boundary.policies.clone();
        policies[0]
            .path_parameters
            .insert("output_path".into(), ToolPathAccess::Read);
        // A real input is present, so the policy's file validation alone passes.
        fs::write(fixture.boundary.root.join("asset.bin"), b"owner input").unwrap();
        fixture.boundary = MeteredTools::admit(
            fixture.boundary.root.clone(),
            fixture.harness.owner.clone(),
            policies,
            4096,
        )
        .unwrap();
        assert!(fixture
            .call("wrong-access", "create")
            .await
            .unwrap_err()
            .to_string()
            .contains("matching admitted access grant"));
        let budget = fixture
            .harness
            .owner
            .store
            .adapter_budget(fixture.harness.owner.run_id, &fixture.harness.owner.parent)
            .unwrap();
        assert!(budget.ledger.operations().is_empty());
        assert!(budget.physical_starts.is_empty());
        assert!(!fixture.boundary.root.join("counter").exists());
        fixture.harness.owner.close_and_drain().await;
        fixture.harness.queue.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn directory_outputs_are_rejected_before_paid_reservation() {
        // The meter seals regular files. A typed directory creation must not
        // be charged and only then discovered to violate that output contract.
        let fixture = ToolFixture::with_path_access(true, Some("create_directory")).await;
        let error = fixture
            .call("unsupported-directory", "create")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("matching admitted access grant"));
        let budget = fixture
            .harness
            .owner
            .store
            .adapter_budget(fixture.harness.owner.run_id, &fixture.harness.owner.parent)
            .unwrap();
        assert!(budget.ledger.operations().is_empty());
        assert!(budget.physical_starts.is_empty());
        assert!(!fixture.boundary.root.join("asset.bin").exists());
        assert!(!fixture.boundary.root.join("counter").exists());
        fixture.harness.owner.close_and_drain().await;
        fixture.harness.queue.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn governed_generation_regeneration_and_replay_share_the_existing_ledger() {
        let mut fixture = ToolFixture::new(true).await;
        let first = fixture
            .call("first-native-tool-invocation", "create")
            .await
            .unwrap();
        assert_eq!(
            fs::read(fixture.boundary.root.join("asset.bin")).unwrap(),
            b"asset"
        );
        // A fresh runtime boundary restores missing output from the durable
        // receipt, without another provider invocation.
        fs::remove_file(fixture.boundary.root.join("asset.bin")).unwrap();
        fixture.boundary = MeteredTools::admit(
            fixture.boundary.root.clone(),
            fixture.harness.owner.clone(),
            fixture.boundary.policies.clone(),
            4096,
        )
        .unwrap();
        // An identical replay returns exact sealed output, including provider id.
        let replay = fixture
            .call("first-native-tool-invocation", "create")
            .await
            .unwrap();
        assert_eq!(replay.as_text(), first.as_text());
        assert_eq!(
            fs::read(fixture.boundary.root.join("asset.bin")).unwrap(),
            b"asset"
        );
        assert!(fixture
            .call("first-native-tool-invocation", "changed input")
            .await
            .is_err());
        fs::write(fixture.boundary.root.join("asset.bin"), b"later work").unwrap();
        assert!(fixture
            .call("first-native-tool-invocation", "create")
            .await
            .is_err());
        assert_eq!(
            fs::read(fixture.boundary.root.join("asset.bin")).unwrap(),
            b"later work"
        );
        fixture
            .call("critic-driven-regeneration", "revise")
            .await
            .unwrap();
        assert_eq!(
            fs::read(fixture.boundary.root.join("counter")).unwrap(),
            b"xx"
        );
        assert!(fixture
            .call("past-frozen-call-ceiling", "revise again")
            .await
            .is_err());
        fixture.harness.owner.close_and_drain().await;
        assert_eq!(
            fixture
                .harness
                .owner
                .receipt()
                .await
                .unwrap()
                .actual_cost_microusd,
            Some(62)
        );
        let budget = fixture
            .harness
            .owner
            .store
            .adapter_budget(fixture.harness.owner.run_id, &fixture.harness.owner.parent)
            .unwrap();
        assert_eq!(budget.physical_starts.len(), 2);
        assert_eq!(budget.ledger.spend().unwrap().reserved_microusd, 0);
        fixture.harness.queue.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn dropped_waiter_does_not_admit_a_queued_paid_tool() {
        let fixture = ToolFixture::new(true).await;
        let guard = fixture.boundary.gate.lock().await;
        assert!(tokio::time::timeout(
            Duration::from_millis(50),
            fixture.call("abandoned-native-invocation", "create")
        )
        .await
        .is_err());
        drop(guard);
        fixture.harness.owner.close_and_drain().await;
        assert!(!fixture.boundary.root.join("counter").exists());
        let budget = fixture
            .harness
            .owner
            .store
            .adapter_budget(fixture.harness.owner.run_id, &fixture.harness.owner.parent)
            .unwrap();
        assert!(budget.ledger.operations().is_empty());
        assert!(budget.physical_starts.is_empty());
        fixture.harness.queue.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn missing_governed_audit_authority_reserves_nothing_and_does_not_dispatch() {
        let mut fixture = ToolFixture::new(true).await;
        fixture.context.secret_store = None;
        assert!(fixture
            .call("unprepared-native-invocation", "create")
            .await
            .unwrap_err()
            .to_string()
            .contains("audit authority unavailable before dispatch"));
        let budget = fixture
            .harness
            .owner
            .store
            .adapter_budget(fixture.harness.owner.run_id, &fixture.harness.owner.parent)
            .unwrap();
        assert!(budget.ledger.operations().is_empty());
        assert!(budget.physical_starts.is_empty());
        assert!(!fixture.boundary.root.join("counter").exists());
        assert!(!fixture.boundary.root.join("asset.bin").exists());
        fixture.harness.queue.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn unknown_receipt_blocks_regeneration_without_refund_or_resend() {
        let fixture = ToolFixture::new(false).await;
        assert!(fixture
            .call("uncertain-native-invocation", "create")
            .await
            .is_err());
        assert!(fixture
            .call("uncertain-native-invocation", "create")
            .await
            .is_err());
        assert!(fixture
            .call("new-generation-after-unknown", "revise")
            .await
            .is_err());
        assert_eq!(
            fs::read(fixture.boundary.root.join("counter")).unwrap(),
            b"x"
        );
        fixture.harness.owner.close_and_drain().await;
        assert_eq!(
            fixture
                .harness
                .owner
                .receipt()
                .await
                .unwrap()
                .actual_cost_microusd,
            None
        );
        let budget = fixture
            .harness
            .owner
            .store
            .adapter_budget(fixture.harness.owner.run_id, &fixture.harness.owner.parent)
            .unwrap();
        assert_eq!(budget.ledger.spend().unwrap().reserved_microusd, 40);
        assert_eq!(budget.physical_starts.len(), 1);
        fixture.harness.queue.shutdown(Duration::from_secs(1)).await;
    }
}
