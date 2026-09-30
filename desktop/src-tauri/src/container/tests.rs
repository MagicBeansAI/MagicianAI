//! Unit tests for the container runtime abstraction.
//!
//! Provides a `MockRuntime` that implements `ContainerRuntime` for testing
//! orchestration logic without requiring a real container daemon.

use async_trait::async_trait;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::{
    ContainerConfig, ContainerInfo, ContainerRuntime, ContainerStatus, PortMapping,
    ProgressCallback,
};

/// Tracks calls and controls responses for the mock runtime.
#[derive(Debug, Clone)]
struct MockState {
    /// Which methods have been called (name -> call count).
    calls: HashMap<String, usize>,
    /// Simulated container status.
    container_status: ContainerStatus,
    /// Simulated image existence.
    image_exists: bool,
    /// Whether the runtime reports itself as available.
    available: bool,
    /// Simulated image digest.
    local_digest: String,
    /// Simulated remote image digest.
    remote_digest: String,
    /// If set, start() will return this error.
    start_error: Option<String>,
    supervisor_success: bool,
    last_exec: Option<(String, Vec<String>)>,
    exec_responses: VecDeque<Result<String, String>>,
}

/// A mock implementation of `ContainerRuntime` for unit tests.
struct MockRuntime {
    state: Arc<Mutex<MockState>>,
}

impl MockRuntime {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState {
                calls: HashMap::new(),
                container_status: ContainerStatus::NotFound,
                image_exists: false,
                available: true,
                local_digest: "sha256:aaa".to_string(),
                remote_digest: "sha256:aaa".to_string(),
                start_error: None,
                supervisor_success: true,
                last_exec: None,
                exec_responses: VecDeque::new(),
            })),
        }
    }

    fn with_status(self, status: ContainerStatus) -> Self {
        self.state.lock().unwrap().container_status = status;
        self
    }

    fn with_digests(self, local: &str, remote: &str) -> Self {
        {
            let mut s = self.state.lock().unwrap();
            s.local_digest = local.to_string();
            s.remote_digest = remote.to_string();
        }
        self
    }

    fn with_start_error(self, err: &str) -> Self {
        self.state.lock().unwrap().start_error = Some(err.to_string());
        self
    }

    fn call_count(&self, method: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .calls
            .get(method)
            .copied()
            .unwrap_or(0)
    }

    fn record_call(&self, method: &str) {
        let mut s = self.state.lock().unwrap();
        *s.calls.entry(method.to_string()).or_insert(0) += 1;
    }

    /// Simulate a state transition (e.g., after start or stop).
    fn set_status(&self, status: ContainerStatus) {
        self.state.lock().unwrap().container_status = status;
    }
}

#[async_trait]
impl ContainerRuntime for MockRuntime {
    async fn start_existing(&self, _name: &str) -> Result<(), String> {
        self.record_call("start_existing");
        self.set_status(ContainerStatus::Running);
        Ok(())
    }

    fn name(&self) -> &str {
        "MockRuntime"
    }

    async fn is_available(&self) -> bool {
        self.record_call("is_available");
        self.state.lock().unwrap().available
    }

    async fn install(&self, _progress: Option<&ProgressCallback>) -> Result<(), String> {
        self.record_call("install");
        Ok(())
    }

    async fn pull_image(
        &self,
        _image: &str,
        _progress: Option<&ProgressCallback>,
    ) -> Result<(), String> {
        self.record_call("pull_image");
        Ok(())
    }

    async fn image_exists(&self, _image: &str) -> Result<bool, String> {
        self.record_call("image_exists");
        Ok(self.state.lock().unwrap().image_exists)
    }

    async fn start(&self, _config: &ContainerConfig) -> Result<(), String> {
        self.record_call("start");
        let s = self.state.lock().unwrap();
        if let Some(ref err) = s.start_error {
            return Err(err.clone());
        }
        drop(s);
        self.set_status(ContainerStatus::Running);
        Ok(())
    }

    async fn stop(&self, _name: &str) -> Result<(), String> {
        self.record_call("stop");
        self.set_status(ContainerStatus::Stopped);
        Ok(())
    }

    async fn remove(&self, _name: &str) -> Result<(), String> {
        self.record_call("remove");
        self.set_status(ContainerStatus::NotFound);
        Ok(())
    }

    async fn container_info(&self, name: &str) -> Result<ContainerInfo, String> {
        self.record_call("container_info");
        let s = self.state.lock().unwrap();
        Ok(ContainerInfo {
            name: name.to_string(),
            image: "ghcr.io/magicbeanbs100x/magician:latest".to_string(),
            status: s.container_status.clone(),
            ports: vec![
                PortMapping {
                    host: 3002,
                    container: 3002,
                },
                PortMapping {
                    host: 3003,
                    container: 3003,
                },
            ],
            created_at: Some("2026-01-01T00:00:00Z".to_string()),
        })
    }

    async fn logs(&self, _name: &str, _lines: usize) -> Result<String, String> {
        self.record_call("logs");
        Ok("mock log line 1\nmock log line 2\n".to_string())
    }

    async fn exec_in_container(&self, name: &str, command: &[&str]) -> Result<String, String> {
        self.record_call("exec_in_container");
        let mut state = self.state.lock().unwrap();
        state.last_exec = Some((
            name.to_string(),
            command.iter().map(|arg| arg.to_string()).collect(),
        ));
        if let Some(response) = state.exec_responses.pop_front() {
            return response;
        }
        Ok(format!(
            "Response: {{\"success\":{},\"message\":\"fixture response\"}}",
            state.supervisor_success
        ))
    }

    async fn image_digest(&self, _image: &str) -> Result<String, String> {
        self.record_call("image_digest");
        Ok(self.state.lock().unwrap().local_digest.clone())
    }

    async fn remote_image_digest(&self, _image: &str) -> Result<String, String> {
        self.record_call("remote_image_digest");
        Ok(self.state.lock().unwrap().remote_digest.clone())
    }

    async fn tag_image(&self, _image: &str, _new_tag: &str) -> Result<(), String> {
        self.record_call("tag_image");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests: ContainerConfig::from_desktop_config
// ---------------------------------------------------------------------------

#[test]
fn test_container_config_from_desktop_config_defaults() {
    let cfg = crate::config::MagicianDesktopConfig::default();
    let cc = ContainerConfig::from_desktop_config(&cfg);

    assert_eq!(cc.image, "ghcr.io/magicbeanbs100x/magician:latest");
    assert_eq!(cc.name, "magician");
    assert_eq!(cc.ports.len(), 2);
    assert_eq!(cc.ports[0], (3002, 3002));
    assert_eq!(cc.ports[1], (3003, 3003));
    assert_eq!(cc.memory_limit, Some("4g".to_string()));
    assert_eq!(cc.cpu_limit, Some(2.0));
    assert_eq!(cc.env_vars.get("MAGICIAN_ROOT_DIR").unwrap(), "/data");
    assert_eq!(
        cc.env_vars.get("MAGICIAN_HOST_GATEWAY_URL").unwrap(),
        "http://127.0.0.1:3017"
    );
}

#[test]
fn test_seed_runtime_config_if_missing_is_non_clobbering() {
    let dir = std::env::temp_dir().join(format!(
        "magician_container_seed_test_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    super::seed_runtime_config_if_missing(&dir).expect("seed config");
    let seeded = std::fs::read_to_string(dir.join("magician-config.yaml")).unwrap();
    assert!(seeded.contains("consumer_mode:"));
    assert!(seeded.contains("runtime:"));

    let program_path = dir
        .join("scopes")
        .join("anonymous")
        .join("default")
        .join("programs")
        .join("harness_reliability.md");
    let seeded_program = std::fs::read_to_string(&program_path).unwrap();
    assert!(seeded_program.contains("# Harness Reliability"));

    for agent_id in ["harness-sre", "cto", "internal-system-analyst"] {
        let definition_path = dir
            .join("scopes")
            .join("anonymous")
            .join("default")
            .join("agent_runtime")
            .join("agents")
            .join(agent_id)
            .join("definition.agent.yaml");
        let definition = std::fs::read_to_string(&definition_path).unwrap();
        assert!(definition.contains(&format!("agent_id: {agent_id}")));
        assert!(definition.contains("principal: anonymous"));
        assert!(definition.contains("workspace: default"));
    }

    std::fs::write(dir.join("magician-config.yaml"), "custom: true\n").unwrap();
    std::fs::write(&program_path, "custom program\n").unwrap();
    super::seed_runtime_config_if_missing(&dir).expect("preserve config");
    let preserved = std::fs::read_to_string(dir.join("magician-config.yaml")).unwrap();
    assert_eq!(preserved, "custom: true\n");
    let preserved_program = std::fs::read_to_string(&program_path).unwrap();
    assert_eq!(preserved_program, "custom program\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_container_config_with_api_keys() {
    let mut cfg = crate::config::MagicianDesktopConfig::default();
    cfg.api_keys.openai_api_key = "sk-test123".to_string();
    cfg.api_keys.anthropic_api_key = "sk-ant-test456".to_string();

    let cc = ContainerConfig::from_desktop_config(&cfg);

    assert_eq!(cc.env_vars.get("OPENAI_API_KEY").unwrap(), "sk-test123");
    assert_eq!(
        cc.env_vars.get("ANTHROPIC_API_KEY").unwrap(),
        "sk-ant-test456"
    );
    assert!(cc.env_vars.contains_key("MAGICIAN_HOST_GATEWAY_URL"));
}

#[test]
fn test_container_config_can_disable_host_gateway_env() {
    let mut cfg = crate::config::MagicianDesktopConfig::default();
    cfg.host_gateway.enabled = false;

    let cc = ContainerConfig::from_desktop_config(&cfg);

    assert!(!cc.env_vars.contains_key("MAGICIAN_HOST_GATEWAY_URL"));
}

#[test]
fn test_container_config_custom_ports() {
    let mut cfg = crate::config::MagicianDesktopConfig::default();
    cfg.network.magician_port = 4002;
    cfg.network.magicutor_port = 4003;

    let cc = ContainerConfig::from_desktop_config(&cfg);

    assert_eq!(cc.ports[0], (4002, 3002));
    assert_eq!(cc.ports[1], (4003, 3003));
}

#[test]
fn container_routing_host_env_uses_private_guest_loopback_relay() {
    let mut cfg = crate::config::MagicianDesktopConfig::default();
    cfg.host_gateway.runtime_url = "http://127.0.0.1:4017/custom".to_string();
    let mut cc = ContainerConfig::from_desktop_config(&cfg);

    let env =
        super::runtime_env_for_container(&cc, super::APPLE_CONTAINER_HOST).expect("Apple host env");
    assert_eq!(
        env.get("MAGICIAN_CONTAINER_HOST").map(String::as_str),
        Some("host.container.internal")
    );
    assert_eq!(
        env.get("MAGICIAN_HOST_GATEWAY_URL").map(String::as_str),
        Some("http://127.0.0.1:3017")
    );

    cc.env_vars.insert(
        "MAGICIAN_HOST_GATEWAY_URL".to_string(),
        "https://gateway.example.test/host".to_string(),
    );
    let env = super::runtime_env_for_container(&cc, super::DOCKER_CONTAINER_HOST)
        .expect("Docker host env");
    assert_eq!(
        env.get("MAGICIAN_HOST_GATEWAY_URL").map(String::as_str),
        Some("https://gateway.example.test/host")
    );
}

// ---------------------------------------------------------------------------
// Tests: detect_runtime logic
// ---------------------------------------------------------------------------

#[test]
fn test_detect_eligible_requires_macos_aarch64() {
    // This is a compile-time-constant test: just verify the function
    // doesn't panic regardless of platform.
    let _eligible = super::detect::is_apple_container_eligible_for_test();
}

// ---------------------------------------------------------------------------
// Tests: Container state transitions via MockRuntime
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_not_found_to_running_via_start() {
    let rt = MockRuntime::new().with_status(ContainerStatus::NotFound);
    let cfg =
        ContainerConfig::from_desktop_config(&crate::config::MagicianDesktopConfig::default());

    // Container is not found — simulate first-run start
    let info = rt.container_info("magician").await.unwrap();
    assert_eq!(info.status, ContainerStatus::NotFound);

    rt.start(&cfg).await.unwrap();

    let info = rt.container_info("magician").await.unwrap();
    assert_eq!(info.status, ContainerStatus::Running);
    assert_eq!(rt.call_count("start"), 1);
}

#[tokio::test]
async fn test_stopped_to_running_via_remove_and_start() {
    let rt = MockRuntime::new().with_status(ContainerStatus::Stopped);

    let info = rt.container_info("magician").await.unwrap();
    assert_eq!(info.status, ContainerStatus::Stopped);

    // Orchestration: remove old stopped container, then start fresh
    rt.remove("magician").await.unwrap();
    let cfg =
        ContainerConfig::from_desktop_config(&crate::config::MagicianDesktopConfig::default());
    rt.start(&cfg).await.unwrap();

    let info = rt.container_info("magician").await.unwrap();
    assert_eq!(info.status, ContainerStatus::Running);
    assert_eq!(rt.call_count("remove"), 1);
    assert_eq!(rt.call_count("start"), 1);
}

#[tokio::test]
async fn test_running_to_healthy_keeps_running() {
    let rt = MockRuntime::new().with_status(ContainerStatus::Running);

    let info = rt.container_info("magician").await.unwrap();
    assert_eq!(info.status, ContainerStatus::Running);

    // Health check would happen externally; container stays running
    // Just verify we can query status without changing it
    assert_eq!(rt.call_count("start"), 0);
    assert_eq!(rt.call_count("stop"), 0);
}

#[tokio::test]
async fn test_stop_transitions_to_stopped() {
    let rt = MockRuntime::new().with_status(ContainerStatus::Running);

    rt.stop("magician").await.unwrap();

    let info = rt.container_info("magician").await.unwrap();
    assert_eq!(info.status, ContainerStatus::Stopped);
    assert_eq!(rt.call_count("stop"), 1);
}

#[tokio::test]
async fn test_restart_cycle() {
    let rt = MockRuntime::new().with_status(ContainerStatus::Running);
    let cfg =
        ContainerConfig::from_desktop_config(&crate::config::MagicianDesktopConfig::default());

    // Stop
    rt.stop("magician").await.unwrap();
    assert_eq!(
        rt.container_info("magician").await.unwrap().status,
        ContainerStatus::Stopped
    );

    // Remove
    rt.remove("magician").await.unwrap();
    assert_eq!(
        rt.container_info("magician").await.unwrap().status,
        ContainerStatus::NotFound
    );

    // Start
    rt.start(&cfg).await.unwrap();
    assert_eq!(
        rt.container_info("magician").await.unwrap().status,
        ContainerStatus::Running
    );
}

#[tokio::test]
async fn test_start_error_preserves_state() {
    let rt = MockRuntime::new()
        .with_status(ContainerStatus::NotFound)
        .with_start_error("simulated start failure");

    let cfg =
        ContainerConfig::from_desktop_config(&crate::config::MagicianDesktopConfig::default());
    let result = rt.start(&cfg).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("simulated start failure"));
    // Status should remain NotFound since start failed
    assert_eq!(
        rt.container_info("magician").await.unwrap().status,
        ContainerStatus::NotFound
    );
}

// ---------------------------------------------------------------------------
// Tests: Update detection via MockRuntime
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_no_update_when_digests_match() {
    let rt = MockRuntime::new().with_digests("sha256:abc", "sha256:abc");

    let local = rt.image_digest("img").await.unwrap();
    let remote = rt.remote_image_digest("img").await.unwrap();

    assert_eq!(local, remote);
}

#[tokio::test]
async fn test_update_detected_when_digests_differ() {
    let rt = MockRuntime::new().with_digests("sha256:old", "sha256:new");

    let local = rt.image_digest("img").await.unwrap();
    let remote = rt.remote_image_digest("img").await.unwrap();

    assert_ne!(local, remote);
}

// ---------------------------------------------------------------------------
// Tests: tag_image for rollback support
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tag_image_called() {
    let rt = MockRuntime::new();

    rt.tag_image("magician:latest", "magician:previous")
        .await
        .unwrap();

    assert_eq!(rt.call_count("tag_image"), 1);
}

#[tokio::test]
async fn container_routing_restart_preserves_the_existing_instance() {
    for initial in [ContainerStatus::Running, ContainerStatus::Stopped] {
        let rt = MockRuntime::new().with_status(initial.clone());
        rt.restart_existing("integration-test").await.unwrap();
        assert_eq!(rt.call_count("start_existing"), 1);
        assert_eq!(
            rt.call_count("stop"),
            usize::from(initial == ContainerStatus::Running)
        );
        assert_eq!(rt.call_count("remove"), 0);
        assert_eq!(rt.call_count("start"), 0);
    }
    for initial in [ContainerStatus::NotFound, ContainerStatus::Restarting] {
        let rt = MockRuntime::new().with_status(initial);
        assert!(rt.restart_existing("integration-test").await.is_err());
        assert_eq!(rt.call_count("start_existing"), 0);
        assert_eq!(rt.call_count("remove"), 0);
        assert_eq!(rt.call_count("start"), 0);
    }
}

#[tokio::test]
async fn container_routing_services_target_only_the_named_container() {
    for command in [
        "restart-magician",
        "stop-magician",
        "restart-magicutor",
        "stop-magicutor",
    ] {
        let rt = MockRuntime::new().with_status(ContainerStatus::Running);
        super::run_supervisor_command(&rt, "integration-test", command)
            .await
            .unwrap();
        assert_eq!(
            rt.state.lock().unwrap().last_exec,
            Some((
                "integration-test".to_string(),
                vec![
                    "/app/magic-supervisor".to_string(),
                    "client".to_string(),
                    command.to_string()
                ]
            ))
        );
        assert_eq!(rt.call_count("remove"), 0);
        assert_eq!(rt.call_count("stop"), 0);
    }
}

#[tokio::test]
async fn container_routing_service_errors_do_not_fall_back_to_native() {
    let rt = MockRuntime::new().with_status(ContainerStatus::Stopped);
    assert!(
        super::run_supervisor_command(&rt, "integration-test", "restart-magician")
            .await
            .is_err()
    );
    assert_eq!(rt.call_count("exec_in_container"), 0);
    rt.set_status(ContainerStatus::Running);
    assert!(
        super::run_supervisor_command(&rt, "integration-test", "restart-ui-dev")
            .await
            .is_err()
    );
    assert_eq!(rt.call_count("exec_in_container"), 0);
    rt.state.lock().unwrap().supervisor_success = false;
    assert!(
        super::run_supervisor_command(&rt, "integration-test", "restart-magician")
            .await
            .is_err()
    );
    assert_eq!(rt.call_count("exec_in_container"), 1);
}

#[tokio::test]
async fn container_routing_timeout_confirms_state_without_repeating_mutation() {
    for action in ["stop", "restart"] {
        let rt = MockRuntime::new().with_status(ContainerStatus::Running);
        let snapshot = |status: &str, pid: u32| {
            Ok(serde_json::json!({"success": true, "data": {
                "magician": {"status": status, "pid": pid}
            }})
            .to_string())
        };
        {
            let mut state = rt.state.lock().unwrap();
            if action == "restart" {
                state.exec_responses.push_back(snapshot("running", 10));
            }
            state
                .exec_responses
                .push_back(Err("Timed out waiting for supervisor response".into()));
            state.exec_responses.push_back(if action == "restart" {
                snapshot("running", 11)
            } else {
                snapshot("stopped", 0)
            });
        }
        super::run_supervisor_command(&rt, "integration-test", &format!("{action}-magician"))
            .await
            .unwrap();
        assert_eq!(
            rt.call_count("exec_in_container"),
            if action == "restart" { 3 } else { 2 }
        );
        assert_eq!(
            rt.state
                .lock()
                .unwrap()
                .last_exec
                .as_ref()
                .unwrap()
                .1
                .last()
                .unwrap(),
            "status"
        );
    }
}

#[test]
fn container_routing_timeout_requires_observed_service_transition() {
    let before = serde_json::json!({"data": {"magician": {"status": "running", "pid": 10}}});
    assert!(!super::supervisor_command_completed(
        "restart-magician",
        Some(&before),
        &before
    ));
    assert!(!super::supervisor_command_completed(
        "stop-magician",
        None,
        &before
    ));
    assert!(!super::supervisor_command_completed(
        "restart-magician",
        Some(&before),
        &serde_json::json!({})
    ));
    assert!(super::parse_supervisor_response("not json").is_err());
}

/// Opt-in integration lane. Exercises the production adapter and dispatcher on
/// an explicitly named disposable container, never the native supervisor.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "restarts services and the explicitly selected integration container"]
async fn container_routing_live_existing_container_controls() {
    let name = std::env::var("MAGICIAN_DESKTOP_TEST_CONTAINER")
        .expect("set MAGICIAN_DESKTOP_TEST_CONTAINER to a disposable integration container");
    assert!(
        name.starts_with("magician-integration-"),
        "requires an integration container name"
    );
    let rt = super::apple::AppleContainerRuntime::new();
    let before = rt.container_info(&name).await.unwrap();
    assert_eq!(before.status, ContainerStatus::Running);
    let port = before
        .ports
        .iter()
        .find(|p| p.container == 3002)
        .unwrap()
        .host;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let health = format!("http://127.0.0.1:{port}/health");
    let status_command = ["/app/magic-supervisor", "client", "status"];
    let status = |text: String| -> serde_json::Value {
        serde_json::from_str(text.trim().strip_prefix("Response:").unwrap().trim()).unwrap()
    };
    let initial = status(rt.exec_in_container(&name, &status_command).await.unwrap());
    let outcome: Result<(), String> = async {
        for service in ["magician", "magicutor"] {
            super::run_supervisor_command(&rt, &name, &format!("stop-{service}")).await?;
            let stopped = status(rt.exec_in_container(&name, &status_command).await?);
            assert_eq!(stopped["data"][service]["status"], "stopped");
            super::run_supervisor_command(&rt, &name, &format!("restart-{service}")).await?;
        }
        let after_services = status(rt.exec_in_container(&name, &status_command).await?);
        for service in ["magician", "magicutor"] {
            assert_ne!(
                initial["data"][service]["pid"],
                after_services["data"][service]["pid"]
            );
            assert_eq!(after_services["data"][service]["status"], "running");
        }
        rt.restart_existing(&name).await?;
        Ok(())
    }
    .await;
    // Best-effort recovery even when a stop/start test fails partway through.
    if outcome.is_err() {
        if matches!(
            rt.container_info(&name).await.map(|i| i.status),
            Ok(ContainerStatus::Stopped)
        ) {
            let _ = rt.start_existing(&name).await;
        }
        for command in ["restart-magician", "restart-magicutor"] {
            let _ = super::run_supervisor_command(&rt, &name, command).await;
        }
    }
    outcome.unwrap();
    let after = rt.container_info(&name).await.unwrap();
    assert_eq!(after.status, ContainerStatus::Running);
    assert_eq!(
        before.created_at, after.created_at,
        "restart must not replace the container"
    );
    assert_eq!(before.image, after.image);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        if let Ok(response) = client.get(&health).send().await {
            if response.status().is_success() {
                if let Ok(body) = response.json::<serde_json::Value>().await {
                    if body["magician"] == "healthy" && body["magicutor_status"] == "healthy" {
                        break;
                    }
                }
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "container health did not recover"
        );
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
