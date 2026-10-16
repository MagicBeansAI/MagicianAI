//! Durable HMAC seal for the plane's delegation-attenuation sidecar
//! (`plane_attenuation.json` + `plane_attenuated.json` marker). The seal
//! lives in the scope's restricted store, not beside the execution files;
//! a coordinated local edit of the sidecar files is detected at dispatch
//! and the run fails closed rather than silently widening.
//!
//! Mirrors [`super::durable_routing`] — same key derivation, same scope
//! binding — so the two sidecars share one integrity model.

use serde::{Deserialize, Serialize};

use crate::magician_v2::{
    analytics::llm_trace_content::scoped_content_fingerprint,
    artifact_v2::workspace::ArtifactV2Workspace,
    execution::agentic::types::PlaneDelegationAttenuation,
};

const PLANE_ATTENUATION_SEAL_SCHEMA_VERSION: u8 = 1;
const PLANE_ATTENUATION_SEAL_DOMAIN: &str = "magician.plane-attenuation.v1";

/// Durable integrity proof for a plane delegation-attenuation sidecar.
/// Binding scope plus task/execution identity prevents a valid
/// sidecar/seal pair from being copied to a different run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaneAttenuationIntegritySeal {
    pub schema_version: u8,
    pub hmac_sha256: String,
}

pub fn plane_attenuation_seal_path(
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
        .join("plane_attenuation")
        .join(format!("{identity}.json"))
}

fn attenuation_binding_bytes(
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
    attenuation: &PlaneDelegationAttenuation,
) -> Result<Vec<u8>, String> {
    let canonical = serde_json::to_value((
        PLANE_ATTENUATION_SEAL_DOMAIN,
        PLANE_ATTENUATION_SEAL_SCHEMA_VERSION,
        principal,
        workspace,
        task_id,
        execution_id,
        attenuation,
    ))
    .map_err(|error| format!("plane_attenuation_binding_serialize_failed:{error}"))?;
    serde_json::to_vec(&canonical)
        .map_err(|error| format!("plane_attenuation_binding_serialize_failed:{error}"))
}

pub fn seal_plane_attenuation(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
    attenuation: &PlaneDelegationAttenuation,
) -> Result<PlaneAttenuationIntegritySeal, String> {
    let scope = magicllm::LlmScope::new(principal, workspace);
    let bytes =
        attenuation_binding_bytes(principal, workspace, task_id, execution_id, attenuation)?;
    let hmac_sha256 = scoped_content_fingerprint(workspace_layout, &scope, &bytes)
        .map_err(|error| error.to_string())?;
    Ok(PlaneAttenuationIntegritySeal {
        schema_version: PLANE_ATTENUATION_SEAL_SCHEMA_VERSION,
        hmac_sha256,
    })
}

pub fn verify_plane_attenuation_seal(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
    attenuation: &PlaneDelegationAttenuation,
    seal: &PlaneAttenuationIntegritySeal,
) -> Result<(), String> {
    if seal.schema_version != PLANE_ATTENUATION_SEAL_SCHEMA_VERSION {
        return Err(format!(
            "plane_attenuation_seal_schema_unsupported:{}",
            seal.schema_version
        ));
    }
    let expected = seal_plane_attenuation(
        workspace_layout,
        principal,
        workspace,
        task_id,
        execution_id,
        attenuation,
    )?;
    if expected.hmac_sha256 != seal.hmac_sha256 {
        return Err(format!("plane_attenuation_seal_mismatch:{execution_id}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> ArtifactV2Workspace {
        ArtifactV2Workspace::new(
            std::env::temp_dir().join(format!("plane-att-seal-{}", uuid::Uuid::new_v4().simple())),
        )
    }

    fn attenuation() -> PlaneDelegationAttenuation {
        PlaneDelegationAttenuation {
            denied: vec!["run_task".to_string()],
            allowed: Some(vec!["read_file".to_string()]),
            harness_engine: Some("claude_code".to_string()),
            max_usd: Some(1.0),
            max_wall_clock_secs: Some(600),
        }
    }

    /// The seal detects a coordinated sidecar edit: the exact threat the
    /// fail-closed marker alone could not catch.
    #[test]
    fn a_tampered_sidecar_fails_seal_verification() {
        let layout = layout();
        let original = attenuation();
        let seal =
            seal_plane_attenuation(&layout, "owner", "default", "task-1", "exec-1", &original)
                .expect("seal");
        verify_plane_attenuation_seal(
            &layout, "owner", "default", "task-1", "exec-1", &original, &seal,
        )
        .expect("clean attenuation verifies");
        let mut tampered = original.clone();
        tampered.allowed = None;
        assert!(
            verify_plane_attenuation_seal(
                &layout, "owner", "default", "task-1", "exec-1", &tampered, &seal,
            )
            .is_err(),
            "a widened sidecar must fail the seal"
        );
    }

    /// Binding includes execution identity: a valid pair cannot be replayed
    /// onto a different run.
    #[test]
    fn a_seal_does_not_transfer_across_executions() {
        let layout = layout();
        let attenuation = attenuation();
        let seal = seal_plane_attenuation(
            &layout,
            "owner",
            "default",
            "task-1",
            "exec-1",
            &attenuation,
        )
        .expect("seal");
        assert!(
            verify_plane_attenuation_seal(
                &layout,
                "owner",
                "default",
                "task-1",
                "exec-2",
                &attenuation,
                &seal,
            )
            .is_err(),
            "cross-execution replay must fail"
        );
    }
}
