//! Durable record of the browsers the owner has accepted onto the bridge.
//!
//! A browser connecting for the first time is admitted `pending`: it holds a
//! slot but gets no work until the owner accepts it, in its extension panel or
//! remotely. Acceptance is kept here — browser id and a keyed hash of its
//! secret `browser_key` — so the next connect after a restart is accepted
//! without asking again. The hash key is a random secret generated once and
//! stored in this file (owner-only permissions), so hashes survive restarts
//! but cannot be computed by anyone who has not read the file.
//!
//! Upgrade path: the boot that creates this file writes an absolute
//! `migration_deadline_ms` ([`MIGRATION_WINDOW`] ahead). Until then, loopback
//! browsers that reconnect are accepted automatically (at most
//! `max_connections` of them), each recorded as `auto_accepted` so the owner
//! can list and revoke it; afterwards the window is closed for good — a
//! restart never reopens it. If the file cannot be written the store fails
//! closed: nothing is auto-accepted.
//!
//! Pairing is owner UX, not the trust boundary: the bridge token and a
//! loopback-only bind are (see the component docs).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{error, info, warn};

/// How long the one-time upgrade window stays open after the store is created.
pub const MIGRATION_WINDOW: Duration = Duration::from_secs(10 * 60);

const STORE_VERSION: u32 = 1;
const STORE_FILE_NAME: &str = "bridge_browsers.json";

/// One browser the owner accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedBrowser {
    pub browser_id: String,
    /// Keyed hash of the install's `browser_key` (see [`BrowserStore::key_hash`]).
    pub key_hash: String,
    pub label: String,
    pub accepted_at_ms: i64,
    /// Accepted without an owner decision: by the upgrade window or as the
    /// first browser ever (bootstrap). Listed so the owner can revoke it.
    #[serde(default)]
    pub auto_accepted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    /// 32 hex digits: the two SipHash keys for `key_hash`.
    secret: String,
    #[serde(default)]
    migration_done: bool,
    /// Absolute end of the upgrade window (epoch ms). Absent = no window.
    #[serde(default)]
    migration_deadline_ms: Option<i64>,
    #[serde(default)]
    accepted: Vec<AcceptedBrowser>,
    /// Browser chosen with "Always use this browser"; the default browser
    /// while it is connected and accepted.
    #[serde(default)]
    always_browser: Option<String>,
    /// Set by the first acceptance ever. The first-run bootstrap applies only
    /// before it, so revoking every browser cannot re-arm it.
    #[serde(default)]
    bootstrapped: bool,
}

pub struct BrowserStore {
    /// `None` keeps the store in memory only (tests).
    path: Option<PathBuf>,
    keys: (u64, u64),
    file: StoreFile,
    /// False once a save failed: the store then fails closed (no automatic
    /// acceptance of any kind) because nothing it decides would persist.
    persist_ok: bool,
}

impl Default for BrowserStore {
    /// An in-memory store with no upgrade window: nothing is auto-accepted
    /// except the first-run bootstrap.
    fn default() -> Self {
        Self::in_memory(false)
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn random_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
    .chars()
    .filter(|c| c.is_ascii_hexdigit())
    .take(32)
    .collect()
}

fn keys_from_secret(secret: &str) -> Option<(u64, u64)> {
    if secret.len() != 32 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let k0 = u64::from_str_radix(&secret[..16], 16).ok()?;
    let k1 = u64::from_str_radix(&secret[16..], 16).ok()?;
    Some((k0, k1))
}

fn fresh_file(window: bool) -> StoreFile {
    StoreFile {
        version: STORE_VERSION,
        secret: random_secret(),
        migration_done: !window,
        migration_deadline_ms: window.then(|| now_ms() + MIGRATION_WINDOW.as_millis() as i64),
        ..StoreFile::default()
    }
}

impl BrowserStore {
    /// An in-memory store. `migration_open` opens the upgrade window.
    pub fn in_memory(migration_open: bool) -> Self {
        let file = fresh_file(migration_open);
        let keys = keys_from_secret(&file.secret).unwrap_or((0x5eed, 0xb0a7));
        Self {
            path: None,
            keys,
            file,
            persist_ok: true,
        }
    }

    /// Where the process store lives: `MAGICUTOR_BROWSER_STORE_PATH`, else
    /// `<data root>/magicutor/bridge_browsers.json`.
    pub fn default_path() -> PathBuf {
        if let Ok(explicit) = std::env::var("MAGICUTOR_BROWSER_STORE_PATH") {
            if !explicit.trim().is_empty() {
                return PathBuf::from(explicit);
            }
        }
        crate::resolve_data_root()
            .join("magicutor")
            .join(STORE_FILE_NAME)
    }

    /// Load the store at `path`. Creating it opens the one-time upgrade
    /// window, ending at an absolute deadline written into the file; a store
    /// that exists keeps whatever window it has left — never a new one.
    pub fn load(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let existing = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<StoreFile>(&bytes).ok());
        let (mut file, created) = match existing {
            Some(file) if keys_from_secret(&file.secret).is_some() => (file, false),
            Some(_) => {
                warn!(path = %path.display(), "bridge browser store has no usable secret; starting a new one without an upgrade window");
                (fresh_file(false), false)
            },
            None if path.exists() => {
                warn!(path = %path.display(), "bridge browser store is unreadable; starting a new one without an upgrade window");
                (fresh_file(false), false)
            },
            None => (fresh_file(true), true),
        };
        // An absolute deadline: a restart inside the window keeps only what
        // is left of it; a store without one (older builds) has none.
        let window_left = file
            .migration_deadline_ms
            .is_some_and(|deadline| deadline > now_ms());
        if !window_left {
            file.migration_done = true;
        }
        // Files from before the flag: any recorded acceptance means the
        // bootstrap has happened.
        if !file.accepted.is_empty() {
            file.bootstrapped = true;
        }
        let keys = keys_from_secret(&file.secret).unwrap_or((0x5eed, 0xb0a7));
        let mut store = Self {
            path: Some(path),
            keys,
            file,
            persist_ok: true,
        };
        store.save();
        if !store.persist_ok {
            error!("bridge browser store cannot be saved: no browser is accepted automatically until it can be");
        } else if created {
            info!("bridge browser store created; loopback browsers reconnecting in the next 10 minutes are accepted once");
        }
        store
    }

    /// Keyed hash of a browser key. Stable across restarts (the key lives in
    /// the store file), so an accepted browser stays accepted. SipHash-2-4
    /// with a 128-bit secret key is a PRF; std's keyed `SipHasher` is
    /// deprecated only as a general-purpose hasher.
    #[allow(deprecated)]
    pub fn key_hash(&self, key: &str) -> String {
        use std::hash::Hasher;
        let mut first = std::hash::SipHasher::new_with_keys(self.keys.0, self.keys.1);
        first.write(key.as_bytes());
        let mut second = std::hash::SipHasher::new_with_keys(self.keys.1, self.keys.0);
        second.write(key.as_bytes());
        format!("{:016x}{:016x}", first.finish(), second.finish())
    }

    /// Whether `browser_id` with this key was accepted.
    pub fn is_accepted(&self, browser_id: &str, key: &str) -> bool {
        let hash = self.key_hash(key);
        self.file
            .accepted
            .iter()
            .any(|record| record.browser_id == browser_id && record.key_hash == hash)
    }

    /// Whether the store can make automatic decisions at all.
    pub fn persists(&self) -> bool {
        self.persist_ok
    }

    /// Whether the one-time upgrade window is open: before its absolute
    /// deadline, not closed by an owner decision, and the store saving.
    pub fn migration_open(&mut self) -> bool {
        if self.file.migration_done || !self.persist_ok {
            return false;
        }
        match self.file.migration_deadline_ms {
            Some(deadline) if deadline > now_ms() => true,
            _ => {
                self.close_migration();
                false
            },
        }
    }

    /// End the upgrade window for good.
    pub fn close_migration(&mut self) {
        if !self.file.migration_done {
            self.file.migration_done = true;
            self.save();
        }
    }

    /// Whether any browser was ever accepted (the first-run bootstrap is
    /// spent).
    pub fn bootstrapped(&self) -> bool {
        self.file.bootstrapped
    }

    /// How many browsers were accepted without an owner decision.
    pub fn auto_accepted_count(&self) -> usize {
        self.file
            .accepted
            .iter()
            .filter(|record| record.auto_accepted)
            .count()
    }

    /// Record (or refresh) a browser the owner accepted.
    pub fn accept(&mut self, browser_id: &str, key_hash: &str, label: &str) {
        self.record(browser_id, key_hash, label, false);
    }

    /// Record a browser accepted automatically (upgrade window, bootstrap).
    /// Returns false — and keeps nothing — when the record cannot be saved.
    pub fn accept_auto(&mut self, browser_id: &str, key_hash: &str, label: &str) -> bool {
        if !self.persist_ok {
            return false;
        }
        self.record(browser_id, key_hash, label, true);
        if !self.persist_ok {
            self.file
                .accepted
                .retain(|record| record.browser_id != browser_id);
            return false;
        }
        true
    }

    fn record(&mut self, browser_id: &str, key_hash: &str, label: &str, auto_accepted: bool) {
        self.file
            .accepted
            .retain(|record| record.browser_id != browser_id);
        self.file.accepted.push(AcceptedBrowser {
            browser_id: browser_id.to_string(),
            key_hash: key_hash.to_string(),
            label: label.to_string(),
            accepted_at_ms: now_ms(),
            auto_accepted,
        });
        self.file.bootstrapped = true;
        self.save();
    }

    /// Forget an accepted browser. Returns whether it was known.
    pub fn revoke(&mut self, browser_id: &str) -> bool {
        let before = self.file.accepted.len();
        self.file
            .accepted
            .retain(|record| record.browser_id != browser_id);
        let removed = self.file.accepted.len() != before;
        if self.file.always_browser.as_deref() == Some(browser_id) {
            self.file.always_browser = None;
        }
        self.save();
        removed
    }

    pub fn accepted(&self) -> &[AcceptedBrowser] {
        &self.file.accepted
    }

    /// Keep the stored label in step with a rename.
    pub fn relabel(&mut self, browser_id: &str, label: &str) {
        let mut changed = false;
        for record in self.file.accepted.iter_mut() {
            if record.browser_id == browser_id && record.label != label {
                record.label = label.to_string();
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }

    pub fn always_browser(&self) -> Option<&str> {
        self.file.always_browser.as_deref()
    }

    pub fn set_always_browser(&mut self, browser_id: Option<&str>) {
        self.file.always_browser = browser_id.map(str::to_string);
        self.save();
    }

    #[cfg(test)]
    pub(crate) fn set_migration_deadline_for_tests(&mut self, deadline_ms: i64) {
        self.file.migration_done = false;
        self.file.migration_deadline_ms = Some(deadline_ms);
        self.save();
    }

    fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let result = serde_json::to_vec_pretty(&self.file)
            .map_err(std::io::Error::other)
            .and_then(|bytes| write_owner_only(&path, &bytes));
        match result {
            Ok(()) => self.persist_ok = true,
            Err(err) => {
                if self.persist_ok {
                    error!(path = %path.display(), error = %err, "cannot save the bridge browser store; automatic acceptance is off");
                }
                self.persist_ok = false;
            },
        }
    }
}

/// Write `bytes` to `path` through a temp file created owner-only (0600 from
/// the start, never world-readable), then rename it into place.
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&temp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store_path(label: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("magicutor-store-{label}-{}", uuid::Uuid::new_v4()))
            .join(STORE_FILE_NAME)
    }

    #[test]
    fn acceptance_and_the_always_browser_survive_a_restart() {
        let path = temp_store_path("restart");
        let mut store = BrowserStore::load(&path);
        let hash = store.key_hash("install-key-0123456789");
        store.accept("install-1", &hash, "Comet");
        store.set_always_browser(Some("install-1"));
        drop(store);

        let reloaded = BrowserStore::load(&path);
        assert!(reloaded.is_accepted("install-1", "install-key-0123456789"));
        assert!(!reloaded.is_accepted("install-1", "another-key-0123456789"));
        assert!(!reloaded.is_accepted("install-2", "install-key-0123456789"));
        assert_eq!(reloaded.always_browser(), Some("install-1"));
        assert_eq!(reloaded.accepted()[0].label, "Comet");
        assert!(!reloaded.accepted()[0].auto_accepted);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_upgrade_window_has_an_absolute_deadline_that_a_restart_never_extends() {
        let path = temp_store_path("migration");
        let mut first = BrowserStore::load(&path);
        assert!(first.migration_open());
        // Restarting inside the window keeps it (what is left of it).
        drop(first);
        let mut second = BrowserStore::load(&path);
        assert!(second.migration_open());
        // Once the deadline has passed it is closed, with or without any
        // browser connecting, and a restart does not reopen it.
        second.set_migration_deadline_for_tests(now_ms() - 1);
        assert!(!second.migration_open());
        drop(second);
        let mut third = BrowserStore::load(&path);
        assert!(!third.migration_open());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_store_written_without_a_deadline_has_no_window() {
        let path = temp_store_path("legacy-file");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                r#"{{"version":1,"secret":"{}","migration_done":false}}"#,
                random_secret()
            ),
        )
        .unwrap();
        let mut store = BrowserStore::load(&path);
        assert!(!store.migration_open());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_store_that_cannot_be_saved_fails_closed() {
        let dir = std::env::temp_dir().join(format!("magicutor-store-ro-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // The parent "directory" is a file, so nothing can be written.
        let blocker = dir.join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let mut store = BrowserStore::load(blocker.join(STORE_FILE_NAME));
        assert!(!store.persists());
        assert!(!store.migration_open());
        let hash = store.key_hash("install-key-0123456789");
        assert!(!store.accept_auto("install-1", &hash, "Dia"));
        assert!(store.accepted().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_store_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_store_path("mode");
        let _store = BrowserStore::load(&path);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn auto_acceptances_are_recorded_and_counted() {
        let mut store = BrowserStore::in_memory(true);
        let hash = store.key_hash("k-0123456789abcdef");
        assert!(store.accept_auto("install-1", &hash, "Chrome"));
        assert_eq!(store.auto_accepted_count(), 1);
        assert!(store.accepted()[0].auto_accepted);
        // An owner accept of the same browser replaces the automatic one.
        store.accept("install-1", &hash, "Chrome");
        assert_eq!(store.auto_accepted_count(), 0);
    }

    #[test]
    fn revoking_every_browser_does_not_re_arm_the_first_run_bootstrap() {
        let path = temp_store_path("bootstrap");
        let mut store = BrowserStore::load(&path);
        assert!(!store.bootstrapped());
        let hash = store.key_hash("install-key-0123456789");
        store.accept("install-1", &hash, "Chrome");
        assert!(store.bootstrapped());
        store.revoke("install-1");
        assert!(store.accepted().is_empty());
        assert!(store.bootstrapped());
        drop(store);
        assert!(BrowserStore::load(&path).bootstrapped());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn revoking_forgets_the_browser_and_its_always_choice() {
        let mut store = BrowserStore::in_memory(false);
        let hash = store.key_hash("install-key-0123456789");
        store.accept("install-1", &hash, "Dia");
        store.set_always_browser(Some("install-1"));
        assert!(store.revoke("install-1"));
        assert!(!store.is_accepted("install-1", "install-key-0123456789"));
        assert_eq!(store.always_browser(), None);
        assert!(!store.revoke("install-1"));
    }

    #[test]
    fn key_hashes_depend_on_the_store_secret() {
        let a = BrowserStore::in_memory(false);
        let b = BrowserStore::in_memory(false);
        assert_eq!(a.key_hash("k"), a.key_hash("k"));
        assert_ne!(a.key_hash("k"), b.key_hash("k"));
        assert_eq!(a.key_hash("k").len(), 32);
    }
}
