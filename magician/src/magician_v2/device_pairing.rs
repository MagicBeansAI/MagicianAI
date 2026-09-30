//! Which devices are allowed to connect, and proof that they are.
//!
//! The bridge endpoint required a token from the first commit and never checked
//! it. Presence is not authentication: anything that could reach the endpoint
//! could claim any device id under any principal, and the scope that decides
//! what a device may touch was taken on the device's own word.
//!
//! Pairing is deliberately owner-initiated. Magician mints a token for a named
//! scope, the owner carries it to the handset once, and the device presents it
//! on every connection. Nothing self-registers — a companion that could enrol
//! itself would make the roster meaningless.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering},
    Arc,
};

use base64::Engine as _;
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::RwLock;
use zeroize::{Zeroize, Zeroizing};

use crate::magician_v2::device_bridge::{
    global_hub, DeviceBridgeHub, DeviceConnectionId, DeviceKey, DeviceSink,
};
#[cfg(not(test))]
use crate::magician_v2::secrets::durable_platform_key_provider;

/// Five minutes is long enough to open App Pilot and scan a code, but short
/// enough that a QR left on screen does not become a standing credential.
pub const DEVICE_ENROLLMENT_TTL_MS: i64 = 5 * 60 * 1_000;

/// Pending enrollments are deliberately memory-only and bounded. A restart
/// invalidates every outstanding QR, which is safer than resurrecting a bearer
/// capability from disk, while the cap prevents an authenticated but buggy UI
/// from growing the daemon forever.
const MAX_PENDING_ENROLLMENTS: usize = 64;
/// Ordinary mobile API traffic may be frequent; keep presence useful without
/// turning every request into a roster fsync.
const MOBILE_LAST_SEEN_WRITE_INTERVAL_MS: i64 = 60_000;
const MAX_AUTOMATION_PACKAGES: usize = 64;
const MAX_ANDROID_PACKAGE_BYTES: usize = 255;
const MAX_PAIRING_ROSTER_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PAIRED_DEVICES_GLOBAL: usize = 1_024;
const MAX_PAIRED_DEVICES_PER_SCOPE: usize = 64;
const PAIRING_ROSTER_SCHEMA: &str = "magician.paired-devices.sealed.v2";
const PAIRING_ROSTER_SEAL_DOMAIN: &[u8] = b"magician.paired-devices.seal.v2\0";
const PAIRING_ROSTER_KEY_DOMAIN: &[u8] = b"magician.paired-devices.key.v1\0";
#[cfg(not(test))]
const PAIRING_ROSTER_KEYCHAIN_SERVICE: &str = "com.magicbeans.magician.paired-devices";

/// The native client the owner intended to enroll. This is captured when the
/// authenticated owner creates the QR; an unpaired phone cannot choose a more
/// privileged kind during exchange.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MobileClientKind {
    Ios,
    #[default]
    Android,
    /// A small embedded companion using the physically guarded ESP bootstrap
    /// route. It receives ordinary mobile-client authority and never Android
    /// automation authority.
    Esp32,
    /// A Tauri desktop acting as Magician Edge. It receives its own
    /// independently revocable device credential and may open only the Edge
    /// capability socket, not the Android physical-owner bridge.
    Desktop,
}

/// Independently revocable authority attached to one mobile credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCapability {
    MobileClient,
    DeviceAutomation,
    EdgeClient,
}

/// The only Android Apps operation an owner can currently review. Keeping the
/// set typed prevents a future bridge verb from silently inheriting an older
/// snapshot grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceAutomationAction {
    Snapshot,
    Screenshot,
    Launch,
    Close,
    Tap,
    Type,
    Key,
    Scroll,
}

pub(crate) const DEVICE_AUTOMATION_ACTION_ROSTER: [DeviceAutomationAction; 8] = [
    DeviceAutomationAction::Snapshot,
    DeviceAutomationAction::Screenshot,
    DeviceAutomationAction::Launch,
    DeviceAutomationAction::Close,
    DeviceAutomationAction::Tap,
    DeviceAutomationAction::Type,
    DeviceAutomationAction::Key,
    DeviceAutomationAction::Scroll,
];

impl DeviceAutomationAction {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Screenshot => "screenshot",
            Self::Launch => "launch",
            Self::Close => "close",
            Self::Tap => "tap",
            Self::Type => "type",
            Self::Key => "key",
            Self::Scroll => "scroll",
        }
    }
}

/// Durable owner approval for the Android Apps physical owner. The opaque
/// target is credential-derived, so re-pairing rotates it and invalidates this
/// record. Package authority is never accepted from an App invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceAutomationReview {
    pub target_ref: String,
    pub generation: u64,
    pub review_digest: String,
    pub actions: Vec<DeviceAutomationAction>,
    pub allowed_packages: Vec<String>,
    pub reviewed_at_ms: i64,
}

/// Independently verified Magdroid installation identity pinned by the
/// Apps-only enrollment. The certificate chain stays at the verifier; the
/// roster retains only the exact key and reviewed artifact fingerprints needed
/// for socket challenge verification and durable physical-owner identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceAutomationIdentity {
    pub enrollment_id: String,
    pub key_id: String,
    pub public_key_spki_base64: String,
    pub app_package: String,
    pub app_version_code: u64,
    pub app_signing_sha256: String,
    pub apk_sha256: String,
    pub attestation_chain_sha256: String,
    pub attestation_root_sha256: String,
    pub attestation_security_level: String,
    pub attestation_policy_digest: String,
    pub enrolled_at_ms: i64,
}

fn legacy_device_capabilities() -> Vec<DeviceCapability> {
    // A roster written before handset attestation cannot prove which app or
    // hardware key holds its bearer token. Migration must preserve mobile
    // messaging availability without silently upgrading that legacy bearer to
    // Apps physical-owner authority.
    vec![DeviceCapability::MobileClient]
}

fn capabilities_for(kind: MobileClientKind) -> Vec<DeviceCapability> {
    match kind {
        MobileClientKind::Ios => vec![DeviceCapability::MobileClient],
        MobileClientKind::Android => vec![DeviceCapability::MobileClient],
        MobileClientKind::Esp32 => vec![DeviceCapability::MobileClient],
        MobileClientKind::Desktop => vec![DeviceCapability::EdgeClient],
    }
}

/// Tokens are compared by digest, never held in the clear.
///
/// A paired device's token is as good as the device, and this file sits in the
/// same store as everything else — so it holds hashes, and a leaked roster does
/// not hand anyone a working credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedDevice {
    pub principal: String,
    pub workspace: String,
    pub device_id: String,
    /// Hex BLAKE3 of the token.
    pub token_digest: String,
    pub label: String,
    #[serde(default)]
    pub client_kind: MobileClientKind,
    #[serde(default = "legacy_device_capabilities")]
    pub capabilities: Vec<DeviceCapability>,
    pub paired_at_ms: i64,
    /// Set on every accepted connection, so an owner can spot a device that has
    /// gone quiet or one they do not recognise.
    #[serde(default)]
    pub last_seen_ms: Option<i64>,
    /// Monotonic even when the current review is revoked. This makes stale UI
    /// writes and pre-rotation approvals fail closed.
    #[serde(default)]
    pub automation_review_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_review: Option<DeviceAutomationReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_identity: Option<DeviceAutomationIdentity>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PairingRosterPayload<'a> {
    schema: &'static str,
    generation: u64,
    devices: &'a [PairedDevice],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedPairingRoster {
    schema: String,
    generation: u64,
    devices: Vec<PairedDevice>,
    seal_hex: String,
}

struct PairingRosterSealOwner {
    key: Zeroizing<[u8; 32]>,
    anchor_account: String,
}

struct PairingRosterGenerationAnchor {
    generation: u64,
    seal_hex: String,
}

fn generation_anchor_matches(
    anchor: &PairingRosterGenerationAnchor,
    generation: u64,
    seal_hex: &str,
) -> bool {
    anchor.generation == generation && digests_match(&anchor.seal_hex, seal_hex)
}

impl PairingRosterSealOwner {
    fn load(base_root: &Path) -> Result<Self, PairingError> {
        let canonical_root = std::fs::canonicalize(base_root)?;
        let root_bytes = canonical_root.as_os_str().as_encoded_bytes();
        #[cfg(not(test))]
        let mut master_key = durable_platform_key_provider()
            .and_then(|provider| provider.get_or_create_key())
            .map_err(|error| PairingError::SealUnavailable(error.to_string()))?;
        #[cfg(test)]
        let mut master_key = [0x9du8; 32];
        let mut derivation = Hmac::<Sha256>::new_from_slice(&master_key)
            .map_err(|_| PairingError::SealUnavailable("pairing seal key is invalid".to_owned()))?;
        derivation.update(PAIRING_ROSTER_KEY_DOMAIN);
        derivation.update(&(root_bytes.len() as u64).to_le_bytes());
        derivation.update(root_bytes);
        let derived = derivation.finalize().into_bytes();
        let mut key = [0u8; 32];
        key.copy_from_slice(&derived);
        master_key.zeroize();
        let mut anchor = blake3::Hasher::new();
        anchor.update(b"magician.paired-devices.anchor-account.v1\0");
        anchor.update(&(root_bytes.len() as u64).to_le_bytes());
        anchor.update(root_bytes);
        Ok(Self {
            key: Zeroizing::new(key),
            anchor_account: format!("generation-{}", anchor.finalize().to_hex()),
        })
    }

    fn seal(&self, generation: u64, devices: &[PairedDevice]) -> Result<String, PairingError> {
        let payload = serde_json::to_vec(&PairingRosterPayload {
            schema: PAIRING_ROSTER_SCHEMA,
            generation,
            devices,
        })
        .map_err(|error| PairingError::Corrupt(error.to_string()))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(self.key.as_ref())
            .map_err(|_| PairingError::SealUnavailable("pairing seal key is invalid".to_owned()))?;
        mac.update(PAIRING_ROSTER_SEAL_DOMAIN);
        mac.update(&payload);
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    fn verify(&self, document: &SealedPairingRoster) -> Result<(), PairingError> {
        let expected = self.seal(document.generation, &document.devices)?;
        if document.seal_hex.len() != 64
            || !document
                .seal_hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !digests_match(&document.seal_hex, &expected)
        {
            return Err(PairingError::Corrupt(
                "paired device seal verification failed".to_owned(),
            ));
        }
        Ok(())
    }

    fn generation_high_water(&self) -> Result<PairingRosterGenerationAnchor, PairingError> {
        #[cfg(test)]
        {
            let anchors = test_pairing_generation_anchors().lock().map_err(|_| {
                PairingError::SealUnavailable("test anchor lock poisoned".to_owned())
            })?;
            return Ok(anchors
                .get(&self.anchor_account)
                .cloned()
                .map(|(generation, seal_hex)| PairingRosterGenerationAnchor {
                    generation,
                    seal_hex,
                })
                .unwrap_or(PairingRosterGenerationAnchor {
                    generation: 0,
                    seal_hex: String::new(),
                }));
        }
        #[cfg(not(test))]
        {
            let entry = keyring::Entry::new(PAIRING_ROSTER_KEYCHAIN_SERVICE, &self.anchor_account)
                .map_err(|error| PairingError::SealUnavailable(error.to_string()))?;
            match entry.get_password() {
                Ok(value) => value
                    .split_once(':')
                    .and_then(|(generation, seal_hex)| {
                        Some(PairingRosterGenerationAnchor {
                            generation: generation.parse::<u64>().ok()?,
                            seal_hex: seal_hex.to_owned(),
                        })
                    })
                    .filter(|anchor| {
                        anchor.generation > 0
                            && anchor.seal_hex.len() == 64
                            && anchor.seal_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                    .ok_or_else(|| {
                        PairingError::SealUnavailable(
                            "paired device generation anchor is corrupt".to_owned(),
                        )
                    }),
                Err(keyring::Error::NoEntry) => Ok(PairingRosterGenerationAnchor {
                    generation: 0,
                    seal_hex: String::new(),
                }),
                Err(error) => Err(PairingError::SealUnavailable(error.to_string())),
            }
        }
    }

    fn advance_generation_high_water(
        &self,
        generation: u64,
        seal_hex: &str,
    ) -> Result<(), PairingError> {
        let current = self.generation_high_water()?;
        if generation == current.generation && digests_match(seal_hex, &current.seal_hex) {
            return Ok(());
        }
        if generation == 0
            || generation <= current.generation
            || seal_hex.len() != 64
            || !seal_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(PairingError::Corrupt(
                "paired device generation did not advance monotonically".to_owned(),
            ));
        }
        #[cfg(test)]
        {
            test_pairing_generation_anchors()
                .lock()
                .map_err(|_| PairingError::SealUnavailable("test anchor lock poisoned".to_owned()))?
                .insert(
                    self.anchor_account.clone(),
                    (generation, seal_hex.to_owned()),
                );
            return Ok(());
        }
        #[cfg(not(test))]
        {
            keyring::Entry::new(PAIRING_ROSTER_KEYCHAIN_SERVICE, &self.anchor_account)
                .map_err(|error| PairingError::SealUnavailable(error.to_string()))?
                .set_password(&format!("{generation}:{seal_hex}"))
                .map_err(|error| PairingError::SealUnavailable(error.to_string()))
        }
    }
}

#[cfg(test)]
fn test_pairing_generation_anchors() -> &'static std::sync::Mutex<HashMap<String, (u64, String)>> {
    static ANCHORS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, (u64, String)>>> =
        std::sync::OnceLock::new();
    ANCHORS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Owner-facing automation selector. The random pairing credential, its
/// digest and the transport's raw device id remain inside the roster; Apps
/// review and persist only this opaque, scope-bound identity plus its label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairedAutomationTarget {
    pub principal: String,
    pub workspace: String,
    pub target_ref: String,
    pub label: String,
    pub enrollment_id: String,
    pub key_id: String,
    pub automation_identity_digest: String,
    pub app_package: String,
    pub app_version_code: u64,
    pub app_signing_sha256: String,
    pub apk_sha256: String,
    pub attestation_root_sha256: String,
    pub attestation_security_level: String,
    pub attestation_policy_digest: String,
    pub paired_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_ms: Option<i64>,
    pub review_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<DeviceAutomationReview>,
}

impl PairedDevice {
    pub fn key(&self) -> DeviceKey {
        DeviceKey::new(&self.principal, &self.workspace, &self.device_id)
    }

    pub fn allows(&self, capability: DeviceCapability) -> bool {
        self.capabilities.contains(&capability)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("no device `{0}` is paired for this scope")]
    NotPaired(String),
    #[error("the token presented for `{0}` does not match")]
    BadToken(String),
    #[error("device enrollment is invalid or has expired")]
    InvalidEnrollment,
    #[error("too many device enrollments are waiting to be scanned")]
    TooManyPendingEnrollments,
    #[error("Android automation review is invalid")]
    InvalidAutomationReview,
    #[error("Android automation review generation changed")]
    AutomationReviewConflict,
    #[error("Android Apps devices require the signed native owner channel")]
    AppsOwnerRequired,
    #[error("pairing store: {0}")]
    Io(#[from] std::io::Error),
    #[error("pairing store is unreadable: {0}")]
    Corrupt(String),
    #[error("pairing store owner seal is unavailable: {0}")]
    SealUnavailable(String),
}

/// The one-time half of a pairing shown to the owner as a QR code.
///
/// The clear secret exists only in this return value. The store keeps its
/// digest, and the caller must never log or persist this structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEnrollmentTicket {
    pub enrollment_id: String,
    pub secret: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone)]
struct PendingDeviceEnrollment {
    principal: String,
    workspace: String,
    secret_digest: String,
    expires_at_ms: i64,
    client_kind: MobileClientKind,
    /// Exact server-owned origin embedded in this ticket's QR. Keeping it on
    /// the one-time capability lets local and remote codes coexist and keeps a
    /// later config or network change from retargeting an issued credential.
    connection_origin: Option<String>,
}

/// Result of consuming a one-time enrollment. The scope is taken from the
/// owner-created ticket, never from headers supplied by the unpaired phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEnrollmentGrant {
    pub principal: String,
    pub workspace: String,
    pub device_id: String,
    pub label: String,
    pub token: String,
    pub paired_at_ms: i64,
    pub client_kind: MobileClientKind,
    pub capabilities: Vec<DeviceCapability>,
    pub connection_origin: Option<String>,
}

/// Independently attested Apps authority for one candidate socket. The bearer
/// is still required, but it does not enter this value: possession alone never
/// admits the socket to the physical-owner hub.
#[derive(Clone)]
pub struct DeviceAutomationSocketAuthority {
    target_ref: String,
    key_id: String,
    public_key_spki_base64: String,
    identity_digest: String,
    review_generation: u64,
    apk_sha256: String,
    attestation_policy_digest: String,
    app_package: String,
    app_version_code: u64,
    app_signing_sha256: String,
    attestation_root_sha256: String,
    attestation_security_level: String,
    play_integrity_verdict_digest: Option<String>,
}

impl DeviceAutomationSocketAuthority {
    pub fn target_ref(&self) -> &str {
        &self.target_ref
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn public_key_spki_base64(&self) -> &str {
        &self.public_key_spki_base64
    }

    pub fn identity_digest(&self) -> &str {
        &self.identity_digest
    }

    pub fn review_generation(&self) -> u64 {
        self.review_generation
    }

    pub fn apk_sha256(&self) -> &str {
        &self.apk_sha256
    }

    pub fn attestation_policy_digest(&self) -> &str {
        &self.attestation_policy_digest
    }

    pub fn app_package(&self) -> &str {
        &self.app_package
    }

    pub fn app_version_code(&self) -> u64 {
        self.app_version_code
    }

    pub fn app_signing_sha256(&self) -> &str {
        &self.app_signing_sha256
    }

    pub fn attestation_root_sha256(&self) -> &str {
        &self.attestation_root_sha256
    }

    pub fn attestation_security_level(&self) -> &str {
        &self.attestation_security_level
    }

    pub fn play_integrity_verdict_digest(&self) -> Option<&str> {
        self.play_integrity_verdict_digest.as_deref()
    }

    pub fn bind_runtime_trust_verdict(mut self, digest: String) -> Option<Self> {
        if digest.len() != 71 || !digest.starts_with("blake3:") {
            return None;
        }
        self.play_integrity_verdict_digest = Some(digest);
        Some(self)
    }

    fn matches_device(&self, device: &PairedDevice) -> bool {
        let Some(identity) = device.automation_identity.as_ref() else {
            return false;
        };
        self.target_ref == automation_target_ref(device)
            && self.key_id == identity.key_id
            && self.public_key_spki_base64 == identity.public_key_spki_base64
            && self.identity_digest == automation_identity_digest(identity)
            && self.review_generation == device.automation_review_generation
            && self.apk_sha256 == identity.apk_sha256
            && self.attestation_policy_digest == identity.attestation_policy_digest
            && self.app_package == identity.app_package
            && self.app_version_code == identity.app_version_code
            && self.app_signing_sha256 == identity.app_signing_sha256
            && self.attestation_root_sha256 == identity.attestation_root_sha256
            && self.attestation_security_level == identity.attestation_security_level
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) fn test_device_automation_socket_authority() -> DeviceAutomationSocketAuthority {
    DeviceAutomationSocketAuthority {
        target_ref: "android-device:test-owner".to_owned(),
        key_id: "test-key-id-1234567890".to_owned(),
        public_key_spki_base64: "test-public-key".to_owned(),
        identity_digest: format!(
            "blake3:{}",
            blake3::hash(b"test-automation-identity").to_hex()
        ),
        review_generation: 1,
        apk_sha256: "a".repeat(64),
        attestation_policy_digest: format!("blake3:{}", "b".repeat(64)),
        app_package: "ai.magicbeans.magdroid".to_owned(),
        app_version_code: 11,
        app_signing_sha256: "c".repeat(64),
        attestation_root_sha256: "d".repeat(64),
        attestation_security_level: "tee".to_owned(),
        play_integrity_verdict_digest: Some(format!("blake3:{}", "e".repeat(64))),
    }
}

fn digest(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

const HANDSET_SECRET_SHA256_PREFIX: &str = "sha256:";

fn handset_secret_digest(value: &str) -> Result<String, PairingError> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PairingError::InvalidEnrollment);
    }
    Ok(format!("{HANDSET_SECRET_SHA256_PREFIX}{value}"))
}

fn token_digest_matches(stored: &str, presented: &str) -> bool {
    if let Some(expected) = stored.strip_prefix(HANDSET_SECRET_SHA256_PREFIX) {
        use sha2::Digest as _;
        let actual = hex::encode(sha2::Sha256::digest(presented.as_bytes()));
        digests_match(expected, &actual)
    } else {
        digests_match(stored, &digest(presented))
    }
}

fn automation_identity_digest(identity: &DeviceAutomationIdentity) -> String {
    let mut hasher = blake3::Hasher::new();
    let app_version_code = identity.app_version_code.to_le_bytes();
    hasher.update(b"magician.android-automation-identity.v1\0");
    for component in [
        identity.enrollment_id.as_bytes(),
        identity.key_id.as_bytes(),
        identity.public_key_spki_base64.as_bytes(),
        identity.app_package.as_bytes(),
        app_version_code.as_slice(),
        identity.app_signing_sha256.as_bytes(),
        identity.apk_sha256.as_bytes(),
        identity.attestation_chain_sha256.as_bytes(),
        identity.attestation_root_sha256.as_bytes(),
        identity.attestation_security_level.as_bytes(),
        identity.attestation_policy_digest.as_bytes(),
    ] {
        hasher.update(&(component.len() as u64).to_le_bytes());
        hasher.update(component);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Compare digests without leaking how far a match got.
///
/// Both sides are fixed-length hex here, so this is cheap insurance rather than
/// a fix for a known leak — but a token check is exactly the place to not have
/// to reason about it.
fn digests_match(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let l = left.get(index).copied().unwrap_or(0);
        let r = right.get(index).copied().unwrap_or(0);
        diff |= (l ^ r) as usize;
    }
    diff == 0
}

/// The roster of devices allowed to open a bridge.
pub struct DevicePairingStore {
    path: PathBuf,
    seal_owner: Option<Arc<PairingRosterSealOwner>>,
    unavailable_reason: Option<String>,
    /// A published roster whose high-water update had an ambiguous outcome
    /// must not be replaced in-process. Reopening performs the only safe
    /// anchor/document reconciliation.
    reopen_required: AtomicBool,
    durable_generation: AtomicU64,
    devices: RwLock<HashMap<String, PairedDevice>>,
    /// Serializes roster writes.
    ///
    /// [`Self::persist`] snapshots under a **read** guard, which several tasks
    /// hold at once — and `verify` persists on every accepted connection, so
    /// several is the normal case when a handful of devices reconnect together.
    /// Without this, two snapshots taken at different generations can rename in
    /// the opposite order and the newer roster is the one that loses. Held
    /// across the snapshot *and* the rename, which is what makes the file
    /// monotone rather than merely whole.
    write_lock: tokio::sync::Mutex<()>,
    pending_enrollments: RwLock<HashMap<String, PendingDeviceEnrollment>>,
}

fn index_key(key: &DeviceKey) -> String {
    format!("{}::{}::{}", key.principal, key.workspace, key.device_id)
}

fn automation_target_ref_parts(
    principal: &str,
    workspace: &str,
    device_id: &str,
    token_digest: &str,
    public_key_spki_base64: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.android-paired-automation-target.v1\0");
    for component in [
        principal.as_bytes(),
        workspace.as_bytes(),
        device_id.as_bytes(),
        token_digest.as_bytes(),
        public_key_spki_base64.as_bytes(),
    ] {
        hasher.update(&(component.len() as u64).to_le_bytes());
        hasher.update(component);
    }
    format!("android-device:{}", hasher.finalize().to_hex())
}

fn automation_target_ref(device: &PairedDevice) -> String {
    automation_target_ref_parts(
        &device.principal,
        &device.workspace,
        &device.device_id,
        &device.token_digest,
        device
            .automation_identity
            .as_ref()
            .map(|identity| identity.public_key_spki_base64.as_str())
            .unwrap_or_default(),
    )
}

/// Compute the exact opaque owner identities that the desktop must review
/// before an attested handset credential is published. The plaintext
/// connection secret never crosses this boundary.
pub fn prospective_attested_automation_identity(
    key: &DeviceKey,
    connection_secret_sha256: &str,
    identity: &DeviceAutomationIdentity,
) -> Result<(String, String), PairingError> {
    let token_digest = handset_secret_digest(connection_secret_sha256)?;
    let target_ref = automation_target_ref_parts(
        &key.principal,
        &key.workspace,
        &key.device_id,
        &token_digest,
        &identity.public_key_spki_base64,
    );
    Ok((target_ref, automation_identity_digest(identity)))
}

fn paired_automation_target(device: &PairedDevice) -> Result<PairedAutomationTarget, PairingError> {
    let identity = device
        .automation_identity
        .as_ref()
        .ok_or(PairingError::InvalidAutomationReview)?;
    Ok(PairedAutomationTarget {
        principal: device.principal.clone(),
        workspace: device.workspace.clone(),
        target_ref: automation_target_ref(device),
        label: device.label.clone(),
        enrollment_id: identity.enrollment_id.clone(),
        key_id: identity.key_id.clone(),
        automation_identity_digest: automation_identity_digest(identity),
        app_package: identity.app_package.clone(),
        app_version_code: identity.app_version_code,
        app_signing_sha256: identity.app_signing_sha256.clone(),
        apk_sha256: identity.apk_sha256.clone(),
        attestation_root_sha256: identity.attestation_root_sha256.clone(),
        attestation_security_level: identity.attestation_security_level.clone(),
        attestation_policy_digest: identity.attestation_policy_digest.clone(),
        paired_at_ms: device.paired_at_ms,
        last_seen_ms: device.last_seen_ms,
        review_generation: device.automation_review_generation,
        review: device.automation_review.clone(),
    })
}

fn valid_android_package(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ANDROID_PACKAGE_BYTES
        && value.split('.').count() >= 2
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment.bytes().enumerate().all(|(index, byte)| match byte {
                    b'a'..=b'z' | b'A'..=b'Z' => true,
                    b'0'..=b'9' | b'_' => index > 0,
                    _ => false,
                })
        })
}

fn automation_review_digest(
    device: &PairedDevice,
    target_ref: &str,
    generation: u64,
    packages: &[String],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.android-automation-review.v2\0");
    let generation_bytes = generation.to_le_bytes();
    for component in [
        device.principal.as_bytes(),
        device.workspace.as_bytes(),
        target_ref.as_bytes(),
        generation_bytes.as_slice(),
    ] {
        hasher.update(&(component.len() as u64).to_le_bytes());
        hasher.update(component);
    }
    for action in DEVICE_AUTOMATION_ACTION_ROSTER {
        let action = action.wire_name().as_bytes();
        hasher.update(&(action.len() as u64).to_le_bytes());
        hasher.update(action);
    }
    for package in packages {
        hasher.update(&(package.len() as u64).to_le_bytes());
        hasher.update(package.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn validate_automation_review(device: &PairedDevice) -> Result<(), PairingError> {
    let identity_valid = device.automation_identity.as_ref().is_some_and(|identity| {
        !identity.enrollment_id.is_empty()
            && identity.enrollment_id.len() <= 192
            && identity.key_id.len() >= 16
            && identity.key_id.len() <= 128
            && identity.public_key_spki_base64.len() >= 64
            && identity.public_key_spki_base64.len() <= 1024
            && valid_android_package(&identity.app_package)
            && identity.app_version_code > 0
            && [
                identity.app_signing_sha256.as_str(),
                identity.apk_sha256.as_str(),
                identity.attestation_chain_sha256.as_str(),
                identity.attestation_root_sha256.as_str(),
            ]
            .into_iter()
            .all(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            && identity.attestation_policy_digest.starts_with("blake3:")
            && identity.attestation_policy_digest.len() == 71
            && identity
                .attestation_policy_digest
                .strip_prefix("blake3:")
                .is_some_and(|digest| digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            && matches!(
                identity.attestation_security_level.as_str(),
                "tee" | "strongbox"
            )
    });
    if device.allows(DeviceCapability::DeviceAutomation)
        != (device.client_kind == MobileClientKind::Android && identity_valid)
    {
        return Err(PairingError::InvalidAutomationReview);
    }
    let Some(review) = &device.automation_review else {
        return Ok(());
    };
    let expected_target = automation_target_ref(device);
    if !device.allows(DeviceCapability::DeviceAutomation)
        || review.target_ref != expected_target
        || review.generation == 0
        || review.generation != device.automation_review_generation
        || review.actions.as_slice() != DEVICE_AUTOMATION_ACTION_ROSTER.as_slice()
        || review.allowed_packages.is_empty()
        || review.allowed_packages.len() > MAX_AUTOMATION_PACKAGES
        || review
            .allowed_packages
            .iter()
            .any(|package| !valid_android_package(package))
        || !review
            .allowed_packages
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        || review.review_digest
            != automation_review_digest(
                device,
                &expected_target,
                review.generation,
                &review.allowed_packages,
            )
    {
        return Err(PairingError::InvalidAutomationReview);
    }
    Ok(())
}

fn index_pairing_roster(
    list: Vec<PairedDevice>,
) -> Result<HashMap<String, PairedDevice>, PairingError> {
    if list.len() > MAX_PAIRED_DEVICES_GLOBAL {
        return Err(PairingError::Corrupt(
            "paired device roster exceeds its device ceiling".to_owned(),
        ));
    }
    let mut devices = HashMap::with_capacity(list.len());
    let mut scope_counts = HashMap::<(String, String), usize>::new();
    for device in list {
        validate_automation_review(&device)
            .map_err(|error| PairingError::Corrupt(error.to_string()))?;
        let scope_count = scope_counts
            .entry((device.principal.clone(), device.workspace.clone()))
            .or_default();
        *scope_count = scope_count.saturating_add(1);
        if *scope_count > MAX_PAIRED_DEVICES_PER_SCOPE {
            return Err(PairingError::Corrupt(
                "paired device roster exceeds its per-scope ceiling".to_owned(),
            ));
        }
        if devices.insert(index_key(&device.key()), device).is_some() {
            return Err(PairingError::Corrupt(
                "paired device roster contains a duplicate identity".to_owned(),
            ));
        }
    }
    Ok(devices)
}

fn decode_legacy_mobile_roster(bytes: &[u8]) -> Result<Vec<PairedDevice>, PairingError> {
    let mut devices: Vec<PairedDevice> = serde_json::from_slice(bytes).map_err(|_| {
        PairingError::Corrupt(
            "paired device roster is unsigned or has an unsupported schema".to_owned(),
        )
    })?;
    for device in &mut devices {
        let digest_valid = device.token_digest.len() == 64
            && device
                .token_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit());
        if device.principal.is_empty()
            || device.principal.len() > 256
            || device.workspace.is_empty()
            || device.workspace.len() > 256
            || device.device_id.is_empty()
            || device.device_id.len() > 256
            || device.label.is_empty()
            || device.label.len() > 640
            || !digest_valid
        {
            return Err(PairingError::Corrupt(
                "legacy paired device identity is invalid".to_owned(),
            ));
        }

        // The old file had no seal and therefore cannot confer Android Apps
        // authority. Preserve only the ordinary mobile bearer that existed in
        // that format, and require a new signed native enrollment/review before
        // any physical-device action can become available.
        device.capabilities = legacy_device_capabilities();
        device.automation_review_generation = 0;
        device.automation_review = None;
        device.automation_identity = None;
    }
    Ok(devices)
}

impl DevicePairingStore {
    /// Open the roster, tolerating its absence.
    ///
    /// A missing file means nothing is paired yet, which is the correct state
    /// for a fresh install — not an error to refuse startup over.
    pub async fn open(base_root: &Path) -> Result<Self, PairingError> {
        let seal_root = base_root.to_path_buf();
        let seal_slot = magician_core::blocking_admission::acquire_blocking_admission()
            .await
            .map_err(|error| PairingError::SealUnavailable(error.to_string()))?;
        let (seal_owner, generation_high_water) =
            magician_core::blocking_admission::spawn_blocking_with_admission(
                seal_slot,
                move || {
                    let owner = PairingRosterSealOwner::load(&seal_root)?;
                    let generation = owner.generation_high_water()?;
                    Ok::<_, PairingError>((owner, generation))
                },
            )
            .await
            .map_err(|error| PairingError::SealUnavailable(error.to_string()))??;
        let path = crate::magician_v2::system_owners::host_system_path(
            base_root,
            crate::magician_v2::system_owners::SystemOwner::DevicePairing,
        );
        let read_path = path.clone();
        let read_slot = magician_core::blocking_admission::acquire_blocking_admission()
            .await
            .map_err(|error| PairingError::SealUnavailable(error.to_string()))?;
        let roster_bytes = magician_core::blocking_admission::spawn_blocking_with_admission(
            read_slot,
            move || {
                // A fresh roster still needs a private parent. Check at open,
                // before advertising readiness or issuing a QR that cannot
                // be exchanged. Do not repair an existing unsafe directory.
                let parent = read_path.parent().ok_or_else(|| {
                    PairingError::Corrupt("paired device roster has no owner directory".to_owned())
                })?;
                ensure_private_pairing_directory(parent)?;
                read_private_pairing_roster(&read_path)
            },
        )
        .await
        .map_err(|error| PairingError::SealUnavailable(error.to_string()))??;
        let seal_owner = Arc::new(seal_owner);
        let (list, durable_generation, legacy_migration) = match roster_bytes {
            Some(bytes) => match serde_json::from_slice::<SealedPairingRoster>(&bytes) {
                Ok(document) => {
                    if document.schema != PAIRING_ROSTER_SCHEMA || document.generation == 0 {
                        return Err(PairingError::Corrupt(
                            "paired device roster is unsigned or has an unsupported schema"
                                .to_owned(),
                        ));
                    }
                    seal_owner.verify(&document)?;
                    let exact_anchor = generation_anchor_matches(
                        &generation_high_water,
                        document.generation,
                        &document.seal_hex,
                    );
                    let recoverable_forward_publish = generation_high_water
                        .generation
                        .checked_add(1)
                        .is_some_and(|generation| generation == document.generation);
                    if !exact_anchor && !recoverable_forward_publish {
                        return Err(PairingError::Corrupt(
                            "paired device roster generation was rolled back or is incomplete"
                                .to_owned(),
                        ));
                    }
                    if recoverable_forward_publish {
                        let owner = Arc::clone(&seal_owner);
                        let generation = document.generation;
                        let seal_hex = document.seal_hex.clone();
                        let slot = magician_core::blocking_admission::acquire_blocking_admission()
                            .await
                            .map_err(|error| PairingError::SealUnavailable(error.to_string()))?;
                        magician_core::blocking_admission::spawn_blocking_with_admission(
                            slot,
                            move || owner.advance_generation_high_water(generation, &seal_hex),
                        )
                        .await
                        .map_err(|error| PairingError::SealUnavailable(error.to_string()))??;
                    }
                    (document.devices, document.generation, false)
                },
                Err(_) if generation_high_water.generation == 0 => {
                    (decode_legacy_mobile_roster(&bytes)?, 0, true)
                },
                Err(_) => {
                    return Err(PairingError::Corrupt(
                        "paired device roster is unsigned below a durable generation anchor"
                            .to_owned(),
                    ));
                },
            },
            None => {
                if generation_high_water.generation != 0 {
                    return Err(PairingError::Corrupt(
                        "paired device roster is missing below its durable generation anchor"
                            .to_owned(),
                    ));
                }
                (Vec::new(), 0, false)
            },
        };
        let migration_snapshot = legacy_migration.then(|| list.clone());
        let devices = index_pairing_roster(list)?;
        let store = Self {
            path,
            seal_owner: Some(seal_owner),
            unavailable_reason: None,
            reopen_required: AtomicBool::new(false),
            durable_generation: AtomicU64::new(durable_generation),
            devices: RwLock::new(devices),
            write_lock: tokio::sync::Mutex::new(()),
            pending_enrollments: RwLock::new(HashMap::new()),
        };
        if let Some(snapshot) = migration_snapshot {
            store.persist_snapshot(&snapshot).await?;
        }
        Ok(store)
    }

    /// Construct a fail-closed roster owner after durable owner-key or store
    /// recovery failed. Startup may continue for unrelated subsystems, while
    /// every device authority route observes an empty/unavailable owner and no
    /// mutation can be persisted with an in-memory fallback key.
    pub fn unavailable(base_root: &Path, error: impl Into<String>) -> Self {
        Self {
            path: crate::magician_v2::system_owners::host_system_path(
                base_root,
                crate::magician_v2::system_owners::SystemOwner::DevicePairing,
            ),
            seal_owner: None,
            unavailable_reason: Some(error.into()),
            reopen_required: AtomicBool::new(true),
            durable_generation: AtomicU64::new(0),
            devices: RwLock::new(HashMap::new()),
            write_lock: tokio::sync::Mutex::new(()),
            pending_enrollments: RwLock::new(HashMap::new()),
        }
    }

    pub fn ensure_available(&self) -> Result<(), PairingError> {
        if self.reopen_required.load(AtomicOrdering::Acquire) {
            return Err(PairingError::SealUnavailable(
                "paired device authority requires durable reopen reconciliation".to_owned(),
            ));
        }
        self.unavailable_reason
            .as_ref()
            .map(|reason| Err(PairingError::SealUnavailable(reason.clone())))
            .unwrap_or(Ok(()))
    }

    /// Mint a one-time enrollment capability for an already authenticated
    /// owner. Pending capabilities are never written to disk.
    pub async fn begin_enrollment(
        &self,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        now_ms: i64,
    ) -> Result<DeviceEnrollmentTicket, PairingError> {
        self.begin_mobile_enrollment(principal, workspace, MobileClientKind::Android, now_ms)
            .await
    }

    pub async fn begin_mobile_enrollment(
        &self,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        client_kind: MobileClientKind,
        now_ms: i64,
    ) -> Result<DeviceEnrollmentTicket, PairingError> {
        self.begin_mobile_enrollment_at_origin(principal, workspace, client_kind, None, now_ms)
            .await
    }

    pub async fn begin_mobile_enrollment_at_origin(
        &self,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        client_kind: MobileClientKind,
        connection_origin: Option<String>,
        now_ms: i64,
    ) -> Result<DeviceEnrollmentTicket, PairingError> {
        self.ensure_available()?;
        let principal = principal.into();
        let workspace = workspace.into();
        let mut pending = self.pending_enrollments.write().await;
        pending.retain(|_, enrollment| enrollment.expires_at_ms > now_ms);
        if pending.len() >= MAX_PENDING_ENROLLMENTS {
            return Err(PairingError::TooManyPendingEnrollments);
        }

        let enrollment_id = loop {
            let candidate = random_url_token(18);
            if !pending.contains_key(&candidate) {
                break candidate;
            }
        };
        let secret = random_url_token(32);
        let expires_at_ms = now_ms.saturating_add(DEVICE_ENROLLMENT_TTL_MS);
        pending.insert(
            enrollment_id.clone(),
            PendingDeviceEnrollment {
                principal,
                workspace,
                secret_digest: digest(&secret),
                expires_at_ms,
                client_kind,
                connection_origin,
            },
        );
        Ok(DeviceEnrollmentTicket {
            enrollment_id,
            secret,
            expires_at_ms,
        })
    }

    /// Exchange an owner-created QR capability for the durable device token.
    ///
    /// Validation and removal happen under one write guard, so two phones (or
    /// two retries) cannot both consume the same QR. A bad guess does not burn
    /// the ticket; with a 256-bit secret, removing it would only create a cheap
    /// denial-of-service primitive.
    pub async fn exchange_enrollment(
        &self,
        enrollment_id: &str,
        secret: &str,
        device_id: &str,
        label: &str,
        now_ms: i64,
    ) -> Result<DeviceEnrollmentGrant, PairingError> {
        self.ensure_available()?;
        let device_id = device_id.trim();
        if device_id.is_empty() || device_id.len() > 256 {
            return Err(PairingError::InvalidEnrollment);
        }

        let mut pending = self.pending_enrollments.write().await;
        let enrollment = {
            let Some(candidate) = pending.get(enrollment_id) else {
                return Err(PairingError::InvalidEnrollment);
            };
            if candidate.expires_at_ms <= now_ms {
                pending.remove(enrollment_id);
                return Err(PairingError::InvalidEnrollment);
            }
            if !digests_match(&candidate.secret_digest, &digest(secret.trim())) {
                return Err(PairingError::InvalidEnrollment);
            }
            candidate.clone()
        };

        let label = label.trim();
        let label = if label.is_empty() { device_id } else { label };
        let label = label.chars().take(160).collect::<String>();
        let key = DeviceKey::new(&enrollment.principal, &enrollment.workspace, device_id);
        let capabilities = capabilities_for(enrollment.client_kind);
        let token = self
            .pair_with_capabilities(
                key,
                label.clone(),
                enrollment.client_kind,
                capabilities.clone(),
                None,
                now_ms,
            )
            .await?;
        pending.remove(enrollment_id);
        Ok(DeviceEnrollmentGrant {
            principal: enrollment.principal,
            workspace: enrollment.workspace,
            device_id: device_id.to_string(),
            label,
            token,
            paired_at_ms: now_ms,
            client_kind: enrollment.client_kind,
            capabilities,
            connection_origin: enrollment.connection_origin,
        })
    }

    /// Cancel a QR from the same scope that created it. Returns false both for
    /// an unknown ticket and for one belonging to another scope, so this cannot
    /// be used as a cross-scope existence oracle.
    pub async fn cancel_enrollment(
        &self,
        principal: &str,
        workspace: &str,
        enrollment_id: &str,
    ) -> bool {
        let mut pending = self.pending_enrollments.write().await;
        let owned = pending
            .get(enrollment_id)
            .is_some_and(|entry| entry.principal == principal && entry.workspace == workspace);
        if owned {
            pending.remove(enrollment_id);
        }
        owned
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    async fn pending_enrollment_count(&self) -> usize {
        self.pending_enrollments.read().await.len()
    }

    /// Publish the roster.
    ///
    /// A truncated roster would lock every device out — [`Self::open`] refuses
    /// to parse one and the daemon refuses to start over it — so this does the
    /// whole write-then-rename, not half of it:
    ///
    /// * one writer at a time, so two snapshots cannot land out of order;
    /// * a temp name unique per write, because a fixed `paired-devices.json.tmp`
    ///   defeats the guard the moment two writers share it;
    /// * the temp fsynced before the rename, or the rename is durable while the
    ///   contents are not — which is precisely the empty roster this is
    ///   avoiding;
    /// * the directory fsynced after, because the rename is a directory
    ///   mutation;
    /// * the temp removed on failure, so a failed write does not leave the
    ///   roster's contents lying beside it under a name nothing cleans up.
    async fn persist(&self) -> Result<(), PairingError> {
        let _serialized = self.write_lock.lock().await;

        let list = {
            let devices = self.devices.read().await;
            let mut list = devices.values().cloned().collect::<Vec<_>>();
            // Stable order so the file does not churn on every write.
            list.sort_by(|a, b| index_key(&a.key()).cmp(&index_key(&b.key())));
            list
        };
        self.persist_snapshot(&list).await
    }

    /// Write one already serialized authority snapshot while the caller owns
    /// `write_lock`. Review/rotation transactions use this before publishing
    /// new in-memory authority, so an I/O error cannot create a live but
    /// unrecoverable grant.
    async fn persist_snapshot(&self, list: &[PairedDevice]) -> Result<(), PairingError> {
        self.ensure_available()?;
        let seal_owner = self.seal_owner.as_ref().ok_or_else(|| {
            PairingError::SealUnavailable("paired device seal owner is absent".to_owned())
        })?;
        let generation = self
            .durable_generation
            .load(AtomicOrdering::Acquire)
            .checked_add(1)
            .ok_or_else(|| {
                PairingError::Corrupt("paired device generation is exhausted".to_owned())
            })?;
        let seal_hex = seal_owner.seal(generation, list)?;
        let bytes = serde_json::to_vec_pretty(&SealedPairingRoster {
            schema: PAIRING_ROSTER_SCHEMA.to_owned(),
            generation,
            devices: list.to_vec(),
            seal_hex: seal_hex.clone(),
        })
        .map_err(|error| PairingError::Corrupt(error.to_string()))?;
        if list.len() > MAX_PAIRED_DEVICES_GLOBAL
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_PAIRING_ROSTER_BYTES
        {
            return Err(PairingError::Corrupt(
                "paired device snapshot exceeds its durable ceiling".to_owned(),
            ));
        }

        let Some(parent) = self.path.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} has no parent directory", self.path.display()),
            )
            .into());
        };
        let parent_path = parent.to_path_buf();
        let directory_slot = magician_core::blocking_admission::acquire_blocking_admission()
            .await
            .map_err(|error| PairingError::SealUnavailable(error.to_string()))?;
        magician_core::blocking_admission::spawn_blocking_with_admission(
            directory_slot,
            move || ensure_private_pairing_directory(&parent_path),
        )
        .await
        .map_err(|error| PairingError::SealUnavailable(error.to_string()))??;

        let temporary = self
            .path
            .with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4().simple()));
        if let Err(error) = write_and_sync(&temporary, &bytes).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.into());
        }
        if let Err(error) = tokio::fs::rename(&temporary, &self.path).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.into());
        }
        tokio::fs::File::open(parent).await?.sync_all().await?;
        // Publish and fsync the exact sealed document before advancing its
        // high-water. A crash in this narrow forward window is recovered only
        // when startup verifies the HMAC and sees exactly anchor + 1. The
        // reverse ordering could leave an unrecoverable anchor above disk.
        let seal_owner = Arc::clone(seal_owner);
        let anchor_owner = Arc::clone(&seal_owner);
        let expected_seal_hex = seal_hex.clone();
        let seal_slot = magician_core::blocking_admission::acquire_blocking_admission()
            .await
            .map_err(|error| PairingError::SealUnavailable(error.to_string()))?;
        let advance = magician_core::blocking_admission::spawn_blocking_with_admission(
            seal_slot,
            move || seal_owner.advance_generation_high_water(generation, &seal_hex),
        )
        .await
        .map_err(|error| PairingError::SealUnavailable(error.to_string()))?;
        if let Err(advance_error) = advance {
            // Keychain writes can report an error after committing. Resolve
            // that ambiguity before returning: exact N+seal is success. Any
            // other result leaves the already-fsynced forward document in
            // place and latches this owner until reopen, so another mutation
            // cannot overwrite generation N with different authority.
            let read_slot =
                match magician_core::blocking_admission::acquire_blocking_admission().await {
                    Ok(slot) => slot,
                    Err(_) => {
                        self.reopen_required.store(true, AtomicOrdering::Release);
                        return Err(advance_error);
                    },
                };
            let observed = magician_core::blocking_admission::spawn_blocking_with_admission(
                read_slot,
                move || anchor_owner.generation_high_water(),
            )
            .await;
            match observed {
                Ok(Ok(anchor))
                    if generation_anchor_matches(&anchor, generation, &expected_seal_hex) => {},
                _ => {
                    self.reopen_required.store(true, AtomicOrdering::Release);
                    return Err(advance_error);
                },
            }
        }
        self.durable_generation
            .store(generation, AtomicOrdering::Release);
        Ok(())
    }

    /// Mint a token for a device and record it.
    ///
    /// Returns the token exactly once. It is stored only as a digest, so an
    /// owner who loses it re-pairs rather than recovers it — which is the
    /// property that makes the stored roster safe to keep.
    pub async fn pair(
        &self,
        key: DeviceKey,
        label: impl Into<String>,
        now_ms: i64,
    ) -> Result<String, PairingError> {
        self.pair_with_capabilities(
            key,
            label,
            MobileClientKind::Android,
            vec![DeviceCapability::MobileClient],
            None,
            now_ms,
        )
        .await
    }

    /// Mint the legacy physically bootstrapped ESP credential with an accurate
    /// roster kind. The kind is selected by the server-owned route rather than
    /// accepted from the unauthenticated request body.
    pub async fn pair_esp32(
        &self,
        key: DeviceKey,
        label: impl Into<String>,
        now_ms: i64,
    ) -> Result<String, PairingError> {
        self.pair_with_capabilities(
            key,
            label,
            MobileClientKind::Esp32,
            vec![DeviceCapability::MobileClient],
            None,
            now_ms,
        )
        .await
    }

    /// Mint the distinct Apps-capable credential only after the Android
    /// attestation verifier has produced a pinned installation identity.
    #[cfg(test)]
    pub async fn pair_attested_automation(
        &self,
        key: DeviceKey,
        label: impl Into<String>,
        identity: DeviceAutomationIdentity,
        now_ms: i64,
    ) -> Result<String, PairingError> {
        self.pair_with_capabilities(
            key,
            label,
            MobileClientKind::Android,
            vec![
                DeviceCapability::MobileClient,
                DeviceCapability::DeviceAutomation,
            ],
            Some(identity),
            now_ms,
        )
        .await
    }

    /// Publish an owner-approved Apps credential from a handset-generated
    /// connection-secret digest. The runtime never receives the plaintext
    /// bearer; exact retries are idempotent after a lost response or restart.
    pub async fn pair_approved_attested_automation(
        &self,
        key: DeviceKey,
        label: impl Into<String>,
        identity: DeviceAutomationIdentity,
        connection_secret_sha256: &str,
        now_ms: i64,
    ) -> Result<String, PairingError> {
        let token_digest = handset_secret_digest(connection_secret_sha256)?;
        let label = label.into();
        let expected_target_ref = automation_target_ref_parts(
            &key.principal,
            &key.workspace,
            &key.device_id,
            &token_digest,
            &identity.public_key_spki_base64,
        );
        self.pair_with_capabilities_and_digest(
            key,
            label,
            MobileClientKind::Android,
            vec![
                DeviceCapability::MobileClient,
                DeviceCapability::DeviceAutomation,
            ],
            Some(identity),
            token_digest,
            now_ms,
        )
        .await?;
        Ok(expected_target_ref)
    }

    async fn pair_with_capabilities(
        &self,
        key: DeviceKey,
        label: impl Into<String>,
        client_kind: MobileClientKind,
        capabilities: Vec<DeviceCapability>,
        automation_identity: Option<DeviceAutomationIdentity>,
        now_ms: i64,
    ) -> Result<String, PairingError> {
        let token = {
            let mut bytes = [0u8; 32];
            getrandom_bytes(&mut bytes);
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        };
        self.pair_with_capabilities_and_digest(
            key,
            label.into(),
            client_kind,
            capabilities,
            automation_identity,
            digest(&token),
            now_ms,
        )
        .await?;
        Ok(token)
    }

    async fn pair_with_capabilities_and_digest(
        &self,
        key: DeviceKey,
        label: String,
        client_kind: MobileClientKind,
        capabilities: Vec<DeviceCapability>,
        automation_identity: Option<DeviceAutomationIdentity>,
        token_digest: String,
        now_ms: i64,
    ) -> Result<(), PairingError> {
        let _serialized = self.write_lock.lock().await;
        let mut devices = self.devices.write().await;
        if automation_identity.is_none()
            && devices
                .get(&index_key(&key))
                .is_some_and(|device| device.allows(DeviceCapability::DeviceAutomation))
        {
            return Err(PairingError::AppsOwnerRequired);
        }
        if let Some(existing) = devices.get(&index_key(&key)) {
            if automation_identity.as_ref().is_some_and(|identity| {
                existing.token_digest == token_digest
                    && existing.label == label
                    && existing.client_kind == client_kind
                    && existing.capabilities == capabilities
                    && existing.automation_identity.as_ref() == Some(identity)
            }) {
                return Ok(());
            }
        }
        let previous_generation = devices
            .get(&index_key(&key))
            .map_or(0, |device| device.automation_review_generation);
        if !devices.contains_key(&index_key(&key)) {
            let scope_count = devices
                .values()
                .filter(|device| {
                    device.principal == key.principal && device.workspace == key.workspace
                })
                .count();
            if devices.len() >= MAX_PAIRED_DEVICES_GLOBAL
                || scope_count >= MAX_PAIRED_DEVICES_PER_SCOPE
            {
                return Err(PairingError::InvalidEnrollment);
            }
        }
        let rotated_generation = previous_generation
            .checked_add(1)
            .ok_or(PairingError::InvalidAutomationReview)?;
        let device = PairedDevice {
            principal: key.principal.clone(),
            workspace: key.workspace.clone(),
            device_id: key.device_id.clone(),
            token_digest,
            label,
            client_kind,
            capabilities,
            paired_at_ms: now_ms,
            last_seen_ms: None,
            automation_review_generation: rotated_generation,
            automation_review: None,
            automation_identity,
        };
        validate_automation_review(&device)?;
        // A credential rotation also rotates the Apps target. Terminate the
        // old socket before publishing either durable or in-memory authority,
        // so an action bound to the retired review cannot win the persist →
        // handler-revoke gap.
        if devices.contains_key(&index_key(&key)) {
            if let Some(hub) = global_hub() {
                hub.revoke(&key);
            }
        }
        let mut snapshot = devices.values().cloned().collect::<Vec<_>>();
        snapshot.retain(|existing| existing.key() != key);
        snapshot.push(device.clone());
        snapshot.sort_by(|left, right| index_key(&left.key()).cmp(&index_key(&right.key())));
        self.persist_snapshot(&snapshot).await?;
        devices.insert(index_key(&key), device);
        Ok(())
    }

    /// Authenticate a paired native client without trusting scope headers
    /// supplied by that client. The credential lookup returns its canonical
    /// owner scope, which middleware writes onto the request. Desktop Edge
    /// credentials deliberately carry `EdgeClient` instead of `MobileClient`;
    /// route handlers still enforce which socket each client kind may open.
    pub async fn authenticate_mobile(
        &self,
        device_id: &str,
        token: &str,
        now_ms: i64,
    ) -> Result<PairedDevice, PairingError> {
        let device_id = device_id.trim();
        let (matched, changed) = {
            let mut devices = self.devices.write().await;
            let device = devices
                .values_mut()
                .find(|device| {
                    device.device_id == device_id
                        && token_digest_matches(&device.token_digest, token.trim())
                        && (device.allows(DeviceCapability::MobileClient)
                            || device.allows(DeviceCapability::EdgeClient))
                })
                .ok_or_else(|| PairingError::BadToken(device_id.to_string()))?;
            let due = device.last_seen_ms.is_none_or(|seen| {
                now_ms.saturating_sub(seen) >= MOBILE_LAST_SEEN_WRITE_INTERVAL_MS
            });
            if due {
                device.last_seen_ms = Some(now_ms);
            }
            (device.clone(), due)
        };
        if changed {
            // Presence is diagnostic, not authorization state. A failed write
            // must not turn an otherwise valid mobile request into an outage.
            if let Err(error) = self.persist().await {
                tracing::warn!("[DEVICE-PAIRING] could not persist mobile presence: {error}");
            }
        }
        Ok(matched)
    }

    pub async fn verify_automation(
        &self,
        key: &DeviceKey,
        token: &str,
        now_ms: i64,
    ) -> Result<(), PairingError> {
        self.verify_automation_socket(key, token, now_ms)
            .await
            .map(|_| ())
    }

    /// Verify the Apps-only bearer and return the pinned key that must sign
    /// the server's exact socket-generation challenge before hub admission.
    pub async fn verify_automation_socket(
        &self,
        key: &DeviceKey,
        token: &str,
        now_ms: i64,
    ) -> Result<DeviceAutomationSocketAuthority, PairingError> {
        let authority = {
            let mut devices = self.devices.write().await;
            let device = devices
                .get_mut(&index_key(key))
                .ok_or_else(|| PairingError::NotPaired(key.device_id.clone()))?;
            if !token_digest_matches(&device.token_digest, token.trim())
                || !device.allows(DeviceCapability::DeviceAutomation)
            {
                return Err(PairingError::BadToken(key.device_id.clone()));
            }
            validate_automation_review(device)?;
            let identity = device
                .automation_identity
                .as_ref()
                .ok_or(PairingError::InvalidAutomationReview)?;
            device.last_seen_ms = Some(now_ms);
            DeviceAutomationSocketAuthority {
                target_ref: automation_target_ref(device),
                key_id: identity.key_id.clone(),
                public_key_spki_base64: identity.public_key_spki_base64.clone(),
                identity_digest: automation_identity_digest(identity),
                review_generation: device.automation_review_generation,
                apk_sha256: identity.apk_sha256.clone(),
                attestation_policy_digest: identity.attestation_policy_digest.clone(),
                app_package: identity.app_package.clone(),
                app_version_code: identity.app_version_code,
                app_signing_sha256: identity.app_signing_sha256.clone(),
                attestation_root_sha256: identity.attestation_root_sha256.clone(),
                attestation_security_level: identity.attestation_security_level.clone(),
                play_integrity_verdict_digest: None,
            }
        };
        let _ = self.persist().await;
        Ok(authority)
    }

    /// Revalidate an already-verified proof against the exact current durable
    /// authority and publish it to the hub while retaining the roster read
    /// guard. Review revoke/credential rotation therefore cannot slip between
    /// the last check and admission or let a stale proof evict a newer socket.
    pub async fn admit_automation_socket(
        &self,
        key: &DeviceKey,
        connection_id: DeviceConnectionId,
        authority: &DeviceAutomationSocketAuthority,
        hub: &DeviceBridgeHub,
        sink: Arc<dyn DeviceSink>,
    ) -> Result<bool, PairingError> {
        self.ensure_available()?;
        if authority.play_integrity_verdict_digest().is_none() {
            return Err(PairingError::InvalidAutomationReview);
        }
        let devices = self.devices.read().await;
        let device = devices
            .get(&index_key(key))
            .ok_or_else(|| PairingError::NotPaired(key.device_id.clone()))?;
        validate_automation_review(device)?;
        if !device.allows(DeviceCapability::DeviceAutomation) || !authority.matches_device(device) {
            return Err(PairingError::InvalidAutomationReview);
        }
        Ok(hub.connect(key.clone(), connection_id, authority.clone(), sink))
    }

    /// Verify a connecting device, and note that it was seen.
    pub async fn authenticate(&self, key: &DeviceKey, token: &str) -> Result<(), PairingError> {
        let devices = self.devices.read().await;
        let Some(device) = devices.get(&index_key(key)) else {
            return Err(PairingError::NotPaired(key.device_id.clone()));
        };
        if !token_digest_matches(&device.token_digest, token) {
            return Err(PairingError::BadToken(key.device_id.clone()));
        }
        Ok(())
    }

    /// Verify a connecting device, and note that it was seen.
    pub async fn verify(
        &self,
        key: &DeviceKey,
        token: &str,
        now_ms: i64,
    ) -> Result<(), PairingError> {
        self.authenticate(key, token).await?;
        // Recorded after the check, so a failed attempt never updates last-seen
        // and cannot be used to probe which device ids exist.
        if let Some(device) = self.devices.write().await.get_mut(&index_key(key)) {
            device.last_seen_ms = Some(now_ms);
        }
        let _ = self.persist().await;
        Ok(())
    }

    pub async fn unpair(&self, key: &DeviceKey) -> Result<bool, PairingError> {
        let _serialized = self.write_lock.lock().await;
        let mut devices = self.devices.write().await;
        let index = index_key(key);
        let present = devices.contains_key(&index);
        // Unpairing is a reducing action. Close the live physical owner before
        // the durable mutation so an already prepared Apps action cannot win a
        // roster-removal -> final-start gap. If persistence fails the old
        // authority remains in memory and on disk, but the handset must prove
        // a fresh socket generation before it can be used again.
        if present {
            if let Some(hub) = global_hub() {
                hub.revoke(key);
            }
        }
        let mut snapshot = devices.values().cloned().collect::<Vec<_>>();
        snapshot.retain(|device| device.key() != key.clone());
        snapshot.sort_by(|left, right| index_key(&left.key()).cmp(&index_key(&right.key())));
        self.persist_snapshot(&snapshot).await?;
        devices.remove(&index);
        Ok(present)
    }

    /// Legacy/mobile owner routes may remove only ordinary mobile records.
    /// Apps-capable identity rotation/revocation requires a signed desktop
    /// receipt so a paired handset cannot delete a sibling physical owner.
    pub async fn unpair_mobile(&self, key: &DeviceKey) -> Result<bool, PairingError> {
        let _serialized = self.write_lock.lock().await;
        let mut devices = self.devices.write().await;
        let index = index_key(key);
        if devices
            .get(&index)
            .is_some_and(|device| device.allows(DeviceCapability::DeviceAutomation))
        {
            return Err(PairingError::AppsOwnerRequired);
        }
        let present = devices.contains_key(&index);
        let mut snapshot = devices.values().cloned().collect::<Vec<_>>();
        snapshot.retain(|device| device.key() != key.clone());
        snapshot.sort_by(|left, right| index_key(&left.key()).cmp(&index_key(&right.key())));
        self.persist_snapshot(&snapshot).await?;
        devices.remove(&index);
        Ok(present)
    }

    /// Devices paired in one scope. Never returns digests.
    pub async fn list(&self, principal: &str, workspace: &str) -> Vec<PairedDevice> {
        let mut list = self
            .devices
            .read()
            .await
            .values()
            .filter(|device| device.principal == principal && device.workspace == workspace)
            .cloned()
            .map(|mut device| {
                device.token_digest = String::new();
                device
            })
            .collect::<Vec<_>>();
        list.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        list
    }

    /// Mobile/general HTTP callers cannot enumerate Apps-capable records or
    /// learn their raw device ids. Trusted Settings uses opaque automation
    /// targets through the signed native channel instead.
    pub async fn list_mobile(&self, principal: &str, workspace: &str) -> Vec<PairedDevice> {
        self.list(principal, workspace)
            .await
            .into_iter()
            .filter(|device| !device.allows(DeviceCapability::DeviceAutomation))
            .collect()
    }

    /// Stable, non-secret selectors for the owner's Apps installation UI.
    /// Re-pairing rotates the underlying credential and therefore rotates the
    /// selector, forcing every previously reviewed grant to be reviewed again.
    pub async fn list_automation_targets(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Vec<PairedAutomationTarget> {
        let mut targets = self
            .devices
            .read()
            .await
            .values()
            .filter(|device| {
                device.principal == principal
                    && device.workspace == workspace
                    && device.allows(DeviceCapability::DeviceAutomation)
            })
            .map(|device| PairedAutomationTarget {
                principal: device.principal.clone(),
                workspace: device.workspace.clone(),
                target_ref: automation_target_ref(device),
                label: device.label.clone(),
                enrollment_id: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.enrollment_id.clone())
                    .unwrap_or_default(),
                key_id: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.key_id.clone())
                    .unwrap_or_default(),
                automation_identity_digest: device
                    .automation_identity
                    .as_ref()
                    .map(automation_identity_digest)
                    .unwrap_or_default(),
                app_package: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.app_package.clone())
                    .unwrap_or_default(),
                app_version_code: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.app_version_code)
                    .unwrap_or_default(),
                app_signing_sha256: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.app_signing_sha256.clone())
                    .unwrap_or_default(),
                apk_sha256: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.apk_sha256.clone())
                    .unwrap_or_default(),
                attestation_root_sha256: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.attestation_root_sha256.clone())
                    .unwrap_or_default(),
                attestation_security_level: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.attestation_security_level.clone())
                    .unwrap_or_default(),
                attestation_policy_digest: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.attestation_policy_digest.clone())
                    .unwrap_or_default(),
                paired_at_ms: device.paired_at_ms,
                last_seen_ms: device.last_seen_ms,
                review_generation: device.automation_review_generation,
                review: device.automation_review.clone(),
            })
            .collect::<Vec<_>>();
        targets.sort_by(|left, right| left.target_ref.cmp(&right.target_ref));
        targets
    }

    /// Bounded trusted-owner projection across all local scopes. This is used
    /// only by the desktop-signed Settings channel; ordinary mobile routes
    /// continue to receive a scope-filtered list that omits Apps records.
    pub async fn list_all_automation_targets(&self) -> Vec<PairedAutomationTarget> {
        let mut targets = self
            .devices
            .read()
            .await
            .values()
            .filter(|device| device.allows(DeviceCapability::DeviceAutomation))
            .map(|device| PairedAutomationTarget {
                principal: device.principal.clone(),
                workspace: device.workspace.clone(),
                target_ref: automation_target_ref(device),
                label: device.label.clone(),
                enrollment_id: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.enrollment_id.clone())
                    .unwrap_or_default(),
                key_id: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.key_id.clone())
                    .unwrap_or_default(),
                automation_identity_digest: device
                    .automation_identity
                    .as_ref()
                    .map(automation_identity_digest)
                    .unwrap_or_default(),
                app_package: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.app_package.clone())
                    .unwrap_or_default(),
                app_version_code: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.app_version_code)
                    .unwrap_or_default(),
                app_signing_sha256: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.app_signing_sha256.clone())
                    .unwrap_or_default(),
                apk_sha256: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.apk_sha256.clone())
                    .unwrap_or_default(),
                attestation_root_sha256: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.attestation_root_sha256.clone())
                    .unwrap_or_default(),
                attestation_security_level: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.attestation_security_level.clone())
                    .unwrap_or_default(),
                attestation_policy_digest: device
                    .automation_identity
                    .as_ref()
                    .map(|identity| identity.attestation_policy_digest.clone())
                    .unwrap_or_default(),
                paired_at_ms: device.paired_at_ms,
                last_seen_ms: device.last_seen_ms,
                review_generation: device.automation_review_generation,
                review: device.automation_review.clone(),
            })
            .collect::<Vec<_>>();
        targets.sort_by(|left, right| {
            (&left.principal, &left.workspace, &left.target_ref).cmp(&(
                &right.principal,
                &right.workspace,
                &right.target_ref,
            ))
        });
        targets
    }

    /// Resolve only an exact already-published handset-generated Apps
    /// credential. This supports byte-identical enrollment polling without
    /// returning or persisting the plaintext connection secret.
    pub async fn approved_automation_enrollment(
        &self,
        enrollment_id: &str,
        device_id: &str,
        key_id: &str,
        connection_secret_sha256: &str,
    ) -> Result<Option<PairedAutomationTarget>, PairingError> {
        self.ensure_available()?;
        let token_digest = handset_secret_digest(connection_secret_sha256)?;
        let devices = self.devices.read().await;
        let target = devices.values().find(|device| {
            device.device_id == device_id
                && device.token_digest == token_digest
                && device.allows(DeviceCapability::DeviceAutomation)
                && device.automation_identity.as_ref().is_some_and(|identity| {
                    identity.enrollment_id == enrollment_id && identity.key_id == key_id
                })
        });
        target.map(paired_automation_target).transpose()
    }

    /// Current durable owner approvals for this scope. Apps may select a
    /// target only from this list and fail closed on ambiguity; public action
    /// arguments never contribute packages or physical identity.
    pub(crate) async fn active_automation_reviews(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Vec<DeviceAutomationReview> {
        let mut reviews = self
            .devices
            .read()
            .await
            .values()
            .filter(|device| device.principal == principal && device.workspace == workspace)
            .filter_map(|device| device.automation_review.clone())
            .collect::<Vec<_>>();
        reviews.sort_by(|left, right| left.target_ref.cmp(&right.target_ref));
        reviews
    }

    /// Atomically replace the exact eight-action/package review after an
    /// authenticated owner confirms it. `expected_generation` is the value
    /// returned by the roster and makes stale tabs harmless.
    pub async fn review_automation_actions(
        &self,
        key: &DeviceKey,
        expected_generation: u64,
        allowed_packages: Vec<String>,
        now_ms: i64,
    ) -> Result<DeviceAutomationReview, PairingError> {
        let packages = allowed_packages
            .into_iter()
            .map(|package| package.trim().to_owned())
            .collect::<BTreeSet<_>>();
        if packages.is_empty()
            || packages.len() > MAX_AUTOMATION_PACKAGES
            || packages
                .iter()
                .any(|package| !valid_android_package(package))
        {
            return Err(PairingError::InvalidAutomationReview);
        }
        let packages = packages.into_iter().collect::<Vec<_>>();
        let _serialized = self.write_lock.lock().await;
        let mut devices = self.devices.write().await;
        let index = index_key(key);
        let current = devices
            .get(&index)
            .ok_or_else(|| PairingError::NotPaired(key.device_id.clone()))?;
        if !current.allows(DeviceCapability::DeviceAutomation)
            || current.client_kind != MobileClientKind::Android
        {
            return Err(PairingError::InvalidAutomationReview);
        }
        if current.automation_review_generation != expected_generation {
            return Err(PairingError::AutomationReviewConflict);
        }
        let generation = expected_generation
            .checked_add(1)
            .ok_or(PairingError::InvalidAutomationReview)?;
        let target_ref = automation_target_ref(current);
        let review = DeviceAutomationReview {
            target_ref: target_ref.clone(),
            generation,
            review_digest: automation_review_digest(current, &target_ref, generation, &packages),
            actions: DEVICE_AUTOMATION_ACTION_ROSTER.to_vec(),
            allowed_packages: packages,
            reviewed_at_ms: now_ms,
        };
        let mut updated = current.clone();
        updated.automation_review_generation = generation;
        updated.automation_review = Some(review.clone());
        validate_automation_review(&updated)?;
        // Review generation and socket generation advance together. This is a
        // reducing action even if the disk write later fails; the handset may
        // reconnect only after the caller observes the outcome.
        if let Some(hub) = global_hub() {
            hub.revoke(key);
        }
        let mut snapshot = devices.values().cloned().collect::<Vec<_>>();
        snapshot.retain(|device| device.key() != key.clone());
        snapshot.push(updated.clone());
        snapshot.sort_by(|left, right| index_key(&left.key()).cmp(&index_key(&right.key())));
        self.persist_snapshot(&snapshot).await?;
        devices.insert(index, updated);
        Ok(review)
    }

    /// Apply an owner review through the opaque target projected by the
    /// trusted Apps owner channel. Raw device ids never cross that boundary.
    pub async fn review_automation_actions_target(
        &self,
        principal: &str,
        workspace: &str,
        target_ref: &str,
        expected_generation: u64,
        allowed_packages: Vec<String>,
        now_ms: i64,
    ) -> Result<DeviceAutomationReview, PairingError> {
        let key = self
            .automation_target_key(principal, workspace, target_ref)
            .await?;
        self.review_automation_actions(&key, expected_generation, allowed_packages, now_ms)
            .await
    }

    /// Revoke only Apps automation while leaving ordinary mobile pairing
    /// intact. The tombstone generation is durable, so a stale approval cannot
    /// resurrect authority after this returns.
    pub async fn revoke_automation_review(
        &self,
        key: &DeviceKey,
        expected_generation: u64,
    ) -> Result<u64, PairingError> {
        let _serialized = self.write_lock.lock().await;
        let mut devices = self.devices.write().await;
        let index = index_key(key);
        let current = devices
            .get(&index)
            .ok_or_else(|| PairingError::NotPaired(key.device_id.clone()))?;
        if current.automation_review_generation != expected_generation {
            return Err(PairingError::AutomationReviewConflict);
        }
        let generation = expected_generation
            .checked_add(1)
            .ok_or(PairingError::InvalidAutomationReview)?;
        let mut updated = current.clone();
        updated.automation_review_generation = generation;
        updated.automation_review = None;
        if let Some(hub) = global_hub() {
            hub.revoke(key);
        }
        let mut snapshot = devices.values().cloned().collect::<Vec<_>>();
        snapshot.retain(|device| device.key() != key.clone());
        snapshot.push(updated.clone());
        snapshot.sort_by(|left, right| index_key(&left.key()).cmp(&index_key(&right.key())));
        self.persist_snapshot(&snapshot).await?;
        devices.insert(index, updated);
        Ok(generation)
    }

    pub async fn revoke_automation_review_target(
        &self,
        principal: &str,
        workspace: &str,
        target_ref: &str,
        expected_generation: u64,
    ) -> Result<u64, PairingError> {
        let key = self
            .automation_target_key(principal, workspace, target_ref)
            .await?;
        self.revoke_automation_review(&key, expected_generation)
            .await
    }

    async fn automation_target_key(
        &self,
        principal: &str,
        workspace: &str,
        target_ref: &str,
    ) -> Result<DeviceKey, PairingError> {
        let devices = self.devices.read().await;
        let mut matches = devices.values().filter(|device| {
            device.principal == principal
                && device.workspace == workspace
                && device.client_kind == MobileClientKind::Android
                && device.allows(DeviceCapability::DeviceAutomation)
                && automation_target_ref(device) == target_ref
        });
        let key = matches
            .next()
            .map(PairedDevice::key)
            .ok_or_else(|| PairingError::NotPaired(target_ref.to_owned()))?;
        if matches.next().is_some() {
            return Err(PairingError::Corrupt(
                "Android automation target is ambiguous".to_owned(),
            ));
        }
        Ok(key)
    }

    /// Revalidate the exact owner approval at the last possible workflow
    /// boundary and return only the private transport key.
    pub(crate) async fn resolve_automation_review(
        &self,
        principal: &str,
        workspace: &str,
        target_ref: &str,
        generation: u64,
        review_digest: &str,
    ) -> Result<DeviceKey, PairingError> {
        let devices = self.devices.read().await;
        let mut matches = devices.values().filter(|device| {
            device.principal == principal
                && device.workspace == workspace
                && device.automation_review.as_ref().is_some_and(|review| {
                    review.target_ref == target_ref
                        && review.generation == generation
                        && digests_match(&review.review_digest, review_digest)
                })
        });
        let Some(device) = matches.next() else {
            return Err(PairingError::NotPaired(target_ref.to_owned()));
        };
        if matches.next().is_some() {
            return Err(PairingError::InvalidAutomationReview);
        }
        Ok(device.key())
    }

    /// Resolve one reviewed opaque selector to its private transport key.
    /// Multiple connected or paired phones are never guessed: the exact
    /// selector has to be present in the owner-approved app grant.
    #[cfg(test)]
    pub(crate) async fn resolve_automation_target(
        &self,
        principal: &str,
        workspace: &str,
        target_ref: &str,
    ) -> Result<DeviceKey, PairingError> {
        let devices = self.devices.read().await;
        let mut matches = devices.values().filter(|device| {
            device.principal == principal
                && device.workspace == workspace
                && device.allows(DeviceCapability::DeviceAutomation)
                && automation_target_ref(device) == target_ref
        });
        let Some(device) = matches.next() else {
            return Err(PairingError::NotPaired(target_ref.to_owned()));
        };
        if matches.next().is_some() {
            // The digest is collision-resistant, but treating a collision as
            // ambiguity keeps this authority boundary fail-closed.
            return Err(PairingError::InvalidEnrollment);
        }
        Ok(device.key())
    }

    /// Return the server-owned client kind for an already authenticated
    /// device. Mobile APIs use this after middleware has resolved scope; they
    /// must never let a handset claim iOS or Android privileges in its body.
    pub async fn client_kind(
        &self,
        principal: &str,
        workspace: &str,
        device_id: &str,
    ) -> Option<MobileClientKind> {
        let key = DeviceKey::new(principal, workspace, device_id);
        self.devices
            .read()
            .await
            .get(&index_key(&key))
            .map(|device| device.client_kind)
    }
}

fn read_private_pairing_roster(path: &Path) -> Result<Option<Vec<u8>>, PairingError> {
    use std::io::Read as _;

    let named = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if named.file_type().is_symlink() || !named.is_file() || named.len() > MAX_PAIRING_ROSTER_BYTES
    {
        return Err(PairingError::Corrupt(
            "paired device roster is not a bounded regular file".to_owned(),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        PairingError::Corrupt("paired device roster has no owner directory".to_owned())
    })?;
    validate_private_pairing_directory(parent)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        if named.uid() != unsafe { libc::geteuid() }
            || named.mode() & 0o777 != 0o600
            || named.nlink() != 1
        {
            return Err(PairingError::Corrupt(
                "paired device roster owner, mode, or link count is invalid".to_owned(),
            ));
        }
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path)?;
    let opened = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if !opened.is_file()
            || opened.dev() != named.dev()
            || opened.ino() != named.ino()
            || opened.uid() != named.uid()
            || opened.mode() != named.mode()
        {
            return Err(PairingError::Corrupt(
                "paired device roster changed during no-follow open".to_owned(),
            ));
        }
    }
    #[cfg(not(unix))]
    if !opened.is_file() {
        return Err(PairingError::Corrupt(
            "paired device roster changed during open".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
    file.by_ref()
        .take(MAX_PAIRING_ROSTER_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_PAIRING_ROSTER_BYTES
        || after.len() != opened.len()
        || after.modified().ok() != opened.modified().ok()
    {
        return Err(PairingError::Corrupt(
            "paired device roster changed during bounded read".to_owned(),
        ));
    }
    Ok(Some(bytes))
}

fn ensure_private_pairing_directory(path: &Path) -> Result<(), PairingError> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    validate_private_pairing_directory(path)
}

fn validate_private_pairing_directory(path: &Path) -> Result<(), PairingError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PairingError::Corrupt(
            "paired device owner directory is not a real directory".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            return Err(PairingError::Corrupt(
                "paired device owner directory must be owned by the runtime user with mode 0700"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

async fn write_and_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path).await?;
    file.write_all(bytes).await?;
    file.sync_all().await
}

fn getrandom_bytes(buffer: &mut [u8]) {
    use rand::RngCore;
    rand::thread_rng().fill_bytes(buffer);
}

fn random_url_token(byte_count: usize) -> String {
    let mut bytes = vec![0u8; byte_count];
    getrandom_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn key() -> DeviceKey {
        DeviceKey::new("owner", "default", "pixel-9")
    }

    fn automation_identity(seed: &str) -> DeviceAutomationIdentity {
        DeviceAutomationIdentity {
            enrollment_id: format!("enrollment-{seed}-123456"),
            key_id: format!("test-key-{seed}-123456"),
            public_key_spki_base64: format!("{seed}{}", "A".repeat(96)),
            app_package: "ai.magicbeans.magdroid".to_owned(),
            app_version_code: 11,
            app_signing_sha256: "a".repeat(64),
            apk_sha256: "d".repeat(64),
            attestation_chain_sha256: "b".repeat(64),
            attestation_root_sha256: "c".repeat(64),
            attestation_security_level: "tee".to_owned(),
            attestation_policy_digest: format!("blake3:{}", "e".repeat(64)),
            enrolled_at_ms: 1_000,
        }
    }

    #[test]
    fn automation_identity_digest_uses_canonical_blake3_encoding() {
        let digest = automation_identity_digest(&automation_identity("canonical"));
        let hex = digest
            .strip_prefix("blake3:")
            .expect("automation identity digest must identify its algorithm");
        assert_eq!(hex.len(), 64);
        assert!(hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
    }

    #[test]
    fn ambiguous_anchor_reconciliation_requires_exact_generation_and_seal() {
        let exact = PairingRosterGenerationAnchor {
            generation: 7,
            seal_hex: "a".repeat(64),
        };
        assert!(generation_anchor_matches(&exact, 7, &"a".repeat(64)));
        assert!(!generation_anchor_matches(&exact, 6, &"a".repeat(64)));
        assert!(!generation_anchor_matches(&exact, 7, &"b".repeat(64)));

        let unavailable = DevicePairingStore::unavailable(
            tempfile::tempdir().unwrap().path(),
            "ambiguous anchor requires reopen",
        );
        assert!(unavailable.reopen_required.load(AtomicOrdering::Acquire));
        assert!(matches!(
            unavailable.ensure_available(),
            Err(PairingError::SealUnavailable(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fresh_roster_checks_private_parent_before_advertising_readiness() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("system");
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        assert!(store.ensure_available().is_ok());
        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(store);
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            DevicePairingStore::open(temp.path()).await,
            Err(PairingError::Corrupt(_))
        ));
        assert!(!parent.join("paired-devices.json").exists());
        assert_eq!(
            std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[tokio::test]
    async fn approved_enrollment_retry_is_idempotent_and_never_persists_plaintext_secret() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let key = DeviceKey::new("owner", "private", "pixel-approved");
        let identity = automation_identity("approved");
        let connection_secret = "handset-plaintext-connection-secret";
        let connection_secret_sha256 = {
            use sha2::Digest as _;
            hex::encode(sha2::Sha256::digest(connection_secret.as_bytes()))
        };
        let first = store
            .pair_approved_attested_automation(
                key.clone(),
                "Approved phone".to_owned(),
                identity.clone(),
                &connection_secret_sha256,
                1_000,
            )
            .await
            .unwrap();
        let second = store
            .pair_approved_attested_automation(
                key,
                "Approved phone".to_owned(),
                identity,
                &connection_secret_sha256,
                2_000,
            )
            .await
            .unwrap();
        assert_eq!(first, second);
        let bytes = std::fs::read(temp.path().join("system/paired-devices.json")).unwrap();
        let persisted = String::from_utf8(bytes).unwrap();
        assert!(!persisted.contains(&connection_secret));
        assert!(persisted.contains(&format!("sha256:{connection_secret_sha256}")));
    }

    #[tokio::test]
    async fn a_paired_device_verifies_and_an_unpaired_one_does_not() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();

        // Nothing is paired yet: a fresh install is not an error, it is a refusal.
        assert!(matches!(
            store.verify(&key(), "anything", 1).await,
            Err(PairingError::NotPaired(_))
        ));

        let token = store.pair(key(), "Pixel 9", 100).await.unwrap();
        assert!(store.verify(&key(), &token, 200).await.is_ok());
    }

    #[tokio::test]
    async fn esp_bootstrap_records_an_esp_kind_with_mobile_only_authority() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let device = DeviceKey::new("owner", "private", "esp32-c6");

        let token = store
            .pair_esp32(device.clone(), "Desk terminal", 100)
            .await
            .unwrap();

        assert_eq!(
            store.client_kind("owner", "private", "esp32-c6").await,
            Some(MobileClientKind::Esp32)
        );
        assert!(store.verify(&device, &token, 200).await.is_ok());
        assert!(store.verify_automation(&device, &token, 200).await.is_err());
    }

    #[tokio::test]
    async fn a_wrong_token_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        store.pair(key(), "Pixel 9", 100).await.unwrap();

        assert!(matches!(
            store.verify(&key(), "not-the-token", 200).await,
            Err(PairingError::BadToken(_))
        ));
    }

    /// The token that opens one owner's phone must not open another's, even
    /// when the device id is identical.
    #[tokio::test]
    async fn a_token_does_not_cross_scopes() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let token = store.pair(key(), "Pixel 9", 100).await.unwrap();

        let other = DeviceKey::new("someone-else", "default", "pixel-9");
        assert!(matches!(
            store.verify(&other, &token, 200).await,
            Err(PairingError::NotPaired(_))
        ));
    }

    /// The roster survives a restart, or every device is locked out after one.
    #[tokio::test]
    async fn pairings_survive_a_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let token = {
            let store = DevicePairingStore::open(temp.path()).await.unwrap();
            store.pair(key(), "Pixel 9", 100).await.unwrap()
        };
        let reopened = DevicePairingStore::open(temp.path()).await.unwrap();
        assert!(reopened.verify(&key(), &token, 300).await.is_ok());
    }

    #[tokio::test]
    async fn unsigned_legacy_roster_is_sealed_as_mobile_only_authority() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("system/paired-devices.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                path.parent().unwrap(),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let token = "legacy-mobile-token";
        let legacy = PairedDevice {
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            device_id: "legacy-phone".to_owned(),
            token_digest: digest(token),
            label: "Legacy phone".to_owned(),
            client_kind: MobileClientKind::Android,
            capabilities: vec![
                DeviceCapability::MobileClient,
                DeviceCapability::DeviceAutomation,
            ],
            paired_at_ms: 10,
            last_seen_ms: None,
            automation_review_generation: 9,
            automation_review: None,
            automation_identity: None,
        };
        std::fs::write(&path, serde_json::to_vec_pretty(&vec![legacy]).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let key = DeviceKey::new("owner", "default", "legacy-phone");
        assert!(store.verify(&key, token, 20).await.is_ok());
        let migrated = store.list_mobile("owner", "default").await;
        assert_eq!(migrated.len(), 1);
        assert_eq!(
            migrated[0].capabilities,
            vec![DeviceCapability::MobileClient]
        );
        assert_eq!(migrated[0].automation_review_generation, 0);
        assert!(migrated[0].automation_identity.is_none());
        assert!(migrated[0].automation_review.is_none());

        let document: SealedPairingRoster =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document.schema, PAIRING_ROSTER_SCHEMA);
        assert!(document.generation > 0);
        assert!(DevicePairingStore::open(temp.path()).await.is_ok());
    }

    #[tokio::test]
    async fn missing_owner_key_disables_pairing_without_an_in_memory_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::unavailable(temp.path(), "keychain unavailable");
        assert!(matches!(
            store
                .begin_mobile_enrollment("owner", "default", MobileClientKind::Ios, 90)
                .await,
            Err(PairingError::SealUnavailable(_))
        ));
        assert_eq!(store.pending_enrollment_count().await, 0);
        assert!(matches!(
            store.pair(key(), "Pixel 9", 100).await,
            Err(PairingError::SealUnavailable(_))
        ));
        assert!(!temp.path().join("system/paired-devices.json").exists());
    }

    #[tokio::test]
    async fn validly_sealed_older_generation_is_rejected_as_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        store.pair(key(), "Pixel 9", 100).await.unwrap();
        let path = temp.path().join("system/paired-devices.json");
        let old_generation = std::fs::read(&path).unwrap();
        store
            .pair(
                DeviceKey::new("owner", "default", "pixel-10"),
                "Pixel 10",
                200,
            )
            .await
            .unwrap();
        drop(store);
        std::fs::write(&path, old_generation).unwrap();
        assert!(matches!(
            DevicePairingStore::open(temp.path()).await,
            Err(PairingError::Corrupt(_))
        ));
    }

    /// The stored roster must not contain a usable credential.
    #[tokio::test]
    async fn the_token_is_never_written_to_disk() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let token = store.pair(key(), "Pixel 9", 100).await.unwrap();

        let raw = tokio::fs::read_to_string(temp.path().join("system/paired-devices.json"))
            .await
            .unwrap();
        assert!(
            !raw.contains(&token),
            "the roster is holding the token itself"
        );
        assert!(raw.contains(&digest(&token)));
    }

    /// Listing is for showing an owner their devices, so it must not hand back
    /// the digest either.
    #[tokio::test]
    async fn listing_withholds_the_digest() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        store.pair(key(), "Pixel 9", 100).await.unwrap();

        let listed = store.list("owner", "default").await;
        assert_eq!(listed.len(), 1);
        assert!(listed[0].token_digest.is_empty());
        assert_eq!(listed[0].label, "Pixel 9");
    }

    #[tokio::test]
    async fn unpairing_revokes_the_token() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let token = store.pair(key(), "Pixel 9", 100).await.unwrap();

        assert!(store.unpair(&key()).await.unwrap());
        assert!(matches!(
            store.verify(&key(), &token, 300).await,
            Err(PairingError::NotPaired(_))
        ));
    }

    /// A failed attempt must not update last-seen: otherwise it becomes an
    /// oracle for which device ids exist.
    #[tokio::test]
    async fn a_failed_attempt_leaves_no_trace() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        store.pair(key(), "Pixel 9", 100).await.unwrap();

        let _ = store.verify(&key(), "wrong", 200).await;
        assert_eq!(store.list("owner", "default").await[0].last_seen_ms, None);
    }

    async fn leftover_temp_files(root: &Path) -> Vec<String> {
        let mut left = Vec::new();
        let mut entries = tokio::fs::read_dir(root.join("system"))
            .await
            .expect("roster directory");
        while let Some(entry) = entries.next_entry().await.expect("roster entry") {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                left.push(name);
            }
        }
        left
    }

    /// `verify` persists on every accepted connection, and it snapshotted under
    /// a **read** guard into a fixed `paired-devices.json.tmp` — so a handful of
    /// devices reconnecting together put several writers inside one file, and
    /// the roster the rename published was whichever mixture won.
    ///
    /// The cost is not a bad row. `open` refuses to parse a torn roster and the
    /// daemon refuses to start over it, so the failure mode is every device
    /// locked out and no daemon to re-pair them with.
    #[tokio::test]
    async fn concurrent_persists_leave_a_whole_roster_and_no_temp() {
        const DEVICES: usize = 8;

        let temp = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(DevicePairingStore::open(temp.path()).await.unwrap());

        let mut paired = Vec::with_capacity(DEVICES);
        for index in 0..DEVICES {
            let device = DeviceKey::new("owner", "default", format!("device-{index}"));
            let token = store
                .pair(device.clone(), format!("Device {index}"), 100)
                .await
                .unwrap();
            paired.push((device, token));
        }

        // Every device reconnecting at once — each accepted connection persists.
        let mut joins = Vec::with_capacity(DEVICES);
        for (device, token) in paired.clone() {
            let store = std::sync::Arc::clone(&store);
            joins.push(tokio::spawn(async move {
                store.verify(&device, &token, 200).await
            }));
        }
        for join in joins {
            join.await.expect("verify task").expect("accepted device");
        }

        assert!(
            leftover_temp_files(temp.path()).await.is_empty(),
            "a completed write left its temp behind, holding the roster under a name nothing cleans up"
        );

        let reopened = DevicePairingStore::open(temp.path())
            .await
            .expect("the roster must still parse after concurrent writes");
        assert_eq!(reopened.list("owner", "default").await.len(), DEVICES);
        for (device, token) in paired {
            assert!(
                reopened.verify(&device, &token, 300).await.is_ok(),
                "a device was locked out by a concurrent persist"
            );
        }
    }

    #[tokio::test]
    async fn roster_rejects_oversized_symlinked_and_over_capacity_files_before_authority_load() {
        let oversized = tempfile::tempdir().unwrap();
        let oversized_path = oversized.path().join("system/paired-devices.json");
        std::fs::create_dir_all(oversized_path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&oversized_path).unwrap();
        file.set_len(MAX_PAIRING_ROSTER_BYTES + 1).unwrap();
        assert!(matches!(
            DevicePairingStore::open(oversized.path()).await,
            Err(PairingError::Corrupt(_))
        ));

        #[cfg(unix)]
        {
            let symlinked = tempfile::tempdir().unwrap();
            let path = symlinked.path().join("system/paired-devices.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let target = symlinked.path().join("outside.json");
            std::fs::write(&target, "[]").unwrap();
            std::os::unix::fs::symlink(target, path).unwrap();
            assert!(matches!(
                DevicePairingStore::open(symlinked.path()).await,
                Err(PairingError::Corrupt(_))
            ));
        }

        let crowded = tempfile::tempdir().unwrap();
        let path = crowded.path().join("system/paired-devices.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let devices = (0..=MAX_PAIRED_DEVICES_GLOBAL)
            .map(|index| PairedDevice {
                principal: format!("owner-{index}"),
                workspace: "default".to_owned(),
                device_id: format!("device-{index}"),
                token_digest: "a".repeat(64),
                label: "fixture".to_owned(),
                client_kind: MobileClientKind::Android,
                capabilities: vec![DeviceCapability::MobileClient],
                paired_at_ms: 1,
                last_seen_ms: None,
                automation_review_generation: 1,
                automation_review: None,
                automation_identity: None,
            })
            .collect::<Vec<_>>();
        std::fs::write(&path, serde_json::to_vec(&devices).unwrap()).unwrap();
        assert!(matches!(
            DevicePairingStore::open(crowded.path()).await,
            Err(PairingError::Corrupt(_))
        ));
    }

    /// A write that cannot complete must not leave the roster lying beside the
    /// real file. Occupying the rename target with a directory fails the rename
    /// after the temp is fully written, which is the path that leaks.
    #[tokio::test]
    async fn a_failed_publish_removes_its_temp() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        tokio::fs::create_dir_all(temp.path().join("system").join("paired-devices.json"))
            .await
            .unwrap();

        assert!(store.pair(key(), "Pixel 9", 100).await.is_err());
        assert!(
            leftover_temp_files(temp.path()).await.is_empty(),
            "a failed write leaked the roster into the scope"
        );
    }

    #[tokio::test]
    async fn an_enrollment_is_single_use_and_inherits_its_owner_scope() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let ticket = store
            .begin_enrollment("owner", "project", 1_000)
            .await
            .unwrap();

        let grant = store
            .exchange_enrollment(
                &ticket.enrollment_id,
                &ticket.secret,
                "phone-1",
                "My phone",
                2_000,
            )
            .await
            .unwrap();
        assert_eq!(grant.principal, "owner");
        assert_eq!(grant.workspace, "project");
        assert!(store
            .verify(
                &DeviceKey::new("owner", "project", "phone-1"),
                &grant.token,
                3_000,
            )
            .await
            .is_ok());
        assert!(matches!(
            store
                .exchange_enrollment(
                    &ticket.enrollment_id,
                    &ticket.secret,
                    "phone-2",
                    "Second phone",
                    3_000,
                )
                .await,
            Err(PairingError::InvalidEnrollment)
        ));
    }

    #[tokio::test]
    async fn ios_enrollment_can_use_mobile_apis_but_never_the_automation_bridge() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let ticket = store
            .begin_mobile_enrollment("owner", "private", MobileClientKind::Ios, 1_000)
            .await
            .unwrap();
        let grant = store
            .exchange_enrollment(
                &ticket.enrollment_id,
                &ticket.secret,
                "iphone-1",
                "iPhone",
                2_000,
            )
            .await
            .unwrap();

        assert_eq!(grant.client_kind, MobileClientKind::Ios);
        assert_eq!(grant.capabilities, vec![DeviceCapability::MobileClient]);
        let canonical = store
            .authenticate_mobile("iphone-1", &grant.token, 3_000)
            .await
            .unwrap();
        assert_eq!(
            (canonical.principal.as_str(), canonical.workspace.as_str()),
            ("owner", "private")
        );
        assert!(store
            .verify_automation(&canonical.key(), &grant.token, 3_000)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn enrollment_grant_keeps_the_exact_route_selected_by_the_owner() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let ticket = store
            .begin_mobile_enrollment_at_origin(
                "owner",
                "private",
                MobileClientKind::Ios,
                Some("http://192.168.1.20:3002".to_owned()),
                1_000,
            )
            .await
            .unwrap();
        let grant = store
            .exchange_enrollment(
                &ticket.enrollment_id,
                &ticket.secret,
                "iphone-local",
                "iPhone",
                2_000,
            )
            .await
            .unwrap();

        assert_eq!(
            grant.connection_origin.as_deref(),
            Some("http://192.168.1.20:3002")
        );
    }

    #[tokio::test]
    async fn desktop_enrollment_mints_only_revocable_edge_authority() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let ticket = store
            .begin_mobile_enrollment("owner", "private", MobileClientKind::Desktop, 1_000)
            .await
            .unwrap();
        let grant = store
            .exchange_enrollment(
                &ticket.enrollment_id,
                &ticket.secret,
                "linux-workstation",
                "Linux workstation",
                2_000,
            )
            .await
            .unwrap();

        assert_eq!(grant.client_kind, MobileClientKind::Desktop);
        assert_eq!(grant.capabilities, vec![DeviceCapability::EdgeClient]);
        let canonical = store
            .authenticate_mobile("linux-workstation", &grant.token, 3_000)
            .await
            .unwrap();
        assert_eq!(canonical.principal, "owner");
        assert_eq!(canonical.workspace, "private");
        assert!(!canonical.allows(DeviceCapability::MobileClient));
        assert!(!canonical.allows(DeviceCapability::DeviceAutomation));

        let key = canonical.key();
        assert!(store.unpair_mobile(&key).await.unwrap());
        assert!(store
            .authenticate_mobile("linux-workstation", &grant.token, 4_000)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn mobile_authentication_derives_scope_from_the_roster_not_phone_headers() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let ticket = store
            .begin_mobile_enrollment("owner", "project", MobileClientKind::Android, 1_000)
            .await
            .unwrap();
        let grant = store
            .exchange_enrollment(
                &ticket.enrollment_id,
                &ticket.secret,
                "phone-1",
                "Phone",
                2_000,
            )
            .await
            .unwrap();

        let canonical = store
            .authenticate_mobile("phone-1", &grant.token, 3_000)
            .await
            .unwrap();
        assert_eq!(canonical.principal, "owner");
        assert_eq!(canonical.workspace, "project");
        assert!(
            !canonical.allows(DeviceCapability::DeviceAutomation),
            "legacy mobile enrollment must never acquire Apps authority"
        );
        assert_eq!(canonical.last_seen_ms, Some(3_000));
        let throttled = store
            .authenticate_mobile("phone-1", &grant.token, 4_000)
            .await
            .unwrap();
        assert_eq!(throttled.last_seen_ms, Some(3_000));
        let advanced = store
            .authenticate_mobile("phone-1", &grant.token, 63_000)
            .await
            .unwrap();
        assert_eq!(advanced.last_seen_ms, Some(63_000));
        assert!(store
            .authenticate_mobile("phone-1", "wrong", 64_000)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_bad_secret_does_not_burn_the_real_enrollment() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let ticket = store
            .begin_enrollment("owner", "default", 1_000)
            .await
            .unwrap();

        assert!(store
            .exchange_enrollment(&ticket.enrollment_id, "wrong", "phone-1", "Phone", 2_000,)
            .await
            .is_err());
        assert!(store
            .exchange_enrollment(
                &ticket.enrollment_id,
                &ticket.secret,
                "phone-1",
                "Phone",
                2_001,
            )
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn expired_and_cancelled_enrollments_cannot_be_exchanged() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let expired = store
            .begin_enrollment("owner", "default", 1_000)
            .await
            .unwrap();
        assert!(store
            .exchange_enrollment(
                &expired.enrollment_id,
                &expired.secret,
                "phone-1",
                "Phone",
                expired.expires_at_ms,
            )
            .await
            .is_err());

        let cancelled = store
            .begin_enrollment("owner", "default", 2_000)
            .await
            .unwrap();
        assert!(
            !store
                .cancel_enrollment("another", "default", &cancelled.enrollment_id)
                .await
        );
        assert!(
            store
                .cancel_enrollment("owner", "default", &cancelled.enrollment_id)
                .await
        );
        assert!(store
            .exchange_enrollment(
                &cancelled.enrollment_id,
                &cancelled.secret,
                "phone-1",
                "Phone",
                2_001,
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn pending_enrollment_memory_is_bounded_and_expiry_reclaims_capacity() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        for index in 0..MAX_PENDING_ENROLLMENTS {
            store
                .begin_enrollment("owner", format!("workspace-{index}"), 1_000)
                .await
                .unwrap();
        }
        assert_eq!(
            store.pending_enrollment_count().await,
            MAX_PENDING_ENROLLMENTS
        );
        assert!(matches!(
            store.begin_enrollment("owner", "overflow", 1_001).await,
            Err(PairingError::TooManyPendingEnrollments)
        ));
        assert!(store
            .begin_enrollment("owner", "after-expiry", 1_000 + DEVICE_ENROLLMENT_TTL_MS,)
            .await
            .is_ok());
        assert_eq!(store.pending_enrollment_count().await, 1);
    }

    #[tokio::test]
    async fn automation_targets_are_scope_bound_opaque_and_rotate_on_repair() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let key = DeviceKey::new("owner", "private", "raw-phone-serial");
        let first_token = store
            .pair_attested_automation(
                key.clone(),
                "Fixture phone",
                automation_identity("first"),
                1_000,
            )
            .await
            .unwrap();
        let first = store.list_automation_targets("owner", "private").await;
        assert_eq!(first.len(), 1);
        assert!(first[0].target_ref.starts_with("android-device:"));
        assert!(!first[0].target_ref.contains("raw-phone-serial"));
        assert!(!first[0].target_ref.contains(&first_token));
        assert!(store
            .resolve_automation_target("owner", "another", &first[0].target_ref)
            .await
            .is_err());
        assert_eq!(
            store
                .resolve_automation_target("owner", "private", &first[0].target_ref)
                .await
                .unwrap(),
            key
        );

        let second_token = store
            .pair_attested_automation(key, "Fixture phone", automation_identity("second"), 2_000)
            .await
            .unwrap();
        let second = store.list_automation_targets("owner", "private").await;
        assert_ne!(first_token, second_token);
        assert_ne!(first[0].target_ref, second[0].target_ref);
        assert!(store
            .resolve_automation_target("owner", "private", &first[0].target_ref)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn automation_review_is_exact_durable_revocable_and_generation_fenced() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let key = DeviceKey::new("owner", "private", "pixel-review");
        store
            .pair_attested_automation(
                key.clone(),
                "Review phone",
                automation_identity("review"),
                1_000,
            )
            .await
            .unwrap();
        let target = store.list_automation_targets("owner", "private").await;
        assert_eq!(target[0].review_generation, 1);

        let review = store
            .review_automation_actions(
                &key,
                1,
                vec![
                    "com.example.second".to_owned(),
                    "com.example.first".to_owned(),
                    "com.example.first".to_owned(),
                ],
                2_000,
            )
            .await
            .unwrap();
        assert_eq!(review.generation, 2);
        assert_eq!(
            review.actions.as_slice(),
            DEVICE_AUTOMATION_ACTION_ROSTER.as_slice()
        );
        assert_eq!(
            review.allowed_packages,
            vec![
                "com.example.first".to_owned(),
                "com.example.second".to_owned()
            ]
        );
        assert_eq!(
            store
                .resolve_automation_review(
                    "owner",
                    "private",
                    &review.target_ref,
                    review.generation,
                    &review.review_digest,
                )
                .await
                .unwrap(),
            key
        );
        assert!(matches!(
            store
                .review_automation_actions(&key, 1, vec!["com.example.stale".to_owned()], 2_001,)
                .await,
            Err(PairingError::AutomationReviewConflict)
        ));

        drop(store);
        let reopened = DevicePairingStore::open(temp.path()).await.unwrap();
        assert_eq!(
            reopened.active_automation_reviews("owner", "private").await,
            vec![review.clone()]
        );
        assert_eq!(reopened.revoke_automation_review(&key, 2).await.unwrap(), 3);
        assert!(reopened
            .resolve_automation_review(
                "owner",
                "private",
                &review.target_ref,
                review.generation,
                &review.review_digest,
            )
            .await
            .is_err());
        assert!(reopened
            .active_automation_reviews("owner", "private")
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn automation_review_rejects_bad_packages_and_rotates_with_pairing_key() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let key = DeviceKey::new("owner", "private", "pixel-rotation");
        store
            .pair_attested_automation(
                key.clone(),
                "Rotation phone",
                automation_identity("rotation-first"),
                1_000,
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .review_automation_actions(&key, 1, vec!["not a package".to_owned()], 2_000,)
                .await,
            Err(PairingError::InvalidAutomationReview)
        ));
        let review = store
            .review_automation_actions(&key, 1, vec!["com.example.fixture".to_owned()], 2_000)
            .await
            .unwrap();
        store
            .pair_attested_automation(
                key.clone(),
                "Rotation phone",
                automation_identity("rotation-second"),
                3_000,
            )
            .await
            .unwrap();
        let target = store.list_automation_targets("owner", "private").await;
        assert_eq!(target[0].review_generation, 3);
        assert!(target[0].review.is_none());
        assert_ne!(target[0].target_ref, review.target_ref);
        assert!(store
            .resolve_automation_review(
                "owner",
                "private",
                &review.target_ref,
                review.generation,
                &review.review_digest,
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn corrupted_automation_review_never_defaults_to_available() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let key = DeviceKey::new("owner", "private", "pixel-corrupt");
        store
            .pair_attested_automation(
                key.clone(),
                "Corrupt phone",
                automation_identity("corrupt"),
                1_000,
            )
            .await
            .unwrap();
        store
            .review_automation_actions(&key, 1, vec!["com.example.fixture".to_owned()], 2_000)
            .await
            .unwrap();
        drop(store);
        let path = temp.path().join("system/paired-devices.json");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            raw.replace("com.example.fixture", "com.example.substituted"),
        )
        .unwrap();
        assert!(matches!(
            DevicePairingStore::open(temp.path()).await,
            Err(PairingError::Corrupt(_))
        ));
    }

    #[tokio::test]
    async fn persisted_action_roster_subset_never_defaults_to_available() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(temp.path()).await.unwrap();
        let key = DeviceKey::new("owner", "private", "pixel-roster-corrupt");
        store
            .pair_attested_automation(
                key.clone(),
                "Roster corrupt phone",
                automation_identity("roster-corrupt"),
                1_000,
            )
            .await
            .unwrap();
        store
            .review_automation_actions(&key, 1, vec!["com.example.fixture".to_owned()], 2_000)
            .await
            .unwrap();
        drop(store);
        let path = temp.path().join("system/paired-devices.json");
        let raw = std::fs::read_to_string(&path).unwrap();
        let substituted = raw.replacen("\"screenshot\",", "", 1);
        assert_ne!(substituted, raw);
        std::fs::write(&path, substituted).unwrap();
        assert!(matches!(
            DevicePairingStore::open(temp.path()).await,
            Err(PairingError::Corrupt(_))
        ));
    }
}
