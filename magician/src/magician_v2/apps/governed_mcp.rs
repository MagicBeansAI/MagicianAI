//! Apps adapter for reviewed governed MCP skills.
//!
//! MCP is not an OS-jail runtime. Checkout, cart checks, OAuth, and INR
//! settlement already live in `dispatch_governed_mcp`. This owner attests the
//! exact reviewed source and product action, then calls that same dispatcher
//! so Zepto/Swiggy from an app cannot skip Resource Authority.

use std::sync::{Arc, OnceLock};

use serde_json::{json, Map, Value};
use thiserror::Error;
use tool_runtime_core::{
    manifest::RuntimeProtocol,
    manifest_parser::{parse_skill_frontmatter, parse_skill_runtime_package, SkillRuntimePackage},
    manifest_validation::validate_skill_runtime_contract,
    mcp_catalog_projection::{project_mcp_catalog, McpCatalogActionProjection},
};

use super::{
    effect_kernel::{AppEffectKernelError, AppEffectPhysicalOwner, AppEffectPhysicalTarget},
    models::{AppDigest, AppReference},
    package_lock::{AppLockedPrimitiveActionBinding, AppLockedPrimitiveBinding},
    tool_disclosure::AttestedAppToolTarget,
    tool_eligibility::{assess_app_tool_eligibility, AppToolAdmissionSource},
};
use crate::magician_v2::{
    execution::{
        actions::ActionResult,
        capability::GovernedRuntimeImplementation,
        error::ExecutionError,
        primitive_dispatch::{
            admit_governed_route, dispatch_governed_mcp, fold_primitive_result, PrimitiveExecCtx,
        },
    },
    json_traversal::{
        canonical_json_bytes, json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded,
    },
};

pub(crate) const APP_GOVERNED_MCP_PROFILE_V1: &str = "magician.app-governed-mcp.v1";
pub(crate) const APP_GOVERNED_MCP_IMPLEMENTATION_REVISION: &str =
    "magician.app-governed-mcp-implementation.2026-08-30.1";
pub(crate) const MAX_APP_GOVERNED_MCP_INPUT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_APP_GOVERNED_MCP_RESULT_BYTES: u64 = 64 * 1024;
const MAX_APP_GOVERNED_MCP_JSON_DEPTH: usize = 32;
const MAX_APP_GOVERNED_MCP_JSON_NODES: usize = 32 * 1024;

#[derive(Debug, Error)]
pub enum AppGovernedMcpError {
    #[error("reviewed app MCP skill bytes are invalid or not app-exposed")]
    InvalidSource,
    #[error("app governed MCP execution requires an official-SDK MCP package")]
    UnsupportedRuntime,
    #[error("the exact locked primitive/action does not match the reviewed MCP skill")]
    IdentityMismatch,
    #[error("app governed MCP input is invalid or exceeds its structural bounds")]
    InvalidInput,
}

/// Move-only exact MCP action plan produced from reviewed source plus one
/// locked product action. Execution authority is the existing governed MCP
/// dispatcher; this type does not admit spend itself.
pub(crate) struct AppGovernedMcpPreparedAction {
    skill_name: String,
    source_digest: AppDigest,
    primitive_ref: AppReference,
    action_name: String,
    action_ref: AppReference,
    input_schema_digest: AppDigest,
    execution_plan_digest: AppDigest,
    transport_result_byte_ceiling: u64,
    runtime: Arc<GovernedRuntimeImplementation>,
    arguments: Value,
}

impl AppGovernedMcpPreparedAction {
    pub(crate) async fn execute(
        self,
        exec_ctx: PrimitiveExecCtx,
    ) -> Result<ActionResult, ExecutionError> {
        let capability_id = self.skill_name.clone();
        let action_id = self.action_name.clone();
        let result = dispatch_governed_mcp(
            admit_governed_route(),
            self.runtime,
            capability_id.clone(),
            action_id.clone(),
            self.arguments,
            exec_ctx,
        )
        .await
        .map_err(ExecutionError::Step)?;
        fold_primitive_result(&capability_id, &action_id, result)
    }
}

impl AppEffectPhysicalOwner for AppGovernedMcpPreparedAction {
    fn attest_effect_target(
        &self,
        tool_ref: &AppReference,
        primitive: &AppLockedPrimitiveBinding,
        action: &AppLockedPrimitiveActionBinding,
    ) -> Result<AppEffectPhysicalTarget, AppEffectKernelError> {
        attest_governed_mcp_effect_target(
            &self.skill_name,
            &self.source_digest,
            &self.primitive_ref,
            &self.action_name,
            &self.action_ref,
            &self.input_schema_digest,
            &self.execution_plan_digest,
            self.transport_result_byte_ceiling,
            tool_ref,
            primitive,
            action,
        )
    }
}

pub(crate) fn reviewed_transport_result_byte_ceiling() -> u64 {
    MAX_APP_GOVERNED_MCP_RESULT_BYTES
}

pub(crate) fn implementation_plan_digest(
    source_digest: &AppDigest,
    package: &SkillRuntimePackage,
    action_name: &str,
    input_schema: &Value,
) -> Option<AppDigest> {
    if !matches!(package.contract.runtime, RuntimeProtocol::Mcp { .. }) || package.actions.is_some()
    {
        return None;
    }
    let runtime_implementation_digest = governed_mcp_runtime_implementation_digest()?;
    AppDigest::blake3_canonical_json(&json!({
        "profile": APP_GOVERNED_MCP_PROFILE_V1,
        "implementation_revision": APP_GOVERNED_MCP_IMPLEMENTATION_REVISION,
        "runtime_implementation_digest": runtime_implementation_digest,
        "source_digest": source_digest,
        "transport_input_byte_ceiling": MAX_APP_GOVERNED_MCP_INPUT_BYTES as u64,
        "transport_result_byte_ceiling": MAX_APP_GOVERNED_MCP_RESULT_BYTES,
        "action": action_name,
        "input_schema": input_schema,
        "contract": &package.contract,
    }))
    .ok()
}

pub(crate) fn mcp_action_input_schema(action: &McpCatalogActionProjection) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for parameter in &action.parameters {
        properties.insert(parameter.name.clone(), parameter.schema.clone());
        if parameter.required {
            required.push(Value::String(parameter.name.clone()));
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

pub(crate) fn prepare_locked_governed_mcp_action(
    source_bytes: Vec<u8>,
    primitive: &AppLockedPrimitiveBinding,
    locked: &AppLockedPrimitiveActionBinding,
    canonical_input: &[u8],
) -> Result<AppGovernedMcpPreparedAction, AppGovernedMcpError> {
    if source_bytes.is_empty()
        || source_bytes.len() > tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES
    {
        return Err(AppGovernedMcpError::InvalidSource);
    }
    let eligible =
        assess_app_tool_eligibility(&source_bytes, AppToolAdmissionSource::ReviewedCatalog)
            .map_err(|_| AppGovernedMcpError::InvalidSource)?;
    let source =
        std::str::from_utf8(&source_bytes).map_err(|_| AppGovernedMcpError::InvalidSource)?;
    #[derive(serde::Deserialize)]
    struct Header {
        name: String,
    }
    let header: Header =
        parse_skill_frontmatter(source).map_err(|_| AppGovernedMcpError::InvalidSource)?;
    if header.name != eligible.name.as_str() {
        return Err(AppGovernedMcpError::InvalidSource);
    }
    let package = parse_skill_runtime_package(source)
        .map_err(|_| AppGovernedMcpError::InvalidSource)?
        .ok_or(AppGovernedMcpError::InvalidSource)?;
    if !matches!(package.contract.runtime, RuntimeProtocol::Mcp { .. }) || package.actions.is_some()
    {
        return Err(AppGovernedMcpError::UnsupportedRuntime);
    }
    validate_skill_runtime_contract(&package.contract)
        .map_err(|_| AppGovernedMcpError::InvalidSource)?;
    let projected =
        project_mcp_catalog(&package).map_err(|_| AppGovernedMcpError::InvalidSource)?;
    let authored = projected
        .actions
        .get(locked.name())
        .ok_or(AppGovernedMcpError::IdentityMismatch)?;
    let input_schema = mcp_action_input_schema(authored);
    let source_digest = AppDigest::blake3(&source_bytes);
    let input_schema_digest = AppDigest::blake3_canonical_json(&input_schema)
        .map_err(|_| AppGovernedMcpError::InvalidSource)?;
    let execution_plan_digest =
        implementation_plan_digest(&source_digest, &package, locked.name(), &input_schema)
            .ok_or(AppGovernedMcpError::InvalidSource)?;
    if primitive.source_content_digest() != &source_digest
        || primitive.primitive_ref().as_str().is_empty()
        || !primitive
            .actions()
            .iter()
            .any(|candidate| candidate == locked)
        || !locked.dispatchable()
        || locked.input_schema_digest() != Some(&input_schema_digest)
        || locked.implementation_plan_digest() != Some(&execution_plan_digest)
        || locked.transport_result_byte_ceiling() != Some(MAX_APP_GOVERNED_MCP_RESULT_BYTES)
        || locked.physical_artifact_revision_ref().is_some()
        || locked.physical_artifact_digest().is_some()
    {
        return Err(AppGovernedMcpError::IdentityMismatch);
    }
    let arguments = parse_mcp_arguments(canonical_input)?;
    Ok(AppGovernedMcpPreparedAction {
        skill_name: header.name,
        source_digest,
        primitive_ref: primitive.primitive_ref().clone(),
        action_name: locked.name().to_owned(),
        action_ref: locked.action_ref().clone(),
        input_schema_digest,
        execution_plan_digest,
        transport_result_byte_ceiling: MAX_APP_GOVERNED_MCP_RESULT_BYTES,
        runtime: Arc::new(GovernedRuntimeImplementation {
            package,
            actions: None,
            executable_directory: None,
            legacy_secret_environment: None,
        }),
        arguments,
    })
}

fn parse_mcp_arguments(canonical_input: &[u8]) -> Result<Value, AppGovernedMcpError> {
    if canonical_input.is_empty() || canonical_input.len() > MAX_APP_GOVERNED_MCP_INPUT_BYTES {
        return Err(AppGovernedMcpError::InvalidInput);
    }
    if !json_bytes_nesting_is_bounded(canonical_input, MAX_APP_GOVERNED_MCP_JSON_DEPTH)
        || !json_bytes_nodes_are_bounded(canonical_input, MAX_APP_GOVERNED_MCP_JSON_NODES)
    {
        return Err(AppGovernedMcpError::InvalidInput);
    }
    let value: Value =
        serde_json::from_slice(canonical_input).map_err(|_| AppGovernedMcpError::InvalidInput)?;
    let recanonical =
        canonical_json_bytes(&value).map_err(|_| AppGovernedMcpError::InvalidInput)?;
    if recanonical.as_slice() != canonical_input {
        return Err(AppGovernedMcpError::InvalidInput);
    }
    let Some(object) = value.as_object() else {
        return Err(AppGovernedMcpError::InvalidInput);
    };
    Ok(Value::Object(
        object
            .iter()
            .filter(|(key, _)| !key.starts_with("__"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn attest_governed_mcp_effect_target(
    skill_name: &str,
    source_digest: &AppDigest,
    primitive_ref: &AppReference,
    action_name: &str,
    action_ref: &AppReference,
    input_schema_digest: &AppDigest,
    execution_plan_digest: &AppDigest,
    transport_result_byte_ceiling: u64,
    tool_ref: &AppReference,
    primitive: &AppLockedPrimitiveBinding,
    action: &AppLockedPrimitiveActionBinding,
) -> Result<AppEffectPhysicalTarget, AppEffectKernelError> {
    let expected_tool_ref = format!("capability:{skill_name}");
    if tool_ref.as_str() != expected_tool_ref
        || primitive.primitive_ref() != primitive_ref
        || primitive.source_content_digest() != source_digest
        || !primitive
            .actions()
            .iter()
            .any(|candidate| candidate == action)
        || !action.dispatchable()
        || action.name() != action_name
        || action.action_ref() != action_ref
        || action.input_schema_digest() != Some(input_schema_digest)
        || action.implementation_plan_digest() != Some(execution_plan_digest)
        || action.transport_result_byte_ceiling() != Some(transport_result_byte_ceiling)
        || action.physical_artifact_revision_ref().is_some()
        || action.physical_artifact_digest().is_some()
    {
        return Err(AppEffectKernelError::IdentityMismatch);
    }
    let target_ref = runtime_target_ref(execution_plan_digest, transport_result_byte_ceiling)
        .map_err(|_| AppEffectKernelError::IdentityMismatch)?;
    let target =
        AttestedAppToolTarget::from_trusted_local_dispatcher(tool_ref.clone(), target_ref.clone());
    Ok(AppEffectPhysicalTarget::from_owner(
        target_ref,
        source_digest.clone(),
        target,
    ))
}

fn runtime_target_ref(
    execution_plan_digest: &AppDigest,
    transport_result_byte_ceiling: u64,
) -> Result<AppReference, AppGovernedMcpError> {
    let runtime_implementation_digest = governed_mcp_runtime_implementation_digest()
        .ok_or(AppGovernedMcpError::IdentityMismatch)?;
    let target_digest = AppDigest::blake3_canonical_json(&json!({
        "profile": APP_GOVERNED_MCP_PROFILE_V1,
        "implementation_revision": APP_GOVERNED_MCP_IMPLEMENTATION_REVISION,
        "runtime_implementation_digest": runtime_implementation_digest,
        "execution_plan_digest": execution_plan_digest,
        "transport_result_byte_ceiling": transport_result_byte_ceiling,
    }))
    .map_err(|_| AppGovernedMcpError::IdentityMismatch)?;
    let digest = target_digest
        .as_str()
        .strip_prefix("blake3:")
        .ok_or(AppGovernedMcpError::IdentityMismatch)?;
    AppReference::parse(format!("runtime:app-governed-mcp:v1:{digest}"))
        .map_err(|_| AppGovernedMcpError::IdentityMismatch)
}

fn governed_mcp_runtime_implementation_digest() -> Option<&'static AppDigest> {
    static DIGEST: OnceLock<Option<AppDigest>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"magician.app-governed-mcp-runtime-source.v1\0");
            hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
            for (name, source) in [
                (
                    "apps/governed-mcp",
                    include_bytes!("governed_mcp.rs").as_slice(),
                ),
                (
                    "execution/governed-mcp",
                    include_bytes!("../execution/primitive_dispatch/governed_mcp.rs").as_slice(),
                ),
                (
                    "core/mcp-catalog-projection",
                    tool_runtime_core::source_bytes::MCP_CATALOG_PROJECTION,
                ),
            ] {
                hasher.update(name.as_bytes());
                hasher.update(b"\0");
                hasher.update(source);
                hasher.update(b"\0");
            }
            Some(AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex())).ok()?)
        })
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_plan_digest_is_none_for_cli_packages() {
        let source = AppDigest::blake3(b"cli");
        let package = parse_skill_runtime_package(
            "---\nname: next-step\nversion: 0.1.0\ndescription: cli\nmetadata:\n  magician:\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires:\n        bins: [next-step]\n      runtime:\n        protocol: cli\n        command_prefix: []\n    runtime_actions:\n      schema_version: tool-runtime.typed-action-overrides.v1\n      actions:\n        rank:\n          description: Rank.\n          fixed_args: [rank]\n---\n",
        )
        .ok()
        .flatten()
        .expect("cli package");
        assert!(implementation_plan_digest(&source, &package, "rank", &json!({})).is_none());
    }
}
