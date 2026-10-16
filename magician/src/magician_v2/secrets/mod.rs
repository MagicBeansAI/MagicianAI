use std::sync::{Arc, OnceLock};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroize;

pub mod broker;
pub mod challenge;
pub mod classify;
pub mod credential_lifecycle_executor;
pub mod credential_material_adapter;
pub mod encryption;
pub mod injection;
pub mod mcp_oauth_vault;
pub mod operator_profile_adapter;
pub mod policy;
pub mod runtime_credential_audit;
pub mod sinks;
pub mod store;

pub use broker::{
    with_secret_scope, BrokerAccessRequest, BrokerAccessResponse, CapabilitySecretBroker,
    LocalSecretBroker, ScopedSecretBroker, SecretBroker, SecretBrokerError,
};
pub use encryption::{
    decrypt, durable_platform_key_provider, encrypt, platform_key_provider, InMemoryKeyProvider,
    KeychainProvider, MasterKeyProvider, SecretEncryptionError,
};
pub use injection::{
    cookie_domain_matches_host, detect_jwt_expiry, filter_cookies_for_url, has_unresolved_refs,
    inject, known_value_replacements, resolve_inline_placeholders, sanitize_json_for_provider,
    sanitize_json_for_provider_owned, sanitize_result, sanitize_text_for_provider,
    CookieWithMetadata, InjectionError, KnownSecretValues, SameSite,
};
pub use policy::{
    check_policy, find_unsupported_provisioned_policy_routes, provisioned_policy_route_catalog,
    provisioned_policy_routes_for_target, AccessRequest, ApprovalChallenge, ApprovalTable,
    GrantBinding, PolicyResult, ProvisionedPolicyRouteCatalog, SecretPolicy, UsageTracker,
};
#[cfg(any(test, feature = "test-fixtures"))]
pub use store::ManualClock;
pub use store::{
    AuthStatusMetadata, CapturedSessionLease, CapturedSessionTarget, CustodyClock, OneTimeBinding,
    OneTimeClaim, OneTimeError, OneTimeReceipt, OneTimeReservation, OneTimeState,
    OneTimeTransition, PreDispatchFailure, ProvisionedSecretMetadata, SecretAuditEvent,
    SecretFeatureStatus, SecretFeatureSupport, SecretListEntry, SecretPartitionStatus,
    SecretRuntimeCapabilities, SecretSourceKind, SecretStore, SecretStoreError,
    SecretStoreResolver, SystemClock, ONE_TIME_MAX_RETENTION_MS,
};

const APP_CONTROL_PLANE_KEY_DOMAIN: &[u8] = b"magician.app-control-plane.master.v1";
type HmacSha256 = Hmac<Sha256>;

/// Process-owned signer for app control-plane records. The key is derived
/// once from the durable OS-backed master key and never serialized or written
/// into workspace storage. Domain separation keeps app seals independent of
/// encrypted secret material even though both share the host root of trust.
pub struct AppControlPlaneSigner {
    key: [u8; 32],
}

impl std::fmt::Debug for AppControlPlaneSigner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppControlPlaneSigner")
            .finish_non_exhaustive()
    }
}

impl Drop for AppControlPlaneSigner {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl AppControlPlaneSigner {
    fn from_master_key(master_key: &[u8; 32]) -> Self {
        let mut mac = HmacSha256::new_from_slice(master_key).expect("valid HMAC key length");
        mac.update(APP_CONTROL_PLANE_KEY_DOMAIN);
        let mut key = [0_u8; 32];
        key.copy_from_slice(&mac.finalize().into_bytes());
        Self { key }
    }

    pub fn fingerprint(&self, domain: &str, bytes: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("valid HMAC key length");
        mac.update(&(domain.len() as u64).to_be_bytes());
        mac.update(domain.as_bytes());
        mac.update(&(bytes.len() as u64).to_be_bytes());
        mac.update(bytes);
        hex::encode(mac.finalize().into_bytes())
    }
}

static APP_CONTROL_PLANE_SIGNER: OnceLock<Arc<AppControlPlaneSigner>> = OnceLock::new();

/// Install the one boot-derived signer before any protected app sidecar or
/// pause is loaded. A different second signer is rejected rather than allowing
/// two authority domains in one process.
pub fn install_app_control_plane_signer(
    signer: Arc<AppControlPlaneSigner>,
) -> Result<(), &'static str> {
    if let Some(existing) = APP_CONTROL_PLANE_SIGNER.get() {
        let challenge = b"magician.app-control-plane.install-check.v1";
        return (existing.fingerprint("install-check", challenge)
            == signer.fingerprint("install-check", challenge))
        .then_some(())
        .ok_or("a different app control-plane signer is already installed");
    }
    APP_CONTROL_PLANE_SIGNER
        .set(signer)
        .map_err(|_| "a different app control-plane signer won the install race")
}

pub fn app_control_plane_signer() -> Option<Arc<AppControlPlaneSigner>> {
    if let Some(signer) = APP_CONTROL_PLANE_SIGNER.get() {
        return Some(Arc::clone(signer));
    }
    #[cfg(any(test, feature = "test-fixtures"))]
    {
        let signer = APP_CONTROL_PLANE_SIGNER
            .get_or_init(|| Arc::new(AppControlPlaneSigner::from_master_key(&[0xA4; 32])));
        return Some(Arc::clone(signer));
    }
    #[cfg(not(any(test, feature = "test-fixtures")))]
    None
}

/// Startup snapshot for the shared secret runtime.
///
/// The planner-visible catalog and executor-visible runtime must be derived
/// from the same capability decision during startup. Otherwise the model can be
/// taught a vault/treasurer path that the executor never registers.
pub struct SecretRuntimeBootstrap {
    key_provider: Box<dyn MasterKeyProvider>,
    app_control_plane_signer: Option<Arc<AppControlPlaneSigner>>,
    capabilities: SecretRuntimeCapabilities,
    startup_warning: Option<String>,
}

impl SecretRuntimeBootstrap {
    /// Install the durable app control-plane signer derived during this exact
    /// bootstrap probe. `Ok(false)` means the OS-backed provider was
    /// unavailable, so protected app admission must remain fail-closed.
    pub fn install_app_control_plane_signer(&self) -> Result<bool, &'static str> {
        let Some(signer) = self.app_control_plane_signer.as_ref() else {
            return Ok(false);
        };
        install_app_control_plane_signer(Arc::clone(signer))?;
        Ok(true)
    }

    pub fn capabilities(&self) -> &SecretRuntimeCapabilities {
        &self.capabilities
    }

    pub fn startup_warning(&self) -> Option<&str> {
        self.startup_warning.as_deref()
    }

    pub fn into_parts(
        self,
    ) -> (
        Box<dyn MasterKeyProvider>,
        SecretRuntimeCapabilities,
        Option<String>,
    ) {
        (self.key_provider, self.capabilities, self.startup_warning)
    }
}

/// Probe the durable key backend once and return the runtime bootstrap state.
///
/// Callers that expose secret-related planner surfaces should reuse this result
/// instead of re-probing independently, so pack pruning and executor wiring
/// stay aligned for the whole process startup.
pub fn bootstrap_secret_runtime() -> SecretRuntimeBootstrap {
    let keychain_provider = KeychainProvider::new();
    match durable_platform_key_provider() {
        Ok(provider) => {
            let provider_name = provider.provider_name().to_string();
            match provider.get_or_create_key() {
                Ok(mut master_key) => {
                    let signer = Arc::new(AppControlPlaneSigner::from_master_key(&master_key));
                    master_key.zeroize();
                    SecretRuntimeBootstrap {
                        key_provider: provider,
                        app_control_plane_signer: Some(signer),
                        capabilities: SecretRuntimeCapabilities::fully_available(provider_name),
                        startup_warning: None,
                    }
                },
                Err(error) => {
                    let reason = format!(
                        "Durable secret and app-control features are disabled because the OS-backed master key could not be loaded: {error}"
                    );
                    SecretRuntimeBootstrap {
                        key_provider: provider,
                        app_control_plane_signer: None,
                        capabilities: SecretRuntimeCapabilities::without_durable_storage(
                            provider_name,
                            reason.clone(),
                        ),
                        startup_warning: Some(reason),
                    }
                },
            }
        },
        Err(err) => {
            let reason = format!(
                "Vault features are disabled because the OS keychain backend '{}' is unavailable: {}",
                keychain_provider.provider_name(),
                err
            );
            SecretRuntimeBootstrap {
                // Keep the module alive for ephemeral secret substitution while
                // the durable partitions remain explicitly disabled.
                key_provider: Box::new(InMemoryKeyProvider::new()),
                app_control_plane_signer: None,
                capabilities: SecretRuntimeCapabilities::without_durable_storage(
                    keychain_provider.provider_name().to_string(),
                    reason.clone(),
                ),
                startup_warning: Some(reason),
            }
        },
    }
}

/// Build a fully-available in-memory secret runtime bootstrap.
///
/// This is intended for tests and local harnesses that need the full secret
/// runtime surface without touching the host OS keychain.
pub fn in_memory_secret_runtime_bootstrap() -> SecretRuntimeBootstrap {
    SecretRuntimeBootstrap {
        key_provider: Box::new(InMemoryKeyProvider::new()),
        app_control_plane_signer: None,
        capabilities: SecretRuntimeCapabilities::fully_available("in_memory"),
        startup_warning: Some(
            "Using in-memory secret runtime bootstrap; encrypted secrets will not persist across restarts."
                .to_string(),
        ),
    }
}

pub use magicvault_core::{
    cookie_field_value, CookieSpec, InjectionTarget, SecretEntry, SecretRef, SecretSource,
};
