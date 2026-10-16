//! The oldest Magician build allowed to write a runtime root, recorded in the
//! root itself.
//!
//! This replaces the per-workspace "legacy-writer cutover" seal. The seal asked
//! an operator to attest, workspace by workspace and forever, that no binary
//! predating the reverse-binding protocol (0.7.4) could still write it. That is
//! a fact about the *deployment*, not the workspace, so it lives here once:
//!
//! * [`MINIMUM_RUNTIME_WRITER_VERSION`] is the oldest build whose writes the
//!   current data formats can trust. 0.7.4 publishes reverse bindings; 0.7.7
//!   also takes the exclusive default-scope lease at startup, so two servers can
//!   no longer write one root at once.
//! * Every serving start records `max(recorded floor, this build's floor)` in
//!   `.magician-storage/runtime-writer-floor.json`, so the floor only rises.
//! * Any build older than the recorded floor refuses to start against the root
//!   — a rollback below a data-format boundary fails loudly instead of silently
//!   writing data the newer build would then trust.
//!
//! A future format change that older builds must not write raises the
//! constant. No per-workspace step, and nothing for a fresh install to do.
//!
//! Builds that predate this file cannot read it, so it cannot stop them; it
//! stops every build from this one on. Pre-0.7.7 builds were never released.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

/// Oldest build whose writes this build's data formats trust.
pub const MINIMUM_RUNTIME_WRITER_VERSION: &str = "0.7.7";

const FLOOR_FILE_NAME: &str = "runtime-writer-floor.json";
const FLOOR_SCHEMA_VERSION: u8 = 1;
const MAX_FLOOR_FILE_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RuntimeWriterFloorRecord {
    schema_version: u8,
    minimum_writer_version: String,
    last_writer_version: String,
    updated_at_ms: i64,
}

/// Why this build may not write the runtime root.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeWriterFloorError {
    #[error(
        "runtime root {root} requires magician >= {required} (last written by {last_writer}); \
         this binary is {running}. Running an older build against newer data is unsupported: \
         upgrade magician instead of downgrading it"
    )]
    BelowFloor {
        root: String,
        required: String,
        running: String,
        last_writer: String,
    },
    #[error("runtime writer floor at {path}: {detail}")]
    Io { path: String, detail: String },
}

/// What [`enforce_runtime_writer_floor`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWriterFloorOutcome {
    pub minimum_writer_version: String,
    pub running_version: String,
    /// The build that last recorded the floor, when one had.
    pub previous_writer_version: Option<String>,
}

/// The version this build reports for the floor.
pub fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn runtime_writer_floor_path(runtime_root: &Path) -> PathBuf {
    runtime_root
        .join(magician_storage::LOCAL_ADAPTER_DIR)
        .join(FLOOR_FILE_NAME)
}

/// Refuse to continue when this build is older than the floor recorded in
/// `runtime_root`; with `record`, raise the recorded floor to this build's own
/// and stamp it as the last writer. Serving starts record; one-shot CLI
/// commands only check.
pub fn enforce_runtime_writer_floor(
    runtime_root: &Path,
    record: bool,
) -> Result<RuntimeWriterFloorOutcome, RuntimeWriterFloorError> {
    enforce_with_version(runtime_root, record, running_version())
}

fn enforce_with_version(
    runtime_root: &Path,
    record: bool,
    running: &str,
) -> Result<RuntimeWriterFloorOutcome, RuntimeWriterFloorError> {
    let path = runtime_writer_floor_path(runtime_root);
    let running_semver = parse_version(running).ok_or_else(|| RuntimeWriterFloorError::Io {
        path: path.display().to_string(),
        detail: format!("this build's version {running:?} is not semver"),
    })?;
    let builtin_floor = parse_version(MINIMUM_RUNTIME_WRITER_VERSION)
        .expect("MINIMUM_RUNTIME_WRITER_VERSION is semver");

    let existing = read_record(&path)?;
    let recorded_floor = existing
        .as_ref()
        .and_then(|record| parse_version(&record.minimum_writer_version));

    if let (Some(existing), Some(recorded_floor)) = (existing.as_ref(), recorded_floor.as_ref()) {
        if running_semver < *recorded_floor {
            let refusal = RuntimeWriterFloorError::BelowFloor {
                root: runtime_root.display().to_string(),
                required: existing.minimum_writer_version.clone(),
                running: running.to_owned(),
                last_writer: existing.last_writer_version.clone(),
            };
            error!(
                root = %runtime_root.display(),
                required = %existing.minimum_writer_version,
                running = %running,
                last_writer = %existing.last_writer_version,
                "[RUNTIME-WRITER-FLOOR] refusing to write a runtime root that requires a newer magician"
            );
            return Err(refusal);
        }
        if parse_version(&existing.last_writer_version).is_some_and(|last| running_semver < last) {
            warn!(
                root = %runtime_root.display(),
                running = %running,
                last_writer = %existing.last_writer_version,
                floor = %existing.minimum_writer_version,
                "[RUNTIME-WRITER-FLOOR] an older build is writing this runtime root (still at or above its floor)"
            );
        }
    }

    let floor = match recorded_floor {
        Some(recorded) if recorded > builtin_floor => recorded,
        _ => builtin_floor,
    };
    let outcome = RuntimeWriterFloorOutcome {
        minimum_writer_version: floor.to_string(),
        running_version: running.to_owned(),
        previous_writer_version: existing
            .as_ref()
            .map(|record| record.last_writer_version.clone()),
    };
    if !record {
        return Ok(outcome);
    }

    let next = RuntimeWriterFloorRecord {
        schema_version: FLOOR_SCHEMA_VERSION,
        minimum_writer_version: outcome.minimum_writer_version.clone(),
        last_writer_version: running.to_owned(),
        updated_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    let unchanged = existing.as_ref().is_some_and(|record| {
        record.schema_version == next.schema_version
            && record.minimum_writer_version == next.minimum_writer_version
            && record.last_writer_version == next.last_writer_version
    });
    if unchanged {
        return Ok(outcome);
    }
    let encoded =
        serde_json::to_vec_pretty(&next).map_err(|error| RuntimeWriterFloorError::Io {
            path: path.display().to_string(),
            detail: format!("encode failed: {error}"),
        })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| RuntimeWriterFloorError::Io {
            path: path.display().to_string(),
            detail: format!("create parent failed: {error}"),
        })?;
    }
    crate::magician_v2::resource_authority::persistence::atomic_write_bytes(&path, &encoded)
        .map_err(|error| RuntimeWriterFloorError::Io {
            path: path.display().to_string(),
            detail: format!("write failed: {error}"),
        })?;
    info!(
        root = %runtime_root.display(),
        floor = %next.minimum_writer_version,
        running = %running,
        previous_writer = ?outcome.previous_writer_version,
        "[RUNTIME-WRITER-FLOOR] recorded the runtime writer floor"
    );
    Ok(outcome)
}

/// A missing file is a root no floor-aware build has served yet. An unreadable
/// or malformed one is logged and rewritten rather than failing closed: the
/// file guards against rollbacks, and a guard nothing can clear must not be the
/// reason a current build cannot start.
fn read_record(path: &Path) -> Result<Option<RuntimeWriterFloorRecord>, RuntimeWriterFloorError> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(RuntimeWriterFloorError::Io {
                path: path.display().to_string(),
                detail: format!("stat failed: {error}"),
            })
        },
    };
    if metadata.len() > MAX_FLOOR_FILE_BYTES {
        warn!(path = %path.display(), bytes = metadata.len(), "[RUNTIME-WRITER-FLOOR] oversized floor file ignored and rewritten");
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|error| RuntimeWriterFloorError::Io {
        path: path.display().to_string(),
        detail: format!("read failed: {error}"),
    })?;
    match serde_json::from_slice::<RuntimeWriterFloorRecord>(&bytes) {
        Ok(record) if parse_version(&record.minimum_writer_version).is_some() => Ok(Some(record)),
        Ok(_) | Err(_) => {
            warn!(path = %path.display(), "[RUNTIME-WRITER-FLOOR] malformed floor file ignored and rewritten");
            Ok(None)
        },
    }
}

fn parse_version(value: &str) -> Option<semver::Version> {
    semver::Version::parse(value.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_at(root: &Path) -> RuntimeWriterFloorRecord {
        serde_json::from_slice(&std::fs::read(runtime_writer_floor_path(root)).expect("floor file"))
            .expect("floor record")
    }

    #[test]
    fn this_build_is_at_or_above_its_own_floor() {
        assert!(
            parse_version(running_version()).expect("crate version")
                >= parse_version(MINIMUM_RUNTIME_WRITER_VERSION).expect("floor")
        );
    }

    #[test]
    fn a_fresh_root_records_the_builtin_floor_and_this_writer() {
        let root = tempfile::tempdir().expect("root");
        let outcome = enforce_with_version(root.path(), true, "0.7.96").expect("fresh root");
        assert_eq!(
            outcome.minimum_writer_version,
            MINIMUM_RUNTIME_WRITER_VERSION
        );
        assert_eq!(outcome.previous_writer_version, None);
        let record = record_at(root.path());
        assert_eq!(
            record.minimum_writer_version,
            MINIMUM_RUNTIME_WRITER_VERSION
        );
        assert_eq!(record.last_writer_version, "0.7.96");
    }

    #[test]
    fn a_build_below_the_recorded_floor_refuses_and_leaves_the_file_alone() {
        let root = tempfile::tempdir().expect("root");
        let path = runtime_writer_floor_path(root.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let newer = RuntimeWriterFloorRecord {
            schema_version: FLOOR_SCHEMA_VERSION,
            minimum_writer_version: "0.9.0".to_owned(),
            last_writer_version: "0.9.3".to_owned(),
            updated_at_ms: 1,
        };
        std::fs::write(&path, serde_json::to_vec(&newer).expect("encode")).expect("write");

        let error = enforce_with_version(root.path(), true, "0.8.9").expect_err("below floor");
        assert!(matches!(
            &error,
            RuntimeWriterFloorError::BelowFloor { required, running, .. }
                if required == "0.9.0" && running == "0.8.9"
        ));
        assert!(error.to_string().contains("requires magician >= 0.9.0"));
        assert_eq!(
            record_at(root.path()),
            newer,
            "a refused build must not rewrite the floor"
        );
        // Check-only callers are refused too.
        assert!(enforce_with_version(root.path(), false, "0.8.9").is_err());
    }

    #[test]
    fn the_floor_only_rises_and_an_in_range_downgrade_is_allowed() {
        let root = tempfile::tempdir().expect("root");
        let path = runtime_writer_floor_path(root.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let raised = RuntimeWriterFloorRecord {
            schema_version: FLOOR_SCHEMA_VERSION,
            minimum_writer_version: "0.7.50".to_owned(),
            last_writer_version: "0.7.99".to_owned(),
            updated_at_ms: 1,
        };
        std::fs::write(&path, serde_json::to_vec(&raised).expect("encode")).expect("write");

        let outcome = enforce_with_version(root.path(), true, "0.7.60").expect("in range");
        assert_eq!(outcome.minimum_writer_version, "0.7.50");
        assert_eq!(outcome.previous_writer_version.as_deref(), Some("0.7.99"));
        let record = record_at(root.path());
        assert_eq!(record.minimum_writer_version, "0.7.50", "never lowered");
        assert_eq!(record.last_writer_version, "0.7.60");
    }

    #[test]
    fn check_only_never_writes() {
        let root = tempfile::tempdir().expect("root");
        enforce_with_version(root.path(), false, "0.7.96").expect("check");
        assert!(!runtime_writer_floor_path(root.path()).exists());
    }

    #[test]
    fn a_malformed_floor_file_is_rewritten_not_fatal() {
        let root = tempfile::tempdir().expect("root");
        let path = runtime_writer_floor_path(root.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, b"{not json").expect("write");
        enforce_with_version(root.path(), true, "0.7.96").expect("rewritten");
        assert_eq!(record_at(root.path()).last_writer_version, "0.7.96");
    }
}
