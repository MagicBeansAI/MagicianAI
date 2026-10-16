use std::process::Command;
use std::sync::{Arc, Mutex};

use magician_storage::lease::OwnerId;
use magician_storage::profile::ResolveOptions;
use magician_storage::{PrincipalId, ScopeId, StorageError, StorageRuntime, WorkspaceId};

fn local_profile() -> magician_storage::ResolvedStorageProfile {
    magician_storage::resolve(ResolveOptions {
        cli_path: None,
        env_path: None,
        allow_ambient: false,
    })
    .unwrap()
}

fn owner() -> OwnerId {
    OwnerId::parse("test-owner").unwrap()
}

fn scope(principal: &str, workspace: &str) -> ScopeId {
    ScopeId::new(
        PrincipalId::parse(principal).unwrap(),
        WorkspaceId::parse(workspace).unwrap(),
    )
}

#[test]
fn install_exposes_typed_capabilities_to_libraries() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        Arc::new(StorageRuntime::open_local(dir.path(), local_profile(), owner()).unwrap());
    StorageRuntime::install(Arc::clone(&runtime));
    let current = StorageRuntime::require().unwrap();
    assert_eq!(current.root, runtime.root);
    assert_eq!(current.operator_health().profile, "local_embedded");
}

#[tokio::test]
async fn two_runtimes_are_independent() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a = StorageRuntime::open_local(a_dir.path(), local_profile(), owner()).unwrap();
    let b = StorageRuntime::open_local(b_dir.path(), local_profile(), owner()).unwrap();
    let scope = scope("anonymous", "default");
    let token_a = a.scope_leases.acquire(&scope).await.unwrap();
    let token_b = b.scope_leases.acquire(&scope).await.unwrap();
    assert_eq!(token_a.generation, 1);
    assert_eq!(token_b.generation, 1);
    assert_ne!(a.root, b.root);
    let health = a.operator_health();
    assert_eq!(health.profile, "local_embedded");
    assert_eq!(health.source, "missing_default");
    assert!(!health.lease_lost);
    assert!(health
        .capabilities
        .iter()
        .any(|row| row.capability == "profile"));
}

#[tokio::test]
async fn remote_profile_fails_closed_without_adapters() {
    let dir = tempfile::tempdir().unwrap();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/remote_durable.yaml");
    let profile = magician_storage::resolve(ResolveOptions {
        cli_path: Some(&path),
        env_path: None,
        allow_ambient: false,
    })
    .unwrap();
    let err = match StorageRuntime::open_local(dir.path(), profile, owner()) {
        Ok(_) => panic!("remote profile must not open local adapters"),
        Err(err) => err,
    };
    assert!(matches!(err, StorageError::UnsupportedCapability));
}

#[tokio::test]
async fn distinct_scopes_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = StorageRuntime::open_local(dir.path(), local_profile(), owner()).unwrap();
    let a = runtime
        .scope_leases
        .acquire(&scope("anonymous", "default"))
        .await
        .unwrap();
    let b = runtime
        .scope_leases
        .acquire(&scope("alice", "home"))
        .await
        .unwrap();
    assert_eq!(a.generation, 1);
    assert_eq!(b.generation, 1);
    assert_eq!(runtime.scope_leases.held_scopes().len(), 2);
}

#[tokio::test]
async fn same_process_reacquire_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = StorageRuntime::open_local(dir.path(), local_profile(), owner()).unwrap();
    let scope = scope("anonymous", "default");
    let first = runtime.scope_leases.acquire(&scope).await.unwrap();
    let second = runtime.scope_leases.acquire(&scope).await.unwrap();
    assert_eq!(first.generation, second.generation);
}

#[tokio::test]
async fn process_exit_reacquires_higher_generation() {
    let scope = scope("anonymous", "default");
    if let Ok(root) = std::env::var("MAGICIAN_SCOPE_EXIT_ROOT") {
        let runtime = StorageRuntime::open_local(root, local_profile(), owner()).unwrap();
        let token = runtime.scope_leases.acquire(&scope).await.unwrap();
        assert_eq!(token.generation, 1);
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .env("MAGICIAN_SCOPE_EXIT_ROOT", dir.path())
        .args(["process_exit_reacquires_higher_generation", "--exact"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runtime = StorageRuntime::open_local(dir.path(), local_profile(), owner()).unwrap();
    let token = runtime.scope_leases.acquire(&scope).await.unwrap();
    assert_eq!(token.generation, 2);
}

#[tokio::test]
async fn stale_generation_renew_is_refused_and_requests_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = StorageRuntime::open_local(dir.path(), local_profile(), owner()).unwrap();
    let scope = scope("anonymous", "default");
    runtime.scope_leases.acquire(&scope).await.unwrap();
    let lost = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&lost);
    runtime.scope_leases.set_on_loss(Arc::new(move |_err| {
        *flag.lock().unwrap() = true;
    }));
    let record = dir
        .path()
        .join("leases")
        .join("scope.anonymous.default.lease.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    value["generation"] = serde_json::json!(99);
    std::fs::write(&record, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let err = runtime.scope_leases.renew_all().await.unwrap_err();
    assert!(matches!(
        err,
        StorageError::LeaseLost { generation: 99, .. }
    ));
    assert!(runtime.scope_leases.lease_lost());
    assert!(*lost.lock().unwrap());
    let health = runtime.operator_health();
    assert!(health.lease_lost);
}

#[tokio::test]
async fn two_process_same_scope_is_refused() {
    let scope = scope("anonymous", "default");
    if let Ok(root) = std::env::var("MAGICIAN_SCOPE_CHILD_ROOT") {
        let runtime = StorageRuntime::open_local(root, local_profile(), owner()).unwrap();
        let err = runtime.scope_leases.acquire(&scope).await.unwrap_err();
        assert!(matches!(err, StorageError::Conflict { .. }));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let runtime = StorageRuntime::open_local(dir.path(), local_profile(), owner()).unwrap();
    runtime.scope_leases.acquire(&scope).await.unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .env("MAGICIAN_SCOPE_CHILD_ROOT", dir.path())
        .args(["two_process_same_scope_is_refused", "--exact"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}
