//! Remote-engine scenarios: no local engine root, no silent empty reads.

use crate::config::MagicianDesktopConfig;
use crate::engine_roots::{
    apply_engine_location, refuse_engine_owned_filesystem, should_supervise_local_engine,
};

fn remote_config() -> MagicianDesktopConfig {
    let mut config = MagicianDesktopConfig::default();
    config.network.engine_base_url = Some("https://engine.example:8443".into());
    config
}

#[test]
fn remote_engine_skips_local_supervision() {
    let config = remote_config();
    assert!(config.is_remote_engine());
    assert!(!should_supervise_local_engine(&config));
    let local = MagicianDesktopConfig::default();
    assert!(should_supervise_local_engine(&local));
}

#[test]
fn remote_engine_owned_filesystem_fails_explicitly() {
    let _serialized = crate::magician_auth::ENGINE_LOCATION_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let config = remote_config();
    apply_engine_location(&config);
    let err = refuse_engine_owned_filesystem("read").expect_err("remote must fail closed");
    assert!(err.contains("Engine is remote"));
    assert!(!err.is_empty());
    apply_engine_location(&MagicianDesktopConfig::default());
    refuse_engine_owned_filesystem("read").expect("local engine may use the runtime root");
}

#[test]
fn remote_engine_urls_do_not_use_loopback() {
    let config = remote_config();
    let health = config.engine_url("/health");
    let api = config.engine_url("/api/magician/v2/chat/sessions");
    assert!(health.starts_with("https://engine.example:8443/"));
    assert!(!health.contains("127.0.0.1"));
    assert!(!api.contains("localhost"));
    assert_eq!(
        config.engine_ws_path("/api/magician/v2/realtime/ws"),
        "wss://engine.example:8443/api/magician/v2/realtime/ws"
    );
}

#[test]
fn restoring_desktop_bootstrap_does_not_require_engine_root() {
    let path = crate::config::config_file_path();
    assert!(path.ends_with("config/magician.toml"));
    let as_text = path.to_string_lossy();
    assert!(
        !as_text.contains("MagicianNotes/scopes"),
        "desktop bootstrap must not sit in the engine tenant tree"
    );
    assert!(remote_config().is_remote_engine());
}
