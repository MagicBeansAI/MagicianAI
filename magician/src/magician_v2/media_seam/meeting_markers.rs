//! Crash markers for live meeting captures.
//!
//! A capture writes a small JSON marker when it spawns and removes it on clean
//! teardown. If the server dies mid-capture (crash, SIGKILL, power loss) the
//! marker survives; the boot sweep (`sweep_interrupted_captures` in
//! `bin/magician.rs`) finds it, posts a "capture was interrupted" note into the
//! meeting's chat thread, and deletes it — so a transcript that just STOPS is
//! explained in-thread instead of silently looking complete.
//!
//! Markers live per scope under
//! `<scope>/workdirs/meeting_capture_markers/<session_id>.json` (the same
//! workdirs root other capability scratch state uses). All filesystem work is
//! best-effort: a marker failure must never block or fail a capture.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

/// Directory (under the scope's capability workdirs root) holding the markers.
pub const CAPTURE_MARKER_DIR: &str = "meeting_capture_markers";

/// What a live capture leaves on disk while it runs. Carries enough to post an
/// interruption note into the right thread after a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureMarker {
    pub session_id: String,
    /// `"attendee"` (joined the call) or `"passive"` (local listener).
    pub mode: String,
    /// `meeting-<label>-<date>` thread the transcript streamed into.
    pub thread: Option<String>,
    pub title: Option<String>,
    /// Scope the capture ran under — the sweep posts the note here.
    pub principal: String,
    pub workspace: String,
    pub started_at_ms: i64,
}

/// What a spawn path passes to the manager so it can write a marker once the
/// session id is assigned: the scope's marker directory + the scope itself.
#[derive(Debug, Clone)]
pub struct MarkerContext {
    pub dir: PathBuf,
    pub principal: String,
    pub workspace: String,
}

impl MarkerContext {
    /// The conventional per-scope marker directory.
    pub fn for_scope(workdirs_root: PathBuf, principal: &str, workspace: &str) -> Self {
        Self {
            dir: workdirs_root.join(CAPTURE_MARKER_DIR),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        }
    }
}

/// Manager-side convenience: write the marker for a freshly-assigned session
/// id from an optional [`MarkerContext`]. Returns the path for the teardown
/// removal, `None` when markers are off (no ctx) or the write failed.
pub fn write_marker_for_session(
    ctx: Option<&MarkerContext>,
    session_id: &str,
    mode: &str,
    thread: Option<&str>,
    title: Option<&str>,
) -> Option<PathBuf> {
    let ctx = ctx?;
    write_marker(
        &ctx.dir,
        &CaptureMarker {
            session_id: session_id.to_string(),
            mode: mode.to_string(),
            thread: thread.map(str::to_string),
            title: title.map(str::to_string),
            principal: ctx.principal.clone(),
            workspace: ctx.workspace.clone(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
        },
    )
}

/// Write `<dir>/<session_id>.json`. Returns the path for later removal, or
/// `None` on failure (logged, never fatal).
pub fn write_marker(dir: &Path, marker: &CaptureMarker) -> Option<PathBuf> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        warn!(target: "meet_bot", error = %e, "capture marker: create dir failed");
        return None;
    }
    let path = dir.join(format!("{}.json", marker.session_id));
    // Durable publish. A marker exists precisely to survive the crash that an
    // in-place write is vulnerable to, and `drain_markers` DELETES anything it
    // cannot parse — so a torn marker does not degrade the interruption note,
    // it removes it, and the transcript that just stops goes unexplained again.
    match serde_json::to_vec_pretty(marker) {
        Ok(bytes) => match write_bytes_durably_sync(&path, &bytes) {
            Ok(()) => Some(path),
            Err(e) => {
                warn!(target: "meet_bot", error = %e, "capture marker: write failed");
                None
            },
        },
        Err(e) => {
            warn!(target: "meet_bot", error = %e, "capture marker: serialize failed");
            None
        },
    }
}

/// Remove a marker written earlier (clean teardown). Best-effort.
pub fn remove_marker(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(target: "meet_bot", error = %e, path = %path.display(), "capture marker: remove failed");
        }
    }
}

/// Read every marker in `dir`, returning `(marker, path)` pairs. Parsable
/// markers are NOT deleted — the caller removes each via [`remove_marker`]
/// only after it has handled the interruption (posted the note), so a failed
/// handling is retried on the next boot instead of silently lost. Unparsable
/// files ARE deleted (they will never become parsable) with a warn.
pub fn drain_markers(dir: &Path) -> Vec<(CaptureMarker, PathBuf)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(), // no dir → no interrupted captures
    };
    let mut markers = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<CaptureMarker>(&bytes).map_err(|e| e.to_string())
            }) {
            Ok(marker) => markers.push((marker, path)),
            Err(e) => {
                warn!(target: "meet_bot", error = %e, path = %path.display(), "capture marker: unreadable; discarding");
                let _ = std::fs::remove_file(&path);
            },
        }
    }
    markers
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;

    #[test]
    fn marker_roundtrip_write_drain() {
        let dir = std::env::temp_dir().join(format!("mtg-markers-{}", uuid::Uuid::new_v4()));
        let marker = CaptureMarker {
            session_id: "listen-test".into(),
            mode: "passive".into(),
            thread: Some("meeting-standup-2026-06-11".into()),
            title: Some("Standup".into()),
            principal: "anonymous".into(),
            workspace: "default".into(),
            started_at_ms: 1,
        };
        let path = write_marker(&dir, &marker).expect("write");
        assert!(path.exists());
        let drained = drain_markers(&dir);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0.session_id, "listen-test");
        assert_eq!(
            drained[0].0.thread.as_deref(),
            Some("meeting-standup-2026-06-11")
        );
        // Drain does NOT consume: an unhandled marker is retried next boot.
        assert_eq!(drain_markers(&dir).len(), 1);
        // Handling done → the caller removes it; the next sweep finds nothing.
        remove_marker(&drained[0].1);
        assert!(drain_markers(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marker_write_publishes_atomically_and_leaves_no_staging_file() {
        let dir = std::env::temp_dir().join(format!("mtg-markers-{}", uuid::Uuid::new_v4()));
        let marker = CaptureMarker {
            session_id: "atomic-test".into(),
            mode: "passive".into(),
            thread: None,
            title: None,
            principal: "anonymous".into(),
            workspace: "default".into(),
            started_at_ms: 1,
        };

        write_marker(&dir, &marker).expect("write");

        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .expect("marker dir listing")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["atomic-test.json".to_string()],
            "the durable write must publish exactly one file, with no staging sibling"
        );
        // And the published marker parses, so the boot sweep will act on it.
        assert_eq!(drain_markers(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_marker_on_clean_teardown() {
        let dir = std::env::temp_dir().join(format!("mtg-markers-{}", uuid::Uuid::new_v4()));
        let marker = CaptureMarker {
            session_id: "meet-test".into(),
            mode: "attendee".into(),
            thread: None,
            title: None,
            principal: "anonymous".into(),
            workspace: "default".into(),
            started_at_ms: 1,
        };
        let path = write_marker(&dir, &marker).expect("write");
        remove_marker(&path);
        assert!(drain_markers(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
