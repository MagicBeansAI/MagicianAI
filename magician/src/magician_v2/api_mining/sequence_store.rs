//! File-backed sequence store.
//!
//! Layout: `<base>/<origin_key>/sequences/<sequence_id>.json`. `<base>` is the
//! per-scope api_mining root (`<scope>/api_mining`). `<origin_key>` is the
//! sanitized origin (same scheme as `capabilities/` and `traces/`). One JSON
//! file per `CapabilitySequence`.

use crate::magician_v2::api_mining::path_safe::{
    canonical_origin_dir, ensure_safe_origin_key, ensure_safe_record_id,
};
use crate::magician_v2::api_mining::sequence::CapabilitySequence;
use crate::magician_v2::artifact_v2::{
    workspace::{ArtifactV2Workspace, WorkspaceFileEntry},
    ArtifactV2Error,
};
use std::io;
use std::path::PathBuf;

/// Per-scope store. Construct once per execution scope and reuse.
pub struct SequenceStore {
    base: PathBuf,
    workspace_layout: ArtifactV2Workspace,
}

impl SequenceStore {
    pub fn new(base: PathBuf) -> Self {
        Self {
            workspace_layout: ArtifactV2Workspace::with_local_file_provider(&base),
            base,
        }
    }

    fn sequences_dir(&self, origin_key: &str) -> PathBuf {
        // canonical_origin_dir strips scheme + sanitizes embedded `/` so
        // producer-side `https://example.com` and HTTP-side `example.com`
        // both resolve to the same on-disk directory.
        self.base
            .join(canonical_origin_dir(origin_key))
            .join("sequences")
    }

    fn entry_path(&self, entry: &WorkspaceFileEntry) -> PathBuf {
        self.base.join(&entry.relative_path)
    }

    fn provider_error(error: ArtifactV2Error) -> io::Error {
        io::Error::other(error)
    }

    fn is_not_found(error: &ArtifactV2Error) -> bool {
        matches!(error, ArtifactV2Error::Io(inner) if inner.kind() == io::ErrorKind::NotFound)
    }

    /// Persist a sequence to disk. Creates parent dirs as needed.
    pub fn save(&self, seq: &CapabilitySequence) -> io::Result<()> {
        ensure_safe_origin_key(&seq.origin_key, "origin_key")?;
        ensure_safe_record_id(&seq.id, "sequence id")?;
        let dir = self.sequences_dir(&seq.origin_key);
        self.workspace_layout
            .create_dir_all_path_sync(&dir)
            .map_err(io::Error::other)?;
        let path = dir.join(format!("{}.json", seq.id));
        let json = serde_json::to_string_pretty(seq).map_err(io::Error::other)?;
        self.workspace_layout
            .write_atomic_path_sync(path, json.as_bytes())
            .map_err(io::Error::other)
    }

    /// Load one sequence. Returns Ok(None) when the file is absent — callers
    /// shouldn't fail on a missing sequence id.
    pub fn load(
        &self,
        origin_key: &str,
        sequence_id: &str,
    ) -> io::Result<Option<CapabilitySequence>> {
        ensure_safe_origin_key(origin_key, "origin_key")?;
        ensure_safe_record_id(sequence_id, "sequence_id")?;
        let path = self
            .sequences_dir(origin_key)
            .join(format!("{sequence_id}.json"));
        match self.workspace_layout.read_to_string_path_sync(&path) {
            Ok(json) => {
                let seq: CapabilitySequence = serde_json::from_str(&json)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(Some(seq))
            },
            Err(e) if Self::is_not_found(&e) => Ok(None),
            Err(e) => Err(Self::provider_error(e)),
        }
    }

    /// List all sequences for an origin. Returns empty Vec when the dir doesn't
    /// exist. Sequences with malformed JSON are skipped with a `tracing::warn`.
    pub fn list(&self, origin_key: &str) -> io::Result<Vec<CapabilitySequence>> {
        ensure_safe_origin_key(origin_key, "origin_key")?;
        let dir = self.sequences_dir(origin_key);
        let entries = match self.workspace_layout.read_dir_path_sync(&dir) {
            Ok(e) => e,
            Err(e) if Self::is_not_found(&e) => return Ok(Vec::new()),
            Err(e) => return Err(Self::provider_error(e)),
        };
        let mut out = Vec::new();
        for entry in entries {
            if !entry.is_file {
                continue;
            }
            let path = self.entry_path(&entry);
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let json = match self.workspace_layout.read_to_string_path_sync(&path) {
                Ok(json) => json,
                Err(e) => {
                    tracing::warn!("sequence store: skip unreadable {:?}: {}", path, e);
                    continue;
                },
            };
            match serde_json::from_str::<CapabilitySequence>(&json) {
                Ok(seq) => out.push(seq),
                Err(e) => tracing::warn!("sequence store: skip malformed {:?}: {}", path, e),
            }
        }
        Ok(out)
    }

    /// Number of sequence files in the origin's directory. Used by Phase 2 to
    /// decide when to trigger compilation (after 2+ sequences exist).
    pub fn count(&self, origin_key: &str) -> io::Result<usize> {
        ensure_safe_origin_key(origin_key, "origin_key")?;
        let dir = self.sequences_dir(origin_key);
        match self.workspace_layout.read_dir_path_sync(&dir) {
            Ok(entries) => Ok(entries
                .into_iter()
                .filter(|e| e.is_file)
                .filter(|e| self.entry_path(e).extension().and_then(|s| s.to_str()) == Some("json"))
                .count()),
            Err(e) if Self::is_not_found(&e) => Ok(0),
            Err(e) => Err(Self::provider_error(e)),
        }
    }

    /// List all sequences for one task across origin directories.
    ///
    /// Task recipe compilation is cross-origin, while sequence storage is
    /// intentionally origin-sharded. A normal run only creates a few origin
    /// directories, so this bounded directory walk avoids a second index.
    pub fn list_for_task(&self, task_id: &str) -> io::Result<Vec<CapabilitySequence>> {
        ensure_safe_record_id(task_id, "task id")?;
        let origins = match self.workspace_layout.read_dir_path_sync(&self.base) {
            Ok(entries) => entries,
            Err(error) if Self::is_not_found(&error) => return Ok(Vec::new()),
            Err(error) => return Err(Self::provider_error(error)),
        };
        let mut sequences = Vec::new();
        for origin in origins.into_iter().filter(|entry| entry.is_dir) {
            // Non-origin directories (recipes, traces, projections) simply
            // have no `sequences/` child and produce an empty list.
            sequences.extend(
                self.list(&origin.file_name)?
                    .into_iter()
                    .filter(|sequence| sequence.task_id == task_id),
            );
        }
        sequences.sort_by_key(|sequence| sequence.captured_at_ms);
        Ok(sequences)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::sequence::{
        CapabilitySequence, ExecutionPath, SequenceStep,
    };
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn sample_sequence(id: &str, origin: &str) -> CapabilitySequence {
        CapabilitySequence {
            id: id.to_string(),
            task_id: "task_x".to_string(),
            execution_id: "exec_x".to_string(),
            origin_key: origin.to_string(),
            steps: vec![SequenceStep {
                step_index: 0,
                capability_id: Some("cap_a".to_string()),
                origin: format!("https://{origin}"),
                concrete_url: format!("https://{origin}/api/x"),
                method: "GET".to_string(),
                request_params: HashMap::new(),
                request_body: None,
                response_status: Some(200),
                response_body: Some("{\"ok\":true}".to_string()),
                action_binding_id: None,
                browser_action_desc: None,
                browser_action: None,
                browser_arguments: None,
                executed_via: ExecutionPath::ApiReplay,
                timestamp_ms: 1_780_000_000_000,
                duration_ms: 42,
            }],
            captured_at_ms: 1_780_000_000_000,
            finalized: true,
        }
    }

    #[test]
    fn store_save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let store = SequenceStore::new(dir.path().to_path_buf());
        let seq = sample_sequence("seq_1", "example.com");
        store.save(&seq).expect("save");
        let loaded = store
            .load("example.com", "seq_1")
            .expect("load")
            .expect("present");
        assert_eq!(loaded.id, "seq_1");
        assert_eq!(loaded.steps.len(), 1);
    }

    #[test]
    fn store_list_returns_origin_sequences_only() {
        let dir = tempdir().unwrap();
        let store = SequenceStore::new(dir.path().to_path_buf());
        store
            .save(&sample_sequence("seq_1", "example.com"))
            .unwrap();
        store
            .save(&sample_sequence("seq_2", "example.com"))
            .unwrap();
        store.save(&sample_sequence("seq_3", "other.com")).unwrap();

        let listed = store.list("example.com").expect("list");
        let mut ids: Vec<_> = listed.iter().map(|s| s.id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["seq_1", "seq_2"]);
    }

    #[test]
    fn store_list_missing_origin_returns_empty() {
        let dir = tempdir().unwrap();
        let store = SequenceStore::new(dir.path().to_path_buf());
        let listed = store.list("never-seen.com").expect("list");
        assert!(listed.is_empty());
    }

    #[test]
    fn store_load_missing_sequence_returns_none() {
        let dir = tempdir().unwrap();
        let store = SequenceStore::new(dir.path().to_path_buf());
        let loaded = store.load("example.com", "seq_nope").expect("load");
        assert!(loaded.is_none());
    }

    #[test]
    fn store_canonicalizes_origin_so_full_url_and_bare_host_converge() {
        // Regression: producer stores sequences with `seq.origin_key`
        // in the `https://example.com` form (from router::extract_origin),
        // but HTTP path params can only carry a single URL segment
        // (`example.com`). The store canonicalizes to `example.com`
        // internally so both producer and HTTP consumer find the same
        // on-disk data.
        let dir = tempdir().unwrap();
        let store = SequenceStore::new(dir.path().to_path_buf());
        let seq = sample_sequence("seq_e2e", "https://example.com");
        store.save(&seq).unwrap();

        // Producer form load: works.
        let via_full_url = store.load("https://example.com", "seq_e2e").unwrap();
        assert!(via_full_url.is_some());

        // HTTP-path-param form load: also works.
        let via_bare_host = store.load("example.com", "seq_e2e").unwrap();
        assert!(via_bare_host.is_some());

        // List with the HTTP-path-param form returns the producer-stored sequence.
        let listed = store.list("example.com").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "seq_e2e");
    }

    #[test]
    fn store_count_per_origin() {
        let dir = tempdir().unwrap();
        let store = SequenceStore::new(dir.path().to_path_buf());
        store
            .save(&sample_sequence("seq_1", "example.com"))
            .unwrap();
        store
            .save(&sample_sequence("seq_2", "example.com"))
            .unwrap();
        assert_eq!(store.count("example.com").unwrap(), 2);
        assert_eq!(store.count("other.com").unwrap(), 0);
    }
}
