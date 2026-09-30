//! Per-(principal, workspace) transport event log.
//!
//! Persists ordinary `RuntimeTransportEvent`s that flow through the
//! broadcaster to a workspace-scoped JSONL file so refreshing the
//! `/events` UI (or any operator surface that reads on-disk events)
//! shows the same rows that were live in-memory before the page reload.
//! Host-sealed app owner notifications are deliberately excluded: their
//! bounded UserRequest/Attention owner is the only durable body surface.
//!
//! ## Why this exists
//!
//! Two event paths used to coexist:
//!   1. **Per-execution canonical** — `ArtifactV2EventType` events are
//!      written to `…/tasks/{task}/executions/{exec}/events.jsonl` by
//!      `FilesystemRuntimeEventSink`. Survives refresh.
//!   2. **Broadcaster-only** — `RuntimeTransportEvent` variants like
//!      `FeedItemCreated`, `ChatMessageReceived`, `ExecutionPanelDelta`,
//!      and the AGUI envelope events flowed through the broadcaster but
//!      were never persisted. They lived in the broadcaster's bounded
//!      ring buffer and disappeared when they aged out.
//!
//! The result was visible to operators: refreshing the panel made certain
//! categories of UI-relevant events vanish. This module closes the gap.
//!
//! ## Layout
//!
//! `…/scopes/{principal}/{workspace}/events.jsonl` — one append-only log
//! per (principal, workspace). Events that emit with `principal: None`
//! / `workspace: None` (system-emitted heartbeats, capability bootstrap
//! events, etc.) land in the `system/system` bucket, mirroring the
//! `event_visible_to_scope` policy from magician v0.6.482.
//!
//! ## Retention
//!
//! On every periodic compaction tick, each log is rewritten to keep the
//! **larger** of:
//!   - events newer than [`EVENTS_RETENTION_WINDOW_MS`] (default 24h), OR
//!   - the newest [`EVENTS_RETENTION_MIN_COUNT`] events (default 2000).
//!
//! Whichever criterion preserves more rows wins. This guarantees that
//! quiet workspaces retain at least the last 2000 events (so a refresh
//! days later still has context) while busy workspaces don't grow
//! unboundedly past 24h. The constants are deliberately hard-coded
//! rather than configurable — they're the contract this module exposes.
//! A startup privacy migration removes legacy app-notification bodies before
//! this ordinary count floor applies. The migration streams each log through a
//! bounded line buffer so large ordinary histories are preserved.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{anyhow, Context};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::magician_v2::{
    agents::storage::AgentStorage,
    artifact_v2::{
        io::{publish_staged_file_durably_sync, write_bytes_durably},
        workspace::ArtifactV2Workspace,
    },
    realtime_events::RuntimeTransportEvent,
};

/// Per-scope log retention floor — events newer than this are always
/// kept, regardless of count. Rolling 24h window in ms.
pub const EVENTS_RETENTION_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// Per-scope log retention floor — at least this many newest events are
/// always kept, regardless of age. Lets a quiet scope keep enough
/// context that a multi-day-later refresh still shows recent activity.
pub const EVENTS_RETENTION_MIN_COUNT: usize = 2000;

/// How often the background compactor walks every known log and trims
/// to the retention bounds. Bigger = lower IO overhead, more drift past
/// the bounds between sweeps.
pub const COMPACTION_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Read-side page size for the startup notification-body privacy rewrite.
/// The source log itself has no aggregate ceiling: ordinary event history must
/// not be discarded merely because a quiet scope accumulated a large file.
const APP_OWNER_NOTIFICATION_SCRUB_READ_BYTES: u64 = 64 * 1024;

/// A single JSONL row has to be bounded independently of the aggregate log.
/// Rows above this ceiling cannot be classified without retaining attacker-
/// controlled memory, so the rewrite omits that one malformed row fail closed
/// while preserving every other ordinary row.
const MAX_APP_OWNER_NOTIFICATION_SCRUB_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Retained bytes are flushed to the staging file in bounded chunks. A single
/// admitted row may temporarily make this buffer larger, but never beyond the
/// per-line ceiling plus one input page.
const APP_OWNER_NOTIFICATION_SCRUB_OUTPUT_BYTES: usize = 256 * 1024;

/// Special bucket for events emitted with no `(principal, workspace)`
/// scope — heartbeats, system-level capability events, anything from
/// boot before a user scope is established. Keeps them visible to
/// operators querying the system bucket without leaking into a tenant
/// view.
pub const SYSTEM_PRINCIPAL: &str = "system";
pub const SYSTEM_WORKSPACE: &str = "system";

/// Quarantine bucket for events whose `(principal, workspace)` strings
/// failed the safe-scope check. Distinct from `system/system` so an
/// operator querying the system bucket doesn't end up reading
/// cross-tenant payloads that landed here only because their scope
/// strings were unsafe to put on disk. The payload still carries its
/// original principal/workspace fields — only the on-disk path is
/// rewritten.
pub const QUARANTINE_PRINCIPAL: &str = "_quarantine";
pub const QUARANTINE_WORKSPACE: &str = "_quarantine";

const LOG_FILENAME: &str = "events.jsonl";
const FILE_OPEN_RETRY_ATTEMPTS: usize = 8;
const FILE_OPEN_RETRY_DELAY: Duration = Duration::from_millis(25);

fn is_fd_pressure(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(23 | 24))
}

async fn sleep_fd_retry(attempt: usize) {
    let multiplier = 1u32 << attempt.min(4);
    tokio::time::sleep(FILE_OPEN_RETRY_DELAY * multiplier).await;
}

async fn open_append_with_fd_retry(path: &Path) -> io::Result<tokio::fs::File> {
    for attempt in 0..FILE_OPEN_RETRY_ATTEMPTS {
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
        {
            Ok(file) => return Ok(file),
            Err(err) if is_fd_pressure(&err) && attempt + 1 < FILE_OPEN_RETRY_ATTEMPTS => {
                sleep_fd_retry(attempt).await;
            },
            Err(err) => return Err(err),
        }
    }
    unreachable!("file-open retry loop always returns on final attempt")
}

async fn open_read_with_fd_retry(path: &Path) -> io::Result<tokio::fs::File> {
    for attempt in 0..FILE_OPEN_RETRY_ATTEMPTS {
        match fs::File::open(path).await {
            Ok(file) => return Ok(file),
            Err(err) if is_fd_pressure(&err) && attempt + 1 < FILE_OPEN_RETRY_ATTEMPTS => {
                sleep_fd_retry(attempt).await;
            },
            Err(err) => return Err(err),
        }
    }
    unreachable!("file-open retry loop always returns on final attempt")
}

/// Local-provider compatibility publication with destination CAS. The caller
/// holds both the in-process and advisory cross-process event-log locks. The
/// bounded second pass catches a same-length replacement before the durable
/// rename instead of letting stale scrub staging lose ordinary rows.
fn publish_staged_file_if_destination_matches_sync(
    staged_path: &Path,
    destination_path: &Path,
    expected_staged_sha256: &str,
    expected_destination_len: u64,
    expected_destination_sha256: &str,
) -> io::Result<()> {
    ensure_file_matches_sync(
        staged_path,
        std::fs::metadata(staged_path)?.len(),
        expected_staged_sha256,
    )?;
    ensure_file_matches_sync(
        destination_path,
        expected_destination_len,
        expected_destination_sha256,
    )?;
    publish_staged_file_durably_sync(staged_path, destination_path)
}

fn ensure_file_matches_sync(
    destination_path: &Path,
    expected_destination_len: u64,
    expected_destination_sha256: &str,
) -> io::Result<()> {
    if expected_destination_sha256.len() != 64
        || !expected_destination_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "transport log file CAS requires a SHA-256 digest",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut destination = options.open(destination_path)?;
    if destination.metadata()?.len() != expected_destination_len {
        return Err(io::Error::other(
            "transport log file changed before scrub publication",
        ));
    }
    let mut digest = Sha256::new();
    let mut observed_len = 0_u64;
    let mut page = [0_u8; APP_OWNER_NOTIFICATION_SCRUB_READ_BYTES as usize];
    loop {
        let read = std::io::Read::read(&mut destination, &mut page)?;
        if read == 0 {
            break;
        }
        digest.update(&page[..read]);
        observed_len = observed_len.saturating_add(read as u64);
    }
    crate::magician_v2::artifact_v2::workspace::validate_workspace_file_path_identity(
        &destination,
        destination_path,
        "transport log file CAS",
    )?;
    if observed_len != expected_destination_len
        || destination.metadata()?.len() != expected_destination_len
        || !format!("{:x}", digest.finalize()).eq_ignore_ascii_case(expected_destination_sha256)
    {
        return Err(io::Error::other(
            "transport log file changed before scrub publication",
        ));
    }
    Ok(())
}

/// Test-only since compaction moved to the shared durable writer, which does
/// its own fd-pressure retry on the same `EMFILE`/`ENFILE` condition. Kept
/// because the compaction tests need to lay down a whole log file in one call
/// without going through `append`.
#[cfg(any(test, feature = "test-fixtures"))]
async fn write_with_fd_retry(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let contents = contents.as_ref();
    for attempt in 0..FILE_OPEN_RETRY_ATTEMPTS {
        match fs::write(path, contents).await {
            Ok(()) => return Ok(()),
            Err(err) if is_fd_pressure(&err) && attempt + 1 < FILE_OPEN_RETRY_ATTEMPTS => {
                sleep_fd_retry(attempt).await;
            },
            Err(err) => return Err(err),
        }
    }
    unreachable!("file-write retry loop always returns on final attempt")
}

/// Reject scope components that could escape the scopes/ subtree via
/// path traversal or that contain reserved filesystem characters. Any
/// rejected value is routed to `system/system` so the data still lands
/// somewhere readable rather than disappearing silently. The allowed
/// charset matches what real principal/workspace values use today
/// (UUIDs, slugs, short ids) and deliberately excludes `/`, `\`, null,
/// leading `.` (which would let `..` slip past), and whitespace.
fn is_safe_scope_component(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 {
        return false;
    }
    if value.starts_with('.') {
        return false;
    }
    value
        .chars()
        .all(|c| matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '-'))
}

/// Where a scope's transport log actually lives on disk.
///
/// The one authority on that question. The path is not
/// `base_root/scopes/<principal>/<workspace>/events.jsonl` for every scope:
/// `is_safe_scope_component` is stricter than the workspace layout's own
/// segment sanitizer, so a principal like `user.name` — fine as a directory
/// name, and where `ArtifactV2Workspace::scope_root` puts it — is routed here
/// to `_quarantine/_quarantine` instead. Anything that wants to stat, size or
/// display this file has to resolve it the same way the writer does, or it
/// silently reports a path that does not exist.
pub fn scope_log_path(base_root: &Path, principal: &str, workspace: &str) -> PathBuf {
    let (safe_principal, safe_workspace) = sanitize_scope_pair(principal, workspace);
    base_root
        .join("scopes")
        .join(safe_principal)
        .join(safe_workspace)
        .join(LOG_FILENAME)
}

/// How many distinct quarantined scope pairs are worth naming individually
/// before the warning collapses into a single "and others" line. The set is
/// keyed by strings that can originate in an event payload, so it needs a
/// ceiling or a hostile emitter could grow it without bound.
const QUARANTINE_REPORT_CAP: usize = 64;

/// How much of a rejected scope component is worth keeping to identify it.
///
/// The entry *count* is capped, which bounds how many strings are retained but
/// not how large each one is: an emitter sending one 10MB principal is inside
/// the 64-entry cap and still resident for the life of the process, and the
/// warn line prints the whole thing. Both halves of the key come from event
/// payloads, so both need a ceiling.
///
/// Sized against `is_safe_scope_component`'s own 128-byte limit, which is what
/// real principal/workspace values fit inside. Anything longer was going to be
/// rejected on length alone, so the truncated form loses nothing an operator
/// could act on — and truncation is applied to the *stored* key, not just the
/// printed one, so two hostile values sharing a prefix collapse into one entry
/// rather than each claiming a slot.
const QUARANTINE_REPORT_MAX_COMPONENT_LEN: usize = 128;

/// Truncate on a character boundary, marking the result when it was cut.
///
/// `String::truncate` panics mid-codepoint, and these strings are arbitrary
/// UTF-8 from a payload. The marker matters because a truncated value must not
/// read as the whole value in a log line an operator is trying to match against
/// a config.
fn bounded_scope_component(value: &str) -> String {
    if value.len() <= QUARANTINE_REPORT_MAX_COMPONENT_LEN {
        return value.to_string();
    }
    let mut end = QUARANTINE_REPORT_MAX_COMPONENT_LEN;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…(truncated)", &value[..end])
}

/// Warn about a quarantined scope the first time that exact pair is seen.
///
/// The routing decision is per *call*, but the fact worth telling an operator
/// is per *scope*, and the two are wildly different rates. `sanitize_scope_pair`
/// is reached from two places: the write path, which runs once per emitted
/// event, and `scope_log_path`, which `/storage` calls on every snapshot. A
/// single principal like `user.name` therefore produced a warn line per event
/// and another per page refresh — thousands of identical lines saying one thing,
/// which is how a real signal becomes something operators filter out.
///
/// Warning once per distinct pair keeps the signal ("this scope's events are
/// being written somewhere other than its own directory") and drops the
/// repetition, which carries no information after the first.
fn report_quarantined_scope_once(principal: &str, workspace: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static REPORTED: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let reported = REPORTED.get_or_init(|| Mutex::new(HashSet::new()));
    // A poisoned lock here must not take the write path down with it: the
    // caller's job is to route an event to disk, and this is only reporting.
    let Ok(mut seen) = reported.lock() else {
        return;
    };
    if seen.len() > QUARANTINE_REPORT_CAP {
        return;
    }
    // Bounded in both dimensions before anything is retained: the cap above
    // bounds how many keys live here, this bounds how large each one can be.
    // Only the count was bounded, and these strings come from event payloads.
    let principal = bounded_scope_component(principal);
    let workspace = bounded_scope_component(workspace);
    if !seen.insert((principal.clone(), workspace.clone())) {
        return;
    }
    if seen.len() > QUARANTINE_REPORT_CAP {
        warn!(
            distinct_scopes = seen.len(),
            "transport_log: too many distinct unsafe scopes to name individually; \
             further quarantine routing will not be reported"
        );
        return;
    }
    warn!(
        principal = %principal,
        workspace = %workspace,
        "transport_log: routing events with unsafe scope to quarantine bucket (reported once per scope)"
    );
}

/// Coerce a (principal, workspace) pair through `is_safe_scope_component`,
/// substituting the **quarantine bucket** for anything unsafe. Used at
/// every on-disk path construction site so untrusted event-payload
/// strings can never influence the filesystem layout. Earlier code
/// folded unsafe values into `system/system`, but an operator querying
/// the system bucket then saw cross-tenant payloads that only landed
/// there because their scope strings were unsafe — a small but real
/// information-leak hazard. Routing to `_quarantine/_quarantine`
/// keeps the data readable for diagnostics without conflating it with
/// genuine system traffic.
fn sanitize_scope_pair(principal: &str, workspace: &str) -> (String, String) {
    let principal_safe = is_safe_scope_component(principal);
    let workspace_safe = is_safe_scope_component(workspace);
    if !principal_safe || !workspace_safe {
        report_quarantined_scope_once(principal, workspace);
        return (
            QUARANTINE_PRINCIPAL.to_string(),
            QUARANTINE_WORKSPACE.to_string(),
        );
    }
    (principal.to_string(), workspace.to_string())
}

#[derive(Deserialize)]
struct AppOwnerNotificationContextProbe {
    #[serde(default)]
    app_owner_notification: bool,
}

#[derive(Deserialize)]
struct AppOwnerNotificationSchemaProbe {
    #[serde(default)]
    context: Option<AppOwnerNotificationContextProbe>,
}

#[derive(Deserialize)]
struct AppOwnerNotificationDataProbe {
    #[serde(default)]
    input_schema: Option<AppOwnerNotificationSchemaProbe>,
    #[serde(default)]
    source: String,
}

#[derive(Deserialize)]
struct AppOwnerNotificationEventTypeProbe {
    event_type: String,
}

#[derive(Deserialize)]
struct AppOwnerNotificationTransportProbe {
    #[serde(default)]
    data: Option<AppOwnerNotificationDataProbe>,
}

#[derive(Deserialize)]
struct AppOwnerNotificationCanonicalProbe {
    #[serde(default)]
    payload: Option<AppOwnerNotificationDataProbe>,
}

fn app_owner_notification_marker(data: &AppOwnerNotificationDataProbe) -> bool {
    data.input_schema
        .as_ref()
        .and_then(|schema| schema.context.as_ref())
        .is_some_and(|context| context.app_owner_notification)
}

/// Classify a JSONL row through the minimum closed projection needed to find
/// the host-sealed notification marker. `None` means the row is not valid JSON
/// in its transport or canonical execution envelope shape and must be omitted
/// by the privacy scrub: retaining an unclassifiable row could retain a damaged
/// notification body.
fn classify_serialized_app_owner_notification_request(line: &str) -> Option<bool> {
    let event_type = serde_json::from_str::<AppOwnerNotificationEventTypeProbe>(line)
        .ok()?
        .event_type;
    match event_type.as_str() {
        "HitlRequested" | "hitl_requested" => {
            let probe = serde_json::from_str::<AppOwnerNotificationTransportProbe>(line).ok()?;
            Some(app_owner_notification_marker(probe.data.as_ref()?))
        },
        "hitl.requested" => {
            let probe = serde_json::from_str::<AppOwnerNotificationCanonicalProbe>(line).ok()?;
            Some(app_owner_notification_marker(probe.payload.as_ref()?))
        },
        _ => Some(false),
    }
}

fn classify_serialized_app_owner_notification_transport_event(line: &str) -> Option<bool> {
    let event_type = serde_json::from_str::<AppOwnerNotificationEventTypeProbe>(line)
        .ok()?
        .event_type;
    match event_type.as_str() {
        "HitlRequested" | "hitl_requested" => {
            let probe = serde_json::from_str::<AppOwnerNotificationTransportProbe>(line).ok()?;
            Some(app_owner_notification_marker(probe.data.as_ref()?))
        },
        "HitlResolved" | "hitl_resolved" => {
            let probe = serde_json::from_str::<AppOwnerNotificationTransportProbe>(line).ok()?;
            Some(probe.data.as_ref()?.source == "app_owner_notification")
        },
        "hitl.requested" => {
            let probe = serde_json::from_str::<AppOwnerNotificationCanonicalProbe>(line).ok()?;
            Some(app_owner_notification_marker(probe.payload.as_ref()?))
        },
        "hitl.resolved" => {
            let probe = serde_json::from_str::<AppOwnerNotificationCanonicalProbe>(line).ok()?;
            Some(probe.payload.as_ref()?.source == "app_owner_notification")
        },
        _ => Some(false),
    }
}

/// Detect the host-sealed app notification marker without deserializing the
/// arbitrary prompt/schema body. Such requests are durably owned by
/// UserRequestService and the expiry-aware HITL lifecycle; the generic
/// workspace observability log must never become a third, count-floor-retained
/// content owner. A structurally valid marked row is removed even when fields
/// outside this shallow projection are unknown to this process version.
pub fn serialized_event_is_app_owner_notification_request(line: &str) -> bool {
    classify_serialized_app_owner_notification_request(line).unwrap_or(false)
}

/// Serialized counterpart of `is_app_owner_notification_transport_event` for
/// transport and canonical execution logs/backfills that must reject the
/// sealed request and its dedicated bodyless resolution before allocating the
/// full runtime enum.
pub fn serialized_event_is_app_owner_notification_transport_event(line: &str) -> bool {
    classify_serialized_app_owner_notification_transport_event(line).unwrap_or(false)
}

/// Decide whether one serialized transport-log row is safe to expose through
/// a generic public backfill. Unlike the positive classifier above, this is a
/// fail-closed boundary: a row which cannot be decoded through the minimum
/// transport or canonical execution envelope is withheld. The streaming
/// scrubber applies the same rule, so an unreadable legacy row cannot become
/// temporarily observable just because a public backfill races ahead of
/// physical startup scrubbing.
pub fn serialized_event_is_safe_for_generic_backfill(line: &str) -> bool {
    matches!(
        classify_serialized_app_owner_notification_transport_event(line),
        Some(false)
    )
}

/// Incremental, memory-bounded JSONL privacy transform. It retains ordinary
/// rows byte-for-byte, drops marked notification requests, and drops only the
/// individual row when it is malformed or exceeds the per-row ceiling.
struct AppOwnerNotificationScrubber {
    line: Vec<u8>,
    output: Vec<u8>,
    dropping_oversized_line: bool,
    digest: Sha256,
    removed: usize,
    omitted_malformed: usize,
}

/// Cancellation-safe ownership for a unique scrub staging sibling. The
/// staging file contains only already-classified ordinary rows, but abandoned
/// siblings would otherwise accumulate without bound across cancelled startup
/// or periodic passes.
struct AppOwnerNotificationScrubStaging {
    path: PathBuf,
}

impl AppOwnerNotificationScrubStaging {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for AppOwnerNotificationScrubStaging {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != io::ErrorKind::NotFound {
                warn!(
                    path = %self.path.display(),
                    error = %error,
                    "transport_log: failed to remove content-free notification scrub staging file"
                );
            }
        }
    }
}

impl AppOwnerNotificationScrubber {
    fn new() -> Self {
        Self {
            line: Vec::new(),
            output: Vec::with_capacity(APP_OWNER_NOTIFICATION_SCRUB_OUTPUT_BYTES),
            dropping_oversized_line: false,
            digest: Sha256::new(),
            removed: 0,
            omitted_malformed: 0,
        }
    }

    fn consume(&mut self, input: &[u8]) {
        for segment in input.split_inclusive(|byte| *byte == b'\n') {
            let terminated = segment.last() == Some(&b'\n');
            let body = if terminated {
                &segment[..segment.len().saturating_sub(1)]
            } else {
                segment
            };

            if self.dropping_oversized_line {
                if terminated {
                    self.dropping_oversized_line = false;
                }
                continue;
            }

            if self.line.len().saturating_add(body.len())
                > MAX_APP_OWNER_NOTIFICATION_SCRUB_LINE_BYTES
            {
                self.line.clear();
                self.dropping_oversized_line = !terminated;
                self.omitted_malformed = self.omitted_malformed.saturating_add(1);
                continue;
            }
            self.line.extend_from_slice(body);
            if terminated {
                self.finish_line(true);
            }
        }
    }

    fn finish_line(&mut self, terminated: bool) {
        if self.line.is_empty() {
            if terminated {
                self.output.push(b'\n');
            }
            return;
        }
        let keep = match std::str::from_utf8(&self.line)
            .ok()
            .and_then(classify_serialized_app_owner_notification_transport_event)
        {
            Some(true) => {
                self.removed = self.removed.saturating_add(1);
                false
            },
            Some(false) => true,
            None => {
                self.omitted_malformed = self.omitted_malformed.saturating_add(1);
                false
            },
        };
        if keep {
            self.output.extend_from_slice(&self.line);
            if terminated {
                self.output.push(b'\n');
            }
        }
        self.line.clear();
    }

    async fn flush(&mut self, staging: &mut tokio::fs::File) -> anyhow::Result<()> {
        if self.output.is_empty() {
            return Ok(());
        }
        staging.write_all(&self.output).await?;
        self.digest.update(&self.output);
        self.output.clear();
        Ok(())
    }

    async fn finish(&mut self, staging: &mut tokio::fs::File) -> anyhow::Result<()> {
        if self.dropping_oversized_line {
            self.dropping_oversized_line = false;
        } else if !self.line.is_empty() {
            self.finish_line(false);
        }
        self.flush(staging).await
    }

    fn should_flush(&self) -> bool {
        self.output.len() >= APP_OWNER_NOTIFICATION_SCRUB_OUTPUT_BYTES
    }
}

/// One append-only event log per (principal, workspace).
#[derive(Debug)]
pub struct WorkspaceEventLog {
    path: PathBuf,
    workspace_layout: Option<ArtifactV2Workspace>,
    /// Serializes appenders so concurrent emit sites don't interleave
    /// half-written JSON lines. The OS would atomically write a single
    /// `write_all` < PIPE_BUF, but we don't rely on size.
    write_lock: Arc<Mutex<()>>,
    /// Legacy app-owner notification bodies are scrubbed once per opened log.
    /// New producer rows are refused before append, so a successful pass never
    /// needs repeating within this process.
    app_owner_notification_scrubbed: AtomicBool,
}

impl WorkspaceEventLog {
    pub fn new(scope_root: &Path) -> Self {
        Self {
            path: scope_root.join(LOG_FILENAME),
            workspace_layout: None,
            write_lock: Arc::new(Mutex::new(())),
            app_owner_notification_scrubbed: AtomicBool::new(false),
        }
    }

    pub fn with_workspace_layout(scope_root: &Path, workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            path: scope_root.join(LOG_FILENAME),
            workspace_layout: Some(workspace_layout),
            write_lock: Arc::new(Mutex::new(())),
            app_owner_notification_scrubbed: AtomicBool::new(false),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a serialized event line (no trailing newline — added
    /// here). Creates parent directories on first write.
    ///
    /// `fallback_timestamp_ms` is the broadcaster-receive time the
    /// writer task captured when this event came off the channel. If
    /// the serialized line doesn't already carry a parseable timestamp
    /// at a known extractor location (top-level `timestamp` /
    /// `timestamp_ms`, or `data.timestamp` / `data.timestamp_ms`), the
    /// fallback is spliced in as a top-level `timestamp_ms` so retention
    /// scans and cross-scope-backfill sort never collapse to `i64::MIN`
    /// or skip rows whose variant happens to lack a timestamp field.
    pub async fn append(&self, line: &str, fallback_timestamp_ms: i64) -> anyhow::Result<()> {
        let mut payload = ensure_top_level_timestamp(line, fallback_timestamp_ms);
        // Single combined write below — assemble payload + newline up
        // front so the actual `write_all` is one syscall. Two separate
        // `write_all`s used to be the source of mid-line tearing in
        // the lockless-read alternative; collapsing to one keeps the
        // line atomic from the kernel's perspective for any
        // `len < PIPE_BUF` (≥512 on every POSIX target). Larger lines
        // remain non-atomic at the kernel boundary, but the write_lock
        // serializes them so concurrent appenders still don't tear.
        if !payload.ends_with('\n') {
            payload.push('\n');
        }
        let _guard = self.write_lock.lock().await;
        let _cross_process_guard = AgentStorage::acquire_file_lock_exclusive(&self.path)
            .await
            .with_context(|| format!("locking workspace event log {:?}", self.path))?;
        if let Some(workspace_layout) = &self.workspace_layout {
            workspace_layout
                .append_path(&self.path, payload.as_bytes())
                .await
                .with_context(|| format!("appending workspace event log {:?}", self.path))?;
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating workspace event log dir {parent:?}"))?;
        }
        let mut file = open_append_with_fd_retry(&self.path)
            .await
            .with_context(|| format!("opening workspace event log {:?}", self.path))?;
        file.write_all(payload.as_bytes()).await?;
        // tokio::fs::File::write_all queues the write on the blocking
        // pool and returns before the kernel-level write completes —
        // dropping the File without flushing leaves the bytes in the
        // internal buffer. Under load the next reader (sync read, or
        // a re-open from the compactor) can race ahead of the queued
        // write and observe a partial / missing tail. Flush blocks
        // until the in-flight write task finishes so subsequent reads
        // see the bytes.
        file.flush().await?;
        Ok(())
    }

    /// Remove every legacy app-owner notification request body regardless of
    /// age or count-floor retention. Current writers never append these rows;
    /// this one-time streaming atomic rewrite migrates quiet pre-fix scope logs
    /// without ever materializing or discarding their complete ordinary
    /// history. Memory is bounded by one read page plus one admitted JSONL row.
    pub async fn scrub_app_owner_notification_payloads(&self) -> anyhow::Result<usize> {
        if self.app_owner_notification_scrubbed.load(Ordering::Acquire) {
            return Ok(0);
        }
        let write_guard = Arc::clone(&self.write_lock).lock_owned().await;
        let cross_process_guard = AgentStorage::acquire_file_lock_exclusive(&self.path)
            .await
            .with_context(|| format!("locking workspace event log {:?}", self.path))?;
        if self.app_owner_notification_scrubbed.load(Ordering::Acquire) {
            return Ok(0);
        }
        let observed_len = if let Some(workspace_layout) = &self.workspace_layout {
            workspace_layout
                .metadata_path(&self.path)
                .await
                .with_context(|| format!("statting {:?}", self.path))?
                .map(|metadata| metadata.len())
        } else {
            match fs::metadata(&self.path).await {
                Ok(metadata) => Some(metadata.len()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error).with_context(|| format!("statting {:?}", self.path));
                },
            }
        };
        let Some(observed_len) = observed_len else {
            self.app_owner_notification_scrubbed
                .store(true, Ordering::Release);
            return Ok(0);
        };

        let staging_path = self.path.with_file_name(format!(
            ".events-notification-scrub-{}.tmp",
            Uuid::new_v4().simple()
        ));
        // Synchronous create-new is a tiny metadata operation and closes the
        // cancellation gap of Tokio's blocking-pool file-open future: once the
        // path exists, the RAII owner is installed before any await can yield.
        let staging_file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staging_path)
            .with_context(|| format!("creating scrub staging file {staging_path:?}"))?;
        let _staging_cleanup = AppOwnerNotificationScrubStaging::new(staging_path.clone());
        let mut staging = tokio::fs::File::from_std(staging_file);
        let transform = async {
            let mut scrubber = AppOwnerNotificationScrubber::new();
            let mut source_digest = Sha256::new();

            if let Some(workspace_layout) = &self.workspace_layout {
                let mut offset = 0u64;
                while offset < observed_len {
                    let requested = APP_OWNER_NOTIFICATION_SCRUB_READ_BYTES
                        .min(observed_len.saturating_sub(offset));
                    let page = workspace_layout
                        .read_range_path(&self.path, offset, requested)
                        .await
                        .with_context(|| {
                            format!("stream-reading {:?} at offset {offset}", self.path)
                        })?;
                    if page.is_empty() {
                        return Err(anyhow!(
                            "workspace event log ended before the observed length while scrubbing"
                        ));
                    }
                    offset = offset.saturating_add(page.len() as u64);
                    source_digest.update(&page);
                    scrubber.consume(&page);
                    if scrubber.should_flush() {
                        scrubber.flush(&mut staging).await?;
                    }
                }
            } else {
                let mut source = open_read_with_fd_retry(&self.path)
                    .await
                    .with_context(|| format!("opening {:?}", self.path))?;
                let mut remaining = observed_len;
                let mut page = vec![0u8; APP_OWNER_NOTIFICATION_SCRUB_READ_BYTES as usize];
                while remaining > 0 {
                    let requested = remaining.min(page.len() as u64) as usize;
                    let read = source
                        .read(&mut page[..requested])
                        .await
                        .with_context(|| format!("stream-reading {:?}", self.path))?;
                    if read == 0 {
                        return Err(anyhow!(
                            "workspace event log ended before the observed length while scrubbing"
                        ));
                    }
                    remaining = remaining.saturating_sub(read as u64);
                    source_digest.update(&page[..read]);
                    scrubber.consume(&page[..read]);
                    if scrubber.should_flush() {
                        scrubber.flush(&mut staging).await?;
                    }
                }
            }
            scrubber.finish(&mut staging).await?;
            staging.flush().await?;
            staging.sync_all().await?;

            // A non-cooperating older process does not share the in-memory or
            // advisory lock. Refuse an obvious length change here; the exact
            // digest/identity CAS at publication also catches same-length
            // replacement before any row can be lost.
            let current_len = if let Some(workspace_layout) = &self.workspace_layout {
                workspace_layout
                    .metadata_path(&self.path)
                    .await
                    .with_context(|| format!("restatting {:?}", self.path))?
                    .map(|metadata| metadata.len())
            } else {
                match fs::metadata(&self.path).await {
                    Ok(metadata) => Some(metadata.len()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(error).with_context(|| format!("restatting {:?}", self.path));
                    },
                }
            };
            if current_len != Some(observed_len) {
                return Err(anyhow!(
                    "workspace event log changed length while notification scrub was in progress"
                ));
            }
            Ok::<_, anyhow::Error>((scrubber, format!("{:x}", source_digest.finalize())))
        }
        .await;
        let (scrubber, expected_destination_sha256) = match transform {
            Ok(transformed) => transformed,
            Err(error) => return Err(error),
        };
        drop(staging);
        let removed = scrubber.removed;
        let omitted_malformed = scrubber.omitted_malformed;
        if removed == 0 && omitted_malformed == 0 {
            let destination_path = self.path.clone();
            tokio::task::spawn_blocking(move || {
                let _write_guard = write_guard;
                let _cross_process_guard = cross_process_guard;
                ensure_file_matches_sync(
                    &destination_path,
                    observed_len,
                    &expected_destination_sha256,
                )
            })
            .await
            .context("joining notification scrub source verification")?
            .with_context(|| format!("verifying scrubbed source {:?}", self.path))?;
            self.app_owner_notification_scrubbed
                .store(true, Ordering::Release);
            return Ok(0);
        }

        let expected_sha256 = format!("{:x}", scrubber.digest.finalize());
        if let Some(workspace_layout) = &self.workspace_layout {
            let workspace_layout = workspace_layout.clone();
            let staged_path = staging_path.clone();
            let destination_path = self.path.clone();
            tokio::task::spawn_blocking(move || {
                let _write_guard = write_guard;
                let _cross_process_guard = cross_process_guard;
                workspace_layout
                    .copy_workspace_file_verified_atomic_path_sync_if_destination_matches(
                        &staged_path,
                        &destination_path,
                        &expected_sha256,
                        observed_len,
                        &expected_destination_sha256,
                    )
            })
            .await
            .context("joining provider-owned notification scrub publication")?
            .with_context(|| format!("publishing scrubbed {:?}", self.path))?;
        } else {
            let staged_path = staging_path.clone();
            let destination_path = self.path.clone();
            tokio::task::spawn_blocking(move || {
                let _write_guard = write_guard;
                let _cross_process_guard = cross_process_guard;
                publish_staged_file_if_destination_matches_sync(
                    &staged_path,
                    &destination_path,
                    &expected_sha256,
                    observed_len,
                    &expected_destination_sha256,
                )
            })
            .await
            .context("joining durable notification scrub publication")?
            .with_context(|| format!("publishing scrubbed {:?}", self.path))?;
        }
        if omitted_malformed > 0 {
            warn!(
                path = %self.path.display(),
                omitted_malformed,
                max_line_bytes = MAX_APP_OWNER_NOTIFICATION_SCRUB_LINE_BYTES,
                "transport_log: omitted malformed or oversized rows during notification privacy scrub"
            );
        }
        self.app_owner_notification_scrubbed
            .store(true, Ordering::Release);
        Ok(removed)
    }

    /// Trim the log to the retention bounds: keep events whose timestamp
    /// is within the window OR whose position is in the newest count
    /// floor, whichever yields more rows. Published atomically through the
    /// shared durable writer (unique staging file, `sync_all`, rename,
    /// parent-directory `sync_all`).
    ///
    /// Lock discipline: the entire compaction (read → compute → write
    /// → rename) runs under `write_lock` so no in-flight appender can
    /// race the file swap. The previous lock-split design — reading
    /// without the lock and re-acquiring only for the swap — left
    /// two races on the table:
    ///   1. `OpenOptions{append:true}` + two `write_all` calls
    ///      (payload, then `\n`) is not atomic across writes; the
    ///      lockless read could capture a partially-written line.
    ///   2. After `fs::rename`, an appender that already opened its
    ///      fd before the swap would write to the renamed-away
    ///      inode — the events silently disappeared into an orphan
    ///      file the OS reaps on fd close.
    /// Holding the lock for the whole compact pays a bounded stall
    /// (broadcaster lag during compaction) but is correct. The
    /// 5-minute compaction interval and small per-file size make
    /// this stall negligible in practice.
    pub async fn compact(&self) -> anyhow::Result<CompactionStats> {
        // Lockless short-circuit — stat the file first and bail without
        // acquiring `write_lock` when the byte-size is so small the
        // file *cannot* contain `EVENTS_RETENTION_MIN_COUNT` lines,
        // regardless of how short each line is. The minimum plausible
        // serialized event is something like `{"e":"x","t":1}\n` ≈ 16
        // bytes; we cap the heuristic at 16 bytes/row so any file
        // below `EVENTS_RETENTION_MIN_COUNT * 16` is guaranteed
        // under-floor (compaction cannot do work). Larger files —
        // including synthetic-test rows in the 30-40 byte range —
        // fall through to the locked streaming-read path.
        //
        // Earlier iterations of this heuristic used 256 / 192 bytes
        // (assuming the ~200-byte production row average) and
        // silently skipped tests writing 33-byte synthetic rows
        // even when those tests intentionally exceeded the count
        // floor. The tighter `* 16` ceiling restores correctness
        // (under-floor is *truly* under-floor) at the cost of
        // skipping the lock only in the genuinely-empty case —
        // which is still the common case for quiet workspaces.
        let metadata = if let Some(workspace_layout) = &self.workspace_layout {
            workspace_layout
                .metadata_path(&self.path)
                .await
                .with_context(|| format!("stat {:?}", self.path))?
        } else {
            match fs::metadata(&self.path).await {
                Ok(meta) => Some(meta),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => return Err(err).with_context(|| format!("stat {:?}", self.path)),
            }
        };
        let Some(meta) = metadata else {
            return Ok(CompactionStats::default());
        };
        let size_floor = (EVENTS_RETENTION_MIN_COUNT as u64).saturating_mul(16);
        if meta.len() <= size_floor {
            return Ok(CompactionStats::default());
        }

        // Past the heuristic — the file *might* need compaction. Acquire
        // the write_lock for the whole read → decide → write → rename
        // sequence. Lock-discipline rationale: see the original design
        // note (TOCTOU between lockless read and fd-already-open
        // appender writing into the renamed-away inode).
        //
        // MEMORY: the BufReader below streams the *read*, but every line is
        // retained in `lines`, so peak memory is the whole file — as one
        // `String` per row rather than one contiguous allocation, which is
        // the only thing that changed when this replaced `fs::read`. An
        // earlier version of this comment claimed the read was capped "to
        // one line at a time regardless of file size"; it is not, and
        // sizing decisions must not be made against that claim. Retention
        // keeps a 24h window (`keep_count` below), so this allocates 24h of
        // whatever the process emits. That was small while only lifecycle
        // events were persisted; it is not small now that the `Activity`
        // family puts every INFO+ log line on the bus. Making this O(1)
        // needs a two-pass streaming rewrite — count in pass one, copy the
        // tail in pass two — which the `workspace_layout` branch above
        // cannot do until that abstraction grows a streaming read.
        use tokio::io::{AsyncBufReadExt, BufReader};
        let _guard = self.write_lock.lock().await;
        let _cross_process_guard = AgentStorage::acquire_file_lock_exclusive(&self.path)
            .await
            .with_context(|| format!("locking workspace event log {:?}", self.path))?;

        let cutoff = chrono::Utc::now().timestamp_millis() - EVENTS_RETENTION_WINDOW_MS;

        // ── Pass 1: count. Retains no line content. ──
        //
        // The file is deliberately not materialised into a `Vec<String>`
        // here. Retention keeps a 24h window, and since the `Activity`
        // family put every INFO+ log line on the bus that window is the
        // whole log stream — so materialising it cost memory proportional
        // to a day of logs, once every `COMPACTION_INTERVAL`, and
        // overwhelmingly just to discover there was nothing to do: the
        // steady state is `keep_count >= total_before`, which returns below
        // without writing anything. Counting first makes that common case
        // allocate nothing, and pays for the retained tail only when the
        // file genuinely needs rewriting.
        //
        // Reading twice is safe under `write_lock` plus the advisory file
        // lock: every current-process and cooperating cross-process appender
        // takes the same pair before mutating this destination.
        let body = match &self.workspace_layout {
            // This path has no streaming read, so it holds the one `String`
            // it can get and walks it twice rather than adding a `Vec` of
            // per-line allocations on top of it.
            Some(workspace_layout) => Some(
                workspace_layout
                    .read_to_string_path(&self.path)
                    .await
                    .with_context(|| format!("reading {:?}", self.path))?,
            ),
            None => None,
        };

        let (total_before, window_count) = if let Some(body) = &body {
            let mut total = 0usize;
            let mut in_window = 0usize;
            for line in body.lines().filter(|line| !line.is_empty()) {
                total += 1;
                if read_event_timestamp_ms(line).is_some_and(|ts| ts >= cutoff) {
                    in_window += 1;
                }
            }
            (total, in_window)
        } else {
            let file = match open_read_with_fd_retry(&self.path).await {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(CompactionStats::default());
                },
                Err(err) => return Err(err).with_context(|| format!("opening {:?}", self.path)),
            };
            let mut total = 0usize;
            let mut in_window = 0usize;
            let mut lines_iter = BufReader::new(file).lines();
            while let Some(line) = lines_iter
                .next_line()
                .await
                .with_context(|| format!("reading {:?}", self.path))?
            {
                if line.is_empty() {
                    continue;
                }
                total += 1;
                if read_event_timestamp_ms(&line).is_some_and(|ts| ts >= cutoff) {
                    in_window += 1;
                }
            }
            (total, in_window)
        };
        if total_before <= EVENTS_RETENTION_MIN_COUNT {
            return Ok(CompactionStats {
                kept: total_before,
                dropped: 0,
                total_before,
            });
        }

        // `window_count` above counts every row whose timestamp falls within
        // the rolling window. The earlier reverse-scan-and-break design
        // assumed chronologically ordered persistence and terminated the
        // scan on the first parsed-too-old row — but with multi-writer
        // out-of-order arrival (broadcaster delivery isn't strictly
        // monotonic, especially under back-pressure), one stale row could
        // mask many fresh ones behind it and silently truncate the retention
        // window back to the MIN_COUNT floor. Walking the entire file gives
        // the true window count and only adds an O(N) parse pass on a
        // compactor that already runs once per 5 minutes.
        let keep_count = window_count.max(EVENTS_RETENTION_MIN_COUNT);
        if keep_count >= total_before {
            return Ok(CompactionStats {
                kept: total_before,
                dropped: 0,
                total_before,
            });
        }
        // ── Pass 2: copy the retained tail. ──
        //
        // Only reached when the file genuinely needs rewriting. `payload`
        // holds the kept rows because the durable writers below take a
        // finished byte slice; that is still bounded by the retention
        // window rather than by the whole file, and it is no longer paid on
        // every no-op compaction.
        let skip = total_before - keep_count;
        let mut payload = String::new();
        if let Some(body) = &body {
            for line in body.lines().filter(|line| !line.is_empty()).skip(skip) {
                payload.push_str(line);
                payload.push('\n');
            }
        } else {
            let file = match open_read_with_fd_retry(&self.path).await {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(CompactionStats::default());
                },
                Err(err) => return Err(err).with_context(|| format!("opening {:?}", self.path)),
            };
            let mut lines_iter = BufReader::new(file).lines();
            let mut seen = 0usize;
            while let Some(line) = lines_iter
                .next_line()
                .await
                .with_context(|| format!("reading {:?}", self.path))?
            {
                if line.is_empty() {
                    continue;
                }
                seen += 1;
                if seen <= skip {
                    continue;
                }
                payload.push_str(&line);
                payload.push('\n');
            }
        }
        if let Some(workspace_layout) = &self.workspace_layout {
            workspace_layout
                .write_string_atomic_path(&self.path, &payload)
                .await
                .with_context(|| format!("writing compacted {:?}", self.path))?;
        } else {
            // The shared durable writer, not a fixed `<log>.jsonl.tmp`
            // sibling. Compaction is the one place this log is rewritten
            // whole, so a torn publish here costs the retained history rather
            // than one event; the writer stages through a per-write unique
            // name (`write_lock` orders compactions within a process, but the
            // fixed name was also shared with any other process compacting the
            // same scope) and `sync_all`s both the file and the scope
            // directory, neither of which the tmp write did.
            write_bytes_durably(&self.path, payload.as_bytes())
                .await
                .with_context(|| format!("writing compacted {:?}", self.path))?;
        }
        Ok(CompactionStats {
            kept: keep_count,
            dropped: total_before - keep_count,
            total_before,
        })
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CompactionStats {
    pub kept: usize,
    pub dropped: usize,
    pub total_before: usize,
}

/// Registry of one log per (principal, workspace). Lazily creates a
/// log on first append for a new scope. Cheap clone (just `Arc`s).
#[derive(Debug, Clone, Default)]
pub struct WorkspaceEventLogRegistry {
    inner: Arc<WorkspaceEventLogRegistryInner>,
}

#[derive(Debug, Default)]
struct WorkspaceEventLogRegistryInner {
    base_root: PathBuf,
    workspace_layout: Option<ArtifactV2Workspace>,
    logs: Mutex<HashMap<(String, String), Arc<WorkspaceEventLog>>>,
}

impl WorkspaceEventLogRegistry {
    pub fn new(base_root: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(WorkspaceEventLogRegistryInner {
                base_root: base_root.into(),
                workspace_layout: None,
                logs: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn with_workspace_layout(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            inner: Arc::new(WorkspaceEventLogRegistryInner {
                base_root: workspace_layout.base_root().to_path_buf(),
                workspace_layout: Some(workspace_layout),
                logs: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub async fn get_or_create(&self, principal: &str, workspace: &str) -> Arc<WorkspaceEventLog> {
        let (safe_principal, safe_workspace) = sanitize_scope_pair(principal, workspace);
        let key = (safe_principal.clone(), safe_workspace.clone());
        let mut guard = self.inner.logs.lock().await;
        if let Some(existing) = guard.get(&key) {
            return existing.clone();
        }
        let scope_root = self
            .inner
            .base_root
            .join("scopes")
            .join(&safe_principal)
            .join(&safe_workspace);
        let log = Arc::new(
            self.inner
                .workspace_layout
                .clone()
                .map(|layout| WorkspaceEventLog::with_workspace_layout(&scope_root, layout))
                .unwrap_or_else(|| WorkspaceEventLog::new(&scope_root)),
        );
        guard.insert(key, log.clone());
        log
    }

    /// Snapshot of every known log path. Used by the periodic compactor
    /// and by `cross_scope_backfill` to enumerate readable scopes
    /// without doing its own filesystem walk.
    pub async fn known_logs(&self) -> Vec<Arc<WorkspaceEventLog>> {
        self.inner.logs.lock().await.values().cloned().collect()
    }

    /// Discover scope logs written by an earlier process so the startup
    /// privacy scrub is not limited to scopes touched since this boot.
    ///
    /// Only the exact `scopes/<principal>/<workspace>/events.jsonl` layout is
    /// admitted. Symlinks and unsafe scope components are refused before a
    /// path is registered, preserving the writer's path-containment policy.
    pub async fn discover_existing_logs(&self) -> anyhow::Result<Vec<Arc<WorkspaceEventLog>>> {
        let scopes_root = self.inner.base_root.join("scopes");
        let mut principal_entries = match fs::read_dir(&scopes_root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {scopes_root:?}"));
            },
        };
        let mut discovered = Vec::new();
        while let Some(principal_entry) = principal_entries
            .next_entry()
            .await
            .with_context(|| format!("enumerating {scopes_root:?}"))?
        {
            let principal_type = principal_entry
                .file_type()
                .await
                .with_context(|| format!("statting {:?}", principal_entry.path()))?;
            if !principal_type.is_dir() || principal_type.is_symlink() {
                continue;
            }
            let Some(principal) = principal_entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_safe_scope_component(&principal) {
                continue;
            }
            let principal_path = principal_entry.path();
            let mut workspace_entries = match fs::read_dir(&principal_path).await {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| format!("reading {principal_path:?}"));
                },
            };
            while let Some(workspace_entry) = workspace_entries
                .next_entry()
                .await
                .with_context(|| format!("enumerating {principal_path:?}"))?
            {
                let workspace_type = workspace_entry
                    .file_type()
                    .await
                    .with_context(|| format!("statting {:?}", workspace_entry.path()))?;
                if !workspace_type.is_dir() || workspace_type.is_symlink() {
                    continue;
                }
                let Some(workspace) = workspace_entry.file_name().to_str().map(str::to_owned)
                else {
                    continue;
                };
                if !is_safe_scope_component(&workspace) {
                    continue;
                }
                let log_path = workspace_entry.path().join(LOG_FILENAME);
                let log_type = match fs::symlink_metadata(&log_path).await {
                    Ok(metadata) => metadata.file_type(),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(error).with_context(|| format!("statting {log_path:?}"));
                    },
                };
                if !log_type.is_file() || log_type.is_symlink() {
                    continue;
                }
                discovered.push(self.get_or_create(&principal, &workspace).await);
            }
        }
        Ok(discovered)
    }

    /// Log path for a (principal, workspace), creating no entries.
    /// Used by the cross-scope backfill to read on-disk logs even for
    /// scopes whose registry entry hasn't been touched in this process.
    pub fn path_for(&self, principal: &str, workspace: &str) -> PathBuf {
        scope_log_path(&self.inner.base_root, principal, workspace)
    }
}

/// Determine which (principal, workspace) bucket a runtime event
/// belongs to. Mirrors the routing rules in
/// `websocket_handler::event_visible_to_scope`: explicit scope wins;
/// missing scope routes to the system bucket so it's still readable
/// via an explicit `system/system` query.
pub fn route_event_to_scope(event: &RuntimeTransportEvent) -> (String, String) {
    if let Some((principal, workspace)) = explicit_scope(event) {
        return (principal.to_string(), workspace.to_string());
    }
    (SYSTEM_PRINCIPAL.to_string(), SYSTEM_WORKSPACE.to_string())
}

/// Splice a top-level `timestamp_ms` into a serialized event line when
/// the variant didn't carry one at any extractor location. Used by the
/// writer task so every persisted row has a parseable timestamp the
/// retention compactor and cross-scope-backfill sort can rely on.
///
/// The fast-path substring probe used to look anywhere in the line for
/// `"timestamp"` / `"timestamp_ms"` — but those substrings also match
/// when buried inside arbitrary string values (e.g. a `delta` field
/// containing the word `timestamp` in chat content). False positives
/// there caused real timestamp-less rows to slip past the injection
/// step, then land at `i64::MIN` ordering in cross-scope-backfill.
/// We now require the substring at a **structural** position — a JSON
/// key opens with `"timestamp":` or `"timestamp_ms":` preceded by
/// either `{` or `,` (with optional whitespace) — so payload strings
/// containing the word can't trigger a false skip.
///
/// Additionally, treat a structural `timestamp == 0` as "missing".
/// Several legacy variants serialize `timestamp: 0` when the emitter
/// didn't stamp a real value (e.g. `FeedItemCreated.timestamp` is
/// `#[serde(default)]` i64 → 0 for older persisted rows). Without
/// this we'd skip injection and the row would sort at epoch zero.
fn ensure_top_level_timestamp(line: &str, fallback_ms: i64) -> String {
    if has_structural_timestamp(line) {
        return line.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(serde_json::Value::Object(mut map)) => {
            map.insert(
                "timestamp_ms".to_string(),
                serde_json::Value::Number(fallback_ms.into()),
            );
            serde_json::to_string(&serde_json::Value::Object(map))
                .unwrap_or_else(|_| line.to_string())
        },
        _ => line.to_string(),
    }
}

/// True iff `line` carries a JSON key `"timestamp"` or `"timestamp_ms"`
/// at a structural position with a non-zero numeric / non-empty string
/// value. Matches only key positions (preceded by `{` or `,` plus
/// optional whitespace), so a string payload containing the literal
/// word `timestamp` doesn't trip a false positive. Zero-valued
/// numeric timestamps return false — the legacy `#[serde(default)]`
/// case where the row "has" a timestamp field that's actually unset.
fn has_structural_timestamp(line: &str) -> bool {
    for needle in ["\"timestamp\"", "\"timestamp_ms\""] {
        let mut search_from = 0usize;
        while let Some(rel) = line[search_from..].find(needle) {
            let pos = search_from + rel;
            let preceded_by_structural = line[..pos]
                .bytes()
                .rev()
                .find(|b| !b.is_ascii_whitespace())
                .is_some_and(|b| b == b'{' || b == b',');
            if preceded_by_structural {
                let after = &line[pos + needle.len()..];
                let after = after.trim_start();
                if let Some(rest) = after.strip_prefix(':') {
                    let value = rest.trim_start();
                    // Reject literal-zero numeric (the legacy "unset" case)
                    // and empty string values, since the extractor can't
                    // get a useful timestamp out of either.
                    if value.starts_with("0,") || value.starts_with("0}") {
                        // continue scanning for a later, real occurrence
                    } else if value.starts_with("\"\"") {
                        // ditto for empty string
                    } else {
                        return true;
                    }
                }
            }
            search_from = pos + needle.len();
        }
    }
    false
}

/// Reach into a serialized event JSON (already ndjson-encoded) and pull
/// out the canonical timestamp. Mirrors the layered extractor used by
/// `events_api::extract_event_timestamp_ms` and the frontend
/// `EventStreamCard.extractTimestamp`. Used by the compactor to decide
/// which tail rows fall within the retention window.
fn read_event_timestamp_ms(line: &str) -> Option<i64> {
    #[derive(Deserialize)]
    struct ProbeData {
        #[serde(default)]
        timestamp: Option<serde_json::Value>,
        #[serde(default)]
        timestamp_ms: Option<serde_json::Value>,
    }
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default)]
        timestamp: Option<serde_json::Value>,
        #[serde(default)]
        timestamp_ms: Option<serde_json::Value>,
        #[serde(default)]
        data: Option<ProbeData>,
    }
    let probe: Probe = match serde_json::from_str(line) {
        Ok(p) => p,
        Err(_) => return None,
    };
    let candidates = [
        probe.timestamp_ms,
        probe.timestamp,
        probe.data.as_ref().and_then(|d| d.timestamp_ms.clone()),
        probe.data.as_ref().and_then(|d| d.timestamp.clone()),
    ];
    for candidate in candidates.iter().flatten() {
        if let Some(n) = candidate.as_i64() {
            return Some(n);
        }
        if let Some(f) = candidate.as_f64() {
            return Some(f as i64);
        }
        if let Some(s) = candidate.as_str() {
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(s) {
                return Some(parsed.timestamp_millis());
            }
        }
    }
    None
}

/// Spawn a background subscriber that drains every event off the
/// broadcaster, routes each to its (principal, workspace) bucket, and
/// appends to the per-scope log. Plus a periodic compaction tick.
///
/// Run once at startup. Returns immediately; both tasks live for the
/// process lifetime.
pub fn spawn_workspace_event_log_writer(
    broadcaster: &crate::magician_v2::realtime_events::RuntimeTransportBroadcaster,
    registry: WorkspaceEventLogRegistry,
) {
    let mut rx = broadcaster.subscribe();
    let writer_registry = registry.clone();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    // Owner notifications are retained by UserRequestService
                    // until their absolute TTL and represented in the HITL
                    // lifecycle by content-free proof. Never copy their body
                    // into the generic workspace log, whose count floor can
                    // otherwise outlive that TTL indefinitely.
                    if crate::magician_v2::realtime_events::is_app_owner_notification_transport_event(
                        &event,
                    ) {
                        if matches!(&event, RuntimeTransportEvent::HitlRequested { .. })
                            && !matches!(
                                crate::magician_v2::realtime_events::app_owner_notification_expiry_ms(
                                    &event,
                                ),
                                Ok(Some(_))
                            )
                        {
                            warn!(
                                "transport_log: refusing malformed app-owner notification"
                            );
                        }
                        continue;
                    }
                    let (principal, workspace) = route_event_to_scope(&event);
                    let line = match serde_json::to_string(&event) {
                        Ok(s) => s,
                        Err(err) => {
                            debug!(error = %err, "transport_log: failed to serialize event");
                            continue;
                        },
                    };
                    // Captured at recv time so a variant that ships
                    // without a `timestamp` field still gets a usable
                    // millisecond stamp the retention compactor can
                    // order by. Drift between emit-time and recv-time
                    // is bounded by broadcaster latency (sub-ms in
                    // practice), so this is a faithful approximation.
                    let fallback_ms = chrono::Utc::now().timestamp_millis();
                    let log = writer_registry.get_or_create(&principal, &workspace).await;
                    if let Err(err) = log.append(&line, fallback_ms).await {
                        warn!(
                            principal = %principal,
                            workspace = %workspace,
                            error = %err,
                            "transport_log: append failed"
                        );
                    }
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    // Surface broadcaster lag as a structured event row
                    // in the system bucket so the gap is operator-
                    // visible on `/events` refresh — and as a `warn!`
                    // log (was `debug!`, which masked silent persistence
                    // loss). The sentinel mirrors the in-flight
                    // `__events_lagged__` produced by the live-tail
                    // forwarder in `events_api`, so the UI's existing
                    // gap-rendering applies uniformly to both paths.
                    warn!(
                        skipped,
                        "transport_log: broadcaster lagged — {skipped} events were dropped before reaching the on-disk log; refresh shows gap"
                    );
                    let fallback_ms = chrono::Utc::now().timestamp_millis();
                    let sentinel = serde_json::json!({
                        "event_type": "__transport_log_lagged__",
                        "timestamp_ms": fallback_ms,
                        "skipped": skipped,
                        "message": format!(
                            "Transport log writer fell behind the broadcaster; {skipped} \
                             events were dropped before persistence. Live-tail subscribers \
                             may have seen them, but a future refresh won't."
                        ),
                    });
                    if let Ok(line) = serde_json::to_string(&sentinel) {
                        let log = writer_registry
                            .get_or_create(SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE)
                            .await;
                        if let Err(err) = log.append(&line, fallback_ms).await {
                            warn!(
                                error = %err,
                                "transport_log: failed to persist lagged-sentinel row"
                            );
                        }
                    }
                },
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let compactor_registry = registry;
    tokio::spawn(async move {
        match compactor_registry.discover_existing_logs().await {
            Ok(logs) => {
                for log in logs {
                    match log.scrub_app_owner_notification_payloads().await {
                        Ok(removed) if removed > 0 => debug!(
                            path = %log.path().display(),
                            removed,
                            "transport_log: scrubbed app-owner notification bodies"
                        ),
                        Ok(_) => {},
                        Err(error) => warn!(
                            path = %log.path().display(),
                            %error,
                            "transport_log: startup notification scrub failed"
                        ),
                    }
                }
            },
            Err(error) => warn!(
                %error,
                "transport_log: failed to discover legacy logs for notification scrub"
            ),
        }
        let mut interval = tokio::time::interval(COMPACTION_INTERVAL);
        // First tick fires immediately; skip it so we don't compact on
        // boot before any events have landed.
        interval.tick().await;
        loop {
            interval.tick().await;
            for log in compactor_registry.known_logs().await {
                if let Err(error) = log.scrub_app_owner_notification_payloads().await {
                    warn!(
                        path = %log.path().display(),
                        %error,
                        "transport_log: notification scrub retry failed"
                    );
                }
                match log.compact().await {
                    Ok(stats) if stats.dropped > 0 => {
                        debug!(
                            path = %log.path().display(),
                            kept = stats.kept,
                            dropped = stats.dropped,
                            "transport_log: compacted"
                        );
                    },
                    Ok(_) => {},
                    Err(err) => warn!(
                        path = %log.path().display(),
                        error = %err,
                        "transport_log: compaction failed"
                    ),
                }
            }
        }
    });
}

/// Pull explicit `(principal, workspace)` strings out of an event,
/// returning `None` for variants that don't carry scope (Heartbeat,
/// system-emitted events). Mirrors the logic in
/// `websocket_handler::event_visible_to_scope` — keep the two in
/// lockstep when adding new variants. The compile-time exhaustive match
/// in that function will fail loudly when a variant is added without a
/// scope decision; this function falls back to "system" for the same
/// variants.
fn explicit_scope(event: &RuntimeTransportEvent) -> Option<(&str, &str)> {
    use RuntimeTransportEvent::*;
    match event {
        FeedItemCreated { item, .. } => Some((item.principal.as_str(), item.workspace.as_str())),
        // `AgentEvent` is the envelope every chat-path / GAUI emit
        // takes (`reasoning.*`, `tool.call.*`, `llm.*`, `agentic.*`,
        // `execution.*`, etc.). Scope lives at one of two depths:
        //   1. on the `AgentEventEnvelope` (`event.principal/workspace`
        //      — set by `new_scoped`),
        //   2. embedded in the payload (some emit sites stamp it via
        //      `payload.principal/workspace`).
        // Without this arm every wrapped event routed to `system/system`
        // and disappeared from user-scoped `/events` queries — the
        // operator's report was "I send a chat message but only see
        // ChatMessageReceived + FeedItemCreated".
        AgentEvent { event } => {
            if let (Some(p), Some(w)) = (event.principal.as_deref(), event.workspace.as_deref()) {
                return Some((p, w));
            }
            let principal = event.payload.get("principal").and_then(|v| v.as_str())?;
            let workspace = event.payload.get("workspace").and_then(|v| v.as_str())?;
            Some((principal, workspace))
        },
        // `ProgressEvent` is the bus envelope for progress-router-bound
        // ProgressMessages (post single-rail migration; producers no
        // longer call `ProgressRouter::publish_message` direct). The
        // scope lives on the inner `ProgressMessage`.
        ProgressEvent { message, .. } => {
            Some((message.principal.as_str(), message.workspace.as_str()))
        },
        DecisionAccountingGap {
            principal,
            workspace,
            ..
        }
        | DecisionShadowAgreement {
            principal,
            workspace,
            ..
        }
        | FeedItemUpdated {
            principal,
            workspace,
            ..
        }
        | FeedItemRemoved {
            principal,
            workspace,
            ..
        }
        | ExecutionPanelDelta {
            principal,
            workspace,
            ..
        }
        | TaskCreated {
            principal,
            workspace,
            ..
        }
        | TaskUpdated {
            principal,
            workspace,
            ..
        }
        | TaskDeleted {
            principal,
            workspace,
            ..
        }
        | V3PlanningStarted {
            principal,
            workspace,
            ..
        }
        | V3PlanningProgress {
            principal,
            workspace,
            ..
        }
        | V3PlanningCompleted {
            principal,
            workspace,
            ..
        }
        | V3PlanningFailed {
            principal,
            workspace,
            ..
        }
        | AgentDefinitionChanged {
            principal,
            workspace,
            ..
        } => Some((principal.as_str(), workspace.as_str())),
        _ => optional_scope(event),
    }
}

/// Optional-scope variants: read principal/workspace if present, else
/// `None` for system bucket routing.
fn optional_scope(event: &RuntimeTransportEvent) -> Option<(&str, &str)> {
    use RuntimeTransportEvent::*;
    macro_rules! match_optional {
        ( $( $variant:ident ),* $(,)? ) => {
            match event {
                $(
                    $variant { principal: Some(p), workspace: Some(w), .. } =>
                        Some((p.as_str(), w.as_str())),
                )*
                _ => None,
            }
        };
    }
    match_optional!(
        ChatMessageReceived,
        MessageProcessingStarted,
        QueryAnalysisCompleted,
        StrategySelected,
        ExplorationProgress,
        ExecutionStarted,
        ExecutionStepStarted,
        ExecutionStepCompleted,
        ExecutionPaused,
        ExecutionResumed,
        ExecutionFailed,
        ExecutionCancelled,
        ExecutionInflightResent,
        ExecutionInflightDropped,
        ExecutionCompleted,
        ExecutionRestoreFailed,
        MessageCompleted,
        ExecutionStatusChanged,
        ExecutionResponsibilityChanged,
        ProcessingError,
        LLMAnalysisStarted,
        LLMAnalysisCompleted,
        LLMAnalysisFailed,
        ClarificationSessionSnapshot,
        ClarificationConfidenceSnapshot,
        SlotGraphDiff,
        WorkflowResumed,
        WorkflowStageResumed,
        WorkflowResumeFailed,
        ObservabilityAlert,
        PipelineStarted,
        PipelineStepStarted,
        PipelineStepCompleted,
        PipelineCompleted,
        PipelineFailed,
        AtomicPlanOutlineStarted,
        AtomicPlanOutlineCompleted,
        AtomicPlanExpansionStarted,
        AtomicPlanGenerated,
        ToolMatchingTierStarted,
        ToolMatchingTierCompleted,
        SlotExtractionStarted,
        SlotExtracted,
        SlotEnrichmentStarted,
        SlotEnrichmentCompleted,
        SlotConfidenceUpdated,
        ClarifiedTaskReady,
        LLMRequestSent,
        LLMResponseReceived,
        InferenceAttempted,
        AgenticExecutionStarted,
        AgenticIterationStarted,
        AgenticIterationCompleted,
        AgenticPageUnderstanding,
        AgenticDecisionMade,
        AgenticActionExecuted,
        AgenticClickFallbackUsed,
        AgenticExecutionCompleted,
        AgenticStepStarted,
        AgenticStepCompleted,
        AgenticStepFailed,
        AgenticWaitingForConfirmation,
        AgenticWaitingForUser,
        AgenticMaxIterationsReached,
        AgenticStepStuckWarning,
        DomChangeDetected,
        AgenticResumed,
        SubGoalRequested,
        SubGoalOutcome,
        ShellOutputChunk,
        ParameterInferenceAttempted,
        ParameterInferred,
        ParameterInferenceFailed,
        ParameterDiscoveryAttempted,
        ParameterDiscovered,
        ParameterDiscoveryFailed,
        ParameterResolutionProgress,
        AgentCycleStarted,
        AgentCycleCompleted,
        AgentTriggered,
        HitlRequested,
        HitlResolved,
        // Activity spans. Omitting these is silent, not a build failure:
        // the payload would still carry principal/workspace while every
        // row persisted to the system/system bucket, so
        // `/events?principal=…` would return no activity and nothing
        // would say why.
        ActivityStarted,
        ActivityFinished,
        ActivityProgress,
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    #[test]
    fn memory_decision_comparisons_persist_in_their_own_scope() {
        let event = super::RuntimeTransportEvent::DecisionShadowAgreement {
            principal: "alice".into(),
            workspace: "work".into(),
            record: Box::new(serde_json::json!({"schema_version": 1, "comparison_id": "opaque"})),
            timestamp: 1,
        };
        assert_eq!(
            super::route_event_to_scope(&event),
            ("alice".into(), "work".into())
        );
        assert!(
            !crate::magician_v2::realtime_events::event_visible_to_scope(&event, "bob", "work")
        );
    }
    use super::*;

    #[test]
    fn generic_backfill_serialized_privacy_boundary_fails_closed() {
        let ordinary = r#"{"event_type":"ExecutionStarted","data":{"task_id":"task-1"}}"#;
        let notification = r#"{"event_type":"HitlRequested","data":{"input_schema":{"context":{"app_owner_notification":true}},"prompt":"private"}}"#;
        let resolution =
            r#"{"event_type":"HitlResolved","data":{"source":"app_owner_notification"}}"#;
        let canonical_notification = r#"{"event_type":"hitl.requested","payload":{"input_schema":{"context":{"app_owner_notification":true}},"prompt":"private"}}"#;
        let canonical_resolution =
            r#"{"event_type":"hitl.resolved","payload":{"source":"app_owner_notification"}}"#;
        let canonical_generic = r#"{"event_type":"hitl.requested","payload":{"source":"user_request","prompt":"ordinary"}}"#;
        let ordinary_scalar_payload =
            r#"{"event_type":"legacy.ordinary","payload":"legacy payload"}"#;
        let damaged_envelope = r#"{"event_type":"HitlRequested","data":"damaged private prompt"}"#;
        let damaged_canonical_envelope =
            r#"{"event_type":"hitl.requested","payload":"damaged private prompt"}"#;
        let missing_event_type = r#"{"payload":{"input_schema":{"context":{"app_owner_notification":true}},"prompt":"private"}}"#;

        assert!(serialized_event_is_safe_for_generic_backfill(ordinary));
        assert!(!serialized_event_is_safe_for_generic_backfill(notification));
        assert!(!serialized_event_is_safe_for_generic_backfill(resolution));
        assert!(!serialized_event_is_safe_for_generic_backfill(
            canonical_notification
        ));
        assert!(!serialized_event_is_safe_for_generic_backfill(
            canonical_resolution
        ));
        assert!(serialized_event_is_safe_for_generic_backfill(
            canonical_generic
        ));
        assert!(serialized_event_is_safe_for_generic_backfill(
            ordinary_scalar_payload
        ));
        assert!(!serialized_event_is_safe_for_generic_backfill(
            damaged_envelope
        ));
        assert!(!serialized_event_is_safe_for_generic_backfill(
            damaged_canonical_envelope
        ));
        assert!(!serialized_event_is_safe_for_generic_backfill(
            missing_event_type
        ));
        assert!(serialized_event_is_app_owner_notification_request(
            canonical_notification
        ));
    }

    #[tokio::test]
    async fn append_then_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let log = WorkspaceEventLog::new(tmp.path());
        log.append(r#"{"event_type":"a","timestamp":1}"#, 0)
            .await
            .unwrap();
        log.append(r#"{"event_type":"b","timestamp":2}"#, 0)
            .await
            .unwrap();
        let body = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(body.lines().count(), 2);
        assert!(body.contains("\"event_type\":\"a\""));
        assert!(body.contains("\"event_type\":\"b\""));
    }

    #[tokio::test]
    async fn compaction_keeps_the_larger_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let log = WorkspaceEventLog::new(tmp.path());
        // Write 3 ancient events (older than 24h). With the count
        // floor at 2000, all 3 should be kept.
        let mut payload = String::new();
        for i in 0..3u64 {
            let line = format!(r#"{{"event_type":"old_{i}","timestamp":1}}"#);
            payload.push_str(&line);
            payload.push('\n');
        }
        write_with_fd_retry(log.path(), payload).await.unwrap();
        let stats = log.compact().await.unwrap();
        assert_eq!(stats.dropped, 0, "ancient events kept by count floor");
    }

    #[tokio::test]
    async fn compaction_drops_old_events_past_count_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let log = WorkspaceEventLog::new(tmp.path());
        let now_ms = chrono::Utc::now().timestamp_millis();
        let ancient_ts = now_ms - EVENTS_RETENTION_WINDOW_MS - 10_000;
        // Write count_floor + 5 ancient events. After compaction, only
        // the newest count_floor (2000) survive — the 5 oldest drop.
        let total = EVENTS_RETENTION_MIN_COUNT + 5;
        let mut payload = String::new();
        for i in 0..total {
            let line = format!(r#"{{"event_type":"e_{i}","timestamp":{ancient_ts}}}"#);
            payload.push_str(&line);
            payload.push('\n');
        }
        write_with_fd_retry(log.path(), payload).await.unwrap();
        let stats = log.compact().await.unwrap();
        assert_eq!(stats.kept, EVENTS_RETENTION_MIN_COUNT);
        assert_eq!(stats.dropped, 5);
        let body = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(body.lines().count(), EVENTS_RETENTION_MIN_COUNT);
        // The first remaining event should be e_5 (the 5 oldest were dropped).
        assert!(body
            .lines()
            .next()
            .unwrap()
            .contains("\"event_type\":\"e_5\""));
    }

    #[tokio::test]
    async fn compaction_keeps_window_when_window_exceeds_count_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let log = WorkspaceEventLog::new(tmp.path());
        let now_ms = chrono::Utc::now().timestamp_millis();
        let recent_ts = now_ms - 1_000; // well within 24h
        let total = EVENTS_RETENTION_MIN_COUNT + 50;
        let mut payload = String::new();
        for i in 0..total {
            let line = format!(r#"{{"event_type":"e_{i}","timestamp":{recent_ts}}}"#);
            payload.push_str(&line);
            payload.push('\n');
        }
        write_with_fd_retry(log.path(), payload).await.unwrap();
        let stats = log.compact().await.unwrap();
        assert_eq!(
            stats.kept, total,
            "window > floor, all recent events retained"
        );
        assert_eq!(stats.dropped, 0);
    }

    /// Compaction rewrites the whole log. A reader of the scope directory must
    /// see the compacted log or the pre-compaction one, never a staging
    /// sibling — and every surviving line must still parse as an event.
    #[tokio::test]
    async fn compaction_publishes_without_leaving_a_staging_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let log = WorkspaceEventLog::new(tmp.path());
        let now_ms = chrono::Utc::now().timestamp_millis();
        let ancient_ts = now_ms - EVENTS_RETENTION_WINDOW_MS - 10_000;
        let total = EVENTS_RETENTION_MIN_COUNT + 5;
        let mut payload = String::new();
        for i in 0..total {
            payload.push_str(&format!(
                r#"{{"event_type":"e_{i}","timestamp":{ancient_ts}}}"#
            ));
            payload.push('\n');
        }
        write_with_fd_retry(log.path(), payload).await.unwrap();

        let stats = log.compact().await.unwrap();
        assert_eq!(stats.dropped, 5);

        let staging_left = std::fs::read_dir(tmp.path())
            .expect("scope listing")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(
            !staging_left,
            "compaction must leave no staging sibling in the scope directory"
        );

        let body = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(body.lines().count(), EVENTS_RETENTION_MIN_COUNT);
        assert!(
            body.lines()
                .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()),
            "every compacted line must still parse as JSON"
        );
    }

    /// Regression: `AgentEvent` envelopes (`reasoning.*` / `tool.call.*` /
    /// every chat-path GAUI event) used to fall through `explicit_scope`
    /// and route to `system/system`, hiding them from user-scoped
    /// `/events` queries. The fix reads scope from the envelope first,
    /// then falls back to embedded payload fields.
    #[test]
    fn agent_event_routes_by_envelope_scope() {
        use crate::magician_v2::realtime_events::AgentEventEnvelope;
        let envelope = AgentEventEnvelope::new_scoped(
            "reasoning.start",
            "agent-1",
            "alice",
            "prod",
            serde_json::json!({"trace_id": "t1"}),
        );
        let event = RuntimeTransportEvent::AgentEvent { event: envelope };
        assert_eq!(
            route_event_to_scope(&event),
            ("alice".to_string(), "prod".to_string()),
        );
    }

    /// When the envelope scope is missing, fall back to payload scope —
    /// some emit sites stamp `principal`/`workspace` into the payload
    /// instead of the envelope.
    #[test]
    fn agent_event_routes_by_payload_scope_when_envelope_unscoped() {
        use crate::magician_v2::realtime_events::AgentEventEnvelope;
        let envelope = AgentEventEnvelope::new(
            "tool.call.started",
            "agent-2",
            serde_json::json!({
                "principal": "bob",
                "workspace": "staging",
                "tool": "echo"
            }),
        );
        let event = RuntimeTransportEvent::AgentEvent { event: envelope };
        assert_eq!(
            route_event_to_scope(&event),
            ("bob".to_string(), "staging".to_string()),
        );
    }

    /// Truly unscoped events route to `system/system` as the last
    /// resort, never silently dropped.
    #[test]
    fn agent_event_falls_back_to_system_when_no_scope_anywhere() {
        use crate::magician_v2::realtime_events::AgentEventEnvelope;
        let envelope = AgentEventEnvelope::new(
            "agent.cycle.started",
            "agent-3",
            serde_json::json!({"goal": "boot"}),
        );
        let event = RuntimeTransportEvent::AgentEvent { event: envelope };
        assert_eq!(
            route_event_to_scope(&event),
            (SYSTEM_PRINCIPAL.to_string(), SYSTEM_WORKSPACE.to_string()),
        );
    }

    // ── has_structural_timestamp ─────────────────────────────────
    //
    // Regression battery for the structural-position substring probe
    // that gates `ensure_top_level_timestamp`. False positives here
    // (payload strings containing the word `timestamp`) silently let
    // injection-needing rows through and sorted them at `i64::MIN`
    // in cross-scope-backfill. False negatives are merely wasteful
    // (we re-serialize unnecessarily) but not incorrect.

    #[test]
    fn structural_timestamp_detects_top_level_numeric() {
        assert!(has_structural_timestamp(
            r#"{"timestamp": 1234567890, "event_type": "x"}"#
        ));
    }

    #[test]
    fn structural_timestamp_detects_nested_numeric_in_data() {
        assert!(has_structural_timestamp(
            r#"{"event_type":"x","data":{"agent_id":"a","timestamp":12345}}"#
        ));
    }

    #[test]
    fn structural_timestamp_detects_timestamp_ms() {
        assert!(has_structural_timestamp(
            r#"{"timestamp_ms": 1234567890, "event_type": "x"}"#
        ));
    }

    #[test]
    fn structural_timestamp_rejects_string_payload_mentioning_timestamp() {
        // Bug fix: the word `timestamp` appearing inside a string value
        // (e.g. tool-call deltas, doc bodies) used to short-circuit
        // injection. Must NOT count as a structural occurrence.
        assert!(!has_structural_timestamp(
            r#"{"event_type":"x","data":{"delta":"please include a timestamp"}}"#
        ));
    }

    #[test]
    fn structural_timestamp_rejects_zero_value() {
        // Legacy `#[serde(default)]` i64 fields serialize as 0 when
        // unset. Must be treated as "missing" so the fallback fires.
        assert!(!has_structural_timestamp(
            r#"{"event_type":"x","data":{"timestamp":0}}"#
        ));
    }

    #[test]
    fn structural_timestamp_rejects_zero_value_top_level() {
        assert!(!has_structural_timestamp(
            r#"{"timestamp":0,"event_type":"x"}"#
        ));
    }

    #[test]
    fn structural_timestamp_rejects_empty_event() {
        assert!(!has_structural_timestamp(r#"{"event_type":"x"}"#));
    }

    #[test]
    fn ensure_top_level_timestamp_injects_when_zero() {
        // The whole point of treating zero as missing — a legacy row
        // with `timestamp: 0` should get its fallback ms spliced in
        // at the top level so retention + sort have something useful.
        let line = r#"{"event_type":"x","data":{"timestamp":0}}"#;
        let out = ensure_top_level_timestamp(line, 42);
        // The original `data.timestamp` survives unchanged; we add a
        // top-level `timestamp_ms` so the extractor ladder hits it
        // first.
        assert!(out.contains("\"timestamp_ms\":42"));
    }

    #[test]
    fn ensure_top_level_timestamp_skips_when_real_value_present() {
        let line = r#"{"event_type":"x","data":{"timestamp":1234}}"#;
        let out = ensure_top_level_timestamp(line, 42);
        // No top-level injection — the existing structural timestamp
        // is fine for downstream extraction.
        assert!(!out.contains("\"timestamp_ms\":42"));
        assert_eq!(out, line);
    }

    #[test]
    fn ensure_top_level_timestamp_skips_when_payload_string_mentions_word() {
        // Real-world failure case: a streaming `tool.call.args` delta
        // carrying the literal word `timestamp` in its argument JSON.
        // Without the structural-position check the line slipped past
        // injection; with it, the fallback fires.
        let line = r#"{"event_type":"tool.call.args","data":{"delta":"set timestamp"}}"#;
        let out = ensure_top_level_timestamp(line, 99);
        assert!(out.contains("\"timestamp_ms\":99"));
    }

    // ── sanitize_scope_pair quarantine routing ───────────────────

    #[test]
    fn unsafe_scope_routes_to_quarantine_not_system() {
        let (p, w) = sanitize_scope_pair("../escape", "default");
        assert_eq!(p, QUARANTINE_PRINCIPAL);
        assert_eq!(w, QUARANTINE_WORKSPACE);
    }

    #[test]
    fn safe_scope_passes_through() {
        let (p, w) = sanitize_scope_pair("alice-prod", "team_42");
        assert_eq!(p, "alice-prod");
        assert_eq!(w, "team_42");
    }

    #[test]
    fn empty_scope_routes_to_quarantine() {
        let (p, w) = sanitize_scope_pair("", "default");
        assert_eq!(p, QUARANTINE_PRINCIPAL);
        assert_eq!(w, QUARANTINE_WORKSPACE);
    }

    /// magicllm sits below magician and cannot import these constants, so it
    /// declares its own copy for the scope unscoped LLM calls fall back to.
    /// The two are only correct while they agree: when magicllm pointed its
    /// fallback at `system`/`default` instead, every unscoped call wrote into a
    /// scope that matched neither reserved pair, and boot-time enumeration then
    /// materialised it as a full phantom tenant.
    #[test]
    fn magicllm_reserved_system_scope_matches_this_crates_constants() {
        assert_eq!(magicllm::trace::RESERVED_SYSTEM_PRINCIPAL, SYSTEM_PRINCIPAL);
        assert_eq!(magicllm::trace::RESERVED_SYSTEM_WORKSPACE, SYSTEM_WORKSPACE);
        let fallback = magicllm::LlmScope::legacy_default();
        assert_eq!(fallback.principal, SYSTEM_PRINCIPAL);
        assert_eq!(fallback.workspace, SYSTEM_WORKSPACE);
    }
}
