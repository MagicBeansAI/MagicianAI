//! What an agent may take from a phone, and the record of what it took.
//!
//! Phase 5 of the Android companion plan. The device itself owns the per-app
//! judgment — a protected app's pixels are never captured, so the wire never
//! carries them — while this module owns the two things only the Magician
//! side can: the scope-wide screenshot policy (a kill switch that refuses the
//! capture before any device round trip) and the durable audit trail that
//! answers "what was done on my phone, to which app" — the generic tool-call
//! trace records *that* `android_act` ran, not what it touched.
//!
//! Both stores live beside the pairing roster under `<base_root>/system/`,
//! published process-wide the same way the device bridge hub is: compiled
//! tool handlers cannot thread constructor arguments, so the binary installs
//! them at boot.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::io::{sync_parent_dir_blocking, write_bytes_durably};

// ---------------------------------------------------------------------------
// Screenshot policy
// ---------------------------------------------------------------------------

/// The scope-wide switch above the device's per-app gate.
///
/// The device refuses protected apps no matter what this says; this is the
/// blanket answer for everything else. Its own file rather than a field on
/// the pairing roster: the roster file is a bare `Vec<PairedDevice>` on disk,
/// and growing it into a struct would break every existing install's parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotPolicy {
    /// Screenshots flow, except from protected apps (which the device already
    /// refuses). The default.
    #[default]
    AllowUnlessProtected,
    /// `android_screenshot` is refused for the whole deployment, before any
    /// device is asked.
    BlockAll,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct PersistedDevicePolicy {
    #[serde(default)]
    screenshot_policy: ScreenshotPolicy,
    /// Paired device ids the owner permitted to read verification codes
    /// from their notifications for a live challenge (secure HITL P6).
    /// Pairing alone grants nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    verification_code_devices: Vec<String>,
}

/// Owner-settable device policy, one file per data root.
pub struct DevicePolicyStore {
    path: PathBuf,
    policy: tokio::sync::RwLock<PersistedDevicePolicy>,
}

impl DevicePolicyStore {
    /// Open the policy, tolerating its absence — a missing file is the
    /// default policy, not an error.
    pub async fn open(base_root: &Path) -> std::io::Result<Self> {
        let path = base_root.join("system").join("device-policy.json");
        let policy = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                // A corrupt owner policy cannot be interpreted as permission.
                // Keep the daemon available but force the conservative kill
                // switch until the owner rewrites a valid policy.
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "device policy file is unreadable; blocking all screenshots"
                );
                PersistedDevicePolicy {
                    screenshot_policy: ScreenshotPolicy::BlockAll,
                    // An unreadable policy is no permission either.
                    verification_code_devices: Vec::new(),
                }
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PersistedDevicePolicy::default()
            },
            Err(error) => return Err(error),
        };
        Ok(Self {
            path,
            policy: tokio::sync::RwLock::new(policy),
        })
    }

    pub async fn screenshot_policy(&self) -> ScreenshotPolicy {
        self.policy.read().await.screenshot_policy
    }

    pub async fn set_screenshot_policy(
        &self,
        policy: ScreenshotPolicy,
    ) -> std::io::Result<ScreenshotPolicy> {
        // The write guard is held across serialize and publish, so two
        // concurrent PUTs cannot interleave snapshot and rename.
        let mut guard = self.policy.write().await;
        guard.screenshot_policy = policy;
        let bytes = serde_json::to_vec_pretty(&*guard).expect("policy serializes");
        write_bytes_durably(&self.path, &bytes).await?;
        Ok(policy)
    }

    /// Devices permitted to read verification codes for a live challenge.
    pub async fn verification_code_devices(&self) -> Vec<String> {
        self.policy.read().await.verification_code_devices.clone()
    }

    pub async fn permits_verification_codes(&self, device_id: &str) -> bool {
        self.policy
            .read()
            .await
            .verification_code_devices
            .iter()
            .any(|permitted| permitted == device_id)
    }

    /// Grant or withdraw the verification-code purpose for one paired device.
    pub async fn set_verification_code_device(
        &self,
        device_id: &str,
        permitted: bool,
    ) -> std::io::Result<Vec<String>> {
        let mut guard = self.policy.write().await;
        guard.verification_code_devices.retain(|d| d != device_id);
        if permitted {
            guard.verification_code_devices.push(device_id.to_string());
        }
        let bytes = serde_json::to_vec_pretty(&*guard).expect("policy serializes");
        write_bytes_durably(&self.path, &bytes).await?;
        Ok(guard.verification_code_devices.clone())
    }
}

// ---------------------------------------------------------------------------
// Audit trail
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceActionVerdict {
    /// The device did it.
    Ok,
    /// The device refused: a protected app was foregrounded.
    AppProtected,
    /// Magician refused before asking any device: the scope policy.
    PolicyBlocked,
    /// The dispatch failed — bridge error, timeout, device error.
    Error,
}

/// One device action, as the owner would want it recounted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceActionRecord {
    pub ts_ms: i64,
    pub principal: String,
    pub workspace: String,
    /// Empty when no device was resolved (policy refusals).
    pub device_id: String,
    /// The agent-facing tool (`android_act`), not just the wire action.
    pub tool: String,
    /// The device action dispatched (`android_tap`).
    pub action: String,
    /// The app that was on screen, when the device said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_package: Option<String>,
    pub verdict: DeviceActionVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Whether pixels left the device on this call.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub screenshot: bool,
    /// Exact live handset socket used by the action. This is audit/evidence
    /// attribution only and is deliberately excluded from stable effect
    /// identity so completed bytes can recover after an ordinary reconnect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_generation: Option<String>,
    /// Server-decoded Play Integrity verdict bound to that exact socket proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_integrity_verdict_digest: Option<String>,
}

/// What a read recovered, with the two loss classes kept separate — the same
/// contract as the tolerant JSONL readers in `artifact_v2`.
#[derive(Debug, Default)]
pub struct DeviceAuditRead {
    /// Newest first.
    pub records: Vec<DeviceActionRecord>,
    /// Committed lines that no longer parse: real loss, surfaced to the API.
    pub corrupt_lines: usize,
    /// Bytes after the last newline: never committed, not a fault.
    pub torn_tail_bytes: usize,
}

/// Append-only device-action log, one file per data root.
///
/// Appends serialize on the store's own lock and fsync per record — a device
/// action is a human-paced event, seconds apart at its fastest, so this is
/// not a hot path and the durability is worth the syscall. The parent
/// directory is synced when the file is first created so the log itself
/// survives a crash.
///
/// **Bounded.** When an append pushes the file past `max_bytes`, the newest
/// `keep_on_compaction` records are rewritten through the durable writer and
/// the rest age out — the same discipline `transport_log` applies to its
/// event log, which the first cut of this store ignored. An audit that grows
/// forever eventually becomes an audit nobody can read.
pub struct DeviceActionAudit {
    path: PathBuf,
    /// Owned by the blocking transaction itself. Cancelling the async waiter
    /// cannot release serialization while an fsync/compaction is still live.
    write_lock: Arc<std::sync::Mutex<()>>,
    /// Bounds queued/running filesystem transactions even if the blocking pool
    /// or storage stalls. The permit is moved into the blocking closure.
    blocking_capacity: Arc<tokio::sync::Semaphore>,
    max_bytes: u64,
    keep_on_compaction: usize,
}

/// ~4 MiB of ~200-byte records is roughly 20k device actions — months of
/// heavy use — and small enough that the whole-file tolerant read stays
/// interactive.
const AUDIT_MAX_BYTES: u64 = 4 * 1024 * 1024;
const AUDIT_KEEP_ON_COMPACTION: usize = 10_000;
const AUDIT_BLOCKING_CAPACITY: usize = 2;

/// The tolerant parse shared by reads and compaction: a torn tail is dropped
/// and measured, a corrupt interior line is skipped and counted, never fatal.
fn parse_committed(bytes: &[u8]) -> DeviceAuditRead {
    let mut read = DeviceAuditRead::default();
    let committed = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(last_newline) => {
            read.torn_tail_bytes = bytes.len() - last_newline - 1;
            &bytes[..=last_newline]
        },
        None => {
            read.torn_tail_bytes = bytes.len();
            &[][..]
        },
    };
    for line in committed.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<DeviceActionRecord>(line) {
            Ok(record) => read.records.push(record),
            Err(_) => read.corrupt_lines += 1,
        }
    }
    read
}

impl DeviceActionAudit {
    pub fn open(base_root: &Path) -> Self {
        Self::with_limits(base_root, AUDIT_MAX_BYTES, AUDIT_KEEP_ON_COMPACTION)
    }

    /// Test seam: production uses [`Self::open`]'s defaults.
    pub fn with_limits(base_root: &Path, max_bytes: u64, keep_on_compaction: usize) -> Self {
        Self {
            path: base_root.join("system").join("device-actions.jsonl"),
            write_lock: Arc::new(std::sync::Mutex::new(())),
            blocking_capacity: Arc::new(tokio::sync::Semaphore::new(AUDIT_BLOCKING_CAPACITY)),
            max_bytes,
            keep_on_compaction,
        }
    }

    pub async fn append(&self, record: DeviceActionRecord) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(&record).expect("audit record serializes");
        line.push(b'\n');
        let capacity = Arc::clone(&self.blocking_capacity)
            .acquire_owned()
            .await
            .map_err(|_| std::io::Error::other("device audit writer is closed"))?;
        let write_lock = Arc::clone(&self.write_lock);
        let path = self.path.clone();
        let max_bytes = self.max_bytes;
        let keep = self.keep_on_compaction;
        // The blocking pool, not the executor: open + write + fsync is three
        // blocking syscalls, and the async caller is a tool handler.
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let _capacity = capacity;
            let _guard = write_lock
                .lock()
                .map_err(|_| std::io::Error::other("device audit writer lock is poisoned"))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let created = !path.exists();
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            file.write_all(&line)?;
            file.sync_all()?;
            let bytes_now = file.metadata()?.len();
            drop(file);
            if created {
                // The rename-less sibling of the durable write: a new file's
                // directory entry needs the parent synced to survive a crash.
                sync_parent_dir_blocking(&path)?;
            }
            if bytes_now > max_bytes {
                // Still under the store's write lock: rewrite the newest
                // `keep` records through the durable writer (unique temp,
                // fsync, rename, parent sync), so the compaction itself can
                // only ever publish whole-or-previous.
                let read = parse_committed(&std::fs::read(&path)?);
                let start = read.records.len().saturating_sub(keep);
                let mut compacted = Vec::new();
                for record in &read.records[start..] {
                    compacted.extend_from_slice(
                        &serde_json::to_vec(record).expect("record reserializes"),
                    );
                    compacted.push(b'\n');
                }
                crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(&path, &compacted)?;
            }
            Ok(())
        })
        .await
        .map_err(|error| std::io::Error::other(format!("audit append task panicked: {error}")))?
    }

    /// Newest-first page, tolerant of a torn tail and corrupt lines.
    pub async fn read_recent(&self, limit: usize) -> std::io::Result<DeviceAuditRead> {
        let mut read = self.read_all().await?;
        read.records.reverse(); // newest first
        read.records.truncate(limit);
        Ok(read)
    }

    /// Newest-first page for one scope, filtered during the scan.
    ///
    /// The loss counts stay **deployment-global**: a corrupt line has no
    /// readable scope to attribute it to, and hiding damage from the scope
    /// that happens to ask would be worse than telling everyone.
    pub async fn read_recent_for_scope(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> std::io::Result<DeviceAuditRead> {
        let mut read = self.read_all().await?;
        read.records
            .retain(|record| record.principal == principal && record.workspace == workspace);
        read.records.reverse(); // newest first
        read.records.truncate(limit);
        Ok(read)
    }

    async fn read_all(&self) -> std::io::Result<DeviceAuditRead> {
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DeviceAuditRead::default());
            },
            Err(error) => return Err(error),
        };
        Ok(parse_committed(&bytes))
    }
}

// ---------------------------------------------------------------------------
// Process-wide handles, the same pattern as the device bridge hub
// ---------------------------------------------------------------------------

static GLOBAL_POLICY: OnceLock<Arc<DevicePolicyStore>> = OnceLock::new();
static GLOBAL_AUDIT: OnceLock<Arc<DeviceActionAudit>> = OnceLock::new();

pub fn install_global_device_governance(
    policy: Arc<DevicePolicyStore>,
    audit: Arc<DeviceActionAudit>,
) {
    let _ = GLOBAL_POLICY.set(policy);
    let _ = GLOBAL_AUDIT.set(audit);
}

pub fn global_device_policy() -> Option<Arc<DevicePolicyStore>> {
    GLOBAL_POLICY.get().cloned()
}

pub fn global_device_audit() -> Option<Arc<DeviceActionAudit>> {
    GLOBAL_AUDIT.get().cloned()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn record(tool: &str, verdict: DeviceActionVerdict) -> DeviceActionRecord {
        DeviceActionRecord {
            ts_ms: 1,
            principal: "alpha".into(),
            workspace: "prod".into(),
            device_id: "phone".into(),
            tool: tool.into(),
            action: format!("{tool}_wire"),
            foreground_package: Some("com.example.app".into()),
            verdict,
            detail: None,
            screenshot: tool == "android_screenshot",
            connection_generation: None,
            play_integrity_verdict_digest: None,
        }
    }

    #[tokio::test]
    async fn appends_and_reads_newest_first() {
        let temp = tempfile::tempdir().unwrap();
        let audit = DeviceActionAudit::open(temp.path());
        audit
            .append(record("android_snapshot", DeviceActionVerdict::Ok))
            .await
            .unwrap();
        audit
            .append(record("android_act", DeviceActionVerdict::Ok))
            .await
            .unwrap();
        audit
            .append(record(
                "android_screenshot",
                DeviceActionVerdict::AppProtected,
            ))
            .await
            .unwrap();

        let read = audit.read_recent(10).await.unwrap();
        assert_eq!(read.records.len(), 3);
        assert_eq!(read.records[0].tool, "android_screenshot");
        assert_eq!(read.records[0].verdict, DeviceActionVerdict::AppProtected);
        assert_eq!(read.records[2].tool, "android_snapshot");
        assert_eq!(read.corrupt_lines, 0);
        assert_eq!(read.torn_tail_bytes, 0);

        let page = audit.read_recent(2).await.unwrap();
        assert_eq!(page.records.len(), 2, "limit pages from the newest end");
        assert_eq!(page.records[0].tool, "android_screenshot");
    }

    /// The tolerance contract, and the mutation check that proves the test
    /// bites: a torn tail is not loss, a corrupt interior line is counted
    /// loss, and neither fails the read.
    #[tokio::test]
    async fn survives_a_torn_tail_and_a_corrupt_line() {
        let temp = tempfile::tempdir().unwrap();
        let audit = DeviceActionAudit::open(temp.path());
        audit
            .append(record("android_snapshot", DeviceActionVerdict::Ok))
            .await
            .unwrap();
        audit
            .append(record("android_act", DeviceActionVerdict::Ok))
            .await
            .unwrap();

        // A corrupt committed line, then a torn (unterminated) tail.
        let path = temp.path().join("system").join("device-actions.jsonl");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{this line was damaged on disk}\n");
        bytes.extend_from_slice(b"{\"ts_ms\":99,\"princi");
        std::fs::write(&path, &bytes).unwrap();

        let read = audit.read_recent(10).await.unwrap();
        assert_eq!(read.records.len(), 2, "both intact records survive");
        assert_eq!(
            read.corrupt_lines, 1,
            "committed damage is counted, not hidden"
        );
        assert_eq!(
            read.torn_tail_bytes, 19,
            "the torn tail is measured, not parsed"
        );
    }

    #[tokio::test]
    async fn concurrent_appends_all_survive() {
        let temp = tempfile::tempdir().unwrap();
        let audit = Arc::new(DeviceActionAudit::open(temp.path()));
        let mut handles = Vec::new();
        for index in 0..16 {
            let audit = audit.clone();
            handles.push(tokio::spawn(async move {
                let mut r = record("android_act", DeviceActionVerdict::Ok);
                r.ts_ms = index;
                audit.append(r).await
            }));
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        let read = audit.read_recent(32).await.unwrap();
        assert_eq!(read.records.len(), 16);
        assert_eq!(read.corrupt_lines, 0, "no interleaved partial lines");
    }

    #[tokio::test]
    async fn cancelled_waiter_cannot_release_a_live_blocking_writer_lease() {
        let temp = tempfile::tempdir().unwrap();
        let audit = DeviceActionAudit::open(temp.path());
        let capacity = Arc::clone(&audit.blocking_capacity)
            .acquire_owned()
            .await
            .unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let transaction = tokio::task::spawn_blocking(move || {
            let _capacity = capacity;
            let _ = entered_tx.send(());
            let _ = release_rx.recv();
        });
        entered_rx.await.unwrap();
        assert_eq!(audit.blocking_capacity.available_permits(), 1);

        // Dropping the async join is the same cancellation shape as an Apps
        // permit expiry while fsync is still in the blocking closure. The
        // owned lease must remain unavailable until that closure really exits.
        transaction.abort();
        assert_eq!(audit.blocking_capacity.available_permits(), 1);
        release_tx.send(()).unwrap();
        for _ in 0..100 {
            if audit.blocking_capacity.available_permits() == AUDIT_BLOCKING_CAPACITY {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            audit.blocking_capacity.available_permits(),
            AUDIT_BLOCKING_CAPACITY
        );
    }

    #[tokio::test]
    async fn compaction_keeps_the_newest_records_and_bounds_the_file() {
        let temp = tempfile::tempdir().unwrap();
        // Tiny limits so the test does not need thousands of fsyncs: compact
        // past 2 KiB, keep the newest 5.
        let audit = DeviceActionAudit::with_limits(temp.path(), 2048, 5);
        for index in 0..40 {
            let mut r = record("android_act", DeviceActionVerdict::Ok);
            r.ts_ms = index;
            audit.append(r).await.unwrap();
        }
        let read = audit.read_recent(100).await.unwrap();
        // The honest bound: compaction keeps `keep` and the file then refills
        // until it next crosses `max_bytes`, so the steady-state count is
        // `keep` plus however many records fit in `max_bytes`. Asserting a
        // tight number here would only encode where in that cycle the 40th
        // append happened to land — a function of record size, not of the
        // invariant. What matters is that 40 appends did NOT accumulate 40
        // records, and that the file is bounded (checked below).
        assert!(
            read.records.len() < 40,
            "compaction ran — 40 appends did not keep 40 records (kept {})",
            read.records.len()
        );
        let newest = read.records.first().unwrap().ts_ms;
        assert_eq!(newest, 39, "the newest record survives compaction");
        let size = std::fs::metadata(temp.path().join("system").join("device-actions.jsonl"))
            .unwrap()
            .len();
        // max_bytes plus one over-limit append's slack; nowhere near 40 records.
        assert!(size < 2048 + 512, "the file stays bounded ({size} bytes)");
    }

    #[tokio::test]
    async fn scoped_read_filters_records_but_reports_global_loss() {
        let temp = tempfile::tempdir().unwrap();
        let audit = DeviceActionAudit::open(temp.path());
        audit
            .append(record("android_act", DeviceActionVerdict::Ok))
            .await
            .unwrap();
        let mut other = record("android_snapshot", DeviceActionVerdict::Ok);
        other.principal = "beta".into();
        audit.append(other).await.unwrap();

        // Damage a committed line: the scoped read must still surface it.
        let path = temp.path().join("system").join("device-actions.jsonl");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{damaged}\n");
        std::fs::write(&path, &bytes).unwrap();

        let read = audit
            .read_recent_for_scope("alpha", "prod", 10)
            .await
            .unwrap();
        assert_eq!(read.records.len(), 1, "only alpha/prod records return");
        assert_eq!(read.records[0].tool, "android_act");
        assert_eq!(
            read.corrupt_lines, 1,
            "loss counts are deployment-global: damage has no scope to hide behind"
        );
    }

    #[tokio::test]
    async fn policy_defaults_persists_and_reloads() {
        let temp = tempfile::tempdir().unwrap();
        let store = DevicePolicyStore::open(temp.path()).await.unwrap();
        assert_eq!(
            store.screenshot_policy().await,
            ScreenshotPolicy::AllowUnlessProtected,
            "a missing file is the default policy"
        );

        store
            .set_screenshot_policy(ScreenshotPolicy::BlockAll)
            .await
            .unwrap();
        assert_eq!(store.screenshot_policy().await, ScreenshotPolicy::BlockAll);

        // A fresh open reads the persisted choice back.
        let reopened = DevicePolicyStore::open(temp.path()).await.unwrap();
        assert_eq!(
            reopened.screenshot_policy().await,
            ScreenshotPolicy::BlockAll
        );
    }

    #[tokio::test]
    async fn a_corrupt_policy_file_fails_closed_without_blocking_startup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("system").join("device-policy.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();

        let store = DevicePolicyStore::open(temp.path()).await.unwrap();
        assert_eq!(
            store.screenshot_policy().await,
            ScreenshotPolicy::BlockAll,
            "corrupt policy cannot become screenshot permission"
        );
    }
}
