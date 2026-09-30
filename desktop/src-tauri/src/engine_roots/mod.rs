//! Task 16B desktop engine-path contract.
//!
//! Desktop must not resolve the engine runtime root in order to read
//! engine-owned data. Device-local and bootstrap paths stay on this machine.
//! Remote adapters are classification + transport, not a second canonical store.

mod guard;

use crate::config::MagicianDesktopConfig;

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;

/// Task 16B path catalogue. Consumed only by the `characterization` and
/// `storage_packet` test modules above, which is the whole point of it — it is
/// the written-down classification those tests assert the desktop still obeys,
/// not something production reads. Gated so the non-test build does not report
/// it as dead code once `make check-all` covers this crate.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathClass {
    DeviceLocal,
    EngineOwned,
    Bootstrap,
    LocalEngineVolume,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopEnginePath {
    pub id: &'static str,
    pub class: PathClass,
    pub layout: &'static str,
}

#[cfg(test)]
pub const DESKTOP_ENGINE_PATHS: [DesktopEnginePath; 8] = [
    DesktopEnginePath {
        id: "wake_word_model",
        class: PathClass::DeviceLocal,
        layout: "MAGICIAN_VOSK_MODEL_DIR, bundled vosk-model, exe-dir vosk-model; local-engine may colocate under runtime root",
    },
    DesktopEnginePath {
        id: "os_permission_state",
        class: PathClass::DeviceLocal,
        layout: "macOS TCC / accessibility; never engine storage",
    },
    DesktopEnginePath {
        id: "desktop_application_support",
        class: PathClass::Bootstrap,
        layout: "~/Library/Application Support/dev.magician.desktop (magician.toml, logs)",
    },
    DesktopEnginePath {
        id: "desktop_env_file_override",
        class: PathClass::Bootstrap,
        layout: "MAGICIAN_DESKTOP_ENV_FILE",
    },
    DesktopEnginePath {
        id: "engine_env_file",
        class: PathClass::EngineOwned,
        layout: "<MAGICIAN_ROOT_DIR>/.env or .env.development",
    },
    DesktopEnginePath {
        id: "engine_tenant_tree",
        class: PathClass::EngineOwned,
        layout: "<MAGICIAN_ROOT_DIR>/scopes/... engine canonical storage",
    },
    DesktopEnginePath {
        id: "local_container_volume",
        class: PathClass::LocalEngineVolume,
        layout: "runtime_root_dir bind-mounted at /data when supervising a local container",
    },
    DesktopEnginePath {
        id: "notes_space_placeholder",
        class: PathClass::Bootstrap,
        layout: "Settings display $MAGICIAN_ROOT_DIR/MagicanNotes; not a desktop reader of engine bytes",
    },
];

#[cfg(test)]
pub fn apply_engine_location(config: &MagicianDesktopConfig) {
    crate::magician_auth::apply_engine_location(
        config.is_remote_engine(),
        &config.engine_base_url(),
    );
}

pub fn is_remote_engine_process() -> bool {
    crate::magician_auth::is_remote_engine_process()
}

pub fn refuse_engine_owned_filesystem(purpose: &str) -> Result<(), String> {
    if is_remote_engine_process() {
        Err(format!(
            "Engine is remote; this desktop cannot {purpose} engine-owned files at MAGICIAN_ROOT_DIR. Use the engine API."
        ))
    } else {
        Ok(())
    }
}

pub fn should_supervise_local_engine(config: &MagicianDesktopConfig) -> bool {
    !config.is_remote_engine()
}

#[cfg(test)]
pub fn classify(id: &str) -> Option<PathClass> {
    DESKTOP_ENGINE_PATHS
        .iter()
        .find(|path| path.id == id)
        .map(|path| path.class)
}
