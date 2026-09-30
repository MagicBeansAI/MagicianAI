use rand::RngCore;
use zeroize::Zeroizing;

/// Per-process, memory-only MAC key. RESERVED / not on the live security path.
///
/// The trusted-store guarantee is carried by the in-process `TrustAuthority` +
/// the `content_hash` verified at the decision site; the sidecar-MAC layer this
/// key was built for was cut (the content-hash check supersedes it). `BootKey` is
/// retained — still held by `AgentApiServices` — as a ready primitive in case loud
/// tamper-*observability* (a `.mac` sidecar on the stores) is ever wanted. Never
/// persisted, never in the OS keychain; regenerated every boot (so any sidecar it
/// ever produced would not survive a restart — matching the "re-ask, don't trust
/// unverifiable disk" rule).
#[derive(Clone)]
pub struct BootKey(std::sync::Arc<Zeroizing<[u8; 32]>>);

impl BootKey {
    pub fn generate() -> Self {
        let mut k = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut k);
        BootKey(std::sync::Arc::new(Zeroizing::new(k)))
    }

    /// Keyed BLAKE3 over exact bytes. `blake3::Hash` compares in constant time.
    pub fn mac(&self, bytes: &[u8]) -> blake3::Hash {
        // Explicit annotation forces the multi-step deref coercion
        // `&Arc<Zeroizing<[u8; 32]>>` -> `&[u8; 32]` at this binding, so the
        // exact `&[u8; 32]` type required by `keyed_hash` is unambiguous.
        let key: &[u8; 32] = &self.0;
        blake3::keyed_hash(key, bytes)
    }
}

impl std::fmt::Debug for BootKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BootKey(<redacted>)")
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn mac_is_deterministic_for_same_key_and_bytes() {
        let key = BootKey::generate();
        let a = key.mac(b"hello world");
        let b = key.mac(b"hello world");
        assert_eq!(a, b, "same key + same bytes must produce the same MAC");
    }

    #[test]
    fn mac_differs_for_different_bytes() {
        let key = BootKey::generate();
        let a = key.mac(b"hello world");
        let b = key.mac(b"hello worle");
        assert_ne!(a, b, "different bytes must produce a different MAC");
    }

    #[test]
    fn mac_differs_for_different_boot_key() {
        let k1 = BootKey::generate();
        let k2 = BootKey::generate();
        let a = k1.mac(b"same bytes");
        let b = k2.mac(b"same bytes");
        // OsRng makes an accidental collision astronomically unlikely; a
        // different per-boot key must not reproduce another key's MAC.
        assert_ne!(a, b, "a different BootKey must produce a different MAC");
    }

    #[test]
    fn debug_is_redacted() {
        let key = BootKey::generate();
        assert_eq!(format!("{:?}", key), "BootKey(<redacted>)");
    }
}
