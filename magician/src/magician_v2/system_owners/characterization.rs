//! Characterization of current system, device, and secret layouts.

use super::local::{LocalSystemStore, SystemAccess};
use super::owners::SystemOwner;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[tokio::test]
async fn restart_reopens_each_system_owner_layout() {
    for owner in SystemOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let store = super::open_local_system_owner(&workspace, "alice", "home", owner);
        store
            .put(owner.sample_rel(), b"{\"id\":\"1\"}")
            .await
            .unwrap();
        let reopened = super::open_local_system_owner(&workspace, "alice", "home", owner);
        assert_eq!(
            reopened.get(owner.sample_rel()).await.unwrap(),
            b"{\"id\":\"1\"}"
        );
    }
}

#[tokio::test]
async fn tenant_scopes_do_not_leak() {
    for owner in SystemOwner::TENANT {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let alice = super::open_local_system_owner(&workspace, "alice", "home", owner);
        let bob = super::open_local_system_owner(&workspace, "bob", "home", owner);
        alice.put(owner.sample_rel(), b"secret").await.unwrap();
        assert!(!bob.exists(owner.sample_rel()).await.unwrap());
    }
}

#[tokio::test]
async fn host_pairing_is_shared_at_runtime_root() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let alice =
        super::open_local_system_owner(&workspace, "alice", "home", SystemOwner::DevicePairing);
    let bob = super::open_local_system_owner(&workspace, "bob", "home", SystemOwner::DevicePairing);
    alice
        .put(SystemOwner::DevicePairing.sample_rel(), b"roster")
        .await
        .unwrap();
    assert_eq!(
        bob.get(SystemOwner::DevicePairing.sample_rel())
            .await
            .unwrap(),
        b"roster"
    );
}

#[tokio::test]
async fn secret_export_is_metadata_only() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalSystemStore::for_scope_root(tmp.path(), SystemOwner::SecretVault);
    store
        .put("secrets/secret_audit.jsonl", b"{\"id\":\"k\"}\n")
        .await
        .unwrap();
    store
        .put("secrets/mcp_oauth.vault", b"CLEARTEXT-PASSWORD")
        .await
        .unwrap();
    let dump = store.export_all().await.unwrap();
    let text = String::from_utf8_lossy(&dump);
    assert!(text.contains("secret_audit.jsonl"));
    assert!(!text.contains("CLEARTEXT-PASSWORD"));
}

#[tokio::test]
async fn device_local_is_not_tenant_truth() {
    assert_eq!(
        SystemOwner::claiming("resource_authority/resource_ledger.jsonl"),
        Some(SystemOwner::ResourceAuthority)
    );
    assert_eq!(
        SystemOwner::claiming("device_local/imessage.marker"),
        Some(SystemOwner::DeviceLocalImessage)
    );
    assert!(
        SystemOwner::claiming("secrets/secret_audit.jsonl")
            != Some(SystemOwner::DeviceLocalImessage)
    );
}

#[tokio::test]
async fn each_sample_has_exactly_one_owner() {
    for owner in SystemOwner::ALL {
        assert_eq!(SystemOwner::claiming(owner.sample_rel()), Some(owner));
    }
    assert_eq!(
        SystemOwner::claiming("capability_evolution/capability_packs.json"),
        Some(SystemOwner::CapabilityEvolution)
    );
    assert!(SystemOwner::claiming("capability_evolution/proposals/p-1.json").is_none());
    assert_eq!(
        SystemOwner::claiming("capability_evolution/pack_promotion_audit.jsonl"),
        Some(SystemOwner::CapabilityEvolution)
    );
}

#[tokio::test]
async fn task16b_is_not_this_kit() {
    assert!(SystemOwner::ALL
        .iter()
        .all(|owner| owner.id() != "desktop_engine_roots"));
}

#[tokio::test]
async fn traversal_is_rejected() {
    let store = LocalSystemStore::for_scope_root("/tmp", SystemOwner::Programs);
    assert!(store.put("../escape.json", b"nope").await.is_err());
}
