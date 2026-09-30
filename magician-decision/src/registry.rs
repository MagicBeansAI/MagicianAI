//! The models loaded in this process for one set of settings.
//!
//! In-process models (`laya-onnx`, `laya-mlx`, `kev-onnx`, `kev-mlx`) are
//! loaded once per settings and shared by every operation and locality
//! runtime that routes to them. When the settings change, the next
//! registry is built [`ModelRegistry::reusing`] the current one: a model
//! whose entry is unchanged moves across without a reload, a new one loads,
//! and whatever the new settings no longer route to is released
//! ([`ModelRegistry::release_unused`]). A released model is freed when the
//! last request still holding it finishes. Nothing is evicted for being
//! idle: a routed model stays loaded until the settings stop routing to it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use tracing::info;

use crate::model::StructuredDecisionModel;

struct Entry {
    /// For logs: adapter and model id.
    label: String,
    model: Arc<dyn StructuredDecisionModel>,
}

#[derive(Default)]
pub struct ModelRegistry {
    admission: Arc<Mutex<BTreeMap<String, Weak<crate::admission::ModelAdmission>>>>,
    loaded: Mutex<BTreeMap<String, Entry>>,
    /// The previous settings' models, until claimed or released.
    reusable: Mutex<BTreeMap<String, Entry>>,
}

impl ModelRegistry {
    pub fn admission(
        &self,
        config: &crate::config::DecisionModelConfig,
    ) -> Arc<crate::admission::ModelAdmission> {
        let identity = config.identity_config();
        let key = serde_json::to_string(&identity).expect("model config serializes");
        let mut entries = self.admission.lock().unwrap_or_else(|p| p.into_inner());
        entries.retain(|_, entry| entry.strong_count() > 0);
        let admission = entries
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let admission =
                    Arc::new(crate::admission::ModelAdmission::new(config.max_in_flight));
                entries.insert(key, Arc::downgrade(&admission));
                admission
            });
        admission.configure(
            config.max_in_flight,
            config.queue_capacity,
            config.queue_max_bytes,
        );
        admission
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// An empty registry that takes over `previous`'s models on request
    /// instead of loading them again.
    pub fn reusing(previous: &ModelRegistry) -> Self {
        let reusable = previous
            .loaded
            .lock()
            .map(|loaded| {
                loaded
                    .iter()
                    .map(|(key, entry)| {
                        (
                            key.clone(),
                            Entry {
                                label: entry.label.clone(),
                                model: Arc::clone(&entry.model),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            admission: previous.admission.clone(),
            loaded: Mutex::default(),
            reusable: Mutex::new(reusable),
        }
    }

    /// The model `key` names (the adapter and every setting that changes
    /// what is loaded): already loaded here, carried over from the previous
    /// settings, or loaded now by `load`.
    pub fn get_or_load(
        &self,
        key: String,
        label: &str,
        load: impl FnOnce() -> Result<Arc<dyn StructuredDecisionModel>, String>,
    ) -> Result<Arc<dyn StructuredDecisionModel>, String> {
        let mut loaded = self
            .loaded
            .lock()
            .map_err(|_| "decision model registry poisoned".to_string())?;
        if let Some(entry) = loaded.get(&key) {
            return Ok(Arc::clone(&entry.model));
        }
        let carried = self
            .reusable
            .lock()
            .map_err(|_| "decision model registry poisoned".to_string())?
            .remove(&key);
        let entry = match carried {
            Some(entry) => {
                info!(model = %entry.label, "decision model kept loaded");
                entry
            },
            None => Entry {
                label: label.to_string(),
                model: load()?,
            },
        };
        let model = Arc::clone(&entry.model);
        loaded.insert(key, entry);
        Ok(model)
    }

    /// Release the previous settings' models that these settings did not
    /// claim, returning their labels.
    pub fn release_unused(&self) -> Vec<String> {
        let released = self
            .reusable
            .lock()
            .map(|mut reusable| std::mem::take(&mut *reusable))
            .unwrap_or_default();
        released
            .into_values()
            .map(|entry| {
                info!(model = %entry.label, "decision model unloaded (no longer routed)");
                entry.label
            })
            .collect()
    }

    /// Labels of the models loaded for these settings.
    pub fn loaded(&self) -> Vec<String> {
        self.loaded
            .lock()
            .map(|loaded| loaded.values().map(|entry| entry.label.clone()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::adapters::MemoryDecisionModel;

    fn model() -> Arc<dyn StructuredDecisionModel> {
        Arc::new(MemoryDecisionModel::new("memory", "memory"))
    }

    #[test]
    fn unchanged_models_carry_over_and_dropped_ones_are_released() {
        let loads = AtomicUsize::new(0);
        let load = || {
            loads.fetch_add(1, Ordering::SeqCst);
            Ok(model())
        };
        let first = ModelRegistry::new();
        let kept = first.get_or_load("a".into(), "a", load).unwrap();
        first.get_or_load("b".into(), "b", load).unwrap();
        // Shared within one set of settings.
        let again = first.get_or_load("a".into(), "a", load).unwrap();
        assert!(Arc::ptr_eq(&kept, &again));
        assert_eq!(loads.load(Ordering::SeqCst), 2);

        let second = ModelRegistry::reusing(&first);
        let carried = second.get_or_load("a".into(), "a", load).unwrap();
        second.get_or_load("c".into(), "c", load).unwrap();
        assert!(Arc::ptr_eq(&kept, &carried), "a is not reloaded");
        assert_eq!(loads.load(Ordering::SeqCst), 3, "only c loads");
        assert_eq!(second.release_unused(), vec!["b".to_string()]);
        assert_eq!(second.loaded(), vec!["a".to_string(), "c".to_string()]);

        // Once the previous settings are gone, b has no owner left.
        let b = first.get_or_load("b".into(), "b", load).unwrap();
        drop(first);
        drop(second);
        assert_eq!(Arc::strong_count(&b), 1);
    }

    #[test]
    fn a_failed_load_is_not_cached() {
        let registry = ModelRegistry::new();
        assert!(registry
            .get_or_load("a".into(), "a", || Err("missing".into()))
            .is_err());
        assert!(registry
            .get_or_load("a".into(), "a", || Ok(model()))
            .is_ok());
    }
}
