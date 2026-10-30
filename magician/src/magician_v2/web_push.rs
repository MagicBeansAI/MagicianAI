//! Standard browser Web Push (RFC 8030 delivery, RFC 8291 `aes128gcm`
//! message encryption, RFC 8292 VAPID) for the registrations
//! `mobile_push` keeps under platform `web`.
//!
//! Everything here is pure: the VAPID identity, the encrypted body, the
//! request headers and the content-free payload. Sending rides the mobile
//! push dispatcher's HTTP client, so a browser registration is delivered,
//! counted and pruned (404/410) exactly like a phone's.
//!
//! The crypto is `ring` (already the runtime's): P-256 ECDH for the per-message
//! key, HMAC-SHA-256 HKDF, AES-128-GCM, and ES256 for the VAPID JWT.
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ring::{
    aead, agreement, hmac,
    rand::{SecureRandom, SystemRandom},
    signature::{self, EcdsaKeyPair, KeyPair},
};
use serde_json::{json, Value};

/// Runtime env: the VAPID public key, base64url (no padding) of the 65-byte
/// uncompressed P-256 point — what the browser passes to `subscribe` as
/// `applicationServerKey`.
pub const VAPID_PUBLIC_KEY_ENV: &str = "MAGICIAN_WEBPUSH_VAPID_PUBLIC_KEY";
/// Runtime env: the VAPID private key, base64url (no padding) of the raw
/// 32-byte P-256 scalar. Secret.
pub const VAPID_PRIVATE_KEY_ENV: &str = "MAGICIAN_WEBPUSH_VAPID_PRIVATE_KEY";
/// Runtime env: the VAPID `sub` claim, a `mailto:` or `https:` URL the push
/// services can contact.
pub const VAPID_SUBJECT_ENV: &str = "MAGICIAN_WEBPUSH_SUBJECT";

/// One record of at most this many bytes (RFC 8188 `rs`). The payloads are a
/// few hundred bytes; a single record keeps the body one AEAD seal.
const RECORD_SIZE: u32 = 4096;
/// The plaintext a single record can carry: `rs` − 16 (tag) − 1 (delimiter).
const MAX_PLAINTEXT: usize = RECORD_SIZE as usize - 17;
/// How long a VAPID JWT is valid. RFC 8292 caps it at 24 hours.
const VAPID_JWT_LIFETIME_SECS: i64 = 12 * 60 * 60;
const MAX_ENDPOINT_BYTES: usize = 2_048;

/// Push services the runtime will POST to. A subscription endpoint is a URL
/// the browser received from its vendor's push service; it is never a free
/// URL, so anything else is refused rather than fetched.
const PUSH_SERVICE_HOST_SUFFIXES: &[&str] = &[
    // Chrome, Edge (Chromium), Brave, Opera.
    "fcm.googleapis.com",
    "android.googleapis.com",
    // Firefox.
    "push.services.mozilla.com",
    // Legacy Edge / Windows.
    "notify.windows.com",
    // Safari (macOS 13+, iOS 16.4+ home-screen apps).
    "push.apple.com",
];

/// The server's VAPID identity (RFC 8292).
pub struct WebPushVapid {
    public_key_b64: String,
    key_pair: EcdsaKeyPair,
    subject: String,
}

impl std::fmt::Debug for WebPushVapid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebPushVapid")
            .field("public_key", &self.public_key_b64)
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

/// A fresh VAPID pair for one deployment, as the env values
/// [`WebPushVapid::from_env`] reads: base64url (no padding) of the 65-byte
/// uncompressed public point and of the raw 32-byte private scalar. The pair
/// is proved by building a [`WebPushVapid`] from it before it is returned.
pub fn generate_vapid_key_pair() -> Result<(String, String)> {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map_err(|_| anyhow!("could not generate a P-256 key"))?;
    let pair = EcdsaKeyPair::from_pkcs8(
        &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        pkcs8.as_ref(),
        &rng,
    )
    .map_err(|_| anyhow!("could not read the generated P-256 key"))?;
    let public_key = pair.public_key().as_ref().to_vec();
    // PKCS#8 wraps an RFC 5915 ECPrivateKey whose privateKey is the first
    // 32-byte OCTET STRING (`04 20`); the algorithm OIDs before it contain no
    // such run. Building the VAPID signer below rejects any wrong slice.
    let der = pkcs8.as_ref();
    let scalar = der
        .windows(2)
        .position(|window| window == [0x04, 0x20])
        .and_then(|at| der.get(at + 2..at + 34))
        .ok_or_else(|| anyhow!("the generated key has no private scalar"))?;
    let public_b64 = URL_SAFE_NO_PAD.encode(&public_key);
    let private_b64 = URL_SAFE_NO_PAD.encode(scalar);
    WebPushVapid::new(&public_b64, &private_b64, "https://magican.ai")
        .context("the generated VAPID pair does not load")?;
    Ok((public_b64, private_b64))
}

impl WebPushVapid {
    /// From the three env values. The private key never appears in an error.
    pub fn new(public_key_b64: &str, private_key_b64: &str, subject: &str) -> Result<Self> {
        let public_key =
            decode_b64url(public_key_b64.trim()).context("VAPID public key is not base64url")?;
        if public_key.len() != 65 || public_key[0] != 0x04 {
            bail!("VAPID public key must be a 65-byte uncompressed P-256 point");
        }
        let private_key = decode_b64url(private_key_b64.trim())
            .map_err(|_| anyhow!("VAPID private key is not base64url"))?;
        if private_key.len() != 32 {
            bail!("VAPID private key must be a raw 32-byte P-256 scalar");
        }
        let key_pair = EcdsaKeyPair::from_private_key_and_public_key(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &private_key,
            &public_key,
            &SystemRandom::new(),
        )
        .map_err(|_| anyhow!("VAPID private and public keys do not form a P-256 key pair"))?;
        let subject = subject.trim();
        if !(subject.starts_with("mailto:") || subject.starts_with("https://")) {
            bail!("VAPID subject must be a mailto: or https: URL");
        }
        Ok(Self {
            public_key_b64: URL_SAFE_NO_PAD.encode(&public_key),
            key_pair,
            subject: subject.to_string(),
        })
    }

    /// From the runtime env; `None` (with a reason) when either key is
    /// missing or unusable — web push is then off.
    pub fn from_env() -> std::result::Result<Option<Self>, String> {
        let value = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let (Some(public_key), Some(private_key)) =
            (value(VAPID_PUBLIC_KEY_ENV), value(VAPID_PRIVATE_KEY_ENV))
        else {
            return Ok(None);
        };
        let subject = value(VAPID_SUBJECT_ENV).unwrap_or_else(|| "https://magican.ai".to_string());
        Self::new(&public_key, &private_key, &subject)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    /// `applicationServerKey` for the browser.
    pub fn public_key_b64(&self) -> &str {
        &self.public_key_b64
    }

    /// `Authorization: vapid t=<jwt>, k=<public key>` for one endpoint.
    pub fn authorization(&self, endpoint: &str, now_secs: i64) -> Result<String> {
        let audience = reqwest::Url::parse(endpoint)
            .context("push endpoint is not a URL")?
            .origin()
            .ascii_serialization();
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "aud": audience,
                "exp": now_secs + VAPID_JWT_LIFETIME_SECS,
                "sub": self.subject,
            }))
            .context("VAPID claims")?,
        );
        let signing_input = format!("{header}.{claims}");
        let signature = self
            .key_pair
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|_| anyhow!("VAPID signing failed"))?;
        Ok(format!(
            "vapid t={signing_input}.{}, k={}",
            URL_SAFE_NO_PAD.encode(signature.as_ref()),
            self.public_key_b64
        ))
    }

    #[cfg(test)]
    fn verify(&self, message: &[u8], signature_bytes: &[u8]) -> bool {
        let public_key = decode_b64url(&self.public_key_b64).expect("own public key decodes");
        signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, &public_key)
            .verify(message, signature_bytes)
            .is_ok()
    }
}

/// A browser `PushSubscription`'s keys, as stored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebPushKeys {
    /// base64url of the 65-byte uncompressed P-256 point.
    pub p256dh: String,
    /// base64url of the 16-byte authentication secret.
    pub auth: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WebPushSubscriptionError {
    #[error("push endpoint must be an https URL of a browser push service")]
    Endpoint,
    #[error("subscription keys are invalid")]
    Keys,
}

/// Validate a browser subscription; returns the keys normalized to
/// unpadded base64url.
pub fn validate_subscription(
    endpoint: &str,
    keys: &WebPushKeys,
) -> std::result::Result<WebPushKeys, WebPushSubscriptionError> {
    validate_endpoint(endpoint)?;
    let p256dh = decode_b64url(keys.p256dh.trim()).map_err(|_| WebPushSubscriptionError::Keys)?;
    let auth = decode_b64url(keys.auth.trim()).map_err(|_| WebPushSubscriptionError::Keys)?;
    if p256dh.len() != 65 || p256dh[0] != 0x04 || auth.len() != 16 {
        return Err(WebPushSubscriptionError::Keys);
    }
    Ok(WebPushKeys {
        p256dh: URL_SAFE_NO_PAD.encode(p256dh),
        auth: URL_SAFE_NO_PAD.encode(auth),
    })
}

/// An endpoint is an https URL on a known push service, without
/// credentials, of bounded length.
pub fn validate_endpoint(endpoint: &str) -> std::result::Result<(), WebPushSubscriptionError> {
    if endpoint.is_empty()
        || endpoint.len() > MAX_ENDPOINT_BYTES
        || endpoint
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(WebPushSubscriptionError::Endpoint);
    }
    let url = reqwest::Url::parse(endpoint).map_err(|_| WebPushSubscriptionError::Endpoint)?;
    let host = url
        .host_str()
        .ok_or(WebPushSubscriptionError::Endpoint)?
        .to_ascii_lowercase();
    let known = PUSH_SERVICE_HOST_SUFFIXES
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")));
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || !known
    {
        return Err(WebPushSubscriptionError::Endpoint);
    }
    Ok(())
}

/// The content-free JSON a browser receives, from the dispatcher's push data.
/// `None` for a kind browsers are not sent (task progress and the like).
pub fn web_push_payload(data: &Value) -> Option<Value> {
    let kind = data.get("kind").and_then(Value::as_str)?;
    let correlation_id = data.get("correlation_id").and_then(Value::as_str)?;
    match kind {
        "attention_requested" => {
            let mut payload = json!({
                "kind": kind,
                "correlation_id": correlation_id,
                "source": data.get("source").cloned().unwrap_or(Value::Null),
                "expires_at": data.get("expires_at").cloned().unwrap_or(Value::Null),
                "web_path": attention_web_path(correlation_id),
            });
            for key in ["answer_mode", "option_count"] {
                if let Some(value) = data.get(key) {
                    payload[key] = value.clone();
                }
            }
            Some(payload)
        },
        "attention_resolved" | "attention_expired" => {
            Some(json!({"kind": kind, "correlation_id": correlation_id}))
        },
        _ => None,
    }
}

/// Which asks each browser subscription was actually shown, so a withdrawal
/// (`attention_resolved` / `attention_expired`) goes only to a subscription
/// that has a notification to close. A browser must show a notification for
/// every push it receives (Chrome penalises silent pushes), so a withdrawal
/// of an ask it never got would be a silent push. In memory and bounded: a
/// restart only forgets to close, it never sends a silent push.
#[derive(Debug)]
pub struct AnnouncedAsks {
    entries: std::collections::HashMap<(String, String), u64>,
    next: u64,
    capacity: usize,
}

impl AnnouncedAsks {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            next: 0,
            capacity: capacity.max(1),
        }
    }

    /// `registration` was sent the ask `correlation_id`.
    pub fn record(&mut self, registration: &str, correlation_id: &str) {
        let key = (registration.to_string(), correlation_id.to_string());
        if !self.entries.contains_key(&key) && self.entries.len() >= self.capacity {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, order)| **order)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.next += 1;
        self.entries.insert(key, self.next);
    }

    pub fn contains(&self, registration: &str, correlation_id: &str) -> bool {
        self.entries
            .contains_key(&(registration.to_string(), correlation_id.to_string()))
    }

    /// The withdrawal went out: nothing is left to close.
    pub fn forget(&mut self, registration: &str, correlation_id: &str) {
        self.entries
            .remove(&(registration.to_string(), correlation_id.to_string()));
    }
}

/// The web app's route to one ask.
pub fn attention_web_path(correlation_id: &str) -> String {
    format!(
        "/attention?attention_item={}",
        urlencoding::encode(correlation_id)
    )
}

/// The RFC 8030 headers beside the body, minus `Authorization`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebPushHeaders {
    /// Seconds the push service keeps an undelivered message.
    pub ttl: u64,
    /// `high` for an ask, `normal` for its withdrawal.
    pub urgency: &'static str,
    /// One per ask, so a later message (a newer announcement, the resolution)
    /// replaces an undelivered earlier one.
    pub topic: Option<String>,
}

/// Headers for one payload: `TTL` from `expires_at` (never negative), else
/// `fallback_ttl_secs` (a withdrawal outlives nothing it withdraws).
pub fn web_push_headers(payload: &Value, now_ms: i64, fallback_ttl_secs: u64) -> WebPushHeaders {
    let requested = payload.get("kind").and_then(Value::as_str) == Some("attention_requested");
    let ttl = payload
        .get("expires_at")
        .and_then(Value::as_i64)
        .map(|expires_at| (expires_at.saturating_sub(now_ms) / 1000).max(0) as u64)
        .unwrap_or(fallback_ttl_secs);
    WebPushHeaders {
        ttl,
        urgency: if requested { "high" } else { "normal" },
        topic: payload
            .get("correlation_id")
            .and_then(Value::as_str)
            .map(push_topic),
    }
}

/// RFC 8030 `Topic`: at most 32 characters of the URL-safe base64 alphabet.
/// A digest, because correlation ids are unbounded and carry `:`/`~`.
pub fn push_topic(correlation_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(correlation_id.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..24])
}

/// Encrypt `plaintext` for one subscription (RFC 8291, `aes128gcm`): a fresh
/// P-256 key and salt per message, one record.
pub fn encrypt(keys: &WebPushKeys, plaintext: &[u8]) -> Result<Vec<u8>> {
    let ua_public = decode_b64url(&keys.p256dh).context("subscription p256dh")?;
    let auth = decode_b64url(&keys.auth).context("subscription auth")?;
    let rng = SystemRandom::new();
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| anyhow!("salt"))?;
    let ephemeral = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
        .map_err(|_| anyhow!("ephemeral key"))?;
    let as_public = ephemeral
        .compute_public_key()
        .map_err(|_| anyhow!("ephemeral public key"))?;
    let ecdh_secret = agreement::agree_ephemeral(
        ephemeral,
        &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &ua_public),
        |secret| secret.to_vec(),
    )
    .map_err(|_| anyhow!("subscription p256dh is not a P-256 point"))?;
    encrypt_with_secret(
        &ecdh_secret,
        &ua_public,
        as_public.as_ref(),
        &auth,
        &salt,
        plaintext,
    )
}

/// The deterministic half of [`encrypt`]: everything after the ECDH.
fn encrypt_with_secret(
    ecdh_secret: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    auth: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    if plaintext.len() > MAX_PLAINTEXT {
        bail!("web push payload exceeds one record");
    }
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let ikm = hkdf_sha256(auth, ecdh_secret, &key_info, 32);
    let cek = hkdf_sha256(salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = hkdf_sha256(salt, &ikm, b"Content-Encoding: nonce\0", 12);

    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, &cek).map_err(|_| anyhow!("content key"))?,
    );
    let mut record = Vec::with_capacity(plaintext.len() + 1 + aead::AES_128_GCM.tag_len());
    record.extend_from_slice(plaintext);
    // The last (and only) record's padding delimiter.
    record.push(0x02);
    key.seal_in_place_append_tag(
        aead::Nonce::try_assume_unique_for_key(&nonce).map_err(|_| anyhow!("nonce"))?,
        aead::Aad::empty(),
        &mut record,
    )
    .map_err(|_| anyhow!("seal"))?;

    let mut body = Vec::with_capacity(16 + 4 + 1 + as_public.len() + record.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&record);
    Ok(body)
}

/// HKDF-SHA-256 for outputs of at most one hash block (all RFC 8291 needs).
fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    debug_assert!(len <= 32);
    let prk = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, salt), ikm);
    let mut input = info.to_vec();
    input.push(0x01);
    let okm = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, prk.as_ref()), &input);
    okm.as_ref()[..len].to_vec()
}

/// Browsers hand keys out padded or unpadded, standard or URL-safe.
fn decode_b64url(value: &str) -> std::result::Result<Vec<u8>, base64::DecodeError> {
    let normalized = value
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    URL_SAFE_NO_PAD.decode(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_vapid_pairs_load_and_differ() {
        let (public_b64, private_b64) = generate_vapid_key_pair().expect("generate");
        let public = decode_b64url(&public_b64).expect("public base64url");
        assert_eq!(public.len(), 65);
        assert_eq!(public[0], 0x04);
        assert_eq!(
            decode_b64url(&private_b64)
                .expect("private base64url")
                .len(),
            32
        );
        let vapid = WebPushVapid::new(&public_b64, &private_b64, "mailto:owner@example.com")
            .expect("the pair loads");
        assert_eq!(vapid.public_key_b64(), public_b64);
        assert!(vapid
            .authorization("https://push.example.com/send/1", 1_700_000_000)
            .is_ok());
        let (other_public, _) = generate_vapid_key_pair().expect("generate again");
        assert_ne!(other_public, public_b64);
    }

    fn d(value: &str) -> Vec<u8> {
        decode_b64url(value).unwrap()
    }

    /// RFC 8291 Appendix A, end to end from its ECDH secret.
    #[test]
    fn encryption_matches_the_rfc_8291_example() {
        let plaintext = d("V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24");
        let as_public = d("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let ua_public = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let auth = d("BTBZMqHH6r4Tts7J_aSIgg");
        let ecdh_secret = d("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");
        let body = encrypt_with_secret(
            &ecdh_secret,
            &ua_public,
            &as_public,
            &auth,
            &salt,
            &plaintext,
        )
        .unwrap();
        assert_eq!(
            URL_SAFE_NO_PAD.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    /// A message to a fresh browser key decrypts with that key.
    #[test]
    fn a_message_decrypts_with_the_subscription_key() {
        let rng = SystemRandom::new();
        let browser =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let browser_public = browser.compute_public_key().unwrap();
        let mut auth = [0u8; 16];
        rng.fill(&mut auth).unwrap();
        let keys = WebPushKeys {
            p256dh: URL_SAFE_NO_PAD.encode(browser_public.as_ref()),
            auth: URL_SAFE_NO_PAD.encode(auth),
        };
        let body = encrypt(&keys, br#"{"kind":"attention_resolved"}"#).unwrap();
        let salt: [u8; 16] = body[..16].try_into().unwrap();
        assert_eq!(&body[16..20], &4096u32.to_be_bytes());
        assert_eq!(body[20], 65);
        let as_public = &body[21..86];
        let ecdh = agreement::agree_ephemeral(
            browser,
            &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public),
            |secret| secret.to_vec(),
        )
        .unwrap();
        let mut key_info = b"WebPush: info\0".to_vec();
        key_info.extend_from_slice(browser_public.as_ref());
        key_info.extend_from_slice(as_public);
        let ikm = hkdf_sha256(&auth, &ecdh, &key_info, 32);
        let cek = hkdf_sha256(&salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
        let nonce = hkdf_sha256(&salt, &ikm, b"Content-Encoding: nonce\0", 12);
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
        let mut record = body[86..].to_vec();
        let plain = key
            .open_in_place(
                aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(),
                aead::Aad::empty(),
                &mut record,
            )
            .unwrap();
        assert_eq!(plain, b"{\"kind\":\"attention_resolved\"}\x02");
    }

    // A key pair made for this test only.
    const TEST_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const TEST_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";

    #[test]
    fn vapid_authorization_is_an_es256_jwt_for_the_push_service_origin() {
        let vapid =
            WebPushVapid::new(TEST_PUBLIC, TEST_PRIVATE, "mailto:owner@example.test").unwrap();
        assert_eq!(vapid.public_key_b64(), TEST_PUBLIC);
        let header = vapid
            .authorization("https://fcm.googleapis.com/fcm/send/abc:def", 1_000)
            .unwrap();
        let rest = header.strip_prefix("vapid t=").unwrap();
        let (jwt, key) = rest.split_once(", k=").unwrap();
        assert_eq!(key, TEST_PUBLIC);
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: Value = serde_json::from_slice(&d(parts[1])).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["exp"], 1_000 + VAPID_JWT_LIFETIME_SECS);
        assert_eq!(claims["sub"], "mailto:owner@example.test");
        let jose: Value = serde_json::from_slice(&d(parts[0])).unwrap();
        assert_eq!(jose["alg"], "ES256");
        assert!(vapid.verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &d(parts[2])
        ));
        // A mismatched pair, a bad subject and a truncated key are refused,
        // and no error repeats the private key.
        let other_private = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
        let error = WebPushVapid::new(TEST_PUBLIC, other_private, "mailto:a@b.test")
            .unwrap_err()
            .to_string();
        assert!(!error.contains(other_private));
        assert!(WebPushVapid::new(TEST_PUBLIC, TEST_PRIVATE, "owner@example.test").is_err());
        assert!(WebPushVapid::new(&TEST_PUBLIC[..40], TEST_PRIVATE, "mailto:a@b.test").is_err());
    }

    #[test]
    fn subscriptions_must_name_a_browser_push_service_and_real_keys() {
        let keys = WebPushKeys {
            p256dh: TEST_PUBLIC.to_string(),
            auth: "BTBZMqHH6r4Tts7J_aSIgg".to_string(),
        };
        for endpoint in [
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://updates.push.services.mozilla.com/wpush/v2/abc",
            "https://web.push.apple.com/QGk",
            "https://wns2-par02p.notify.windows.com/w/?token=x",
        ] {
            assert!(validate_subscription(endpoint, &keys).is_ok(), "{endpoint}");
        }
        for endpoint in [
            "http://fcm.googleapis.com/fcm/send/abc",
            "https://localhost/push",
            "https://169.254.169.254/latest",
            "https://evilfcm.googleapis.com.example.com/x",
            "https://user:pw@fcm.googleapis.com/x",
            "https://fcm.googleapis.com:8443/x",
            "https://fcm.googleapis.com/a b",
            "",
        ] {
            assert_eq!(
                validate_subscription(endpoint, &keys),
                Err(WebPushSubscriptionError::Endpoint),
                "{endpoint}"
            );
        }
        // Padded standard base64 is normalized.
        let padded = WebPushKeys {
            p256dh: base64::engine::general_purpose::STANDARD.encode(d(TEST_PUBLIC)),
            auth: "BTBZMqHH6r4Tts7J/aSIgg==".to_string(),
        };
        assert_eq!(
            validate_subscription("https://fcm.googleapis.com/x", &padded).unwrap(),
            keys
        );
        for bad in [
            WebPushKeys {
                p256dh: "AAAA".into(),
                auth: keys.auth.clone(),
            },
            WebPushKeys {
                p256dh: keys.p256dh.clone(),
                auth: "AAAA".into(),
            },
            WebPushKeys {
                p256dh: "!!".into(),
                auth: keys.auth.clone(),
            },
        ] {
            assert_eq!(
                validate_subscription("https://fcm.googleapis.com/x", &bad),
                Err(WebPushSubscriptionError::Keys)
            );
        }
    }

    #[test]
    fn withdrawals_go_only_where_the_ask_was_shown_and_the_record_is_bounded() {
        let mut asks = AnnouncedAsks::new(2);
        assert!(!asks.contains("push_a", "c1"));
        asks.record("push_a", "c1");
        assert!(asks.contains("push_a", "c1"));
        assert!(!asks.contains("push_b", "c1"), "per subscription");
        asks.record("push_a", "c2");
        asks.record("push_a", "c3");
        assert!(!asks.contains("push_a", "c1"), "the oldest goes first");
        assert!(asks.contains("push_a", "c2") && asks.contains("push_a", "c3"));
        asks.forget("push_a", "c2");
        assert!(!asks.contains("push_a", "c2"));
    }

    #[test]
    fn the_payload_is_content_free_and_the_headers_follow_it() {
        let data = json!({
            "kind": "attention_requested",
            "correlation_id": "exec-1:p:s~a",
            "source": "agentic",
            "event_timestamp": 5,
            "expires_at": 1_000_000 + 600_000,
            "deep_link": "magican://attention?correlation_id=exec-1",
            "actions": "approve_reject",
            "answer_mode": "choice",
            "option_count": 3,
        });
        let payload = web_push_payload(&data).unwrap();
        assert_eq!(
            payload,
            json!({
                "kind": "attention_requested",
                "correlation_id": "exec-1:p:s~a",
                "source": "agentic",
                "expires_at": 1_600_000,
                "web_path": "/attention?attention_item=exec-1%3Ap%3As~a",
                "answer_mode": "choice",
                "option_count": 3,
            })
        );
        assert!(
            payload["option_count"].is_u64(),
            "a JSON number, not FCM's string"
        );
        let headers = web_push_headers(&payload, 1_000_000, 3_600);
        assert_eq!(headers.ttl, 600);
        assert_eq!(headers.urgency, "high");
        let topic = headers.topic.unwrap();
        assert_eq!(topic, push_topic("exec-1:p:s~a"));
        assert!(topic.len() <= 32);
        assert!(topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        // Past expiry: TTL 0, never negative.
        assert_eq!(web_push_headers(&payload, 2_000_000, 3_600).ttl, 0);
        // No mode: no mode keys.
        let plain = web_push_payload(&json!({
            "kind": "attention_requested", "correlation_id": "plan_1",
            "source": "approval", "expires_at": 7,
        }))
        .unwrap();
        assert!(plain.get("answer_mode").is_none());
        assert!(plain.get("option_count").is_none());

        for kind in ["attention_resolved", "attention_expired"] {
            let payload = web_push_payload(
                &json!({"kind": kind, "correlation_id": "plan_1", "event_timestamp": 9}),
            )
            .unwrap();
            assert_eq!(payload, json!({"kind": kind, "correlation_id": "plan_1"}));
            let headers = web_push_headers(&payload, 0, 3_600);
            assert_eq!(headers.ttl, 3_600);
            assert_eq!(headers.urgency, "normal");
            // The same topic as the ask: it replaces an undelivered ask.
            assert_eq!(headers.topic.unwrap(), push_topic("plan_1"));
        }
        assert!(web_push_payload(&json!({"kind": "task_progress", "task_id": "t"})).is_none());
    }
}
