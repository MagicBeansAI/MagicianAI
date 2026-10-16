//! Boot-review failure memory.
//!
//! A boot-admitted system package whose host review fails deterministically —
//! an invalid agent definition in the scope, a manifest the current binary
//! refuses — fails the same way on every boot until one of its inputs changes.
//! Re-running the review each boot bought nothing but seconds per package per
//! scope (measured ~19 s per scope per boot for a scope whose agent definition
//! carried a duplicate tool, on every boot, never succeeding).
//!
//! The memory is keyed by a digest of what actually decides the review: the
//! package (the seed inventory digest names the exact admitted bytes), the
//! scope's primitive catalog (every scope skill and agent definition the review
//! resolves, hashed), a metadata fingerprint of the extra skill and agent
//! template directories the host is configured with (personalities and
//! fallback runner templates are resolved there), and the host build (the
//! magician, magician-apps and magician-api versions plus the running
//! executable's size and mtime — a new binary may judge the same bytes
//! differently). Any change re-reviews. Only failures that are verdicts on
//! those inputs are remembered (see `remembers_boot_review_failure`). It is
//! consulted only by boot enablement: an owner's install or approve through the
//! API always re-reviews.
//!
//! Stored under the scope's own `apps/` directory, so purging the workspace
//! takes it along. Unreadable or corrupt memory reads as empty — it can only
//! ever cost a re-review, never block one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::apps::models::AppDigest;

const BOOT_REVIEW_FAILURES_FILE: &str = "boot-review-failures.json";
const BOOT_REVIEW_FAILURES_SCHEMA_VERSION: u32 = 1;
const MAX_RECORDED_ERROR_CHARS: usize = 2_000;

/// The digest of every input that decides one package's boot review in one
/// scope. See the module docs for why each part is there. `host_inputs` is
/// [`boot_review_host_inputs`], computed once per scope.
pub(super) fn boot_review_input_digest(
    package_dir: &str,
    inventory_digest: &AppDigest,
    catalog_snapshot_digest: &AppDigest,
    host_inputs: &str,
) -> String {
    let material = serde_json::json!({
        "schema_version": BOOT_REVIEW_FAILURES_SCHEMA_VERSION,
        "package_dir": package_dir,
        "inventory_digest": inventory_digest.as_str(),
        "catalog_snapshot_digest": catalog_snapshot_digest.as_str(),
        "host_inputs": host_inputs,
    });
    let bytes = serde_json::to_vec(&material).unwrap_or_default();
    AppDigest::blake3(&bytes).as_str().to_owned()
}

/// Bound on the extra-directory fingerprint walk. Past it the fingerprint
/// carries a per-boot nonce, so nothing is remembered rather than something
/// being remembered on a partial view.
const MAX_FINGERPRINT_ENTRIES: usize = 8_192;
const MAX_FINGERPRINT_DEPTH: usize = 4;

/// The host-side inputs of every boot review, outside the scope's catalog.
/// Blocking: stats the running binary and walks the configured extra
/// directories.
///
/// - The versions of every crate whose code judges a review (magician holds
///   the validators, magician-apps the review, magician-api this caller) and
///   the running executable's size and mtime, so a rebuild without a version
///   bump still re-reviews.
/// - A metadata fingerprint (relative path, size, mtime) of the configured
///   extra skill directories (personalities are looked up there and in the
///   scope's skills, which the scope catalog does not describe) and extra agent
///   template directories (a runner missing from the scope is materialized
///   from them).
pub(super) fn boot_review_host_inputs(
    magician_version: &str,
    magician_apps_version: &str,
    extra_skill_dirs: &[PathBuf],
    extra_agent_template_dirs: &[PathBuf],
) -> String {
    let executable = std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|metadata| {
            format!(
                "{}:{:?}",
                metadata.len(),
                metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            )
        })
        .unwrap_or_else(|_| format!("unknown:{}", uuid::Uuid::new_v4()));
    let material = serde_json::json!({
        "magician_api": env!("CARGO_PKG_VERSION"),
        "magician": magician_version,
        "magician_apps": magician_apps_version,
        "executable": executable,
        "extra_skill_dirs": directories_fingerprint(extra_skill_dirs),
        "extra_agent_template_dirs": directories_fingerprint(extra_agent_template_dirs),
    });
    let bytes = serde_json::to_vec(&material).unwrap_or_default();
    AppDigest::blake3(&bytes).as_str().to_owned()
}

/// Paths plus a bounded metadata walk under each. Any change to a file the
/// review could read (added, removed, rewritten) moves it.
fn directories_fingerprint(dirs: &[PathBuf]) -> String {
    let mut entries = Vec::new();
    let mut overflowed = false;
    for dir in dirs {
        entries.push(format!("root:{}", dir.to_string_lossy()));
        let mut stack = vec![(dir.clone(), 0usize)];
        while let Some((path, depth)) = stack.pop() {
            let Ok(children) = std::fs::read_dir(&path) else {
                continue;
            };
            for child in children.flatten() {
                if entries.len() >= MAX_FINGERPRINT_ENTRIES {
                    overflowed = true;
                    break;
                }
                let child_path = child.path();
                let Ok(metadata) = std::fs::metadata(&child_path) else {
                    continue;
                };
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default();
                entries.push(format!(
                    "{}:{}:{}:{modified}",
                    child_path.to_string_lossy(),
                    metadata.is_dir(),
                    metadata.len(),
                ));
                if metadata.is_dir() && depth < MAX_FINGERPRINT_DEPTH {
                    stack.push((child_path, depth + 1));
                }
            }
        }
    }
    entries.sort();
    if overflowed {
        entries.push(format!("overflow:{}", uuid::Uuid::new_v4()));
    }
    AppDigest::blake3(entries.join("\n").as_bytes())
        .as_str()
        .to_owned()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BootReviewFailureFile {
    schema_version: u32,
    #[serde(default)]
    packages: BTreeMap<String, BootReviewFailureRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BootReviewFailureRecord {
    input_digest: String,
    error: String,
    recorded_at: DateTime<Utc>,
}

/// One scope's remembered boot-review failures, loaded once per boot pass.
#[derive(Debug)]
pub(super) struct BootReviewFailures {
    path: PathBuf,
    file: BootReviewFailureFile,
    dirty: bool,
}

impl BootReviewFailures {
    /// Blocking: one small file read.
    pub(super) fn load(apps_root: &Path) -> Self {
        let path = apps_root.join(BOOT_REVIEW_FAILURES_FILE);
        let file = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<BootReviewFailureFile>(&bytes)
                .ok()
                .filter(|file| file.schema_version == BOOT_REVIEW_FAILURES_SCHEMA_VERSION)
                .unwrap_or_default(),
            Err(_) => BootReviewFailureFile::default(),
        };
        Self {
            path,
            file,
            dirty: false,
        }
    }

    /// The recorded error when this package's last boot review failed with
    /// exactly these inputs; `None` means review it.
    pub(super) fn unchanged_failure(&self, package_dir: &str, input_digest: &str) -> Option<&str> {
        self.file
            .packages
            .get(package_dir)
            .filter(|record| record.input_digest == input_digest)
            .map(|record| record.error.as_str())
    }

    pub(super) fn record(
        &mut self,
        package_dir: &str,
        input_digest: String,
        error: &str,
        now: DateTime<Utc>,
    ) {
        let error = error.chars().take(MAX_RECORDED_ERROR_CHARS).collect();
        let record = BootReviewFailureRecord {
            input_digest,
            error,
            recorded_at: now,
        };
        if self.file.packages.get(package_dir) != Some(&record) {
            self.file.packages.insert(package_dir.to_owned(), record);
            self.dirty = true;
        }
    }

    /// A package that reviewed (or no longer needs review) has nothing to
    /// remember.
    pub(super) fn clear(&mut self, package_dir: &str) {
        if self.file.packages.remove(package_dir).is_some() {
            self.dirty = true;
        }
    }

    /// Blocking. Writes only when something changed; an empty memory removes
    /// the file rather than leaving an empty one behind.
    pub(super) fn persist(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.file.packages.is_empty() {
            match std::fs::remove_file(&self.path) {
                Ok(()) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
        } else {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.file.schema_version = BOOT_REVIEW_FAILURES_SCHEMA_VERSION;
            let bytes = serde_json::to_vec_pretty(&self.file).map_err(std::io::Error::other)?;
            magician::magician_v2::artifact_v2::io::write_bytes_durably_with_mode_sync(
                &self.path,
                &bytes,
                Some(0o600),
            )?;
        }
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(seed: &str) -> AppDigest {
        AppDigest::blake3(seed.as_bytes())
    }

    /// A remembered failure is a hit only for exactly the inputs that failed:
    /// a changed catalog (the scope's definitions were fixed), a changed seed
    /// inventory, or changed host inputs (binary, extra dirs) each re-review.
    #[test]
    fn a_remembered_failure_hits_only_while_its_inputs_are_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let apps_root = temp.path().join("apps");
        let failing =
            boot_review_input_digest("learning", &digest("inv-1"), &digest("cat-1"), "host");

        let mut memory = BootReviewFailures::load(&apps_root);
        assert!(memory.unchanged_failure("learning", &failing).is_none());
        memory.record(
            "learning",
            failing.clone(),
            "agent definition validation error: duplicate tool `create_task`",
            Utc::now(),
        );
        memory.persist().expect("persist");

        // A later boot reads it back.
        let reloaded = BootReviewFailures::load(&apps_root);
        assert_eq!(
            reloaded.unchanged_failure("learning", &failing),
            Some("agent definition validation error: duplicate tool `create_task`")
        );
        assert!(
            reloaded.unchanged_failure("meetings", &failing).is_none(),
            "memory is per package"
        );

        let fixed_definitions =
            boot_review_input_digest("learning", &digest("inv-1"), &digest("cat-2"), "host");
        assert_ne!(fixed_definitions, failing);
        assert!(reloaded
            .unchanged_failure("learning", &fixed_definitions)
            .is_none());

        let new_seed =
            boot_review_input_digest("learning", &digest("inv-2"), &digest("cat-1"), "host");
        assert!(reloaded.unchanged_failure("learning", &new_seed).is_none());

        let new_host = boot_review_input_digest(
            "learning",
            &digest("inv-1"),
            &digest("cat-1"),
            "rebuilt-host",
        );
        assert!(reloaded.unchanged_failure("learning", &new_host).is_none());
    }

    /// Adding, removing or rewriting a file under a configured extra directory
    /// moves the host inputs, so a personality or runner template installed
    /// after a failure re-reviews.
    #[test]
    fn host_inputs_follow_extra_directory_contents_and_versions() {
        let temp = tempfile::tempdir().unwrap();
        let skills = temp.path().join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        let before = boot_review_host_inputs("0.7.96", "0.2.7", &[skills.clone()], &[]);
        assert_eq!(
            before,
            boot_review_host_inputs("0.7.96", "0.2.7", &[skills.clone()], &[]),
            "unchanged inputs are stable"
        );
        std::fs::create_dir_all(skills.join("calm")).unwrap();
        std::fs::write(skills.join("calm").join("SKILL.md"), b"personality").unwrap();
        let after = boot_review_host_inputs("0.7.96", "0.2.7", &[skills.clone()], &[]);
        assert_ne!(before, after, "a new personality re-reviews");
        assert_ne!(
            after,
            boot_review_host_inputs("0.7.97", "0.2.7", &[skills.clone()], &[]),
            "a magician version change re-reviews"
        );
        assert_ne!(
            after,
            boot_review_host_inputs("0.7.96", "0.2.8", &[skills], &[]),
            "a magician-apps version change re-reviews"
        );
    }

    /// Clearing the last entry removes the file; corrupt memory reads empty
    /// rather than blocking a review.
    #[test]
    fn cleared_memory_removes_its_file_and_corrupt_memory_reads_empty() {
        let temp = tempfile::tempdir().unwrap();
        let apps_root = temp.path().join("apps");
        let input = boot_review_input_digest("meetings", &digest("inv"), &digest("cat"), "host");
        let mut memory = BootReviewFailures::load(&apps_root);
        memory.record("meetings", input.clone(), "refused", Utc::now());
        memory.persist().unwrap();
        let file = apps_root.join(BOOT_REVIEW_FAILURES_FILE);
        assert!(file.is_file());

        let mut memory = BootReviewFailures::load(&apps_root);
        memory.clear("meetings");
        memory.persist().unwrap();
        assert!(!file.exists());

        std::fs::write(&file, b"{ not json").unwrap();
        let memory = BootReviewFailures::load(&apps_root);
        assert!(memory.unchanged_failure("meetings", &input).is_none());
    }
}
