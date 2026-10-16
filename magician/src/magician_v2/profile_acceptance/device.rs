//! Device-bound surfaces are not server storage. Remote profiles must fail
//! closed instead of reading a same-path local fallback.

#[cfg(test)]
use crate::magician_v2::track_a_acceptance::load_support_matrix;

pub const DEVICE_BRIDGE_REQUIRED: &str = "device-bridge-required";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceBoundSurface {
    Browser,
    Screen,
    Meeting,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceAvailability {
    Unavailable { reason: &'static str },
}

pub fn device_bound_availability(_surface: DeviceBoundSurface) -> DeviceAvailability {
    DeviceAvailability::Unavailable {
        reason: DEVICE_BRIDGE_REQUIRED,
    }
}

/// Support-matrix ids of the device-local owners the acceptance scenarios
/// assert are unavailable under `remote_durable`. Test-only: the scenarios
/// module is the sole caller.
#[cfg(test)]
pub fn device_local_catalog_ids() -> [&'static str; 4] {
    [
        "notes_provider",
        "desktop_engine_roots",
        "device_local_imessage",
        "device_local_whatsapp",
    ]
}

#[cfg(test)]
pub fn assert_device_local_matrix_unavailable() -> anyhow::Result<()> {
    let matrix = load_support_matrix()?;
    for id in device_local_catalog_ids() {
        let entry = matrix
            .owners
            .iter()
            .find(|row| row.id == id)
            .ok_or_else(|| anyhow::anyhow!("missing matrix entry {id}"))?;
        anyhow::ensure!(
            matches!(
                entry.remote_outcome,
                crate::magician_v2::track_a_acceptance::RemoteOutcome::Unavailable
            ),
            "{id} must be unavailable under remote_durable"
        );
        anyhow::ensure!(
            entry
                .degraded_behavior
                .to_ascii_lowercase()
                .contains("not migrated")
                || entry
                    .degraded_behavior
                    .to_ascii_lowercase()
                    .contains("device"),
            "{id} must explain device-local unavailability"
        );
    }
    Ok(())
}
