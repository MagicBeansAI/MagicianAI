//! Live API-mining master switch.
//!
//! The process config is a hard ceiling. A scope may turn itself off, but a
//! scope override can never turn mining on when the process owner disabled it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct RuntimeConfigCache {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    config: Option<crate::config::ApiMiningConfig>,
}

static RUNTIME_CONFIG_CACHE: OnceLock<Mutex<Option<RuntimeConfigCache>>> = OnceLock::new();

/// Read the authoritative runtime API-mining config without reparsing the
/// multi-thousand-line YAML on every browser primitive or recipe step. Atomic
/// config replacement changes metadata and refreshes this process cache.
pub fn runtime_api_mining_config() -> Option<crate::config::ApiMiningConfig> {
    let path = crate::config::magician_config_path();
    let metadata = std::fs::metadata(&path).ok();
    let modified = metadata.as_ref().and_then(|value| value.modified().ok());
    let len = metadata.as_ref().map_or(0, std::fs::Metadata::len);
    let cache = RUNTIME_CONFIG_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = guard.as_ref() {
        if cached.path == path && cached.modified == modified && cached.len == len {
            return cached.config.clone();
        }
    }
    let config = crate::config::load_magician_config_from_path(&path)
        .ok()
        .map(|config| config.api_mining.validated());
    *guard = Some(RuntimeConfigCache {
        path,
        modified,
        len,
        config: config.clone(),
    });
    config
}

fn invalidate_runtime_config_cache() {
    if let Some(cache) = RUNTIME_CONFIG_CACHE.get() {
        *cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SwitchState {
    pub effective: bool,
    pub process_enabled: bool,
    pub scope_override: Option<bool>,
    pub set_by: &'static str,
}

#[derive(Clone)]
pub struct ApiMiningSwitch {
    process_enabled: Arc<AtomicBool>,
    scope_cache: Arc<RwLock<HashMap<(String, String), Option<bool>>>>,
    scoped_base: Arc<dyn Fn(&str, &str) -> PathBuf + Send + Sync>,
}

impl ApiMiningSwitch {
    /// Stateless entry-point check for components that already own the scoped
    /// API-mining root but not the process handle (compiled providers and
    /// browser continuation code). Malformed settings fail closed.
    pub fn effective_from_disk(process_enabled: bool, scoped_base: &std::path::Path) -> bool {
        if !process_enabled {
            return false;
        }
        let path = scoped_base.join("settings.json");
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<ScopeSettings>(&text)
                .map(|settings| settings.enabled.unwrap_or(true))
                .unwrap_or(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        }
    }

    pub fn new(
        process_enabled: bool,
        scoped_base: impl Fn(&str, &str) -> PathBuf + Send + Sync + 'static,
    ) -> Self {
        Self {
            process_enabled: Arc::new(AtomicBool::new(process_enabled)),
            scope_cache: Arc::new(RwLock::new(HashMap::new())),
            scoped_base: Arc::new(scoped_base),
        }
    }

    pub fn set_process_enabled(&self, enabled: bool) {
        self.process_enabled.store(enabled, Ordering::SeqCst);
        invalidate_runtime_config_cache();
    }

    fn settings_path(&self, principal: &str, workspace: &str) -> PathBuf {
        (self.scoped_base)(principal, workspace).join("settings.json")
    }

    fn read_scope_override(&self, principal: &str, workspace: &str) -> Option<bool> {
        let key = (principal.to_owned(), workspace.to_owned());
        if let Some(cached) = self
            .scope_cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return *cached;
        }
        let value = match std::fs::read_to_string(self.settings_path(principal, workspace)) {
            Ok(text) => serde_json::from_str::<ScopeSettings>(&text)
                .map(|settings| settings.enabled)
                .unwrap_or(Some(false)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => Some(false),
        };
        self.scope_cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, value);
        value
    }

    pub fn state(&self, principal: &str, workspace: &str) -> SwitchState {
        let process_enabled = self.process_enabled.load(Ordering::SeqCst);
        let scope_override = self.read_scope_override(principal, workspace);
        let effective = process_enabled && scope_override.unwrap_or(true);
        let set_by = if !process_enabled {
            "config"
        } else if scope_override.is_some() {
            "scope"
        } else {
            "default"
        };
        SwitchState {
            effective,
            process_enabled,
            scope_override,
            set_by,
        }
    }

    pub fn effective(&self, principal: &str, workspace: &str) -> bool {
        self.state(principal, workspace).effective
    }

    pub fn set_scope(
        &self,
        principal: &str,
        workspace: &str,
        enabled: Option<bool>,
    ) -> std::io::Result<()> {
        let path = self.settings_path(principal, workspace);
        let settings = ScopeSettings {
            enabled,
            updated_at_ms: Some(chrono::Utc::now().timestamp_millis()),
        };
        let text = serde_json::to_string_pretty(&settings).map_err(std::io::Error::other)?;
        crate::magician_v2::runtime_settings::write_text_file_atomic(&path, &text, Some(0o600))?;
        self.scope_cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert((principal.to_owned(), workspace.to_owned()), enabled);
        Ok(())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn process_ceiling_and_scope_override_compose_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let switch = ApiMiningSwitch::new(false, move |principal, workspace| {
            root.join(principal).join(workspace)
        });
        switch.set_scope("p", "w", Some(true)).unwrap();
        let state = switch.state("p", "w");
        assert!(!state.effective);
        assert_eq!(state.set_by, "config");
        switch.set_process_enabled(true);
        assert!(switch.state("p", "w").effective);
        switch.set_scope("p", "w", Some(false)).unwrap();
        assert!(!switch.state("p", "w").effective);
        assert_eq!(switch.state("p", "w").set_by, "scope");
        switch.set_scope("p", "w", None).unwrap();
        assert!(switch.state("p", "w").effective);
        assert_eq!(switch.state("p", "w").set_by, "default");
    }

    #[test]
    fn malformed_scope_settings_fail_closed_on_both_read_paths() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("settings.json"), "not-json").unwrap();
        assert!(!ApiMiningSwitch::effective_from_disk(true, temp.path()));

        let root = temp.path().to_path_buf();
        let switch = ApiMiningSwitch::new(true, move |_, _| root.clone());
        assert!(!switch.effective("p", "w"));
        assert_eq!(switch.state("p", "w").scope_override, Some(false));
    }
}
