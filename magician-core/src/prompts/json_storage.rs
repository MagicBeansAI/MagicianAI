// JSON file-based implementation of the PromptStore trait

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};
use tracing::{debug, error, info, warn};

use crate::durable_io::write_bytes_durably;
use crate::prompts::{
    storage::{PromptStore, StorageConfig},
    types::Prompt,
};

/// Configuration for JSON-based prompt storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonStorageConfig {
    /// Base directory for storing prompt files
    pub storage_dir: PathBuf,

    /// Whether to enable caching
    pub enable_cache: bool,

    /// Maximum cache size
    pub max_cache_entries: usize,
}

impl Default for JsonStorageConfig {
    fn default() -> Self {
        Self {
            storage_dir: default_prompt_dir(),
            enable_cache: true,
            max_cache_entries: 100,
        }
    }
}

impl StorageConfig for JsonStorageConfig {
    fn validate(&self) -> Result<()> {
        if self.max_cache_entries == 0 {
            return Err(anyhow!("max_cache_entries must be greater than 0"));
        }
        Ok(())
    }

    fn storage_type(&self) -> &str {
        "json_file"
    }
}

/// In-memory cache entry
#[derive(Debug, Clone)]
struct CacheEntry {
    prompt: Prompt,
    last_accessed: std::time::Instant,
}

/// JSON file-based prompt storage implementation
pub struct JsonPromptStorage {
    config: JsonStorageConfig,
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
}

fn packaged_prompt_dir(executable: &Path) -> Option<PathBuf> {
    executable
        .parent()
        .map(|parent| parent.join("data/magician_v2/prompts"))
}

/// Returns the existing packaged or development prompt directory.
///
/// Release/container binaries live beside `data/`. The compile-time manifest
/// path is a development fallback only; using it first bakes the builder's
/// absolute checkout path into otherwise portable binaries. The process
/// working directory is intentionally not searched because it is caller
/// controlled and must not shadow packaged prompts.
pub fn default_prompt_dir() -> PathBuf {
    let executable_dir = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(packaged_prompt_dir);
    let development_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/magician_v2/prompts");

    executable_dir
        .into_iter()
        .chain(std::iter::once(development_dir.clone()))
        .find(|candidate| candidate.is_dir())
        .unwrap_or(development_dir)
}

impl JsonPromptStorage {
    /// Create a new JSON storage instance
    pub fn new(config: JsonStorageConfig) -> Result<Self> {
        config.validate()?;

        Ok(Self {
            config,
            cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Create with default configuration
    pub fn with_default_config() -> Result<Self> {
        Self::new(JsonStorageConfig::default())
    }

    /// Get the file path for a prompt
    fn get_prompt_path(&self, name: &str, version: &str) -> PathBuf {
        self.config
            .storage_dir
            .join(format!("{}_v{}.json", name, version))
    }

    /// Get cache key for a prompt
    fn cache_key(&self, name: &str, version: &str) -> String {
        format!("{}:{}", name, version)
    }

    /// Load prompt from file
    async fn load_from_file(&self, name: &str, version: &str) -> Result<Prompt> {
        let file_path = self.get_prompt_path(name, version);

        if !file_path.exists() {
            return Err(anyhow!(
                "Prompt file not found: {} v{} at {}",
                name,
                version,
                file_path.display()
            ));
        }

        let content = fs::read_to_string(&file_path)
            .await
            .map_err(|e| anyhow!("Failed to read prompt file {}: {}", file_path.display(), e))?;

        let prompt: Prompt = serde_json::from_str(&content)
            .map_err(|e| anyhow!("Failed to parse prompt file {}: {}", file_path.display(), e))?;

        // Validate that the prompt matches requested name/version
        if prompt.name != name || prompt.version != version {
            return Err(anyhow!(
                "Prompt file mismatch: expected {} v{}, got {} v{}",
                name,
                version,
                prompt.name,
                prompt.version
            ));
        }

        debug!(
            "[MAGICIAN-V2-PROMPTS] Loaded prompt {} v{} from file",
            name, version
        );
        Ok(prompt)
    }

    /// Add prompt to cache
    async fn cache_prompt(&self, prompt: &Prompt) {
        if !self.config.enable_cache {
            return;
        }

        let mut cache = self.cache.write().await;
        let key = self.cache_key(&prompt.name, &prompt.version);

        // Evict oldest entries if cache is full
        if cache.len() >= self.config.max_cache_entries {
            if let Some(oldest_key) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.last_accessed)
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest_key);
                debug!("[MAGICIAN-V2-PROMPTS] Evicted cache entry: {}", oldest_key);
            }
        }

        cache.insert(
            key.clone(),
            CacheEntry {
                prompt: prompt.clone(),
                last_accessed: std::time::Instant::now(),
            },
        );

        debug!("[MAGICIAN-V2-PROMPTS] Cached prompt: {}", key);
    }

    /// Get prompt from cache
    async fn get_from_cache(&self, name: &str, version: &str) -> Option<Prompt> {
        if !self.config.enable_cache {
            return None;
        }

        let mut cache = self.cache.write().await;
        let key = self.cache_key(name, version);

        if let Some(entry) = cache.get_mut(&key) {
            entry.last_accessed = std::time::Instant::now();
            debug!("[MAGICIAN-V2-PROMPTS] Cache hit for prompt: {}", key);
            Some(entry.prompt.clone())
        } else {
            debug!("[MAGICIAN-V2-PROMPTS] Cache miss for prompt: {}", key);
            None
        }
    }

    /// List all JSON files in the storage directory
    async fn list_json_files(&self) -> Result<Vec<PathBuf>> {
        if !self.config.storage_dir.exists() {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        let mut entries = fs::read_dir(&self.config.storage_dir).await.map_err(|e| {
            anyhow!(
                "Failed to read storage directory {}: {}",
                self.config.storage_dir.display(),
                e
            )
        })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| anyhow!("Failed to read directory entry: {}", e))?
        {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }

        Ok(files)
    }

    /// Parse prompt name and version from filename
    fn parse_filename(&self, path: &Path) -> Option<(String, String)> {
        let file_name = path.file_stem()?.to_str()?;

        // Expected format: {name}_v{version}
        if let Some(v_pos) = file_name.rfind("_v") {
            let name = file_name[..v_pos].to_string();
            let version = file_name[v_pos + 2..].to_string();
            Some((name, version))
        } else {
            None
        }
    }
}

#[async_trait]
impl PromptStore for JsonPromptStorage {
    async fn get_prompt(&self, name: &str, version: &str) -> Result<Prompt> {
        // Try cache first
        if let Some(prompt) = self.get_from_cache(name, version).await {
            return Ok(prompt);
        }

        // Load from file
        let prompt = self.load_from_file(name, version).await?;

        // Cache the loaded prompt
        self.cache_prompt(&prompt).await;

        Ok(prompt)
    }

    async fn list_versions(&self, name: &str) -> Result<Vec<String>> {
        let files = self.list_json_files().await?;
        let mut versions = Vec::new();

        for file in files {
            if let Some((file_name, version)) = self.parse_filename(&file) {
                if file_name == name {
                    versions.push(version);
                }
            }
        }

        // Sort versions (simple string sort - could be improved with semantic
        // versioning)
        versions.sort();
        Ok(versions)
    }

    async fn list_prompt_names(&self) -> Result<Vec<String>> {
        let files = self.list_json_files().await?;
        let mut names = std::collections::HashSet::new();

        for file in files {
            if let Some((name, _)) = self.parse_filename(&file) {
                names.insert(name);
            }
        }

        let mut result: Vec<String> = names.into_iter().collect();
        result.sort();
        Ok(result)
    }

    async fn save_prompt(&self, prompt: &Prompt) -> Result<()> {
        let file_path = self.get_prompt_path(&prompt.name, &prompt.version);

        // Ensure directory exists
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                anyhow!(
                    "Failed to create storage directory {}: {}",
                    parent.display(),
                    e
                )
            })?;
        }

        // Serialize prompt to JSON
        let json_content = serde_json::to_string_pretty(prompt).map_err(|e| {
            anyhow!(
                "Failed to serialize prompt {} v{}: {}",
                prompt.name,
                prompt.version,
                e
            )
        })?;

        // Write to file — durably, because a truncated prompt does not surface
        // as an error anywhere a caller can see. `load_from_file` fails the
        // parse, `rendered_prompt_or` catches that and substitutes the
        // compiled-in fallback, and the model runs on different text with only
        // a `warn!` to show for it.
        write_bytes_durably(&file_path, json_content.as_bytes())
            .await
            .map_err(|e| anyhow!("Failed to write prompt file {}: {}", file_path.display(), e))?;

        // Update cache
        self.cache_prompt(prompt).await;

        debug!(
            "[MAGICIAN-V2-PROMPTS] Saved prompt {} v{} to {}",
            prompt.name,
            prompt.version,
            file_path.display()
        );
        Ok(())
    }

    async fn prompt_exists(&self, name: &str, version: &str) -> Result<bool> {
        // Check cache first
        if self.get_from_cache(name, version).await.is_some() {
            return Ok(true);
        }

        // Check file system
        let file_path = self.get_prompt_path(name, version);
        Ok(file_path.exists())
    }

    async fn latest_version(&self, name: &str) -> Result<String> {
        let versions = self.list_versions(name).await?;

        if versions.is_empty() {
            return Err(anyhow!("No versions found for prompt: {}", name));
        }

        let mut parsed: Vec<(Version, String)> = Vec::new();
        for version in &versions {
            let trimmed = version.trim_start_matches(['v', 'V']);
            if let Ok(parsed_version) = Version::parse(trimmed) {
                parsed.push((parsed_version, version.clone()));
            }
        }

        if !parsed.is_empty() {
            parsed.sort_by(|a, b| a.0.cmp(&b.0));
            return Ok(parsed.last().unwrap().1.clone());
        }

        // Fall back to lexicographic ordering when no semantic versions parse cleanly.
        Ok(versions.into_iter().last().unwrap())
    }

    async fn delete_prompt(&self, name: &str, version: &str) -> Result<()> {
        let file_path = self.get_prompt_path(name, version);

        if !file_path.exists() {
            return Err(anyhow!("Prompt file not found: {} v{}", name, version));
        }

        fs::remove_file(&file_path).await.map_err(|e| {
            anyhow!(
                "Failed to delete prompt file {}: {}",
                file_path.display(),
                e
            )
        })?;

        // Remove from cache
        if self.config.enable_cache {
            let mut cache = self.cache.write().await;
            let key = self.cache_key(name, version);
            cache.remove(&key);
            debug!("[MAGICIAN-V2-PROMPTS] Removed from cache: {}", key);
        }

        info!("[MAGICIAN-V2-PROMPTS] Deleted prompt {} v{}", name, version);
        Ok(())
    }

    async fn initialize(&self) -> Result<()> {
        // Create storage directory if it doesn't exist
        if !self.config.storage_dir.exists() {
            fs::create_dir_all(&self.config.storage_dir)
                .await
                .map_err(|e| {
                    anyhow!(
                        "Failed to create storage directory {}: {}",
                        self.config.storage_dir.display(),
                        e
                    )
                })?;
            debug!(
                "[MAGICIAN-V2-PROMPTS] Created storage directory: {}",
                self.config.storage_dir.display()
            );
        }

        // Validate existing files
        let files = self.list_json_files().await?;
        let mut valid_count = 0;
        let mut invalid_count = 0;

        for file in files {
            if let Some((name, version)) = self.parse_filename(&file) {
                match self.load_from_file(&name, &version).await {
                    Ok(_) => valid_count += 1,
                    Err(e) => {
                        invalid_count += 1;
                        warn!(
                            "[MAGICIAN-V2-PROMPTS] Invalid prompt file {}: {}",
                            file.display(),
                            e
                        );
                    },
                }
            } else {
                invalid_count += 1;
                warn!(
                    "[MAGICIAN-V2-PROMPTS] Invalid filename format: {}",
                    file.display()
                );
            }
        }

        debug!(
            "[MAGICIAN-V2-PROMPTS] JsonPromptStorage initialized: {} valid prompts, {} invalid \
             files",
            valid_count, invalid_count
        );

        if invalid_count > 0 {
            warn!(
                "[MAGICIAN-V2-PROMPTS] Found {} invalid prompt files. Consider cleaning up the \
                 storage directory.",
                invalid_count
            );
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<bool> {
        // Check if storage directory is accessible
        if !self.config.storage_dir.exists() {
            return Ok(false);
        }

        // Try to create a test file
        let test_file = self.config.storage_dir.join(".health_check");
        match fs::write(&test_file, "health_check").await {
            Ok(_) => {
                // Clean up test file
                let _ = fs::remove_file(&test_file).await;
                Ok(true)
            },
            Err(e) => {
                error!("[MAGICIAN-V2-PROMPTS] Health check failed: {}", e);
                Ok(false)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_prompt_directory_is_executable_relative() {
        assert_eq!(
            packaged_prompt_dir(Path::new("/app/magician.bin")),
            Some(PathBuf::from("/app/data/magician_v2/prompts"))
        );
    }

    #[test]
    fn development_prompt_directory_exists() {
        assert!(default_prompt_dir().is_dir());
    }

    #[tokio::test]
    async fn every_checked_in_prompt_file_loads() {
        let storage = JsonPromptStorage::new(JsonStorageConfig {
            storage_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../data/magician_v2/prompts"),
            enable_cache: false,
            max_cache_entries: 8,
        })
        .expect("storage");
        let files = storage.list_json_files().await.expect("prompt directory");
        assert!(!files.is_empty(), "checked-in prompt directory is empty");
        let mut invalid = Vec::new();
        for file in files {
            let Some((name, version)) = storage.parse_filename(&file) else {
                invalid.push(format!("{}: invalid filename", file.display()));
                continue;
            };
            if let Err(error) = storage.load_from_file(&name, &version).await {
                invalid.push(format!("{}: {error}", file.display()));
            }
        }
        assert!(
            invalid.is_empty(),
            "checked-in prompt files must all load:\n{}",
            invalid.join("\n")
        );
    }

    #[tokio::test]
    async fn save_prompt_publishes_atomically_and_leaves_no_staging_file() {
        use crate::prompts::types::PromptCategory;

        let temp = tempfile::tempdir().expect("tempdir");
        let storage = JsonPromptStorage::new(JsonStorageConfig {
            storage_dir: temp.path().to_path_buf(),
            // Cache off, so the read below genuinely goes back to disk.
            enable_cache: false,
            max_cache_entries: 8,
        })
        .expect("storage");
        let prompt = Prompt::new(
            "durability_probe".to_string(),
            "1.0.0".to_string(),
            "hello there".to_string(),
            PromptCategory::General,
            "durability probe".to_string(),
            "test".to_string(),
        );

        storage.save_prompt(&prompt).await.expect("save prompt");

        let mut names: Vec<String> = std::fs::read_dir(temp.path())
            .expect("storage listing")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["durability_probe_v1.0.0.json".to_string()],
            "the durable write must publish exactly one file, with no staging sibling"
        );

        let reloaded = storage
            .get_prompt("durability_probe", "1.0.0")
            .await
            .expect("published prompt must parse");
        assert_eq!(reloaded.content, "hello there");
    }
}
