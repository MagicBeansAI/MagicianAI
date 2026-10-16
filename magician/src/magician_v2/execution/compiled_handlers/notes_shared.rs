//! Shared argument reading and result shaping for the Notes tools.
//!
//! The HTTP surface already classifies a notes failure by `io::ErrorKind` —
//! a bad argument is a 400, a missing note a 404, a provider outage a 503. A
//! tool answers a model rather than a browser, so the same distinction is
//! carried as a `retryable` flag and a stable `reason`: without it every
//! failure reads alike and the model's only recovery is to try the identical
//! call again, which is right for an outage and wrong for a bad path.

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ArtifactV2Error;
use crate::magician_v2::notes::NoteRef;

/// Read a string argument, treating blank as absent so a model passing `""`
/// for "no preference" does not become an empty provider name.
///
/// Required-ness is the caller's to enforce, which is why this returns an
/// `Option` under one name rather than a `required_`/`optional_` pair that
/// differ only in what they promise.
pub fn text_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// A caller-fixable problem: the arguments were wrong, so retrying them
/// unchanged cannot succeed.
pub fn argument_error(reason: &str) -> Value {
    json!({
        "status": "error",
        "reason": reason,
        "retryable": false,
    })
}

/// The HTTP surface's own reading of which failures are transient.
/// Classify a notes IO failure.
///
/// `fallback` names what failed when the kind says nothing useful, so a
/// read-only tool never reports that a write failed. Everything the caller can
/// act on — bad argument, denied, missing, provider down — is decided here and
/// is the same whichever direction the call was going.
fn io_classification(error: &std::io::Error, fallback: &'static str) -> (&'static str, bool) {
    match error.kind() {
        std::io::ErrorKind::InvalidInput => ("invalid_note_request", false),
        std::io::ErrorKind::PermissionDenied => ("notes_permission_denied", false),
        std::io::ErrorKind::NotFound => ("note_not_found", false),
        std::io::ErrorKind::AlreadyExists => ("note_already_exists", false),
        std::io::ErrorKind::WouldBlock
        | std::io::ErrorKind::TimedOut
        | std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::NotConnected => ("notes_provider_unavailable", true),
        _ => (fallback, false),
    }
}

/// Shape a notes write failure, preserving that reading.
pub fn write_error(tool: &str, error: std::io::Error) -> Value {
    let (reason, retryable) = io_classification(&error, "notes_write_failed");
    json!({
        "status": "error",
        "tool": tool,
        "reason": reason,
        "retryable": retryable,
        "detail": error.to_string(),
    })
}

/// Shape a task lookup failure.
///
/// A missing task and a filesystem hiccup are not the same answer: reporting
/// both as `task_not_found` tells the model to give up on work that would have
/// succeeded on a second call. Only the not-found variants are permanent.
pub fn task_lookup_error(tool: &str, error: ArtifactV2Error) -> Value {
    let (reason, retryable) = match &error {
        ArtifactV2Error::TaskNotFound(_) => ("task_not_found", false),
        ArtifactV2Error::Io(io) => io_classification(io, "task_lookup_failed"),
        _ => ("task_lookup_failed", false),
    };
    json!({
        "status": "error",
        "tool": tool,
        "reason": reason,
        "retryable": retryable,
        "detail": error.to_string(),
    })
}

/// Shape a notes read failure.
///
/// `open_note` and `search_notes` never write, so reporting `notes_write_failed`
/// from them tells the model something untrue about what the tool did — and a
/// model that believes a write was attempted may go looking for a half-written
/// note that cannot exist.
pub fn read_error(tool: &str, error: std::io::Error) -> Value {
    let (reason, retryable) = io_classification(&error, "notes_read_failed");
    json!({
        "status": "error",
        "tool": tool,
        "reason": reason,
        "retryable": retryable,
        "detail": error.to_string(),
    })
}

/// Shape a successful write.
///
/// `used_fallback` is surfaced rather than smoothed over: the note landed
/// somewhere other than the requested provider, and an agent that reports "saved
/// to SilverBullet" when it went to local Markdown has told the owner something
/// untrue.
pub fn note_result(note: NoteRef) -> Value {
    json!({
        "status": "ok",
        "path": note.path,
        "absolute_path": note.absolute_path,
        "provider": note.provider,
        "requested_provider": note.requested_provider,
        "used_fallback": note.used_fallback,
        "fallback_reason": note.fallback_reason,
        "open_url": note.open_url,
    })
}
