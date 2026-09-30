//! Server-authoritative Google Play Integrity verification for Android Apps.
//!
//! The handset-provided APK checksum is diagnostic only: an application can
//! report any value it can sign with its own Keystore key. Apps authority is
//! admitted only after Google decodes a standard token whose request hash is
//! bound to the exact one-time enrollment or socket proof.

use std::{
    collections::BTreeSet,
    sync::{Arc, OnceLock},
    time::Duration,
};

use base64::Engine as _;
use futures_util::StreamExt as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt as _;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const EXPECTED_PACKAGE: &str =
    magician_app_contract::android_owner::APP_ANDROID_OWNER_COMPANION_PACKAGE;
const PLAY_INTEGRITY_SCOPE: &str = "https://www.googleapis.com/auth/playintegrity";
const GOOGLE_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const MAX_SERVICE_ACCOUNT_BYTES: u64 = 64 * 1024;
const MAX_INTEGRITY_TOKEN_BYTES: usize = 32 * 1024;
const MAX_DECODE_RESPONSE_BYTES: usize = 128 * 1024;
const MAX_OAUTH_RESPONSE_BYTES: usize = 32 * 1024;
const MAX_VERDICT_AGE_MS: i64 = 2 * 60 * 1_000;
const MAX_FUTURE_SKEW_MS: i64 = 30 * 1_000;
const PROVIDER_DEADLINE: Duration = Duration::from_secs(12);
const MAX_CONCURRENT_PROVIDER_CALLS: usize = 2;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AndroidPlayIntegrityError {
    #[error("Play Integrity authority is not configured")]
    Unavailable,
    #[error("Play Integrity provider transport failed")]
    Transport,
    #[error("Play Integrity verdict is malformed")]
    Malformed,
    #[error("Play Integrity verdict does not match reviewed policy")]
    Untrusted,
}

#[derive(Clone)]
pub(crate) struct AndroidPlayIntegrityPolicy {
    pub(crate) cloud_project_number: u64,
    version_codes: BTreeSet<u64>,
    certificate_sha256: BTreeSet<String>,
    required_device_verdicts: BTreeSet<String>,
    service_account_path: String,
    digest: String,
}

impl AndroidPlayIntegrityPolicy {
    pub(crate) fn reviewed(
        cloud_project_number: Option<u64>,
        version_codes: &[u64],
        certificate_sha256: &[String],
        required_device_verdicts: &[String],
        service_account_path: Option<&str>,
    ) -> Result<Self, AndroidPlayIntegrityError> {
        let cloud_project_number = cloud_project_number
            .filter(|value| *value > 0)
            .ok_or(AndroidPlayIntegrityError::Unavailable)?;
        let version_codes = version_codes
            .iter()
            .copied()
            .filter(|value| *value > 0)
            .collect::<BTreeSet<_>>();
        let certificate_sha256 = certificate_sha256
            .iter()
            .map(|value| value.trim().to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let required_device_verdicts = required_device_verdicts
            .iter()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty() && value.len() <= 64)
            .collect::<BTreeSet<_>>();
        let service_account_path = service_account_path
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(AndroidPlayIntegrityError::Unavailable)?
            .to_owned();
        if version_codes.is_empty()
            || certificate_sha256.is_empty()
            || certificate_sha256.iter().any(|value| {
                value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || required_device_verdicts.is_empty()
        {
            return Err(AndroidPlayIntegrityError::Unavailable);
        }
        let digest = policy_digest(
            cloud_project_number,
            &version_codes,
            &certificate_sha256,
            &required_device_verdicts,
        );
        Ok(Self {
            cloud_project_number,
            version_codes,
            certificate_sha256,
            required_device_verdicts,
            service_account_path,
            digest,
        })
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
}

fn policy_digest(
    cloud_project_number: u64,
    versions: &BTreeSet<u64>,
    certificates: &BTreeSet<String>,
    device_verdicts: &BTreeSet<String>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.android-play-integrity-policy.v1\0");
    let cloud_project_number = cloud_project_number.to_le_bytes();
    for bytes in [
        EXPECTED_PACKAGE.as_bytes(),
        cloud_project_number.as_slice(),
        b"PLAY_RECOGNIZED",
    ] {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    for version in versions {
        hasher.update(&version.to_le_bytes());
    }
    for values in [certificates, device_verdicts] {
        for value in values {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// RequestHash is a compact, domain-separated correlation over material that
/// the hardware-backed Apps key already signed. It contains no bearer secret.
pub(crate) fn play_integrity_request_hash(
    domain: &[u8],
    signed_material: &[u8],
    signature: &[u8],
) -> String {
    let mut hasher = Sha256::new();
    for value in [domain, signed_material, signature] {
        hasher.update((value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct ServiceAccount {
    project_id: String,
    client_email: String,
    private_key: String,
    #[serde(default)]
    token_uri: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
struct CachedBearer {
    account_path: String,
    value: String,
    expires_at_s: i64,
}

fn bearer_cache() -> &'static tokio::sync::Mutex<Option<CachedBearer>> {
    static CACHE: OnceLock<tokio::sync::Mutex<Option<CachedBearer>>> = OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(None))
}

fn provider_capacity() -> &'static Arc<tokio::sync::Semaphore> {
    static CAPACITY: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    CAPACITY.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PROVIDER_CALLS)))
}

async fn service_account(path: &str) -> Result<ServiceAccount, AndroidPlayIntegrityError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .await
        .map_err(|_| AndroidPlayIntegrityError::Unavailable)?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| AndroidPlayIntegrityError::Unavailable)?;
    if !metadata.is_file() || metadata.len() > MAX_SERVICE_ACCOUNT_BYTES {
        return Err(AndroidPlayIntegrityError::Unavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(AndroidPlayIntegrityError::Unavailable);
        }
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
    file.take(MAX_SERVICE_ACCOUNT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| AndroidPlayIntegrityError::Unavailable)?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > MAX_SERVICE_ACCOUNT_BYTES {
        return Err(AndroidPlayIntegrityError::Unavailable);
    }
    let account: ServiceAccount = serde_json::from_slice(bytes.as_slice())
        .map_err(|_| AndroidPlayIntegrityError::Unavailable)?;
    if account.project_id.trim().is_empty()
        || account.client_email.trim().is_empty()
        || account.private_key.trim().is_empty()
        || account
            .token_uri
            .as_deref()
            .unwrap_or(GOOGLE_OAUTH_TOKEN_URL)
            != GOOGLE_OAUTH_TOKEN_URL
        || account
            .r#type
            .as_deref()
            .is_some_and(|value| value != "service_account")
    {
        return Err(AndroidPlayIntegrityError::Unavailable);
    }
    Ok(account)
}

async fn google_bearer(
    policy: &AndroidPlayIntegrityPolicy,
) -> Result<Zeroizing<String>, AndroidPlayIntegrityError> {
    let now_s = chrono::Utc::now().timestamp();
    let mut cache = bearer_cache().lock().await;
    if let Some(cached) = cache.as_ref().filter(|cached| {
        cached.account_path == policy.service_account_path && cached.expires_at_s > now_s + 60
    }) {
        return Ok(Zeroizing::new(cached.value.clone()));
    }
    let account = service_account(&policy.service_account_path).await?;
    #[derive(Serialize)]
    struct Claims<'a> {
        iss: &'a str,
        scope: &'a str,
        aud: &'a str,
        iat: i64,
        exp: i64,
    }
    let assertion = Zeroizing::new(
        jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &Claims {
                iss: &account.client_email,
                scope: PLAY_INTEGRITY_SCOPE,
                aud: GOOGLE_OAUTH_TOKEN_URL,
                iat: now_s,
                exp: now_s + 3_600,
            },
            &EncodingKey::from_rsa_pem(account.private_key.as_bytes())
                .map_err(|_| AndroidPlayIntegrityError::Unavailable)?,
        )
        .map_err(|_| AndroidPlayIntegrityError::Unavailable)?,
    );
    #[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
    struct TokenResponse {
        access_token: String,
        expires_in: i64,
    }
    let client = reqwest::Client::builder()
        .timeout(PROVIDER_DEADLINE)
        .build()
        .map_err(|_| AndroidPlayIntegrityError::Unavailable)?;
    let response = client
        .post(GOOGLE_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", assertion.as_str()),
        ])
        .send()
        .await
        .map_err(|_| AndroidPlayIntegrityError::Transport)?
        .error_for_status()
        .map_err(|_| AndroidPlayIntegrityError::Transport)?;
    let response_bytes =
        Zeroizing::new(bounded_response_bytes(response, MAX_OAUTH_RESPONSE_BYTES).await?);
    let response = serde_json::from_slice::<TokenResponse>(&response_bytes)
        .map_err(|_| AndroidPlayIntegrityError::Malformed)?;
    if response.access_token.is_empty() || response.access_token.len() > 16 * 1024 {
        return Err(AndroidPlayIntegrityError::Malformed);
    }
    let cached = CachedBearer {
        account_path: policy.service_account_path.clone(),
        value: response.access_token.clone(),
        expires_at_s: now_s.saturating_add(response.expires_in.max(60)),
    };
    *cache = Some(cached);
    Ok(Zeroizing::new(response.access_token.clone()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DecodeResponse {
    token_payload_external: TokenPayload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenPayload {
    request_details: RequestDetails,
    app_integrity: AppIntegrity,
    device_integrity: DeviceIntegrity,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestDetails {
    request_package_name: String,
    request_hash: String,
    timestamp_millis: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppIntegrity {
    app_recognition_verdict: String,
    package_name: String,
    certificate_sha256_digest: Vec<String>,
    version_code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceIntegrity {
    device_recognition_verdict: Vec<String>,
}

pub(crate) async fn verify_play_integrity_token(
    policy: &AndroidPlayIntegrityPolicy,
    token: &str,
    expected_request_hash: &str,
    now_ms: i64,
) -> Result<String, AndroidPlayIntegrityError> {
    if token.is_empty()
        || token.len() > MAX_INTEGRITY_TOKEN_BYTES
        || expected_request_hash.len() != 43
    {
        return Err(AndroidPlayIntegrityError::Malformed);
    }
    let _capacity = tokio::time::timeout(
        PROVIDER_DEADLINE,
        Arc::clone(provider_capacity()).acquire_owned(),
    )
    .await
    .map_err(|_| AndroidPlayIntegrityError::Transport)?
    .map_err(|_| AndroidPlayIntegrityError::Unavailable)?;
    let bearer = google_bearer(policy).await?;
    let client = reqwest::Client::builder()
        .timeout(PROVIDER_DEADLINE)
        .build()
        .map_err(|_| AndroidPlayIntegrityError::Unavailable)?;
    let url =
        format!("https://playintegrity.googleapis.com/v1/{EXPECTED_PACKAGE}:decodeIntegrityToken");
    let response = client
        .post(url)
        .bearer_auth(bearer.as_str())
        .json(&serde_json::json!({"integrity_token": token}))
        .send()
        .await
        .map_err(|_| AndroidPlayIntegrityError::Transport)?
        .error_for_status()
        .map_err(|_| AndroidPlayIntegrityError::Untrusted)?;
    let bytes = bounded_response_bytes(response, MAX_DECODE_RESPONSE_BYTES).await?;
    let decoded: DecodeResponse =
        serde_json::from_slice(&bytes).map_err(|_| AndroidPlayIntegrityError::Malformed)?;
    let timestamp_ms = validate_token_payload(
        policy,
        decoded.token_payload_external,
        expected_request_hash,
        now_ms,
    )?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.android-play-integrity-verdict.v1\0");
    for value in [
        expected_request_hash,
        policy.digest(),
        &timestamp_ms.to_string(),
    ] {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

/// Closed predicate over the Google-decoded payload. Keeping this separate
/// makes every release-identity field directly regression-testable without a
/// network or a client-decoded verdict fixture.
fn validate_token_payload(
    policy: &AndroidPlayIntegrityPolicy,
    payload: TokenPayload,
    expected_request_hash: &str,
    now_ms: i64,
) -> Result<i64, AndroidPlayIntegrityError> {
    let details = payload.request_details;
    let app = payload.app_integrity;
    let device = payload.device_integrity;
    let timestamp_ms = details
        .timestamp_millis
        .parse::<i64>()
        .map_err(|_| AndroidPlayIntegrityError::Malformed)?;
    let version_code = app
        .version_code
        .parse::<u64>()
        .map_err(|_| AndroidPlayIntegrityError::Malformed)?;
    let certificate_sha256 = app
        .certificate_sha256_digest
        .iter()
        .map(|value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(value)
                .or_else(|_| base64::engine::general_purpose::STANDARD.decode(value))
                .ok()
                .filter(|bytes| bytes.len() == 32)
                .map(hex::encode)
        })
        .collect::<Option<BTreeSet<_>>>()
        .ok_or(AndroidPlayIntegrityError::Malformed)?;
    let device_verdicts = device
        .device_recognition_verdict
        .into_iter()
        .collect::<BTreeSet<_>>();
    if details.request_package_name != EXPECTED_PACKAGE
        || details.request_hash != expected_request_hash
        || timestamp_ms > now_ms.saturating_add(MAX_FUTURE_SKEW_MS)
        || now_ms.saturating_sub(timestamp_ms) > MAX_VERDICT_AGE_MS
        || app.app_recognition_verdict != "PLAY_RECOGNIZED"
        || app.package_name != EXPECTED_PACKAGE
        || !policy.version_codes.contains(&version_code)
        || certificate_sha256 != policy.certificate_sha256
        || !policy.required_device_verdicts.is_subset(&device_verdicts)
    {
        return Err(AndroidPlayIntegrityError::Untrusted);
    }
    Ok(timestamp_ms)
}

async fn bounded_response_bytes(
    response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, AndroidPlayIntegrityError> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(AndroidPlayIntegrityError::Malformed);
    }
    let mut bytes =
        Vec::with_capacity(response.content_length().unwrap_or(0).min(maximum as u64) as usize);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AndroidPlayIntegrityError::Transport)?;
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(AndroidPlayIntegrityError::Malformed);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reviewed_policy() -> AndroidPlayIntegrityPolicy {
        AndroidPlayIntegrityPolicy::reviewed(
            Some(123),
            &[11],
            &[hex::encode([0xabu8; 32])],
            &["MEETS_DEVICE_INTEGRITY".to_owned()],
            Some("/private/reviewed-service-account.json"),
        )
        .unwrap()
    }

    fn decoded_payload(timestamp_ms: i64, request_hash: &str) -> TokenPayload {
        TokenPayload {
            request_details: RequestDetails {
                request_package_name: EXPECTED_PACKAGE.to_owned(),
                request_hash: request_hash.to_owned(),
                timestamp_millis: timestamp_ms.to_string(),
            },
            app_integrity: AppIntegrity {
                app_recognition_verdict: "PLAY_RECOGNIZED".to_owned(),
                package_name: EXPECTED_PACKAGE.to_owned(),
                certificate_sha256_digest: vec![
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0xabu8; 32])
                ],
                version_code: "11".to_owned(),
            },
            device_integrity: DeviceIntegrity {
                device_recognition_verdict: vec!["MEETS_DEVICE_INTEGRITY".to_owned()],
            },
        }
    }

    #[test]
    fn policy_is_unavailable_without_every_reviewed_play_boundary() {
        assert!(AndroidPlayIntegrityPolicy::reviewed(None, &[], &[], &[], None).is_err());
        assert!(AndroidPlayIntegrityPolicy::reviewed(
            Some(1),
            &[11],
            &["a".repeat(64)],
            &["MEETS_DEVICE_INTEGRITY".to_owned()],
            None,
        )
        .is_err());
    }

    #[test]
    fn request_hash_is_domain_and_signature_sensitive() {
        let first = play_integrity_request_hash(b"enroll", b"material", b"signature-a");
        assert_ne!(
            first,
            play_integrity_request_hash(b"socket", b"material", b"signature-a")
        );
        assert_ne!(
            first,
            play_integrity_request_hash(b"enroll", b"material", b"signature-b")
        );
    }

    #[test]
    fn decoded_verdict_requires_every_exact_release_and_freshness_field() {
        let policy = reviewed_policy();
        let now_ms = 1_800_000_000_000i64;
        let request_hash = "A".repeat(43);
        assert_eq!(
            validate_token_payload(
                &policy,
                decoded_payload(now_ms, &request_hash),
                &request_hash,
                now_ms,
            )
            .unwrap(),
            now_ms,
        );

        let mut substituted = decoded_payload(now_ms, &request_hash);
        substituted.request_details.request_hash = "B".repeat(43);
        assert!(validate_token_payload(&policy, substituted, &request_hash, now_ms).is_err());

        let mut substituted = decoded_payload(now_ms, &request_hash);
        substituted.request_details.request_package_name = "com.example.other".to_owned();
        assert!(validate_token_payload(&policy, substituted, &request_hash, now_ms).is_err());

        let mut substituted = decoded_payload(now_ms, &request_hash);
        substituted.app_integrity.app_recognition_verdict = "UNRECOGNIZED_VERSION".to_owned();
        assert!(validate_token_payload(&policy, substituted, &request_hash, now_ms).is_err());

        let mut substituted = decoded_payload(now_ms, &request_hash);
        substituted.app_integrity.package_name = "com.example.other".to_owned();
        assert!(validate_token_payload(&policy, substituted, &request_hash, now_ms).is_err());

        let mut substituted = decoded_payload(now_ms, &request_hash);
        substituted.app_integrity.version_code = "12".to_owned();
        assert!(validate_token_payload(&policy, substituted, &request_hash, now_ms).is_err());

        let mut substituted = decoded_payload(now_ms, &request_hash);
        substituted.app_integrity.certificate_sha256_digest.clear();
        assert!(validate_token_payload(&policy, substituted, &request_hash, now_ms).is_err());

        let mut substituted = decoded_payload(now_ms, &request_hash);
        substituted
            .device_integrity
            .device_recognition_verdict
            .clear();
        assert!(validate_token_payload(&policy, substituted, &request_hash, now_ms).is_err());

        assert!(validate_token_payload(
            &policy,
            decoded_payload(now_ms - MAX_VERDICT_AGE_MS - 1, &request_hash),
            &request_hash,
            now_ms,
        )
        .is_err());
        assert!(validate_token_payload(
            &policy,
            decoded_payload(now_ms + MAX_FUTURE_SKEW_MS + 1, &request_hash),
            &request_hash,
            now_ms,
        )
        .is_err());
    }
}
