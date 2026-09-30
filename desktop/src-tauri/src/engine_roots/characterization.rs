//! Characterization of current desktop engine-path reads.

use crate::config::MagicianDesktopConfig;
use crate::engine_roots::{classify, PathClass, DESKTOP_ENGINE_PATHS};

#[test]
fn every_cataloged_path_has_a_class() {
    for path in DESKTOP_ENGINE_PATHS {
        assert_eq!(classify(path.id), Some(path.class));
        assert!(!path.layout.is_empty());
    }
}

#[test]
fn wake_word_and_os_state_are_device_local() {
    assert_eq!(classify("wake_word_model"), Some(PathClass::DeviceLocal));
    assert_eq!(
        classify("os_permission_state"),
        Some(PathClass::DeviceLocal)
    );
}

#[test]
fn desktop_config_is_bootstrap_not_engine_storage() {
    assert_eq!(
        classify("desktop_application_support"),
        Some(PathClass::Bootstrap)
    );
    let config_path = crate::config::config_file_path();
    assert!(
        !config_path
            .to_string_lossy()
            .contains("MagicianNotes/scopes"),
        "desktop magician.toml must not live under the engine tenant tree"
    );
}

#[test]
fn engine_env_and_tenant_tree_are_engine_owned() {
    assert_eq!(classify("engine_env_file"), Some(PathClass::EngineOwned));
    assert_eq!(classify("engine_tenant_tree"), Some(PathClass::EngineOwned));
}

#[test]
fn default_profile_is_still_local() {
    let config = MagicianDesktopConfig::default();
    assert!(!config.is_remote_engine());
    assert_eq!(config.engine_base_url(), "http://127.0.0.1:3002");
}
