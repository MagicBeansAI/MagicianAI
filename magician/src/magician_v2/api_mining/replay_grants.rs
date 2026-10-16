//! Durable approval grants for write replay, scoped to a request shape.

use super::auto_replay::DEFAULT_URL_DENYLIST;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    pub request_shape_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayGrant {
    pub id: String,
    #[serde(flatten)]
    pub key: GrantKey,
    pub origin: String,
    pub granted_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_by_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GrantFile {
    version: String,
    #[serde(default)]
    grants: Vec<ReplayGrant>,
}

impl Default for GrantFile {
    fn default() -> Self {
        Self {
            version: "1.0.0".into(),
            grants: Vec::new(),
        }
    }
}

static GRANT_STATES: OnceLock<Mutex<HashMap<PathBuf, Arc<RwLock<GrantFile>>>>> = OnceLock::new();

pub struct ReplayGrantStore {
    path: PathBuf,
    workspace_layout: ArtifactV2Workspace,
    state: Arc<RwLock<GrantFile>>,
}

pub fn is_denylisted_url_template(url_template: &str) -> bool {
    let lower = url_template.to_ascii_lowercase();
    DEFAULT_URL_DENYLIST
        .iter()
        .any(|needle| lower.contains(needle))
}

impl ReplayGrantStore {
    pub fn open<P: AsRef<Path>>(base_path: P) -> Self {
        let base = base_path.as_ref().to_path_buf();
        let path = base.join("replay_grants.json");
        let workspace_layout = ArtifactV2Workspace::with_local_file_provider(&base);
        let states = GRANT_STATES.get_or_init(|| Mutex::new(HashMap::new()));
        let state = {
            let mut states = states.lock().unwrap_or_else(|error| error.into_inner());
            states
                .entry(path.clone())
                .or_insert_with(|| {
                    let loaded = workspace_layout
                        .read_to_string_path_sync(&path)
                        .ok()
                        .and_then(|json| serde_json::from_str(&json).ok())
                        .unwrap_or_default();
                    Arc::new(RwLock::new(loaded))
                })
                .clone()
        };
        Self {
            path,
            workspace_layout,
            state,
        }
    }

    pub fn forget_shared_state<P: AsRef<Path>>(base_path: P) {
        if let Some(states) = GRANT_STATES.get() {
            states
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&base_path.as_ref().join("replay_grants.json"));
        }
    }

    pub fn lookup(&self, key: &GrantKey) -> Option<ReplayGrant> {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .grants
            .iter()
            .find(|grant| grant.revoked_at_ms.is_none() && Self::keys_match(&grant.key, key))
            .cloned()
    }

    fn keys_match(stored: &GrantKey, wanted: &GrantKey) -> bool {
        stored.request_shape_fingerprint == wanted.request_shape_fingerprint
            && stored.recipe_id == wanted.recipe_id
            && stored.step_id == wanted.step_id
            && stored.capability_id == wanted.capability_id
    }

    fn grant(
        &self,
        key: &GrantKey,
        origin: &str,
        request_id: Option<&str>,
    ) -> Result<ReplayGrant, String> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = state
            .grants
            .iter()
            .find(|grant| grant.revoked_at_ms.is_none() && Self::keys_match(&grant.key, key))
        {
            return Ok(existing.clone());
        }
        let grant = ReplayGrant {
            id: format!("grant_{}", ulid::Ulid::new()),
            key: key.clone(),
            origin: origin.to_owned(),
            granted_at_ms: chrono::Utc::now().timestamp_millis(),
            granted_by_request_id: request_id.map(str::to_owned),
            revoked_at_ms: None,
        };
        state.grants.push(grant.clone());
        if let Err(error) = self.persist_locked(&state) {
            state.grants.pop();
            return Err(error);
        }
        Ok(grant)
    }

    pub fn grant_for_url(
        &self,
        key: &GrantKey,
        url_template: &str,
        request_id: Option<&str>,
    ) -> Result<ReplayGrant, String> {
        if is_denylisted_url_template(url_template) {
            return Err(format!(
                "url matches the replay denylist floor; no durable grant: {url_template}"
            ));
        }
        self.grant(
            key,
            &super::router::extract_origin(url_template),
            request_id,
        )
    }

    pub fn revoke(&self, grant_id: &str) -> Result<bool, String> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let Some(position) = state
            .grants
            .iter()
            .position(|grant| grant.id == grant_id && grant.revoked_at_ms.is_none())
        else {
            return Ok(false);
        };
        let previous = state.grants[position].revoked_at_ms;
        state.grants[position].revoked_at_ms = Some(chrono::Utc::now().timestamp_millis());
        if let Err(error) = self.persist_locked(&state) {
            state.grants[position].revoked_at_ms = previous;
            return Err(error);
        }
        Ok(true)
    }

    pub fn revoke_origin(&self, origin: &str) -> Result<usize, String> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let now = chrono::Utc::now().timestamp_millis();
        let mut revoked_positions = Vec::new();
        for (position, grant) in state.grants.iter_mut().enumerate() {
            if grant.origin == origin && grant.revoked_at_ms.is_none() {
                grant.revoked_at_ms = Some(now);
                revoked_positions.push(position);
            }
        }
        if !revoked_positions.is_empty() {
            if let Err(error) = self.persist_locked(&state) {
                for position in &revoked_positions {
                    state.grants[*position].revoked_at_ms = None;
                }
                return Err(error);
            }
        }
        Ok(revoked_positions.len())
    }

    pub fn list(&self) -> Vec<ReplayGrant> {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .grants
            .clone()
    }

    /// Count active grants without cloning the durable audit log. Aggregate
    /// dashboard refreshes need only this scalar and may run frequently.
    pub fn active_count(&self) -> usize {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .grants
            .iter()
            .filter(|grant| grant.revoked_at_ms.is_none())
            .count()
    }

    fn persist_locked(&self, state: &GrantFile) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            self.workspace_layout
                .create_dir_all_path_sync(parent)
                .map_err(|error| error.to_string())?;
        }
        let json = serde_json::to_string_pretty(state).map_err(|error| error.to_string())?;
        self.workspace_layout
            .write_atomic_path_sync(self.path.clone(), json.as_bytes())
            .map_err(|error| error.to_string())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn key() -> GrantKey {
        GrantKey {
            recipe_id: Some("rcp_1".into()),
            step_id: Some("s2".into()),
            capability_id: None,
            request_shape_fingerprint: "fp".into(),
        }
    }

    #[test]
    fn grant_then_lookup_then_revoke() {
        let tmp = TempDir::new().unwrap();
        let store = ReplayGrantStore::open(tmp.path());
        assert!(store.lookup(&key()).is_none());
        let grant = store
            .grant(&key(), "https://x.test", Some("req_1"))
            .unwrap();
        assert_eq!(store.lookup(&key()).unwrap().id, grant.id);
        assert!(store.revoke(&grant.id).unwrap());
        assert!(store.lookup(&key()).is_none());
        assert_eq!(store.list().len(), 1);
        ReplayGrantStore::forget_shared_state(tmp.path());
    }

    #[test]
    fn capability_keyed_grants_match_without_a_recipe() {
        let tmp = TempDir::new().unwrap();
        let store = ReplayGrantStore::open(tmp.path());
        let key = GrantKey {
            recipe_id: None,
            step_id: None,
            capability_id: Some("cap_9".into()),
            request_shape_fingerprint: "fp".into(),
        };
        store.grant(&key, "https://x.test", None).unwrap();
        assert!(store.lookup(&key).is_some());
        ReplayGrantStore::forget_shared_state(tmp.path());
    }

    #[test]
    fn a_recipe_step_grant_never_authorizes_another_recipe() {
        let tmp = TempDir::new().unwrap();
        let store = ReplayGrantStore::open(tmp.path());
        let first = GrantKey {
            recipe_id: Some("rcp_first".into()),
            step_id: Some("write".into()),
            capability_id: Some("cap_shared".into()),
            request_shape_fingerprint: "same_shape".into(),
        };
        let mut second = first.clone();
        second.recipe_id = Some("rcp_second".into());
        store.grant(&first, "https://x.test", None).unwrap();

        assert!(store.lookup(&first).is_some());
        assert!(store.lookup(&second).is_none());
        ReplayGrantStore::forget_shared_state(tmp.path());
    }

    #[test]
    fn revoke_origin_only_revokes_active_grants_for_that_origin() {
        let tmp = TempDir::new().unwrap();
        let store = ReplayGrantStore::open(tmp.path());
        let first = key();
        let mut second = key();
        second.step_id = Some("s3".into());
        second.request_shape_fingerprint = "fp_other".into();
        store.grant(&first, "https://api.one.test", None).unwrap();
        store.grant(&second, "https://api.two.test", None).unwrap();

        assert_eq!(store.revoke_origin("https://api.one.test").unwrap(), 1);
        assert!(store.lookup(&first).is_none());
        assert!(store.lookup(&second).is_some());
        assert_eq!(store.revoke_origin("https://api.one.test").unwrap(), 0);
        ReplayGrantStore::forget_shared_state(tmp.path());
    }

    #[test]
    fn denylisted_paths_never_receive_a_durable_grant() {
        assert!(is_denylisted_url_template(
            "https://shop.test/api/checkout/confirm"
        ));
        assert!(is_denylisted_url_template("https://shop.test/PAYMENT/{id}"));
        assert!(!is_denylisted_url_template(
            "https://shop.test/api/cart/items"
        ));
        let tmp = TempDir::new().unwrap();
        let store = ReplayGrantStore::open(tmp.path());
        assert!(store
            .grant_for_url(&key(), "https://shop.test/api/checkout", None)
            .unwrap_err()
            .contains("denylist"));
        ReplayGrantStore::forget_shared_state(tmp.path());
    }
}
