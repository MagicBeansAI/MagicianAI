//! Per-origin capability persistence
//!
//! Stores ApiCapability objects as individual JSON files organized by origin.
//!
//! Disk layout for a bound scope:
//! ```text
//! magician_data_v3/scopes/{principal}/{workspace}/api_mining/
//!   {origin_key}/
//!     capabilities/
//!       {capability_id}.json
//! ```

use super::capability::ApiCapability;
use crate::magician_v2::artifact_v2::{
    workspace::{ArtifactV2Workspace, WorkspaceFileEntry},
    ArtifactV2Error,
};
use crate::magician_v2::process_storage;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Default maximum capabilities per origin (safety limit)
const DEFAULT_MAX_CAPABILITIES_PER_ORIGIN: usize = 100;

fn default_api_mining_base() -> PathBuf {
    process_storage::workspace().api_mining_root("__unbound__", "__unbound__")
}

/// Per-origin capability store
pub struct CapabilityStore {
    base_path: PathBuf,
    workspace_layout: ArtifactV2Workspace,
    /// Configurable max capabilities per origin (from ApiMiningConfig)
    max_capabilities_per_origin: usize,
}

impl CapabilityStore {
    /// Create a new capability store with the default base path
    pub fn new() -> Self {
        let base_path = default_api_mining_base();
        Self {
            workspace_layout: ArtifactV2Workspace::with_local_file_provider(&base_path),
            base_path,
            max_capabilities_per_origin: DEFAULT_MAX_CAPABILITIES_PER_ORIGIN,
        }
    }

    /// Create a store with a custom base path (for testing)
    pub fn with_base_path<P: AsRef<Path>>(path: P) -> Self {
        let base_path = path.as_ref().to_path_buf();
        Self {
            workspace_layout: ArtifactV2Workspace::with_local_file_provider(&base_path),
            base_path,
            max_capabilities_per_origin: DEFAULT_MAX_CAPABILITIES_PER_ORIGIN,
        }
    }

    /// Set the maximum capabilities per origin (from config)
    pub fn with_max_capabilities(mut self, max: usize) -> Self {
        self.max_capabilities_per_origin = max;
        self
    }

    /// Convert an origin URL to a filesystem-safe key
    ///
    /// Example: "https://mail.google.com" → "https___mail_google_com"
    pub fn origin_to_key(origin: &str) -> String {
        if origin.is_empty() {
            return "unknown_origin".to_string();
        }
        origin
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else if c == ':' && origin.contains("://") {
                    '_'
                } else {
                    '_'
                }
            })
            .collect()
    }

    /// Validate a capability ID is safe for filesystem use
    pub fn validate_id(id: &str) -> Result<(), String> {
        if id.is_empty() {
            return Err("Capability ID must not be empty".to_string());
        }
        if id.contains("..") || id.contains('/') || id.contains('\\') {
            return Err(format!("Invalid capability ID: {}", id));
        }
        if id.len() > 128 {
            return Err("Capability ID too long".to_string());
        }
        Ok(())
    }

    /// Get the capabilities directory for an origin
    fn capabilities_dir(&self, origin_key: &str) -> PathBuf {
        self.base_path.join(origin_key).join("capabilities")
    }

    fn entry_path(&self, entry: &WorkspaceFileEntry) -> PathBuf {
        self.base_path.join(&entry.relative_path)
    }

    fn path_exists(&self, path: &Path) -> Result<bool, String> {
        self.workspace_layout
            .metadata_path_sync(path)
            .map(|metadata| metadata.is_some())
            .map_err(|e| e.to_string())
    }

    fn is_not_found(error: &ArtifactV2Error) -> bool {
        matches!(error, ArtifactV2Error::Io(inner) if inner.kind() == std::io::ErrorKind::NotFound)
    }

    /// Ensure the capabilities directory exists for an origin
    fn ensure_capabilities_dir(&self, origin_key: &str) -> Result<PathBuf, String> {
        let dir = self.capabilities_dir(origin_key);
        self.workspace_layout
            .create_dir_all_path_sync(&dir)
            .map_err(|e| format!("Failed to create capabilities dir: {}", e))?;

        Ok(dir)
    }

    /// Save a capability to disk
    pub fn save(&self, capability: &ApiCapability) -> Result<PathBuf, String> {
        Self::validate_id(&capability.id)?;
        let origin_key = Self::origin_to_key(&capability.origin);
        let dir = self.ensure_capabilities_dir(&origin_key)?;
        let filepath = dir.join(format!("{}.json", capability.id));

        // Enforce per-origin limit (allow updates to existing capabilities)
        if !self.path_exists(&filepath)? {
            let existing_count = self
                .count(&capability.origin)
                .map_err(|e| {
                    warn!(
                        "Failed to count capabilities for origin {}: {}",
                        capability.origin, e
                    );
                    e
                })
                .unwrap_or(0);
            if existing_count >= self.max_capabilities_per_origin {
                return Err(format!(
                    "Capability limit reached for origin {}: {} >= {}",
                    capability.origin, existing_count, self.max_capabilities_per_origin
                ));
            }
        }

        let bytes = serde_json::to_vec_pretty(capability)
            .map_err(|e| format!("Failed to serialize capability: {}", e))?;
        self.workspace_layout
            .write_atomic_path_sync(&filepath, &bytes)
            .map_err(|e| format!("Failed to write capability file: {}", e))?;

        Ok(filepath)
    }

    /// Load a capability by ID from a specific origin
    pub fn load(&self, origin: &str, capability_id: &str) -> Result<ApiCapability, String> {
        Self::validate_id(capability_id)?;
        let origin_key = Self::origin_to_key(origin);
        let filepath = self
            .capabilities_dir(&origin_key)
            .join(format!("{}.json", capability_id));

        let content = self
            .workspace_layout
            .read_to_string_path_sync(&filepath)
            .map_err(|e| format!("Failed to read capability {}: {}", capability_id, e))?;

        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse capability {}: {}", capability_id, e))
    }

    /// Load all capabilities for an origin
    pub fn load_all(&self, origin: &str) -> Result<Vec<ApiCapability>, String> {
        let origin_key = Self::origin_to_key(origin);
        let dir = self.capabilities_dir(&origin_key);

        if !self.path_exists(&dir)? {
            return Ok(Vec::new());
        }

        let entries = self
            .workspace_layout
            .read_dir_path_sync(&dir)
            .map_err(|e| format!("Failed to read capabilities dir: {}", e))?;

        let mut capabilities = Vec::new();
        for entry in entries {
            if !entry.is_file {
                continue;
            }
            let path = self.entry_path(&entry);

            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            match self.workspace_layout.read_to_string_path_sync(&path) {
                Ok(content) => match serde_json::from_str::<ApiCapability>(&content) {
                    Ok(cap) => capabilities.push(cap),
                    Err(e) => {
                        warn!("Skipping corrupt capability file {:?}: {}", path, e);
                    },
                },
                Err(e) => {
                    warn!("Failed to read {:?}: {}", path, e);
                },
            }
        }

        // Sort by ID for deterministic ordering across reloads.
        // read_dir() returns entries in filesystem order (undefined),
        // which causes nondeterministic capability selection when
        // multiple templates can match the same URL.
        capabilities.sort_by(|a, b| a.id.cmp(&b.id));

        Ok(capabilities)
    }

    /// Load all capabilities across all origins
    pub fn load_all_origins(&self) -> Result<HashMap<String, Vec<ApiCapability>>, String> {
        let mut result = HashMap::new();

        if !self.path_exists(&self.base_path)? {
            return Ok(result);
        }

        let entries = self
            .workspace_layout
            .read_dir_path_sync(&self.base_path)
            .map_err(|e| format!("Failed to read api_mining dir: {}", e))?;

        for entry in entries {
            let path = self.entry_path(&entry);

            if !entry.is_dir {
                continue;
            }

            let caps_dir = path.join("capabilities");
            if !self.path_exists(&caps_dir)? {
                continue;
            }

            let origin_key = entry.file_name;

            // Load capabilities from this origin directory
            let caps_entries = self
                .workspace_layout
                .read_dir_path_sync(&caps_dir)
                .map_err(|e| format!("Failed to read capabilities dir: {}", e))?;

            let mut caps = Vec::new();
            for cap_entry in caps_entries {
                if !cap_entry.is_file {
                    continue;
                }
                let cap_path = self.entry_path(&cap_entry);

                if cap_path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }

                match self.workspace_layout.read_to_string_path_sync(&cap_path) {
                    Ok(content) => match serde_json::from_str::<ApiCapability>(&content) {
                        Ok(cap) => caps.push(cap),
                        Err(e) => {
                            warn!("Skipping corrupt capability {:?}: {}", cap_path, e);
                        },
                    },
                    Err(e) => {
                        warn!("Failed to read {:?}: {}", cap_path, e);
                    },
                }
            }

            if !caps.is_empty() {
                // Sort by ID for deterministic ordering (read_dir order is undefined)
                caps.sort_by(|a, b| a.id.cmp(&b.id));
                result.insert(origin_key, caps);
            }
        }

        Ok(result)
    }

    /// Delete a capability
    pub fn delete(&self, origin: &str, capability_id: &str) -> Result<(), String> {
        Self::validate_id(capability_id)?;
        let origin_key = Self::origin_to_key(origin);
        let filepath = self
            .capabilities_dir(&origin_key)
            .join(format!("{}.json", capability_id));

        match self.workspace_layout.remove_file_path_sync(&filepath) {
            Ok(()) => {},
            Err(error) if Self::is_not_found(&error) => {},
            Err(error) => return Err(format!("Failed to delete capability: {}", error)),
        }

        Ok(())
    }

    /// Delete all capability data for an origin.
    pub fn delete_origin(&self, origin: &str) -> Result<usize, String> {
        let origin_key = Self::origin_to_key(origin);
        let origin_dir = self.base_path.join(&origin_key);
        let count = self.count(origin)?;

        match self.workspace_layout.remove_dir_all_path_sync(&origin_dir) {
            Ok(()) => {},
            Err(error) if Self::is_not_found(&error) => {},
            Err(error) => return Err(format!("Failed to delete origin capabilities: {}", error)),
        }

        Ok(count)
    }

    /// Count capabilities for an origin
    pub fn count(&self, origin: &str) -> Result<usize, String> {
        let origin_key = Self::origin_to_key(origin);
        let dir = self.capabilities_dir(&origin_key);

        if !self.path_exists(&dir)? {
            return Ok(0);
        }

        let count = self
            .workspace_layout
            .read_dir_path_sync(&dir)
            .map_err(|e| format!("Failed to read capabilities dir: {}", e))?
            .into_iter()
            .filter(|e| e.is_file)
            .filter(|e| self.entry_path(e).extension().and_then(|s| s.to_str()) == Some("json"))
            .count();

        Ok(count)
    }

    /// Check if the origin has exceeded the capability limit
    pub fn is_at_capacity(&self, origin: &str) -> Result<bool, String> {
        Ok(self.count(origin)? >= self.max_capabilities_per_origin)
    }

    /// List all known origin keys
    pub fn list_origins(&self) -> Result<Vec<String>, String> {
        if !self.path_exists(&self.base_path)? {
            return Ok(Vec::new());
        }

        let entries = self
            .workspace_layout
            .read_dir_path_sync(&self.base_path)
            .map_err(|e| format!("Failed to read api_mining dir: {}", e))?;

        let origins: Vec<String> = entries
            .into_iter()
            .filter(|e| e.is_dir)
            .filter(|e| {
                self.path_exists(&self.entry_path(e).join("capabilities"))
                    .unwrap_or(false)
            })
            .map(|e| e.file_name)
            .collect();

        Ok(origins)
    }
}

impl Default for CapabilityStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::ApiCapability;
    use tempfile::TempDir;

    fn create_test_capability(name: &str, origin: &str) -> ApiCapability {
        ApiCapability::new(
            name.to_string(),
            origin.to_string(),
            "GET".to_string(),
            format!("https://{}/api/test", origin.replace("https://", "")),
        )
    }

    #[test]
    fn test_origin_to_key() {
        // All special chars replaced with underscore for filesystem safety
        let key = CapabilityStore::origin_to_key("https://mail.google.com");
        assert!(!key.contains('/'));
        assert!(!key.contains(".."));
        assert!(key.contains("mail"));
        assert!(key.contains("google"));
        assert!(key.contains("com"));

        let key2 = CapabilityStore::origin_to_key("https://example.com:8080");
        assert!(!key2.contains('/'));
        assert!(!key2.contains(':'));
        assert!(key2.contains("example"));
        assert!(key2.contains("8080"));
    }

    #[test]
    fn test_validate_id_rejects_traversal() {
        assert!(CapabilityStore::validate_id("../../../etc/passwd").is_err());
        assert!(CapabilityStore::validate_id("foo/bar").is_err());
        assert!(CapabilityStore::validate_id("foo\\bar").is_err());
        assert!(CapabilityStore::validate_id("").is_err());
        assert!(CapabilityStore::validate_id("valid-cap-id-123").is_ok());
    }

    #[test]
    fn test_save_and_load() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());

        let cap = create_test_capability("test_api", "https://example.com");
        let cap_id = cap.id.clone();

        store.save(&cap).unwrap();

        let loaded = store.load("https://example.com", &cap_id).unwrap();
        assert_eq!(loaded.id, cap_id);
        assert_eq!(loaded.name, "test_api");
    }

    #[test]
    fn test_load_all_for_origin() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());

        let cap1 = create_test_capability("api_1", "https://example.com");
        let cap2 = create_test_capability("api_2", "https://example.com");

        store.save(&cap1).unwrap();
        store.save(&cap2).unwrap();

        let all = store.load_all("https://example.com").unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_load_all_origins() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());

        store
            .save(&create_test_capability("api_1", "https://example.com"))
            .unwrap();
        store
            .save(&create_test_capability("api_2", "https://other.com"))
            .unwrap();

        let all = store.load_all_origins().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_delete() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());

        let cap = create_test_capability("test_api", "https://example.com");
        let cap_id = cap.id.clone();

        store.save(&cap).unwrap();
        assert_eq!(store.count("https://example.com").unwrap(), 1);

        store.delete("https://example.com", &cap_id).unwrap();
        assert_eq!(store.count("https://example.com").unwrap(), 0);
    }

    #[test]
    fn test_count_and_capacity() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());

        assert_eq!(store.count("https://example.com").unwrap(), 0);
        assert!(!store.is_at_capacity("https://example.com").unwrap());

        store
            .save(&create_test_capability("api_1", "https://example.com"))
            .unwrap();
        assert_eq!(store.count("https://example.com").unwrap(), 1);
    }

    #[test]
    fn test_list_origins() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());

        store
            .save(&create_test_capability("api_1", "https://example.com"))
            .unwrap();
        store
            .save(&create_test_capability("api_2", "https://other.com"))
            .unwrap();

        let origins = store.list_origins().unwrap();
        assert_eq!(origins.len(), 2);
    }

    #[test]
    fn test_empty_origin_returns_empty() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());

        let caps = store.load_all("https://nonexistent.com").unwrap();
        assert!(caps.is_empty());
    }
}
