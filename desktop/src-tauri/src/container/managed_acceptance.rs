//! Opt-in acceptance of the actual desktop adapter and a packaged Linux backend.
//! Uses a new private root, no published ports or provider credentials, and only
//! synthetic identities. Keeps stopped fixtures/evidence for inspection.

use super::{apple::AppleContainerRuntime, ContainerConfig, ContainerRuntime, ContainerStatus};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

async fn guest(
    runtime: &AppleContainerRuntime,
    name: &str,
    args: &[&str],
) -> Result<String, String> {
    tokio::time::timeout(
        Duration::from_secs(80),
        runtime.exec_in_container(name, args),
    )
    .await
    .map_err(|_| "Acceptance guest command timed out".to_string())?
}

async fn ready(runtime: &AppleContainerRuntime, name: &str) -> Result<(), String> {
    guest(
        runtime,
        name,
        &[
            "python3",
            "-c",
            r#"
import json,time,urllib.request
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
deadline=time.monotonic()+60
while True:
 try:
  with opener.open('http://127.0.0.1:3002/api/magician/v2/devices',timeout=2) as response:
   assert json.load(response)['pairing_available'], 'pairing authority unavailable'
  break
 except OSError:
  if time.monotonic()>=deadline: raise
  time.sleep(.5)
print('PASS packaged backend pairing authority ready')
"#,
        ],
    )
    .await
    .map(|_| ())
}

async fn protocol(runtime: &AppleContainerRuntime, name: &str, phase: &str) -> Result<(), String> {
    let output = guest(
        runtime,
        name,
        &[
            "python3",
            "/data/acceptance/qualify-mobile-connectivity.py",
            phase,
            "--origin",
            "http://127.0.0.1:3002",
            "--state-file",
            "/data/acceptance/protocol-state.json",
        ],
    )
    .await?;
    print!("{output}");
    Ok(())
}

async fn roster(runtime: &AppleContainerRuntime, name: &str) -> Result<Value, String> {
    let output = guest(runtime, name, &["python3", "-c", r#"
import json,urllib.request
with urllib.request.urlopen('http://127.0.0.1:3002/api/magician/v2/devices',timeout=5) as response:
 devices=json.load(response)['devices']
print(json.dumps(sorted([{k:d.get(k) for k in ('device_id','principal','workspace','client_kind','capabilities','paired_at_ms')} for d in devices],key=lambda d:d['device_id'])))
"#]).await?;
    serde_json::from_str(&output).map_err(|_| "Invalid roster metadata".to_string())
}

struct RestoreSecret(PathBuf, Vec<u8>);
impl Drop for RestoreSecret {
    fn drop(&mut self) {
        // Only the fresh acceptance fixture's custody is modified here.
        let _ = std::fs::write(&self.0, &self.1);
    }
}

const ACCEPTANCE_QUERY_ANALYSIS_PROFILE: &str = "op-app-workflow-local";

fn pin_acceptance_query_analysis(router_path: &Path) -> Result<(), String> {
    const CLOUD_BINDING: &str =
        "      query_analysis:\n        default: gpt6luna-responses-toolsany\n";
    let local_binding =
        format!("      query_analysis:\n        default: {ACCEPTANCE_QUERY_ANALYSIS_PROFILE}\n");
    let router = std::fs::read_to_string(router_path)
        .map_err(|error| format!("Cannot read acceptance router: {error}"))?;
    if router.matches(CLOUD_BINDING).count() != 1 {
        return Err("Packaged query_analysis binding changed; update the acceptance pin".into());
    }
    std::fs::write(
        router_path,
        router.replacen(CLOUD_BINDING, &local_binding, 1),
    )
    .map_err(|error| format!("Cannot pin acceptance query analysis: {error}"))
}

async fn qualify(
    runtime: &AppleContainerRuntime,
    config: &ContainerConfig,
    root: &Path,
) -> Result<Value, String> {
    runtime.start(config).await?;
    ready(runtime, &config.name).await?;
    protocol(runtime, &config.name, "enroll").await?;
    let original_roster = roster(runtime, &config.name).await?;
    if original_roster.as_array().map(Vec::len) != Some(3) {
        return Err("Expected exactly three synthetic identities".into());
    }
    let first = runtime.container_info(&config.name).await?;
    let boot_command = ["cat", "/proc/sys/kernel/random/boot_id"];
    let original_boot = guest(runtime, &config.name, &boot_command).await?;
    let mounts = runtime.prepare_keyring(config).await?;
    let secret = mounts
        .iter()
        .find_map(|arg| arg.strip_suffix(":/run/secrets/magician-keyring-password:ro"))
        .ok_or("Managed secret mount missing")?;
    let secret = PathBuf::from(secret);
    if !secret.starts_with(root.join("custody")) {
        return Err("Acceptance custody escaped the isolated root".into());
    }
    let original = std::fs::read(&secret).map_err(|_| "Cannot read acceptance custody")?;
    {
        let restore = RestoreSecret(secret.clone(), original.clone());
        std::fs::write(&secret, b"wrong-private-acceptance-secret-1234567890")
            .map_err(|_| "Cannot write acceptance secret")?;
        let refused = runtime.prepare_keyring(config).await;
        drop(restore);
        if !refused.is_err_and(|error| error.contains("secret changed")) {
            return Err("Changed secret was not refused before replacement".into());
        }
    }
    let after_refusal = runtime.container_info(&config.name).await?;
    if after_refusal.status != ContainerStatus::Running
        || after_refusal.created_at != first.created_at
        || guest(runtime, &config.name, &boot_command).await? != original_boot
    {
        return Err("Refused preflight changed the running fixture".into());
    }
    protocol(runtime, &config.name, "verify").await?;
    // The same sequence used by managed recreation: preflight before stop/remove,
    // followed by start, which independently rechecks custody before creation.
    runtime.prepare_keyring(config).await?;
    runtime.stop(&config.name).await?;
    runtime.remove(&config.name).await?;
    runtime.start(config).await?;
    ready(runtime, &config.name).await?;
    protocol(runtime, &config.name, "verify").await?;
    if roster(runtime, &config.name).await? != original_roster {
        return Err("Pairing metadata changed across managed recreation".into());
    }
    let second = runtime.container_info(&config.name).await?;
    if second.created_at.is_none() || second.created_at == first.created_at {
        return Err("Container was not recreated".into());
    }
    if std::fs::read(&secret).map_err(|_| "Secret disappeared")? != original {
        return Err("Recreation changed the unlock secret".into());
    }
    protocol(runtime, &config.name, "revoke").await?;
    runtime.prepare_keyring(config).await?;
    runtime.stop(&config.name).await?;
    runtime.remove(&config.name).await?;
    runtime.start(config).await?;
    ready(runtime, &config.name).await?;
    protocol(runtime, &config.name, "verify-revoked").await?;
    if roster(runtime, &config.name).await? != json!([]) {
        return Err("Revoked identities returned after recreation".into());
    }
    if std::fs::read(config.data_dir.join("acceptance/sentinel")).map_err(|_| "Sentinel lost")?
        != b"managed-container-acceptance\n"
    {
        return Err("Runtime file changed across recreation".into());
    }
    Ok(json!({
        "image": config.image, "container": config.name, "runtime_root": config.data_dir,
        "cpus": config.cpu_limit, "memory": config.memory_limit, "published_ports": [],
        "adapter": "desktop::container::apple::AppleContainerRuntime",
        "first_creation": first.created_at, "second_creation": second.created_at,
        "fresh_automatic_custody": true, "pairings_survived_recreation": true,
        "revocation_survived_recreation": true, "wrong_secret_refused_before_stop": true,
        "runtime_file_preserved": true, "same_unlock_secret": true,
        "query_analysis_profile": ACCEPTANCE_QUERY_ANALYSIS_PROFILE,
        "hardware_acceptance": false, "installer_qualified": false,
    }))
}

#[tokio::test]
#[ignore = "starts an isolated 1 CPU/3 GiB backend; requires explicit image and new SSD test root"]
async fn managed_container_preserves_device_credentials() {
    let root = PathBuf::from(
        std::env::var("MAGICIAN_MANAGED_CONTAINER_TEST_ROOT")
            .expect("set an unused absolute test root"),
    );
    assert!(root.is_absolute(), "test root must be absolute");
    let image = std::env::var("MAGICIAN_MANAGED_CONTAINER_TEST_IMAGE")
        .expect("set a locally available image");
    assert_eq!(
        std::env::var_os("MAGICIAN_CONTAINER_KEYRING_HOME"),
        Some(root.join("custody").into_os_string()),
        "custody must be isolated beside the test runtime"
    );
    let runtime = AppleContainerRuntime::new();
    assert!(
        runtime.is_available().await,
        "start Apple Container separately"
    );
    assert!(
        runtime.image_exists(&image).await.unwrap(),
        "candidate image must already exist; this test never builds/pulls"
    );
    let cli = super::apple::find_container_cli_path();
    let dns = tokio::process::Command::new(cli)
        .args(["system", "dns", "list", "--quiet"])
        .output()
        .await
        .unwrap();
    assert!(
        dns.status.success()
            && String::from_utf8_lossy(&dns.stdout)
                .lines()
                .any(|line| line.trim() == super::APPLE_CONTAINER_HOST),
        "configure host forwarding separately; acceptance never prompts for admin rights"
    );
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("test root must not exist");
    let data = root.join("runtime");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&data)
        .unwrap();
    super::seed_runtime_config_if_missing(&data).unwrap();
    // This isolated lane deliberately supplies no cloud credentials. Keep its
    // mandatory startup analysis on the packaged host-Ollama route so the test
    // qualifies container transport and custody rather than an API account.
    pin_acceptance_query_analysis(&data.join("llm-router.yaml")).unwrap();
    let config_path = data.join("magician-config.yaml");
    let config_text = std::fs::read_to_string(&config_path).unwrap();
    assert!(config_text.contains("  public_origin: null"));
    std::fs::write(
        &config_path,
        config_text.replacen(
            "  public_origin: null",
            "  public_origin: http://127.0.0.1:3002",
            1,
        ),
    )
    .unwrap();
    let helpers = data.join("acceptance");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&helpers)
        .unwrap();
    std::fs::write(
        helpers.join("qualify-mobile-connectivity.py"),
        include_bytes!("../../../../scripts/qualify-mobile-connectivity.py"),
    )
    .unwrap();
    std::fs::write(
        helpers.join("qualify-container-integration.py"),
        include_bytes!("../../../../scripts/qualify-container-integration.py"),
    )
    .unwrap();
    std::fs::write(helpers.join("sentinel"), b"managed-container-acceptance\n").unwrap();
    let config = ContainerConfig {
        name: format!(
            "magician-managed-acceptance-{}",
            uuid::Uuid::new_v4().simple()
        ),
        image,
        config_dir: data.join("config"),
        log_dir: data.join("logs"),
        data_dir: data,
        ports: vec![],
        cpu_limit: Some(1.0),
        memory_limit: Some("3g".into()),
        env_vars: HashMap::from([("MAGICIAN_ROOT_DIR".into(), "/data".into())]),
    };
    let outcome =
        tokio::time::timeout(Duration::from_secs(600), qualify(&runtime, &config, &root)).await;
    // Stop only our UUID-named fixture on either success or failure; retain its
    // data and logs. Never run the installer or stop native/shared services here.
    let stopped = match runtime.container_info(&config.name).await {
        Ok(info) if info.status == ContainerStatus::Running => {
            tokio::time::timeout(Duration::from_secs(60), runtime.stop(&config.name)).await
        },
        Ok(info) if info.status == ContainerStatus::Stopped => Ok(Ok(())),
        Ok(_) => Ok(Err("Acceptance fixture disappeared before cleanup".into())),
        Err(error) => Ok(Err(error)),
    };
    if let Ok(logs) = runtime.logs(&config.name, 1000).await {
        std::fs::write(root.join("container.log"), logs).unwrap();
    }
    let mut report = match outcome {
        Ok(Ok(report)) => report,
        Ok(Err(error)) => json!({"status": "failed", "error": error, "container": config.name}),
        Err(_) => {
            json!({"status": "failed", "error": "acceptance timed out", "container": config.name})
        },
    };
    let success = report.get("error").is_none() && matches!(stopped, Ok(Ok(())));
    report["status"] = json!(if success { "passed" } else { "failed" });
    report["fixture_stopped"] = json!(matches!(stopped, Ok(Ok(()))));
    std::fs::write(
        root.join("acceptance.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    assert!(
        success,
        "Managed backend acceptance failed; inspect {}",
        root.display()
    );
    println!(
        "PASS managed backend acceptance; evidence: {}",
        root.display()
    );
}
