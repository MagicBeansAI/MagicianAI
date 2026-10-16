use std::sync::{Mutex, OnceLock};

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tracing::warn;
use zeroize::Zeroize;

type HmacSha256 = Hmac<Sha256>;

#[cfg(any(test, not(feature = "test-fixtures")))]
const APP_DATA_KEYCHAIN_SERVICE: &str = "com.magician.app-data";
#[cfg(any(test, not(feature = "test-fixtures")))]
const APP_DATA_ROOT_KEY_NAME: &str = "root-key-v1";
const APP_DATA_SCOPE_KEY_DOMAIN: &[u8] = b"magician.app-data.scope-database.v1";

pub use magicvault_core::encryption::{
    decrypt, decrypt_with_aad, encrypt, encrypt_with_aad, InMemoryKeyProvider, MasterKeyProvider,
    SecretEncryptionError,
};

/// A process-owned, domain-separated app-data key for one authenticated scope.
/// Key bytes are deliberately neither serializable nor printable.
pub(crate) struct AppDataScopeKey {
    bytes: [u8; 32],
    key_id: String,
}

impl std::fmt::Debug for AppDataScopeKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppDataScopeKey")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl Drop for AppDataScopeKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl AppDataScopeKey {
    pub(crate) fn expose_for_sqlcipher(&self) -> &[u8; 32] {
        &self.bytes
    }

    pub(crate) fn key_id(&self) -> &str {
        &self.key_id
    }
}

struct AppDataRootGeneration {
    revision: u32,
    key: [u8; 32],
}

impl Drop for AppDataRootGeneration {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

struct AppDataRootKeyring {
    generations: Vec<AppDataRootGeneration>,
}

static APP_DATA_ROOT_KEYRING: OnceLock<Mutex<Option<AppDataRootKeyring>>> = OnceLock::new();

/// Resolve the durable OS-keychain root and derive a unique database key for
/// one authenticated app scope. Principal/workspace components are
/// length-prefixed so no two component tuples can alias the HMAC input.
#[cfg(test)]
pub(crate) fn app_data_scope_key(
    principal: &str,
    workspace: &str,
) -> Result<AppDataScopeKey, SecretEncryptionError> {
    app_data_scope_key_candidates(principal, workspace)?
        .into_iter()
        .next()
        .ok_or_else(|| SecretEncryptionError::Keychain("app-data keyring is empty".to_owned()))
}

/// Return active then retained historical scope keys. Historical generations
/// are kept only so a writable open can authenticate an idle database and
/// immediately rekey it to the active generation; new databases and plaintext
/// migrations always use the first key.
pub(crate) fn app_data_scope_key_candidates(
    principal: &str,
    workspace: &str,
) -> Result<Vec<AppDataScopeKey>, SecretEncryptionError> {
    let keyring = APP_DATA_ROOT_KEYRING.get_or_init(|| Mutex::new(None));
    let mut guard = keyring
        .lock()
        .map_err(|_| SecretEncryptionError::Keychain("app-data keyring lock failed".to_owned()))?;
    if guard.is_none() {
        *guard = Some(load_app_data_root_keyring()?);
    }
    let generations = guard
        .as_ref()
        .ok_or_else(|| SecretEncryptionError::Keychain("app-data keyring is empty".to_owned()))?;
    generations
        .generations
        .iter()
        .rev()
        .map(|generation| {
            derive_app_data_scope_key(&generation.key, generation.revision, principal, workspace)
        })
        .collect()
}

fn derive_app_data_scope_key(
    root: &[u8; 32],
    _root_revision: u32,
    principal: &str,
    workspace: &str,
) -> Result<AppDataScopeKey, SecretEncryptionError> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(root).expect("HMAC accepts a 256-bit key");
    mac.update(APP_DATA_SCOPE_KEY_DOMAIN);
    mac.update(&(principal.len() as u64).to_be_bytes());
    mac.update(principal.as_bytes());
    mac.update(&(workspace.len() as u64).to_be_bytes());
    mac.update(workspace.as_bytes());
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&mac.finalize().into_bytes());

    let mut digest = Sha256::new();
    digest.update(b"magician.app-data.key-id.v1");
    digest.update(bytes);
    let key_id = format!("app-data-v1:{}", hex::encode(digest.finalize()));
    Ok(AppDataScopeKey { bytes, key_id })
}

/// Only the explicit offline recovery command may try the historical fixture
/// root. It is never a candidate for ordinary database opens or new stores.
pub(crate) fn legacy_fixture_app_data_scope_key(
    principal: &str,
    workspace: &str,
) -> Result<AppDataScopeKey, SecretEncryptionError> {
    derive_app_data_scope_key(&[0xD4_u8; 32], 1, principal, workspace)
}

fn load_app_data_root_keyring() -> Result<AppDataRootKeyring, SecretEncryptionError> {
    #[cfg(any(test, feature = "test-fixtures"))]
    {
        return Ok(AppDataRootKeyring {
            generations: vec![AppDataRootGeneration {
                revision: 1,
                key: [0xD4_u8; 32],
            }],
        });
    }

    #[cfg(not(any(test, feature = "test-fixtures")))]
    {
        if std::env::var("MAGICIAN_SKIP_KEYCHAIN")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            return Err(SecretEncryptionError::Keychain(
                "durable app-data keychain access is disabled".to_owned(),
            ));
        }
        let entry = keyring::Entry::new(APP_DATA_KEYCHAIN_SERVICE, APP_DATA_ROOT_KEY_NAME)
            .map_err(|error| {
                SecretEncryptionError::Keychain(format!(
                    "failed to create app-data root-key entry: {error}"
                ))
            })?;
        match entry.get_password() {
            Ok(mut encoded) => {
                let decoded = decode_root_keyring(&encoded);
                encoded.zeroize();
                decoded
            },
            Err(keyring::Error::NoEntry) => {
                let mut generated = [0_u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut generated);
                let keyring = AppDataRootKeyring {
                    generations: vec![AppDataRootGeneration {
                        revision: 1,
                        key: generated,
                    }],
                };
                persist_app_data_root_keyring(&keyring)?;
                Ok(keyring)
            },
            Err(error) => Err(SecretEncryptionError::Keychain(format!(
                "app-data root-key read failed: {error}"
            ))),
        }
    }
}

#[cfg(not(any(test, feature = "test-fixtures")))]
fn decode_root_keyring(encoded: &str) -> Result<AppDataRootKeyring, SecretEncryptionError> {
    // The original production vertical stored one raw hex root. Accept it as
    // generation one so upgrading does not strand already-encrypted scopes.
    if !encoded.trim_start().starts_with('{') {
        return Ok(AppDataRootKeyring {
            generations: vec![AppDataRootGeneration {
                revision: 1,
                key: decode_root_key_hex(encoded)?,
            }],
        });
    }
    let document: serde_json::Value = serde_json::from_str(encoded).map_err(|_| {
        SecretEncryptionError::Keychain("stored app-data keyring is malformed".to_owned())
    })?;
    if document
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(SecretEncryptionError::Keychain(
            "stored app-data keyring version is unsupported".to_owned(),
        ));
    }
    let rows = document
        .get("generations")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            SecretEncryptionError::Keychain("stored app-data keyring is malformed".to_owned())
        })?;
    if rows.is_empty() || rows.len() > 32 {
        return Err(SecretEncryptionError::Keychain(
            "stored app-data keyring generation count is invalid".to_owned(),
        ));
    }
    let mut generations = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let revision = row
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                SecretEncryptionError::Keychain("stored app-data keyring is malformed".to_owned())
            })?;
        if revision != u32::try_from(index + 1).unwrap_or(u32::MAX) {
            return Err(SecretEncryptionError::Keychain(
                "stored app-data keyring revisions are non-contiguous".to_owned(),
            ));
        }
        let encoded_key = row
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                SecretEncryptionError::Keychain("stored app-data keyring is malformed".to_owned())
            })?;
        generations.push(AppDataRootGeneration {
            revision,
            key: decode_root_key_hex(encoded_key)?,
        });
    }
    Ok(AppDataRootKeyring { generations })
}

#[cfg(not(any(test, feature = "test-fixtures")))]
fn decode_root_key_hex(encoded: &str) -> Result<[u8; 32], SecretEncryptionError> {
    let mut bytes = hex::decode(encoded).map_err(|_| {
        SecretEncryptionError::Keychain("stored app-data root key is malformed".to_owned())
    })?;
    let result = bytes.as_slice().try_into().map_err(|_| {
        SecretEncryptionError::Keychain("stored app-data root key has an invalid length".to_owned())
    });
    bytes.zeroize();
    result
}

#[cfg(not(any(test, feature = "test-fixtures")))]
fn persist_app_data_root_keyring(
    keyring: &AppDataRootKeyring,
) -> Result<(), SecretEncryptionError> {
    let generations = keyring
        .generations
        .iter()
        .map(|generation| {
            serde_json::json!({
                "revision": generation.revision,
                "key": hex::encode(generation.key),
            })
        })
        .collect::<Vec<_>>();
    let mut encoded = serde_json::to_string(&serde_json::json!({
        "format_version": 1,
        "generations": generations,
    }))
    .map_err(|_| SecretEncryptionError::Keychain("app-data keyring encoding failed".to_owned()))?;
    let entry = keyring::Entry::new(APP_DATA_KEYCHAIN_SERVICE, APP_DATA_ROOT_KEY_NAME).map_err(
        |error| {
            SecretEncryptionError::Keychain(format!(
                "failed to create app-data root-key entry: {error}"
            ))
        },
    )?;
    let result = entry.set_password(&encoded).map_err(|error| {
        SecretEncryptionError::Keychain(format!("failed to persist app-data keyring: {error}"))
    });
    encoded.zeroize();
    result
}

#[cfg(any(test, feature = "test-fixtures"))]
fn persist_app_data_root_keyring(
    _keyring: &AppDataRootKeyring,
) -> Result<(), SecretEncryptionError> {
    Ok(())
}

/// Append a new durable root generation. Historical roots remain in the OS
/// keychain so idle scope databases can be opened and lazily rekeyed; once the
/// bounded generation ceiling is reached, rotation refuses rather than
/// silently making an unvisited database unrecoverable.
pub(crate) fn rotate_app_data_root_key(
    principal: &str,
    workspace: &str,
) -> Result<String, SecretEncryptionError> {
    let keyring = APP_DATA_ROOT_KEYRING.get_or_init(|| Mutex::new(None));
    let mut guard = keyring
        .lock()
        .map_err(|_| SecretEncryptionError::Keychain("app-data keyring lock failed".to_owned()))?;
    if guard.is_none() {
        *guard = Some(load_app_data_root_keyring()?);
    }
    let keyring = guard
        .as_mut()
        .ok_or_else(|| SecretEncryptionError::Keychain("app-data keyring is empty".to_owned()))?;
    if keyring.generations.len() >= 32 {
        return Err(SecretEncryptionError::Keychain(
            "app-data keyring requires an explicit retirement audit before another rotation"
                .to_owned(),
        ));
    }
    let revision = u32::try_from(keyring.generations.len() + 1)
        .map_err(|_| SecretEncryptionError::Keychain("app-data revision exhausted".to_owned()))?;
    let mut key = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    keyring
        .generations
        .push(AppDataRootGeneration { revision, key });
    if let Err(error) = persist_app_data_root_keyring(keyring) {
        keyring.generations.pop();
        return Err(error);
    }
    derive_app_data_scope_key(
        &keyring
            .generations
            .last()
            .expect("new app-data root generation exists")
            .key,
        revision,
        principal,
        workspace,
    )
    .map(|scope_key| scope_key.key_id().to_owned())
}

const SERVICE_NAME: &str = "com.magician.secret-store";
const KEY_NAME: &str = "master-key";

/// Stores and retrieves the master encryption key via the OS keychain.
pub struct KeychainProvider;

impl KeychainProvider {
    pub fn new() -> Self {
        Self
    }

    fn entry(&self) -> Result<keyring::Entry, SecretEncryptionError> {
        keyring::Entry::new(SERVICE_NAME, KEY_NAME).map_err(|e| {
            SecretEncryptionError::Keychain(format!("failed to create keyring entry: {e}"))
        })
    }
}

impl Default for KeychainProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MasterKeyProvider for KeychainProvider {
    fn get_or_create_key(&self) -> Result<[u8; 32], SecretEncryptionError> {
        let entry = self.entry()?;

        match entry.get_password() {
            Ok(hex_str) => {
                let bytes = hex::decode(&hex_str).map_err(|e| {
                    SecretEncryptionError::Keychain(format!("stored key is not valid hex: {e}"))
                })?;
                if bytes.len() != 32 {
                    return Err(SecretEncryptionError::Keychain(format!(
                        "stored key is {} bytes, expected 32",
                        bytes.len()
                    )));
                }
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                Ok(key)
            },
            Err(keyring::Error::NoEntry) => {
                let mut key = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut key);
                let hex_str = hex::encode(key);

                entry.set_password(&hex_str).map_err(|e| {
                    SecretEncryptionError::Keychain(format!("failed to store key in keychain: {e}"))
                })?;

                Ok(key)
            },
            Err(e) => Err(SecretEncryptionError::Keychain(format!(
                "keychain read failed: {e}"
            ))),
        }
    }

    fn delete_key(&self) -> Result<(), SecretEncryptionError> {
        let entry = self.entry()?;
        entry.delete_password().map_err(|e| {
            SecretEncryptionError::Keychain(format!("failed to delete key from keychain: {e}"))
        })
    }

    fn provider_name(&self) -> &str {
        #[cfg(target_os = "macos")]
        {
            "macos_keychain"
        }
        #[cfg(target_os = "linux")]
        {
            "linux_secret_service"
        }
        #[cfg(target_os = "windows")]
        {
            "windows_credential_manager"
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            "unknown_keychain"
        }
    }
}

/// Return the best available key provider for the current platform.
pub fn platform_key_provider() -> Box<dyn MasterKeyProvider> {
    let keychain = KeychainProvider::new();
    match keychain.get_or_create_key() {
        Ok(_) => Box::new(keychain),
        Err(e) => {
            warn!(
                "OS keychain unavailable ({}), falling back to in-memory key provider. \
                 Encrypted secrets will not persist across restarts.",
                e
            );
            Box::new(InMemoryKeyProvider::new())
        },
    }
}

/// Return a durable OS-backed key provider or the concrete availability error.
///
/// New runtime wiring should prefer this probe over `platform_key_provider()`
/// so higher layers can explicitly disable durable secret features instead of
/// silently degrading them into a memory-only false path.
pub fn durable_platform_key_provider() -> Result<Box<dyn MasterKeyProvider>, SecretEncryptionError>
{
    // Skip keychain access when MAGICIAN_SKIP_KEYCHAIN=1 (dev/CI environments)
    if std::env::var("MAGICIAN_SKIP_KEYCHAIN")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return Err(SecretEncryptionError::Keychain(
            "keychain access skipped via MAGICIAN_SKIP_KEYCHAIN=1".to_string(),
        ));
    }

    let keychain = KeychainProvider::new();
    keychain.get_or_create_key()?;
    Ok(Box::new(keychain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_preserves_magician_keychain_and_derivation_identities() {
        assert_eq!(SERVICE_NAME, "com.magician.secret-store");
        assert_eq!(KEY_NAME, "master-key");
        assert_eq!(APP_DATA_KEYCHAIN_SERVICE, "com.magician.app-data");
        assert_eq!(APP_DATA_ROOT_KEY_NAME, "root-key-v1");
        assert_eq!(
            APP_DATA_SCOPE_KEY_DOMAIN,
            b"magician.app-data.scope-database.v1"
        );
    }

    #[test]
    fn app_data_scope_keys_are_domain_separated() {
        let first = app_data_scope_key("owner-a", "default").unwrap();
        let repeated = app_data_scope_key("owner-a", "default").unwrap();
        let other_owner = app_data_scope_key("owner-b", "default").unwrap();
        let other_workspace = app_data_scope_key("owner-a", "other").unwrap();

        assert_eq!(
            first.expose_for_sqlcipher(),
            repeated.expose_for_sqlcipher()
        );
        assert_eq!(first.key_id(), repeated.key_id());
        assert_ne!(
            first.expose_for_sqlcipher(),
            other_owner.expose_for_sqlcipher()
        );
        assert_ne!(
            first.expose_for_sqlcipher(),
            other_workspace.expose_for_sqlcipher()
        );
        let rotated = derive_app_data_scope_key(&[0xE5; 32], 2, "owner-a", "default").unwrap();
        assert_ne!(first.expose_for_sqlcipher(), rotated.expose_for_sqlcipher());
        assert_ne!(first.key_id(), rotated.key_id());
    }
}
