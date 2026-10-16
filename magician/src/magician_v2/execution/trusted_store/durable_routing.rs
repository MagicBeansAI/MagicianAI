use serde::{Deserialize, Serialize};

use crate::magician_v2::{
    analytics::llm_trace_content::scoped_content_fingerprint,
    artifact_v2::workspace::ArtifactV2Workspace,
    query_analysis::operation_llm_router::OperationRoutingOverrides,
};

const EXECUTION_ROUTING_SEAL_SCHEMA_VERSION: u8 = 1;
const EXECUTION_ROUTING_SEAL_DOMAIN: &str = "magician.execution-routing.v1";

pub fn execution_routing_seal_path(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
) -> std::path::PathBuf {
    let identity = blake3::hash(format!("{task_id}\0{execution_id}").as_bytes())
        .to_hex()
        .to_string();
    workspace_layout
        .scope_root(principal, workspace)
        .join("restricted")
        .join("execution_routing")
        .join(format!("{identity}.json"))
}

/// Durable integrity proof for an execution-local routing sidecar.
///
/// The HMAC key lives in the scope's restricted store rather than beside the
/// agent-visible execution files. Binding scope plus task/execution identity
/// prevents a valid sidecar/seal pair from being copied to a different run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionRoutingIntegritySeal {
    pub schema_version: u8,
    pub hmac_sha256: String,
}

fn routing_binding_bytes(
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
    overrides: &OperationRoutingOverrides,
) -> Result<Vec<u8>, String> {
    let canonical = serde_json::to_value((
        EXECUTION_ROUTING_SEAL_DOMAIN,
        EXECUTION_ROUTING_SEAL_SCHEMA_VERSION,
        principal,
        workspace,
        task_id,
        execution_id,
        overrides,
    ))
    .map_err(|error| format!("execution_routing_binding_serialize_failed:{error}"))?;
    serde_json::to_vec(&canonical)
        .map_err(|error| format!("execution_routing_binding_serialize_failed:{error}"))
}

pub fn seal_execution_routing(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
    overrides: &OperationRoutingOverrides,
) -> Result<ExecutionRoutingIntegritySeal, String> {
    let scope = magicllm::LlmScope::new(principal, workspace);
    let bytes = routing_binding_bytes(principal, workspace, task_id, execution_id, overrides)?;
    let hmac_sha256 = scoped_content_fingerprint(workspace_layout, &scope, &bytes)
        .map_err(|error| error.to_string())?;
    Ok(ExecutionRoutingIntegritySeal {
        schema_version: EXECUTION_ROUTING_SEAL_SCHEMA_VERSION,
        hmac_sha256,
    })
}

pub fn verify_execution_routing_seal(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
    overrides: &OperationRoutingOverrides,
    seal: &ExecutionRoutingIntegritySeal,
) -> Result<(), String> {
    if seal.schema_version != EXECUTION_ROUTING_SEAL_SCHEMA_VERSION {
        return Err(format!(
            "execution_routing_seal_schema_unsupported:{}",
            seal.schema_version
        ));
    }
    let expected = seal_execution_routing(
        workspace_layout,
        principal,
        workspace,
        task_id,
        execution_id,
        overrides,
    )?;
    if !constant_time_eq(expected.hmac_sha256.as_bytes(), seal.hmac_sha256.as_bytes()) {
        return Err("execution_routing_seal_mismatch".to_string());
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingEndpoint;

    #[test]
    fn routing_seal_binds_route_and_execution_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let route = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::for_profile("eval-luna"),
            ..Default::default()
        };
        let seal = seal_execution_routing(
            &workspace,
            "principal",
            "workspace",
            "task-1",
            "exec-1",
            &route,
        )
        .expect("seal");
        verify_execution_routing_seal(
            &workspace,
            "principal",
            "workspace",
            "task-1",
            "exec-1",
            &route,
            &seal,
        )
        .expect("matching binding");
        assert!(verify_execution_routing_seal(
            &workspace,
            "principal",
            "workspace",
            "task-2",
            "exec-1",
            &route,
            &seal,
        )
        .is_err());
        assert!(verify_execution_routing_seal(
            &workspace,
            "principal",
            "other-workspace",
            "task-1",
            "exec-1",
            &route,
            &seal,
        )
        .is_err());

        let changed_route = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::for_profile("production-terra"),
            ..Default::default()
        };
        assert!(verify_execution_routing_seal(
            &workspace,
            "principal",
            "workspace",
            "task-1",
            "exec-1",
            &changed_route,
            &seal,
        )
        .is_err());
        assert!(verify_execution_routing_seal(
            &workspace,
            "principal",
            "workspace",
            "task-1",
            "exec-2",
            &route,
            &seal,
        )
        .is_err());
    }
}
