//! End-to-end: media_edit trims a synthetic clip in the background and
//! media_edit_status reports completion, with the output registered as an
//! attachment. No binary fixtures — the input clip is synthesized via
//! ffmpeg's lavfi virtual source.

use std::sync::{Arc, OnceLock, RwLock};

use serde_json::json;

use magician::config::MagicianConfig;
use magician::magician_v2::agents::definition_store::AgentDefinitionStore;
use magician::magician_v2::agents::memory::AgentMemoryResolver;
use magician::magician_v2::agents::storage::AgentStorage;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::execution::agent_resources::AgentResources;
use magician::magician_v2::execution::media_edit::{handle_media_edit, handle_media_edit_status};

fn test_agent_resources(root: &std::path::Path) -> Arc<AgentResources> {
    let workspace = ArtifactV2Workspace::new(root);
    let memory_resolver = Arc::new(AgentMemoryResolver::new(root));
    let agent_storage = AgentStorage::new(root);
    let agent_definition_store = Arc::new(AgentDefinitionStore::new(agent_storage));

    Arc::new(AgentResources {
        magician_config: Arc::new(RwLock::new(MagicianConfig::default())),
        memory_resolver,
        agent_definition_store,
        artifact_workspace: workspace,
        artifact_v2_service: None,
        event_broadcaster: None,
        operation_llm_router: None,
        secret_store_resolver: None,
        content_acquisition_resolver: Arc::new(RwLock::new(None)),
        file_sandbox: Default::default(),
        tool_index: Arc::new(OnceLock::new()),
        user_request_service: None,
        agent_runtime: None,
    })
}

fn resolve_ffmpeg_bin() -> String {
    for candidate in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"] {
        if std::path::Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "ffmpeg".to_string()
}

#[tokio::test]
async fn trims_a_synthetic_clip_end_to_end() {
    let root = std::env::temp_dir().join(format!("media_edit_e2e_test_{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let resources = test_agent_resources(&root);

    let workspace_root = resources
        .artifact_workspace
        .capability_home_root("test-principal", "test-workspace");
    std::fs::create_dir_all(&workspace_root).unwrap();
    let input_path = workspace_root.join("input.mp4");

    let status = tokio::process::Command::new(resolve_ffmpeg_bin())
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=3:size=64x64:rate=5",
            "-f",
            "lavfi",
            "-i",
            "sine=duration=3",
            "-shortest",
        ])
        .arg(&input_path)
        .status()
        .await
        .unwrap();
    assert!(status.success(), "failed to synthesize input clip");

    let edit_args = json!({
        "__principal": "test-principal",
        "__workspace": "test-workspace",
        "operation": "trim",
        "input_paths": ["input.mp4"],
        "params": {"start_secs": 0.5, "end_secs": 2.0},
    });
    let edit_result = handle_media_edit(Arc::clone(&resources), edit_args)
        .await
        .unwrap();
    assert_eq!(
        edit_result["status"], "running",
        "unexpected response: {edit_result:?}"
    );
    let job_id = edit_result["job_id"].as_str().unwrap().to_string();

    let mut final_status = json!({});
    for _ in 0..200 {
        let status_args = json!({"job_id": job_id});
        final_status = handle_media_edit_status(Arc::clone(&resources), status_args)
            .await
            .unwrap();
        if final_status["status"] == "completed" || final_status["status"] == "failed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    assert_eq!(
        final_status["status"], "completed",
        "job did not complete: {final_status:?}"
    );
    let output_path = final_status["output_path"].as_str().unwrap();
    let metadata = std::fs::metadata(output_path).expect("trimmed output should exist on disk");
    assert!(metadata.len() > 0, "trimmed output should be non-empty");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn unknown_operation_is_refused_synchronously_with_no_job_created() {
    let root = std::env::temp_dir().join(format!(
        "media_edit_e2e_unknown_op_test_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let resources = test_agent_resources(&root);

    let edit_args = json!({
        "__principal": "test-principal",
        "__workspace": "test-workspace",
        "operation": "not_a_real_op",
        "input_paths": ["input.mp4"],
    });
    let edit_result = handle_media_edit(Arc::clone(&resources), edit_args)
        .await
        .unwrap();
    assert_eq!(edit_result["status"], "error");
    assert!(edit_result.get("job_id").is_none());

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn status_for_an_unknown_job_id_is_a_clear_error() {
    let root = std::env::temp_dir().join(format!(
        "media_edit_e2e_unknown_job_test_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let resources = test_agent_resources(&root);

    let status_args = json!({"job_id": "not-a-real-job-id"});
    let result = handle_media_edit_status(Arc::clone(&resources), status_args)
        .await
        .unwrap();
    assert_eq!(result["status"], "error");

    let _ = std::fs::remove_dir_all(&root);
}
