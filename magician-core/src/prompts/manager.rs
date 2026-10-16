// Prompt manager with version selection in code

use std::{collections::HashMap, sync::Arc};

use anyhow::{anyhow, Result};
use runtime_core::{Prompt, PromptStore};
use tracing::{debug, warn};

/// Process-global PromptManager handle — for code that runs OUTSIDE the
/// app_data dependency graph (bare HTTP routes like the screen endpoints,
/// media-rail sessions built in process-global registries). Mirrors the
/// `set_global_operation_router` pattern; set once at startup by the
/// builder.
static GLOBAL_PROMPT_MANAGER: std::sync::OnceLock<Arc<PromptManager>> = std::sync::OnceLock::new();

pub fn set_global_prompt_manager(manager: Arc<PromptManager>) {
    let _ = GLOBAL_PROMPT_MANAGER.set(manager);
}

pub fn global_prompt_manager() -> Option<Arc<PromptManager>> {
    GLOBAL_PROMPT_MANAGER.get().cloned()
}

/// The global manager, or the on-disk store when we are inside a test binary.
///
/// A test binary never runs the startup builder that installs the global, so
/// without a fallback every prompt-backed test fails on wiring rather than on
/// its subject. The fallback resolves `data/magician_v2/prompts` from
/// `CARGO_MANIFEST_DIR`, so it does not depend on the working directory.
///
/// # Why the condition is not just `test`
///
/// `#[cfg(test)]` is true only while **this** crate compiles its own tests. A
/// downstream crate's tests link magician-core as an ordinary dependency, where
/// it is false — so they got `global PromptManager is not initialized` instead
/// of the fallback this function appeared to promise, and the error named the
/// missing global rather than the reason it was missing.
///
/// `test-support` closes that gap. It is enabled through `[dev-dependencies]`,
/// which `cargo build` does not resolve, so a release build still fails loudly
/// on an uninstalled manager — which is the behaviour that matters in
/// production and must not be softened.
fn required_prompt_manager() -> Result<Arc<PromptManager>> {
    match global_prompt_manager() {
        Some(manager) => Ok(manager),
        None => {
            #[cfg(any(test, feature = "test-support"))]
            {
                Ok(Arc::new(PromptManager::new(Arc::new(
                    crate::prompts::JsonPromptStorage::with_default_config()?,
                ))))
            }
            #[cfg(not(any(test, feature = "test-support")))]
            {
                Err(anyhow!("global PromptManager is not initialized"))
            }
        },
    }
}

/// Load a named prompt through the process-global managed store without a
/// compiled fallback.
pub async fn managed_prompt(name: &str, version: &str) -> Result<Prompt> {
    required_prompt_manager()?.get_prompt(name, version).await
}

/// Render a named prompt through the process-global manager without a compiled
/// prompt fallback. Background rails use this when a missing managed prompt
/// must degrade the operation rather than silently changing its contract.
pub async fn rendered_prompt(
    name: &str,
    version: &str,
    variables: HashMap<String, String>,
) -> Result<String> {
    let manager = required_prompt_manager()?;
    manager.get_rendered_prompt(name, version, variables).await
}

/// Render a named prompt via the global manager, falling back to the
/// caller's compiled-in text when the manager is unset or the template is
/// missing/unrenderable. The store (`data/magician_v2/prompts/`) is the
/// editable source of truth; the fallback exists so a missing file
/// degrades a long-running rail or endpoint to known-good behavior
/// instead of bricking it — loudly, via the warn.
pub async fn rendered_prompt_or(
    name: &str,
    version: &str,
    variables: HashMap<String, String>,
    fallback: &str,
) -> String {
    let Some(manager) = global_prompt_manager() else {
        warn!(
            "[MAGICIAN-V2-PROMPTS] global PromptManager unset — using compiled fallback for '{name}'"
        );
        return fallback.to_string();
    };
    match manager.get_rendered_prompt(name, version, variables).await {
        Ok(rendered) => rendered,
        Err(error) => {
            warn!(
                "[MAGICIAN-V2-PROMPTS] prompt '{name}' v{version} unavailable ({error}) — using compiled fallback"
            );
            fallback.to_string()
        },
    }
}

/// Prompt manager for MagicianV2 with version selection in code
pub struct PromptManager {
    storage: Arc<dyn PromptStore>,
    cache: dashmap::DashMap<(String, String), Prompt>,
}

impl PromptManager {
    /// Create a new prompt manager
    pub fn new(storage: Arc<dyn PromptStore>) -> Self {
        Self {
            storage,
            cache: dashmap::DashMap::new(),
        }
    }

    /// Initialize the prompt manager and storage
    pub async fn initialize(&self) -> Result<()> {
        debug!("[MAGICIAN-V2-PROMPTS] Initializing PromptManager...");

        // Initialize storage backend
        self.storage.initialize().await?;

        // Health check
        if !self.storage.health_check().await? {
            return Err(anyhow!("Storage health check failed"));
        }

        // Log available prompts
        match self.storage.list_prompt_names().await {
            Ok(names) => {
                debug!(
                    "[MAGICIAN-V2-PROMPTS] Available prompts in storage: {:?}",
                    names
                );
                for name in &names {
                    if let Ok(versions) = self.storage.list_versions(name).await {
                        debug!(
                            "[MAGICIAN-V2-PROMPTS] Prompt '{}' has versions: {:?}",
                            name, versions
                        );
                    }
                }
            },
            Err(e) => {
                warn!(
                    "[MAGICIAN-V2-PROMPTS] Failed to list prompts from storage: {}",
                    e
                );
            },
        }

        debug!("[MAGICIAN-V2-PROMPTS] PromptManager initialized successfully");
        Ok(())
    }

    /// Get a prompt with version specified in code
    pub async fn get_prompt(&self, name: &str, version: &str) -> Result<Prompt> {
        let key = (name.to_string(), version.to_string());
        if let Some(cached) = self.cache.get(&key) {
            return Ok(cached.clone());
        }

        match self.storage.get_prompt(name, version).await {
            Ok(prompt) => {
                debug!(
                    "[MAGICIAN-V2-PROMPTS] Loaded prompt '{}' v{} from storage",
                    name, version
                );
                self.cache.insert(key, prompt.clone());
                Ok(prompt)
            },
            Err(e) => Err(anyhow!(
                "Prompt '{}' v{} not found in storage: {}",
                name,
                version,
                e
            )),
        }
    }

    /// Get a rendered prompt with variable substitution
    pub async fn get_rendered_prompt(
        &self,
        name: &str,
        version: &str,
        variables: HashMap<String, String>,
    ) -> Result<String> {
        // Values may contain private messages, memory, or credentials. Log
        // only variable names and character counts, never prompt inputs.
        let variable_summary: Vec<String> = variables
            .iter()
            .map(|(key, value)| format!("{key}=<{} chars>", value.chars().count()))
            .collect();

        tracing::debug!(
            "[MAGICIAN-V2-PROMPTS] Rendering prompt '{}' v{} with {} variables: {}",
            name,
            version,
            variables.len(),
            variable_summary.join(", ")
        );

        let prompt = self.get_prompt(name, version).await?;
        prompt.render(&variables)
    }

    /// Check if a prompt exists in storage
    pub async fn prompt_exists(&self, name: &str, version: &str) -> bool {
        self.storage
            .prompt_exists(name, version)
            .await
            .unwrap_or(false)
    }

    /// List available prompt names
    pub async fn list_prompt_names(&self) -> Result<Vec<String>> {
        let mut names = self.storage.list_prompt_names().await?;
        names.sort();
        Ok(names)
    }

    /// List available versions for a prompt
    pub async fn list_versions(&self, name: &str) -> Result<Vec<String>> {
        self.storage.list_versions(name).await
    }

    /// Save a new prompt version (for development/management tools)
    pub async fn save_prompt(&self, prompt: &Prompt) -> Result<()> {
        self.storage.save_prompt(prompt).await
    }
}
