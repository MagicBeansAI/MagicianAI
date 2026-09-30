//! Magician compatibility facade over the shared custody implementation.
//! Scope layout and the typed runtime receipt remain product-owned; no second
//! store, cache, writer, or execution path is introduced by this module.

use super::runtime_credential_audit::RuntimeCredentialAuditReceipt;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use std::path::{Path, PathBuf};

pub use magicvault_core::one_time::{
    OneTimeBinding, OneTimeClaim, OneTimeError, OneTimeReceipt, OneTimeReservation, OneTimeState,
    OneTimeTransition, PreDispatchFailure, ONE_TIME_MAX_RETENTION_MS,
};
#[cfg(any(test, feature = "test-fixtures"))]
pub use magicvault_core::store::ManualClock;
pub use magicvault_core::store::{
    AuthStatusMetadata, CapturedSessionLease, CapturedSessionTarget, CustodyClock,
    PendingSecretApproval, ProvisionedSecretMetadata, SecretFeatureStatus, SecretFeatureSupport,
    SecretListEntry, SecretPartitionStatus, SecretRuntimeCapabilities, SecretSourceKind,
    SecretStore, SecretStoreError, SystemClock,
};
// Only the audit tests name the journal file; production code reaches it
// through the store. The gate mirrors the consuming `mod tests` exactly,
// so enabling `test-fixtures` without `cfg(test)` still resolves it.
#[cfg(any(test, feature = "test-fixtures"))]
pub(super) use magicvault_core::store::SECRET_AUDIT_FILENAME;

pub type SecretAuditEvent = magicvault_core::store::AuditEvent<RuntimeCredentialAuditReceipt>;
pub type SecretStoreResolver = magicvault_core::store::SecretStoreResolver<ArtifactV2Workspace>;

impl magicvault_core::store::AuditReceipt for RuntimeCredentialAuditReceipt {}

impl magicvault_core::store::SecretScopeLayout for ArtifactV2Workspace {
    fn from_base_root(base_root: &Path) -> Self {
        Self::new(Self::resolve_scoped_root(base_root))
    }

    fn secrets_root(&self, principal: &str, workspace: &str) -> PathBuf {
        ArtifactV2Workspace::secrets_root(self, principal, workspace)
    }
}
