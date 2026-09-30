use std::collections::BTreeSet;

use magician_storage::{StorageCatalogId, StorageError};

/// Owner-by-owner bypass ratchet. Disabled guards accept every site.
#[derive(Debug, Clone)]
pub struct SourceGuard {
    owner_id: StorageCatalogId,
    enabled: bool,
    allowlist: BTreeSet<String>,
}

impl SourceGuard {
    pub fn disabled(owner_id: StorageCatalogId) -> Self {
        Self {
            owner_id,
            enabled: false,
            allowlist: BTreeSet::new(),
        }
    }

    pub fn enabled(
        owner_id: StorageCatalogId,
        allowlist: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            owner_id,
            enabled: true,
            allowlist: allowlist.into_iter().map(Into::into).collect(),
        }
    }

    pub fn owner_id(&self) -> &StorageCatalogId {
        &self.owner_id
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn check(&self, site: &str) -> Result<(), StorageError> {
        if !self.enabled || self.allowlist.contains(site) {
            return Ok(());
        }
        Err(StorageError::PermissionDenied)
    }
}
