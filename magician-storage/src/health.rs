use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageHealth {
    pub capability: String,
    pub status: HealthStatus,
    pub safe_detail: String,
}

#[derive(Debug, Default)]
pub struct StorageHealthRegistry {
    inner: Mutex<BTreeMap<String, StorageHealth>>,
}

impl StorageHealthRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, health: StorageHealth) {
        let mut inner = self.inner.lock().expect("health registry lock");
        inner.insert(health.capability.clone(), health);
    }

    pub fn snapshot(&self) -> Vec<StorageHealth> {
        self.inner
            .lock()
            .expect("health registry lock")
            .values()
            .cloned()
            .collect()
    }
}
