use serde::{Deserialize, Serialize};

use crate::magician_v2::{
    analytics::llm_trace_content::scoped_content_fingerprint,
    artifact_v2::workspace::ArtifactV2Workspace,
    query_analysis::operation_llm_router::OperationRoutingOverrides,
    work_context::WorkAuthorityRef,
};

pub const ACCEPTED_RUNTIME_LAUNCH_SCHEMA_VERSION: u8 = 2;
const ACCEPTED_RUNTIME_LAUNCH_V1_SCHEMA_VERSION: u8 = 1;
const ACCEPTED_RUNTIME_LAUNCH_V1_SEAL_DOMAIN: &str = "magician.accepted-runtime-launch.v1";
const ACCEPTED_RUNTIME_LAUNCH_V2_SEAL_DOMAIN: &str = "magician.accepted-runtime-launch.v2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcceptedRuntimeLaunchMode {
    Direct,
    Planning,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "phase")]
pub enum AcceptedRuntimeLaunchState {
    Pending,
    Claimed {
        claim_id: String,
        claimed_until_ms: i64,
    },
    Consumed {
        consumed_at_ms: i64,
        reason: String,
    },
}

/// Exact first-owner delegation selected before an HTTP launch is accepted.
/// Both fields are derived from the canonical target and sealed request goal;
/// recovery never consults the mutable execution sidecar for this decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceptedExplicitDelegation {
    pub target_agent_id: String,
    pub child_execution_id: String,
}

/// Exact server-accepted request that must survive the gap between an HTTP
/// `202 Accepted` response and the first durable runtime/control generation.
///
/// The intent and its scope-keyed HMAC live in one restricted atomic envelope.
/// Every lifecycle mutation increments `revision` and replaces that envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcceptedRuntimeLaunchIntent {
    pub schema_version: u8,
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub artifact_execution_id: String,
    pub runtime_execution_id: String,
    pub runtime_created_at: i64,
    pub runtime_created_updated_at: i64,
    pub runtime_root_execution_id: Option<String>,
    pub runtime_parent_execution_id: Option<String>,
    pub runtime_active_owner_agent_id: String,
    pub runtime_owner_stack: Vec<String>,
    pub runtime_work_authority: Option<WorkAuthorityRef>,
    pub runtime_entry_mode: String,
    pub mode: AcceptedRuntimeLaunchMode,
    pub message: String,
    pub max_iterations: Option<usize>,
    pub llm_routing_overrides: Option<OperationRoutingOverrides>,
    pub env_mode: Option<String>,
    pub in_reply_to_slot_id: Option<String>,
    pub accepted_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_delegation: Option<AcceptedExplicitDelegation>,
    pub accepted_at_ms: i64,
    pub revision: u64,
    pub state: AcceptedRuntimeLaunchState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceptedRuntimeLaunchIntentSeal {
    pub schema_version: u8,
    pub hmac_sha256: String,
}

/// Single-file commit envelope. Keeping the intent and its HMAC in one atomic
/// rename prevents a crash during claim/heartbeat updates from producing a
/// permanently mismatched two-file authority pair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SealedAcceptedRuntimeLaunch {
    pub intent: AcceptedRuntimeLaunchIntent,
    pub seal: AcceptedRuntimeLaunchIntentSeal,
}

pub fn accepted_runtime_launch_seal_path(
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
        .join("accepted_runtime_launch")
        .join(format!("{identity}.json"))
}

fn accepted_runtime_launch_binding_bytes(
    intent: &AcceptedRuntimeLaunchIntent,
) -> Result<Vec<u8>, String> {
    let domain = match intent.schema_version {
        ACCEPTED_RUNTIME_LAUNCH_V1_SCHEMA_VERSION => ACCEPTED_RUNTIME_LAUNCH_V1_SEAL_DOMAIN,
        ACCEPTED_RUNTIME_LAUNCH_SCHEMA_VERSION => ACCEPTED_RUNTIME_LAUNCH_V2_SEAL_DOMAIN,
        other => {
            return Err(format!(
                "accepted_runtime_launch_schema_unsupported:{other}"
            ))
        },
    };
    let canonical = serde_json::to_value((domain, intent.schema_version, intent))
        .map_err(|error| format!("accepted_runtime_launch_binding_serialize_failed:{error}"))?;
    serde_json::to_vec(&canonical)
        .map_err(|error| format!("accepted_runtime_launch_binding_serialize_failed:{error}"))
}

pub fn seal_accepted_runtime_launch(
    workspace_layout: &ArtifactV2Workspace,
    intent: &AcceptedRuntimeLaunchIntent,
) -> Result<AcceptedRuntimeLaunchIntentSeal, String> {
    if !matches!(
        intent.schema_version,
        ACCEPTED_RUNTIME_LAUNCH_V1_SCHEMA_VERSION | ACCEPTED_RUNTIME_LAUNCH_SCHEMA_VERSION
    ) {
        return Err(format!(
            "accepted_runtime_launch_schema_unsupported:{}",
            intent.schema_version
        ));
    }
    let scope = magicllm::LlmScope::new(&intent.principal, &intent.workspace);
    let bytes = accepted_runtime_launch_binding_bytes(intent)?;
    let hmac_sha256 = scoped_content_fingerprint(workspace_layout, &scope, &bytes)
        .map_err(|error| error.to_string())?;
    Ok(AcceptedRuntimeLaunchIntentSeal {
        schema_version: intent.schema_version,
        hmac_sha256,
    })
}

pub fn verify_accepted_runtime_launch_seal(
    workspace_layout: &ArtifactV2Workspace,
    intent: &AcceptedRuntimeLaunchIntent,
    seal: &AcceptedRuntimeLaunchIntentSeal,
) -> Result<(), String> {
    if seal.schema_version != intent.schema_version {
        return Err(format!(
            "accepted_runtime_launch_seal_schema_unsupported:{}",
            seal.schema_version
        ));
    }
    let expected = seal_accepted_runtime_launch(workspace_layout, intent)?;
    if !constant_time_eq(expected.hmac_sha256.as_bytes(), seal.hmac_sha256.as_bytes()) {
        return Err("accepted_runtime_launch_seal_mismatch".to_string());
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

    fn intent() -> AcceptedRuntimeLaunchIntent {
        AcceptedRuntimeLaunchIntent {
            schema_version: ACCEPTED_RUNTIME_LAUNCH_SCHEMA_VERSION,
            principal: "principal".to_owned(),
            workspace: "workspace".to_owned(),
            task_id: "task-1".to_owned(),
            artifact_execution_id: "exec-1".to_owned(),
            runtime_execution_id: "exec-1".to_owned(),
            runtime_created_at: 10,
            runtime_created_updated_at: 11,
            runtime_root_execution_id: Some("exec-1".to_owned()),
            runtime_parent_execution_id: None,
            runtime_active_owner_agent_id: "agent-1".to_owned(),
            runtime_owner_stack: Vec::new(),
            runtime_work_authority: None,
            runtime_entry_mode: "planning_backed".to_owned(),
            mode: AcceptedRuntimeLaunchMode::Direct,
            message: "do the exact work".to_owned(),
            max_iterations: Some(17),
            llm_routing_overrides: None,
            env_mode: Some("browser".to_owned()),
            in_reply_to_slot_id: None,
            accepted_turn_id: None,
            explicit_delegation: None,
            accepted_at_ms: 12,
            revision: 1,
            state: AcceptedRuntimeLaunchState::Pending,
        }
    }

    #[test]
    fn accepted_launch_seal_binds_request_and_lifecycle_revision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let original = intent();
        let seal = seal_accepted_runtime_launch(&workspace, &original).expect("seal");
        verify_accepted_runtime_launch_seal(&workspace, &original, &seal).expect("verify");

        let mut changed = original.clone();
        changed.message.push_str(" changed");
        assert!(verify_accepted_runtime_launch_seal(&workspace, &changed, &seal).is_err());
        changed = original.clone();
        changed.revision += 1;
        assert!(verify_accepted_runtime_launch_seal(&workspace, &changed, &seal).is_err());

        let mut explicit = original.clone();
        explicit.explicit_delegation = Some(AcceptedExplicitDelegation {
            target_agent_id: "delegate".to_owned(),
            child_execution_id: "exec-deleg-exact".to_owned(),
        });
        assert!(verify_accepted_runtime_launch_seal(&workspace, &explicit, &seal).is_err());
    }

    #[test]
    fn rollout_v1_envelope_remains_verifiable_until_lifecycle_upgrade() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let mut legacy = intent();
        legacy.schema_version = ACCEPTED_RUNTIME_LAUNCH_V1_SCHEMA_VERSION;
        legacy.explicit_delegation = None;
        let seal = seal_accepted_runtime_launch(&workspace, &legacy).expect("v1 seal");
        assert_eq!(
            seal.schema_version,
            ACCEPTED_RUNTIME_LAUNCH_V1_SCHEMA_VERSION
        );
        verify_accepted_runtime_launch_seal(&workspace, &legacy, &seal).expect("v1 verify");
    }
}
