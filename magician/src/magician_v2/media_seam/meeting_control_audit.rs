//! Durable audit for every capture start and stop, whichever door it came
//! through.
//!
//! Capture is the one app-reachable action that can run for hours and hear a
//! room. The reviewed control-action class is allowed to reach the SAME join
//! path and manager transitions the first-party `/meetings` API serves — never
//! around them — so the audit has to sit on that shared path rather than inside
//! either caller. One appender, two callers:
//!
//!   * the first-party `/meetings/{listen,join,stop}` handlers, and
//!   * the `magician.meeting-control` destination owner.
//!
//! A row therefore answers "who asked for this capture, through which door, at
//! what time" without cross-referencing two logs. Pause and resume are recorded
//! too: they change what the room is being heard by.
//!
//! This is NOT the app-facing receipt, and the two must not converge. The
//! `magician.meeting-control` destination mints one `control_receipt` row per
//! signed decision whose fate is final, in a sibling directory under the same
//! scope root. THIS log records every attempt in the operator's vocabulary,
//! with free-text detail that names host paths and registry state. That ledger
//! records fates only, in the package's closed entity vocabulary, and crosses
//! back into an app's own data plane — so it carries a closed error code and
//! never the detail below.
//!
//! The attendee rail is audited INSIDE the shared join path, which takes a
//! required [`CaptureControlOrigin`]: a future caller cannot reach that rail
//! without declaring its door. The listener rail is audited by its two callers,
//! because its start function has per-platform and per-capture-source variants
//! and both callers are in this repo; a third listener caller must add its own
//! row, and this comment is where that obligation is recorded.
//!
//! Rows land per scope at `<scope>/workdirs/meeting_control_audit/<UTC date>.jsonl`,
//! newline-delimited, append-only, and every filesystem step is best-effort:
//! failing to record must never wedge a stop, which is the risk-reducing
//! control. A failure is logged at `warn` rather than swallowed.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Directory (under the scope's capability workdirs root) holding the audit.
pub const CAPTURE_CONTROL_AUDIT_DIR: &str = "meeting_control_audit";

/// Longest text this audit will retain for any single free-text field. The
/// audit is an operator record, not a transcript store.
const MAX_AUDIT_TEXT_BYTES: usize = 1024;

/// Which door a control came through. Not free text: a new door must be added
/// here deliberately, and an app can never label itself first-party.
///
/// `join_meeting_with_scope` takes one of these as a required argument
/// precisely so a future caller cannot reach the attendee rail without saying
/// which door it is.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureControlOrigin {
    /// The first-party `/meetings` HTTP surface.
    FirstPartyApi,
    /// The reviewed `magician.meeting-control` app destination.
    AppControlDestination,
    /// The compiled `meeting` capability tool — an agent joining a call.
    CompiledMeetingTool,
}

impl CaptureControlOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FirstPartyApi => "first_party_api",
            Self::AppControlDestination => "app_control_destination",
            Self::CompiledMeetingTool => "compiled_meeting_tool",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureControlVerb {
    Listen,
    Join,
    Pause,
    Resume,
    Stop,
}

impl CaptureControlVerb {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Listen => "listen",
            Self::Join => "join",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Stop => "stop",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureControlOutcome {
    Accepted,
    Refused,
}

/// One audited control act.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureControlRecord {
    pub recorded_at: String,
    pub origin: CaptureControlOrigin,
    pub verb: CaptureControlVerb,
    pub outcome: CaptureControlOutcome,
    pub principal: String,
    pub workspace: String,
    /// Present once a session exists; absent for a refused or not-yet-started
    /// capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The app installation, when the act came through the app destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<String>,
    /// The signed decision the destination applied, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    /// The surface gesture that carried the owner's intent into a START.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture_id: Option<String>,
    /// Why a refusal happened. Free text, bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CaptureControlRecord {
    pub fn new(
        origin: CaptureControlOrigin,
        verb: CaptureControlVerb,
        outcome: CaptureControlOutcome,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            recorded_at: Utc::now().to_rfc3339(),
            origin,
            verb,
            outcome,
            principal: principal.into(),
            workspace: workspace.into(),
            session_id: None,
            thread_id: None,
            title: None,
            url: None,
            installation_id: None,
            decision_id: None,
            gesture_id: None,
            detail: None,
        }
    }

    pub fn with_session(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id.map(|value| bounded(&value));
        self
    }

    pub fn with_meeting(
        mut self,
        thread_id: Option<String>,
        title: Option<String>,
        url: Option<String>,
    ) -> Self {
        self.thread_id = thread_id.map(|value| bounded(&value));
        self.title = title.map(|value| bounded(&value));
        self.url = url.map(|value| bounded(&value));
        self
    }

    pub fn with_app(
        mut self,
        installation_id: Option<String>,
        decision_id: Option<String>,
        gesture_id: Option<String>,
    ) -> Self {
        self.installation_id = installation_id.map(|value| bounded(&value));
        self.decision_id = decision_id.map(|value| bounded(&value));
        self.gesture_id = gesture_id.map(|value| bounded(&value));
        self
    }

    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail.map(|value| bounded(&value));
        self
    }
}

/// Truncate on a character boundary. The audit never panics on a wide byte.
fn bounded(value: &str) -> String {
    if value.len() <= MAX_AUDIT_TEXT_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_AUDIT_TEXT_BYTES.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

/// The scope's audit directory under its capability workdirs root.
pub fn capture_control_audit_dir(workdirs_root: &std::path::Path) -> PathBuf {
    workdirs_root.join(CAPTURE_CONTROL_AUDIT_DIR)
}

/// Append one row from a context where blocking is acceptable.
///
/// Best-effort by contract: a filesystem failure is logged and never returned,
/// because refusing to stop a capture over an audit write would invert the risk
/// this audit exists to manage. Callers on an async request worker must use
/// [`record_capture_control_detached`] instead — this function opens and writes
/// a file synchronously.
pub fn record_capture_control(workdirs_root: &std::path::Path, entry: &CaptureControlRecord) {
    let dir = capture_control_audit_dir(workdirs_root);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        warn!(
            target: "meet_bot",
            %error,
            dir = %dir.display(),
            "capture control audit directory could not be created; the act is not recorded"
        );
        return;
    }
    // One file per UTC day keeps a long-running host's log rotatable without a
    // sweeper, and keeps the append path a single open/write/close.
    let path = dir.join(format!("{}.jsonl", Utc::now().format("%Y-%m-%d")));
    let mut line = match serde_json::to_vec(entry) {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(
                target: "meet_bot",
                %error,
                "capture control audit row could not be serialized"
            );
            return;
        },
    };
    line.push(b'\n');
    let write = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(&line));
    if let Err(error) = write {
        warn!(
            target: "meet_bot",
            %error,
            path = %path.display(),
            verb = entry.verb.as_str(),
            origin = entry.origin.as_str(),
            "capture control audit row could not be appended"
        );
    }
}

/// Append one row from an async context without blocking the runtime worker.
///
/// The audit sits on the hot stop/pause/resume paths and on every app control
/// admission. A synchronous `create_dir_all` + append there would stall the
/// reactor for every other request sharing the worker whenever the disk is
/// slow — on the very paths whose latency matters most. Fire-and-forget by
/// contract: nothing awaits the row, and a runtime that refuses the spawn falls
/// back to writing inline rather than dropping the record.
pub fn record_capture_control_detached(
    workdirs_root: &std::path::Path,
    entry: CaptureControlRecord,
) {
    let workdirs_root = workdirs_root.to_path_buf();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || record_capture_control(&workdirs_root, &entry));
        },
        Err(_) => record_capture_control(&workdirs_root, &entry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_append_one_json_line_each_and_survive_a_reopen() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let first = CaptureControlRecord::new(
            CaptureControlOrigin::FirstPartyApi,
            CaptureControlVerb::Listen,
            CaptureControlOutcome::Accepted,
            "owner",
            "default",
        )
        .with_session(Some("listen-1".to_owned()));
        let second = CaptureControlRecord::new(
            CaptureControlOrigin::AppControlDestination,
            CaptureControlVerb::Stop,
            CaptureControlOutcome::Refused,
            "owner",
            "default",
        )
        .with_app(
            Some("installation:meetings".to_owned()),
            Some("blake3:decision".to_owned()),
            None,
        )
        .with_detail(Some("unknown session".to_owned()));

        record_capture_control(temp.path(), &first);
        record_capture_control(temp.path(), &second);

        let path = capture_control_audit_dir(temp.path())
            .join(format!("{}.jsonl", Utc::now().format("%Y-%m-%d")));
        let body = std::fs::read_to_string(&path).expect("audit file");
        let rows = body
            .lines()
            .map(|line| serde_json::from_str::<CaptureControlRecord>(line).expect("row"))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].verb, CaptureControlVerb::Listen);
        assert_eq!(rows[0].origin, CaptureControlOrigin::FirstPartyApi);
        assert_eq!(rows[1].outcome, CaptureControlOutcome::Refused);
        assert_eq!(
            rows[1].installation_id.as_deref(),
            Some("installation:meetings")
        );
    }

    #[test]
    fn text_fields_truncate_on_character_boundaries() {
        let wide = "é".repeat(MAX_AUDIT_TEXT_BYTES);
        let entry = CaptureControlRecord::new(
            CaptureControlOrigin::FirstPartyApi,
            CaptureControlVerb::Join,
            CaptureControlOutcome::Accepted,
            "owner",
            "default",
        )
        .with_detail(Some(wide));
        let detail = entry.detail.expect("detail");
        assert!(detail.len() <= MAX_AUDIT_TEXT_BYTES + "…".len());
        assert!(detail.ends_with('…'));
    }

    #[test]
    fn an_unwritable_root_is_logged_and_never_panics() {
        // A file where a directory is expected: create_dir_all fails and the
        // recorder must return quietly rather than unwinding into a stop path.
        let temp = tempfile::tempdir().expect("temp");
        let blocked = temp.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").expect("write blocker");
        record_capture_control(
            &blocked,
            &CaptureControlRecord::new(
                CaptureControlOrigin::FirstPartyApi,
                CaptureControlVerb::Stop,
                CaptureControlOutcome::Accepted,
                "owner",
                "default",
            ),
        );
    }
}
