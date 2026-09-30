//! File-backed workflow store. Mirrors `SequenceStore` but persists
//! `WorkflowGraph` records at `<base>/<origin_key>/workflows/<workflow_id>.json`.

use crate::magician_v2::api_mining::path_safe::{
    canonical_origin_dir, ensure_safe_origin_key, ensure_safe_record_id,
};
use crate::magician_v2::api_mining::workflow::WorkflowGraph;
use crate::magician_v2::artifact_v2::{
    workspace::{ArtifactV2Workspace, WorkspaceFileEntry},
    ArtifactV2Error,
};
use std::io;
use std::path::PathBuf;

pub struct WorkflowStore {
    base: PathBuf,
    workspace_layout: ArtifactV2Workspace,
}

impl WorkflowStore {
    pub fn new(base: PathBuf) -> Self {
        Self {
            workspace_layout: ArtifactV2Workspace::with_local_file_provider(&base),
            base,
        }
    }

    fn workflows_dir(&self, origin_key: &str) -> PathBuf {
        // canonical_origin_dir strips scheme + sanitizes embedded `/` so
        // producer-side `https://example.com` and HTTP-side `example.com`
        // both resolve to the same on-disk directory.
        self.base
            .join(canonical_origin_dir(origin_key))
            .join("workflows")
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

    pub fn save(&self, workflow: &WorkflowGraph) -> io::Result<()> {
        ensure_safe_origin_key(&workflow.origin_key, "origin_key")?;
        ensure_safe_record_id(&workflow.id, "workflow id")?;
        let dir = self.workflows_dir(&workflow.origin_key);
        self.workspace_layout
            .create_dir_all_path_sync(&dir)
            .map_err(io::Error::other)?;
        let path = dir.join(format!("{}.json", workflow.id));
        let json = serde_json::to_string_pretty(workflow).map_err(io::Error::other)?;
        self.workspace_layout
            .write_atomic_path_sync(path, json.as_bytes())
            .map_err(io::Error::other)
    }

    pub fn load(&self, origin_key: &str, workflow_id: &str) -> io::Result<Option<WorkflowGraph>> {
        ensure_safe_origin_key(origin_key, "origin_key")?;
        ensure_safe_record_id(workflow_id, "workflow_id")?;
        let path = self
            .workflows_dir(origin_key)
            .join(format!("{workflow_id}.json"));
        match self.workspace_layout.read_to_string_path_sync(&path) {
            Ok(json) => {
                let wf: WorkflowGraph = serde_json::from_str(&json)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(Some(wf))
            },
            Err(e) if Self::is_not_found(&e) => Ok(None),
            Err(e) => Err(Self::provider_error(e)),
        }
    }

    pub fn list(&self, origin_key: &str) -> io::Result<Vec<WorkflowGraph>> {
        ensure_safe_origin_key(origin_key, "origin_key")?;
        let dir = self.workflows_dir(origin_key);
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
                    tracing::warn!("workflow store: skip unreadable {:?}: {}", path, e);
                    continue;
                },
            };
            match serde_json::from_str::<WorkflowGraph>(&json) {
                Ok(wf) => out.push(wf),
                Err(e) => tracing::warn!("workflow store: skip malformed {:?}: {}", path, e),
            }
        }
        Ok(out)
    }

    pub fn count(&self, origin_key: &str) -> io::Result<usize> {
        ensure_safe_origin_key(origin_key, "origin_key")?;
        let dir = self.workflows_dir(origin_key);
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
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::workflow::{
        AuthRequirements, ReplayStats, WorkflowConfidence, WorkflowMaturity, WorkflowStep,
    };
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn sample_workflow(id: &str, origin: &str) -> WorkflowGraph {
        WorkflowGraph {
            id: id.to_string(),
            origin_key: origin.to_string(),
            name: "Test workflow".to_string(),
            steps: vec![WorkflowStep {
                id: "step_0".to_string(),
                step_index: 0,
                capability_id: Some("cap_a".to_string()),
                param_sources: HashMap::new(),
                skip_if: None,
                browser_only: false,
                browser_fallback: None,
            }],
            data_flows: vec![],
            auth_requirements: AuthRequirements::default(),
            confidence: WorkflowConfidence {
                workflow_level: WorkflowMaturity::Draft,
                step_confidences: HashMap::new(),
            },
            compiled_from_sequence_ids: vec![],
            last_compiled_at_ms: 1_780_000_000_000,
            last_replayed_at_ms: None,
            replay_stats: ReplayStats::default(),
        }
    }

    #[test]
    fn workflow_store_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let store = WorkflowStore::new(dir.path().to_path_buf());
        store.save(&sample_workflow("wf_1", "example.com")).unwrap();
        let loaded = store.load("example.com", "wf_1").unwrap().unwrap();
        assert_eq!(loaded.id, "wf_1");
    }

    #[test]
    fn workflow_store_list_filters_by_origin() {
        let dir = tempdir().unwrap();
        let store = WorkflowStore::new(dir.path().to_path_buf());
        store.save(&sample_workflow("wf_1", "example.com")).unwrap();
        store.save(&sample_workflow("wf_2", "other.com")).unwrap();
        let listed = store.list("example.com").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "wf_1");
    }

    #[test]
    fn workflow_store_canonicalizes_origin_so_full_url_and_bare_host_converge() {
        // Same regression check as sequence_store: producer stores with
        // `workflow.origin_key = "https://example.com"`, HTTP consumer
        // passes `example.com`. Canonicalization at the store boundary
        // makes both find the same on-disk data.
        let dir = tempdir().unwrap();
        let store = WorkflowStore::new(dir.path().to_path_buf());
        store
            .save(&sample_workflow("wf_e2e", "https://example.com"))
            .unwrap();

        let via_full_url = store.load("https://example.com", "wf_e2e").unwrap();
        assert!(via_full_url.is_some());

        let via_bare_host = store.load("example.com", "wf_e2e").unwrap();
        assert!(via_bare_host.is_some());

        let listed = store.list("example.com").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "wf_e2e");
    }

    #[test]
    fn workflow_store_count() {
        let dir = tempdir().unwrap();
        let store = WorkflowStore::new(dir.path().to_path_buf());
        store.save(&sample_workflow("wf_1", "example.com")).unwrap();
        assert_eq!(store.count("example.com").unwrap(), 1);
        assert_eq!(store.count("nope.com").unwrap(), 0);
    }
}
