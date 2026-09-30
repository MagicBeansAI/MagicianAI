use magician_app_contract::APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Exact byte identity of the immutable package/data-plane component contract.
pub const APP_DATA_PLANE_COMPONENT_CONTRACT_SHA256: &str =
    "4c6f6f4edfeaafcc56610fea425cf3df24913ed18605b75c46805e18584e8b2d";

/// Validates both the semantic component version and the bytes bound to its
/// immutable registry revision. A same-version edit must mint a new identity.
pub fn validate_app_data_plane_component_contract(bytes: &[u8]) -> Result<(), String> {
    let actual_sha256 = format!("{:x}", Sha256::digest(bytes));
    if actual_sha256 != APP_DATA_PLANE_COMPONENT_CONTRACT_SHA256 {
        return Err(format!(
            "component contract byte identity mismatch: expected sha256:{}; found \
             sha256:{actual_sha256}",
            APP_DATA_PLANE_COMPONENT_CONTRACT_SHA256
        ));
    }

    let document: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("component contract JSON is invalid: {error}"))?;
    let actual_version = document.get("contract_version").and_then(Value::as_str);
    if actual_version != Some(APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION) {
        return Err(format!(
            "component contract version mismatch: expected {}; found {}",
            APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
            actual_version.unwrap_or("<missing>")
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTRACT_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs/contracts/app-platform/components/v1/contract.json"
    ));

    #[test]
    fn immutable_component_revision_rejects_a_same_version_byte_change() {
        validate_app_data_plane_component_contract(CONTRACT_BYTES)
            .expect("checked component contract must match its immutable identity");

        let mut mutated = CONTRACT_BYTES.to_vec();
        let whitespace = mutated
            .iter()
            .position(|byte| *byte == b' ')
            .expect("fixture contains JSON whitespace");
        mutated[whitespace] = b'\t';

        let error = validate_app_data_plane_component_contract(&mutated)
            .expect_err("same-version byte mutation must be rejected");
        assert!(error.contains("byte identity mismatch"));
    }
}
