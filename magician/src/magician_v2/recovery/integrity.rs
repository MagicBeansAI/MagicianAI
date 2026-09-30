//! Reference-integrity walker for restored manifests.
//!
//! Restore validation walks object references and dataset parts, then checks
//! existence, version, and digest before mutations are served.

#[cfg(test)]
use magician_storage::{ObjectVersion, StorageKey};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferencedObject {
    pub key: String,
    pub version: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryManifest {
    pub objects: Vec<ReferencedObject>,
    pub dataset_parts: Vec<String>,
    pub index_watermarks: Vec<String>,
    pub device_records: Vec<String>,
    pub secret_refs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntegrityReport {
    pub missing: Vec<String>,
}

impl RecoveryManifest {
    pub fn walk(
        &self,
        present_objects: &[ReferencedObject],
        present_parts: &[String],
        present_devices: &[String],
        present_secret_refs: &[String],
    ) -> IntegrityReport {
        let mut missing = Vec::new();
        for object in &self.objects {
            let found = present_objects.iter().any(|have| {
                have.key == object.key
                    && have.version == object.version
                    && have.digest == object.digest
            });
            if !found {
                missing.push(format!(
                    "object:{}@{}#{}",
                    object.key, object.version, object.digest
                ));
            }
        }
        for part in &self.dataset_parts {
            if !present_parts.iter().any(|have| have == part) {
                missing.push(format!("dataset:{part}"));
            }
        }
        for device in &self.device_records {
            if !present_devices.iter().any(|have| have == device) {
                missing.push(format!("device:{device}"));
            }
        }
        for secret in &self.secret_refs {
            if !present_secret_refs.iter().any(|have| have == secret) {
                missing.push(format!("secret:{secret}"));
            }
        }
        IntegrityReport { missing }
    }
}

/// Typed constructor for drill manifests. Production walkers build
/// [`ReferencedObject`] from exported records (string keys/versions), so this
/// helper only exists for the recovery drills.
#[cfg(test)]
pub fn referenced_object(
    key: &StorageKey,
    version: &ObjectVersion,
    digest_hex: &str,
) -> ReferencedObject {
    ReferencedObject {
        key: key.encode(),
        version: version.as_str().to_string(),
        digest: digest_hex.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walker_reports_version_and_digest_mismatch() {
        let manifest = RecoveryManifest {
            objects: vec![ReferencedObject {
                key: "t/alice/home/tasks/blob".into(),
                version: "v1".into(),
                digest: "abc".into(),
            }],
            dataset_parts: vec!["events/dt=2026-08-31/batch.parquet".into()],
            index_watermarks: vec![],
            device_records: vec!["system/paired-devices.json".into()],
            secret_refs: vec!["secret_audit.jsonl".into()],
        };
        let wrong = manifest.walk(
            &[ReferencedObject {
                key: "t/alice/home/tasks/blob".into(),
                version: "v1".into(),
                digest: "other".into(),
            }],
            &["events/dt=2026-08-31/batch.parquet".into()],
            &["system/paired-devices.json".into()],
            &["secret_audit.jsonl".into()],
        );
        assert_eq!(wrong.missing.len(), 1);
        assert!(wrong.missing[0].starts_with("object:"));
        let ok = manifest.walk(
            &[ReferencedObject {
                key: "t/alice/home/tasks/blob".into(),
                version: "v1".into(),
                digest: "abc".into(),
            }],
            &["events/dt=2026-08-31/batch.parquet".into()],
            &["system/paired-devices.json".into()],
            &["secret_audit.jsonl".into()],
        );
        assert!(ok.missing.is_empty());
    }
}
